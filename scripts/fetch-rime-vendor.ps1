# 获取发布用的 Rime 运行时（第三方二进制/数据，不入库，gitignored 于 vendor/）：
#   1) librime 1.17.0 stable Windows-msvc-x64 → rime.dll（x64）
#   2) Weasel 0.17.4 安装包 → Rime 数据（luna_pinyin_simp / opencc / …）
#   3) rime-wubi → wubi86 五笔方案
# 产物: vendor/rime/{rime.dll, data/}
# 用法: pwsh scripts/fetch-rime-vendor.ps1   （需 PowerShell 7+，utf8NoBOM）
# 说明: librime 资产名嵌 commit hash，按 tag + 名称动态解析，不写死 URL。
# 需 gh CLI（GitHub runner 预装，本地 `gh auth login`）。
$ErrorActionPreference = "Stop"
# 让 gh api 等原生命令的非零退出码直接抛错（错误 JSON 会写在 stdout，尽早浮出真因）
$PSNativeCommandUseErrorActionPreference = $true

$repoRoot = Split-Path -Parent $PSScriptRoot
$vendor = Join-Path $repoRoot "vendor\rime"
$dataDir = Join-Path $vendor "data"
$tmp = Join-Path $env:TEMP "verba-rime-vendor"

New-Item -ItemType Directory -Path $vendor -Force | Out-Null
if (Test-Path $tmp) { Remove-Item -LiteralPath $tmp -Recurse -Force }
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

function Get-7z {
    foreach ($c in @("C:\Program Files\7-Zip\7z.exe", "C:\Program Files (x86)\7-Zip\7z.exe")) {
        if (Test-Path $c) { return $c }
    }
    Write-Host "未找到 7-Zip，安装中（choco install 7zip.install）…"
    choco install 7zip.install -y --no-progress | Out-Null
    if (Test-Path "C:\Program Files\7-Zip\7z.exe") { return "C:\Program Files\7-Zip\7z.exe" }
    throw "7-Zip 安装失败"
}
$7z = Get-7z

# 1) librime 1.17.0 stable
# 用 gh api（认证，5000 次/时）而非裸 Invoke-RestMethod api.github.com（未认证
# 60 次/时/IP，CI 共享 IP 曾 403 限流）；需 gh CLI（GitHub runner 预装）
$rel = (gh api repos/rime/librime/releases/tags/1.17.0 | ConvertFrom-Json)
# 排除 rime-deps-*（后缀相同但只含 opencc 工具与 include，无 rime.dll）
$asset = $rel.assets | Where-Object { $_.name -match "Windows-msvc-x64\.7z$" -and $_.name -notmatch "^rime-deps-" } | Select-Object -First 1
if (-not $asset) { throw "未找到 librime 1.17.0 Windows-msvc-x64 资产" }
Write-Host "下载 librime: $($asset.name)"
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile (Join-Path $tmp "rime-x64.7z") -UseBasicParsing
& $7z x (Join-Path $tmp "rime-x64.7z") ("-o" + (Join-Path $tmp "rime")) -y | Out-Null
$rimeDll = Join-Path $tmp "rime\dist\lib\rime.dll"
if (-not (Test-Path $rimeDll)) { throw "librime 资产中未找到 dist\lib\rime.dll" }
Copy-Item $rimeDll $vendor -Force

# 2) Weasel 0.17.4 数据（与平台无关，macOS 侧同一来源）
$weaselUrl = "https://github.com/rime/weasel/releases/download/0.17.4/weasel-0.17.4.0-installer.exe"
Invoke-WebRequest -Uri $weaselUrl -OutFile (Join-Path $tmp "weasel.exe") -UseBasicParsing
& $7z x (Join-Path $tmp "weasel.exe") ("-o" + (Join-Path $tmp "weasel")) -y | Out-Null
if (-not (Test-Path (Join-Path $tmp "weasel\data"))) { throw "Weasel 安装包中未找到 data 目录" }
# 先清空再拷贝：增量合并会让上一版残留文件（Weasel 升级删除/改名的 schema/dict/
# opencc）静默留在 vendor 并随安装包发布、被 Rime 加载（复审 sweep）。data/ 为本
# 脚本独占产物，整体重建最安全。
if (Test-Path $dataDir) { Remove-Item $dataDir -Recurse -Force }
# 目标目录须先创建：Copy-Item 通配符 + -Recurse 到不存在的目标会对子目录条目报
# "Container cannot be copied onto existing leaf item"（PowerShell 已知行为）
New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
Copy-Item (Join-Path $tmp "weasel\data\*") $dataDir -Recurse -Force

