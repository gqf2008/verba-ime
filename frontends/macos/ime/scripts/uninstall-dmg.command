#!/bin/bash
# Verba 输入法一键卸载（DMG 内双击运行）。
# 先让 app 内 verba-register 移除当前用户的输入源条目，再删除用户级 app。
set -euo pipefail

DEST="$HOME/Library/Input Methods/Verba.app"

if [ -x "$DEST/Contents/MacOS/verba-register" ]; then
    "$DEST/Contents/MacOS/verba-register" --uninstall || true
fi

pkill -f "$DEST/Contents/MacOS/verba-mac" 2>/dev/null || true
pkill -f "$DEST/Contents/MacOS/verba-daemon" 2>/dev/null || true

if [ -d "$DEST" ]; then
    rm -rf "$DEST"
    echo "已卸载 $DEST"
else
    echo "未找到用户级 Verba.app，已尝试清理输入源条目。"
fi
