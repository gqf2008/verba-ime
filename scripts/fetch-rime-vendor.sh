#!/usr/bin/env bash
# 获取发布用的 Rime 运行时（第三方二进制/数据，不入库，gitignored 于 vendor/）：
#   1) librime 1.17.0 stable macOS-universal → librime.dylib
#   2) Weasel 0.17.4 安装包 → Rime 数据（与平台无关，与 fetch-rime-vendor.ps1 同一来源）
#   3) rime-wubi → wubi86 五笔方案
# 产物: vendor/rime/{librime.dylib, data/}
# 用法: bash scripts/fetch-rime-vendor.sh
# 说明: librime 资产名嵌 commit hash，按 tag + 名称动态解析，不写死 URL。
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="$REPO_ROOT/vendor/rime"
DATA="$VENDOR/data"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 7zz：macOS 解 Weasel（NSIS 安装包）用；优先系统已有，其次 brew，最后官方静态二进制
resolve_7zz() {
    if command -v 7zz >/dev/null 2>&1; then
        echo "7zz"
        return
    fi
    if command -v brew >/dev/null 2>&1; then
        brew install sevenzip >/dev/null 2>&1 || true
        if command -v 7zz >/dev/null 2>&1; then
            echo "7zz"
            return
        fi
    fi
    echo "::warning::brew 不可用/失败，下载 7-Zip 官方静态 7zz（26.02）" >&2
    curl -fsSL -o "$TMP/7z.tar.xz" "https://github.com/ip7z/7zip/releases/download/26.02/7z2602-mac.tar.xz"
    tar -xJf "$TMP/7z.tar.xz" -C "$TMP"
    echo "$TMP/7zz"
}

mkdir -p "$VENDOR"

# 1) librime 1.17.0 stable（macOS-universal 覆盖 arm64 + x86_64）
# 用 gh api（认证，5000 次/时）而非裸 curl api.github.com（未认证 60 次/时/IP，
# CI 共享 IP 曾 403 限流）；需 gh CLI（GitHub runner 预装，本地 `gh auth login`）
REL="$(gh api repos/rime/librime/releases/tags/1.17.0)"
# 排除 rime-deps-*（后缀相同但只含 opencc 工具与 include，无 librime.dylib）
ASSET_URL="$(printf '%s' "$REL" | python3 -c 'import json,sys; r=json.load(sys.stdin); print(next(a["browser_download_url"] for a in r["assets"] if a["name"].endswith("macOS-universal.tar.bz2") and not a["name"].startswith("rime-deps-")))')"
echo "下载 librime: $ASSET_URL"
curl -fsSL -o "$TMP/rime-macos.tar.bz2" "$ASSET_URL"
tar -xjf "$TMP/rime-macos.tar.bz2" -C "$TMP"
if [ ! -f "$TMP/dist/lib/librime.dylib" ]; then
    echo "::error::librime 资产中未找到 dist/lib/librime.dylib" >&2
    exit 1
fi
# cp -a 保留符号链接链（librime.dylib -> librime.1.dylib -> librime.1.17.0.dylib）：
# @rpath/librime.1.dylib 是实体的 install_name（LC_ID_DYLIB），后续若有人链接
# librime 会按此解析；裸 cp 解引用只留实体，曾导致链接链丢失。
cp -a "$TMP/dist/lib/librime.dylib" "$TMP/dist/lib/librime.1.dylib" "$TMP/dist/lib/librime.1.17.0.dylib" "$VENDOR/"
# 回归守卫：链接链必须保留（旧代码裸 cp 解引用，此断言为红）
[ -L "$VENDOR/librime.dylib" ] || { echo "::error::librime.dylib 应为符号链接（链接链被解引用）" >&2; exit 1; }
[ -L "$VENDOR/librime.1.dylib" ] || { echo "::error::librime.1.dylib 应为符号链接（链接链被解引用）" >&2; exit 1; }

# 2) Weasel 0.17.4 数据（与 Windows 同一来源，与平台无关）
SEVENZ="$(resolve_7zz)"
curl -fsSL -o "$TMP/weasel.exe" "https://github.com/rime/weasel/releases/download/0.17.4/weasel-0.17.4.0-installer.exe"
mkdir -p "$TMP/weasel"
"$SEVENZ" x "$TMP/weasel.exe" "-o$TMP/weasel" -y >/dev/null
if [ ! -d "$TMP/weasel/data" ]; then
    echo "::error::Weasel 安装包中未找到 data 目录" >&2
    exit 1
