#ifndef AppVersion
  #define AppVersion "0.1.3"
#endif
#define RepoRoot SourcePath + "..\..\"
#define BinaryDir RepoRoot + "target\installer\x86_64-pc-windows-msvc\release\"
#define WireGuardProductCode "{2FDB79CE-5193-4A39-82BB-E00158CC1533}"
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
Source: "{#RepoRoot}LICENSE"; DestDir: "{app}"; Flags: ignoreversion
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
  DependencyUninstallRestart: Boolean;

const
  DependencyKey = 'Software\{#ProductName}\Dependencies\WireGuard';
  BundledProductCode = '{#WireGuardProductCode}';

function MsiQueryProductState(ProductCode: string): Integer;
  external 'MsiQueryProductStateW@msi.dll stdcall';

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
  Result := FileExists(ExpandConstant('{commonpf64}\WireGuard\wireguard.exe')) or
    (MsiQueryProductState(BundledProductCode) = 5);
end;

function RecordWireGuardOwnership: Boolean;
begin
  { This is only called after we installed a previously absent dependency.
    Existing records survive upgrades; an existing dependency is never adopted. }
  Result := RegWriteStringValue(HKLM64, DependencyKey, 'ExecutableSHA256',
    GetSHA256OfFile(ExpandConstant('{commonpf64}\WireGuard\wireguard.exe')));
  if Result then
    Result := RegWriteStringValue(HKLM64, DependencyKey, 'ProductCode', BundledProductCode);
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
#ifdef InstallerTest
  { A test installer must never install or repair the real system dependency. }
  Result := 'Packaging test requires an existing WireGuard installation.';
  Exit;
#endif
  { Remove obsolete ownership before installing a new, known MSI. }
  RegDeleteKeyIncludingSubkeys(HKLM64, DependencyKey);
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
  if not FileExists(ExpandConstant('{commonpf64}\WireGuard\wireguard.exe')) or
    (MsiQueryProductState(BundledProductCode) <> 5) then begin
    Result := '未找到安装后的 WireGuard。请安装官方 WireGuard 后重新运行本安装包。';
    Exit;
  end;
  if not RecordWireGuardOwnership then
    Result := 'WireGuard 已安装，但无法记录依赖来源。请检查注册表写入权限后重试。';
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

function WireGuardPreserveReason: string;
var
  ProductCode, ExpectedHash, Executable, ServicesKey: string;
  Services: TArrayOfString;
  Index: Integer;
#ifndef InstallerTest
  Profiles: TFindRec;
#endif
begin
  Result := '';
  if not RegQueryStringValue(HKLM64, DependencyKey, 'ProductCode', ProductCode) or
    (ProductCode <> BundledProductCode) then begin
    Result := '没有由 xxtab 安装 WireGuard 的来源记录';
    Exit;
  end;
  if MsiQueryProductState(ProductCode) <> 5 then begin
    Result := '原附带的 WireGuard MSI 已被移除或更换版本';
    Exit;
  end;
  Executable := ExpandConstant('{commonpf64}\WireGuard\wireguard.exe');
  if not FileExists(Executable) then begin
    Result := 'WireGuard 程序不存在';
    Exit;
  end;
  if not RegQueryStringValue(HKLM64, DependencyKey, 'ExecutableSHA256', ExpectedHash) or
    (CompareText(ExpectedHash, GetSHA256OfFile(Executable)) <> 0) then begin
    Result := 'WireGuard 程序已更换或来源记录不完整';
    Exit;
  end;
#ifdef InstallerTest
  { Simulated service inventory: never stop or uninstall real test-host tunnels. }
  ServicesKey := 'Software\{#ProductName}\TestServices';
#else
  ServicesKey := 'SYSTEM\CurrentControlSet\Services';
#endif
  if not RegGetSubkeyNames(HKLM64, ServicesKey, Services) then begin
    Result := '无法检查 WireGuard 隧道服务';
    Exit;
  end;
  for Index := 0 to GetArrayLength(Services) - 1 do begin
    if CompareText(Copy(Services[Index], 1, Length('WireGuardTunnel$')), 'WireGuardTunnel$') = 0 then begin
      Result := '仍存在 WireGuard 隧道服务，请先断开并移除不再使用的隧道';
      Exit;
    end;
  end;
#ifndef InstallerTest
  if FindFirst(ExpandConstant('{commonpf64}\WireGuard\Data\Configurations\*.conf.dpapi'), Profiles) then begin
    FindClose(Profiles);
    Result := 'WireGuard 中仍保存了独立的隧道配置';
  end;
#endif
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
  Reason, LogPath: string;
begin
  { usUninstall runs after confirmation, before Inno calculates restart status. }
  if CurUninstallStep <> usUninstall then Exit;
  Reason := WireGuardPreserveReason;
  if Reason <> '' then begin
    Log('Preserving WireGuard: ' + Reason);
    if RegKeyExists(HKLM64, DependencyKey) then
      SuppressibleMsgBox('WireGuard 将保留：' + Reason + '。xxtab 将继续卸载。', mbInformation, MB_OK, IDOK);
    RegDeleteKeyIncludingSubkeys(HKLM64, DependencyKey);
    Exit;
  end;
  LogPath := ExpandConstant('{localappdata}\{#ProductName}\wireguard-uninstall.log');
#ifdef InstallerTest
  { Exercise the decision without ever invoking msiexec /x on the test host. }
  Log('TEST: would uninstall owned WireGuard MSI ' + BundledProductCode);
  SaveStringToFile(ExpandConstant('{app}.dependency-uninstalled.txt'), BundledProductCode, False);
  Code := StrToIntDef(ExpandConstant('{param:TESTMSICODE|0}'), 0);
#else
  ForceDirectories(ExtractFileDir(LogPath));
  if not Exec(ExpandConstant('{sys}\msiexec.exe'),
    '/x ' + BundledProductCode + ' /qn /norestart /L*v "' + LogPath + '"',
    '', SW_HIDE, ewWaitUntilTerminated, Code) then begin
    SuppressibleMsgBox('无法启动 WireGuard 卸载程序，xxtab 将继续卸载。请在 Windows“已安装的应用”中卸载 WireGuard。', mbError, MB_OK, IDOK);
    Exit;
  end;
#endif
  if (Code = 0) or (Code = 3010) or (Code = 1605) then begin
    DependencyUninstallRestart := Code = 3010;
    RegDeleteKeyIncludingSubkeys(HKLM64, DependencyKey);
    Log('Bundled WireGuard dependency uninstalled.');
  end else begin
    Log('WireGuard uninstall failed with code ' + IntToStr(Code));
    SuppressibleMsgBox('WireGuard 卸载失败，错误码：' + IntToStr(Code) +
      '。xxtab 将继续卸载，请在 Windows“已安装的应用”中卸载 WireGuard。日志：' + LogPath, mbError, MB_OK, IDOK);
  end;
end;

function UninstallNeedRestart: Boolean;
begin
  Result := DependencyUninstallRestart;
end;
