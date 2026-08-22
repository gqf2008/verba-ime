; Verba · 拾言输入法 Windows 安装脚本（Inno Setup 6）
; 用法: ISCC.exe verba-ime.iss   （需以管理员运行以注册 TSF 档案）

#define AppName "Verba 拾言输入法"
#define AppVersion "0.1.0"
#define AppPublisher "Verba Contributors"

[Setup]
AppId={{7C2D4E6A-1F3B-4A9E-8C5D-2F6B9A0E3D51}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\Verba
OutputDir=output
OutputBaseFilename=verba-ime-setup
Compression=lzma2
SolidCompression=yes
PrivilegesRequired=admin
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#AppName}

[Files]
; 先构建（见 docs/building.md），产物路径依 release 构建调整
Source: "..\ime\target\release\verba_ime_windows.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\ime\target\release\verba-reg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\..\target\release\verba-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion

[Run]
; 先注销旧档案（清理早期版本遗留/损坏项），再注册（TSF 档案/类别，需管理员）
Filename: "{app}\verba-reg.exe"; Parameters: "unregister"; Flags: runhidden
Filename: "{app}\verba-reg.exe"; Parameters: "register ""{app}\verba_ime_windows.dll"""; Flags: runhidden

[UninstallRun]
Filename: "{app}\verba-reg.exe"; Parameters: "unregister"; Flags: runhidden

[Icons]
Name: "{autoprograms}\Verba 设置（开发占位）"; Filename: "{app}\verba-reg.exe"