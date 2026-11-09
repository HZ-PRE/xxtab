#ifndef AppVersion
  #define AppVersion "0.1.3"
#endif
#define RepoRoot SourcePath + "..\..\"
#define BinaryDir RepoRoot + "target\installer\x86_64-pc-windows-msvc\release\"
#ifdef InstallerTest
  #define ProductName "xxtab Packaging Test"
  #define ProductId "xxtab-packaging-test-7983c12d"
  #define SetupName "xxtab-setup-test"
#else
  #define ProductName "xxtab"
  #define ProductId "{{5703AA41-24C8-4A21-BA2C-EDCF9F6AB688}"
  #define SetupName "xxtab-" + AppVersion + "-windows-x64-setup"
#endif

[Setup]
AppId={#ProductId}
AppName={#ProductName}
AppVersion={#AppVersion}
AppPublisher=xxtab contributors
DefaultDirName={autopf}\{#ProductName}
DefaultGroupName={#ProductName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
PrivilegesRequired=admin
OutputDir={#RepoRoot}dist\installers
OutputBaseFilename={#SetupName}
SetupIconFile={#RepoRoot}assets\xxtab.ico
UninstallDisplayIcon={app}\xxtab-gui.exe
Compression=lzma2
SolidCompression=yes
LZMANumBlockThreads=1
WizardStyle=modern
LicenseFile={#RepoRoot}LICENSE
InfoBeforeFile=install-info.txt
CloseApplications=no
RestartApplications=no
SetupLogging=yes
ShowLanguageDialog=no

[Languages]
Name: "zhcn"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "快捷方式："

[Files]
; Runtime-only allowlist: programs, required dependency and license notices.
Source: "{#BinaryDir}xxtab-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinaryDir}xxtab.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}.tools\wireguard-runtime\wd.exe"; DestDir: "{app}\runtime"; Flags: ignoreversion
Source: "{#RepoRoot}LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "WireGuard-LICENSE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "third-party.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{group}\{#ProductName}"; Filename: "{app}\xxtab-gui.exe"; WorkingDir: "{app}"
Name: "{group}\卸载 {#ProductName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#ProductName}"; Filename: "{app}\xxtab-gui.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\xxtab-gui.exe"; Description: "打开 {#ProductName}"; Flags: postinstall nowait skipifsilent unchecked

[Code]
function CreateFileW(FileName: string; DesiredAccess, ShareMode, SecurityAttributes,
  CreationDisposition, FlagsAndAttributes, TemplateFile: Cardinal): THandle;
  external 'CreateFileW@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';

function FileInUse(const Path: string): Boolean;
var
  Handle: THandle;
begin
  Result := False;
  if not FileExists(Path) then Exit;
  { A running executable cannot be opened for exclusive write. Never kill it. }
  Handle := CreateFileW(Path, $40000000, 0, 0, 3, $80, 0);
  Result := Handle = THandle(-1);
  if not Result then CloseHandle(Handle);
end;

function ApplicationInUse: Boolean;
begin
  Result := FileInUse(ExpandConstant('{app}\xxtab-gui.exe')) or
    FileInUse(ExpandConstant('{app}\xxtab.exe'));
end;

function PrepareToInstall(var NeedsRestart: Boolean): string;
begin
  Result := '';
  if ApplicationInUse then begin
    Result := '安装目录中的 xxtab 正在运行或文件被占用。请先断开连接并正常退出，再重试。';
    Exit;
  end;
end;

function InitializeUninstall: Boolean;
begin
  Result := not ApplicationInUse;
  if not Result then
    SuppressibleMsgBox('请先断开连接并正常退出安装目录中的 xxtab，再卸载。用户配置将保留。',
      mbError, MB_OK, IDOK);
end;
