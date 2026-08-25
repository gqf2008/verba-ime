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
    echo "::warning::brew 不可用/失败，下载 7-Zip 官方静态 7zz（26.02）"
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
cp "$TMP/dist/lib/librime.dylib" "$VENDOR/"

# 2) Weasel 0.17.4 数据（与 Windows 同一来源，与平台无关）
SEVENZ="$(resolve_7zz)"
curl -fsSL -o "$TMP/weasel.exe" "https://github.com/rime/weasel/releases/download/0.17.4/weasel-0.17.4.0-installer.exe"
mkdir -p "$TMP/weasel"
"$SEVENZ" x "$TMP/weasel.exe" "-o$TMP/weasel" -y >/dev/null
if [ ! -d "$TMP/weasel/data" ]; then
    echo "::error::Weasel 安装包中未找到 data 目录" >&2
    exit 1
fi
mkdir -p "$DATA"
cp -R "$TMP/weasel/data/." "$DATA/"

# 3) wubi86 五笔
for f in wubi86.schema.yaml wubi86.dict.yaml; do
    curl -fsSL -o "$DATA/$f" "https://raw.githubusercontent.com/rime/rime-wubi/master/$f"
done

# 4) default.yaml 的 schema_list 追加 wubi86（否则部署不会编译该方案）
if ! grep -q "wubi86" "$DATA/default.yaml"; then
    sed -i '' 's|  - schema: terra_pinyin|  - schema: terra_pinyin\n  - schema: wubi86|' "$DATA/default.yaml"
fi

# 5) 结构校验（发布构建依赖）
[ -f "$VENDOR/librime.dylib" ] || { echo "::error::vendor/rime/librime.dylib 缺失" >&2; exit 1; }
[ -d "$DATA/opencc" ] || { echo "::error::vendor/rime/data/opencc 缺失" >&2; exit 1; }
[ -f "$DATA/default.yaml" ] || { echo "::error::vendor/rime/data/default.yaml 缺失" >&2; exit 1; }

echo "vendor 就绪: $VENDOR"
find "$VENDOR" -type f | sed "s|$VENDOR/|  |"
