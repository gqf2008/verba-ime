# Verba M1 端到端冒烟：mock LLM -> daemon -> CLI（ping / config set / ai 流式）
# 用法: pwsh scripts/smoke_test.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$mockPort = 8765
$env:VERBA_API_KEY = "sk-mock"

# 清理旧进程
Get-Process | Where-Object { $_.ProcessName -match 'verba|cargo|mock_openai' } | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800

# 1) mock LLM
$mock = Start-Process -FilePath "python" -ArgumentList "$root\scripts\mock_openai.py", "$mockPort" -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

# 2) daemon（配置用默认，随后用 CLI 热更新）
$daemon = Start-Process -FilePath "cargo" -ArgumentList "run","-q","-p","verba-daemon" -WorkingDirectory $root -WindowStyle Hidden -RedirectStandardOutput "$env:TEMP\vd.out" -RedirectStandardError "$env:TEMP\vd.err" -PassThru
Start-Sleep -Seconds 8

Write-Output "==== ping ===="
cargo run -q -p verba-cli -- ping
Write-Output "==== config set（热更新指向 mock） ===="
cargo run -q -p verba-cli -- config set "llm_base_url=http://127.0.0.1:$mockPort/v1" llm_model=mock max_tokens=256
Write-Output "==== config ===="
cargo run -q -p verba-cli -- config
Write-Output "==== ai 你好 (模拟 // 链路) ===="
cargo run -q -p verba-cli -- ai "你好"

# 清理
Get-Process | Where-Object { $_.ProcessName -match 'verba|cargo|mock_openai' } | Stop-Process -Force -ErrorAction SilentlyContinue
Write-Output "==== smoke done ===="