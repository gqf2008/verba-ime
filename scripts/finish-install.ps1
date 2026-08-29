# Verba v0.2.3 物理机收尾脚本（管理员运行）：
# 1) 用 %LOCALAPPDATA%\Verba\ime 的 verba-reg 正式注册（CLSID + TSF 档案 +
#    类别——含显示属性 provider 类别，组合下划线生效的前提）
# 2) 删除 HKCU 开发模式覆盖注册（回退正式注册）
# 前置：本机已部署 %LOCALAPPDATA%\Verba\ime（DLL + daemon + rime + verba-reg）。
# 运行后输入法 = HKLM 正式注册 → %LOCALAPPDATA%\Verba\ime（0.2.3 代码 + 下划线）。

$ErrorActionPreference = "Stop"
$ime = Join-Path $env:LOCALAPPDATA "Verba\ime"
$verbaReg = Join-Path $ime "verba-reg.exe"
$clsid = "{7C2D4E6A-1F3B-4A9E-8C5D-2F6B9A0E3D51}"

if (-not (Test-Path $verbaReg)) {
    Write-Host "错误: 未找到 $verbaReg（先部署稳定目录）" -ForegroundColor Red
    exit 1
}

Write-Host "== 1/2 正式注册（含显示属性 provider 类别）==" -ForegroundColor Cyan
& $verbaReg register (Join-Path $ime "verba_ime_windows.dll")
if ($LASTEXITCODE -ne 0) {
    Write-Host "verba-reg register 失败 (exit=$LASTEXITCODE)" -ForegroundColor Red
    exit 1
}
Write-Host "注册完成" -ForegroundColor Green

Write-Host "== 2/2 清理 HKCU 开发注册 ==" -ForegroundColor Cyan
$key = "HKCU:\Software\Classes\CLSID\$clsid"
if (Test-Path $key) {
    Remove-Item -Path $key -Recurse -Force
    Write-Host "已删除 HKCU 开发注册" -ForegroundColor Green
} else {
    Write-Host "HKCU 开发注册不存在（已清理）" -ForegroundColor Yellow
}

Write-Host "== 验证 ==" -ForegroundColor Cyan
$hk = Get-ItemProperty "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\CLSID\$clsid\InprocServer32" -ErrorAction SilentlyContinue
if ($hk) { Write-Host "正式注册指向: $($hk.'(default)')" -ForegroundColor Green }
$cat = Test-Path "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\CTF\TIP\$clsid\Category\Category\{046B8C80-1647-40F7-9B21-B93B81AABC1B}\$clsid"
if ($cat) { Write-Host "显示属性 provider 类别已注册 ✓（组合下划线生效）" -ForegroundColor Green } else { Write-Host "警告: provider 类别未找到" -ForegroundColor Yellow }
Write-Host ""
Write-Host "完成。新开应用输入拼音验证下划线；旧应用需重启。" -ForegroundColor Cyan
