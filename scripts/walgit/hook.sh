#!/usr/bin/env bash
# walgit collab watch --exec 回调。每个新/变更 collab ref 调一次。
#   stdin : entry blob 原文（entry JSON）
#   env   : WALGIT_COLLAB_REF / _KIND / _THREAD / _ACTOR / _VERIFIED
#
# 红线：**必须永远 exit 0**——watch 的 exec 非零退出会终止 watcher 进程（at-least-once 不推进）。
# 本钩子只做「过滤 + 去重 + 落队列 + 拉起 worker」，重活交给 worker.sh 异步做。
#
# 运行时状态（都不入库）在 $REPO/.git/walgit/：queue/ logs/ seen keys/ worktrees/
# 开闸标志：.git/walgit/ENABLE_AUTO → DRY_RUN=0；.git/walgit/ENABLE_MERGE → ALLOW_AUTO_MERGE=1
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WG="$REPO/.git/walgit"
QUEUE="$WG/queue"
LOGDIR="$WG/logs"
mkdir -p "$QUEUE" "$LOGDIR"

TS() { date '+%Y-%m-%dT%H:%M:%S%z'; }
log() { printf '%s [hook] %s\n' "$(TS)" "$*" >> "$LOGDIR/hook.log"; }

ENTRY="$(cat 2>/dev/null || true)"
kind="${WALGIT_COLLAB_KIND:-}"
thread="${WALGIT_COLLAB_THREAD:-}"
actor="${WALGIT_COLLAB_ACTOR:-}"
verified="${WALGIT_COLLAB_VERIFIED:-}"
ref="${WALGIT_COLLAB_REF:-}"

log "ref=$ref kind=$kind thread=$thread actor=$actor verified=$verified"

# 只接「新工作单元（issue）」；其余（comment/status/patch/review/snapshot/principal…）忽略。
[ "$kind" = "issue" ] || exit 0
# 只接验签通过的条目（未验签可能是伪造/未注册者）。
[ "$verified" = "true" ] || { log "skip unverified issue thread=$thread"; exit 0; }

cd "$REPO" 2>/dev/null || exit 0
oid="$(git rev-parse --verify --quiet "$ref" 2>/dev/null || true)"
[ -n "$oid" ] || oid="$(printf '%s' "$ENTRY" | git hash-object --stdin 2>/dev/null || true)"
[ -n "$oid" ] || { log "cannot resolve oid (ref=$ref)"; exit 0; }

SEEN="$WG/seen"
if grep -qxF "$oid" "$SEEN" 2>/dev/null; then exit 0; fi
echo "$oid" >> "$SEEN"

printf '%s' "$ENTRY" > "$QUEUE/$oid.json"
printf '%s|%s|%s|%s\n' "$oid" "$thread" "$actor" "issue" >> "$QUEUE/todo"
log "queued oid=$oid thread=$thread"

# 幂等拉起 worker（pid 文件存活则不重复起）。
if [ "${WALGIT_HOOK_NO_WORKER:-0}" = "1" ]; then log "worker autostart disabled (test)"; exit 0; fi
# 开闸标志（默认全关，安全）：ENABLE_AUTO → DRY_RUN=0；ENABLE_MERGE → ALLOW_AUTO_MERGE=1
if [ -f "$WG/ENABLE_AUTO" ]; then export DRY_RUN=0; else export DRY_RUN=1; fi
if [ -f "$WG/ENABLE_MERGE" ]; then export ALLOW_AUTO_MERGE=1; else export ALLOW_AUTO_MERGE=0; fi
if [ -f "$WG/worker.pid" ] && kill -0 "$(cat "$WG/worker.pid" 2>/dev/null)" 2>/dev/null; then
  :
else
  nohup "$REPO/scripts/walgit/worker.sh" >> "$LOGDIR/worker.out" 2>&1 &
  echo $! > "$WG/worker.pid"
  log "started worker pid=$(cat "$WG/worker.pid") DRY_RUN=$DRY_RUN ALLOW_AUTO_MERGE=$ALLOW_AUTO_MERGE"
fi
exit 0
