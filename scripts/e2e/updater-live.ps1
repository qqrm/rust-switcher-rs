# Exercise a real running Windows process and the production updater against
# an existing public GitHub Release. Both binaries live in a disposable folder.
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('rust-switcher', 'brightwheel')]
    [string]$Application
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$PSNativeCommandUseErrorActionPreference = $false

if ($Application -eq 'rust-switcher') {
    $repository = 'qqrm/rust-switcher-rs'
    $binary = 'rust-switcher.exe'
    $processName = 'rust-switcher'
    $parentParameter = '-ParentPid'
    $optionalCli = $false
} else {
    $repository = 'qqrm/brightwheel'
    $binary = 'brightwheel.exe'
    $processName = 'brightwheel'
    $parentParameter = '-ParentProcessId'
    $optionalCli = $true
}

$root = (Resolve-Path '.').Path
$source = Join-Path $root "target/release/$binary"
$scriptPath = Join-Path $root 'scripts/windows-updater.ps1'
$install = Join-Path $env:RUNNER_TEMP "updater-live-$Application-$([guid]::NewGuid().ToString('N'))"
$logPath = Join-Path $install 'updater.log'
$target = Join-Path $install $binary
$process = $null
$windowsPowerShell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'

New-Item -ItemType Directory -Force -Path $install | Out-Null
Copy-Item -LiteralPath $source -Destination $target
if ($optionalCli) {
    Copy-Item -LiteralPath (Join-Path $root 'target/release/bright.exe') -Destination (Join-Path $install 'bright.exe')
}

function Run-Updater([string]$version, [int]$parent) {
    $arguments = @('-NoProfile', '-NonInteractive', '-File', $scriptPath,
        '-TargetPath', $target, '-CurrentVersion', $version,
        $parentParameter, [string]$parent, '-LogPath', $logPath)
    & $windowsPowerShell @arguments
    return $LASTEXITCODE
}

function Assert-Asset([object]$release, [string]$filename) {
    $assets = @($release.assets | Where-Object { $_.name -ceq $filename })
    if ($assets.Count -ne 1) { throw "Expected exactly one asset: $filename" }
    $asset = $assets[0]
    $path = Join-Path $install $filename
    $digest = [string]$asset.digest
    if ($digest -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw "Invalid release digest for $filename"
    }
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -cne $digest.Substring(7) -or (Get-Item -LiteralPath $path).Length -ne [long]$asset.size) {
        throw "Installed $filename does not match published release asset"
    }
    Write-Host "$filename matches published $($release.tag_name) SHA-256 and length"
}

try {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/latest" -Headers @{'User-Agent'='Windows-Updater-E2E'} -TimeoutSec 40
    Write-Host "Testing $Application against real release $($release.tag_name)"
    $process = Start-Process -FilePath $target -WorkingDirectory $install -PassThru
    Start-Sleep -Seconds 3
    if ($process.HasExited) { throw "$binary terminated unexpectedly on startup" }

    # Ensure an up-to-date invocation does not stop or modify the live program.
    $before = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
    $noOpCode = Run-Updater '999.0.0' $process.Id
    $expectedNoOpCode = if ($optionalCli) { 10 } else { 0 }
    if ($noOpCode -ne $expectedNoOpCode) {
        throw "No-update check returned $noOpCode (expected $expectedNoOpCode)"
    }
    $after = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
    if ($process.HasExited -or $after -cne $before) {
        throw "No-update check changed or stopped the running program"
    }
    Write-Host 'PASS: no-update check preserves running process and executable'

    # Force a controlled version comparison, without modifying the compiled app,
    # so a previously published real release can exercise replacement in CI.
    $updateCode = Run-Updater '0.0.0' $process.Id
    if ($updateCode -ne 0) {
        $failureLog = if (Test-Path -LiteralPath $logPath) { Get-Content -LiteralPath $logPath -Raw } else { '[no updater log]' }
        throw "Live update failed (exit $updateCode): $failureLog"
    }
    if (-not $process.WaitForExit(20000)) {
        throw 'Original program remained running after updater reported success'
    }
    Assert-Asset $release $binary
    if ($optionalCli) { Assert-Asset $release 'bright.exe' }

    $replacement = @()
    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        $replacement = @(Get-Process -Name $processName -ErrorAction SilentlyContinue | Where-Object {
            try { $_.Path -ieq $target } catch { $false }
        })
        if ($replacement.Count -gt 0) { break }
        Start-Sleep -Milliseconds 500
    }
    if ($replacement.Count -eq 0) { throw 'Replacement executable was not restarted' }

    $backups = @(Get-ChildItem -LiteralPath $install -File -Filter '*backup*')
    if ($backups.Count -gt 0) {
        throw "Updater left backup files after a successful replacement: $($backups.Name -join ', ')"
    }
    Write-Host "PASS: original exited, published release replaced binary, and new process restarted"
} finally {
    Get-Process -Name $processName -ErrorAction SilentlyContinue |
        Where-Object {
            try { $_.Path -ieq $target } catch { $false }
        } | Stop-Process -Force -ErrorAction SilentlyContinue
    if ($null -ne $process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $logPath) {
        Get-Content -LiteralPath $logPath | ForEach-Object { Write-Host $_ }
    }
}
