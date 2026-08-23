# Verba 端到端冒烟：mock LLM -> daemon -> CLI
#   M1: ping / config set / ai 流式
#   M5: candidates 候选融合（词库 + LLM 候选，经 LlmCandidates/Candidates 协议）
# 用法: pwsh scripts/smoke_test.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$mockPort = 8765
$env:VERBA_API_KEY = "sk-mock"

# 清理旧进程（注意：python 进程名不含脚本名，须按 PID 精确停 mock）
Get-Process | Where-Object { $_.ProcessName -match 'verba|cargo' } | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800

# 1) mock LLM（候选模式：提示词含「拼音：」时按行返回候选）
$mock = Start-Process -FilePath "python" -ArgumentList "$root\scripts\mock_openai.py", "$mockPort" -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

# 2) daemon（配置用默认，随后用 CLI 热更新）
$daemon = Start-Process -FilePath "cargo" -ArgumentList "run","-q","-p","verba-daemon" -WorkingDirectory $root -WindowStyle Hidden -RedirectStandardOutput "$env:TEMP\vd.out" -RedirectStandardError "$env:TEMP\vd.err" -PassThru
Start-Sleep -Seconds 8

try {
    Write-Output "==== ping ===="
    cargo run -q -p verba-cli -- ping

    Write-Output "==== config set（热更新指向 mock） ===="
    cargo run -q -p verba-cli -- config set "llm_base_url=http://127.0.0.1:$mockPort/v1" llm_model=mock max_tokens=256
    Write-Output "==== config ===="
    cargo run -q -p verba-cli -- config

    Write-Output "==== ai 你好 (模拟 // 链路) ===="
    cargo run -q -p verba-cli -- ai "你好"

    Write-Output "==== candidates nishishui (候选融合) ===="
    $candOut = cargo run -q -p verba-cli -- candidates nishishui
    $candOut
    $candJoined = $candOut -join "`n"
    foreach ($expect in @("你是谁呀", "你是谁啊", "你就是你", "谁是你", "你是谁呢")) {
        if ($candJoined -notmatch [regex]::Escape($expect)) {
            throw "候选融合冒烟失败：未收到预期候选「$expect」（输出：$candJoined）"
        }
    }
    Write-Output "==== 候选融合冒烟通过 ===="
}
finally {
    # 清理：按 PID 停 mock/daemon，并停残留 verba/cargo
    Stop-Process -Id $mock.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
    Get-Process | Where-Object { $_.ProcessName -match 'verba|cargo' } | Stop-Process -Force -ErrorAction SilentlyContinue
    Write-Output "==== smoke done ===="
}
