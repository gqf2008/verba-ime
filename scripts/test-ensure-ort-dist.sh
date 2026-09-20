#!/usr/bin/env bash
# ensure-ort-dist.sh 的自检：全程无网络，且用 ORT_CACHE_DIR/ORT_DIST_CACHE 指到临时目录，
# **不碰真实 ort 缓存**（也不依赖本机是否已下过 dist）。
#
# 覆盖：--help / --list / --check 阳性对照（空缓存必红）/ --fix-cache 可逆移开 /
#       默认 emit（文档主用法 eval "$(…)"）/ -- <cmd> / --print-path / --offline 无缓存必红 /
#       ORT_CACHE_DIR 生效（与 ort-sys 同优先级）。
#
# **回归重点**：bash 3.2（macOS /bin/bash）在 `set -u` 下展开**空数组**的 "${arr[@]}" 会报
# `arr[@]: unbound variable` 并中断，于是 `eval "$(bash scripts/ensure-ort-dist.sh)"` 拿不到
# export、直接失败。故本脚本默认用 /bin/bash（macOS 上就是 3.2）跑被测脚本。
#
# 用法：bash scripts/test-ensure-ort-dist.sh
#   ORT_DIST_SCRIPT=<path>      被测脚本（默认本目录的 ensure-ort-dist.sh；可指向旧版做"修前必红"对照）
#   ORT_DIST_TEST_SHELL=<path>  跑被测脚本的 shell（默认 /bin/bash）
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SCRIPT="${ORT_DIST_SCRIPT:-$HERE/ensure-ort-dist.sh}"
[ -f "$SCRIPT" ] || { echo "找不到被测脚本：$SCRIPT" >&2; exit 2; }
if [ -n "${ORT_DIST_TEST_SHELL:-}" ]; then
    SHELL_BIN="$ORT_DIST_TEST_SHELL"
elif [ -x /bin/bash ]; then
    SHELL_BIN=/bin/bash
else
    SHELL_BIN=bash
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/test-ensure-ort-dist.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

CACHE="$TMP/ortcache"      # 假 ort 缓存根（ORT_CACHE_DIR；脚本会再拼 /dfbin/<target>/<hash>）
STORE="$TMP/diststore"     # 假本地 dist 缓存（ORT_DIST_CACHE）
TARGET="$(rustc -vV | awk '/^host: /{print $2}')"

passed=0
failed=0
pass() { echo "ok   - $1"; passed=$((passed + 1)); }
fail() { echo "FAIL - $1"; failed=$((failed + 1)); }

# 清掉可能污染结果的环境变量，并固定 ORT_CACHE_DIR/ORT_DIST_CACHE
run() {
    env -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET \
        ORT_CACHE_DIR="$CACHE" ORT_DIST_CACHE="$STORE" \
        "$SHELL_BIN" "$SCRIPT" "$@"
}

# 直跑（可换缓存根/加参数），用于 --offline 等需要不同环境的用例
run_with() {
    local cache="$1" store="$2"; shift 2
    env -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET \
        ORT_CACHE_DIR="$cache" ORT_DIST_CACHE="$store" \
        "$SHELL_BIN" "$SCRIPT" "$@"
}

# 注意：bash 3.2 下 `$VAR` 紧跟多字节字符（如全角括号）会被当成变量名的一部分而崩掉/输出乱码，
# 所以这些位置一律用 ${VAR}
echo "# 被测脚本：${SCRIPT}（shell: ${SHELL_BIN} · target: ${TARGET}）"

# 1) --help 不依赖缓存，直接可用
if run --help >/dev/null 2>&1; then pass "--help 退出 0"; else fail "--help 应退出 0"; fi

# 2) --list 必须给出该 target 的 dist 行（含 sha256，供 ORT_DIST_HASH 用）
LIST="$(run --list 2>/dev/null || true)"
HASH="$(printf '%s\n' "$LIST" | awk '/^  /{print $2; exit}')"
if printf '%s' "$HASH" | grep -Eq '^[0-9a-f]{64}$'; then
    pass "--list 给出 sha256（${HASH:0:12}…）"
else
    fail "--list 未给出 sha256（输出：${LIST:-空}）"
    echo "# 后续用例需要 dist 行，提前退出" >&2
    exit 1
fi

CACHE_HIT="$CACHE/dfbin/$TARGET/$HASH"
STORE_HIT="$STORE/$TARGET/$HASH"

# 3) 阳性对照：缓存目录存在但为空 → --check 必须判红（否则守卫是装饰）
mkdir -p "$CACHE_HIT"
if run --check >/dev/null 2>&1; then
    fail "空缓存目录时 --check 应非 0"
else
    pass "空缓存目录时 --check 判红（阳性对照）"
fi

