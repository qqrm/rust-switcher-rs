param([Parameter(Mandatory = $true)][string]$DebugExe)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Password mutation checks require the dedicated GitHub-hosted Windows job.'
}

$DebugExe = (Resolve-Path -LiteralPath $DebugExe).Path
$source = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../src/platform/win/secure_input.rs')).Path
$original = [System.IO.File]::ReadAllBytes($source)
$text = [System.Text.Encoding]::UTF8.GetString($original)
$anchor = [regex]::new('(?m)^pub\(crate\) fn element_status\(element: &IUIAutomationElement\) -> Status \{\r?$')
if ($anchor.Matches($text).Count -ne 1) { throw 'The UIA password mutation must match exactly one production predicate' }
$mutated = $anchor.Replace($text, {
    param($match)
    return $match.Value + "`n    if std::hint::black_box(true) { return Status::NotSecure; }"
}, 1)
$runRoot = Join-Path $env:RUNNER_TEMP ('switcher-security-mutation-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runRoot | Out-Null
$mutantExe = Join-Path $runRoot 'rust-switcher-debug.exe'

try {
    try {
        [System.IO.File]::WriteAllText($source, $mutated, [System.Text.UTF8Encoding]::new($false))
        cargo +nightly build --locked --bin rust-switcher-debug --features debug-tracing,debug-binary
        if ($LASTEXITCODE -ne 0) { throw 'The intentional UIA guard mutation did not compile' }
        Copy-Item -LiteralPath $DebugExe -Destination $mutantExe
    } finally {
        # Restore the source and executable before any packaging/release check.
        # There is no runtime setting that disables protection in the product.
        [System.IO.File]::WriteAllBytes($source, $original)
        cargo +nightly build --locked --bin rust-switcher-debug --features debug-tracing,debug-binary
        if ($LASTEXITCODE -ne 0) { throw 'Could not restore the protected debug executable after mutation' }
    }

    $info = [System.Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-File', (Join-Path $PSScriptRoot 'run-browser.ps1'),
        '-DebugExe', $mutantExe, '-SecurityOnly')) {
        $info.ArgumentList.Add($argument)
    }
    $process = [System.Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(180000)) {
            $process.Kill($true)
            $process.WaitForExit(5000) | Out-Null
            throw 'Mutation harness timed out; a timeout does not prove password protection'
        }
        $stdout.Wait()
        $stderr.Wait()
        $output = $stdout.Result + "`n" + $stderr.Result
        Write-Output $output
        if ($process.ExitCode -eq 0) { throw 'Browser password tests accepted a build with UIA protection removed' }
        if (-not $output.Contains('SECURITY_CAPTURE_VIOLATION:')) {
            throw 'The mutant failed without detecting journal capture; unrelated harness failures are not sufficient'
        }
        Write-Output 'PASS: UIA password-protection mutation was rejected by real browser journal assertions'
    } finally { $process.Dispose() }
} finally {
    if ([Convert]::ToBase64String([System.IO.File]::ReadAllBytes($source)) -ne [Convert]::ToBase64String($original)) {
        [System.IO.File]::WriteAllBytes($source, $original)
        throw 'Mutation source restoration mismatch'
    }
    Remove-Item -LiteralPath $runRoot -Recurse -Force
}
