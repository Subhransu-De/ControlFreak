[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$ExpectedVersion
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Executable = (Resolve-Path -LiteralPath $Executable).Path

function Invoke-Server([string[]]$Arguments, [string]$InputText = '') {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Executable
    $info.WorkingDirectory = Split-Path $Executable -Parent
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.Environment['PATH'] = ''
    $info.Environment.Remove('CONTROLFREAK_GLOW_ERROR_LOG') | Out-Null
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    try {
        if (-not $process.Start()) { throw 'Packaged executable did not start.' }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write($InputText)
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(20000)) { $process.Kill($true); throw 'Packaged executable exceeded the 20 second deadline.' }
        if (-not $stdout.Wait(5000) -or -not $stderr.Wait(5000)) { throw 'Packaged executable did not close its streams.' }
        # Never print the captured protocol, diagnostics, or machine information.
        return @{ Code = $process.ExitCode; Out = $stdout.Result; Err = $stderr.Result }
    } finally {
        if ($process.Id -and -not $process.HasExited) { $process.Kill($true) }
        $process.Dispose()
    }
}

$version = Invoke-Server @('--version')
if ($version.Code -ne 0 -or $version.Out.Trim() -cne "controlfreak $ExpectedVersion") { throw 'Packaged executable version mismatch.' }
$capabilities = Invoke-Server @('--print-capabilities')
if ($capabilities.Code -ne 0) { throw 'Packaged capabilities command failed.' }
$report = $capabilities.Out | ConvertFrom-Json
$arguments = @()
if ($report.security_context.elevated) {
    $refusal = Invoke-Server @()
    if ($refusal.Code -eq 0 -or $refusal.Out -ne '') { throw 'Elevated startup was not refused cleanly.' }
    $failure = $refusal.Err | ConvertFrom-Json
    if ($failure.error.code -ne 'elevated_operation_requires_opt_in') { throw 'Wrong elevated startup error.' }
    # This harness only initializes, lists tool schemas and reads server status.
    $arguments = @('--allow-elevated')
}
$requests = @'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"synthetic-package-check","version":"1.0.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_server_status","arguments":{}}}
'@
$output = Invoke-Server $arguments ($requests + "`n")
if ($output.Code -ne 0) { throw 'Packaged MCP startup or disconnect failed.' }
$responses = @($output.Out -split '\r?\n' | Where-Object { $_ -ne '' } | ForEach-Object { ConvertFrom-Json $_ })
foreach ($response in $responses) { if ($response.jsonrpc -ne '2.0') { throw 'Non-protocol stdout.' } }
$initialize = @($responses | Where-Object { $_.PSObject.Properties['id'] -and $_.id -eq 1 })
$tools = @($responses | Where-Object { $_.PSObject.Properties['id'] -and $_.id -eq 2 })
$status = @($responses | Where-Object { $_.PSObject.Properties['id'] -and $_.id -eq 3 })
if ($initialize.Count -ne 1 -or $initialize[0].result.serverInfo.version -cne $ExpectedVersion) { throw 'MCP build identity mismatch.' }
if ($initialize[0].result.serverInfo.name -ne 'controlfreak') { throw 'Wrong MCP server identity.' }
if ($tools.Count -ne 1 -or $tools[0].result.tools.Count -lt 1 -or 'get_server_status' -notin $tools[0].result.tools.name) { throw 'MCP tool discovery failed.' }
if ($status.Count -ne 1 -or $status[0].result.isError -or $status[0].result.structuredContent.status -ne 'ready') { throw 'Packaged server did not report ready.' }
Write-Output 'Packaged executable: version, capability reporting, MCP initialize/tools/status and disconnect passed.'
