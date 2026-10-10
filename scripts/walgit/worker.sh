#!/usr/bin/env bash
# 自动接单 worker（并发版）：主循环只做「扫描 + 派发」，每个任务跑在独立子进程里，
# 互不阻塞；带并发上限与运行锁（线程级去重、崩溃后回收）。
#
#   身份：verba-coordinator（接单/合并）、verba-worker-3（实现）、verba-reviewer-3（评审）
#   key ：$REPO/.git/walgit/keys/<principal>.ed25519（0600，项目内、不入库）
#
# 任务链：
#   issue  → jev 三分类(accept/needs-human/skip) → 认领 → worktree 实现 → patch → 评审 → 门禁合并
#   review → 独立评审（非作者）→ 门禁合并（base=main）
#
# 闸门（默认全关）：
#   DRY_RUN=1（默认）        只记日志、不写仓库、不发条目
#   DRY_RUN=0                真发签名条目 / 起子代理 / 建 worktree
#   ALLOW_AUTO_MERGE=1       门禁全过才自动合并到 origin/main
#   MAX_CONCURRENCY=3        同时运行的任务上限
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SELF="$REPO/scripts/walgit/worker.sh"
WG="$REPO/.git/walgit"
QUEUE="$WG/queue"
LOGDIR="$WG/logs"
KEYS="$WG/keys"
WORKTREES="$WG/worktrees"
RUNNING="$WG/running"
JEV="${JEV:-$HOME/.agents/skills/jev-decision/scripts/jev.py}"
PI="$(command -v pi || echo pi)"
COORD="verba-coordinator"
WORKER="verba-worker-3"
REVIEWER="verba-reviewer-3"
DRY_RUN="${DRY_RUN:-1}"
ALLOW_AUTO_MERGE="${ALLOW_AUTO_MERGE:-0}"
MAX_CONCURRENCY="${MAX_CONCURRENCY:-3}"
cd "$REPO" || exit 1
mkdir -p "$LOGDIR" "$QUEUE" "$WORKTREES" "$RUNNING"

W() { walgit --config ~/.walgit/walgit.toml "$@"; }
TS() { date '+%Y-%m-%dT%H:%M:%S%z'; }
log() { printf '%s [worker] %s\n' "$(TS)" "$*" >> "$LOGDIR/worker.log"; }
safe_key() { printf '%s' "$1" | tr -c 'A-Za-z0-9._-' '_'; }

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
field_of() { python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get(sys.argv[2],"") or "")' "$1" "$2" 2>/dev/null; }

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

