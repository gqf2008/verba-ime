# verba-cand-common.ps1 —— 候选窗诊断公共部分
# （diag-cand-full.ps1 / diag-cand-pixels.ps1 / diag-cand-window.ps1 共用，dot-source 引入）
# 单一来源：P/Invoke 声明、窗口类名、EnumWindows 轮询回调。
# 窗口类名与 frontends/windows/ime/src/candidate_window.rs 的 CAND_CLASS 保持一致；
# 改名时必须同步本文件，否则三个诊断脚本都会静默超时。

if (-not ("VerbaCand" -as [type])) {
    Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class VerbaCand {
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lp);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern int GetWindowRgn(IntPtr h, IntPtr rgn);
    [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
    [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
    [DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int i);
    [DllImport("user32.dll")] public static extern IntPtr MonitorFromWindow(IntPtr h, uint flags);
    [DllImport("user32.dll")] public static extern bool GetMonitorInfoW(IntPtr h, ref MONITORINFO mi);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int idx);
    [DllImport("gdi32.dll")] public static extern IntPtr CreateRectRgn(int a, int b, int c, int d);
    [DllImport("gdi32.dll")] public static extern bool GetRgnBox(IntPtr h, out RECT r);
    [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr h);
    [DllImport("gdi32.dll")] public static extern bool BitBlt(IntPtr hdc, int x, int y, int w, int h, IntPtr src, int sx, int sy, uint rop);
    public delegate bool EnumWindowsProc(IntPtr h, IntPtr lp);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] public struct MONITORINFO { public int cbSize; public RECT rcMonitor; public RECT rcWork; public uint dwFlags; }
}
"@
}

$script:VerbaCandClass = "VerbaCandidateWindow"

# SetProcessDpiAwarenessContext 必须成功：失败时 rect 可能是虚拟化（逻辑）坐标，
# 而屏幕 DC BitBlt 取物理像素，混合 DPI 下画面错位。pwsh/powershell 默认 DPI-unaware，可正常设置。
function Set-VerbaCandDpiAware {
    $ok = [VerbaCand]::SetProcessDpiAwarenessContext([IntPtr](-4))
    if (-not $ok) {
        Write-Warning "SetProcessDpiAwarenessContext 失败（进程已声明其他 DPI 感知）——混合 DPI 下捕获可能错位"
    }
    return $ok
}

# 枚举回调：delegate 经 .NET 调用，看不到函数局部变量，只能读写 $script: 作用域；
# 且回调实例在脚本加载时创建一次（不要在轮询循环里重建），
# 依赖 $script: 变量在调用时取值——不要把它包进函数局部闭包。
$script:VerbaCandAll = $true
$script:VerbaCandTargetPids = @()
$script:VerbaCandHits = @()
$script:VerbaCandCb = [VerbaCand+EnumWindowsProc]{ param($h, $lp)
    $cls = New-Object System.Text.StringBuilder 256
    [VerbaCand]::GetClassName($h, $cls, 256) | Out-Null
    if ($cls.ToString() -eq $script:VerbaCandClass) {
        [uint32]$p = 0
        [VerbaCand]::GetWindowThreadProcessId($h, [ref]$p) | Out-Null
        if ($script:VerbaCandAll -or $script:VerbaCandTargetPids -contains $p) {
            $r = New-Object VerbaCand+RECT
            $okRect = [VerbaCand]::GetWindowRect($h, [ref]$r)
            $script:VerbaCandHits += [PSCustomObject]@{
                H       = $h
                Pid     = $p
                Visible = [VerbaCand]::IsWindowVisible($h)
                # rect 获取失败（窗口刚被销毁）记为 ERR，不得伪造 0,0,0,0
                Rect    = if ($okRect) { "$($r.L),$($r.T),$($r.R),$($r.B)" } else { "ERR" }
            }
        }
    }
    return $true
}

# 枚举候选窗。TargetPids 空数组或含 0 = 匹配全部进程（与 diag-cand-window.ps1 的 0=全部 语义一致）。
function Get-VerbaCandidateWindowHits {
    param([uint32[]]$TargetPids = @())
    $script:VerbaCandAll = ($TargetPids.Count -eq 0) -or ($TargetPids -contains 0)
    $script:VerbaCandTargetPids = @($TargetPids | Where-Object { $_ -ne 0 })
    $script:VerbaCandHits = @()
    [VerbaCand]::EnumWindows($script:VerbaCandCb, [IntPtr]::Zero) | Out-Null
    return $script:VerbaCandHits
}
