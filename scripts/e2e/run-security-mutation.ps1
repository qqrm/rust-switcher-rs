param([Parameter(Mandatory = $true)][string]$DebugExe)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Target mutation checks require the dedicated GitHub-hosted Windows job.'
}
$DebugExe = (Resolve-Path -LiteralPath $DebugExe).Path
$sourceRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../src')).Path
$paths = @(
    (Join-Path $sourceRoot 'domain/text/selection_probe.rs'),
    (Join-Path $sourceRoot 'platform/win/secure_input.rs')
)
$original = @{}
foreach ($path in $paths) { $original[$path] = [System.IO.File]::ReadAllBytes($path) }
$runRoot = Join-Path $env:RUNNER_TEMP ('switcher-target-mutation-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runRoot | Out-Null
$mutantExe = Join-Path $runRoot 'rust-switcher-debug.exe'

function Add-GuardMutation([string]$Text, [string]$Pattern, [string]$ReturnValue) {
    $anchor = [regex]::new($Pattern)
    if ($anchor.Matches($Text).Count -ne 1) { throw "Target mutation must match exactly one guard: $Pattern" }
    return $anchor.Replace($Text, {
        param($match)
        return $match.Value + "`n    if std::hint::black_box(true) { return $ReturnValue; }"
    }, 1)
}
function Invoke-TargetRunner([string]$Exe, [bool]$ExpectViolation) {
    $info = [System.Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-File', (Join-Path $PSScriptRoot 'run-target-regression.ps1'), '-Exe', $Exe)) {
        $info.ArgumentList.Add($argument)
    }
    $process = [System.Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(90000)) {
            $process.Kill($true)
            $process.WaitForExit(5000) | Out-Null
            throw 'Target harness timed out; a timeout does not detect a wrong-target mutation'
        }
        $stdout.Wait(); $stderr.Wait()
        $output = $stdout.Result + "`n" + $stderr.Result
        Write-Output $output
        if ($ExpectViolation) {
            if ($process.ExitCode -eq 0 -or -not $output.Contains('WRONG_TARGET_WRITE:')) {
                throw 'The mutant must fail for a concrete wrong-target write, not setup, build or unrelated errors'
            }
            if (-not $output.Contains('PASS: native target positive control converted once')) {
                throw 'The mutant did not establish a working conversion positive control'
            }
        } elseif ($process.ExitCode -ne 0) { throw 'The unmodified source failed its target-race baseline' }
    } finally { $process.Dispose() }
}

try {
    Invoke-TargetRunner $DebugExe $false
    try {
        $selection = [System.Text.Encoding]::UTF8.GetString($original[$paths[0]])
        $selection = Add-GuardMutation $selection '(?m)^\s*pub\(super\) fn is_current\(self\) -> bool \{' 'true'
        $context = [System.Text.Encoding]::UTF8.GetString($original[$paths[1]])
        $context = Add-GuardMutation $context '(?m)^fn active_command_is_current\(\) -> bool \{' 'true'
        $context = Add-GuardMutation $context '(?m)^pub\(crate\) fn protect_current_context\(\) -> Status \{' 'Status::Verified'
        # Remove the overlapping source-target and command-origin checks together.
        # This deliberately broken build represents wrong-target delivery; it
        # does not claim each individual guard is independently necessary.
        [System.IO.File]::WriteAllText($paths[0], $selection, [System.Text.UTF8Encoding]::new($false))
        [System.IO.File]::WriteAllText($paths[1], $context, [System.Text.UTF8Encoding]::new($false))
        cargo +nightly build --locked --bin rust-switcher-debug --features debug-tracing,debug-binary
        if ($LASTEXITCODE -ne 0) { throw 'The intentional target-ownership mutation did not compile' }
        Copy-Item -LiteralPath $DebugExe -Destination $mutantExe
    } finally {
        foreach ($path in $paths) { [System.IO.File]::WriteAllBytes($path, $original[$path]) }
        cargo +nightly build --locked --bin rust-switcher-debug --features debug-tracing,debug-binary
        if ($LASTEXITCODE -ne 0) { throw 'Could not restore the original executable after target mutation' }
    }
    Invoke-TargetRunner $mutantExe $true
    Write-Output 'PASS: target-ownership mutation caused a real write into the wrong native field and was rejected'
} finally {
    foreach ($path in $paths) {
        if ([Convert]::ToBase64String([System.IO.File]::ReadAllBytes($path)) -ne [Convert]::ToBase64String($original[$path])) {
            [System.IO.File]::WriteAllBytes($path, $original[$path])
            throw 'Mutation source restoration mismatch'
        }
    }
    Remove-Item -LiteralPath $runRoot -Recurse -Force
}
