[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PackageDirectory,
    [Parameter(Mandatory)][string]$TestDirectory,
    [Parameter(Mandatory)][string]$ExpectedVersion
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (Test-Path -LiteralPath $TestDirectory) { throw 'Use a new extraction directory.' }
$archiveName = "controlfreak-v$ExpectedVersion-x86_64-pc-windows-msvc.zip"
$installerName = "ControlFreak-$ExpectedVersion-Setup.exe"
foreach ($name in @($archiveName, $installerName)) {
    $path = Join-Path $PackageDirectory $name
    $expected = (Get-Content -LiteralPath "$path.sha256" -Raw).Trim()
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() + "  $name"
    if ($expected -cne $actual) { throw 'Release artifact checksum mismatch.' }
}
Expand-Archive -LiteralPath (Join-Path $PackageDirectory $archiveName) -DestinationPath $TestDirectory
foreach ($name in @('controlfreak.exe', 'controlfreak-installer.exe', 'README.md', 'CHANGELOG.md', 'SECURITY.md', 'LICENSE', 'Cargo.lock', 'version.txt')) {
    if (-not (Test-Path -LiteralPath (Join-Path $TestDirectory $name) -PathType Leaf)) { throw 'Required release payload file missing.' }
}
if ((Get-Content -LiteralPath (Join-Path $TestDirectory 'version.txt') -Raw).Trim() -cne $ExpectedVersion) { throw 'Package version mismatch.' }
& (Join-Path $PSScriptRoot 'verify-windows-artifact.ps1') -Executable (Join-Path $TestDirectory 'controlfreak.exe') -ExpectedVersion $ExpectedVersion