# 3) wubi86 五笔
foreach ($f in @("wubi86.schema.yaml", "wubi86.dict.yaml")) {
    Invoke-WebRequest -Uri "https://raw.githubusercontent.com/rime/rime-wubi/master/$f" -OutFile (Join-Path $dataDir $f) -UseBasicParsing
}

# 4) default.yaml 的 schema_list 追加 wubi86（否则部署不会编译该方案）
$default = Join-Path $dataDir "default.yaml"
$c = Get-Content $default -Raw
if ($c -notmatch "wubi86") {
    $c = $c -replace "  - schema: terra_pinyin", "  - schema: terra_pinyin`n  - schema: wubi86"
    Set-Content -LiteralPath $default -Value $c -Encoding utf8NoBOM
}
# 回归守卫：-replace 以 terra_pinyin 为锚，若上游 default.yaml 改版/换锚则静默不
# 插入，五笔方案不会被部署编译——必须断言插入成功（复审 V23）。
$c2 = Get-Content $default -Raw
if ($c2 -notmatch "wubi86") { throw "wubi86 未能插入 default.yaml 的 schema_list（锚点 terra_pinyin 可能已变）" }

# 5) Verba 自定义短语（scripts/rime-extra/，biáng 等）：
#    luna_pinyin_simp 经 __include 继承 luna_pinyin.schema，而后者已内置
#    table_translator@custom_phrase 接线（rime-luna-pinyin 上游，2026-08 核实），
#    因此只需注入词条文件；接线存在性由回归守卫断言，上游改版时响亮失败。
$extra = Join-Path $repoRoot "scripts\rime-extra"
$targetTxt = Join-Path $dataDir "custom_phrase.txt"
$sourceTxt = Join-Path $extra "custom_phrase.txt"
if (Test-Path $targetTxt) {
    # 上游已带 custom_phrase.txt：先补尾换行（防词条粘连到末行），再补缺失
    # 词条行（幂等），不覆盖上游内容
    $existing = Get-Content $targetTxt -Raw
    if ($existing -and -not $existing.EndsWith("`n")) {
        Set-Content -LiteralPath $targetTxt -Value ($existing + "`n") -Encoding utf8NoBOM -NoNewline
        $existing = Get-Content $targetTxt -Raw
    }
    foreach ($line in (Get-Content $sourceTxt)) {
        if ($line -match "^\s*#" -or $line -notmatch "`t") { continue }
        if ($existing -notmatch [regex]::Escape($line)) {
            Add-Content -LiteralPath $targetTxt -Value $line -Encoding utf8NoBOM
            $existing = Get-Content $targetTxt -Raw
        }
    }
} else {
    Copy-Item $sourceTxt $targetTxt -Force
}
# 回归守卫：接线（simp 或基 schema 含 custom_phrase）与词条必须落地，
# 上游改版导致落空时此处为红（防静默失效）
$simpRaw = Get-Content (Join-Path $dataDir "luna_pinyin_simp.schema.yaml") -Raw
$baseRaw = Get-Content (Join-Path $dataDir "luna_pinyin.schema.yaml") -Raw
if (($simpRaw -notmatch "custom_phrase") -and ($baseRaw -notmatch "custom_phrase")) {
    throw "custom_phrase 接线缺失（上游 schema 已变）"
}
if ((Get-Content $targetTxt -Raw) -notmatch "`tbiang") {
    throw "biang 词条未注入 custom_phrase.txt"
}

# 6) 结构校验（发布构建依赖）
if (-not (Test-Path (Join-Path $vendor "rime.dll"))) { throw "vendor/rime/rime.dll 缺失" }
if (-not (Test-Path (Join-Path $dataDir "opencc"))) { throw "vendor/rime/data/opencc 缺失" }
if (-not (Test-Path (Join-Path $dataDir "default.yaml"))) { throw "vendor/rime/data/default.yaml 缺失" }

Write-Host "vendor 就绪: $vendor"
Get-ChildItem $vendor -Recurse -File | ForEach-Object { Write-Host "  $($_.FullName.Substring($vendor.Length + 1))" }
