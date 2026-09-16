#ifndef AppVersion
  #define AppVersion "0.1.1"
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
; Explicit allowlist: exclude docs, dist backups and local/user configurations.
Source: "{#BinaryDir}xxtab-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinaryDir}xxtab.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RepoRoot}tools\diagnose-network.ps1"; DestDir: "{app}\tools"; Flags: ignoreversion
Source: "{#RepoRoot}deploy\nginx-xxtab.conf.example"; DestDir: "{app}\examples"; Flags: onlyifdoesntexist
Source: "{#RepoRoot}examples\xxtab.toml"; DestDir: "{app}\examples"; Flags: onlyifdoesntexist
Source: "{#RepoRoot}examples\wg.conf.example"; DestDir: "{app}\examples"; Flags: onlyifdoesntexist
Source: "WireGuard-LICENSE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "third-party.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#RepoRoot}.tools\wireguard-amd64-0.5.3.msi"; Flags: dontcopy noencryption

[Icons]
Name: "{group}\{#ProductName}"; Filename: "{app}\xxtab-gui.exe"; WorkingDir: "{app}"
Name: "{group}\卸载 {#ProductName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#ProductName}"; Filename: "{app}\xxtab-gui.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\xxtab-gui.exe"; Description: "打开 {#ProductName}"; Flags: postinstall nowait skipifsilent unchecked; Check: CanLaunch

[Code]
var
  DependencyRestart: Boolean;

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

function WireGuardInstalled: Boolean;
begin
  Result := FileExists(ExpandConstant('{commonpf64}\WireGuard\wireguard.exe'));
end;

function PrepareToInstall(var NeedsRestart: Boolean): string;
var
  Code: Integer;
  MsiPath: string;
begin
  Result := '';
  if ApplicationInUse then begin
    Result := '安装目录中的 xxtab 正在运行或文件被占用。请先断开连接并正常退出，再重试。';
    Exit;
  end;
  if WireGuardInstalled then begin
    Log('WireGuard already installed; preserving existing installation.');
    Exit;
  end;
  WizardForm.StatusLabel.Caption := '正在安装官方 WireGuard，请稍候…';
  ExtractTemporaryFile('wireguard-amd64-0.5.3.msi');
  MsiPath := ExpandConstant('{tmp}\wireguard-amd64-0.5.3.msi');
  if CompareText(GetSHA256OfFile(MsiPath),
    '76fcec042c5989c5b816cd32eaed1e5b1c3b998a4b1c9eca55f299e3314ef7e4') <> 0 then begin
    Result := 'WireGuard 安装文件校验失败，请重新下载安装包。';
    Exit;
  end;
  if not Exec(ExpandConstant('{sys}\msiexec.exe'),
    '/i "' + MsiPath + '" /qn /norestart DO_NOT_LAUNCH=1 /L*v "' +
    ExpandConstant('{tmp}\wireguard-install.log') + '"', '', SW_HIDE,
    ewWaitUntilTerminated, Code) then begin
    Result := '无法启动 WireGuard 安装程序。错误码：' + IntToStr(Code);
    Exit;
  end;
  if (Code <> 0) and (Code <> 3010) then begin
    Result := 'WireGuard 安装失败，错误码：' + IntToStr(Code) +
      '。日志：' + ExpandConstant('{tmp}\wireguard-install.log');
    Exit;
  end;
  DependencyRestart := Code = 3010;
  if not WireGuardInstalled then
    Result := '未找到安装后的 WireGuard。请安装官方 WireGuard 后重新运行本安装包。';
end;

function NeedRestart: Boolean;
begin
  Result := DependencyRestart;
end;

function CanLaunch: Boolean;
begin
  Result := not DependencyRestart;
end;

function InitializeUninstall: Boolean;
begin
  Result := not ApplicationInUse;
  if not Result then
    SuppressibleMsgBox('请先断开连接并正常退出安装目录中的 xxtab，再卸载。用户配置将保留。',
      mbError, MB_OK, IDOK);
end;
