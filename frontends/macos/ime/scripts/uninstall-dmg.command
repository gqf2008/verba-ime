#!/bin/bash
# Verba 输入法一键卸载（DMG 内双击运行）。
# 先让 verba-register 移除当前用户的输入源条目，成功后删除用户级 app。
set -euo pipefail
cd "$(dirname "$0")"

DEST="$HOME/Library/Input Methods/Verba.app"
INSTALLED_HELPER="$DEST/Contents/MacOS/verba-register"
DMG_HELPER="$PWD/Verba.app/Contents/MacOS/verba-register"
cleanup_ok=0

if [ -x "$INSTALLED_HELPER" ]; then
    if "$INSTALLED_HELPER" --uninstall; then
        cleanup_ok=1
    else
        echo "警告：已安装版本清理失败，尝试使用 DMG 内 helper 重试" >&2
    fi
fi

if [ "$cleanup_ok" -eq 0 ] && [ -x "$DMG_HELPER" ]; then
    if "$DMG_HELPER" --uninstall; then
        cleanup_ok=1
    fi
fi

if [ "$cleanup_ok" -ne 1 ]; then
    echo "错误：输入源清理失败，已保留 $DEST 供重试。" >&2
    exit 1
fi

/usr/bin/pkill -f "$DEST/Contents/MacOS/verba-mac" 2>/dev/null || true
/usr/bin/pkill -f "$DEST/Contents/MacOS/verba-daemon" 2>/dev/null || true

if [ -d "$DEST" ]; then
    rm -rf "$DEST"
    echo "已卸载 $DEST"
else
    echo "已清理输入源条目；未找到用户级 Verba.app。"
fi

# 独立安装的设置面板（第 2 步）：/Applications 与 ~/Applications 两处都清。
for SETTINGS_APP in "/Applications/Verba 设置.app" "$HOME/Applications/Verba 设置.app"; do
    if [ -d "$SETTINGS_APP" ]; then
        if rm -rf "$SETTINGS_APP"; then
            echo "已卸载 $SETTINGS_APP"
        else
            echo "警告：未能删除 $SETTINGS_APP（可能需要管理员权限）" >&2
        fi
    fi
done
