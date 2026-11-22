#requires -Version 7.0
<#
Compile and run a read-only Inno 6 array/native-boundary diagnostic. A successful
exit means the native/static and selected token-access controls passed, not
that dynamic-array handoffs or legacy token masks passed. Every observation
and mismatch is retained in result.json and setup.log.
#>
[CmdletBinding()]
param(
    [string]$SourcePath = (Join-Path $PSScriptRoot 'inno-array-probe.iss'),
    [string]$ReportDirectory,
    [string]$CompilerPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or
    $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'The native Inno probe requires a disposable GitHub-hosted Windows VM.'
}
if (-not $ReportDirectory) { $ReportDirectory = Join-Path $env:RUNNER_TEMP 'inno-array-probe' }
$work = Join-Path ([IO.Path]::GetFullPath($ReportDirectory)) ([guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work -Force | Out-Null
$SourcePath = (Resolve-Path -LiteralPath $SourcePath).Path
$setupLog = Join-Path $work 'setup.log'
$commands = [Collections.Generic.List[object]]::new()
$observations = [Collections.Generic.List[object]]::new()
$tokenRecords = [Collections.Generic.List[string]]::new()
$failure = $null
$completed = $false

function Invoke-ProbeProcess {
    param([string]$FilePath, [string[]]$Arguments, [string]$Name, [int]$TimeoutSeconds)
    $stdoutPath = Join-Path $work "$Name.stdout.log"
    $stderrPath = Join-Path $work "$Name.stderr.log"
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $startInfo.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $command = [ordered]@{
        name = $Name; executable = $FilePath; arguments = $Arguments
        timeoutSeconds = $TimeoutSeconds; exitCode = $null; timedOut = $false
    }
    $commands.Add($command)
    $outputTask = $null
    $errorTask = $null
    try {
        if (-not $process.Start()) { throw "Could not start $Name" }
        $outputTask = $process.StandardOutput.ReadToEndAsync()
        $errorTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $command.timedOut = $true
            $process.Kill($true)
            if (-not $process.WaitForExit(5000)) { throw "$Name did not exit after owned process-tree termination" }
            throw "$Name exceeded its $TimeoutSeconds second deadline"
        }
        $command.exitCode = $process.ExitCode
        if ($process.ExitCode -ne 0) { throw "$Name exited with code $($process.ExitCode)" }
    } finally {
        if ($null -ne $outputTask -and $outputTask.Wait(5000)) {
            [IO.File]::WriteAllText($stdoutPath, $outputTask.GetAwaiter().GetResult())
        }
        if ($null -ne $errorTask -and $errorTask.Wait(5000)) {
            [IO.File]::WriteAllText($stderrPath, $errorTask.GetAwaiter().GetResult())
        }
        $process.Dispose()
    }
}

try {
    if (-not $CompilerPath) {
        $installedCompiler = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
        if (Test-Path -LiteralPath $installedCompiler) { $CompilerPath = $installedCompiler }
        else {
            $compiler = Get-Command ISCC.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1
            $CompilerPath = $compiler.Source
        }
    }
    $CompilerPath = (Resolve-Path -LiteralPath $CompilerPath).Path
    $metadata = [Diagnostics.FileVersionInfo]::GetVersionInfo($CompilerPath)
    Write-Output "Inno probe compiler: $CompilerPath; FileVersion=$($metadata.FileVersion); ProductVersion=$($metadata.ProductVersion)"
    Invoke-ProbeProcess -FilePath $CompilerPath -Name compile -TimeoutSeconds 60 `
        -Arguments @('/Qp', "/O$work", $SourcePath)
    $executable = Join-Path $work 'inno-array-probe.exe'
    if (-not (Test-Path -LiteralPath $executable)) { throw 'The compiler did not produce the probe executable.' }
    Invoke-ProbeProcess -FilePath $executable -Name probe -TimeoutSeconds 30 `
        -Arguments @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/SP-', '/NORESTART', "/LOG=$setupLog")
    $completed = $true
} catch {
    $failure = $_.Exception.Message
} finally {
    $completion = $null
    if (Test-Path -LiteralPath $setupLog) {
        foreach ($line in Get-Content -LiteralPath $setupLog) {
            if ($line -match '\[token-probe\] (?<record>.*)') { $tokenRecords.Add($Matches.record) }
            if ($line -match '\[array-probe\] name=(?<name>[^;]+);actual=(?<actual>-?\d+);expected=(?<expected>-?\d+);required=(?<required>[01])') {
                $observations.Add([ordered]@{
                    name = $Matches.name; actual = [int]$Matches.actual
                    expected = [int]$Matches.expected; required = $Matches.required -eq '1'
                    matched = $Matches.actual -eq $Matches.expected
                })
            } elseif ($line -match '\[array-probe\] complete=(?<complete>[01]);required-failures=(?<failures>\d+);observations=(?<count>\d+)') {
                $completion = [ordered]@{
                    complete = $Matches.complete -eq '1'
                    requiredFailures = [int]$Matches.failures; observations = [int]$Matches.count
                }
            }
        }
    }
    if ($completed -and ($null -eq $completion -or -not $completion.complete -or
        $completion.requiredFailures -ne 0 -or $completion.observations -ne $observations.Count)) {
        $completed = $false
        $failure = 'The probe did not record a complete, passing set of native/static and token controls.'
    }
    $dynamicMismatches = @($observations | Where-Object {
        -not $_.required -and -not $_.matched -and -not $_.name.StartsWith('token-')
    })
    $tokenMismatches = @($observations | Where-Object {
        -not $_.required -and -not $_.matched -and $_.name.StartsWith('token-')
    })
    [ordered]@{
        controlsPassed = $completed; failure = $failure; completion = $completion
        dynamicMismatchCount = $dynamicMismatches.Count; dynamicMismatches = $dynamicMismatches
        tokenMismatchCount = $tokenMismatches.Count; tokenMismatches = $tokenMismatches
        tokenRecords = @($tokenRecords)
        observations = @($observations); commands = @($commands)
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $work 'result.json') -Encoding utf8
    Write-Output "Inno probe: controlsPassed=$completed; dynamicMismatchCount=$($dynamicMismatches.Count); tokenMismatchCount=$($tokenMismatches.Count); report=$work"
    foreach ($mismatch in $dynamicMismatches) {
        Write-Output "Observed dynamic mismatch: $($mismatch.name) actual=$($mismatch.actual) expected=$($mismatch.expected)"
    }
    foreach ($record in $tokenRecords) { Write-Output "Token probe: $record" }
}
if (-not $completed) { throw $failure }
