#!/usr/bin/env bash
# ensure-ort-dist.sh 的自检：全程无网络，且用 ORT_CACHE_DIR/ORT_DIST_CACHE 指到临时目录，
# **不碰真实 ort 缓存**（也不依赖本机是否已下过 dist）。冒烟所需的 dist.tsv 由本脚本临时**合成**
# （假 cargo home + CARGO_HOME 指过去），因此不依赖本机 cargo registry 里是否解包了 ort-sys。
#
# 覆盖：--help / --list / --check 阳性对照（空缓存必红）/ --fix-cache 可逆移开 /
#       默认 emit（文档主用法 eval "$(…)"）/ -- <cmd> / --print-path / --offline 无缓存必红 /
#       ORT_CACHE_DIR 生效（与 ort-sys 同优先级）。
#
# **回归重点**：
#   1) bash 3.2（macOS /bin/bash）在 `set -u` 下展开**空数组**的 "${arr[@]}" 会报
#      `arr[@]: unbound variable` 并中断，于是 `eval "$(bash scripts/ensure-ort-dist.sh)"` 拿不到
#      export、直接失败。故本脚本默认用 /bin/bash（macOS 上就是 3.2）跑被测脚本。
#   2) Windows 语义的四个修复点各有用例钉住（能红→绿地证明修掉了）：
#      14) $HOME/.cargo 不存在时回退 %USERPROFILE%\.cargo；15) PATH 上的 uname 被遮蔽时
#      缓存根判定不变且正确；16) 只有 `python`（无 python3）时下载→校验→解压闭环；
#      17) ORT_LIB_LOCATION 指旧 hash 必红（曾假绿）；18) Windows 默认 store 根走 %LOCALAPPDATA%。
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
KIND="ort.pyke.io/dfbin"

sha_of() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
    else sha256sum "$1" | awk '{print $1}'; fi
}
is_win_ostype() { case "${OSTYPE:-}" in msys*|cygwin*|mingw*) return 0 ;; *) return 1 ;; esac; }

# ── 合成 dist.tsv（假 cargo home）────────────────────────────────────────────
# 被测脚本按 Cargo.lock 里的 ort-sys 版本在 $CARGO_HOME/registry/src/*/ort-sys-<ver>/build/download/dist.tsv
# 找 dist 声明。这里造一份只含本机 $TARGET 的，避免依赖真实 registry / 真实 ort 缓存。
VER="$(awk '/^\[\[package\]\]/{p=0} /^name = "ort-sys"$/{p=1} p && /^version = /{gsub(/[" ]/,"",$3); print $3; exit}' "$HERE/../Cargo.lock")"
[ -n "$VER" ] || { echo "Cargo.lock 里没有 ort-sys，无法自检" >&2; exit 2; }
FAKE_SHA="19fdf38815473f988be4bd26e9b558f2dfd77b8191daa785402ba211be828f2d"
FAKE_SHA2="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
FIXCARGO="$TMP/cargohome"
FIXDIR="$FIXCARGO/registry/src/index.fixture/ort-sys-$VER/build/download"
mkdir -p "$FIXDIR"
{
    printf '%s\t%s\t%s\t%s\n' "$TARGET" "default"  "https://example.invalid/${TARGET}-default.tar.lzma2"  "$FAKE_SHA"
    printf '%s\t%s\t%s\t%s\n' "$TARGET" "directml" "https://example.invalid/${TARGET}-directml.tar.lzma2" "$FAKE_SHA2"
    printf '%s\t%s\t%s\t%s\n' "x86_64-unknown-linux-gnu" "default" "https://example.invalid/linux.tar.lzma2" "$FAKE_SHA"
} > "$FIXDIR/dist.tsv"

passed=0
failed=0
pass() { echo "ok   - $1"; passed=$((passed + 1)); }
fail() { echo "FAIL - $1"; failed=$((failed + 1)); }

# 清掉可能污染结果的环境变量，并固定 ORT_CACHE_DIR/ORT_DIST_CACHE/CARGO_HOME（合成 dist.tsv）
run() {
    env -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET \
        CARGO_HOME="$FIXCARGO" \
        ORT_CACHE_DIR="$CACHE" ORT_DIST_CACHE="$STORE" \
        "$SHELL_BIN" "$SCRIPT" "$@"
}

# 直跑（可换缓存根/加参数），用于 --offline 等需要不同环境的用例
run_with() {
    local cache="$1" store="$2"; shift 2
    env -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET \
        CARGO_HOME="$FIXCARGO" \
        ORT_CACHE_DIR="$cache" ORT_DIST_CACHE="$store" \
        "$SHELL_BIN" "$SCRIPT" "$@"
}

# 注意：bash 3.2 下 `$VAR` 紧跟多字节字符（如全角括号）会被当成变量名的一部分而崩掉/输出乱码，
# 所以这些位置一律用 ${VAR}
echo "# 被测脚本：${SCRIPT}（shell: ${SHELL_BIN} · target: ${TARGET} · ort-sys: ${VER}）"

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

# ── 以下为 Windows 语义 4 个修复点的回归用例 ─────────────────────────────────────

