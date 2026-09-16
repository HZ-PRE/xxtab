# Isolated install/upgrade/uninstall smoke test. Does not launch the GUI or
# install/repair WireGuard. Test AppId and shortcut names differ from production.
[CmdletBinding()]
param([string]$Iscc)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this packaging smoke test from an administrator PowerShell.'
}
$wireguard = "$env:ProgramFiles/WireGuard/wireguard.exe"
if (-not (Test-Path -LiteralPath $wireguard)) {
    throw 'This smoke test requires existing WireGuard so it does not modify system dependencies.'
}
$buildArgs = @{ SkipBuild = $true; TestPackage = $true }
if ($Iscc) { $buildArgs.Iscc = $Iscc }
& "$PSScriptRoot/build-installer.ps1" @buildArgs
$setup = Join-Path $repoRoot '.tools/installer-test/xxtab-setup-test.exe'
$testDir = Join-Path $repoRoot ('.tools/installer-smoke-' + [guid]::NewGuid().ToString('N'))
$menu = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'xxtab Packaging Test'
$desktopLink = Join-Path ([Environment]::GetFolderPath('CommonDesktopDirectory')) 'xxtab Packaging Test.lnk'
$regPath = 'HKLM:/SOFTWARE/Microsoft/Windows/CurrentVersion/Uninstall/xxtab-packaging-test-7983c12d_is1'
if ((Test-Path -LiteralPath $menu) -or (Test-Path -LiteralPath $desktopLink) -or (Test-Path -LiteralPath $regPath)) {
    throw 'An earlier packaging test installation exists; refusing to overwrite it.'
}
$wgHash = (Get-FileHash -LiteralPath $wireguard).Hash
$uninstaller = Join-Path $testDir 'unins000.exe'
$log = Join-Path $repoRoot '.tools/installer-smoke.log'
$arguments = "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /TASKS=desktopicon /DIR=`"$testDir`" /LOG=`"$log`""
function Run-Setup {
    $process = Start-Process -FilePath $setup -ArgumentList $arguments -WindowStyle Hidden -Wait -PassThru
    return $process.ExitCode
}
try {
    if ((Run-Setup) -ne 0) { throw "Installation failed; see $log" }
    foreach ($excluded in 'docs','examples','tools','README.md') {
        if (Test-Path -LiteralPath (Join-Path $testDir $excluded)) {
            throw "Non-runtime content was installed: $excluded"
        }
    }
    foreach ($name in 'xxtab.exe','xxtab-gui.exe') {
        $installed = (Get-FileHash -LiteralPath (Join-Path $testDir $name)).Hash
        $built = (Get-FileHash -LiteralPath (Join-Path $repoRoot "target/installer/x86_64-pc-windows-msvc/release/$name")).Hash
        if ($installed -ne $built) { throw "Installed binary mismatch: $name" }
    }
    $smokeConfig = Join-Path $testDir 'smoke-config.toml'
    Set-Content -LiteralPath $smokeConfig -Value "server = 'wss://example.com'"
    & (Join-Path $testDir 'xxtab.exe') check $smokeConfig
    if ($LASTEXITCODE -ne 0) { throw 'Installed command-line binary cannot run.' }
    Remove-Item -LiteralPath $smokeConfig
    if (-not (Test-Path -LiteralPath $desktopLink) -or -not (Test-Path -LiteralPath "$menu/xxtab Packaging Test.lnk")) {
        throw 'Missing desktop/start menu shortcuts.'
    }
    if ((Get-ItemProperty -LiteralPath $regPath).DisplayName -ne 'xxtab Packaging Test version 0.1.0' -and
        (Get-ItemProperty -LiteralPath $regPath).DisplayName -notlike 'xxtab Packaging Test*') {
        throw 'Missing Add/Remove Programs registration.'
    }
    if (-not (Select-String -LiteralPath $log -SimpleMatch 'WireGuard already installed; preserving existing installation.' -Quiet)) {
        throw 'Existing WireGuard was not detected.'
    }
    $marker = Join-Path $testDir 'user-note.txt'
    Set-Content -LiteralPath $marker -Value 'preserve user-created files'
    $markerHash = (Get-FileHash -LiteralPath $marker).Hash
    $locked = [IO.File]::Open((Join-Path $testDir 'xxtab-gui.exe'), 'Open', 'Read', 'Read')
    try {
        if ((Run-Setup) -eq 0) { throw 'Upgrade must reject an in-use executable.' }
    } finally { $locked.Dispose() }
    if ((Run-Setup) -ne 0) { throw 'Upgrade failed after releasing the executable.' }
    if ((Get-FileHash -LiteralPath $marker).Hash -ne $markerHash) { throw 'Upgrade overwrote a user file.' }
    $process = Start-Process -FilePath $uninstaller -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART' -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw 'Uninstall failed.' }
    if ((Test-Path -LiteralPath "$testDir/xxtab-gui.exe") -or (Test-Path -LiteralPath $regPath) -or (Test-Path -LiteralPath $desktopLink) -or (Test-Path -LiteralPath $menu)) {
        throw 'Uninstall left application binaries, registration or shortcuts.'
    }
    if (-not (Test-Path -LiteralPath $marker)) { throw 'Uninstall removed an untracked user file.' }
    if ((Get-FileHash -LiteralPath $wireguard).Hash -ne $wgHash) { throw 'WireGuard executable was modified.' }
    Write-Output 'PASS: runtime-only package, install, binary hashes, shortcuts, registration, dependency detection, locked-file protection, upgrade, uninstall and user-file preservation.'
} finally {
    # The test only removes its own installation, using its own uninstaller.
    if (Test-Path -LiteralPath $uninstaller) {
        Start-Process -FilePath $uninstaller -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART' -WindowStyle Hidden -Wait | Out-Null
    }
    $marker = Join-Path $testDir 'user-note.txt'
    if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker }
    # Remove only an empty, explicitly created test directory (no recursion).
    if ((Test-Path -LiteralPath $testDir) -and -not (Get-ChildItem -LiteralPath $testDir -Force)) {
        Remove-Item -LiteralPath $testDir
    }
}
