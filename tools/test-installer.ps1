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
$dependencyRoot = 'HKLM:\SOFTWARE\xxtab Packaging Test'
if ((Test-Path -LiteralPath $dependencyRoot) -and
    (@(Get-ChildItem -LiteralPath $dependencyRoot).Count -ne 0 -or
     (Get-Item -LiteralPath $dependencyRoot).ValueCount -ne 0)) {
    throw 'An earlier dependency test registry key exists; refusing to overwrite it.'
}
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

# Exercise dependency ownership with the test installer's compile-time MSI stub.
# This never installs/uninstalls WireGuard or changes real tunnel services.
$dependencyKey = "$dependencyRoot\Dependencies\WireGuard"
$servicesKey = "$dependencyRoot\TestServices"
$productCode = '{2FDB79CE-5193-4A39-82BB-E00158CC1533}'
$receipt = "$testDir.dependency-uninstalled.txt"
foreach ($scenario in 'owned', 'changed-hash', 'foreign-product', 'other-tunnel', 'query-failed', 'msi-failed', 'reboot') {
    try {
        if ((Run-Setup) -ne 0) { throw "Installation failed for dependency test: $scenario" }
        New-Item -Path $dependencyKey -Force | Out-Null
        New-Item -Path $servicesKey -Force | Out-Null
        New-ItemProperty -LiteralPath $dependencyKey -Name ProductCode -Value $productCode -PropertyType String -Force | Out-Null
        New-ItemProperty -LiteralPath $dependencyKey -Name ExecutableSHA256 -Value $wgHash -PropertyType String -Force | Out-Null
        switch ($scenario) {
            'changed-hash' { Set-ItemProperty -LiteralPath $dependencyKey -Name ExecutableSHA256 -Value ('0' * 64) }
            'foreign-product' { Set-ItemProperty -LiteralPath $dependencyKey -Name ProductCode -Value '{00000000-0000-0000-0000-000000000000}' }
            'other-tunnel' { New-Item -Path ($servicesKey + '\WireGuardTunnel$another_client') -Force | Out-Null }
            'query-failed' { Remove-Item -LiteralPath $servicesKey }
        }
        if ($scenario -eq 'owned') {
            if ((Run-Setup) -ne 0) { throw 'Upgrade failed in dependency ownership test.' }
            if ((Get-ItemProperty -LiteralPath $dependencyKey).ExecutableSHA256 -ne $wgHash) {
                throw 'Upgrade lost dependency ownership.'
            }
        }
        $msiCode = switch ($scenario) { 'msi-failed' { 1603 }; 'reboot' { 3010 }; default { 0 } }
        $uninstallLog = Join-Path $repoRoot ".tools/installer-dependency-$scenario.log"
        $process = Start-Process -FilePath $uninstaller -ArgumentList "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /TESTMSICODE=$msiCode /LOG=`"$uninstallLog`"" -WindowStyle Hidden -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw "Application uninstall failed in scenario $scenario" }
        $expectedAttempt = $scenario -in @('owned', 'msi-failed', 'reboot')
        if ((Test-Path -LiteralPath $receipt) -ne $expectedAttempt) {
            throw "Wrong dependency uninstall decision: $scenario"
        }
        if ((Test-Path -LiteralPath $dependencyKey) -ne ($scenario -eq 'msi-failed')) {
            throw "Wrong ownership cleanup decision: $scenario"
        }
        if ($scenario -eq 'reboot' -and
            -not (Select-String -LiteralPath $uninstallLog -SimpleMatch 'Need to restart Windows? Yes' -Quiet)) {
            throw 'Dependency restart requirement was not propagated to the uninstaller.'
        }
        if ((Get-FileHash -LiteralPath $wireguard).Hash -ne $wgHash) {
            throw 'Synthetic dependency tests modified system WireGuard.'
        }
        Write-Output "PASS: dependency uninstall $scenario"
    } finally {
        if (Test-Path -LiteralPath $uninstaller) {
            Start-Process -FilePath $uninstaller -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART' -WindowStyle Hidden -Wait | Out-Null
        }
        if (Test-Path -LiteralPath $dependencyRoot) {
            $resolvedKey = (Get-Item -LiteralPath $dependencyRoot).Name
            if ($resolvedKey -ne 'HKEY_LOCAL_MACHINE\SOFTWARE\xxtab Packaging Test') {
                throw "Unexpected registry test cleanup target: $resolvedKey"
            }
            # Only remove the exact fixture keys, from leaves to root.
            foreach ($key in @(($servicesKey + '\WireGuardTunnel$another_client'), $dependencyKey, $servicesKey, "$dependencyRoot\Dependencies")) {
                if (Test-Path -LiteralPath $key) {
                    if (@(Get-ChildItem -LiteralPath $key).Count -ne 0) { throw "Unexpected nested test key: $key" }
                    Remove-Item -LiteralPath $key
                }
            }
            # Keep the empty test namespace; no forced registry cleanup is needed.
        }
        if (Test-Path -LiteralPath $receipt) { Remove-Item -LiteralPath $receipt }
        if ((Test-Path -LiteralPath $testDir) -and -not (Get-ChildItem -LiteralPath $testDir -Force)) {
            Remove-Item -LiteralPath $testDir
        }
    }
}
