#!/bin/bash
# Verba 输入法一键安装（DMG 内双击运行，issue #48 项 3）。
# 把同目录的 Verba.app 安装到 ~/Library/Input Methods（用户级，无需管理员），
# 调用 app 内 verba-register 写第三方输入源白名单并启用；无需拖到 /Applications。
set -euo pipefail
cd "$(dirname "$0")"

APP="$PWD/Verba.app"
[ -d "$APP" ] || { echo "错误：找不到同目录的 Verba.app" >&2; exit 1; }
[ -x "$APP/Contents/MacOS/verba-register" ] || { echo "错误：Verba.app 缺少可执行的 verba-register" >&2; exit 1; }

DEST_DIR="$HOME/Library/Input Methods"
DEST="$DEST_DIR/Verba.app"
mkdir -p "$DEST_DIR"

STAGING="$DEST_DIR/.Verba.app.installing.$$"
BACKUP="$DEST_DIR/.Verba.app.previous.$$"
cleanup() { rm -rf "$STAGING"; }
trap cleanup EXIT

rm -rf "$STAGING"
cp -R "$APP" "$STAGING"
[ -x "$STAGING/Contents/MacOS/verba-register" ] || { echo "错误：暂存安装副本不完整" >&2; exit 1; }

# 更新前停止旧进程，避免旧二进制继续运行并干扰新版本。
/usr/bin/pkill -f "$DEST/Contents/MacOS/verba-mac" 2>/dev/null || true
/usr/bin/pkill -f "$DEST/Contents/MacOS/verba-daemon" 2>/dev/null || true

if [ -d "$DEST" ]; then
    echo "更新安装：替换旧版 Verba.app（用户词库在 ~/Library/Application Support，不受影响）"
    rm -rf "$BACKUP"
    mv "$DEST" "$BACKUP"
fi

if ! mv "$STAGING" "$DEST"; then
    if [ -d "$BACKUP" ]; then
        mv "$BACKUP" "$DEST"
    fi
    echo "错误：安装替换失败，已保留或恢复原版本" >&2
    exit 1
fi
rm -rf "$BACKUP"
trap - EXIT

echo "已安装到 ${DEST}，正在注册并启用输入源…"
"$DEST/Contents/MacOS/verba-register"
open "$DEST" 2>/dev/null || true
echo "完成。无需手动到系统设置添加；在输入法菜单选择「拾言输入法」即可。"
