#!/bin/bash
# Verba 输入法一键安装（DMG 内双击运行，issue #48 项 3）。
# 把同目录的 Verba.app 安装到 ~/Library/Input Methods（用户级，无需管理员），
# 调用 app 内 verba-register 写第三方输入源白名单并启用；无需拖到 /Applications。
set -euo pipefail
cd "$(dirname "$0")"

APP="$PWD/Verba.app"
[ -d "$APP" ] || { echo "错误：找不到同目录的 Verba.app" >&2; exit 1; }

DEST="$HOME/Library/Input Methods"
mkdir -p "$DEST"

if [ -d "$DEST/Verba.app" ]; then
    echo "更新安装：移除旧版 Verba.app（用户词库在 ~/Library/Application Support，不受影响）"
    rm -rf "$DEST/Verba.app"
fi
cp -R "$APP" "$DEST/"
echo "已安装到 $DEST/Verba.app，正在注册并启用输入源…"

"$DEST/Verba.app/Contents/MacOS/verba-register"
open "$DEST/Verba.app" 2>/dev/null || true
echo "完成。无需手动到系统设置添加；在输入法菜单选择「拾言输入法」即可。"
