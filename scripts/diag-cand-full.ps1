# 候选窗综合诊断：检测到候选窗显示时，同时抓取：
# 1) 窗口矩形画面（屏幕 DC BitBlt——候选窗为 GDI 直绘窗口，不走 WM_PAINT，PrintWindow 得空白）
# 2) 全屏画面（虚拟屏幕实际合成结果，按虚拟屏幕边界取，兼容任意分辨率/多显示器）
# 3) 窗口状态（rect/rgn/exstyle/可见性/目标显示器）
# 用法：pwsh -File diag-cand-full.ps1 （无参数；检测到可见候选窗即抓取，最长等待 300s）
# 输出：%TEMP%\verba-cand-full\（diag.log + window_*.png + screen_*.png）
# 窗口类名来源：frontends/windows/ime/src/candidate_window.rs 的 CAND_CLASS（见 verba-cand-common.ps1）。
. $PSScriptRoot\verba-cand-common.ps1

Set-VerbaCandDpiAware | Out-Null
Add-Type -AssemblyName System.Drawing
$outDir = Join-Path $env:TEMP "verba-cand-full"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
Get-ChildItem $outDir -Filter *.png -ErrorAction SilentlyContinue | Remove-Item -Force
$log = Join-Path $outDir "diag.log"
"综合诊断开始 $(Get-Date -Format HH:mm:ss.fff)" | Out-File $log -Encoding utf8

