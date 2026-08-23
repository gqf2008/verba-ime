@echo off
rem 用 MSVC 工具链构建（RapidOCR/ort 需要 x86_64-pc-windows-msvc）。用法: scripts\build-msvc.cmd <cargo 子命令+参数>
call "C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\VsDevCmd.bat" -arch=x64 >nul
cargo %*