run_review() { # <worktree> <thread> <title> [base_branch=main] → stdout: {"decision","note"}
  local wt="$1" thread="$2" title="$3" base="${4:-main}"
  local prompt="你是独立评审者（不得是作者）。工作目录是该 patch 的 head worktree。
用 \`git diff origin/${base}...HEAD\` 与 \`git log origin/${base}..HEAD\` 审查改动，对照任务：${title}。
判断能否合并进 ${base}。只输出一行 JSON（不要解释、不要代码块）：{\"decision\":\"approve\"或\"request_changes\",\"note\":\"理由\"}"
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

# ── issue 编排 ────────────────────────────────────────────────────────────────
dispatch() { # <thread> <title> <body>
  local thread="$1" title="$2" body="$3"
  local branch="auto/$thread"
  local wt="$WORKTREES/$thread"

  if [ "$DRY_RUN" = "1" ]; then
    log "DRY git worktree add -b $branch $wt origin/main"
  else
    git -C "$REPO" fetch origin -q
    git -C "$REPO" worktree add -b "$branch" "$wt" origin/main >>"$LOGDIR/worker.log" 2>&1 || {
      log "worktree add FAILED thread=$thread"
      emit comment "$thread" "$COORD" "$(thread_tip "$thread")" "{\"note\":\"自动实现中止：worktree 创建失败\"}" >/dev/null
      return 1; }
  fi

  run_impl "$wt" "$thread" "$title" "$body"

  local ncommits=1
  [ "$DRY_RUN" = "0" ] && ncommits="$(git -C "$wt" rev-list --count origin/main..HEAD 2>/dev/null || echo 0)"
  if [ "${ncommits:-0}" -eq 0 ]; then
    log "impl produced no commit thread=$thread"
    emit comment "$thread" "$COORD" "$(thread_tip "$thread")" "{\"note\":\"实现子代理未产出提交，已中止（见 .git/walgit/logs/impl-$thread.log）\"}" >/dev/null
    return 1
  fi

  if [ "$DRY_RUN" = "0" ]; then
    git -C "$wt" push -u origin "$branch" >>"$LOGDIR/worker.log" 2>&1 || { log "push FAILED thread=$thread"; return 1; }
  else
    log "DRY git push -u origin $branch"
  fi

  local p; p="$(thread_tip "$thread")"
  p="$(emit patch "$thread" "$WORKER" "$p" \
        "{\"title\":\"$title\",\"message\":\"自动实现（${WORKER}）\"}" \
        --base refs/heads/main --head "refs/heads/$branch")"
  log "patch emitted thread=$thread oid=$p"

  local rev; rev="$(run_review "$wt" "$thread" "$title" main)"
  local decision note
  decision="$(printf '%s' "$rev" | parse_decision)"; [ -n "$decision" ] || decision="request_changes"
  note="$(printf '%s' "$rev" | parse_note)"
  p="$(thread_tip "$thread")"
  p="$(emit review "$thread" "$REVIEWER" "$p" \
        "{\"decision\":\"$decision\",\"agent\":\"$REVIEWER\",\"note\":\"$note\"}")"
  log "review emitted thread=$thread decision=$decision oid=$p"

  if [ "$decision" != "approve" ]; then log "no merge：review=$decision"; return 0; fi
  if [ "$DRY_RUN" != "0" ] || [ "$ALLOW_AUTO_MERGE" != "1" ]; then log "merge skipped (gates)"; return 0; fi

  local ok
  ok="$(W collab pr "$thread" 2>/dev/null | python3 -c 'import sys,json
try:
  d=json.load(sys.stdin); m=d.get("merge") or {}
  print(bool(m.get("allowed")))
except Exception: print(False)')"
  [ "$ok" = "True" ] || { log "merge rule not ok thread=$thread → stop"; return 0; }

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

process() { # <key> <thread> <actor>
  local key="$1" thread="$2" actor="$3" f="$QUEUE/$(safe_key "$key").json"
  [ -f "$f" ] || f="$QUEUE/$key.json"
  [ -f "$f" ] || { log "missing entry $f"; return; }
  if [ "${WALGIT_WORKER_FORCE:-0}" != "1" ]; then
    thread_free "$thread" || { log "skip $thread: already claimed/terminal"; return; }
  fi

  local title body blob decision
  title="$(title_of "$f")"; body="$(body_of "$f")"; blob="$(cat "$f")"
  decision="$("$JEV" choice "仓库 verba-ime（Rust 跨平台 AI 输入法；主分支 main）。
issue 线程根内容（JSON）：$blob" \
    "这个 issue 该如何处理？$COORD 是自动接单方，目标是及时推进；合理就接。" \
    accept="合理、可自动推进：接单并实施" \
    needs-human="信息不足 / 需人工决策 / 超出自动能力：标记交给人类" \
    skip="明确无需处理（重复 / 已废弃 / 非本仓）" 2>>"$LOGDIR/worker.log" \
    | python3 -c 'import sys,json;print(json.load(sys.stdin).get("choice",""))' 2>/dev/null)"
  [ -n "${WALGIT_WORKER_FORCE_DECISION:-}" ] && decision="$WALGIT_WORKER_FORCE_DECISION"
  case "$decision" in
    accept|needs-human|skip) ;;
    *) decision="needs-human" ;;
  esac
  log "issue thread=$thread decision=$decision"

  local parent; parent="$(thread_tip "$thread")"
  if [ "$decision" = "needs-human" ]; then
    emit status "$thread" "$COORD" "$parent" \
      "{\"status\":\"needs-human\",\"owner\":\"\",\"note\":\"自动分诊（jev）：需人工判断后处理\"}" >/dev/null
    return
  fi
  if [ "$decision" = "skip" ]; then
    emit comment "$thread" "$COORD" "$parent" \
      "{\"note\":\"自动分诊（jev）：判为无需处理（重复/已废弃/非本仓）；如需处理请直接评论。\"}" >/dev/null
    return
  fi

  parent="$(emit status "$thread" "$COORD" "$parent" \
    "{\"status\":\"in-progress\",\"owner\":\"$COORD\",\"work\":\"auto-dispatch\",\"note\":\"由 $COORD 自动认领（jev 判定接单）\"}")"
  log "claimed thread=$thread parent=$parent"
  dispatch "$thread" "$title" "$body"
}

# ── needs-review 编排 ─────────────────────────────────────────────────────────
do_review_merge() { # <thread> <title> [branch]
  local thread="$1" title="$2" branch_hint="${3:-}"
  local pr head base allowed approved branch base_branch wt rev decision note p mwt moid
  pr="$(W collab pr "$thread" 2>/dev/null)"
  [ -n "$pr" ] || { log "review: no pr for $thread"; return; }
  head="$(printf '%s' "$pr" | python3 -c 'import sys,json;print((json.load(sys.stdin).get("pr") or {}).get("head") or "")' 2>/dev/null)"
  base="$(printf '%s' "$pr" | python3 -c 'import sys,json;print((json.load(sys.stdin).get("pr") or {}).get("base") or "")' 2>/dev/null)"
  allowed="$(printf '%s' "$pr" | python3 -c 'import sys,json;print(bool((json.load(sys.stdin).get("merge") or {}).get("allowed")))' 2>/dev/null)"
  approved="$(printf '%s' "$pr" | python3 -c '
import sys, json
d = json.load(sys.stdin); pr = d.get("pr", {})
authors = set(pr.get("authors", []))
revs = list(pr.get("reviews", [])) + list(pr.get("human_approvals", []))
print(1 if any(r.get("decision") == "approve" and r.get("actor") not in authors for r in revs) else 0)' 2>/dev/null)"
  [ -n "$head" ] || head="refs/heads/$branch_hint"
  [ -n "$base" ] || base="refs/heads/main"
  branch="${head#refs/heads/}"; base_branch="${base#refs/heads/}"
  [ -n "$branch" ] || { log "review: no head/branch for $thread"; return; }

  if [ "$approved" = "1" ]; then
    decision="approve"; log "review $thread: 已有非作者 approve，跳过评审"
  else
    wt="$WORKTREES/review-$thread"
    if [ "$DRY_RUN" = "1" ]; then
      log "DRY review worktree+pi for $thread (base=$base_branch)"; decision="approve"; note="dry-run"
    else
      git -C "$REPO" fetch origin -q
      git -C "$REPO" worktree add --detach "$wt" "origin/$branch" >>"$LOGDIR/worker.log" 2>&1 || { log "review worktree FAILED $thread"; return; }
      rev="$(run_review "$wt" "$thread" "$title" "$base_branch")"
      decision="$(printf '%s' "$rev" | parse_decision)"; [ -n "$decision" ] || decision="request_changes"
      note="$(printf '%s' "$rev" | parse_note)"
    fi
    p="$(thread_tip "$thread")"
    emit review "$thread" "$REVIEWER" "$p" "{\"decision\":\"$decision\",\"agent\":\"$REVIEWER\",\"note\":\"${note:-独立评审}\"}" >/dev/null
    log "review emitted $thread decision=$decision"
  fi

  [ "$decision" = "approve" ] || { log "review $thread: $decision，不合并"; return; }
  [ "$allowed" = "True" ] || { log "review $thread: merge rule 不通过，不合并"; return; }
  [ "$base" = "refs/heads/main" ] || { log "review $thread: base=$base 非 main，留人工合并"; return; }
  if [ "$DRY_RUN" != "0" ] || [ "$ALLOW_AUTO_MERGE" != "1" ]; then log "review $thread: merge skipped (gates)"; return; fi

  git -C "$REPO" fetch origin -q
  mwt="$WORKTREES/.merge-$thread"
  git -C "$REPO" worktree add --detach "$mwt" origin/main >>"$LOGDIR/worker.log" 2>&1 || { log "review merge wt FAILED $thread"; return; }
  if git -C "$mwt" merge --no-ff "origin/$branch" -m "merge: $thread" >>"$LOGDIR/worker.log" 2>&1; then
    moid="$(git -C "$mwt" rev-parse HEAD)"
    git -C "$mwt" push origin "HEAD:main" >>"$LOGDIR/worker.log" 2>&1 || { log "review merge push FAILED $thread"; return; }
    emit merge_result "$thread" "$COORD" "$(thread_tip "$thread")" \
      "{\"merged\":true,\"oid\":\"$moid\",\"result\":\"merged\",\"note\":\"自动评审+合并（${COORD}）\"}" >/dev/null
    log "MERGED(review) $thread oid=$moid"
  else
    log "review merge conflict $thread"
    emit comment "$thread" "$COORD" "$(thread_tip "$thread")" "{\"note\":\"自动合并冲突，需人工处理\"}" >/dev/null
  fi
}

# ── 主动扫描（只入队，不阻塞主循环）─────────────────────────────────────────
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
        title = (c.get("title","") or "").replace("\t"," ").replace("\n"," ").replace("\r"," ")[:200]
        prose = (c.get("prose","") or "").replace("\t"," ").replace("\n"," ").replace("\r"," ")[:800]
        print(f"{tid}\t{title}\t{prose}")
' | while IFS=$'\t' read -r tid title prose; do
    [ -n "$tid" ] || continue
    key="open:$tid"
    grep -qxF "$key" "$WG/seen" 2>/dev/null && continue
    echo "$key" >> "$WG/seen"
    python3 -c 'import json,sys;print(json.dumps({"body":{"title":sys.argv[1],"body":sys.argv[2]}}))' "$title" "$prose" > "$QUEUE/$(safe_key "$key").json"
    printf '%s|%s|%s|%s\n' "$key" "$tid" "scan" "issue" >> "$QUEUE/todo"
    log "scan enqueued open thread=$tid"
  done
}

scan_needs_review() {
  W collab board --format json 2>/dev/null | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for col in d.get("columns", []):
    if col.get("name") != "needs-review": continue
    for c in col.get("cards", []):
        tid = c.get("id", "")
        if not tid: continue
        title = (c.get("title","") or "").replace("\t"," ").replace("\n"," ").replace("\r"," ")[:200]
        branch = (c.get("branch","") or "").replace("\t"," ").strip()
        print(f"{tid}\t{title}\t{branch}")
' | while IFS=$'\t' read -r tid title branch; do
    [ -n "$tid" ] || continue
    key="review:$tid"
    grep -qxF "$key" "$WG/seen" 2>/dev/null && continue
    echo "$key" >> "$WG/seen"
    python3 -c 'import json,sys;print(json.dumps({"title":sys.argv[1],"branch":sys.argv[2]}))' "$title" "$branch" > "$QUEUE/$(safe_key "$key").json"
    printf '%s|%s|%s|%s\n' "$key" "$tid" "" "review" >> "$QUEUE/todo"
    log "scan enqueued needs-review thread=$tid"
  done
}

# ── 派发（不阻塞）────────────────────────────────────────────────────────────
running_count() { find "$RUNNING" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' '; }

cleanup_locks() {
  local d pid
  for d in "$RUNNING"/*; do
    [ -d "$d" ] || continue
    pid="$(cat "$d/pid" 2>/dev/null || true)"
    if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then rm -rf "$d"; fi
  done
}

drain() {
  cleanup_locks
  while [ -s "$QUEUE/todo" ]; do
    local line key thread actor jobtype lock
    line="$(head -1 "$QUEUE/todo")"
    tail -n +2 "$QUEUE/todo" > "$QUEUE/todo.tmp" 2>/dev/null && mv "$QUEUE/todo.tmp" "$QUEUE/todo"
    IFS='|' read -r key thread actor jobtype <<<"$line"
    [ -n "${key:-}" ] || continue
    while [ "$(running_count)" -ge "$MAX_CONCURRENCY" ]; do cleanup_locks; sleep 2; done
    lock="$RUNNING/$(safe_key "$key")"
    mkdir "$lock" 2>/dev/null || continue   # 已有同名任务在跑 → 跳过
    nohup "$SELF" --task "$key" "$thread" "${actor:-}" "${jobtype:-issue}" >>"$LOGDIR/worker.log" 2>&1 &
    echo $! > "$lock/pid"
    log "dispatched key=$key thread=$thread job=${jobtype:-issue} pid=$(cat "$lock/pid")"
  done
}

task() { # <key> <thread> <actor> <jobtype>
  local key="$1" thread="$2" actor="$3" jobtype="$4"
  local lock="$RUNNING/$(safe_key "$key")"
  trap "rm -rf '$lock'" EXIT
  log "task start key=$key thread=$thread job=$jobtype pid=$$"
  if [ "$jobtype" = "review" ]; then
    local f="$QUEUE/$(safe_key "$key").json"
    do_review_merge "$thread" "$(field_of "$f" title)" "$(field_of "$f" branch)"
  else
    process "$key" "$thread" "$actor"
  fi
  log "task done key=$key pid=$$"
}

main() {
  if [ "${1:-}" = "--task" ]; then shift; task "$@"; exit $?; fi
  log "worker start pid=$$ DRY_RUN=$DRY_RUN ALLOW_AUTO_MERGE=$ALLOW_AUTO_MERGE MAX_CONCURRENCY=$MAX_CONCURRENCY once=${1:-}"
  LAST_SCAN=0
  if [ "${1:-}" = "--once" ]; then
    scan_open; scan_needs_review
    drain
    # --once：等所有派发任务结束
    while [ "$(running_count)" -gt 0 ]; do cleanup_locks; sleep 2; done
    log "worker --once done"; exit 0
  fi
  while :; do
    local now; now="$(date +%s)"
    if [ $((now - LAST_SCAN)) -ge 30 ]; then scan_open; scan_needs_review; LAST_SCAN="$now"; fi
    drain
    sleep 5
  done
}
main "$@"
