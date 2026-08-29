# 清理 HKCU 开发模式注册（v0.2.3 正式安装后运行）。
# 前置：先用 verba-ime-setup-0.2.3.exe 安装正式版（更新 HKLM 注册 + 安装目录 DLL），
# 再运行本脚本删除 HKCU 覆盖键，输入法回退到正式注册（安装目录 0.2.3）。
# 验证：删除后 `Get-ItemProperty "HKCU:\Software\Classes\CLSID\{7C2D4E6A-1F3B-4A9E-8C5D-2F6B9A0E3D51}\InprocServer32"` 应报不存在。
$clsid = "{7C2D4E6A-1F3B-4A9E-8C5D-2F6B9A0E3D51}"
$key = "HKCU:\Software\Classes\CLSID\$clsid"
if (Test-Path $key) {
    Remove-Item -Path $key -Recurse -Force
    "已删除 HKCU 开发注册：$key"
} else {
    "HKCU 开发注册不存在（已清理或未创建）"
}
# 确认正式注册（HKLM）指向安装目录
$hk = Get-ItemProperty "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\CLSID\$clsid\InprocServer32" -ErrorAction SilentlyContinue
if ($hk) { "正式注册指向: $($hk.'(default)')" } else { "警告: HKLM 注册缺失（需重跑安装包）" }
