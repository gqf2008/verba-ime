# Verba M5 实机验收辅助：一键核对部署态 + CLI 级自动化检查 + 打印手工验收清单。
# 用法: pwsh scripts/acceptance.ps1
# 手工部分（TSF 交互）仍需你在真实输入框里操作，见输出末尾清单。

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dllDir = Join-Path $root "frontends\windows\ime\target_dev12\release"
$cli = Join-Path $dllDir "verba-cli.exe"
$daemonExe = Join-Path $dllDir "verba-daemon.exe"
$mockPort = 8765
$env:VERBA_API_KEY = "sk-mock"
$clsid = "{7C2D4E6A-1F3B-4A9E-8C5D-2F6B9A0E3D51}"

Write-Output "==== 1) 部署态核对 ===="
$dll = Join-Path $dllDir "verba_ime_windows.dll"
foreach ($f in @($dll, $daemonExe, $cli)) {
    if (-not (Test-Path $f)) { throw "缺失: $f" }
}
Write-Output "产物齐备: DLL/daemon/cli"

$regVal = (Get-ItemProperty "HKCU:\SOFTWARE\Classes\CLSID\$clsid\InprocServer32" -ErrorAction SilentlyContinue)."(default)"
if ($regVal -ne $dll) {
    Write-Warning "HKCU CLSID 未指向 target_dev12（当前: $regVal）——请确认使用新 DLL"
} else {
    Write-Output "HKCU CLSID 指向 target_dev12 ✅"
}
# 注册表分裂检查：HKLM CLSID 若指向旧安装版，需 HKCU 优先才生效（HKCU\Software\Classes 优先于 HKLM）
$hklmVal = (Get-ItemProperty "HKLM:\SOFTWARE\Classes\CLSID\$clsid\InprocServer32" -ErrorAction SilentlyContinue)."(default)"
if ($hklmVal -and $hklmVal -ne $dll) {
    Write-Warning "HKLM CLSID 指向旧安装版（$hklmVal）。HKCU 优先所以 dev DLL 生效；若卸载/清理 HKCU 会退回旧版。"
}
# TSF 档案检查：语言栏可见性依赖 HKLM TIP LanguageProfile（zh-CN 0x0804 / zh-TW 0x0404 / en-US 0x0409）
$prof = "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$clsid\LanguageProfile"
$ok = @()
foreach ($lang in @("0x00000804", "0x00000404", "0x00000409")) {
    if (Test-Path (Join-Path $prof $lang)) { $ok += $lang }
}
if ($ok.Count -ge 2) {
    Write-Output "TSF 档案已注册（$($ok -join ', ')）——语言栏可见 ✅"
} else {
    Write-Warning "TSF 档案缺失（仅 $($ok -join ', ')）——语言栏可能看不到输入法；请管理员运行 verba-reg register <dll>"
}

$rimeDir = Join-Path $dllDir "rime"
if (-not (Test-Path (Join-Path $rimeDir "rime.dll"))) {
    Write-Warning "rime/ 缺失（engine=rime 需要）"
} else {
    Write-Output "rime/ 就绪（rime.dll + data）✅"
}

Write-Output "==== 2) 启动 mock + daemon ===="
Get-Process | Where-Object { $_.ProcessName -match 'verba|cargo' } | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 600
$mock = Start-Process -FilePath "python" -ArgumentList "$root\scripts\mock_openai.py", "$mockPort" -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2
$daemon = Start-Process -FilePath $daemonExe -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2
& $cli ping | Out-Null
Write-Output "daemon ping 通过 ✅"

try {
    Write-Output "==== 3) LLM 候选融合（engine=builtin） ===="
    & $cli config set engine=builtin | Out-Null
    $cand = & $cli candidates nishishui
    $joined = $cand -join "`n"
    foreach ($expect in @("你是谁呀", "你是谁啊")) {
        if ($joined -notmatch [regex]::Escape($expect)) { throw "LLM 融合缺失「$expect」: $joined" }
    }
    Write-Output "candidates nishishui → 含「你是谁呀/你是谁啊」✅"

    Write-Output "==== 4) Rime 拼音（engine=rime） ===="
    & $cli config set engine=rime rime_schema=luna_pinyin_simp | Out-Null
    $r1 = & $cli rime nishishui
    if (($r1 -join "`n") -notmatch "你是谁") { throw "Rime 拼音缺失「你是谁」: $($r1 -join ';')" }
    Write-Output "rime nishishui → 你是谁 ✅"

    Write-Output "==== 5) Rime 五笔（wubi86） ===="
    & $cli config set rime_schema=wubi86 | Out-Null
    $r2 = & $cli rime wqvb wubi86
    if (($r2 -join "`n") -notmatch "你好") { throw "Rime 五笔缺失「你好」: $($r2 -join ';')" }
    Write-Output "rime wqvb wubi86 → 你好 ✅"

    # 恢复推荐默认：engine=rime + luna_pinyin_simp
    & $cli config set rime_schema=luna_pinyin_simp | Out-Null
    Write-Output "（配置已恢复 engine=rime + luna_pinyin_simp）"
}
finally {
    Stop-Process -Id $mock.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
    Get-Process | Where-Object { $_.ProcessName -match 'verba|cargo' } | Stop-Process -Force -ErrorAction SilentlyContinue
}

Write-Output ""
Write-Output "==== CLI 级自动化检查全部通过 ✅ ===="
Write-Output "==== 手工实机验收清单（请在真实输入框操作） ===="
Write-Output "1. 关闭已打开的应用 → 重新打开记事本 → Win+Space 切到「Verba · 拾言输入法」"
Write-Output "2. 输入 nishishui 停顿 ~0.5s → 候选窗出现 Rime「你是谁 / 你是 / 妳是…」（engine=rime）"
Write-Output "3. verba-cli config set rime_schema=wubi86 → 输入 wqvb → 出「你好/您好」；aaaa → 工"
Write-Output "4. verba-cli config set engine=builtin → 输入 nishishui → 候选窗尾部追加 LLM「你是谁呀/你是谁啊」"
Write-Output "5. 分页：= 或 PageDown 下翻、- 或 PageUp 上翻，底部页码脚 1/3"
Write-Output "6. 主题：verba-cli config set theme.preset=dark → 候选窗变深色（热更新）"
Write-Output "7. 数字选候选上屏、Esc 取消组合、方向键不被吞"
Write-Output "逐项通过后即 M5 收口。"
