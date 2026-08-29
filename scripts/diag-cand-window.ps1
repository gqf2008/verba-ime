# 诊断候选窗：检测 VerbaCandidateWindow 的可见性翻转并记录物理坐标（per-monitor v2）。
# 用法：pwsh -File diag-cand-window.ps1 [目标PID]（省略或 0 = 全部进程）。
# 注意：候选窗创建在 TSF 宿主进程内（即当前前台应用，Terminal/记事本等），PID 每次会话都不同，
# 因此省略参数（匹配全部）通常比猜一个 PID 更可靠。
# 窗口类名来源：frontends/windows/ime/src/candidate_window.rs 的 CAND_CLASS（见 verba-cand-common.ps1）。
. $PSScriptRoot\verba-cand-common.ps1

Set-VerbaCandDpiAware | Out-Null

$targetArg = if ($args.Count -gt 0) { $args[0] } else { "" }
if ($targetArg -ne "" -and $targetArg -notmatch '^\d+$') {
    Write-Error "目标参数必须是 PID（数字）或省略（全部进程）：$targetArg"
    exit 1
}
$targetPids = @()
if ($targetArg -ne "") {
    try {
        $targetPids = @([uint32]$targetArg)
    } catch {
        Write-Error "无效 PID: $targetArg"
        exit 1
    }
}
$outFile = Join-Path $env:TEMP "verba-cand-diag.txt"
"target=$targetArg out=$outFile $(Get-Date -Format HH:mm:ss)" | Out-File $outFile -Encoding utf8

# 每 100ms 采样一次，最多 40 秒；检测到可见即记录并退出。
# （候选窗随击键快速显隐，300ms 采样会漏掉 <300ms 的完整显隐周期，故用 100ms。）
$deadline = (Get-Date).AddSeconds(40)
$done = $false
while ((Get-Date) -lt $deadline -and -not $done) {
    $w = Get-VerbaCandidateWindowHits -TargetPids $targetPids
    foreach ($cand in $w) {
        if ($cand.Visible) {
            $line = "$(Get-Date -Format HH:mm:ss.fff) visible=True rect=$($cand.Rect) <-- 候选窗可见!"
            $line | Out-File $outFile -Append -Encoding utf8
            Write-Output $line
            $done = $true
            break
        }
    }
    Start-Sleep -Milliseconds 100
}
if (-not $done) {
    $line = "超时未捕获到可见候选窗（期望窗口类: VerbaCandidateWindow）$(Get-Date -Format HH:mm:ss)"
    $line | Out-File $outFile -Append -Encoding utf8
    Write-Output $line
    exit 1
}
