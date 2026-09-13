[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PackageDirectory,
    [Parameter(Mandatory)][string]$TestDirectory,
    [switch]$ProductionInstaller
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$PackageDirectory = (Resolve-Path -LiteralPath $PackageDirectory).Path
$TestDirectory = [IO.Path]::GetFullPath($TestDirectory)
if (Test-Path -LiteralPath $TestDirectory) { throw 'Use a new test directory.' }
$installers = @(Get-ChildItem -LiteralPath $PackageDirectory -Filter '*-Setup.exe')
if ($installers.Count -ne 1) { throw 'Expected exactly one installer.' }
$installer = $installers[0]
$identity = 'ControlFreak.Installer.Test'
if ($ProductionInstaller) {
    if ($env:GITHUB_ACTIONS -ne 'true' -or $installer.VersionInfo.FileDescription.Trim() -ne 'ControlFreak setup') { throw 'Production installer tests require a disposable GitHub Actions runner.' }
    $identity = 'ControlFreak.Windows.x64'
} elseif ($installer.VersionInfo.FileDescription.Trim() -ne 'ControlFreak installer test') { throw 'Only a package built with -TestSetup may run this test.' }
$registryPath = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\${identity}_is1"
if (Test-Path -LiteralPath $registryPath) { throw 'A previous installer test is still registered; clean it up first.' }
New-Item -ItemType Directory -Path $TestDirectory | Out-Null
$installDir = Join-Path $TestDirectory 'Apps with spaces 日本語/ControlFreak'
$profile = Join-Path $TestDirectory 'synthetic-profile'
$variables = @('USERPROFILE', 'LOCALAPPDATA', 'APPDATA', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'PI_CODING_AGENT_DIR', 'OPENCODE_CONFIG', 'OPENCODE_CONFIG_DIR', 'XDG_CONFIG_HOME')
$previousEnvironment = @{}
foreach ($name in $variables) { $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name) }

function Run-Setup([string]$File, [string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $File
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        if (-not $process.WaitForExit(60000)) { $process.Kill($true); throw 'Installer test timed out.' }
        return $process.ExitCode
    } finally { $process.Dispose() }
}

function Permission-Fingerprint([string]$Path) {
    $acl = Get-Acl -LiteralPath $Path
    # Compare actual access rules and inheritance protection. Windows may clear
    # the historical AUTO_INHERITED bookkeeping bit when applying a protected DACL.
    $rules = @($acl.Access | ForEach-Object {
        "$($_.IdentityReference.Value)|$($_.FileSystemRights)|$($_.AccessControlType)|$($_.IsInherited)|$($_.InheritanceFlags)|$($_.PropagationFlags)"
    })
    return "$($acl.Owner)|$($acl.Group)|$($acl.AreAccessRulesProtected)|$($rules -join ';')"
}

