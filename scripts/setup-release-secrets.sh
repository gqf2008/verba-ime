#!/usr/bin/env bash
# 配置 GitHub Actions 发布 secrets（Apple 签名/公证，5 项）。
# 用法: bash scripts/setup-release-secrets.sh [owner/repo]   （默认 gqf2008/verba-ime）
#
# 说明:
# - GitHub 个人账号无账号级 secrets，每个仓库需单独配置一遍（值复用同一套凭证）
# - P12 必须用 -t identities 导出（含私钥；-t certs 只有证书，CI 会报
#   "SecItemCopyMatching: The specified item could not be found"）
# - 密码/账号交互输入，不落盘、不进 shell 历史；临时 P12 用 trap 清理
# 前置: 本机钥匙串已有 Developer ID Application 证书（security find-identity -v -p codesigning）
set -euo pipefail

REPO="${1:-gqf2008/verba-ime}"
KEYCHAIN="${KEYCHAIN:-$HOME/Library/Keychains/login.keychain-db}"

# 注意：find-identity 的钥匙串是位置参数（-k 是 export 的选项，放这里会报 illegal option）
CERT="$(security find-identity -v -p codesigning "$KEYCHAIN" | awk -F'"' '/Developer ID Application/ && $0 !~ /\(invalid\)/ {print $2; exit}')"
if [ -z "$CERT" ]; then
    echo "::error::本机钥匙串未找到 Developer ID Application 证书（先到 Apple 开发者后台创建并安装）" >&2
    exit 1
fi
TEAM_ID="$(printf '%s' "$CERT" | sed -n 's/.*(\([A-Z0-9]*\))/\1/p')"
echo "证书: $CERT"
echo "TEAM_ID: $TEAM_ID"

# P12 导出密码：交互输入（可用 VERBA_P12_PASSWORD 环境变量覆盖，便于自动化）
P12_PW="${VERBA_P12_PASSWORD:-}"
if [ -z "$P12_PW" ]; then
    read -r -s -p "P12 导出密码（仅本次使用，不落盘）: " P12_PW
    echo
fi
[ -n "$P12_PW" ] || { echo "::error::P12 密码为空" >&2; exit 1; }

# Apple 账号：交互输入（可用环境变量覆盖）
APPLE_ID="${VERBA_APPLE_ID:-}"
if [ -z "$APPLE_ID" ]; then
    read -r -p "Apple 开发者账号（APPLE_ID，如 you@example.com）: " APPLE_ID
fi
APP_PW="${VERBA_APPLE_APP_PASSWORD:-}"
if [ -z "$APP_PW" ]; then
    read -r -s -p "App 专用密码（APPLE_APP_PASSWORD，appleid.apple.com 生成）: " APP_PW
    echo
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# -t identities = 证书 + 私钥（关键；-t certs 只有证书链）
security export -k "$KEYCHAIN" -t identities -f pkcs12 -P "$P12_PW" -o "$TMP/verba-cert.p12" "$CERT"
P12_B64="$(base64 < "$TMP/verba-cert.p12")"

echo "==> 设置 secrets 到 $REPO"
gh secret set APPLE_CERT_P12       -R "$REPO" --body "$P12_B64"
gh secret set APPLE_CERT_PASSWORD  -R "$REPO" --body "$P12_PW"
gh secret set APPLE_TEAM_ID        -R "$REPO" --body "$TEAM_ID"
gh secret set APPLE_ID             -R "$REPO" --body "$APPLE_ID"
gh secret set APPLE_APP_PASSWORD   -R "$REPO" --body "$APP_PW"

echo "==> 完成。核对:"
gh secret list -R "$REPO"
echo "==> 下一步: workflow_dispatch 干跑验证（gh workflow run \"Build & Release\" -R $REPO）"
