#!/bin/bash
# 离线验证 PKG postinstall 的 helper 成功/失败协议，不触碰真实输入源。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
POSTINSTALL="$ROOT/pkg-postinstall.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/verba-pkg-postinstall-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

APP="$TMP/Verba.app"
mkdir -p "$APP/Contents/MacOS"
touch "$APP/Contents/MacOS/verba-mac" "$APP/Contents/MacOS/verba-register"
chmod +x "$APP/Contents/MacOS/verba-mac" "$APP/Contents/MacOS/verba-register"

write_fake_launchctl() {
    local path="$1"
    local status="$2"
    local log="$3"
    cat > "$path" <<FAKE
#!/bin/bash
FAKE_STATUS=$(printf '%q' "$status")
FAKE_LOG=$(printf '%q' "$log")
STATUS=""
LOG=""
while [ "\$#" -gt 0 ]; do
    case "\$1" in
        --status) STATUS="\$2"; shift 2 ;;
        --log) LOG="\$2"; shift 2 ;;
        *) shift ;;
    esac
done
printf '%s\n' "\$FAKE_STATUS" > "\$STATUS"
printf '%s\n' "\$FAKE_LOG" > "\$LOG"
FAKE
    chmod +x "$path"
}

FAKE_OK="$TMP/fake-launchctl-ok"
write_fake_launchctl "$FAKE_OK" "0" "fake helper ok"
VERBA_PKG_APP="$APP" \
VERBA_PKG_CONSOLE_USER="$(id -un)" \
VERBA_PKG_CONSOLE_UID="$(id -u)" \
VERBA_PKG_LAUNCHCTL="$FAKE_OK" \
    bash "$POSTINSTALL" >/dev/null

FAKE_FAIL="$TMP/fake-launchctl-fail"
write_fake_launchctl "$FAKE_FAIL" "1" "fake helper failed"
if VERBA_PKG_APP="$APP" \
    VERBA_PKG_CONSOLE_USER="$(id -un)" \
    VERBA_PKG_CONSOLE_UID="$(id -u)" \
    VERBA_PKG_LAUNCHCTL="$FAKE_FAIL" \
    bash "$POSTINSTALL" >"$TMP/fail.out" 2>&1; then
    echo "error: helper status=1 时 postinstall 仍返回成功" >&2
    exit 1
fi
grep -q 'helper status=1' "$TMP/fail.out"

FAKE_LAUNCH_FAIL="$TMP/fake-launchctl-launch-fail"
cat > "$FAKE_LAUNCH_FAIL" <<'FAKE'
#!/bin/bash
exit 1
FAKE
chmod +x "$FAKE_LAUNCH_FAIL"
if VERBA_PKG_APP="$APP" \
    VERBA_PKG_CONSOLE_USER="$(id -un)" \
    VERBA_PKG_CONSOLE_UID="$(id -u)" \
    VERBA_PKG_LAUNCHCTL="$FAKE_LAUNCH_FAIL" \
    bash "$POSTINSTALL" >"$TMP/launch-fail.out" 2>&1; then
    echo "error: LaunchServices 启动失败时 postinstall 仍返回成功" >&2
    exit 1
fi
grep -q '启动 Verba 注册 helper 失败' "$TMP/launch-fail.out"

FAKE_NO_STATUS="$TMP/fake-launchctl-no-status"
cat > "$FAKE_NO_STATUS" <<'FAKE'
#!/bin/bash
exit 0
FAKE
chmod +x "$FAKE_NO_STATUS"
if VERBA_PKG_APP="$APP"     VERBA_PKG_CONSOLE_USER="$(id -un)"     VERBA_PKG_CONSOLE_UID="$(id -u)"     VERBA_PKG_POLL_COUNT=2     VERBA_PKG_LAUNCHCTL="$FAKE_NO_STATUS"     bash "$POSTINSTALL" >"$TMP/no-status.out" 2>&1; then
    echo "error: helper 不写状态时 postinstall 仍返回成功" >&2
    exit 1
fi
grep -q 'helper status=missing' "$TMP/no-status.out"

echo "PASS: pkg postinstall helper 成功/失败/无状态路径"
