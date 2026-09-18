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

# 设置面板作为**独立应用**装到 /Applications（第 2 步/verba-settings-standalone-app）：
# DMG 里只有 bundle 内那一份（避免 19MB 复制两份），这里拷出去作为顶层 app；
# /Applications 不可写时回退 ~/Applications 并说明。
SETTINGS_SRC="$DEST/Contents/Library/Verba Settings.app"
SETTINGS_DST="/Applications/Verba 设置.app"
if [ -d "$SETTINGS_SRC" ]; then
    if rm -rf "$SETTINGS_DST" 2>/dev/null && cp -R "$SETTINGS_SRC" "$SETTINGS_DST" 2>/dev/null; then
        echo "设置面板已安装到 ${SETTINGS_DST}"
    else
        FALLBACK="$HOME/Applications/Verba 设置.app"
        mkdir -p "$HOME/Applications"
        rm -rf "$FALLBACK" 2>/dev/null || true
        if cp -R "$SETTINGS_SRC" "$FALLBACK" 2>/dev/null; then
            echo "设置面板已安装到 ${FALLBACK}（/Applications 不可写，回退用户级）"
        else
            echo "警告：设置面板安装失败；输入法菜单会自动回退到 bundle 内那份，功能不受影响" >&2
        fi
    fi
else
    echo "警告：bundle 内未找到 Verba Settings.app，跳过独立安装（第 1 步产物缺失？）" >&2
fi

echo "已安装到 ${DEST}，正在注册输入源…"
set +e
"$DEST/Contents/MacOS/verba-register"
rc=$?
set -e
open "$DEST" 2>/dev/null || true
case "$rc" in
    0)
        echo "完成。在输入法菜单选择「拾言输入法」即可。"
        ;;
    2)
        # macOS 26：第三方输入法进菜单必须过一次用户批准（只写偏好会被系统收回，
        # 真机 2026-09-17 实测）。verba-register 已把 系统设置 → 键盘 打开，
        # 这里如实说明「已装好、还差一次确认」，不能报成失败。
        echo "已安装。还差一次确认：在 系统设置 → 键盘 → 输入法 点「＋」添加「拾言输入法」，"
        echo "并在系统弹出「允许『拾言输入法』启用…」时选择允许（只需一次）。"
        ;;
    *)
        echo "错误：注册输入源失败（verba-register 退出码 $rc）。app 已安装，可重试：" >&2
        echo "  \"$DEST/Contents/MacOS/verba-register\"" >&2
        exit 1
        ;;
esac