# 14) 回归：Windows/MSYS 下 $HOME/.cargo 常不存在，必须回退 %USERPROFILE%\.cargo（曾直接判红）
FAKEUP="$TMP/fakeuser"
mkdir -p "$FAKEUP/.cargo/registry/src/index.fixture/ort-sys-$VER/build/download"
cp "$FIXDIR/dist.tsv" "$FAKEUP/.cargo/registry/src/index.fixture/ort-sys-$VER/build/download/dist.tsv"
NOHOME="$TMP/nohome"; mkdir -p "$NOHOME"
if OUT="$(env -u CARGO_HOME -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET \
            HOME="$NOHOME" USERPROFILE="$FAKEUP" \
            "$SHELL_BIN" "$SCRIPT" --list 2>/dev/null || true)" \
   && printf '%s' "$OUT" | grep -q "$TARGET"; then
    pass "cargo home：\$HOME/.cargo 缺失时回退 %USERPROFILE%\\.cargo"
else
    fail "cargo home 未回退 %USERPROFILE%\\.cargo（实际：${OUT:-空}）"
fi

# 15) 回归：PATH 上的 uname 被遮蔽（返回 Windows_NT）时，缓存根判定必须与 /usr/bin/uname 一致且正确
NOCACHE_HOME="$TMP/nocache-home"; NOCACHE_LOCAL="$TMP/nocache-local"; NOCACHE_XDG="$TMP/nocache-xdg"
mkdir -p "$NOCACHE_HOME" "$NOCACHE_LOCAL" "$NOCACHE_XDG"
SHIMDIR="$TMP/shim-uname"; mkdir -p "$SHIMDIR"
printf '#!/bin/sh\necho Windows_NT\n' > "$SHIMDIR/uname"; chmod +x "$SHIMDIR/uname"
check_root() {  # $1 = PATH 前缀（""=默认 PATH）
    local pfx="$1" out
    if [ -n "$pfx" ]; then
        out="$(env -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET -u ORT_CACHE_DIR -u ORT_DIST_CACHE \
            CARGO_HOME="$FIXCARGO" HOME="$NOCACHE_HOME" LOCALAPPDATA="$NOCACHE_LOCAL" XDG_CACHE_HOME="$NOCACHE_XDG" \
            PATH="$pfx:$PATH" "$SHELL_BIN" "$SCRIPT" --check 2>&1 >/dev/null || true)"
    else
        out="$(env -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET -u ORT_CACHE_DIR -u ORT_DIST_CACHE \
            CARGO_HOME="$FIXCARGO" HOME="$NOCACHE_HOME" LOCALAPPDATA="$NOCACHE_LOCAL" XDG_CACHE_HOME="$NOCACHE_XDG" \
            "$SHELL_BIN" "$SCRIPT" --check 2>&1 >/dev/null || true)"
    fi
    printf '%s\n' "$out" | sed -n 's/.*ort 缓存 \(.*\) 里没有 onnxruntime 库.*/\1/p'
    return 0
}
ROOT_A="$(check_root "")"
ROOT_B="$(check_root "/usr/bin")"
ROOT_C="$(check_root "$SHIMDIR")"
if [ -n "$ROOT_A" ] && [ "$ROOT_A" = "$ROOT_B" ] && [ "$ROOT_A" = "$ROOT_C" ]; then
    pass "缓存根不随 PATH 上被遮蔽的 uname 改变（默认=/usr/bin/shim 一致）"
else
    fail "缓存根随 uname 改变（默认=${ROOT_A:-空} / /usr/bin=${ROOT_B:-空} / shim=${ROOT_C:-空}）"
fi
if is_win_ostype; then
    if [ "$ROOT_A" = "$NOCACHE_LOCAL/$KIND/$TARGET/$HASH" ]; then
        pass "Windows 缓存根正确命中 %LOCALAPPDATA%\\$KIND"
    else
        fail "Windows 缓存根不是 %LOCALAPPDATA%（实际：${ROOT_A:-空}）"
    fi
fi

# 16) 回归：Windows 常规只有 `python`（无 python3）——下载→校验→解压→导出 仍须能闭环
PYBIN=""
if command -v python3 >/dev/null 2>&1; then PYBIN=python3
elif command -v python >/dev/null 2>&1; then PYBIN=python
elif command -v py >/dev/null 2>&1; then PYBIN="py -3"
fi
if [ -z "$PYBIN" ]; then
    echo "skip - 本机没有 Python，跳过 16)（解压闭环用例）"
else
    SHIMBIN="$TMP/shimbin"; mkdir -p "$SHIMBIN"
    # python3 存在但不可用（模拟 Windows 只有 python.exe）；curl 只做本地复制、不触网
    printf '#!/bin/sh\necho "no python3 here" >&2\nexit 127\n' > "$SHIMBIN/python3"
    cat > "$SHIMBIN/curl" <<'EOSH'
#!/bin/sh
out=""; prev=""
for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
[ -n "$out" ] || { echo "shim-curl: no -o given" >&2; exit 2; }
cp "${ORT_TEST_ARCHIVE:?}" "$out"
EOSH
    chmod +x "$SHIMBIN/python3" "$SHIMBIN/curl"
    ARCH="$TMP/dist.tar.lzma2"
    # shellcheck disable=SC2086  # 故意不加引号：$PYBIN 可能是 "py -3"
    $PYBIN - "$ARCH" <<'EOPY'
