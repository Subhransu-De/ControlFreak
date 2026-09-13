[CmdletBinding()]
param([Parameter(Mandatory)][string]$Destination)
$ErrorActionPreference = 'Stop'
$Destination = [IO.Path]::GetFullPath($Destination)
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
$setup = Join-Path $Destination 'innosetup-6.7.1.exe'
Invoke-WebRequest 'https://github.com/jrsoftware/issrc/releases/download/is-6_7_1/innosetup-6.7.1.exe' -OutFile $setup
if ((Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash -ne '4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0') {
    throw 'Inno Setup compiler checksum mismatch.'
}
$compiler = Start-Process -FilePath $setup -ArgumentList @('/PORTABLE=1', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER', "/DIR=`"$Destination/compiler`"") -PassThru -WindowStyle Hidden
try {
    if (-not $compiler.WaitForExit(120000)) { $compiler.Kill(); throw 'Compiler extraction timed out.' }
    if ($compiler.ExitCode -ne 0) { throw 'Compiler extraction failed.' }
} finally { $compiler.Dispose() }
Write-Output (Join-Path $Destination 'compiler/ISCC.exe')
