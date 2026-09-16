[CmdletBinding()]
param(
    [string]$Iscc,
    [switch]$SkipBuild,
    [switch]$TestPackage
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $Iscc) {
    $candidates = @(
        (Join-Path $repoRoot '.tools/inno/ISCC.exe'),
        "${env:ProgramFiles(x86)}/Inno Setup 6/ISCC.exe",
        "$env:ProgramFiles/Inno Setup 6/ISCC.exe"
    )
    $Iscc = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $Iscc) {
        $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
        if ($command) { $Iscc = $command.Source }
    }
}
if (-not $Iscc -or -not (Test-Path -LiteralPath $Iscc)) {
    throw 'Install Inno Setup 6.5+ (https://jrsoftware.org/isdl.php), or pass -Iscc C:\path\ISCC.exe.'
}
$toolDir = Join-Path $repoRoot '.tools'
New-Item -ItemType Directory -Force $toolDir | Out-Null
$msi = Join-Path $toolDir 'wireguard-amd64-0.5.3.msi'
if (-not (Test-Path -LiteralPath $msi)) {
    Invoke-WebRequest 'https://download.wireguard.com/windows-client/wireguard-amd64-0.5.3.msi' -OutFile $msi
}
if ((Get-FileHash -LiteralPath $msi -Algorithm SHA256).Hash -ne '76fcec042c5989c5b816cd32eaed1e5b1c3b998a4b1c9eca55f299e3314ef7e4') {
    throw 'WireGuard MSI SHA256 mismatch. The downloaded file will not be packaged.'
}
$signature = Get-AuthenticodeSignature -LiteralPath $msi
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=WireGuard LLC') {
    throw 'WireGuard MSI must have a valid signature from WireGuard LLC.'
}
Push-Location $repoRoot
try {
    if (-not $SkipBuild) {
        # Explicit target keeps these flags off host proc-macros. Static CRT
        # avoids depending on a separately installed Visual C++ runtime.
        $previousRustFlags = $env:RUSTFLAGS
        try {
            $env:RUSTFLAGS = "$previousRustFlags -C target-feature=+crt-static".Trim()
            & cargo build --release --locked --features windows-gui --target x86_64-pc-windows-msvc --target-dir target/installer
            if ($LASTEXITCODE -ne 0) { throw 'Installer release build failed.' }
        } finally { $env:RUSTFLAGS = $previousRustFlags }
    }
    foreach ($name in 'xxtab.exe','xxtab-gui.exe') {
        $binary = Join-Path $repoRoot "target/installer/x86_64-pc-windows-msvc/release/$name"
        $contents = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($binary))
        if ($contents -match '(?i)(VCRUNTIME\d+|MSVCP\d+)\.dll') {
            throw "Installer binary still depends on Visual C++ runtime: $name"
        }
    }
    $metadata = (& cargo metadata --no-deps --format-version 1 | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Cargo metadata.' }
    $version = ($metadata.packages | Where-Object name -eq 'xxtab').version
    if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Installer expects a numeric major.minor.patch version.' }
    $argsList = @("/DAppVersion=$version")
    if ($TestPackage) {
        $argsList += '/DInstallerTest'
        $argsList += "/O$(Join-Path $toolDir 'installer-test')"
    }
    $argsList += (Join-Path $repoRoot 'packaging/windows/xxtab.iss')
    & $Iscc @argsList
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed.' }
    $installer = if ($TestPackage) {
        Join-Path $toolDir 'installer-test/xxtab-setup-test.exe'
    } else {
        Join-Path $repoRoot "dist/installers/xxtab-$version-windows-x64-setup.exe"
    }
    $hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $(Split-Path -Leaf $installer)" | Set-Content -LiteralPath "$installer.sha256" -Encoding ascii
    Get-Item -LiteralPath $installer | Select-Object FullName, Length
} finally {
    Pop-Location
}
