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
$originalProcess = $null
$launchGate = $null
$launchGateHeld = $false
$preserveInstallerStaging = $false

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

function Get-InstallerRegistration([string]$executable) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
        [Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Uninstall\QQRM.RustSwitcher_is1')
        if ($null -eq $key) { return $null }
        try {
            $location = [string]$key.GetValue('InstallLocation', '')
            $kind = [string]$key.GetValue('WinGetInstallerType', '')
            if ($kind -cne 'inno' -or -not [IO.Path]::IsPathRooted($location)) { return $null }
            $registered = [IO.Path]::GetFullPath((Join-Path $location 'rust-switcher.exe'))
            if (-not [string]::Equals($registered, [IO.Path]::GetFullPath($executable), [StringComparison]::OrdinalIgnoreCase)) { return $null }
            return [pscustomobject]@{ Version = [string]$key.GetValue('DisplayVersion', '') }
        } finally { $key.Dispose() }
    } finally { $base.Dispose() }
}

function Get-InstalledAssetMetadata($release, [string]$tag) {
    $matchingAssets = @($release.assets | Where-Object { $_.name -ceq 'rust-switcher.exe' })
    if ($matchingAssets.Count -ne 1) { throw 'Expected exactly one installed executable release asset' }
    $asset = $matchingAssets[0]
    $url = "https://github.com/qqrm/rust-switcher-rs/releases/download/$tag/rust-switcher.exe"
    $digest = [string]$asset.digest
    if ([string]$asset.browser_download_url -cne $url -or $digest -cnotmatch '^sha256:[0-9a-f]{64}$' -or
        [long]$asset.size -lt 1024 -or [long]$asset.size -gt 104857600) {
        throw 'Installed executable release metadata is invalid'
    }
    return [pscustomobject]@{ Hash = $digest.Substring(7); Size = [long]$asset.size }
}

function Initialize-InstalledReadiness {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class RustSwitcherInstalledReady {
    private delegate bool EnumWindowCallback(IntPtr window, IntPtr parameter);
    [StructLayout(LayoutKind.Sequential)] private struct IconIdentifier {
        public uint size; public IntPtr window; public uint id; public Guid guid;
    }
    [StructLayout(LayoutKind.Sequential)] private struct Rectangle { public int left, top, right, bottom; }
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetClassName(IntPtr window, StringBuilder name, int capacity);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true)]
    private static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr w, IntPtr l, uint flags, uint timeout, out UIntPtr result);
    [DllImport("shell32.dll")] private static extern int Shell_NotifyIconGetRect(ref IconIdentifier icon, out Rectangle rectangle);
    public static bool HasReadyTray(int processId) {
        bool ready = false;
        EnumWindows(delegate(IntPtr window, IntPtr parameter) {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner != (uint)processId) return true;
            var name = new StringBuilder(256);
            GetClassName(window, name, name.Capacity);
            if (name.ToString() != "RustSwitcherMainWindow") return true;
            UIntPtr result;
            if (SendMessageTimeout(window, 0, UIntPtr.Zero, IntPtr.Zero, 2, 500, out result) == IntPtr.Zero) return true;
            var icon = new IconIdentifier();
            icon.size = (uint)Marshal.SizeOf(typeof(IconIdentifier));
            icon.window = window;
            icon.id = 1;
            Rectangle rectangle;
            ready = Shell_NotifyIconGetRect(ref icon, out rectangle) == 0 &&
                rectangle.right > rectangle.left && rectangle.bottom > rectangle.top;
            return !ready;
        }, IntPtr.Zero);
        return ready;
    }
}
'@
}

