#!/usr/bin/env bash
# 自动接单 worker：消费 .git/walgit/queue/todo，做「分诊→认领→实现→评审→合并」编排。
#
#   身份：verba-coordinator（接单/合并）、verba-worker-3（实现）、verba-reviewer-3（评审）
#   key ：$REPO/.git/walgit/keys/<principal>.ed25519（0600，项目内、不入库）
#
# 闸门（默认全关，逐级放开）：
#   DRY_RUN=1（默认）         只记日志、不写仓库、不发条目
#   DRY_RUN=0                 真发签名条目 / 起子代理 / 建 worktree
#   ALLOW_AUTO_MERGE=1        在 DRY_RUN=0 且评审 approve 且 merge rule 通过时，才自动合并到 origin/main
# 由 hook.sh 依 .git/walgit/ENABLE_AUTO、ENABLE_MERGE 标志导出上述变量；也可手工：
#   DRY_RUN=0 ALLOW_AUTO_MERGE=1 scripts/walgit/worker.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WG="$REPO/.git/walgit"
QUEUE="$WG/queue"
LOGDIR="$WG/logs"
KEYS="$WG/keys"
WORKTREES="$WG/worktrees"
JEV="${JEV:-$HOME/.agents/skills/jev-decision/scripts/jev.py}"
PI="$(command -v pi || echo pi)"
COORD="verba-coordinator"
WORKER="verba-worker-3"
REVIEWER="verba-reviewer-3"
DRY_RUN="${DRY_RUN:-1}"
ALLOW_AUTO_MERGE="${ALLOW_AUTO_MERGE:-0}"
cd "$REPO" || exit 1
mkdir -p "$LOGDIR" "$QUEUE" "$WORKTREES"

W() { walgit --config ~/.walgit/walgit.toml "$@"; }
TS() { date '+%Y-%m-%dT%H:%M:%S%z'; }
log() { printf '%s [worker] %s\n' "$(TS)" "$*" >> "$LOGDIR/worker.log"; }

# emit <kind> <id> <actor> <parent> <body-json> [extra args…] → stdout: entry oid
emit() {
  local kind="$1" id="$2" actor="$3" parent="$4" body="$5" out
  shift 5
  if [ "$DRY_RUN" = "1" ]; then
    log "DRY emit kind=$kind id=$id actor=$actor parent=${parent:-<root>} extra=[$*] body=$body"
    printf 'dry-oid-%s' "$(printf '%s%s' "$body" "$*" | git hash-object --stdin | cut -c1-12)"
    return 0
  fi
  out="$(W collab entry --kind "$kind" --id "$id" --actor "$actor" --parent "$parent" \
        --body "$body" --key "$KEYS/$actor.ed25519" "$@" --push origin 2>>"$LOGDIR/worker.log")" || {
    log "emit FAILED kind=$kind id=$id actor=$actor"; return 1; }
  printf '%s' "$out" | awk '{print $NF}' | tail -1
}

thread_tip() {
  W collab thread "$1" 2>/dev/null | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d[-1]["oid"] if d else "")' 2>/dev/null
}

thread_free() {
  W collab thread "$1" 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(1)
claimed = {"in-progress", "needs-review", "merged", "done"}
for e in d:
    ent = e.get("entry", {}); k = ent.get("kind"); b = ent.get("body", {}) or {}
    if k == "status" and b.get("status") in claimed: sys.exit(1)
    if k == "merge_result" and b.get("merged"): sys.exit(1)
sys.exit(0)'
}

title_of() { python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));b=d.get("body",{}) if isinstance(d.get("body"),dict) else {};print(b.get("title",""))' "$1" 2>/dev/null; }
body_of()  { python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));b=d.get("body",{}) if isinstance(d.get("body"),dict) else {};print(b.get("body",""))' "$1" 2>/dev/null; }