# 4) --fix-cache 只做可逆移动（.broken-<ts>），不是删除
if run --fix-cache >/dev/null 2>&1 && [ ! -d "$CACHE_HIT" ] && ls -d "$CACHE_HIT".broken-* >/dev/null 2>&1; then
    pass "--fix-cache 把空缓存移成 .broken-<ts>（可逆、未删除）"
else
    fail "--fix-cache 未按预期把空缓存可逆移开"
fi

# 5) 回归点：dist 缓存命中 + 默认 emit → stdout 必须是可 eval 的一行 export
mkdir -p "$STORE_HIT"
: >"$STORE_HIT/libonnxruntime.a"
if OUT="$(run 2>/dev/null)"; then
    EXPECT="export ORT_LIB_LOCATION=$STORE_HIT"
    if [ "$OUT" = "$EXPECT" ]; then
        pass "默认 emit 输出恰好一行：$EXPECT"
    else
        fail "默认 emit 输出不符合预期（实际：${OUT:-空}）"
    fi
    if eval "$OUT" && [ "${ORT_LIB_LOCATION:-}" = "$STORE_HIT" ]; then
        pass "eval \"\$(…)\" 后 ORT_LIB_LOCATION 指向 dist 目录"
        unset ORT_LIB_LOCATION
    else
        fail "eval 后 ORT_LIB_LOCATION 不是 ${STORE_HIT}"
    fi
else
    fail "默认 emit 模式应退出 0（bash 3.2 空数组的 set -u 崩溃会走到这里）"
fi

# 6) -- <cmd>：把 ORT_LIB_LOCATION 透给子进程
# shellcheck disable=SC2016  # 故意单引号：让子 shell 自己展开 ORT_LIB_LOCATION
if GOT="$(run -- sh -c 'printf %s "${ORT_LIB_LOCATION:-}"' 2>/dev/null)" && [ "$GOT" = "$STORE_HIT" ]; then
    pass "-- <cmd> 形态把 ORT_LIB_LOCATION 传给子进程"
else
    fail "-- <cmd> 未把 ORT_LIB_LOCATION 传给子进程（实际：${GOT:-空}）"
fi

# 7) --print-path 与 emit 指向同一目录
if GOT="$(run --print-path 2>/dev/null)" && [ "$GOT" = "$STORE_HIT" ]; then
    pass "--print-path 输出与 emit 一致"
else
    fail "--print-path 输出不一致（实际：${GOT:-空}）"
fi

# 8) --offline 且本地无 dist → 必须判红（不能静默"成功"却什么都不指）
if run_with "$TMP/empty-cache" "$TMP/empty-store" --offline >/dev/null 2>&1; then
    fail "--offline 且本地无 dist 时应非 0"
else
    pass "--offline 且本地无 dist 时判红"
fi

# 9) ORT_CACHE_DIR 生效：命中假缓存 → --check 绿、--print-path 取 ort 缓存
mkdir -p "$CACHE_HIT"
: >"$CACHE_HIT/libonnxruntime.a"
if run --check >/dev/null 2>&1; then
    pass "ORT_CACHE_DIR 指向的缓存命中时 --check 绿"
else
    fail "ORT_CACHE_DIR 命中时 --check 应为 0"
fi
if GOT="$(run --print-path 2>/dev/null)" && [ "$GOT" = "$CACHE_HIT" ]; then
    pass "--print-path 优先取 ort 缓存目录"
else
    fail "--print-path 未取 ort 缓存目录（实际：${GOT:-空}）"
fi

# 10) ort 自己的缓存命中时，`-- <cmd>` 也必须真的执行命令。
#     与 6) 的区别：6) 走的是「本地 dist 缓存命中」，10) 走的是「ort 缓存命中」——
#     后者曾漏掉命令执行（只 log 一行就 return 0），于是缓存本已就绪的机器上
#     `bash scripts/ensure-ort-dist.sh -- cargo test` 什么都不跑却报成功，最难查的一类假绿。
if GOT="$(run -- sh -c 'printf %s ORT-CACHE-HIT-RAN' 2>/dev/null)" && [ "$GOT" = "ORT-CACHE-HIT-RAN" ]; then
    pass "ort 缓存命中时 -- <cmd> 仍然执行命令"
else
    fail "ort 缓存命中时 -- <cmd> 没有执行命令（实际：${GOT:-空}）"
fi

# 11) 同理钉住 emit 形态的契约：缓存就绪时 stdout 保持为空（不需要 export 环境变量），且退出 0
if OUT="$(run 2>/dev/null)" && [ -z "$OUT" ]; then
    pass "ort 缓存就绪时 emit 形态 stdout 为空（无需 export）"
else
    fail "ort 缓存就绪时 emit 形态输出异常（实际：${OUT:-空}）"
fi

echo "# 通过 $passed 项，失败 $failed 项"
[ "$failed" -eq 0 ]
