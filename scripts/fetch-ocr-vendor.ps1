# 获取发布用的 RapidOCR 模型（PP-OCRv5 中文 mobile，不入库，gitignored 于 vendor/）：
#   1) ch_PP-OCRv5_det_mobile.onnx（检测）
#   2) ch_PP-OCRv5_rec_mobile.onnx（识别）
#   3) ppocrv5_dict.txt（字典）
# 产物: vendor/ocr/
# 用法: pwsh scripts/fetch-ocr-vendor.ps1   （需 PowerShell 7+）
# 说明: URL/SHA256 与 rapidocr-core 0.2.2 的 model.rs 常量对齐（改版本须同步）。
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$repoRoot = Split-Path -Parent $PSScriptRoot
$vendor = Join-Path $repoRoot "vendor\ocr"
New-Item -ItemType Directory -Path $vendor -Force | Out-Null

$models = @(
    @{ Name = "ch_PP-OCRv5_det_mobile.onnx"; Url = "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx"; Sha256 = "4d97c44a20d30a81aad087d6a396b08f786c4635742afc391f6621f5c6ae78ae" },
    @{ Name = "ch_PP-OCRv5_rec_mobile.onnx"; Url = "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile.onnx"; Sha256 = "5825fc7ebf84ae7a412be049820b4d86d77620f204a041697b0494669b1742c5" },
    @{ Name = "ppocrv5_dict.txt";           Url = "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/paddle/PP-OCRv5/rec/ch_PP-OCRv5_rec_server/ppocrv5_dict.txt"; Sha256 = "d1979e9f794c464c0d2e0b70a7fe14dd978e9dc644c0e71f14158cdf8342af1b" }
)

foreach ($m in $models) {
    $dst = Join-Path $vendor $m.Name
    if (Test-Path $dst) {
        $h = (Get-FileHash $dst -Algorithm SHA256).Hash.ToLower()
        if ($h -eq $m.Sha256) {
            Write-Host "已存在且校验通过（跳过）: $($m.Name)" -ForegroundColor Green
            continue
        }
        Write-Host "已存在但校验失败，重新下载: $($m.Name)" -ForegroundColor Yellow
    }
    Write-Host "下载 $($m.Name) ..." -ForegroundColor Cyan
    Invoke-WebRequest -Uri $m.Url -OutFile $dst -Headers @{ "User-Agent" = "Mozilla/5.0" }
    $h = (Get-FileHash $dst -Algorithm SHA256).Hash.ToLower()
    if ($h -ne $m.Sha256) {
        Remove-Item $dst -Force
        throw "SHA256 校验失败: $($m.Name) 期望 $($m.Sha256) 实际 $h"
    }
    Write-Host "  -> $((Get-Item $dst).Length) bytes，SHA256 校验通过" -ForegroundColor Green
}

Write-Host ""
Write-Host "OCR 模型就绪: $vendor" -ForegroundColor Green