function Wait-InstalledTray {
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $session = (Get-Process -Id $PID).SessionId
    do {
        $processes = @(Get-Process -Name rust-switcher -ErrorAction SilentlyContinue)
        try {
            $owned = @($processes | Where-Object {
                try {
                    -not $_.HasExited -and $_.SessionId -eq $session -and
                        [string]::Equals($_.Path, $TargetPath, [StringComparison]::OrdinalIgnoreCase)
                } catch { $false } # A process can exit while its path is inspected.
            })
            if ($owned.Count -eq 1 -and $owned[0].Id -ne $ParentPid -and
                [RustSwitcherInstalledReady]::HasReadyTray($owned[0].Id)) { return }
        } finally { foreach ($process in $processes) { $process.Dispose() } }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'The installed update did not leave exactly one responsive application with a usable tray icon'
}

try {
    if (-not (Test-Path -LiteralPath $TargetPath -PathType Leaf)) {
        throw "Installed Rust Switcher executable was not found"
    }
    if ($ParentPid -le 0) {
        throw "Invalid running process ID"
    }
    $originalProcess = Get-Process -Id $ParentPid -ErrorAction SilentlyContinue
    if ($null -eq $originalProcess) {
        Write-UpdateLog 'Original application already exited; cancelling this update'
        return
    }
    # Pin the original process handle; a reused PID must not target a later launch.
    $null = $originalProcess.Handle
    if (-not [string]::Equals($originalProcess.Path, [IO.Path]::GetFullPath($TargetPath),
        [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The updater parent does not match the installed executable'
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

    $managed = $null -ne (Get-InstallerRegistration $TargetPath)
    $assetName = if ($managed) { 'rust-switcher-setup.exe' } else { 'rust-switcher.exe' }
    $assets = @($release.assets | Where-Object { $_.name -ceq $assetName })
    if ($assets.Count -ne 1) {
        throw "Expected exactly one $assetName release asset"
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
    $downloadUrl = "https://github.com/qqrm/rust-switcher-rs/releases/download/$tag/$assetName"
    if ([string]$asset.browser_download_url -cne $downloadUrl) {
        throw "Unexpected release asset URL"
    }

    Write-UpdateLog "Downloading verified release $tag"
    Invoke-WebRequest -Uri $downloadUrl -Headers $headers -OutFile $staged -UseBasicParsing -TimeoutSec 180 | Out-Null
    if ((Get-Item -LiteralPath $staged).Length -ne [long]$asset.size) {
        throw "Downloaded executable size mismatch"
    }
    Assert-Hash $staged $expectedHash

    if ($managed) {
        $installedAsset = Get-InstalledAssetMetadata $release $tag
        Initialize-InstalledReadiness
        if ($originalProcess.HasExited) {
            Write-UpdateLog 'Update cancelled: the original application has exited'
            return
        }
        # Let the installer own shutdown, rollback, restart and ARP version updates.
        # Falling back to a raw replacement would leave WinGet's version stale.
        Write-UpdateLog "Installing verified package $tag"
        $directory = Split-Path -Parent $TargetPath
        $arguments = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-',
            ('/DIR="' + $directory + '"'), ('/LOG="' + $LogPath + '.installer.log"'))
        $installation = Start-Process -FilePath $staged -ArgumentList $arguments -PassThru -ErrorAction Stop
        $preserveInstallerStaging = $true
        try {
            # Waiting for the process tree would also wait for the restarted tray app.
            $null = $installation.Handle
            if (-not $installation.WaitForExit(180000)) {
                throw 'Package installer is still running after 180 seconds; it was left running to finish or roll back'
            }
            $preserveInstallerStaging = $false
            if ($installation.ExitCode -ne 0) { throw "Package installer failed with exit code $($installation.ExitCode)" }
        } finally { $installation.Dispose() }
        if ((Get-Item -LiteralPath $TargetPath).Length -ne $installedAsset.Size) {
            throw 'The installer left an unexpected executable size'
        }
        Assert-Hash $TargetPath $installedAsset.Hash
        $registration = Get-InstallerRegistration $TargetPath
        if ($null -eq $registration -or $registration.Version -cne $latest.ToString()) {
            throw 'The installer did not update the installed application version'
        }
        if (-not $originalProcess.WaitForExit(10000)) { throw 'The installer left the original application running' }
        Wait-InstalledTray
        Write-UpdateLog "Updated package to $tag"
        return
    }

    # A user relaunch and an update must not replace each other's running process.
    # The quit helper bypasses this gate; newly started applications wait for finally.
    $launchGate = [Threading.Mutex]::new($false, 'Local\RustSwitcher_SingleInstance.Restart')
    try { $launchGateHeld = $launchGate.WaitOne(15000) }
    catch [Threading.AbandonedMutexException] { $launchGateHeld = $true }
    if (-not $launchGateHeld) { throw 'Another application launch is still in progress' }
    if ($originalProcess.HasExited) {
        Write-UpdateLog 'Original application was restarted; cancelling this stale update'
        return
    }

    # Preflight write access and make the replacement a same-directory rename.
    $directory = Split-Path -Parent $TargetPath
    $suffix = [guid]::NewGuid().ToString('N')
    $candidate = Join-Path $directory (".rust-switcher-update-$suffix.exe")
    $backup = Join-Path $directory (".rust-switcher-backup-$suffix.exe")
    Copy-Item -LiteralPath $staged -Destination $candidate -ErrorAction Stop
    Assert-Hash $candidate $expectedHash

    Write-UpdateLog "Verified $tag; requesting graceful application exit"
    $quit = Start-Process -FilePath $TargetPath -ArgumentList @('--quit-for-update', '--parent-pid', "$ParentPid") -WindowStyle Hidden -Wait -PassThru -ErrorAction Stop
    if ($quit.ExitCode -ne 0) { throw 'Original application no longer accepts this update' }
    $quitRequested = $true
    if (-not $originalProcess.WaitForExit(5000)) {
        throw "Rust Switcher did not exit within 5 seconds; the original file is unchanged"
    }

    # Both moves occur within the destination directory. Preserve the old binary
    # until the new one has launched, then remove the backup.
    [System.IO.File]::Move($TargetPath, $backup)
    try {
        Assert-Hash $candidate $expectedHash
        [System.IO.File]::Move($candidate, $TargetPath)
        Start-Process -FilePath $TargetPath -ArgumentList '--ensure-running' -WorkingDirectory $directory -ErrorAction Stop
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
            Start-Process -FilePath $TargetPath -ArgumentList '--ensure-running' -ErrorAction Stop
        } catch {
            Write-UpdateLog ("Restart failed: " + $_.Exception.Message)
        }
    }
    exit 1
} finally {
    if ($launchGateHeld) { $launchGate.ReleaseMutex() }
    if ($null -ne $launchGate) { $launchGate.Dispose() }
    if ($null -ne $originalProcess) { $originalProcess.Dispose() }
    if (-not $preserveInstallerStaging) {
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $candidate) {
        Remove-Item -LiteralPath $candidate -Force -ErrorAction SilentlyContinue
    }
    if ($preserveInstallerStaging) {
        Write-UpdateLog "Installer state is still active or unknown; retained staging directory: $workDir"
    } else {
        # The script may remain briefly open; failure to remove it is harmless.
        Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