# ── 子代理 ────────────────────────────────────────────────────────────────────
run_impl() { # <worktree> <thread> <title> <body>
  local wt="$1" thread="$2" title="$3" body="$4"
  local prompt="你是 verba-ime（Rust 跨平台 AI 输入法）的实现子代理。当前工作目录是本 issue 的分支 worktree。
任务（协作线程 id：${thread}）：${title}
${body}

要求：
- 只在本分支实现该 issue，改必要的文件；跑相关验证（cargo check / cargo test / fmt）。
- 完成后 git add -A && git commit（一个提交，信息说清改了什么）。
- 不要 push、不要切换/创建其它分支、不要动 main。
最后用一句话总结改动与验证结果。"
  if [ "$DRY_RUN" = "1" ]; then log "DRY pi -p (impl) wt=$wt"; return 0; fi
  ( cd "$wt" && "$PI" -p "$prompt" --approve ) >>"$LOGDIR/impl-$thread.log" 2>&1
}

run_review() { # <worktree> <thread> <title> → stdout: {"decision","note"}
  local wt="$1" thread="$2" title="$3"
  local prompt="你是独立评审者（不得是作者）。工作目录是本 issue 的分支 worktree。
用 \`git diff origin/main...HEAD\` 与 \`git log origin/main..HEAD\` 审查改动，对照任务：${title}。
判断能否合并进 main。只输出一行 JSON（不要解释、不要代码块）：{\"decision\":\"approve\"或\"request_changes\",\"note\":\"理由\"}"
  if [ "$DRY_RUN" = "1" ]; then echo '{"decision":"approve","note":"dry-run"}'; return 0; fi
  ( cd "$wt" && "$PI" -p "$prompt" --approve ) 2>>"$LOGDIR/review-$thread.log"
}

parse_decision() {
  python3 -c '
import sys, json, re
s = sys.stdin.read()
m = re.search(r"\{.*\}", s, re.S)
if not m: print("request_changes"); sys.exit(0)
try: print(json.loads(m.group(0)).get("decision", "request_changes"))
except Exception: print("request_changes")' 2>/dev/null
}

parse_note() {
  python3 -c '
import sys, json, re
s = sys.stdin.read()
m = re.search(r"\{.*\}", s, re.S)
if not m: print(""); sys.exit(0)
try: print(json.loads(m.group(0)).get("note", "")[:500])
except Exception: print("")' 2>/dev/null
}

# ── 编排 ──────────────────────────────────────────────────────────────────────
dispatch() { # <thread> <title> <body>
  local thread="$1" title="$2" body="$3"
  local branch="auto/$thread"
  local wt="$WORKTREES/$thread"

  # 1) worktree + 分支（基于 origin/main）
  if [ "$DRY_RUN" = "1" ]; then
    log "DRY git worktree add -b $branch $wt origin/main"
  else
    git -C "$REPO" fetch origin -q
    git -C "$REPO" worktree add -b "$branch" "$wt" origin/main >>"$LOGDIR/worker.log" 2>&1 || {
      log "worktree add FAILED thread=$thread"
      emit comment "$thread" "$COORD" "$(thread_tip "$thread")" "{\"note\":\"自动实现中止：worktree 创建失败\"}" >/dev/null
      return 1; }
  fi

  # 2) 实现子代理
  run_impl "$wt" "$thread" "$title" "$body"

  # 3) 是否有提交
  local ncommits=1
  if [ "$DRY_RUN" = "0" ]; then
    ncommits="$(git -C "$wt" rev-list --count origin/main..HEAD 2>/dev/null || echo 0)"
  fi
  if [ "${ncommits:-0}" -eq 0 ]; then
    log "impl produced no commit thread=$thread"
    emit comment "$thread" "$COORD" "$(thread_tip "$thread")" "{\"note\":\"实现子代理未产出提交，已中止（见 .git/walgit/logs/impl-$thread.log）\"}" >/dev/null
    return 1
  fi

  # 4) 推分支
  if [ "$DRY_RUN" = "0" ]; then
    git -C "$wt" push -u origin "$branch" >>"$LOGDIR/worker.log" 2>&1 || { log "push FAILED thread=$thread"; return 1; }
  else
    log "DRY git push -u origin $branch"
  fi

  # 5) patch 条目（verba-worker-3）
  local p; p="$(thread_tip "$thread")"
  p="$(emit patch "$thread" "$WORKER" "$p" \
        "{\"title\":\"$title\",\"message\":\"自动实现（${WORKER}）\"}" \
        --base refs/heads/main --head "refs/heads/$branch")"
  log "patch emitted thread=$thread oid=$p"

  # 6) 独立评审（verba-reviewer-3）
  local rev; rev="$(run_review "$wt" "$thread" "$title")"
  local decision note
  decision="$(printf '%s' "$rev" | parse_decision)"; [ -n "$decision" ] || decision="request_changes"
  note="$(printf '%s' "$rev" | parse_note)"
  p="$(thread_tip "$thread")"
  p="$(emit review "$thread" "$REVIEWER" "$p" \
        "{\"decision\":\"$decision\",\"agent\":\"$REVIEWER\",\"note\":\"$note\"}")"
  log "review emitted thread=$thread decision=$decision oid=$p"

  # 7) 合并（门禁：DRY_RUN=0 + approve + merge rule 通过 + ALLOW_AUTO_MERGE=1）
  if [ "$decision" != "approve" ]; then log "no merge：review=$decision"; return 0; fi
  if [ "$DRY_RUN" != "0" ] || [ "$ALLOW_AUTO_MERGE" != "1" ]; then log "merge skipped (gates: DRY_RUN=$DRY_RUN ALLOW_AUTO_MERGE=$ALLOW_AUTO_MERGE)"; return 0; fi

  local ok
  ok="$(W collab pr "$thread" 2>/dev/null | python3 -c 'import sys,json
try:
  d=json.load(sys.stdin); m=d.get("merge") or {}
  print(m.get("ok", m.get("mergeable", False)))
except Exception: print(False)')"
  if [ "$ok" != "True" ]; then log "merge rule not ok thread=$thread → stop"; return 0; fi

  git -C "$REPO" fetch origin -q
  local mwt="$WORKTREES/.merge-$thread"
  git -C "$REPO" worktree add --detach "$mwt" origin/main >>"$LOGDIR/worker.log" 2>&1 || { log "merge worktree FAILED"; return 1; }
  if git -C "$mwt" merge --no-ff "$branch" -m "merge: $thread" >>"$LOGDIR/worker.log" 2>&1; then
    local moid; moid="$(git -C "$mwt" rev-parse HEAD)"
    git -C "$mwt" push origin "HEAD:main" >>"$LOGDIR/worker.log" 2>&1 || { log "merge push FAILED thread=$thread"; return 1; }
    emit merge_result "$thread" "$COORD" "$(thread_tip "$thread")" \
      "{\"merged\":true,\"oid\":\"$moid\",\"result\":\"merged\",\"note\":\"自动合并（${COORD}）\"}" >/dev/null
    log "MERGED thread=$thread oid=$moid"
  else
    log "merge conflict thread=$thread"
    emit comment "$thread" "$COORD" "$(thread_tip "$thread")" "{\"note\":\"自动合并冲突，需人工处理\"}" >/dev/null
  fi
}

process() { # <oid> <thread> <actor>
  local oid="$1" thread="$2" actor="$3" f="$QUEUE/$oid.json"
  [ -f "$f" ] || { log "missing entry $f"; return; }
  if [ "${WALGIT_WORKER_FORCE:-0}" != "1" ]; then
    thread_free "$thread" || { log "skip $thread: already claimed/terminal"; return; }
  else
    log "FORCE: skip thread_free check for $thread"
  fi

  local title body blob decision
  title="$(title_of "$f")"; body="$(body_of "$f")"; blob="$(cat "$f")"
  decision="$("$JEV" choice "仓库 verba-ime（Rust 跨平台 AI 输入法；主分支 main）。
issue 线程根内容（JSON）：$blob" \
    "这个 issue 是否该由 $COORD 自动接单并进入实现？" \
    accept="接单并实现" reject="不接，仅记录理由" 2>>"$LOGDIR/worker.log" \
    | python3 -c 'import sys,json;print(json.load(sys.stdin).get("choice","reject"))' 2>/dev/null)"
  [ -n "$decision" ] || decision="reject"
  [ -n "${WALGIT_WORKER_FORCE_DECISION:-}" ] && decision="$WALGIT_WORKER_FORCE_DECISION"
  log "issue thread=$thread title=$title decision=$decision"

  local parent; parent="$(thread_tip "$thread")"
  if [ "$decision" != "accept" ]; then
    emit comment "$thread" "$COORD" "$parent" \
      "{\"note\":\"自动分诊（jev）：不接本 issue，需人工处理时请直接评论。\"}" >/dev/null
    return
  fi

  parent="$(emit status "$thread" "$COORD" "$parent" \
    "{\"status\":\"in-progress\",\"owner\":\"$COORD\",\"work\":\"auto-dispatch\",\"note\":\"由 $COORD 自动认领（jev 判定接单）\"}")"
  log "claimed thread=$thread parent=$parent"

  dispatch "$thread" "$title" "$body"
}

# 主动拉取：扫看板 `open` 泳道（status=open、未认领），入队。
# 这是对「只被动等新 ref」的补齐：已存在的 open issue 不会再来 ref 事件，必须主动拉。
scan_open() {
  W collab board --format json 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for col in d.get("columns", []):
    if col.get("name") != "open": continue
    for c in col.get("cards", []):
        tid = c.get("id", "")
        if not tid: continue
        title = (c.get("title", "") or "").replace("\t", " ").replace("\n", " ").replace("\r", " ")[:200]
        prose = (c.get("prose", "") or "").replace("\t", " ").replace("\n", " ").replace("\r", " ")[:800]
        print(f"{tid}\t{title}\t{prose}")
' | while IFS=$'\t' read -r tid title prose; do
    [ -n "$tid" ] || continue
    key="open:$tid"
    grep -qxF "$key" "$WG/seen" 2>/dev/null && continue
    echo "$key" >> "$WG/seen"
    python3 -c 'import json,sys;print(json.dumps({"body":{"title":sys.argv[1],"body":sys.argv[2]}}))' "$title" "$prose" > "$QUEUE/$key.json"
    printf '%s\t%s\t%s\n' "$key" "$tid" "scan" >> "$QUEUE/todo"
    log "scan enqueued open thread=$tid"
  done
}

drain() {
  while [ -s "$QUEUE/todo" ]; do
    local line oid thread actor
    line="$(head -1 "$QUEUE/todo")"
    tail -n +2 "$QUEUE/todo" > "$QUEUE/todo.tmp" 2>/dev/null && mv "$QUEUE/todo.tmp" "$QUEUE/todo"
    IFS=$'\t' read -r oid thread actor <<<"$line"
    [ -n "${oid:-}" ] && process "$oid" "$thread" "${actor:-}"
  done
}

main() {
  log "worker start pid=$$ DRY_RUN=$DRY_RUN ALLOW_AUTO_MERGE=$ALLOW_AUTO_MERGE once=${1:-}"
  LAST_SCAN=0
  if [ "${1:-}" = "--once" ]; then scan_open; drain; log "worker --once done"; exit 0; fi
  while :; do
    local now; now="$(date +%s)"
    if [ $((now - LAST_SCAN)) -ge 30 ]; then scan_open; LAST_SCAN="$now"; fi
    drain
    sleep 5
  done
}
main "${1:-}"
