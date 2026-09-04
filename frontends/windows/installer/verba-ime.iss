; Verba · 拾言输入法 Windows 安装脚本（Inno Setup 6）
; 用法: ISCC.exe [/DMyAppVersion=<版本>] verba-ime.iss   （需以管理员运行以注册 TSF 档案）
; 版本: 发布流水线用 /DMyAppVersion 注入根 Cargo.toml 的 workspace 版本（见 docs/building.md）。

#ifndef MyAppVersion
  #define MyAppVersion "0.2.6"
#endif

#define AppName "Verba 拾言输入法"
#define AppVersion MyAppVersion
#define AppPublisher "Verba Contributors"

[Setup]
AppId={{7C2D4E6A-1F3B-4A9E-8C5D-2F6B9A0E3D51}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\Verba
OutputDir=output
OutputBaseFilename=verba-ime-setup-{#MyAppVersion}
Compression=lzma2
SolidCompression=yes
PrivilegesRequired=admin
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#AppName}
SetupIconFile=..\..\..\assets\branding\verba.ico

[Files]
; 先构建（见 docs/building.md），产物路径依 release 构建调整。
; restartreplace：DLL 会被加载进每个使用输入法的应用进程（in-proc TSF COM），
; 安装时无法强替换被占用文件——登记重启替换，避免"装了新版还在跑旧 DLL"
; （exe 同理兜底；daemon 进程本身在 [Code] PrepareToInstall 已先行停止）。
Source: "..\ime\target\release\verba_ime_windows.dll"; DestDir: "{app}"; Flags: ignoreversion restartreplace
Source: "..\ime\target\release\verba-reg.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace
Source: "..\..\..\target\release\verba-trigger.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace
Source: "..\..\..\target\release\verba-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace
Source: "..\..\..\target\release\verba-settings.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace
; Rime 引擎（scripts/fetch-rime-vendor.ps1 产出）；本地未拉取时跳过（发布流水线会断言存在）
Source: "..\..\..\vendor\rime\*"; DestDir: "{app}\rime"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
; OCR 模型（PP-OCRv5，vendor/ocr 产出）：daemon 同目录查找免首次下载；
; 本地未拉取时跳过（发布流水线会断言存在）
Source: "..\..\..\vendor\ocr\*"; DestDir: "{app}\models-rapidocr"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist

[Code]
var
  TaskkillExitCode: Integer;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  // 升级前停掉在跑的 daemon：IPC 管道名固定，旧 daemon 存活时新 DLL 的
  // ensure_daemon()「管道有人应答就不拉新」会一直连着旧版跑（v0.2.6 升级
  // 真机踩坑：新 DLL + 旧 daemon 混搭）。taskkill 无进程时返回非零，忽略。
  // DLL 已加载进运行中的应用进程不在此强杀（杀用户应用不可接受），由
  // restartreplace 在重启时完成替换；新开窗口自动加载新 DLL。
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM verba-daemon.exe /F /T', '',
    SW_HIDE, ewWaitUntilTerminated, TaskkillExitCode);
  Result := '';
end;

[Run]
; 先注销旧档案（清理早期版本遗留/损坏项），再注册（TSF 档案/类别，需管理员）
Filename: "{app}\verba-reg.exe"; Parameters: "unregister"; Flags: runhidden
Filename: "{app}\verba-reg.exe"; Parameters: "register ""{app}\verba_ime_windows.dll"""; Flags: runhidden

[UninstallRun]
Filename: "{app}\verba-reg.exe"; Parameters: "unregister"; Flags: runhidden

[Icons]
Name: "{autoprograms}\Verba 设置"; Filename: "{app}\verba-settings.exe"; IconFilename: "{app}\verba-settings.exe"
Name: "{autodesktop}\Verba 设置"; Filename: "{app}\verba-settings.exe"; IconFilename: "{app}\verba-settings.exe"