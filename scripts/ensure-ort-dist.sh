#!/usr/bin/env bash
# 让 ort（ONNX Runtime）的预编译 dist 在本机**确定可用**：
#   1) ort-sys 的下载缓存是「按 feature set 哈希的目录」，目录存在但为空时会被当成有效缓存
#      → 链接期报 `could not find native static library onnxruntime`（cargo check 全绿，骗人）。
#   2) 于是有人把 ORT_LIB_LOCATION 指向本机别的旧哈希目录：链接能过，但二进制一跑就 panic
#      `The requested API version [...] is not available`（旧库 ORT 版本低于 ort 要求的 API）。
# 本脚本按 `ort-sys-*/build/download/dist.tsv` 选定该 target 应下载的 dist（sha256 校验），
# 命中 ort 缓存就直接可用，否则下载到本仓库约定的数据卷缓存并导出 ORT_LIB_LOCATION。
#
# 用法：
#   bash scripts/ensure-ort-dist.sh            # 打印 export 语句（不改变当前 shell）
#   eval "$(bash scripts/ensure-ort-dist.sh)"  # 当前 shell 生效，之后直接 cargo build/test
#   bash scripts/ensure-ort-dist.sh -- cargo test --workspace   # 带着 ORT_LIB_LOCATION 跑命令
#   bash scripts/ensure-ort-dist.sh --check    # 只检查：0=无需自愈，1=需要自愈（附原因）
#   bash scripts/ensure-ort-dist.sh --fix-cache # 把该 dist 的空/损坏缓存目录可逆移开（.broken-<ts>）
#   bash scripts/ensure-ort-dist.sh --list     # 列出该 target 的所有 dist 行（含 sha256）
# 环境变量：
#   ORT_DIST_CACHE   本地 dist 缓存根（默认 macOS：/Volumes/DataExt/tmp/verba-ort-dist 或 ~/.cache/verba-ort-dist；
#                    Windows：%LOCALAPPDATA%\verba-ort-dist；其它：$XDG_CACHE_HOME 或 ~/.cache/verba-ort-dist）
#   ORT_DIST_TARGET  覆盖 target triple（默认取 `rustc -vV` 的 host）
#   ORT_DIST_HASH    覆盖 dist 选择（dist.tsv 第四列；启用 cuda/coreml/directml 等 EP feature 时用 --list 选）
#   ORT_CACHE_DIR    ort-sys 自己的缓存根（与 ort-sys 同优先级；设置后 --check/--fix-cache 认它）
#   ORT_LIB_LOCATION 已显式指定时只认「指向本 target 当前 dist 哈希」的目录；旧 hash/版本不一致 → 判红并忽略
#                    （只按文件名判存在会把旧库当健康——链接能过、一跑就 panic，见本文件头第 5 行）
# 说明：本脚本只读 Cargo.lock / ort-sys 源码；除下载缓存外不写仓库，不删除任何数据（--fix-cache 是移动）。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ORT_CACHE_KIND="ort.pyke.io/dfbin"

log() { echo "ensure-ort-dist: $*" >&2; }
die() { log "错误：$*"; exit 1; }

# 用法块 = 文件头注释块（shebang 之后连续的行首 `#` 行）。不写死行号：改注释不会让 --help 漂移/截断。
usage() {
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

# `--` 之后的命令。**全局**而非 main 的 local：bash 3.2（macOS /bin/bash）在 `set -u` 下
# 展开空数组的 `"${REMAINING[@]}"` 会报 `REMAINING[@]: unbound variable` 并中断，
# 文档里的 `eval "$(bash scripts/ensure-ort-dist.sh)"` 会因此拿不到输出，故必须按长度分支。
REMAINING=()

# 可用的 Python 解释器（可能形如 "py -3"，即命令 + 参数）；延迟探测并缓存
PYTHON_CMD=""

# ── 平台判定：**不能**只信 PATH 上的 `uname` ─────────────────────────────────────
# Windows 上 `uname` 常被别的工具链（如 w64devkit）遮蔽、返回 `Windows_NT`，于是
# `case "$(uname -s)"` 落空、静默走 Linux 分支 → 读错缓存根、在已就绪的机器上假红。
# 故：先看 uname，识别到就直接采信；识别不到再用环境标记兜底（MSYS/Cygwin 的 $OSTYPE、
# Windows 的 $OS / %LOCALAPPDATA%），保证「默认 PATH（uname 被遮蔽）」与「/usr/bin/uname」判定一致。
detect_os() {
    case "$(uname -s 2>/dev/null || true)" in
        Darwin)               echo darwin;  return 0 ;;
        MINGW*|MSYS*|CYGWIN*) echo windows; return 0 ;;
        Linux)                echo linux;   return 0 ;;
    esac
    case "${OSTYPE:-}" in
        darwin*)              echo darwin;  return 0 ;;
        msys*|mingw*|cygwin*) echo windows; return 0 ;;
        linux*)               echo linux;   return 0 ;;
    esac
    if [ "${OS:-}" = "Windows_NT" ] || [ -n "${LOCALAPPDATA:-}" ]; then
        echo windows; return 0
    fi
    echo linux
}
is_windows() { [ "$(detect_os)" = windows ]; }

