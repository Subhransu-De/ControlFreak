[CmdletBinding()]
param(
    [string]$Compiler,
    [string]$OutputDirectory = 'target/windows-package',
    [string]$Tag,
    [switch]$ValidateOnly,
    [switch]$RequireSbom,
    [switch]$TestSetup
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    $metadataText = & cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read Cargo metadata.' }
    $metadata = $metadataText | ConvertFrom-Json
    $versions = @($metadata.packages.version | Sort-Object -Unique)
    if ($versions.Count -ne 1) { throw 'All workspace crates must have the same version.' }
    $version = $versions[0]
    if ($Tag -and $Tag -cne "v$version") { throw 'Release tag does not match the workspace version.' }
    if ($ValidateOnly) { Write-Output $version; return }
    if (-not $Compiler) { throw 'Pass -Compiler with the pinned Inno Setup 6.7.1 ISCC.exe path.' }
    $Compiler = (Resolve-Path -LiteralPath $Compiler).Path
    $out = [IO.Path]::GetFullPath($OutputDirectory)
    if (Test-Path -LiteralPath $out) { throw 'Use a new output directory; stale artifacts must never enter a release.' }
    New-Item -ItemType Directory -Path $out | Out-Null
    $packageName = "controlfreak-v$version-x86_64-pc-windows-msvc"
    $payload = Join-Path $out $packageName
    New-Item -ItemType Directory -Path (Join-Path $payload 'sbom') -Force | Out-Null
    foreach ($name in @('controlfreak.exe', 'controlfreak-installer.exe')) {
        Copy-Item -LiteralPath (Join-Path $metadata.target_directory "release/$name") -Destination $payload
    }
    foreach ($name in @('README.md', 'CHANGELOG.md', 'SECURITY.md', 'LICENSE', 'Cargo.lock')) {
        Copy-Item -LiteralPath (Join-Path $root $name) -Destination $payload
    }
    Set-Content -LiteralPath (Join-Path $payload 'version.txt') -Value $version -Encoding utf8NoBOM
    $sbomCount = 0
    if (Test-Path -LiteralPath 'controlfreak.cdx.json') {
        Copy-Item -LiteralPath 'controlfreak.cdx.json' -Destination (Join-Path $payload 'sbom/workspace.cdx.json')
        $sbomCount++
    }
    foreach ($package in $metadata.packages) {
        $sbom = Join-Path (Split-Path $package.manifest_path -Parent) 'controlfreak.cdx.json'
        if (Test-Path -LiteralPath $sbom) {
            Copy-Item -LiteralPath $sbom -Destination (Join-Path $payload "sbom/$($package.name).cdx.json")
            $sbomCount++
        } elseif ($RequireSbom) { throw "Missing crate SBOM: $($package.name)" }
    }
    if ($RequireSbom -and $sbomCount -eq 0) { throw 'Release SBOMs are missing.' }
    # Dev builds retain the folder without representing this file as an SBOM.
    if ($sbomCount -eq 0) { Set-Content (Join-Path $payload 'sbom/NOT-GENERATED.txt') 'Development package: SBOM generation was not requested.' }
    $numeric = ($version -split '[-+]')[0] + '.0'
    $arguments = @('/Qp', "/DVersion=$version", "/DNumericVersion=$numeric", "/DPayloadDir=$payload", "/DOutputPath=$out")
    if ($TestSetup) { $arguments += '/DTestSetup=1' }
    & $Compiler @arguments (Join-Path $root 'packaging/windows/controlfreak.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed.' }
    $archive = Join-Path $out "$packageName.zip"
    Compress-Archive -Path (Join-Path $payload '*') -DestinationPath $archive
    foreach ($artifact in @($archive, (Join-Path $out "ControlFreak-$version-Setup.exe"))) {
        $hash = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $([IO.Path]::GetFileName($artifact))" | Set-Content -LiteralPath "$artifact.sha256" -Encoding utf8NoBOM
    }
    Write-Output $out
} finally { Pop-Location }
