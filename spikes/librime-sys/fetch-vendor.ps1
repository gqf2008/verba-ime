# 获取 librime-sys spike 的 vendor 依赖（第三方二进制，不入库）：
#   1) librime nightly Windows-msvc-x64 → rime.dll（x64）+ rime_api.h
#   2) Weasel 0.17.4 安装包 → Rime 数据（luna_pinyin/bopomofo/stroke…）
#   3) rime-wubi → wubi86 五笔方案
# 用法: pwsh fetch-vendor.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$vendor = Join-Path $root "vendor"
$dataDir = Join-Path $vendor "data"
$tmp = Join-Path $env:TEMP "verba-rime-fetch"
New-Item -ItemType Directory -Path $vendor, $dataDir -Force | Out-Null
if (Test-Path $tmp) { Remove-Item -LiteralPath $tmp -Recurse -Force }
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
$7z = "C:\Program Files\7-Zip\7z.exe"

# 1) librime nightly（查询最新 asset 名，匹配 Windows-msvc-x64）
$rel = Invoke-RestMethod -Uri "https://api.github.com/repos/rime/librime/releases/latest"
$asset = $rel.assets | Where-Object { $_.name -match "Windows-msvc-x64\.7z" } | Select-Object -First 1
if (-not $asset) { throw "未找到 librime Windows-msvc-x64 资产" }
Write-Output "下载 librime: $($asset.name)"
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile (Join-Path $tmp "rime-x64.7z") -UseBasicParsing
& $7z x (Join-Path $tmp "rime-x64.7z") ("-o" + (Join-Path $tmp "rime")) -y | Out-Null
Copy-Item (Join-Path $tmp "rime\dist\lib\rime.dll") $vendor -Force
New-Item -ItemType Directory -Path (Join-Path $vendor "include") -Force | Out-Null
Copy-Item (Join-Path $tmp "rime\dist\include\rime_api.h") (Join-Path $vendor "include") -Force

# 2) Weasel 0.17.4 数据
$weaselUrl = "https://github.com/rime/weasel/releases/download/0.17.4/weasel-0.17.4.0-installer.exe"
Invoke-WebRequest -Uri $weaselUrl -OutFile (Join-Path $tmp "weasel.exe") -UseBasicParsing
& $7z x (Join-Path $tmp "weasel.exe") ("-o" + (Join-Path $tmp "weasel")) -y | Out-Null
Get-ChildItem (Join-Path $tmp "weasel\data") -File | Copy-Item -Destination $dataDir -Force

# 3) wubi86 五笔
foreach ($f in @("wubi86.schema.yaml", "wubi86.dict.yaml")) {
    Invoke-WebRequest -Uri "https://raw.githubusercontent.com/rime/rime-wubi/master/$f" -OutFile (Join-Path $dataDir $f) -UseBasicParsing
}

# 4) default.yaml 的 schema_list 补 wubi86（否则部署不会编译该方案）
$default = Join-Path $dataDir "default.yaml"
$c = Get-Content $default -Raw
if ($c -notmatch "wubi86") {
    $c = $c -replace "  - schema: terra_pinyin", "  - schema: terra_pinyin`n  - schema: wubi86"
    Set-Content -LiteralPath $default -Value $c -Encoding utf8NoBOM
}
Write-Output "vendor 就绪: $vendor"
