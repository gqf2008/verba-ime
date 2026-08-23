#!/usr/bin/env bash
# 打包 macOS IMK .app：构建 Rust cdylib，并组装 VerbaIMK.app 目录。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --release --manifest-path "$ROOT/Cargo.toml"
LIB="$(find "$ROOT/target" -name 'libverba_ime_macos.dylib' | head -1)"
if [ -z "$LIB" ]; then echo "未找到 libverba_ime_macos.dylib"; exit 1; fi
APP="$ROOT/dist/VerbaIMK.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks"
cp "$ROOT/app/Info.plist" "$APP/Contents/Info.plist"
cp "$LIB" "$APP/Contents/Frameworks/libverba_ime_macos.dylib"
echo "打包完成: $APP"