import io, lzma, tarfile, sys
buf = io.BytesIO()
with tarfile.open(fileobj=buf, mode="w") as tf:
    data = b"fake-ort-lib-for-test"
    ti = tarfile.TarInfo("libonnxruntime.a"); ti.size = len(data)
    tf.addfile(ti, io.BytesIO(data))
comp = lzma.compress(buf.getvalue(), format=lzma.FORMAT_RAW,
                     filters=[{"id": lzma.FILTER_LZMA2, "dict_size": 1 << 20}])
open(sys.argv[1], "wb").write(comp)
EOPY
    ASHA="$(sha_of "$ARCH")"
    FIX16="$TMP/cargo16"; FIX16DIR="$FIX16/registry/src/index.fixture/ort-sys-$VER/build/download"
    mkdir -p "$FIX16DIR"
    printf '%s\t%s\t%s\t%s\n' "$TARGET" "default" "https://example.invalid/py.tar.lzma2" "$ASHA" > "$FIX16DIR/dist.tsv"
    STORE16="$TMP/store16"; CACHE16="$TMP/cache16"
    if OUT="$(env -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET \
                PATH="$SHIMBIN:$PATH" ORT_TEST_ARCHIVE="$ARCH" \
                CARGO_HOME="$FIX16" ORT_CACHE_DIR="$CACHE16" ORT_DIST_CACHE="$STORE16" \
                "$SHELL_BIN" "$SCRIPT" --print-path 2>/dev/null || true)" \
       && [ "$OUT" = "$STORE16/$TARGET/$ASHA" ]; then
        pass "只有 python（无 python3）时下载→校验→解压→导出 闭环"
    else
        fail "python 回退失败（期望 ${STORE16}/${TARGET}/${ASHA}，实际：${OUT:-空}）"
    fi
fi

# 17) 回归：ORT_LIB_LOCATION 指旧 hash（同名库、同 target、哈希不同）→ --check 必红（曾假绿）；
#     指当前 target/hash → 绿（正对照）。
OLD_SHA="0000000000000000000000000000000000000000000000000000000000000000"
OLDDIR="$TMP/oldhash/$TARGET/$OLD_SHA"; mkdir -p "$OLDDIR"; : > "$OLDDIR/onnxruntime.lib"
if env -u ORT_DIST_HASH -u ORT_DIST_TARGET ORT_LIB_LOCATION="$OLDDIR" \
       ORT_CACHE_DIR="$TMP/empty17a" ORT_DIST_CACHE="$TMP/empty17a-store" CARGO_HOME="$FIXCARGO" \
       "$SHELL_BIN" "$SCRIPT" --check >/dev/null 2>&1; then
    fail "ORT_LIB_LOCATION 指旧 hash 时 --check 应非 0（曾假绿）"
else
    pass "ORT_LIB_LOCATION 指旧 hash 时 --check 判红（旧 API 陷阱）"
fi
CURDIR="$TMP/curhash/$TARGET/$FAKE_SHA"; mkdir -p "$CURDIR"; : > "$CURDIR/onnxruntime.lib"
if env -u ORT_DIST_HASH -u ORT_DIST_TARGET ORT_LIB_LOCATION="$CURDIR" \
       ORT_CACHE_DIR="$TMP/empty17b" ORT_DIST_CACHE="$TMP/empty17b-store" CARGO_HOME="$FIXCARGO" \
       "$SHELL_BIN" "$SCRIPT" --check >/dev/null 2>&1; then
    pass "ORT_LIB_LOCATION 指当前 target/hash 时 --check 绿（正对照）"
else
    fail "ORT_LIB_LOCATION 指当前 dist 哈希时 --check 应为 0"
fi

# 18) 回归：Windows 下默认 store 根走 %LOCALAPPDATA%（此前落 $HOME/.cache，且写死 macOS 数据卷）
if is_win_ostype; then
    SROOT="$(env -u ORT_LIB_LOCATION -u ORT_DIST_HASH -u ORT_DIST_TARGET -u ORT_CACHE_DIR -u ORT_DIST_CACHE \
        CARGO_HOME="$FIXCARGO" HOME="$NOCACHE_HOME" LOCALAPPDATA="$NOCACHE_LOCAL" XDG_CACHE_HOME="$NOCACHE_XDG" \
        "$SHELL_BIN" "$SCRIPT" --offline 2>&1 >/dev/null || true)"
    case "$SROOT" in
        *"$NOCACHE_LOCAL/verba-ort-dist/$TARGET/$HASH"*)
            pass "Windows 默认 store 根走 %LOCALAPPDATA%\\verba-ort-dist" ;;
        *) fail "Windows 默认 store 根异常：${SROOT:-空}" ;;
    esac
else
    echo "skip - 非 Windows 平台，跳过 18)（store 根平台感知）"
fi

echo "# 通过 $passed 项，失败 $failed 项"
[ "$failed" -eq 0 ]
