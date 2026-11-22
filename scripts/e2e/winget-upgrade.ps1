#requires -Version 7.0
<#
Acceptance test for the actual package-manager path on a disposable Windows VM.
The baseline comes from the public WinGet source. Candidate manifests are copied
from the production packaging output; only their download URL and hash change.
Local manifests exercise WinGet selection, the installer, and installed ARP state.
They do not exercise publication or writing the source's tracking catalog.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('brightwheel', 'rust-switcher')][string]$Application,
    [Parameter(Mandatory)][string]$InstallerPath,
    [Parameter(Mandatory)][string]$ExecutablePath,
    [Parameter(Mandatory)][string]$ManifestDirectory,
    [Parameter(Mandatory)][string]$PackageVersion,
    [string]$LegacyVersion,
    [string]$BuildScriptPath,
    [string]$ManagedUpdaterTestPath = (Join-Path $PSScriptRoot 'installer-updater.ps1'),
    [string]$ReportDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# An accident guard, not a sandbox: the disposable hosted VM is the boundary.
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or
    $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'Package-manager acceptance requires a disposable GitHub-hosted Windows VM.'
}
if (-not [Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
    throw 'A logged-in interactive Windows desktop is required for tray assertions.'
}

$applicationInfo = if ($Application -eq 'brightwheel') {
    @{ Id = 'QQRM.BrightWheel'; Name = 'BrightWheel'; WindowClass = 'BrightWheel.HiddenWindow';
        Baseline = '0.1.9'; BuildScript = 'package-windows.ps1'; Setup = 'brightwheel-setup.exe' }
} else {
    @{ Id = 'QQRM.RustSwitcher'; Name = 'RustSwitcher'; WindowClass = 'RustSwitcherMainWindow';
        Baseline = '1.0.29'; BuildScript = 'build-installer.ps1'; Setup = 'rust-switcher-setup.exe' }
}
$packageId = $applicationInfo.Id
$productCode = $packageId + '_is1'
$legacyProductCode = $packageId + '_Microsoft.Winget.Source_8wekyb3d8bbwe'
$uninstallRoot = 'Software\Microsoft\Windows\CurrentVersion\Uninstall'
$runRegistryPath = 'Software\Microsoft\Windows\CurrentVersion\Run'
if (-not $LegacyVersion) { $LegacyVersion = $applicationInfo.Baseline }
if (-not $BuildScriptPath) { $BuildScriptPath = Join-Path $PSScriptRoot ('../' + $applicationInfo.BuildScript) }
foreach ($inputPath in @($InstallerPath, $ExecutablePath, $ManifestDirectory, $BuildScriptPath, $ManagedUpdaterTestPath)) {
    if (-not (Test-Path -LiteralPath $inputPath)) { throw "Required input does not exist: $inputPath" }
}
$InstallerPath = (Resolve-Path -LiteralPath $InstallerPath).Path
$ExecutablePath = (Resolve-Path -LiteralPath $ExecutablePath).Path
$ManifestDirectory = (Resolve-Path -LiteralPath $ManifestDirectory).Path
$BuildScriptPath = (Resolve-Path -LiteralPath $BuildScriptPath).Path
$ManagedUpdaterTestPath = (Resolve-Path -LiteralPath $ManagedUpdaterTestPath).Path
$version = [Version]$PackageVersion
if ($PackageVersion -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' -or
    $version -le [Version]$LegacyVersion) {
    throw 'PackageVersion must be a three-component version newer than LegacyVersion.'
}
if (@(Get-Process -Name $Application -ErrorAction SilentlyContinue).Count) {
    throw 'A pre-existing application process would invalidate this isolated scenario.'
}
if (-not $ReportDirectory) { $ReportDirectory = Join-Path $env:RUNNER_TEMP "winget-upgrade-$Application" }
$ReportDirectory = [IO.Path]::GetFullPath($ReportDirectory)
$work = Join-Path $ReportDirectory ([guid]::NewGuid().ToString('N'))
$installDirectory = Join-Path $work 'Installed application with spaces'
$installedExe = Join-Path $installDirectory "$Application.exe"
$profileDirectory = Join-Path $work 'Isolated profile'
$serveDirectory = Join-Path $work 'downloads'
$logDirectory = Join-Path $work 'logs'
$expectedHash = (Get-FileHash -LiteralPath $ExecutablePath -Algorithm SHA256).Hash
$expectedFileVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($ExecutablePath).FileVersion
$checks = [Collections.Generic.List[string]]::new()
$commands = [Collections.Generic.List[object]]::new()
$tokenComparisons = [Collections.Generic.List[object]]::new()
$cleanupErrors = [Collections.Generic.List[string]]::new()
$serverJob = $null
$fileLock = $null
$winget = $null
$changedLocalManifestSetting = $false
$settingsSnapshot = $null
$runSnapshot = $null
$pathSnapshot = $null
$brightConfiguredSnapshot = $null
$brightUpdatesSnapshot = $null
$brightConfigExisted = $false
$snapshotTaken = $false
$installedByTest = $false
$sentinel = $null
$sentinelText = 'Unrelated WinGet alias fixture ' + [guid]::NewGuid().ToString('N')
$appDataSnapshot = $env:APPDATA
$processPathSnapshot = $env:PATH
$failure = $null
$fixtureToken = [IntPtr]::Zero
$tokenTest = $null

Add-Type -TypeDefinition @'
using System;
using System.Collections;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
public static class PackageAcceptanceNative {
    public sealed class TokenSnapshot {
        public string UserSid { get; set; }
        public string IntegritySid { get; set; }
        public int Elevated { get; set; }
        public int SessionId { get; set; }
    }
    [DllImport("kernel32.dll", SetLastError=true)]
    private static extern IntPtr OpenProcess(uint access, bool inherit, int processId);
    [DllImport("advapi32.dll", SetLastError=true)]
    private static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError=true)]
    private static extern bool GetTokenInformation(IntPtr token, int informationClass, IntPtr information,
        uint length, out uint returned);
    [DllImport("advapi32.dll", SetLastError=true)]
    private static extern bool DuplicateTokenEx(IntPtr existing, uint access, IntPtr attributes,
        int impersonationLevel, int tokenType, out IntPtr token);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern bool ConvertSidToStringSid(IntPtr sid, out IntPtr text);
    [DllImport("kernel32.dll")] private static extern IntPtr LocalFree(IntPtr memory);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    private static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string className, string title);
    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true)]
    private static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr w,
        IntPtr l, uint flags, uint timeout, out UIntPtr result);
    [StructLayout(LayoutKind.Sequential)] private struct IconIdentifier {
        public uint size; public IntPtr window; public uint id; public Guid guid;
    }
    [StructLayout(LayoutKind.Sequential)] private struct Rectangle {
        public int left, top, right, bottom;
    }
    [DllImport("shell32.dll")]
    private static extern int Shell_NotifyIconGetRect(ref IconIdentifier icon, out Rectangle rect);
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] private struct StartupInfo {
        public uint size; public string reserved, desktop, title;
        public uint x, y, width, height, xChars, yChars, fill, flags;
        public ushort show, reservedSize;
        public IntPtr reservedBytes, input, output, error;
    }
    [StructLayout(LayoutKind.Sequential)] private struct ProcessInformation {
        public IntPtr process, thread; public uint processId, threadId;
    }
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern bool CreateProcess(string application, StringBuilder command,
        IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags,
        IntPtr environment, string directory, ref StartupInfo startup, out ProcessInformation process);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern bool CreateProcessWithTokenW(IntPtr token, uint logonFlags, string application,
        StringBuilder command, uint creationFlags, IntPtr environment, string directory,
        ref StartupInfo startup, out ProcessInformation process);
    [DllImport("kernel32.dll")] private static extern bool CloseHandle(IntPtr handle);
    private static IntPtr TokenInformation(IntPtr token, int informationClass) {
        uint length;
        GetTokenInformation(token, informationClass, IntPtr.Zero, 0, out length);
        if (length == 0) throw new Win32Exception(Marshal.GetLastWin32Error());
        var memory = Marshal.AllocHGlobal(checked((int)length));
        if (!GetTokenInformation(token, informationClass, memory, length, out length)) {
            int error = Marshal.GetLastWin32Error();
            Marshal.FreeHGlobal(memory);
            throw new Win32Exception(error);
        }
        return memory;
    }
    private static string TokenSid(IntPtr token, int informationClass) {
        var information = TokenInformation(token, informationClass);
        try {
            IntPtr text;
            if (!ConvertSidToStringSid(Marshal.ReadIntPtr(information), out text))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            try { return Marshal.PtrToStringUni(text); }
            finally { LocalFree(text); }
        } finally { Marshal.FreeHGlobal(information); }
    }
    private static int TokenNumber(IntPtr token, int informationClass) {
        var information = TokenInformation(token, informationClass);
        try { return Marshal.ReadInt32(information); }
        finally { Marshal.FreeHGlobal(information); }
    }
    public static TokenSnapshot SnapshotToken(int processId) {
        var process = OpenProcess(0x1000, false, processId); // PROCESS_QUERY_LIMITED_INFORMATION
        if (process == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
        try {
            IntPtr token;
            if (!OpenProcessToken(process, 0x0008, out token)) // TOKEN_QUERY
                throw new Win32Exception(Marshal.GetLastWin32Error());
            try {
                return new TokenSnapshot { UserSid = TokenSid(token, 1), IntegritySid = TokenSid(token, 25),
                    Elevated = TokenNumber(token, 20), SessionId = TokenNumber(token, 12) };
            } finally { CloseHandle(token); }
        } finally { CloseHandle(process); }
    }
    public static IntPtr LinkedNormalToken(out string reason) {
        reason = null;
        IntPtr current;
        if (!OpenProcessToken(new IntPtr(-1), 0x0008, out current))
            throw new Win32Exception(Marshal.GetLastWin32Error());
        try {
            var information = Marshal.AllocHGlobal(IntPtr.Size);
            IntPtr linked = IntPtr.Zero;
            try {
                uint returned;
                if (!GetTokenInformation(current, 19, information, (uint)IntPtr.Size, out returned)) {
                    reason = "No linked normal token (Win32 " + Marshal.GetLastWin32Error() + ")";
                    return IntPtr.Zero;
                }
                linked = Marshal.ReadIntPtr(information);
                if (linked == IntPtr.Zero || TokenNumber(linked, 20) != 0 ||
                    TokenSid(linked, 1) != TokenSid(current, 1) || TokenNumber(linked, 12) != TokenNumber(current, 12)) {
                    reason = "Linked token is not an unelevated token for the same user and session";
                    return IntPtr.Zero;
                }
                IntPtr duplicate;
                if (!DuplicateTokenEx(linked, 0x000B, IntPtr.Zero, 2, 1, out duplicate))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                reason = "Using the runner user's linked unelevated token";
                return duplicate;
            } finally {
                if (linked != IntPtr.Zero) CloseHandle(linked);
                Marshal.FreeHGlobal(information);
            }
        } finally { CloseHandle(current); }
    }
    public static void ReleaseToken(IntPtr token) { if (token != IntPtr.Zero) CloseHandle(token); }
    public static IntPtr Window(int processId, string className) {
        foreach (var parent in new[] { IntPtr.Zero, new IntPtr(-3) }) {
            var after = IntPtr.Zero;
            while (true) {
                var window = FindWindowEx(parent, after, className, null);
                if (window == IntPtr.Zero) break;
                uint owner;
                GetWindowThreadProcessId(window, out owner);
                if (owner == (uint)processId) return window;
                after = window;
            }
        }
        return IntPtr.Zero;
    }
    public static bool Ready(int processId, string className) {
        var window = Window(processId, className);
        if (window == IntPtr.Zero) return false;
        UIntPtr result;
        if (SendMessageTimeout(window, 0, UIntPtr.Zero, IntPtr.Zero, 2, 500, out result) == IntPtr.Zero)
            return false;
        var icon = new IconIdentifier { size = (uint)Marshal.SizeOf<IconIdentifier>(), window = window, id = 1 };
        Rectangle rect;
        return Shell_NotifyIconGetRect(ref icon, out rect) >= 0 && rect.right > rect.left && rect.bottom > rect.top;
    }
    public static Process StartStoredCommand(string command, string directory, IntPtr token) {
        var startup = new StartupInfo { size = (uint)Marshal.SizeOf<StartupInfo>() };
        ProcessInformation created;
        if (token == IntPtr.Zero) {
            if (!CreateProcess(null, new StringBuilder(command), IntPtr.Zero, IntPtr.Zero,
                false, 0, IntPtr.Zero, directory, ref startup, out created))
                throw new Win32Exception(Marshal.GetLastWin32Error());
        } else {
            // Preserve the isolated APPDATA fixture explicitly. A null environment
            // in CreateProcessWithTokenW can load values from the user's profile.
            var variables = new SortedDictionary<string, string>(StringComparer.OrdinalIgnoreCase);
            foreach (DictionaryEntry value in Environment.GetEnvironmentVariables())
                variables[(string)value.Key] = (string)value.Value;
            var block = string.Join("\0", variables.Select(pair => pair.Key + "=" + pair.Value)) + "\0\0";
            var environment = Marshal.StringToHGlobalUni(block);
            try {
                if (!CreateProcessWithTokenW(token, 0, null, new StringBuilder(command), 0x0400,
                    environment, directory, ref startup, out created))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
            } finally { Marshal.FreeHGlobal(environment); }
        }
        try { return Process.GetProcessById(checked((int)created.processId)); }
        finally { CloseHandle(created.thread); CloseHandle(created.process); }
    }
}
'@

