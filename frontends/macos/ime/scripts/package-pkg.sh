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
cleanup() {
    local status=$?
    rm -rf "$WORK"
    exit "$status"
}
trap cleanup EXIT

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
        # launchctl asuser 只切 Mach bootstrap/audit session，不降 euid；须再 sudo -u
        # 真正以 console 用户执行（TIS 注册/启用是 per-user 状态）。
        if ! launchctl asuser "$CONSOLE_UID" /usr/bin/sudo -u "$CONSOLE_USER" -- \
            "$APP/Contents/MacOS/verba-register"; then
            echo "warning: verba-register 自动注册失败，请在系统设置 → 键盘 → 输入法手动启用「拾言输入法」" >&2
        fi
    fi
fi
exit 0
POSTINSTALL
chmod +x "$SCRIPTS/postinstall"

COMPONENT="$WORK/Verba-component.pkg"
# 关键：pkgbuild --root 默认 BundleIsRelocatable=true。若用户机器上任意位置
# 存在同 bundle id 的 Verba.app（常见于仓库 dist/ 构建产物），Installer 会把
# payload 重定位到那里而不是 /Library/Input Methods，导致“PKG 已安装但系统级
# 路径为空、输入法菜单不出现”。显式关闭 relocation，并钉住 identifier/version。
COMPONENT_PLIST="$WORK/components.plist"
cat > "$COMPONENT_PLIST" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
  <dict>
    <key>RootRelativeBundlePath</key>
    <string>Verba.app</string>
    <key>BundleIsRelocatable</key>
    <false/>
    <key>BundleIsVersionChecked</key>
    <true/>
    <key>BundleHasStrictIdentifier</key>
    <true/>
    <key>BundleOverwriteAction</key>
    <string>upgrade</string>
  </dict>
</array>
</plist>
PLIST

pkgbuild \
    --root "$PAYLOAD" \
    --component-plist "$COMPONENT_PLIST" \
    --identifier "$PKG_ID" \
    --version "$VERSION" \
    --install-location "$INSTALL_LOCATION" \
    --scripts "$SCRIPTS" \
    "$COMPONENT"

# 防回归：关闭 BundleIsRelocatable 后，pkgbuild 必须生成空的 <relocate/>。
# 仅检查 pkg-info 的 relocatable="false" 不够——默认可重定位包同样带这个属性，
# 但会保留 <relocate><bundle .../></relocate>，Installer 仍会重定位到已有 app。
EXPANDED_COMPONENT="$WORK/expanded-component"
pkgutil --expand "$COMPONENT" "$EXPANDED_COMPONENT"
RELOCATE_BUNDLES="$(xmllint --xpath \
    'count(/*[local-name()="pkg-info"]/*[local-name()="relocate"]/*)' \
    "$EXPANDED_COMPONENT/PackageInfo")"
if [ "$RELOCATE_BUNDLES" != "0" ]; then
    echo "::error::PKG 仍允许 bundle relocation（relocate 子元素数=${RELOCATE_BUNDLES}），拒绝产出" >&2
    exit 1
fi

STAGED="$WORK/Verba-$VERSION.pkg"
if [ -n "${INSTALLER_IDENTITY:-}" ]; then
    productbuild --package "$COMPONENT" --sign "$INSTALLER_IDENTITY" "$STAGED"
else
    productbuild --package "$COMPONENT" "$STAGED"
fi
# 成功后再替换 dist 产物，避免签名/打包失败时丢失上一份有效 pkg。
OUT="$IME_ROOT/dist/Verba-$VERSION.pkg"
mv -f "$STAGED" "$OUT"

echo "打包完成: $OUT"
echo "安装: 双击 ${OUT}（需管理员；装到 ${INSTALL_LOCATION}/Verba.app）"
if [ -z "${INSTALLER_IDENTITY:-}" ]; then
    echo "提示: 未提供 INSTALLER_IDENTITY，产物未签名；正式分发请用 Developer ID Installer 证书签名并公证。"
fi
