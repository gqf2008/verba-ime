#!/usr/bin/env bash
# 打包 macOS IMK .app：构建输入法本体（verba-mac）与 daemon（verba-daemon），
# 组装 Verba.app 目录并做 ad-hoc 签名。
#
# 用法：scripts/package.sh
# 产物：dist/Verba.app
# 安装：cp -R dist/Verba.app "$HOME/Library/Input Methods/"
#       然后到 系统设置 → 键盘 → 输入法 启用「拾言输入法」。
set -euo pipefail

IME_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$IME_ROOT/../../.." && pwd)"

cd "$IME_ROOT"
cargo build --release --manifest-path "$IME_ROOT/Cargo.toml"
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" -p verba-daemon
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" -p verba-settings
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" -p verba-trigger

APP="$IME_ROOT/dist/Verba.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

cp "$IME_ROOT/target/release/verba-mac" "$APP/Contents/MacOS/verba-mac"
cp "$REPO_ROOT/target/release/verba-daemon" "$APP/Contents/MacOS/verba-daemon"
cp "$REPO_ROOT/target/release/verba-settings" "$APP/Contents/MacOS/verba-settings"
# 触发工具（issue #82 跨平台统一）：选区截图/录音/TTS 播放的共享 CLI，
# `///` 选区 OCR 由 verba-mac spawn 本进程完成；随 bundle 分发、
# 由 release.yml 的逐二进制签名循环覆盖。
cp "$REPO_ROOT/target/release/verba-trigger" "$APP/Contents/MacOS/verba-trigger"
# 安装注册助手（issue #48 项 3）：DMG 内「安装.command」双击后由它注册/启用
# 输入源（TIS C API），随 bundle 分发、由 release.yml 的逐二进制签名循环覆盖。
cp "$IME_ROOT/target/release/verba-register" "$APP/Contents/MacOS/verba-register"
cp "$IME_ROOT/app/Info.plist" "$APP/Contents/Info.plist"
# 输入源显示名本地化（InfoPlist.strings：TIS 按 TISInputSourceID 取值，
# 缺失时系统设置列表回退显示原始 ID，见 app/Resources/）
cp -R "$IME_ROOT/app/Resources" "$APP/Contents/Resources"

# 版本注入：以根 Cargo.toml 的 workspace 版本为唯一版本源，同步
# CFBundleShortVersionString / CFBundleVersion（只改拷贝，不弄脏源码树）。
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml")"
# 空版本会产出 <string></string>（非法/空版本），先用守卫阻断（复审 V15）。
if [ -z "$VERSION" ]; then
    echo "::error::未能从根 Cargo.toml 解析 workspace 版本（version = \"...\"），拒绝注入空版本" >&2
    exit 1
fi
# 用 PlistBuddy 精确写两个版本键：原全局 sed 会替换 plist 中所有 x.y.z 字符串
# （一旦未来加入其他版本字段会被误改），定向写入只动这两个键。
PLIST="$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$PLIST"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" "$PLIST"

# 可选：捆绑 Rime（librime.dylib + data/），daemon 从 $APP/Contents/MacOS/rime/ 加载。
# 缺失时 daemon 日志会报 librime 加载失败，可用 VERBA_RIME_DYLIB/SHARED/USER 指向外部。
if [ -d "$REPO_ROOT/vendor/rime" ]; then
    # 需含版本化 dylib：librime 实体的 install_name 为 @rpath/librime.1.dylib
    # （LC_ID_DYLIB，非运行依赖——daemon 全路径 dlopen；缺失会影响未来链接场景与
    # 完整性，故仍作打包前置校验）
    if [ ! -f "$REPO_ROOT/vendor/rime/librime.dylib" ] || [ ! -f "$REPO_ROOT/vendor/rime/librime.1.dylib" ] || [ ! -d "$REPO_ROOT/vendor/rime/data" ]; then
        echo "::error::vendor/rime 不完整（需 librime.dylib + librime.1.dylib + data/，见 scripts/fetch-rime-vendor.sh）" >&2
        exit 1
    fi
    cp -R "$REPO_ROOT/vendor/rime" "$APP/Contents/MacOS/rime"
    echo "已捆绑 Rime: vendor/rime -> Verba.app/Contents/MacOS/rime"
else
    echo "未找到 vendor/rime（librime.dylib + data/），跳过 Rime 捆绑；发布构建须先跑 scripts/fetch-rime-vendor.sh"
fi

# ad-hoc 签名（本地安装足够；正式发布需 Developer ID + 公证）。
# 失败不吞：CI 与本地都应看到签名错误。
codesign --force --deep --sign - "$APP"
codesign --verify "$APP" 

echo "打包完成: $APP"
echo "一键安装: DMG 内双击「安装.command」（release.yml 组装）；"
echo "手动安装: cp -R '$APP' \"\$HOME/Library/Input Methods/\""
echo "然后运行 \$HOME/Library/Input\\ Methods/Verba.app/Contents/MacOS/verba-register 注册并启用，"
echo "或在 系统设置 → 键盘 → 输入法 中手动启用「拾言输入法」"
