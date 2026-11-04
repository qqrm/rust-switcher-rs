#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TargetPath,
    [Parameter(Mandatory = $true)][string]$CurrentVersion,
    [Parameter(Mandatory = $true)][int]$ParentPid,
    [Parameter(Mandatory = $true)][string]$LogPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$workDir = Split-Path -Parent $PSCommandPath
$staged = Join-Path $workDir 'rust-switcher.download.exe'
$candidate = $null
$backup = $null
$quitRequested = $false
$startedNew = $false

function Write-UpdateLog([string]$message) {
    try {
        $logDirectory = Split-Path -Parent $LogPath
        New-Item -ItemType Directory -Force -Path $logDirectory | Out-Null
        Add-Content -LiteralPath $LogPath -Value ("{0:u} {1}" -f (Get-Date), $message) -Encoding UTF8
    } catch {
        # Logging must not interrupt a verified update or its rollback.
    }
}

function Read-Version([string]$value) {
    if ($value -cnotmatch '^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        throw "Invalid release version: $value"
    }
    return [version]($value.TrimStart('v'))
}

function Assert-Hash([string]$file, [string]$expected) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "Missing staged executable"
    }
    $actual = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -cne $expected) {
        throw "Downloaded executable SHA-256 mismatch"
    }
}

try {
    if (-not (Test-Path -LiteralPath $TargetPath -PathType Leaf)) {
        throw "Installed Rust Switcher executable was not found"
    }
    if ($ParentPid -le 0) {
        throw "Invalid running process ID"
    }

    $current = Read-Version $CurrentVersion
    $headers = @{ 'User-Agent' = 'RustSwitcher-Updater' }
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/qqrm/rust-switcher-rs/releases/latest' -Headers $headers -TimeoutSec 45
    if ($release.draft -or $release.prerelease) {
        throw "Latest release is not a stable published release"
    }

    $tag = [string]$release.tag_name
    $latest = Read-Version $tag
    if ($latest -le $current) {
        Write-UpdateLog "Already up to date ($CurrentVersion)"
        return
    }

    $assets = @($release.assets | Where-Object { $_.name -ceq 'rust-switcher.exe' })
    if ($assets.Count -ne 1) {
        throw "Expected exactly one rust-switcher.exe release asset"
    }
    $asset = $assets[0]
    $digest = [string]$asset.digest
    if ($digest -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw "Release asset has no valid SHA-256 digest"
    }
    $expectedHash = $digest.Substring(7)
    if ([long]$asset.size -lt 1024 -or [long]$asset.size -gt 104857600) {
        throw "Release executable size is outside the permitted bounds"
    }

    # Never follow a release-provided arbitrary URL; construct the pinned repository URL.
    $downloadUrl = "https://github.com/qqrm/rust-switcher-rs/releases/download/$tag/rust-switcher.exe"
    if ([string]$asset.browser_download_url -cne $downloadUrl) {
        throw "Unexpected release asset URL"
    }

    Write-UpdateLog "Downloading verified release $tag"
    Invoke-WebRequest -Uri $downloadUrl -Headers $headers -OutFile $staged -UseBasicParsing -TimeoutSec 180 | Out-Null
    if ((Get-Item -LiteralPath $staged).Length -ne [long]$asset.size) {
        throw "Downloaded executable size mismatch"
    }
    Assert-Hash $staged $expectedHash

    # Preflight write access and make the replacement a same-directory rename.
    $directory = Split-Path -Parent $TargetPath
    $suffix = [guid]::NewGuid().ToString('N')
    $candidate = Join-Path $directory (".rust-switcher-update-$suffix.exe")
    $backup = Join-Path $directory (".rust-switcher-backup-$suffix.exe")
    Copy-Item -LiteralPath $staged -Destination $candidate -ErrorAction Stop
    Assert-Hash $candidate $expectedHash

    Write-UpdateLog "Verified $tag; requesting graceful application exit"
    $quitRequested = $true
    Start-Process -FilePath $TargetPath -ArgumentList '--quit-for-update' -WindowStyle Hidden -Wait -ErrorAction Stop
    $running = Get-Process -Id $ParentPid -ErrorAction SilentlyContinue
    if ($null -ne $running -and -not $running.WaitForExit(90000)) {
        throw "Rust Switcher did not exit within 90 seconds; the original file is unchanged"
    }

    # Both moves occur within the destination directory. Preserve the old binary
    # until the new one has launched, then remove the backup.
    [System.IO.File]::Move($TargetPath, $backup)
    try {
        Assert-Hash $candidate $expectedHash
        [System.IO.File]::Move($candidate, $TargetPath)
        Start-Process -FilePath $TargetPath -WorkingDirectory $directory -ErrorAction Stop
        $startedNew = $true
        Write-UpdateLog "Updated to $tag"
    } catch {
        if (Test-Path -LiteralPath $TargetPath -PathType Leaf) {
            Remove-Item -LiteralPath $TargetPath -Force -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $backup -PathType Leaf) {
            [System.IO.File]::Move($backup, $TargetPath)
        }
        throw
    }

    Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
} catch {
    Write-UpdateLog ("Update failed: " + $_.Exception.Message)
    if ($null -ne $backup -and (Test-Path -LiteralPath $backup -PathType Leaf) -and
        -not (Test-Path -LiteralPath $TargetPath -PathType Leaf)) {
        try {
            [System.IO.File]::Move($backup, $TargetPath)
        } catch {
            Write-UpdateLog ("Rollback failed: " + $_.Exception.Message)
        }
    }
    if ($quitRequested -and -not $startedNew -and
        (Test-Path -LiteralPath $TargetPath -PathType Leaf)) {
        try {
            Start-Process -FilePath $TargetPath -ErrorAction Stop
        } catch {
            Write-UpdateLog ("Restart failed: " + $_.Exception.Message)
        }
    }
    exit 1
} finally {
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    if ($null -ne $candidate) {
        Remove-Item -LiteralPath $candidate -Force -ErrorAction SilentlyContinue
    }
    # The script may remain briefly open; failure to remove it is harmless.
    Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
}
