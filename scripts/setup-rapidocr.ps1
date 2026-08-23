# 安装 RapidOCR 本地 OCR 运行环境（Python venv + rapidocr_onnxruntime）。
# 用法: pwsh scripts/setup-rapidocr.ps1
# 说明: 本机 Rust 工具链为 x86_64-pc-windows-gnu（无 MSVC），`ort` 无 windows-gnu 预编译，
#       故 Verba 的 `rapid` OCR provider 通过子进程调用 Python `rapidocr_onnxruntime`
#       （同 PaddleOCR PP-OCRv4 算法/模型）。此脚本在数据目录创建 venv-ocr 并安装依赖。
$ErrorActionPreference = "Stop"
$appData = [Environment]::GetFolderPath("ApplicationData")
# 与 verba-config::VerbaDirs 一致：data_dir = %APPDATA%\verba\Verba\data（Windows）
$dataDir = Join-Path $appData "verba\Verba\data"
$venv = Join-Path $dataDir "venv-ocr"
$python = Join-Path $venv "Scripts\python.exe"

Write-Output "数据目录: $dataDir"
New-Item -ItemType Directory -Force -Path $dataDir | Out-Null

if (-not (Test-Path $python)) {
    Write-Output "创建 venv: $venv"
    python -m venv $venv
    if ($LASTEXITCODE -ne 0) { throw "创建 venv 失败" }
}

Write-Output "安装 rapidocr_onnxruntime ..."
& $python -m pip install --upgrade --quiet rapidocr_onnxruntime
if ($LASTEXITCODE -ne 0) { throw "安装 rapidocr_onnxruntime 失败" }

& $python -c "import rapidocr_onnxruntime; print('RapidOCR 就绪:', rapidocr_onnxruntime.__version__)"
Write-Output "完成。在设置面板把 OCR 切到 `"rapid（本地 RapidOCR/PaddleOCR）`" 即可使用。"
