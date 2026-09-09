#!/usr/bin/env bash
# 打包 macOS .pkg 安装包：把已组装（并已签名）的 Verba.app 打成 Installer 包，
# 安装到系统级 /Library/Input Methods，并为当前登录用户注册/启用输入源。
#
# 用法：
#   scripts/package-pkg.sh                 # 使用现有 dist/Verba.app
#   scripts/package-pkg.sh --build         # 先跑 scripts/package.sh 重新组装 .app
#   INSTALLER_IDENTITY="Developer ID Installer: Name (TEAMID)" scripts/package-pkg.sh
#
# 产物：dist/Verba-<version>.pkg
#
# 说明：
# - .pkg 为系统级安装（/Library/Input Methods/Verba.app，需管理员）。
#   postinstall 会以当前 console 用户身份调用 app 内 verba-register 注册并启用
#   输入源（TIS 注册/启用是 per-user 状态）。
# - 正式分发须用 Developer ID Installer 证书签名并公证；未提供 INSTALLER_IDENTITY
#   时产出未签名 pkg（本机安装会触发 Gatekeeper 提示，仅用于本地/CI dry-run）。
set -euo pipefail

IME_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$IME_ROOT/dist/Verba.app"
PKG_ID="dev.verba.inputmethod"
INSTALL_LOCATION="/Library/Input Methods"

if [ "${1:-}" = "--build" ]; then
    bash "$IME_ROOT/scripts/package.sh"
fi

if [ ! -d "$APP" ]; then
    echo "::error::找不到 $APP；请先运行 scripts/package.sh，或用 --build" >&2
    exit 1
fi

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" 2>/dev/null || true)"
if [ -z "$VERSION" ]; then
    echo "::error::无法从 $APP/Contents/Info.plist 读取 CFBundleShortVersionString" >&2
    exit 1
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/verba-pkg.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

PAYLOAD="$WORK/payload"
mkdir -p "$PAYLOAD"
# ditto 保留权限/扩展属性/资源叉——codesign seal 依赖这些元数据，不能改用 cp。
ditto "$APP" "$PAYLOAD/Verba.app"

SCRIPTS="$WORK/scripts"
mkdir -p "$SCRIPTS"
cat > "$SCRIPTS/postinstall" <<'POSTINSTALL'
#!/bin/bash
# pkg 以 root 执行；为当前 console 用户注册/启用输入源（TIS 为 per-user 状态）。
# 失败不阻塞安装：app 落位后系统扫描仍会注册，用户也可在系统设置手动启用。
set -u
APP="/Library/Input Methods/Verba.app"
CONSOLE_USER="$(stat -f%Su /dev/console 2>/dev/null || true)"
if [ -n "$CONSOLE_USER" ] && [ "$CONSOLE_USER" != "root" ] && [ -x "$APP/Contents/MacOS/verba-register" ]; then
    CONSOLE_UID="$(id -u "$CONSOLE_USER" 2>/dev/null || true)"
    if [ -n "$CONSOLE_UID" ]; then
        launchctl asuser "$CONSOLE_UID" "$APP/Contents/MacOS/verba-register" || true
    fi
fi
exit 0
POSTINSTALL
chmod +x "$SCRIPTS/postinstall"

COMPONENT="$WORK/Verba-component.pkg"
pkgbuild \
    --root "$PAYLOAD" \
    --identifier "$PKG_ID" \
    --version "$VERSION" \
    --install-location "$INSTALL_LOCATION" \
    --scripts "$SCRIPTS" \
    "$COMPONENT"

OUT="$IME_ROOT/dist/Verba-$VERSION.pkg"
rm -f "$OUT"
if [ -n "${INSTALLER_IDENTITY:-}" ]; then
    productbuild --package "$COMPONENT" --sign "$INSTALLER_IDENTITY" "$OUT"
else
    productbuild --package "$COMPONENT" "$OUT"
fi

echo "打包完成: $OUT"
echo "安装: 双击 ${OUT}（需管理员；装到 ${INSTALL_LOCATION}/Verba.app）"
if [ -z "${INSTALLER_IDENTITY:-}" ]; then
    echo "提示: 未提供 INSTALLER_IDENTITY，产物未签名；正式分发请用 Developer ID Installer 证书签名并公证。"
fi