fi
# 先清空再拷贝：增量合并会让上一版残留文件（Weasel 升级删除/改名的 schema/dict/
# opencc）静默留在 vendor 并随安装包发布、被 Rime 加载（复审 sweep）。data/ 为本
# 脚本独占产物，整体重建最安全。
rm -rf "$DATA"
mkdir -p "$DATA"
cp -R "$TMP/weasel/data/." "$DATA/"

# 3) wubi86 五笔
for f in wubi86.schema.yaml wubi86.dict.yaml; do
    curl -fsSL -o "$DATA/$f" "https://raw.githubusercontent.com/rime/rime-wubi/master/$f"
done

# 4) default.yaml 的 schema_list 追加 wubi86（否则部署不会编译该方案）
if ! grep -q "wubi86" "$DATA/default.yaml"; then
    # 可移植注入：不用 sed——macOS 的 BSD sed 需要 `-i ''`（GNU 视 '' 为文件名）
    # 且其替换串不解释 \n；awk + 临时文件在两端语义一致。
    awk \
        '/  - schema: terra_pinyin/ && !done { print; print "  - schema: wubi86"; done = 1; next } { print }' \
        "$DATA/default.yaml" > "$DATA/default.yaml.tmp" &&
        mv "$DATA/default.yaml.tmp" "$DATA/default.yaml"
fi
# 回归守卫：sed 以 terra_pinyin 为锚，若上游 default.yaml 改版/换锚则静默不插入，
# 五笔方案不会被部署编译——必须断言插入成功（复审 V23）。
grep -q "wubi86" "$DATA/default.yaml" || { echo "::error::wubi86 未能插入 default.yaml 的 schema_list（锚点 terra_pinyin 可能已变）" >&2; exit 1; }

# 5) Verba 自定义短语（scripts/rime-extra/，biáng 等）：
#    luna_pinyin_simp 经 __include 继承 luna_pinyin.schema，而后者已内置
#    table_translator@custom_phrase 接线（rime-luna-pinyin 上游，2026-08 核实），
#    因此只需注入词条文件；接线存在性由回归守卫断言，上游改版时响亮失败。
EXTRA="$REPO_ROOT/scripts/rime-extra"
if [ -f "$DATA/custom_phrase.txt" ]; then
    # 上游已带 custom_phrase.txt：先补尾换行（防词条粘连到末行），再补缺失
    # 词条行（幂等），不覆盖上游内容
    [ -n "$(tail -c 1 "$DATA/custom_phrase.txt")" ] && printf '\n' >> "$DATA/custom_phrase.txt"
    while IFS= read -r line; do
        case "$line" in ''|'#'*) continue ;; esac        # 跳过注释/空行
        case "$line" in *$'\t'*) ;; *) continue ;; esac  # 只取含制表符的词条行
        grep -Fqx "$line" "$DATA/custom_phrase.txt" || printf '%s\n' "$line" >> "$DATA/custom_phrase.txt"
    done < "$EXTRA/custom_phrase.txt"
else
    cp "$EXTRA/custom_phrase.txt" "$DATA/"
fi
# 回归守卫：接线（simp 或基 schema 含 custom_phrase）与词条必须落地，
# 上游改版导致落空时此处为红（防静默失效）
{ grep -q "custom_phrase" "$DATA/luna_pinyin_simp.schema.yaml" \
    || grep -q "custom_phrase" "$DATA/luna_pinyin.schema.yaml"; } \
    || { echo "::error::custom_phrase 接线缺失（上游 schema 已变）" >&2; exit 1; }
grep -Fq $'\tbiang' "$DATA/custom_phrase.txt" \
    || { echo "::error::biang 词条未注入 custom_phrase.txt" >&2; exit 1; }

# 6) 结构校验（发布构建依赖）
[ -f "$VENDOR/librime.dylib" ] || { echo "::error::vendor/rime/librime.dylib 缺失" >&2; exit 1; }
[ -d "$DATA/opencc" ] || { echo "::error::vendor/rime/data/opencc 缺失" >&2; exit 1; }
[ -f "$DATA/default.yaml" ] || { echo "::error::vendor/rime/data/default.yaml 缺失" >&2; exit 1; }

echo "vendor 就绪: $VENDOR"
find "$VENDOR" -type f | sed "s|$VENDOR/|  |"
