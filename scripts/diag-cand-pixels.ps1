# 候选窗像素抓取：检测 VerbaCandidateWindow 显示时，从屏幕 DC 抓取窗口矩形内容存 PNG。
# 用法: pwsh -File diag-cand-pixels.ps1 <目标进程名或PID；空=全部>（"0" 与空等同=全部）
# 注意：候选窗在 TSF 宿主进程（前台应用）内创建，PID 每次会话变化——传进程名（如 Terminal）
# 或具体 PID 均可。候选窗是 GDI 直绘窗口（不走 WM_PAINT），PrintWindow 抓不到内容，
# 故用屏幕 BitBlt 取窗口矩形（候选窗 TOPMOST，可见时不被遮挡）。
# 窗口类名来源：frontends/windows/ime/src/candidate_window.rs 的 CAND_CLASS（见 verba-cand-common.ps1）。
. $PSScriptRoot\verba-cand-common.ps1

Set-VerbaCandDpiAware | Out-Null
$target = if ($args.Count -gt 0) { $args[0] } else { "" }
$outDir = Join-Path $env:TEMP "verba-cand-pix"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
# 清掉上次运行残留，避免超时后把旧 PNG 误当本次产物
Get-ChildItem $outDir -Filter *.png -ErrorAction SilentlyContinue | Remove-Item -Force
$runStart = Get-Date
"监控开始 $(Get-Date -Format HH:mm:ss.fff) target=$target outDir=$outDir" | Out-File (Join-Path $outDir "capture.log") -Encoding utf8

# 目标解析：空=全部；数字=PID（0 也按全部处理，与 diag-cand-window.ps1 语义一致）；
# 否则按进程名精确匹配（自动去掉 .exe 后缀，通配符按字面处理；找不到则报错退出）
$targetPids = @()
if ($target -ne "") {
    if ($target -match '^\d+$') {
        try {
            $targetPids = @([uint32]$target)
        } catch {
            Write-Error "无效 PID: $target"
            exit 1
        }
    } else {
        $name = $target -replace '\.exe$', ''
        $procs = Get-Process -Name ([System.Management.Automation.WildcardPattern]::Escape($name)) -ErrorAction SilentlyContinue
        if (-not $procs) {
            Write-Error "找不到进程名: $target（进程名不带 .exe 后缀）"
            exit 1
        }
        $targetPids = @($procs | ForEach-Object { [uint32]$_.Id })
    }
}

Add-Type -AssemblyName System.Drawing
$deadline = (Get-Date).AddSeconds(90)
$captured = $false
while ((Get-Date) -lt $deadline -and -not $captured) {
    $hits = Get-VerbaCandidateWindowHits -TargetPids $targetPids
    foreach ($w in $hits) {
        if (-not $w.Visible) { continue }
        # 抓取前复核可见性：枚举后窗口可能已被 SW_HIDE（组合提交），
        # 避免把窗口下方内容当作候选窗画面。
        if (-not [VerbaCand]::IsWindowVisible($w.H)) { continue }
        $r2 = New-Object VerbaCand+RECT
        if (-not [VerbaCand]::GetWindowRect($w.H, [ref]$r2)) { continue }
        $ww = $r2.R - $r2.L; $hh = $r2.B - $r2.T
        if ($ww -le 0 -or $hh -le 0) { continue }
        $srcDc = [VerbaCand]::GetDC([IntPtr]::Zero)
        $bmp = $null; $g = $null; $hdc = [IntPtr]::Zero
        try {
            $bmp = New-Object System.Drawing.Bitmap($ww, $hh)
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $hdc = $g.GetHdc()
            $ok = [VerbaCand]::BitBlt($hdc, 0, 0, $ww, $hh, $srcDc, $r2.L, $r2.T, 0x00CC0020)
            $g.ReleaseHdc($hdc); $hdc = [IntPtr]::Zero
            if (-not $ok) {
                # BitBlt 失败不得保存黑图冒充捕获：记录后重试下一轮
                $line = "blt失败 $(Get-Date -Format HH:mm:ss.fff) pid=$($w.Pid) rect=$($r2.L),$($r2.T),$($r2.R),$($r2.B) — 桌面 DC 不可用/会话锁定?，重试下一轮"
                $line | Out-File (Join-Path $outDir "capture.log") -Append -Encoding utf8
                Write-Output $line
                continue
            }
            $fname = Join-Path $outDir ("cand_pid{0}_{1}.png" -f $w.Pid, (Get-Date -Format HHmmssfff))
            $bmp.Save($fname, [System.Drawing.Imaging.ImageFormat]::Png)
            # 日志 rect 用抓取实际使用的 $r2（枚举时的旧值可能与画面不符）
            $line = "捕获 $(Get-Date -Format HH:mm:ss.fff) pid=$($w.Pid) rect=$($r2.L),$($r2.T),$($r2.R),$($r2.B) size=${ww}x${hh} blt=$ok file=$fname"
            $line | Out-File (Join-Path $outDir "capture.log") -Append -Encoding utf8
            Write-Output $line
            $captured = $true
            break
        } catch {
            $line = "捕获失败 $(Get-Date -Format HH:mm:ss.fff) pid=$($w.Pid): $($_.Exception.Message)"
            $line | Out-File (Join-Path $outDir "capture.log") -Append -Encoding utf8
            Write-Output $line
        } finally {
            if ($hdc -ne [IntPtr]::Zero) { try { $g.ReleaseHdc($hdc) } catch {} }
            if ($g) { $g.Dispose() }
            if ($bmp) { $bmp.Dispose() }
            [VerbaCand]::ReleaseDC([IntPtr]::Zero, $srcDc) | Out-Null
        }
    }
    Start-Sleep -Milliseconds 250
}
if (-not $captured) {
    "超时未捕获（期望窗口类: VerbaCandidateWindow）$(Get-Date -Format HH:mm:ss.fff)" | Out-File (Join-Path $outDir "capture.log") -Append -Encoding utf8
    Write-Output "超时"
    exit 1
}
# 只列本次运行产生的 PNG（上次运行残留因被占用删不掉时不得混入本次产物）
Get-ChildItem $outDir -Filter *.png | Where-Object { $_.LastWriteTime -ge $runStart } | ForEach-Object { $_.FullName }
