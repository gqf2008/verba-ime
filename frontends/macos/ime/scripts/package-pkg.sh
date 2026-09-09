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
#   postinstall 经 LaunchServices 在 console 用户会话内启动 Verba.app 的
#   --register 短命模式，再由它调用 app 内 verba-register 注册并启用输入源
#   （TIS 注册/启用是 per-user 状态；package_script_service 内直接写会被沙盒拒绝）。
# - 正式分发须用 Developer ID Installer 证书签名并公证；未提供 INSTALLER_IDENTITY
#   时产出未签名 pkg（本机安装会触发 Gatekeeper 提示，仅用于本地/CI dry-run）。
set -euo pipefail

IME_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$IME_ROOT/dist/Verba.app"
REPO_ROOT="$(cd "$IME_ROOT/../../.." && pwd)"
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
EXPECTED_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml")"
if [ "$VERSION" != "$EXPECTED_VERSION" ]; then
    echo "::error::payload 版本 $VERSION 与 workspace $EXPECTED_VERSION 不一致；拒绝打包旧 app" >&2
    exit 1
fi
WORK="$(mktemp -d "${TMPDIR:-/tmp}/verba-pkg.XXXXXX")"
cleanup() {
    local status=$?
    rm -rf "$WORK"
    exit "$status"
}
trap cleanup EXIT

# 无副作用探针：旧版 verba-mac 忽略 --register 会进入 IMK 主循环。用 Perl
# fork + 独立进程组设置硬超时，超时杀整组，避免打包本身被旧 payload 挂住。
PROBE_FILE="$WORK/probe.out"
set +e
/usr/bin/perl -e '
my $timeout = shift;
my $pid = fork();
die "fork: $!" unless defined $pid;
if ($pid == 0) {
    setpgrp(0, 0);
    exec @ARGV;
    exit 127;
}
setpgrp($pid, $pid);
local $SIG{ALRM} = sub {
    kill "TERM", -$pid;
    waitpid($pid, 0);
    exit 124;
};
alarm $timeout;
waitpid($pid, 0);
exit($? >> 8);
' 5 "$APP/Contents/MacOS/verba-mac" --register-probe >"$PROBE_FILE" 2>/dev/null
PROBE_RC=$?
set -e
PROBE_OUT="$(/usr/bin/head -c 256 "$PROBE_FILE" 2>/dev/null || true)"
if [ "$PROBE_RC" -ne 0 ] || [ "$PROBE_OUT" != "verba-register-mode-supported" ]; then
    echo "::error::payload verba-mac 不支持 --register（probe_rc=$PROBE_RC probe=${PROBE_OUT:-empty}）；请重新构建 dist/Verba.app" >&2
    exit 1
fi

PAYLOAD="$WORK/payload"
mkdir -p "$PAYLOAD"
# ditto 保留权限/扩展属性/资源叉——codesign seal 依赖这些元数据，不能改用 cp。
ditto "$APP" "$PAYLOAD/Verba.app"

SCRIPTS="$WORK/scripts"
mkdir -p "$SCRIPTS"
cp "$IME_ROOT/scripts/pkg-postinstall.sh" "$SCRIPTS/postinstall"
chmod +x "$SCRIPTS/postinstall"
# 防止 postinstall 被改回 package_script_service 内直调 CLI。
grep -q 'open -n' "$SCRIPTS/postinstall"
if grep -q 'open -n -W' "$SCRIPTS/postinstall"; then
    echo "::error::postinstall 不应使用会永久阻塞的 open -W" >&2
    exit 1
fi
grep -q -- '--register' "$SCRIPTS/postinstall"

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
POSTINSTALL_EXPANDED="$EXPANDED_COMPONENT/Scripts/postinstall"
test -x "$POSTINSTALL_EXPANDED"
grep -q 'open -n' "$POSTINSTALL_EXPANDED"
if grep -q 'open -n -W' "$POSTINSTALL_EXPANDED"; then
    echo "::error::最终 PKG postinstall 不应使用 open -W" >&2
    exit 1
fi
grep -q -- '--register' "$POSTINSTALL_EXPANDED"
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