function Record-Pass([string]$Name) { $checks.Add($Name); Write-Host "PASS: $Name" }
function Assert-TokenPreserved($Before, [int]$ProcessId, [string]$Stage) {
    $after = [PackageAcceptanceNative]::SnapshotToken($ProcessId)
    $tokenComparisons.Add(@{ Stage = $Stage; Before = $Before; After = $after })
    foreach ($property in @('UserSid', 'IntegritySid', 'Elevated', 'SessionId')) {
        if ($Before.$property -cne $after.$property) {
            throw "$Stage changed the application's $property from '$($Before.$property)' to '$($after.$property)'."
        }
    }
}
function Test-PathEntry([string]$Path) {
    # Test-Path can return false for a dangling symlink, which is still a leak.
    return $null -ne (Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue)
}
function Read-RegistryValue([string]$Subkey, [string]$Name) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($Subkey)
    try {
        if ($null -eq $key -or $key.GetValueNames() -notcontains $Name) { return @{ Exists = $false } }
        return @{ Exists = $true; Kind = $key.GetValueKind($Name);
            Value = $key.GetValue($Name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }
    } finally { if ($null -ne $key) { $key.Dispose() } }
}
function Restore-RegistryValue([string]$Subkey, [string]$Name, $Snapshot) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($Subkey)
    try {
        if ($Snapshot.Exists) { $key.SetValue($Name, $Snapshot.Value, $Snapshot.Kind) }
        else { $key.DeleteValue($Name, $false) }
    } finally { $key.Dispose() }
}
function Read-Arp([string]$Code) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser,
        [Microsoft.Win32.RegistryView]::Registry64)
    $key = $base.OpenSubKey("$uninstallRoot\$Code")
    try {
        if ($null -eq $key) { return $null }
        $result = @{}
        foreach ($name in $key.GetValueNames()) { $result[$name] = $key.GetValue($name) }
        return $result
    } finally { if ($null -ne $key) { $key.Dispose() }; $base.Dispose() }
}
function Invoke-Logged([string]$File, [string[]]$Arguments, [string]$Label, [int]$TimeoutSeconds = 180) {
    $info = [Diagnostics.ProcessStartInfo]::new($File)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw "Timed out after ${TimeoutSeconds}s: $Label"
        }
        $text = $stdout.GetAwaiter().GetResult() + $stderr.GetAwaiter().GetResult()
        $log = Join-Path $logDirectory (('{0:D2}-' -f $commands.Count) + $Label + '.log')
        [IO.File]::WriteAllText($log, $text)
        $commands.Add(@{ Label = $Label; Arguments = $Arguments; ExitCode = $process.ExitCode; Log = $log })
        Write-Host "$Label returned $($process.ExitCode); log: $log"
        return @{ ExitCode = $process.ExitCode; Text = $text; Log = $log }
    } finally { $process.Dispose() }
}
function Invoke-WinGet([string[]]$Arguments, [string]$Label, [switch]$AllowFailure) {
    $result = Invoke-Logged $winget $Arguments $Label
    if ($result.ExitCode -ne 0 -and -not $AllowFailure) { throw "$Label failed: $($result.Text)" }
    return $result
}
function Get-InstalledProcesses {
    return @(Get-Process -Name $Application -ErrorAction SilentlyContinue | Where-Object {
        try { [string]::Equals($_.Path, $installedExe, [StringComparison]::OrdinalIgnoreCase) }
        catch { $false }
    })
}
function Wait-Ready([int]$DifferentFrom = 0) {
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        $processes = @(Get-InstalledProcesses)
        if ($processes.Count -gt 1) { throw 'Multiple installed application instances are running.' }
        if ($processes.Count -eq 1 -and $processes[0].Id -ne $DifferentFrom -and
            [PackageAcceptanceNative]::Ready($processes[0].Id, $applicationInfo.WindowClass)) {
            return $processes[0]
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Installed application did not expose a responsive window and a registered tray icon.'
}
function Start-Installed {
    $command = '"' + $installedExe + '" --skip-startup-update'
    $process = [PackageAcceptanceNative]::StartStoredCommand($command, $installDirectory, $fixtureToken)
    $process.Dispose()
    $ready = Wait-Ready
    if ($fixtureToken -ne [IntPtr]::Zero -and
        [PackageAcceptanceNative]::SnapshotToken($ready.Id).Elevated -ne 0) {
        throw 'The linked-normal-token fixture unexpectedly started an elevated application.'
    }
    return $ready
}
function Stop-Installed {
    $processes = @(Get-InstalledProcesses)
    if (-not $processes.Count) { return }
    foreach ($process in $processes) {
        if ($process.HasExited) { continue }
        $quitArguments = @('--quit-for-update')
        if ($Application -eq 'brightwheel') { $quitArguments += "--quit-for-update=$($process.Id)" }
        $null = Invoke-Logged $installedExe $quitArguments "application-quit-$($process.Id)" 15
        if (-not $process.WaitForExit(10000)) { throw 'Installed application did not stop after its quit command.' }
    }
}
function Assert-Stopped {
    $deadline = [DateTime]::UtcNow.AddSeconds(2)
    do {
        if (@(Get-InstalledProcesses).Count) { throw 'Installer launched an application that was stopped before the update.' }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
}
function Set-AutostartFixture([bool]$Enabled) {
    $command = '"' + $installedExe + '"'
    if ($Application -eq 'rust-switcher') { $command += ' --autostart' }
    Restore-RegistryValue $runRegistryPath $applicationInfo.Name @{
        Exists = $Enabled; Value = $command; Kind = [Microsoft.Win32.RegistryValueKind]::String
    }
}
function Assert-Autostart([bool]$Enabled) {
    $actual = Read-RegistryValue $runRegistryPath $applicationInfo.Name
    $expected = '"' + $installedExe + '"'
    if ($Application -eq 'rust-switcher') { $expected += ' --autostart' }
    if ($actual.Exists -ne $Enabled -or ($Enabled -and
        ($actual.Value -cne $expected -or $actual.Kind -ne [Microsoft.Win32.RegistryValueKind]::String))) {
        throw 'Package update changed the stored autostart choice or command.'
    }
    if ($Application -eq 'brightwheel') {
        if ((Read-RegistryValue 'Software\BrightWheel' 'AutoStartConfigured').Value -ne 1 -or
            (Read-RegistryValue 'Software\BrightWheel' 'AutoUpdateEnabled').Value -ne 0) {
            throw 'Package update changed BrightWheel preferences.'
        }
    } elseif ($script:configHash -and
        (Get-FileHash -LiteralPath $script:configPath -Algorithm SHA256).Hash -cne $script:configHash) {
        throw 'Package update changed the persisted Rust Switcher configuration.'
    }
}
function Assert-Installed([string]$ExpectedVersion, [bool]$Autostart) {
    $arp = Read-Arp $productCode
    if ($null -eq $arp -or $arp.DisplayVersion -cne $ExpectedVersion) { throw "Wrong Inno ARP version; expected $ExpectedVersion" }
    if ($null -ne (Read-Arp $legacyProductCode)) { throw 'Portable ARP entry survived migration; WinGet would see two installations.' }
    if (-not [string]::Equals(([string]$arp.InstallLocation).TrimEnd('\'), $installDirectory,
            [StringComparison]::OrdinalIgnoreCase)) { throw 'Migration changed the existing install directory.' }
    if ((Get-FileHash -LiteralPath $installedExe -Algorithm SHA256).Hash -cne $expectedHash -or
        [Diagnostics.FileVersionInfo]::GetVersionInfo($installedExe).FileVersion -cne $expectedFileVersion) {
        throw 'Installed payload does not match the built candidate executable.'
    }
    Assert-Autostart $Autostart
    if ($sentinel -and (!(Test-Path -LiteralPath $sentinel) -or [IO.File]::ReadAllText($sentinel) -cne $sentinelText)) {
        throw 'Installer modified an unrelated WinGet alias.'
    }
}
function Prepare-Manifest([string]$Source, [string]$Setup, [string]$Version, [string]$Stage) {
    $destination = Join-Path $work "manifest-$Stage"
    New-Item -ItemType Directory -Path $destination | Out-Null
    $setupName = "$Stage-setup.exe"
    Copy-Item -LiteralPath $Setup -Destination (Join-Path $serveDirectory $setupName)
    $hash = (Get-FileHash -LiteralPath $Setup -Algorithm SHA256).Hash
    $files = @(Get-ChildItem -LiteralPath $Source -Filter "$packageId*.yaml" -File)
    if ($files.Count -ne 3) { throw 'Expected the three production WinGet manifest files.' }
    foreach ($file in $files) {
        $content = [IO.File]::ReadAllText($file.FullName)
        if ($content -notmatch ('(?m)^PackageIdentifier:\s*' + [regex]::Escape($packageId) + '\s*$') -or
            $content -notmatch ('(?m)^PackageVersion:\s*' + [regex]::Escape($Version) + '\s*$')) {
            throw "Production manifest identity/version mismatch: $($file.Name)"
        }
        if ($file.Name.EndsWith('.installer.yaml')) {
            if ($content -notmatch '(?m)^InstallerType:\s*inno\s*$' -or
                $content -notmatch '(?m)^UpgradeBehavior:\s*install\s*$' -or
                $content -notmatch [regex]::Escape($productCode) -or
                $content -notmatch [regex]::Escape($legacyProductCode)) {
                throw 'Production installer manifest must declare Inno and legacy portable compatibility.'
            }
            if ([regex]::Matches($content, '(?m)^\s*InstallerUrl:').Count -ne 1 -or
                [regex]::Matches($content, '(?m)^\s*InstallerSha256:').Count -ne 1) {
                throw 'This acceptance scenario expects one x64 installer.'
            }
            $content = [regex]::Replace($content, '(?m)^(\s*)InstallerUrl:[^\r\n]*',
                ('${1}InstallerUrl: ' + $script:serverUrl + $setupName))
            $content = [regex]::Replace($content, '(?m)^(\s*)InstallerSha256:[^\r\n]*',
                ('${1}InstallerSha256: ' + $hash))
        }
        [IO.File]::WriteAllText((Join-Path $destination $file.Name), $content)
    }
    $null = Invoke-WinGet @('validate', '--manifest', $destination, '--disable-interactivity') "validate-$Stage"
    return $destination
}
function Build-Fixture([int]$Offset) {
    $fixtureVersion = '{0}.{1}.{2}' -f $version.Major, $version.Minor, ($version.Build + $Offset)
    $output = Join-Path $work "package-$Offset"
    if ($Application -eq 'brightwheel') {
        & $BuildScriptPath -Version $fixtureVersion -BinaryDirectory (Split-Path $ExecutablePath) -OutputDirectory $output | Out-Host
    } else {
        & $BuildScriptPath -Version $fixtureVersion -ExecutablePath $ExecutablePath -OutputDirectory $output | Out-Host
    }
    $setup = Join-Path $output $applicationInfo.Setup
    if (-not (Test-Path -LiteralPath $setup)) { throw 'Production packaging did not produce the expected fixture installer.' }
    return @{ Version = $fixtureVersion; Setup = $setup; Manifests = (Join-Path $output 'winget') }
}
function Upgrade-Package([string]$Manifest, [string]$Stage, [switch]$AllowFailure) {
    return Invoke-WinGet @('upgrade', '--manifest', $Manifest, '--silent',
        '--accept-package-agreements', '--accept-source-agreements', '--disable-interactivity',
        '--log', (Join-Path $logDirectory "$Stage-installer.log"), '--verbose-logs') $Stage -AllowFailure:$AllowFailure
}

$script:configHash = $null
$script:configPath = Join-Path $profileDirectory 'RustSwitcher/config.json'
try {
    New-Item -ItemType Directory -Path $work, $serveDirectory, $logDirectory, $profileDirectory -Force | Out-Null
    foreach ($hive in @([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryHive]::LocalMachine)) {
        $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, [Microsoft.Win32.RegistryView]::Registry64)
        try {
            foreach ($code in @($productCode, $legacyProductCode)) {
                $existing = $base.OpenSubKey("$uninstallRoot\$code")
                if ($null -ne $existing) { $existing.Dispose(); throw "An existing installation must not be changed by this test: $hive/$code" }
            }
        } finally { $base.Dispose() }
    }
    $runSnapshot = Read-RegistryValue $runRegistryPath $applicationInfo.Name
    $pathSnapshot = Read-RegistryValue 'Environment' 'Path'
    if ($Application -eq 'brightwheel') {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\BrightWheel')
        $brightConfigExisted = $null -ne $key
        if ($key) { $key.Dispose() }
        $brightConfiguredSnapshot = Read-RegistryValue 'Software\BrightWheel' 'AutoStartConfigured'
        $brightUpdatesSnapshot = Read-RegistryValue 'Software\BrightWheel' 'AutoUpdateEnabled'
    }
    $snapshotTaken = $true
    $env:APPDATA = $profileDirectory
    $tokenReason = ''
    $fixtureToken = [PackageAcceptanceNative]::LinkedNormalToken([ref]$tokenReason)
    $tokenTest = @{ Runner = [PackageAcceptanceNative]::SnapshotToken($PID); Detail = $tokenReason;
        Mode = $(if ($fixtureToken -ne [IntPtr]::Zero) { 'linked-normal-token' } else { 'runner-token' });
        Limitation = $(if ($fixtureToken -eq [IntPtr]::Zero) {
            'This VM exposes no linked normal token; normal-app/elevated-manager coverage was not exercised. Actual process-token preservation is still asserted.'
        } else { $null }) }
    Write-Host "Process token fixture: $($tokenTest.Mode). $tokenReason"

    # Reuse the pinned client when present. Prefer the registered package path:
    # prepending WindowsApps can shadow the runner's real Python with Store aliases.
    $wingetVersion = '1.29.380'
    $appInstaller = Get-AppxPackage -Name Microsoft.DesktopAppInstaller | Sort-Object Version -Descending | Select-Object -First 1
    if ($appInstaller -and $appInstaller.InstallLocation) {
        $winget = Join-Path $appInstaller.InstallLocation 'winget.exe'
    } else {
        $command = Get-Command winget.exe -ErrorAction SilentlyContinue
        if ($command) { $winget = $command.Source }
    }
    $clientReady = $false
    if ($winget -and (Test-Path -LiteralPath $winget)) {
        $probe = Invoke-Logged $winget @('--version') 'winget-version-before'
        $clientReady = $probe.ExitCode -eq 0 -and $probe.Text.Trim() -eq "v$wingetVersion"
    }
    if (-not $clientReady) {
        $bootstrap = Join-Path (Split-Path (Split-Path $BuildScriptPath)) '.github/scripts/install_winget.ps1'
        if (Test-Path -LiteralPath $bootstrap) {
            & $bootstrap | Out-Host
        } else {
            # Microsoft's documented bootstrap with an explicit module/client pin.
            # https://learn.microsoft.com/windows/package-manager/winget/#install-winget-on-windows-sandbox
            $module = Get-Module -ListAvailable -Name Microsoft.WinGet.Client | Where-Object Version -eq ([Version]$wingetVersion)
            if (-not $module) {
                Install-Module Microsoft.WinGet.Client -RequiredVersion $wingetVersion -Scope CurrentUser -Repository PSGallery -Force
            }
            Import-Module Microsoft.WinGet.Client -RequiredVersion $wingetVersion
            Repair-WinGetPackageManager -Version $wingetVersion -Force
        }
        $appInstaller = Get-AppxPackage -Name Microsoft.DesktopAppInstaller | Sort-Object Version -Descending | Select-Object -First 1
        if (-not $appInstaller -or -not $appInstaller.InstallLocation) { throw 'The pinned WinGet package was not registered.' }
        $winget = Join-Path $appInstaller.InstallLocation 'winget.exe'
        $probe = Invoke-WinGet @('--version') 'winget-version-after'
        if ($probe.Text.Trim() -ne "v$wingetVersion") { throw "Expected WinGet v$wingetVersion, got $($probe.Text)" }
    }
    $settings = Invoke-WinGet @('settings', 'export') 'settings-before'
    $settingsSnapshot = $settings.Text | ConvertFrom-Json -AsHashtable
    if (-not $settingsSnapshot.adminSettings.ContainsKey('LocalManifestFiles')) { throw 'WinGet did not export the LocalManifestFiles setting.' }
    if (-not $settingsSnapshot.adminSettings.LocalManifestFiles) {
        $changedLocalManifestSetting = $true
        $null = Invoke-WinGet @('settings', '--enable', 'LocalManifestFiles', '--disable-interactivity') 'enable-local-manifests'
    }

    # The listener binds to loopback and serves only the generated setup filenames.
    $reservation = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $reservation.Start()
    $port = $reservation.LocalEndpoint.Port
    $reservation.Stop()
    $script:serverUrl = "http://127.0.0.1:$port/"
    $serverLog = Join-Path $logDirectory 'loopback-server.log'
    $serverJob = Start-Job -ArgumentList $script:serverUrl, $serveDirectory, $serverLog -ScriptBlock {
        param($Prefix, $Directory, $LogPath)
        $ErrorActionPreference = 'Stop'
        function Write-ServerLog([string]$Message) {
            [IO.File]::AppendAllText($LogPath,
                ([DateTime]::UtcNow.ToString('o') + ' ' + $Message + [Environment]::NewLine))
        }
        $listener = [Net.HttpListener]::new()
        try {
            $listener.Prefixes.Add($Prefix)
            $listener.Start()
            Write-ServerLog "Listening on $Prefix; serving $Directory"
            while ($listener.IsListening) {
                # A synchronous GetContext blocks Stop-Job indefinitely when
                # no client is connected. Poll async acceptance so PowerShell
                # can stop the job and run the listener's finally block.
                $pending = $listener.GetContextAsync()
                while (-not $pending.Wait(200)) { }
                $context = $pending.GetAwaiter().GetResult()
                try {
                    $name = $context.Request.Url.AbsolutePath.TrimStart('/')
                    $method = $context.Request.HttpMethod
                    if ($method -notin @('GET', 'HEAD')) {
                        $context.Response.StatusCode = 405
                    } elseif ($name -eq 'health') {
                        $bytes = [Text.Encoding]::UTF8.GetBytes('ready')
                        # Without a text Content-Type, PowerShell 7 exposes
                        # Content as byte[], so comparing it with 'ready' fails.
                        $context.Response.ContentType = 'text/plain; charset=utf-8'
                        $context.Response.ContentLength64 = $bytes.Length
                        if ($method -eq 'GET') { $context.Response.OutputStream.Write($bytes, 0, $bytes.Length) }
                    } elseif ($name -cmatch '^[a-z0-9-]+-setup\.exe$' -and
                        (Test-Path -LiteralPath (Join-Path $Directory $name))) {
                        $stream = [IO.File]::OpenRead((Join-Path $Directory $name))
                        try {
                            $context.Response.ContentType = 'application/octet-stream'
                            $context.Response.ContentLength64 = $stream.Length
                            if ($method -eq 'GET') { $stream.CopyTo($context.Response.OutputStream) }
                        } finally { $stream.Dispose() }
                    } else { $context.Response.StatusCode = 404 }
                    Write-ServerLog "$method /$name -> $($context.Response.StatusCode), $($context.Response.ContentLength64) bytes"
                } finally { $context.Response.Close() }
            }
        } catch {
            Write-ServerLog ("Server failure: " + $_.Exception.ToString())
            throw
        } finally { $listener.Close() }
    }
    $ready = $false
    $lastProbe = 'No HTTP response received.'
    $readyDeadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        if ($serverJob.State -notin @('NotStarted', 'Running')) { break }
        try {
            $response = Invoke-WebRequest ($script:serverUrl + 'health') -TimeoutSec 2 -NoProxy
            $lastProbe = "HTTP $($response.StatusCode), content type '$($response.Headers['Content-Type'])', body type $($response.Content.GetType().FullName)"
            $ready = $response.StatusCode -eq 200 -and $response.Content -ceq 'ready'
        } catch { $lastProbe = $_.Exception.ToString() }
        if ($ready) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $readyDeadline)
    if (-not $ready) {
        $diagnostics = @("Job state: $($serverJob.State)", "Last health probe: $lastProbe")
        foreach ($job in @($serverJob) + @($serverJob.ChildJobs)) {
            if ($null -ne $job.JobStateInfo.Reason) { $diagnostics += $job.JobStateInfo.Reason.ToString() }
            $diagnostics += @($job.Error | ForEach-Object { $_.ToString() })
        }
        if (Test-Path -LiteralPath $serverLog) { $diagnostics += Get-Content -LiteralPath $serverLog -Tail 20 }
        throw ("Loopback download server did not become ready.`n" + ($diagnostics -join [Environment]::NewLine))
    }
    Write-Host "Loopback download server ready: $script:serverUrl ($lastProbe)"
    $initialManifest = Prepare-Manifest $ManifestDirectory $InstallerPath $PackageVersion 'migration'

    $installedByTest = $true
    $null = Invoke-WinGet @('install', '--id', $packageId, '--exact', '--version', $LegacyVersion,
        '--source', 'winget', '--scope', 'user', '--location', $installDirectory,
        '--silent', '--accept-package-agreements', '--accept-source-agreements', '--disable-interactivity') 'install-official-legacy'
    $legacy = Read-Arp $legacyProductCode
    if ($null -eq $legacy -or $legacy.WinGetInstallerType -ne 'portable' -or
        $legacy.WinGetPackageIdentifier -cne $packageId -or $legacy.WinGetSourceIdentifier -cne 'Microsoft.Winget.Source_8wekyb3d8bbwe' -or
        $legacy.DisplayVersion -cne $LegacyVersion -or -not [string]::Equals($legacy.TargetFullPath, $installedExe,
            [StringComparison]::OrdinalIgnoreCase)) { throw 'The official baseline did not create the expected portable registration.' }
    $legacyHash = (Get-FileHash -LiteralPath $installedExe -Algorithm SHA256).Hash
    if ($legacyHash -cne ([string]$legacy.SHA256).ToUpperInvariant()) { throw 'Installed baseline hash does not match WinGet catalog registration.' }
    if ($legacyHash -ceq $expectedHash) { throw 'Candidate payload equals the published baseline; this would not prove executable replacement.' }
    $legacyAlias = [string]$legacy.SymlinkFullPath
    if (-not $legacyAlias -or -not (Test-Path -LiteralPath $legacyAlias)) { throw 'Baseline has no working WinGet command alias.' }
    $sentinel = Join-Path (Split-Path $legacyAlias) ('qqrm-unrelated-' + [guid]::NewGuid().ToString('N') + '.exe')
    [IO.File]::WriteAllText($sentinel, $sentinelText)
    if ($Application -eq 'brightwheel') {
        Restore-RegistryValue 'Software\BrightWheel' 'AutoStartConfigured' @{ Exists = $true; Value = 1; Kind = [Microsoft.Win32.RegistryValueKind]::DWord }
        Restore-RegistryValue 'Software\BrightWheel' 'AutoUpdateEnabled' @{ Exists = $true; Value = 0; Kind = [Microsoft.Win32.RegistryValueKind]::DWord }
    }
    Set-AutostartFixture $true
    $before = Start-Installed
    $beforeToken = [PackageAcceptanceNative]::SnapshotToken($before.Id)
    if ($Application -eq 'rust-switcher') { $script:configHash = (Get-FileHash -LiteralPath $script:configPath -Algorithm SHA256).Hash }
    $null = Upgrade-Package $initialManifest 'portable-to-inno'
    if (-not $before.WaitForExit(10000)) { throw 'Legacy process survived the package-manager update.' }
    $current = Wait-Ready $before.Id
    Assert-TokenPreserved $beforeToken $current.Id 'portable-to-inno'
    Assert-Installed $PackageVersion $true
    if ((Get-FileHash -LiteralPath $legacyAlias -Algorithm SHA256).Hash -cne $expectedHash) { throw 'WinGet alias did not resolve to the updated executable.' }
    Record-Pass 'Official portable installation upgrades while running: new payload, new PID, responsive tray, one ARP entry, enabled autostart preserved'

    Stop-Installed
    Set-AutostartFixture $false
    $before = Start-Installed
    $beforeToken = [PackageAcceptanceNative]::SnapshotToken($before.Id)
    $next = Build-Fixture 1
    $manifest = Prepare-Manifest $next.Manifests $next.Setup $next.Version 'running'
    $null = Upgrade-Package $manifest 'inno-running-update'
    if (-not $before.WaitForExit(10000)) { throw 'Prior Inno process survived the update.' }
    $current = Wait-Ready $before.Id
    Assert-TokenPreserved $beforeToken $current.Id 'inno-running-update'
    Assert-Installed $next.Version $false
    Record-Pass 'Inno-to-Inno package update restarts the running process and preserves disabled autostart'

    Stop-Installed
    Set-AutostartFixture $true
    $next = Build-Fixture 2
    $manifest = Prepare-Manifest $next.Manifests $next.Setup $next.Version 'stopped'
    $null = Upgrade-Package $manifest 'inno-stopped-update'
    Assert-Stopped
    Assert-Installed $next.Version $true
    $stored = Read-RegistryValue $runRegistryPath $applicationInfo.Name
    $started = [PackageAcceptanceNative]::StartStoredCommand($stored.Value, $installDirectory, $fixtureToken)
    $current = Wait-Ready
    if ($current.Id -ne $started.Id) { throw 'The actual saved Run command did not start the installed application.' }
    Record-Pass 'Updating a stopped application leaves it stopped; its exact saved Run command still launches a responsive tray'

    $managed = Build-Fixture 3
    $beforeToken = [PackageAcceptanceNative]::SnapshotToken($current.Id)
    & $ManagedUpdaterTestPath -Application $Application -InstalledExecutable $installedExe -InstallerPath $managed.Setup `
        -Version $managed.Version -WorkDirectory (Join-Path $work 'managed-updater') | Out-Host
    $current = Wait-Ready
    Assert-TokenPreserved $beforeToken $current.Id 'managed-updater'
    Assert-Installed $managed.Version $true
    Record-Pass 'The production built-in updater also retains the installer-managed registration'

    $next = Build-Fixture 4
    $manifest = Prepare-Manifest $next.Manifests $next.Setup $next.Version 'locked'
    $beforeToken = [PackageAcceptanceNative]::SnapshotToken($current.Id)
    $fileLock = [IO.File]::Open($installedExe, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $result = Upgrade-Package $manifest 'inno-locked-update' -AllowFailure
        if ($result.ExitCode -eq 0) { throw 'WinGet reported success although the executable could not be replaced.' }
        $installerLog = Join-Path $logDirectory 'inno-locked-update-installer.log'
        if (-not (Test-Path -LiteralPath $installerLog) -or
            [IO.File]::ReadAllText($installerLog) -notmatch [regex]::Escape("$Application.exe")) {
            throw 'Failure occurred before the candidate installer reached the locked application; this is not a rollback test.'
        }
        $current = Wait-Ready
        Assert-TokenPreserved $beforeToken $current.Id 'failed-update-recovery'
        Assert-Installed $managed.Version $true
    } finally { $fileLock.Dispose(); $fileLock = $null }
    Record-Pass 'A locked payload produces a failed update while preserving the previous payload, ARP version, preferences, and working tray'

    Stop-Installed
    $null = Invoke-WinGet @('uninstall', '--product-code', $productCode, '--exact', '--silent',
        '--accept-source-agreements', '--disable-interactivity') 'uninstall-inno'
    if ($null -ne (Read-Arp $productCode) -or $null -ne (Read-Arp $legacyProductCode) -or
        (Test-PathEntry $installedExe) -or (Test-PathEntry $legacyAlias)) {
        throw 'Uninstall left an owned executable, alias, or ARP entry.'
    }
    if ((Read-RegistryValue $runRegistryPath $applicationInfo.Name).Exists) { throw 'Uninstall left its autostart registration.' }
    if (!(Test-Path -LiteralPath $sentinel) -or [IO.File]::ReadAllText($sentinel) -cne $sentinelText) {
        throw 'Uninstall removed an unrelated WinGet alias.'
    }
    if ($Application -eq 'rust-switcher' -and
        (Get-FileHash -LiteralPath $script:configPath -Algorithm SHA256).Hash -cne $script:configHash) {
        throw 'Uninstall removed or changed user configuration.'
    }
    Record-Pass 'WinGet uninstalls the migrated application and its alias without deleting unrelated aliases or user configuration'

    # Reuse the last compiled fixture to cover the target-absent installer path.
    $installDirectory = Join-Path $work 'Fresh installation with spaces'
    $installedExe = Join-Path $installDirectory "$Application.exe"
    $null = Invoke-WinGet @('install', '--manifest', $manifest, '--scope', 'user', '--location', $installDirectory,
        '--silent', '--accept-package-agreements', '--accept-source-agreements', '--disable-interactivity',
        '--log', (Join-Path $logDirectory 'fresh-installer.log')) 'fresh-silent-install'
    Assert-Stopped
    Assert-Installed $next.Version $false
    $fresh = Start-Installed
    $null = Invoke-WinGet @('uninstall', '--product-code', $productCode, '--exact', '--silent',
        '--accept-source-agreements', '--disable-interactivity') 'uninstall-running-fresh'
    if (-not $fresh.WaitForExit(10000)) { throw 'Uninstall did not stop its running application.' }
    Assert-Stopped
    if ($null -ne (Read-Arp $productCode) -or (Test-PathEntry $installedExe) -or
        (Read-RegistryValue $runRegistryPath $applicationInfo.Name).Exists) {
        throw 'Fresh-install uninstall left its executable, ARP entry, or autostart registration.'
    }
    if (!(Test-Path -LiteralPath $sentinel) -or [IO.File]::ReadAllText($sentinel) -cne $sentinelText) {
        throw 'Fresh-install uninstall removed an unrelated WinGet alias.'
    }
    Record-Pass 'A fresh silent installation stays stopped, launches normally, and uninstalls while running'
} catch {
    $failure = $_
} finally {
    if ($null -ne $fileLock) { $fileLock.Dispose() }
    foreach ($process in @(Get-InstalledProcesses)) {
        try { $process.Kill(); $null = $process.WaitForExit(10000) }
        catch { $cleanupErrors.Add("Owned process cleanup: $_") }
    }
    if ($installedByTest -and $winget) {
        foreach ($code in @($productCode, $legacyProductCode)) {
            try {
                $arp = Read-Arp $code
                if ($null -eq $arp) { continue }
                if (-not [string]::Equals(([string]$arp.InstallLocation).TrimEnd('\'), $installDirectory,
                        [StringComparison]::OrdinalIgnoreCase)) { throw 'Refusing cleanup outside the owned install directory.' }
                $result = Invoke-WinGet @('uninstall', '--product-code', $code, '--exact', '--silent', '--force',
                    '--accept-source-agreements', '--disable-interactivity') ('cleanup-' + $code) -AllowFailure
                if ($result.ExitCode -ne 0) { throw $result.Text }
            } catch { $cleanupErrors.Add("Owned installation cleanup: $_") }
        }
    }
    if ($serverJob) { Stop-Job $serverJob -ErrorAction SilentlyContinue; Remove-Job $serverJob -Force -ErrorAction SilentlyContinue }
    if ($sentinel -and (Test-Path -LiteralPath $sentinel)) { Remove-Item -LiteralPath $sentinel -Force }
    if ($changedLocalManifestSetting -and $winget) {
        try { $null = Invoke-WinGet @('settings', '--disable', 'LocalManifestFiles', '--disable-interactivity') 'restore-local-manifests' }
        catch { $cleanupErrors.Add("WinGet setting restore: $_") }
    }
    if ($snapshotTaken) {
        try {
            Restore-RegistryValue $runRegistryPath $applicationInfo.Name $runSnapshot
            Restore-RegistryValue 'Environment' 'Path' $pathSnapshot
            if ($Application -eq 'brightwheel') {
                Restore-RegistryValue 'Software\BrightWheel' 'AutoStartConfigured' $brightConfiguredSnapshot
                Restore-RegistryValue 'Software\BrightWheel' 'AutoUpdateEnabled' $brightUpdatesSnapshot
                if (-not $brightConfigExisted) {
                    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\BrightWheel')
                    $empty = $key -and $key.ValueCount -eq 0 -and $key.SubKeyCount -eq 0
                    if ($key) { $key.Dispose() }
                    if ($empty) { [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKey('Software\BrightWheel', $false) }
                }
            }
        } catch { $cleanupErrors.Add("Preference restore: $_") }
    }
    $env:APPDATA = $appDataSnapshot
    $env:PATH = $processPathSnapshot
    [PackageAcceptanceNative]::ReleaseToken($fixtureToken)
    if (Test-Path -LiteralPath $work) {
        $report = @{ Application = $Application; LegacyVersion = $LegacyVersion; CandidateVersion = $PackageVersion;
            CandidateHash = $expectedHash; Checks = $checks.ToArray(); Commands = $commands.ToArray();
            TokenComparisons = $tokenComparisons.ToArray();
            TokenTest = $tokenTest;
            Failure = $(if ($failure) { $failure.ToString() } else { $null }); CleanupErrors = $cleanupErrors.ToArray();
            Limitation = 'Local candidate manifests do not publish or write the source tracking catalog.' }
        $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $work 'result.json') -Encoding utf8
        Write-Host "Package-manager acceptance report: $work"
    }
}
if ($failure) { throw $failure }
if ($cleanupErrors.Count) { throw ($cleanupErrors -join [Environment]::NewLine) }
