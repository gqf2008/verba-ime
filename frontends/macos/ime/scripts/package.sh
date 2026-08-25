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

APP="$IME_ROOT/dist/Verba.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

cp "$IME_ROOT/target/release/verba-mac" "$APP/Contents/MacOS/verba-mac"
cp "$REPO_ROOT/target/release/verba-daemon" "$APP/Contents/MacOS/verba-daemon"
cp "$REPO_ROOT/target/release/verba-settings" "$APP/Contents/MacOS/verba-settings"
cp "$IME_ROOT/app/Info.plist" "$APP/Contents/Info.plist"

# 版本注入：以根 Cargo.toml 的 workspace 版本为唯一版本源，同步
# CFBundleShortVersionString / CFBundleVersion（只改拷贝，不弄脏源码树）。
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml")"
sed -i '' -E "s|<string>[0-9]+\.[0-9]+\.[0-9]+</string>|<string>$VERSION</string>|g" "$APP/Contents/Info.plist"

# 可选：捆绑 Rime（librime.dylib + data/），daemon 从 $APP/Contents/MacOS/rime/ 加载。
# 缺失时 daemon 日志会报 librime 加载失败，可用 VERBA_RIME_DYLIB/SHARED/USER 指向外部。
if [ -d "$REPO_ROOT/vendor/rime" ]; then
    # 需含版本化 dylib：librime.dylib 依赖 @rpath/librime.1.dylib，缺失会导致公证 Invalid
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
echo "安装: cp -R '$APP' \"\$HOME/Library/Input Methods/\""
echo "然后在 系统设置 → 键盘 → 输入法 中启用「拾言输入法」"
