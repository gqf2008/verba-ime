#!/bin/bash
# Verba PKG postinstall：在 console 用户会话内通过 LaunchServices 启动 GUI helper。
#
# package_script_service 沙盒内直接调用 verba-register 会被拒绝写用户偏好；
# open 由 LaunchServices 在用户 Aqua session 内拉起 Verba.app 的 --register
# 短命模式，再由它调用同 bundle 的 verba-register。helper 的退出码写入临时
# 文件，postinstall 据此 fail-closed，避免“app 装上了但输入源没启用”仍报成功。
set -u

APP="${VERBA_PKG_APP:-/Library/Input Methods/Verba.app}"
CONSOLE_USER="${VERBA_PKG_CONSOLE_USER:-$(/usr/bin/stat -f%Su /dev/console 2>/dev/null || true)}"

if [ -z "$CONSOLE_USER" ] || [ "$CONSOLE_USER" = "root" ]; then
    echo "error: 无法确定当前 console 用户，拒绝在无用户会话时注册输入源" >&2
    exit 1
fi

CONSOLE_UID="${VERBA_PKG_CONSOLE_UID:-$(/usr/bin/id -u "$CONSOLE_USER" 2>/dev/null || true)}"
if [ -z "$CONSOLE_UID" ]; then
    echo "error: 无法解析 console 用户 UID: $CONSOLE_USER" >&2
    exit 1
fi

if [ ! -x "$APP/Contents/MacOS/verba-mac" ] || [ ! -x "$APP/Contents/MacOS/verba-register" ]; then
    echo "error: $APP 缺少可执行的 verba-mac/verba-register" >&2
    exit 1
fi

STATUS_DIR="$(/usr/bin/mktemp -d /tmp/verba-register.XXXXXX 2>/dev/null || true)"
if [ -z "$STATUS_DIR" ]; then
    echo "error: 无法创建 helper 状态目录" >&2
    exit 1
fi
# shellcheck disable=SC2329  # trap EXIT 间接调用
cleanup() { /bin/rm -rf "$STATUS_DIR"; }
trap cleanup EXIT

# 目录保持 root-owned 且不可被用户替换；只给两个固定 regular 文件写权限。
# 这样 helper 只能截断/写内容，不能把 status 换成 FIFO、设备或符号链接。
/bin/chmod 755 "$STATUS_DIR"
STATUS_FILE="$STATUS_DIR/status"
LOG_FILE="$STATUS_DIR/register.log"
: > "$STATUS_FILE"
: > "$LOG_FILE"
/bin/chmod 600 "$STATUS_FILE" "$LOG_FILE"
if ! /usr/sbin/chown "$CONSOLE_UID" "$STATUS_FILE" "$LOG_FILE" 2>/dev/null; then
    echo "error: 无法把 helper 状态文件交给 console 用户 $CONSOLE_USER" >&2
    exit 1
fi

LAUNCHCTL="${VERBA_PKG_LAUNCHCTL:-/bin/launchctl}"
# 不使用 open -W：旧 payload 若忽略 --register 会进入 IMK 主循环并永久阻塞安装。
# open 只负责交给 LaunchServices，随后由 root 侧有界轮询状态文件。
if ! "$LAUNCHCTL" asuser "$CONSOLE_UID" /usr/bin/sudo -u "$CONSOLE_USER" --     /usr/bin/open -n "$APP" --args --register --status "$STATUS_FILE" --log "$LOG_FILE"; then
    echo "error: 在用户会话内启动 Verba 注册 helper 失败" >&2
    if [ -f "$LOG_FILE" ] && [ ! -L "$LOG_FILE" ]; then
        /usr/bin/head -c 65536 "$LOG_FILE" 2>/dev/null | /usr/bin/sed 's/^/  /' >&2 || true
    fi
    exit 1
fi

STATUS=""
for _ in $(/usr/bin/seq 1 80); do
    if [ -f "$STATUS_FILE" ] && [ ! -L "$STATUS_FILE" ]; then
        STATUS="$(/usr/bin/head -c 64 "$STATUS_FILE" 2>/dev/null || true)"
        [ -n "$STATUS" ] && break
    fi
    /bin/sleep 0.25
done

if [ "$STATUS" != "0" ]; then
    echo "error: Verba 输入源自动启用失败（helper status=${STATUS:-missing}）" >&2
    if [ -f "$LOG_FILE" ] && [ ! -L "$LOG_FILE" ]; then
        /usr/bin/head -c 65536 "$LOG_FILE" 2>/dev/null | /usr/bin/sed 's/^/  /' >&2 || true
    fi
    exit 1
fi

echo "Verba 输入源已在用户会话内注册并启用"
exit 0