try {
    foreach ($name in $variables) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
    $env:USERPROFILE = $profile
    $env:LOCALAPPDATA = Join-Path $profile 'local'
    $env:APPDATA = Join-Path $profile 'roaming'
    New-Item -ItemType Directory -Force -Path $profile | Out-Null
    $aclFixture = Join-Path $profile '.claude.json'
    Set-Content -LiteralPath $aclFixture -Value '{"preferences":{"synthetic":true}}'
    $acl = Get-Acl -LiteralPath $aclFixture
    $acl.SetAccessRuleProtection($true, $false)
    $rule = [Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.WindowsIdentity]::GetCurrent().User, 'FullControl', 'Allow')
    $acl.SetAccessRule($rule)
    Set-Acl -LiteralPath $aclFixture -AclObject $acl
    $expectedAcl = Permission-Fingerprint $aclFixture
    $parameters = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=$installDir", '/CLIENTS=codex,claude-code,claude-desktop,pi,opencode')
    $installCode = Run-Setup $installer.FullName ($parameters + @("/LOG=$TestDirectory/setup.log"))
    if ($installCode -ne 0) { throw "Fresh installation failed with exit code $installCode." }
    if (-not (Test-Path -LiteralPath $registryPath)) { throw 'Uninstall registration missing.' }
    if ((Permission-Fingerprint $aclFixture) -ne $expectedAcl) { throw 'Configuration file permissions changed.' }
    $backups = @(Get-ChildItem -LiteralPath $profile -Filter '.claude.json.controlfreak-backup-*.bak')
    if ($backups.Count -ne 1 -or (Permission-Fingerprint $backups[0].FullName) -ne $expectedAcl) { throw 'Configuration backup did not retain source permissions.' }
    $version = (Get-Content -LiteralPath (Join-Path $installDir 'version.txt') -Raw).Trim()
    $archive = @(Get-ChildItem -LiteralPath $PackageDirectory -Filter '*.zip')
    if ($archive.Count -ne 1) { throw 'Expected exactly one portable archive.' }
    $extracted = Join-Path $TestDirectory 'portable comparison'
    Expand-Archive -LiteralPath $archive[0].FullName -DestinationPath $extracted
    foreach ($binary in @('controlfreak.exe', 'controlfreak-installer.exe')) {
        if ((Get-FileHash -LiteralPath (Join-Path $installDir $binary)).Hash -ne (Get-FileHash -LiteralPath (Join-Path $extracted $binary)).Hash) { throw 'Installer and portable binaries differ.' }
    }
    & (Join-Path $PSScriptRoot 'verify-windows-artifact.ps1') -Executable (Join-Path $installDir 'controlfreak.exe') -ExpectedVersion $version
    $paths = @('.codex/config.toml', '.claude.json', 'roaming/Claude/claude_desktop_config.json', '.pi/agent/mcp.json', '.config/opencode/opencode.json')
    foreach ($path in $paths) {
        if ((Get-Content -LiteralPath (Join-Path $profile $path) -Raw) -notmatch 'controlfreak') { throw 'Selected client configuration missing.' }
    }
    if ((Run-Setup $installer.FullName $parameters) -ne 0) { throw 'Reinstallation failed.' }
    # Exercise upgrade semantics using an older registered version; binary identity
    # is independently checked by the artifact harness, not faked here.
    Set-ItemProperty -LiteralPath $registryPath -Name DisplayVersion -Value '0.0.1'
    if ((Run-Setup $installer.FullName $parameters) -ne 0) { throw 'Upgrade failed.' }
    Set-ItemProperty -LiteralPath $registryPath -Name DisplayVersion -Value '999.0.0'
    if ((Run-Setup $installer.FullName $parameters) -eq 0) { throw 'Downgrade was not refused.' }
    Set-ItemProperty -LiteralPath $registryPath -Name DisplayVersion -Value $version
    $lockedFile = [IO.File]::Open((Join-Path $installDir 'controlfreak.exe'), 'Open', 'Read', 'None')
    try {
        if ((Run-Setup $installer.FullName $parameters) -eq 0) { throw 'Setup replaced an in-use executable.' }
    } finally { $lockedFile.Dispose() }
    # A malformed client fails independently after installation, producing exit 10.
    Set-Content -LiteralPath (Join-Path $profile '.claude.json') -Value 'synthetic invalid JSON'
    if ((Run-Setup $installer.FullName $parameters) -ne 10) { throw 'Partial configuration failure did not produce exit 10.' }
    Set-Content -LiteralPath (Join-Path $profile '.claude.json') -Value '{}'
    Set-Content -LiteralPath (Join-Path $installDir 'unrelated-fixture.txt') -Value 'retain this unrelated file'
    $uninstaller = Join-Path $installDir 'unins000.exe'
    if ((Run-Setup $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/REMOVECONFIG=1')) -ne 0) { throw 'Uninstall failed.' }
    if (Test-Path -LiteralPath (Join-Path $installDir 'controlfreak.exe')) { throw 'Installed executable remains after uninstall.' }
    if (Test-Path -LiteralPath $registryPath) { throw 'Uninstall registration remains.' }
    if (-not (Test-Path -LiteralPath (Join-Path $installDir 'unrelated-fixture.txt'))) { throw 'Uninstall deleted an unrelated file.' }
    foreach ($path in $paths) {
        if ((Get-Content -LiteralPath (Join-Path $profile $path) -Raw) -match 'controlfreak') { throw 'Installer-created configuration remains.' }
    }
    Write-Output 'Installer: fresh install, all client configurations, reinstall, upgrade, downgrade refusal, locked files, partial failure and uninstall passed.'
} finally {
    $results = Join-Path $installDir 'setup-results.txt'
    if (Test-Path -LiteralPath $results) { Copy-Item -LiteralPath $results -Destination (Join-Path $TestDirectory 'setup-results.txt') }
    # Only this test identity is eligible for cleanup. Never touch a production install.
    if (Test-Path -LiteralPath $registryPath) {
        $uninstaller = Join-Path $installDir 'unins000.exe'
        if (Test-Path -LiteralPath $uninstaller) {
            $null = Run-Setup $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
        }
    }
    foreach ($name in $variables) {
        if ($null -eq $previousEnvironment[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name]) }
    }
}