# cargo home 候选（按优先级）：CARGO_HOME > 平台家目录下的 .cargo。
# Windows/MSYS 下 $HOME 常是 /home/<user>（≠ cargo 真实使用的 %USERPROFILE%），只认 $HOME/.cargo
# 会找不到已拉好的 dist.tsv → 已就绪的机器上假红。
cargo_home_candidates() {
    if [ -n "${CARGO_HOME:-}" ]; then printf '%s\n' "${CARGO_HOME%/}"; fi
    if is_windows; then
        if [ -n "${USERPROFILE:-}" ]; then printf '%s\n' "${USERPROFILE//\\//}/.cargo"; fi
        if [ -n "${HOME:-}" ]; then printf '%s\n' "${HOME%/}/.cargo"; fi
    else
        if [ -n "${HOME:-}" ]; then printf '%s\n' "${HOME%/}/.cargo"; fi
        if [ -n "${USERPROFILE:-}" ]; then printf '%s\n' "${USERPROFILE//\\//}/.cargo"; fi
    fi
}

# ── dist.tsv：ort-sys 声明该 target 有哪些预编译 dist（feature_set / url / sha256）────────
find_dist_tsv() {
    local ver="$1" home f found=""
    while IFS= read -r home; do
        [ -n "$home" ] || continue
        for f in "$home"/registry/src/*/ort-sys-"$ver"/build/download/dist.tsv; do
            if [ -f "$f" ]; then found="$f"; break; fi
        done
        if [ -n "$found" ]; then break; fi
    done < <(cargo_home_candidates)
    if [ -z "$found" ]; then return 1; fi
    printf '%s\n' "$found"
}

ort_sys_version() {
    awk '/^\[\[package\]\]/{p=0} /^name = "ort-sys"$/{p=1} p && /^version = /{gsub(/[" ]/,"",$3); print $3; exit}' \
        "$REPO_ROOT/Cargo.lock"
}

host_target() { rustc -vV | awk '/^host: /{print $2}'; }

sha256_of() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
    else sha256sum "$1" | awk '{print $1}'; fi
}

onnx_cache_root() {
    # 与 ort-sys 的 internal::dirs::cache_dir 优先级一致：ORT_CACHE_DIR 优先，其次按平台
    # （macOS: ~/Library/Caches · Windows: %LOCALAPPDATA% · Linux: $XDG_CACHE_HOME 或 ~/.cache）
    if [ -n "${ORT_CACHE_DIR:-}" ]; then
        echo "${ORT_CACHE_DIR%/}/dfbin"
        return 0
    fi
    case "$(detect_os)" in
        darwin)  echo "$HOME/Library/Caches/$ORT_CACHE_KIND" ;;
        windows) echo "${LOCALAPPDATA:-${USERPROFILE:-$HOME}/AppData/Local}/$ORT_CACHE_KIND" ;;
        *)       echo "${XDG_CACHE_HOME:-$HOME/.cache}/$ORT_CACHE_KIND" ;;
    esac
}

dist_store_root() {
    if [ -n "${ORT_DIST_CACHE:-}" ]; then
        echo "$ORT_DIST_CACHE"
        return 0
    fi
    case "$(detect_os)" in
        darwin)
            if [ -d /Volumes/DataExt/tmp ]; then echo /Volumes/DataExt/tmp/verba-ort-dist
            else echo "$HOME/.cache/verba-ort-dist"; fi ;;
        windows)
            echo "${LOCALAPPDATA:-${USERPROFILE:-$HOME}/AppData/Local}/verba-ort-dist" ;;
        *)
            echo "${XDG_CACHE_HOME:-$HOME/.cache}/verba-ort-dist" ;;
    esac
}

# 在目录里找 onnxruntime 库（静态/动态、各平台命名），打印其所在目录
lib_dir_in() {
    local d="$1" hit
    [ -d "$d" ] || return 1
    hit="$(find "$d" -maxdepth 3 \( -name 'libonnxruntime.a' -o -name 'libonnxruntime.dylib' \
        -o -name 'libonnxruntime.so' -o -name 'onnxruntime.lib' -o -name 'onnxruntime.dll' \) \
        -print -quit 2>/dev/null || true)"
    [ -n "$hit" ] || return 1
    dirname "$hit"
}

# ORT_LIB_LOCATION 是否确实指向「本 target 当前选中的 dist 哈希」对应的目录。
# 只按文件名判存在（lib_dir_in）会把别的 hash 目录（旧 ORT / 旧 API）当成健康缓存——
# 链接能过、二进制一跑就 panic（本文件头第 5 行的坑），故必须核对 target/hash。
lib_location_is_current() {
    local d="$1" t="$2" h="$3" p
    if [ -z "$d" ] || [ -z "$h" ]; then return 1; fi
    p="${d//\\//}"; p="${p%/}"
    case "$p" in
        *"/$t/$h"|*"/$t/$h"/*) return 0 ;;
        *) return 1 ;;
    esac
}

# 找可用的 Python（Windows 常规只有 `python`，或在 `py` 启动器下）：只认能 import lzma+tarfile 的。
find_python() {
    if [ -n "$PYTHON_CMD" ]; then printf '%s\n' "$PYTHON_CMD"; return 0; fi
    local c
    for c in python3 python; do
        if command -v "$c" >/dev/null 2>&1 && "$c" -c 'import lzma, tarfile' >/dev/null 2>&1; then
            PYTHON_CMD="$c"; printf '%s\n' "$c"; return 0
        fi
    done
    if command -v py >/dev/null 2>&1 && py -3 -c 'import lzma, tarfile' >/dev/null 2>&1; then
        PYTHON_CMD="py -3"; printf '%s\n' "py -3"; return 0
    fi
    return 1
}

# 解 ort 的 dist 包：*.tar.lzma2 是无容器头的 **raw LZMA2**（xz/alone/tar 都解不开）
extract_dist() {
    local archive="$1" dest="$2" py
    py="$(find_python)" || die "没有可用的 Python（试过 python3 / python / py -3）：无法解压 raw LZMA2 dist；请装带 lzma 模块的 Python，或自行解压到 ${dest}"
    mkdir -p "$dest"
    # shellcheck disable=SC2086  # 故意不加引号：$py 可能是 "py -3"（命令 + 参数）两个词
    $py - "$archive" "$dest" <<'PY'
import io, lzma, sys, tarfile
archive, dest = sys.argv[1], sys.argv[2]
raw = open(archive, "rb").read()
out = None
for ds in (1 << 26, 1 << 24, 1 << 30, 1 << 20):
    try:
        out = lzma.LZMADecompressor(format=lzma.FORMAT_RAW,
                                    filters=[{"id": lzma.FILTER_LZMA2, "dict_size": ds}]).decompress(raw)
        break
    except lzma.LZMAError:
        continue
if out is None:
    out = lzma.decompress(raw)  # 兜底：某些 dist 是 xz/lzma 容器
with tarfile.open(fileobj=io.BytesIO(out)) as tf:
    tf.extractall(dest)
PY
}

report_and_run() {
    local libdir="$1"; shift
    if [ "$#" -eq 0 ]; then
        printf 'export ORT_LIB_LOCATION=%q\n' "$libdir"
    else
        log "ORT_LIB_LOCATION=$libdir"
        ORT_LIB_LOCATION="$libdir" "$@"
    fi
}

# 统一收尾：给了命令就带着 ORT_LIB_LOCATION 执行，否则只打印 export 供 eval
finish_() {
    local libdir="$1"
    if [ "${#REMAINING[@]}" -gt 0 ]; then
        report_and_run "$libdir" "${REMAINING[@]}"
    else
        report_and_run "$libdir"
    fi
}

main() {
    local mode="emit"
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --check)     mode="check" ;;
            --print-path) mode="path" ;;
            --list)      mode="list" ;;
            --fix-cache) mode="fix" ;;
            --offline)   offline=1 ;;
            --)          shift; REMAINING=("$@"); break ;;
            -h|--help)   usage; return 0 ;;
            *)           die "未知参数：$1（--help 看用法）" ;;
        esac
        shift
    done
    local offline="${offline:-0}"

    local ver tsv target
    ver="$(ort_sys_version)" || true
    [ -n "${ver:-}" ] || die "Cargo.lock 里没有 ort-sys（本仓库 OCR 路径经 rapidocr-core 依赖它）"
    tsv="$(find_dist_tsv "$ver")" || die "找不到 ort-sys $ver 的 dist.tsv（先跑一次 cargo fetch 或 cargo check 把依赖拉下来）"
    target="${ORT_DIST_TARGET:-$(host_target)}"

    local rows row feature url hash
    rows="$(awk -F'\t' -v t="$target" '$1==t {print $2"\t"$3"\t"$4}' "$tsv")"
    [ -n "$rows" ] || die "dist.tsv 里没有 target $target 的预编译 dist（ort-sys $ver 未覆盖该平台）"

    if [ "$mode" = "list" ]; then
        echo "# ort-sys $ver · target ${target}（每行的 feature_set 对应一个 dist）"
        printf '%s\n' "$rows" | awk -F'\t' '{printf "  %-28s %s\n    %s\n", $1, $3, $2}'
        return 0
    fi

    local auto_pick=0
    if [ -n "${ORT_DIST_HASH:-}" ]; then
        row="$(printf '%s\n' "$rows" | awk -F'\t' -v h="$ORT_DIST_HASH" '$3==h {print; exit}')"
        [ -n "$row" ] || die "ORT_DIST_HASH=$ORT_DIST_HASH 不在该 target 的 dist 列表里（--list 查看）"
    else
        # 与 ort-sys resolve_dist 的取向一致：无 EP feature 时选该 target 的第一行
        row="$(printf '%s\n' "$rows" | head -1)"
        auto_pick=1
    fi
    feature="$(printf '%s' "$row" | cut -f1)"
    url="$(printf '%s' "$row" | cut -f2)"
    hash="$(printf '%s' "$row" | cut -f3)"

    if [ "$auto_pick" = "1" ]; then
        local variants
        variants="$(printf '%s\n' "$rows" | awk -F'\t' '{print $1}' | sort -u | wc -l | tr -d ' ')"
        if [ "$variants" -gt 1 ]; then
            log "注意：target $target 有 $variants 种 feature set 组合，默认按无 EP feature 取第一行（feature_set=${feature}）；"
            log "      若本次构建启用了 cuda/coreml/directml 等 EP feature：--list 看清单 → ORT_DIST_HASH=<sha256> 指定"
        fi
    fi

    local cache_dir store_dir
    cache_dir="$(onnx_cache_root)/$target/$hash"
    store_dir="$(dist_store_root)/$target/$hash"

    # 0) 已经显式指过：先确认里面有库，再确认它**确实指向本 target 当前选中的 dist 哈希**。
    #    只按文件名判存在（lib_dir_in）会把任意旧 hash 目录当健康缓存 → 链接能过、一跑就 panic
    #    （本文件头第 5 行的坑）。所以哈希对不上时：--check 判红，其它模式忽略这个不可信路径。
    if [ -n "${ORT_LIB_LOCATION:-}" ]; then
        if ! lib_dir_in "${ORT_LIB_LOCATION}" >/dev/null 2>&1; then
            log "注意：环境里的 ORT_LIB_LOCATION=$ORT_LIB_LOCATION 里没有 onnxruntime 库，忽略它"
        elif lib_location_is_current "${ORT_LIB_LOCATION}" "$target" "$hash"; then
            case "$mode" in
                check) return 0 ;;
                path)  echo "$ORT_LIB_LOCATION"; return 0 ;;
                *)     finish_ "$ORT_LIB_LOCATION"; return 0 ;;
            esac
        else
            log "错误：ORT_LIB_LOCATION=$ORT_LIB_LOCATION 与本次 dist 选择（${target}/${hash}）不一致，"
            log "      很可能是旧 hash / 旧 ORT API 版本：链接能过、二进制一跑就 panic（见文件头第 5 行）。"
            log "      处理：unset ORT_LIB_LOCATION 后重跑本脚本；或 --list 选对 feature set（ORT_DIST_HASH=<sha256>）。"
            [ "$mode" != "check" ] || return 1
            # 非 check 模式：忽略这个不可信的显式路径，继续按 ort 缓存 / 本地 dist 解析出正确的库
        fi
    fi

    # 1) ort 自己的缓存（理想情况：什么都不用做）
    local libdir=""
    libdir="$(lib_dir_in "$cache_dir" || true)"
    if [ -n "$libdir" ]; then
        case "$mode" in
            check) return 0 ;;
            fix)   log "ort 缓存已就绪（${libdir}），无需修复"; return 0 ;;
            path)  echo "$libdir"; return 0 ;;
            *)
                log "ort 缓存已就绪：$libdir"
                # 给了命令就必须执行：曾漏这一步 → 缓存就绪的机器上 `-- cargo test` 静默不跑任何东西却 exit 0
                # （自己绿、别人红，最难查的一类）。emit 形态仍不打 export：缓存就绪时确实不需要设环境变量。
                [ "${#REMAINING[@]}" -eq 0 ] || report_and_run "$libdir" "${REMAINING[@]}"
                return 0
                ;;
        esac
    fi

    # 2) --check：未就绪就红，并说清下一步
    if [ "$mode" = "check" ]; then
        log "ort 缓存 $cache_dir 里没有 onnxruntime 库（该目录存在但为空是已知坑）"
        log "自愈：bash scripts/ensure-ort-dist.sh --fix-cache   （让 ort-sys 重新下载）"
        log "或：  eval \"\$(bash scripts/ensure-ort-dist.sh)\"    （用脚本缓存的 dist 覆盖）"
        return 1
    fi

    # 3) --fix-cache：只把「本 target 该用的那个哈希目录」可逆移开
    if [ "$mode" = "fix" ]; then
        if [ ! -d "$cache_dir" ]; then
            log "缓存目录不存在（${cache_dir}），ort-sys 下次构建会自行下载"
            return 0
        fi
        local broken
        broken="${cache_dir}.broken-$(date +%Y%m%d-%H%M%S)"
        mv "$cache_dir" "$broken"
        log "已把坏缓存移开（可逆）：$cache_dir → $broken"
        log "现在直接跑 cargo 构建即可（ort-sys 会重新下载 ${url}）"
        return 0
    fi

    # 4) 本地 dist 缓存命中？
    libdir="$(lib_dir_in "$store_dir" || true)"
    if [ -n "$libdir" ]; then
        case "$mode" in
            path) echo "$libdir" ;;
            *)    finish_ "$libdir" ;;
        esac
        return 0
    fi

    [ "$offline" = "0" ] || die "离线模式：本地缓存 $store_dir 里没有已解压的 dist"

    # 5) 下载 → 校验 sha256 → 解压 → 输出
    command -v curl >/dev/null 2>&1 || die "需要 curl 下载 dist"
    mkdir -p "$(dirname "$store_dir")"
    local tmp
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/ensure-ort-dist.XXXXXX")"
    trap 'rm -rf "${tmp:-}"' EXIT
    log "下载 dist（feature_set=${feature}）：$url"
    curl -fL --retry 3 --connect-timeout 20 -o "$tmp/dist" "$url" || die "下载失败：$url"
    local got
    got="$(sha256_of "$tmp/dist")"
    [ "$got" = "$hash" ] || die "sha256 不匹配（期望 ${hash}，实际 ${got}）—— 不要用它"
    log "sha256 校验通过（${hash}）"
    extract_dist "$tmp/dist" "$store_dir"
    libdir="$(lib_dir_in "$store_dir" || true)"
    [ -n "$libdir" ] || die "解压后没找到 onnxruntime 库（${store_dir}）"
    log "dist 已就绪：$libdir"
    case "$mode" in
        path) echo "$libdir" ;;
        *)    finish_ "$libdir" ;;
    esac
}

main "$@"
