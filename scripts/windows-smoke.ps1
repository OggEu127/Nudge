# Run in an interactive Windows desktop session. No Microsoft login is invoked.
[CmdletBinding()]
param(
    [string]$BinaryDirectory = (Join-Path $PSScriptRoot '..'),
    [string]$OutputDirectory = (Join-Path $env:TEMP ('Nudge smoke ' + [guid]::NewGuid().ToString('N'))),
    [string]$JavaExecutable,
    [uint32]$JavaMajor = 21
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') { throw 'Run this script on Windows.' }
$cli = (Resolve-Path -LiteralPath (Join-Path $BinaryDirectory 'nudge.exe')).Path
$gui = (Resolve-Path -LiteralPath (Join-Path $BinaryDirectory 'nudge-gui.exe')).Path
if (Test-Path -LiteralPath $OutputDirectory) { throw 'OutputDirectory must not already exist.' }
$null = New-Item -ItemType Directory -Path $OutputDirectory
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
# Include Unicode and spaces even when TEMP and the selected output use ASCII.
$probeName = 'Nudge ' + [char]0x6D4B + [char]0x8BD5
$probeRoot = Join-Path $OutputDirectory $probeName
$config = Join-Path $probeRoot 'config.json'
$data = Join-Path $probeRoot 'data'
$checks = [System.Collections.Generic.List[object]]::new()
$report = [ordered]@{
    startedUtc = [DateTime]::UtcNow.ToString('o')
    os = [Environment]::OSVersion.VersionString
    cliSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $cli).Hash
    guiSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $gui).Hash
    checks = $checks
    manualAcceptance = 'PENDING: visuals, DPI, drag/drop, vault, Java game launch and exit'
}

function ConvertTo-NativeArgument([string]$Value) {
    # Windows command-line quoting, including backslashes before quotes/end.
    '"' + ($Value -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}

function Invoke-CliCheck([string]$Name, [string[]]$CliArguments) {
    $allArguments = @('--config', $config, '--data-dir', $data) + $CliArguments
    $commandLine = ($allArguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join ' '
    # Separate redirection avoids Windows PowerShell 5 treating ordinary native
    # stderr logs as terminating PowerShell errors under ErrorActionPreference.
    $process = Start-Process -FilePath $cli -ArgumentList $commandLine -PassThru `
        -RedirectStandardOutput (Join-Path $OutputDirectory ($Name + '.stdout.log')) `
        -RedirectStandardError (Join-Path $OutputDirectory ($Name + '.stderr.log'))
    try {
        if (-not $process.WaitForExit(30000)) { throw "CLI check $Name timed out." }
        $process.Refresh()
        $code = $process.ExitCode
        $checks.Add([ordered]@{ name = $Name; exitCode = $code })
        if ($code -ne 0) { throw "CLI check $Name failed with exit code $code." }
    } finally {
        if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
}

try {
    Invoke-CliCheck 'version' @('--version')
    Invoke-CliCheck 'config-init' @('config', 'init')
    Invoke-CliCheck 'config-show' @('config', 'show')
    $parsed = Get-Content -LiteralPath $config -Raw | ConvertFrom-Json
    if ($parsed.schema_version -ne 1) { throw 'Unexpected configuration schema.' }
    Invoke-CliCheck 'instances' @('instances')
    Invoke-CliCheck 'accounts-empty' @('account', 'list')
    if ($JavaExecutable) {
        Invoke-CliCheck 'java-inspection' @('java', '--major', "$JavaMajor", '--executable', $JavaExecutable)
    } else {
        $checks.Add([ordered]@{ name = 'java-inspection'; status = 'SKIPPED: no JavaExecutable supplied' })
    }
    # Start-Process joins arguments into a command line. Quote each known path
    # explicitly so Unicode/spaces reach clap unchanged; no shell is involved.
    $arguments = '--config "{0}" --data-dir "{1}" --smoke-test-seconds 3' -f $config, $data
    $process = Start-Process -FilePath $gui -ArgumentList $arguments -PassThru
    try {
        if (-not $process.WaitForExit(30000)) { throw 'GUI did not exit within 30 seconds.' }
        $process.Refresh()
        $checks.Add([ordered]@{ name = 'gui-open-close'; exitCode = $process.ExitCode })
        if ($process.ExitCode -ne 0) { throw "GUI failed with exit code $($process.ExitCode)." }
    } finally {
        if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
    $report['automatedSmoke'] = 'PASSED'
} catch {
    $report['automatedSmoke'] = 'FAILED'
    $report['error'] = $_.Exception.Message
    throw
} finally {
    $report['finishedUtc'] = [DateTime]::UtcNow.ToString('o')
    $report | ConvertTo-Json -Depth 6 | Out-File -LiteralPath (Join-Path $OutputDirectory 'report.json') -Encoding utf8
    Write-Host "Evidence retained in $OutputDirectory"
}