$deadline = (Get-Date).AddSeconds(300)
$captured = $false
while ((Get-Date) -lt $deadline -and -not $captured) {
    $hits = Get-VerbaCandidateWindowHits
    foreach ($w in $hits) {
        $h = $w.H
        if (-not $w.Visible) { continue }
        $stamp = Get-Date -Format "HHmmssfff"
        $pwFile = ""
        $scrFile = ""
        # rect 获取失败（枚举后窗口被销毁，TSF 会话切换竞态）→ 跳过本轮，下轮重试
        $r = New-Object VerbaCand+RECT
        if (-not [VerbaCand]::GetWindowRect($h, [ref]$r)) {
            $line = "跳过 $(Get-Date -Format HH:mm:ss.fff) rect 获取失败（窗口已销毁?）"
            $line | Out-File $log -Append -Encoding utf8
            continue
        }
        $ww = $r.R - $r.L; $hh = $r.B - $r.T
        if ($ww -le 0 -or $hh -le 0) {
            $line = "跳过 $(Get-Date -Format HH:mm:ss.fff) 窗口尺寸为 0（rect=($($r.L),$($r.T),$($r.R),$($r.B))）"
            $line | Out-File $log -Append -Encoding utf8
            continue
        }
        # 抓取前复核可见性：枚举后窗口可能已被 SW_HIDE（组合提交），
        # 避免把窗口下方内容当作候选窗画面。
        if (-not [VerbaCand]::IsWindowVisible($h)) {
            $line = "跳过 $(Get-Date -Format HH:mm:ss.fff) 窗口已隐藏（枚举后被 SW_HIDE）"
            $line | Out-File $log -Append -Encoding utf8
            continue
        }
        $bltWin = $false; $bltScr = $false
        $srcDc = [VerbaCand]::GetDC([IntPtr]::Zero)
        $bmp = $null; $g = $null; $hdc = [IntPtr]::Zero
        try {
            # 1) 窗口矩形画面（屏幕 DC BitBlt；候选窗 TOPMOST，可见时不被遮挡）
            $bmp = New-Object System.Drawing.Bitmap($ww, $hh)
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $hdc = $g.GetHdc()
            $bltWin = [VerbaCand]::BitBlt($hdc, 0, 0, $ww, $hh, $srcDc, $r.L, $r.T, 0x00CC0020)
            $g.ReleaseHdc($hdc); $hdc = [IntPtr]::Zero
            if (-not $bltWin) {
                # BitBlt 失败不得保存黑图冒充捕获：记录后重试下一轮
                $line = "窗口 blt 失败 $(Get-Date -Format HH:mm:ss.fff)（桌面 DC 不可用/会话锁定?）— 重试下一轮"
                $line | Out-File $log -Append -Encoding utf8
                Write-Output $line
                continue
            }
            $pwFile = Join-Path $outDir "window_$stamp.png"
            $bmp.Save($pwFile, [System.Drawing.Imaging.ImageFormat]::Png)
            # 2) 全屏画面（虚拟屏幕边界；仅在窗口画面有效时抓取，避免失败重试时高频分配大位图）
            $vx = [VerbaCand]::GetSystemMetrics(76)   # SM_XVIRTUALSCREEN
            $vy = [VerbaCand]::GetSystemMetrics(77)   # SM_YVIRTUALSCREEN
            $vw = [VerbaCand]::GetSystemMetrics(78)   # SM_CXVIRTUALSCREEN
            $vh = [VerbaCand]::GetSystemMetrics(79)   # SM_CYVIRTUALSCREEN
            if ($vw -gt 0 -and $vh -gt 0) {
                $scr = $null; $sg = $null; $shdc = [IntPtr]::Zero
                try {
                    $scr = New-Object System.Drawing.Bitmap($vw, $vh)
                    $sg = [System.Drawing.Graphics]::FromImage($scr)
                    $shdc = $sg.GetHdc()
                    $bltScr = [VerbaCand]::BitBlt($shdc, 0, 0, $vw, $vh, $srcDc, $vx, $vy, 0x00CC0020)
                    $sg.ReleaseHdc($shdc); $shdc = [IntPtr]::Zero
                    if ($bltScr) {
                        $scrFile = Join-Path $outDir "screen_$stamp.png"
                        $scr.Save($scrFile, [System.Drawing.Imaging.ImageFormat]::Png)
                    }
                } finally {
                    if ($shdc -ne [IntPtr]::Zero) { try { $sg.ReleaseHdc($shdc) } catch {} }
                    if ($sg) { $sg.Dispose() }
                    if ($scr) { $scr.Dispose() }
                }
            }
        } catch {
            $line = "捕获异常 $(Get-Date -Format HH:mm:ss.fff): $($_.Exception.Message)"
            $line | Out-File $log -Append -Encoding utf8
            Write-Output $line
            # 失败退避：全屏位图可达数百 MB，避免 4Hz 高频重试
            Start-Sleep -Milliseconds 1000
            continue
        } finally {
            if ($hdc -ne [IntPtr]::Zero) { try { $g.ReleaseHdc($hdc) } catch {} }
            if ($g) { $g.Dispose() }
            if ($bmp) { $bmp.Dispose() }
            [VerbaCand]::ReleaseDC([IntPtr]::Zero, $srcDc) | Out-Null
        }
        # 3) 状态（region 用后必须 DeleteObject，否则 GDI 句柄泄漏）
        $rgn = [VerbaCand]::CreateRectRgn(0,0,0,0)
        $rt = [VerbaCand]::GetWindowRgn($h, $rgn)
        $rb = New-Object VerbaCand+RECT; $rbox = "none"
        if ($rt -ne 0 -and [VerbaCand]::GetRgnBox($rgn, [ref]$rb)) { $rbox = "($($rb.L),$($rb.T),$($rb.R),$($rb.B))" }
        [VerbaCand]::DeleteObject($rgn) | Out-Null
        $exstyle = [VerbaCand]::GetWindowLongW($h, -20)
        [uint32]$p = 0; [VerbaCand]::GetWindowThreadProcessId($h, [ref]$p) | Out-Null
        $hmon = [VerbaCand]::MonitorFromWindow($h, 2)
        $mi = New-Object VerbaCand+MONITORINFO; $mi.cbSize = [Runtime.InteropServices.Marshal]::SizeOf([type][VerbaCand+MONITORINFO])
        $monStr = "err"
        if ([VerbaCand]::GetMonitorInfoW($hmon, [ref]$mi)) {
            $monStr = "($($mi.rcMonitor.L),$($mi.rcMonitor.T),$($mi.rcMonitor.R),$($mi.rcMonitor.B))"
        }
        $line = "捕获 $stamp pid=$p rect=($($r.L),$($r.T),$($r.R),$($r.B)) size=${ww}x${hh} bltWin=$bltWin bltScr=$bltScr rgnType=$rt rgnBox=$rbox exstyle=0x$($exstyle.ToString('X')) monitor=$monStr"
        $line | Out-File $log -Append -Encoding utf8
        Write-Output $line
        Write-Output "window=$pwFile"
        Write-Output "screen=$scrFile"
        $captured = $true
        break
    }
    Start-Sleep -Milliseconds 250
}
if (-not $captured) {
    $line = "超时未捕获（期望窗口类: VerbaCandidateWindow，见 candidate_window.rs CAND_CLASS）$(Get-Date -Format HH:mm:ss.fff)"
    $line | Out-File $log -Append -Encoding utf8
    Write-Output $line
    exit 1
}
