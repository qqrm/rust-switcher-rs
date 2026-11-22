#requires -Version 7.0
<#
Acceptance test for the actual package-manager path on a disposable Windows VM.
The baseline comes from the public WinGet source. Candidate manifests are copied
from the production packaging output; only their download URL and hash change.
A temporary explicit trusted REST source exercises package selection, download,
installer execution, source tracking and installed ARP state. Its metadata comes
from the generated production manifests; it does not publish to the public index.
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
    [string]$SourceServerScriptPath = (Join-Path $PSScriptRoot 'winget-test-source.py'),
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
foreach ($inputPath in @($InstallerPath, $ExecutablePath, $ManifestDirectory, $BuildScriptPath, $ManagedUpdaterTestPath,
    $SourceServerScriptPath, (Join-Path $PSScriptRoot 'winget-trusted-source.ps1'))) {
    if (-not (Test-Path -LiteralPath $inputPath)) { throw "Required input does not exist: $inputPath" }
}
$InstallerPath = (Resolve-Path -LiteralPath $InstallerPath).Path
$ExecutablePath = (Resolve-Path -LiteralPath $ExecutablePath).Path
$ManifestDirectory = (Resolve-Path -LiteralPath $ManifestDirectory).Path
$BuildScriptPath = (Resolve-Path -LiteralPath $BuildScriptPath).Path
$ManagedUpdaterTestPath = (Resolve-Path -LiteralPath $ManagedUpdaterTestPath).Path
$SourceServerScriptPath = (Resolve-Path -LiteralPath $SourceServerScriptPath).Path
. (Join-Path $PSScriptRoot 'winget-trusted-source.ps1')
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
$wingetLogDirectory = $null
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
$trustedSource = $null
$trustedSourceEvidence = [Collections.Generic.List[object]]::new()
$activeDiagnosticManifest = $null

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
    public sealed class WindowSnapshot {
        public int ProcessId { get; set; }
        public string Handle { get; set; }
        public string TopLevelHandle { get; set; }
        public string ClassName { get; set; }
        public string Text { get; set; }
        public bool Visible { get; set; }
        public bool Enabled { get; set; }
        public bool TextResponsive { get; set; }
        public string OwnerHandle { get; set; }
        public string EnabledPopupHandle { get; set; }
        public string LastActivePopupHandle { get; set; }
        public string[] OwnerChain { get; set; }
    }
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
    private delegate bool EnumWindowCallback(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowCallback callback, IntPtr parameter);
    [DllImport("user32.dll")]
    private static extern bool EnumChildWindows(IntPtr parent, EnumWindowCallback callback, IntPtr parameter);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    private static extern int GetClassName(IntPtr window, StringBuilder text, int capacity);
    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")]
    private static extern bool IsWindowEnabled(IntPtr window);
    [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll")] private static extern IntPtr GetLastActivePopup(IntPtr window);
    [DllImport("user32.dll")] private static extern int GetSystemMetrics(int index);
    [DllImport("kernel32.dll")] private static extern bool ProcessIdToSessionId(uint processId, out uint sessionId);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern IntPtr LoadLibraryEx(string file, IntPtr reserved, uint flags);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern IntPtr FindResource(IntPtr module, IntPtr name, IntPtr type);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern uint SizeofResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll")] private static extern IntPtr LockResource(IntPtr resource);
    [DllImport("kernel32.dll")] private static extern bool FreeLibrary(IntPtr module);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, ExactSpelling=true)]
    private static extern IntPtr ReadWindowText(IntPtr window, uint message, UIntPtr capacity,
        StringBuilder text, uint flags, uint timeout, out UIntPtr result);
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
                // ASSIGN_PRIMARY | DUPLICATE | QUERY | ADJUST_DEFAULT | ADJUST_SESSIONID.
                // These are handle access rights; the token's identity and privileges are unchanged.
                const uint retainedPrimaryTokenAccess = 0x0000018B;
                if (!DuplicateTokenEx(linked, retainedPrimaryTokenAccess, IntPtr.Zero, 2, 1, out duplicate))
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
    public static WindowSnapshot[] DescribeOwnedWindows(int[] processIds) {
        var owners = new HashSet<uint>(processIds.Select(id => checked((uint)id)));
        var windows = new List<WindowSnapshot>();
        var elapsed = Stopwatch.StartNew();
        Func<IntPtr, IntPtr, bool> capture = delegate(IntPtr window, IntPtr topLevel) {
            if (elapsed.ElapsedMilliseconds >= 3000 || windows.Count >= 80) return false;
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (!owners.Contains(owner)) return true;
            var className = new StringBuilder(256);
            GetClassName(window, className, className.Capacity);
            var text = new StringBuilder(2048);
            UIntPtr result;
            bool responsive = ReadWindowText(window, 0x000D, new UIntPtr((uint)text.Capacity),
                text, 2, 100, out result) != IntPtr.Zero;
            windows.Add(new WindowSnapshot { ProcessId = checked((int)owner),
                Handle = "0x" + window.ToInt64().ToString("x"),
                TopLevelHandle = "0x" + topLevel.ToInt64().ToString("x"),
                ClassName = className.ToString(), Text = text.ToString(),
                Visible = IsWindowVisible(window), Enabled = IsWindowEnabled(window),
                TextResponsive = responsive });
            return true;
        };
        EnumWindows(delegate(IntPtr window, IntPtr parameter) {
            if (elapsed.ElapsedMilliseconds >= 3000 || windows.Count >= 80) return false;
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (!owners.Contains(owner)) return true;
            if (!capture(window, window)) return false;
            EnumChildWindows(window, delegate(IntPtr child, IntPtr ignored) { return capture(child, window); }, IntPtr.Zero);
            return true;
        }, IntPtr.Zero);
        return windows.ToArray();
    }
    public static WindowSnapshot[] DescribeVisibleSessionWindows(int sessionId) {
        var windows = new List<WindowSnapshot>();
        var elapsed = Stopwatch.StartNew();
        Func<IntPtr, IntPtr, bool> capture = delegate(IntPtr window, IntPtr topLevel) {
            if (elapsed.ElapsedMilliseconds >= 3000 || windows.Count >= 100) return false;
            if (!IsWindowVisible(window)) return true;
            uint processId, windowSession;
            GetWindowThreadProcessId(window, out processId);
            if (!ProcessIdToSessionId(processId, out windowSession) || windowSession != (uint)sessionId) return true;
            var className = new StringBuilder(256);
            GetClassName(window, className, className.Capacity);
            var text = new StringBuilder(2048);
            UIntPtr result;
            bool responsive = ReadWindowText(window, 0x000D, new UIntPtr((uint)text.Capacity),
                text, 2, 100, out result) != IntPtr.Zero;
            var chain = new List<string>();
            var seen = new HashSet<IntPtr>();
            var owner = GetWindow(window, 4); // GW_OWNER, which can cross process boundaries.
            for (var current = owner; current != IntPtr.Zero && chain.Count < 12 && seen.Add(current); current = GetWindow(current, 4)) {
                uint ownerProcess;
                GetWindowThreadProcessId(current, out ownerProcess);
                chain.Add("0x" + current.ToInt64().ToString("x") + " pid=" + ownerProcess);
            }
            windows.Add(new WindowSnapshot { ProcessId = checked((int)processId),
                Handle = "0x" + window.ToInt64().ToString("x"),
                TopLevelHandle = "0x" + topLevel.ToInt64().ToString("x"),
                ClassName = className.ToString(), Text = text.ToString(),
                Visible = true, Enabled = IsWindowEnabled(window), TextResponsive = responsive,
                OwnerHandle = "0x" + owner.ToInt64().ToString("x"), OwnerChain = chain.ToArray(),
                EnabledPopupHandle = "0x" + GetWindow(window, 6).ToInt64().ToString("x"),
                LastActivePopupHandle = "0x" + GetLastActivePopup(window).ToInt64().ToString("x") });
            return true;
        };
        EnumWindows(delegate(IntPtr window, IntPtr parameter) {
            if (!capture(window, window)) return false;
            if (IsWindowVisible(window))
                EnumChildWindows(window, delegate(IntPtr child, IntPtr ignored) { return capture(child, window); }, IntPtr.Zero);
            return true;
        }, IntPtr.Zero);
        return windows.ToArray();
    }
    public static int[] DesktopBounds() {
        return new int[] { GetSystemMetrics(76), GetSystemMetrics(77), GetSystemMetrics(78), GetSystemMetrics(79) };
    }
    public static string ReadEmbeddedManifest(string path) {
        // Map resource data only: do not execute the installer or resolve imports.
        var module = LoadLibraryEx(path, IntPtr.Zero, 0x0040 | 0x0020);
        if (module == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
        try {
            var resource = FindResource(module, new IntPtr(1), new IntPtr(24)); // RT_MANIFEST / CREATEPROCESS_MANIFEST_RESOURCE_ID
            if (resource == IntPtr.Zero) return null;
            uint size = SizeofResource(module, resource);
            if (size == 0 || size > 1024 * 1024) throw new InvalidOperationException("Unexpected PE manifest size.");
            var address = LockResource(LoadResource(module, resource));
            if (address == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
            var bytes = new byte[(int)size];
            Marshal.Copy(address, bytes, 0, bytes.Length);
            var encoding = bytes.Length >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe ? Encoding.Unicode : Encoding.UTF8;
            return encoding.GetString(bytes).TrimStart('\uFEFF').TrimEnd('\0');
        } finally { FreeLibrary(module); }
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
function Read-SharedTextFile([string]$Path) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read,
        ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
    $reader = [IO.StreamReader]::new($stream)
    try { return $reader.ReadToEnd() }
    finally { $reader.Dispose() }
}
function Read-InstallerSignature([string]$Path) {
    # Certificate verification can consult providers. Bound this read-only probe
    # in its own process so diagnostics cannot delay cleanup indefinitely.
    $scriptText = @'
$signature = Get-AuthenticodeSignature -LiteralPath '__INSTALLER_PATH__' -ErrorAction Stop
[ordered]@{
    Status = [string]$signature.Status
    StatusMessage = $signature.StatusMessage
    SignatureType = [string]$signature.SignatureType
    SignerSubject = $(if ($null -ne $signature.SignerCertificate) { $signature.SignerCertificate.Subject })
    SignerThumbprint = $(if ($null -ne $signature.SignerCertificate) { $signature.SignerCertificate.Thumbprint })
} | ConvertTo-Json -Compress
'@
    $scriptText = $scriptText.Replace('__INSTALLER_PATH__', $Path.Replace("'", "''"))
    $info = [Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'))
    $info.UseShellExecute = $false; $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    foreach ($argument in @('-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand',
        [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($scriptText)))) { $info.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($info)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(5000)) {
            $process.Kill($true); $null = $process.WaitForExit(2000)
            throw 'Authenticode inspection exceeded five seconds.'
        }
        if ($process.ExitCode -ne 0) { throw $stderr.GetAwaiter().GetResult() }
        return $stdout.GetAwaiter().GetResult() | ConvertFrom-Json
    } finally { $process.Dispose() }
}
function Get-StagedInstallerEvidence([string]$ManifestPath, [DateTime]$StartedUtc, [string]$Destination) {
    $evidence = [ordered]@{ Path = $null; ExpectedSha256 = $null; ActualSha256 = $null; Size = $null;
        HashMatches = $false; ZoneIdentifier = $null; Authenticode = $null; EmbeddedManifest = $null;
        PreservedInstaller = $null; Error = $null; ZoneError = $null; SignatureError = $null; ManifestError = $null }
    try {
        if (-not [IO.Path]::GetFullPath($ManifestPath).StartsWith($work + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'The diagnostic manifest is outside the owned fixture directory.'
        }
        $files = @(Get-ChildItem -LiteralPath $ManifestPath -Filter '*.installer.yaml' -File)
        if ($files.Count -ne 1) { throw 'Expected exactly one fixture installer manifest.' }
        $yaml = [IO.File]::ReadAllText($files[0].FullName)
        $fields = @{}
        foreach ($name in @('PackageIdentifier', 'PackageVersion', 'InstallerUrl', 'InstallerSha256')) {
            $values = [regex]::Matches($yaml, '(?m)^\s*' + $name + ':\s*([^\r\n]+)\s*$')
            if ($values.Count -ne 1) { throw "Expected exactly one $name in the fixture manifest." }
            $fields[$name] = $values[0].Groups[1].Value.Trim().Trim([char[]]@('"', "'"))
        }
        if ($fields.PackageIdentifier -cne $packageId -or $fields.PackageVersion -notmatch '^\d+\.\d+\.\d+$' -or
            $fields.InstallerSha256 -notmatch '^[a-fA-F0-9]{64}$') { throw 'Unexpected fixture manifest identity or hash.' }
        $uri = [uri]$fields.InstallerUrl
        if ($uri.Scheme -ne 'http' -or $uri.Host -ne '127.0.0.1') { throw 'Expected the owned loopback fixture URL.' }
        $filename = [IO.Path]::GetFileNameWithoutExtension($uri.AbsolutePath) + '.exe'
        if ($filename -notmatch '^[a-zA-Z0-9_.-]+\.exe$') { throw 'Unexpected fixture installer filename.' }
        $directoryName = $fields.PackageIdentifier + '.' + $fields.PackageVersion
        $paths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($file in @(Get-ChildItem -LiteralPath $wingetLogDirectory -Filter '*.log' -File |
            Where-Object { $_.LastWriteTimeUtc -ge $StartedUtc -and $_.Length -le 4MB } |
            Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 8)) {
            foreach ($match in [regex]::Matches((Read-SharedTextFile $file.FullName), "Starting: '([^']+)' with arguments")) {
                $path = $match.Groups[1].Value
                if ([IO.Path]::GetFileName($path) -ceq $filename -and
                    [IO.Path]::GetFileName([IO.Path]::GetDirectoryName($path)) -ceq $directoryName) { $null = $paths.Add($path) }
            }
        }
        if ($paths.Count -ne 1) { throw 'Could not identify one exact staged fixture installer from the new WinGet trace.' }
        $path = @($paths)[0]
        $evidence.Path = $path
        $evidence.ExpectedSha256 = $fields.InstallerSha256
        $evidence.Size = (Get-Item -LiteralPath $path).Length
        if ($evidence.Size -gt 128MB) { throw 'Staged installer exceeds the bounded diagnostic artifact size.' }
        $evidence.ActualSha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        $evidence.HashMatches = $evidence.ActualSha256 -ieq $evidence.ExpectedSha256
        if (-not $evidence.HashMatches) { throw 'Refusing to preserve an installer whose hash differs from the fixture manifest.' }
        $preserved = [IO.Path]::ChangeExtension($Destination, '.setup.exe')
        Copy-Item -LiteralPath $path -Destination $preserved
        $evidence.PreservedInstaller = $preserved
        try {
            if ($null -ne (Get-Item -LiteralPath $path -Stream Zone.Identifier -ErrorAction SilentlyContinue)) {
                $evidence.ZoneIdentifier = Get-Content -LiteralPath $path -Stream Zone.Identifier -Raw
            }
        } catch { $evidence.ZoneError = $_.Exception.Message }
        try { $evidence.Authenticode = Read-InstallerSignature $path }
        catch { $evidence.SignatureError = $_.Exception.Message }
        try {
            $manifest = [PackageAcceptanceNative]::ReadEmbeddedManifest($path)
            if ($null -ne $manifest) {
                $manifestPath = [IO.Path]::ChangeExtension($Destination, '.manifest.xml')
                [IO.File]::WriteAllText($manifestPath, $manifest)
                $evidence.EmbeddedManifest = $manifestPath
            }
        } catch { $evidence.ManifestError = $_.Exception.Message }
    } catch { $evidence.Error = $_.Exception.ToString() }
    return $evidence
}
function Save-OwnedProcessDiagnostics([Diagnostics.Process]$RootProcess, [string]$Destination, [string]$InstallerManifestPath) {
    $diagnostic = [ordered]@{ RootProcessId = $RootProcess.Id; CapturedUtc = [DateTime]::UtcNow;
        Processes = @(); Windows = @(); Error = $null; SessionId = $null; VisibleSessionWindows = @();
        ShellProcesses = @(); SessionError = $null; Screenshot = $null; ScreenshotError = $null; StagedInstaller = $null }
    try {
        # Inventory only parent/PID/creation metadata first. Command lines are
        # fetched only for the root we started and its verified descendants.
        $snapshot = @(Get-CimInstance -ClassName Win32_Process -Property ProcessId, ParentProcessId, CreationDate -OperationTimeoutSec 2)
        $root = $snapshot | Where-Object ProcessId -eq $RootProcess.Id | Select-Object -First 1
        if ($null -eq $root -or $RootProcess.HasExited -or
            [Math]::Abs(($root.CreationDate.ToUniversalTime() - $RootProcess.StartTime.ToUniversalTime()).TotalSeconds) -gt 1) {
            throw 'The original process exited before its owned tree could be inspected.'
        }
        $owned = @{}; $owned[[int]$root.ProcessId] = $root
        do {
            $added = $false
            foreach ($entry in $snapshot) {
                $parent = [int]$entry.ParentProcessId
                if ($owned.Count -ge 64) { break }
                if ($owned.ContainsKey($parent) -and -not $owned.ContainsKey([int]$entry.ProcessId) -and
                    $entry.CreationDate -ge $owned[$parent].CreationDate) {
                    $owned[[int]$entry.ProcessId] = $entry
                    $added = $true
                }
            }
        } while ($added -and $owned.Count -lt 64)
        $filter = ($owned.Keys | ForEach-Object { "ProcessId=$($_)" }) -join ' OR '
        $details = @(Get-CimInstance -ClassName Win32_Process -Filter $filter -OperationTimeoutSec 2 |
            Where-Object { $owned.ContainsKey([int]$_.ProcessId) -and $_.CreationDate -eq $owned[[int]$_.ProcessId].CreationDate } |
            Select-Object ProcessId, ParentProcessId, Name, ExecutablePath, CommandLine, CreationDate, SessionId)
        $diagnostic.Processes = $details
        if ($details.Count) { $diagnostic.Windows = [PackageAcceptanceNative]::DescribeOwnedWindows([int[]]@($details.ProcessId)) }
    } catch { $diagnostic.Error = $_.Exception.ToString() }
    try {
        # Broker dialogs may live outside the process tree. Broader desktop
        # evidence is permitted only on this explicitly disposable test VM.
        if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
            $env:RUNNER_OS -ne 'Windows' -or -not [Environment]::UserInteractive) {
            throw 'Session diagnostics require the disposable interactive GitHub-hosted Windows runner.'
        }
        $sessionId = (Get-Process -Id $PID).SessionId
        if ($sessionId -eq 0 -or $RootProcess.SessionId -ne $sessionId) { throw 'The WinGet process is not in the current interactive session.' }
        $diagnostic.SessionId = $sessionId
        $diagnostic.VisibleSessionWindows = [PackageAcceptanceNative]::DescribeVisibleSessionWindows($sessionId)
        $names = @('smartscreen.exe', 'explorer.exe', 'consent.exe', 'dllhost.exe', 'RuntimeBroker.exe',
            'ApplicationFrameHost.exe', 'OpenWith.exe', 'sihost.exe', 'SecurityHealthSystray.exe')
        $filter = "SessionId=$sessionId AND (" + (($names | ForEach-Object { "Name='$_'" }) -join ' OR ') + ')'
        $brokerProperties = @('ProcessId', 'ParentProcessId', 'Name', 'ExecutablePath', 'CreationDate', 'SessionId')
        $diagnostic.ShellProcesses = @(Get-CimInstance -ClassName Win32_Process -Filter $filter -Property $brokerProperties -OperationTimeoutSec 2 |
            Select-Object ProcessId, ParentProcessId, Name, ExecutablePath, CreationDate, SessionId)
        try {
            Add-Type -AssemblyName System.Drawing
            $bounds = [PackageAcceptanceNative]::DesktopBounds()
            if ($bounds[2] -le 0 -or $bounds[3] -le 0 -or [long]$bounds[2] * $bounds[3] -gt 16000000) {
                throw 'The interactive desktop dimensions exceed the screenshot limit.'
            }
            $bitmap = [Drawing.Bitmap]::new($bounds[2], $bounds[3])
            try {
                $graphics = [Drawing.Graphics]::FromImage($bitmap)
                try { $graphics.CopyFromScreen($bounds[0], $bounds[1], 0, 0, $bitmap.Size) }
                finally { $graphics.Dispose() }
                $screenshotPath = [IO.Path]::ChangeExtension($Destination, '.desktop.png')
                $bitmap.Save($screenshotPath, [Drawing.Imaging.ImageFormat]::Png)
                $diagnostic.Screenshot = $screenshotPath
            } finally { $bitmap.Dispose() }
        } catch { $diagnostic.ScreenshotError = $_.Exception.ToString() }
    } catch { $diagnostic.SessionError = $_.Exception.ToString() }
    if ($InstallerManifestPath) {
        $diagnostic.StagedInstaller = Get-StagedInstallerEvidence $InstallerManifestPath $RootProcess.StartTime.ToUniversalTime() $Destination
    }
    $diagnostic | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $Destination -Encoding utf8
}
function Invoke-Logged([string]$File, [string[]]$Arguments, [string]$Label, [int]$TimeoutSeconds = 180, [switch]$CaptureDiagnostics) {
    $info = [Diagnostics.ProcessStartInfo]::new($File)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $prefix = Join-Path $logDirectory (('{0:D2}-' -f $commands.Count) + $Label)
    $log = "$prefix.log"
    $stdoutPath = "$prefix.stdout.log"
    $stderrPath = "$prefix.stderr.log"
    $stdoutFile = [IO.File]::Open($stdoutPath, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $stderrFile = [IO.File]::Open($stderrPath, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $process = $null
    $timedOut = $false
    $captureError = $null
    $diagnosticsPath = $null
    try {
        $process = [Diagnostics.Process]::Start($info)
        # Persist output while the command is running, not only after a normal
        # return. A timeout must leave useful stdout/stderr artifacts behind.
        $stdout = $process.StandardOutput.BaseStream.CopyToAsync($stdoutFile)
        $stderr = $process.StandardError.BaseStream.CopyToAsync($stderrFile)
        try {
            if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
                $timedOut = $true
                if ($CaptureDiagnostics -or ($winget -and [string]::Equals($File, $winget, [StringComparison]::OrdinalIgnoreCase))) {
                    $diagnosticsPath = "$prefix-timeout.json"
                    $manifestIndex = [Array]::IndexOf($Arguments, '--manifest')
                    $diagnosticManifest = if ($manifestIndex -ge 0 -and $manifestIndex + 1 -lt $Arguments.Length) {
                        $Arguments[$manifestIndex + 1]
                    } else { $activeDiagnosticManifest }
                    try { Save-OwnedProcessDiagnostics $process $diagnosticsPath $diagnosticManifest }
                    catch { $captureError = 'Timeout diagnostics failed: ' + $_.Exception.ToString() }
                }
                if (-not $process.HasExited) { $process.Kill($true) }
                if (-not $process.WaitForExit(5000)) { throw 'The timed-out process tree did not stop.' }
            }
        } catch { $captureError = $_.Exception.ToString() }
        try {
            if (-not [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($stdout, $stderr)).Wait(5000)) {
                throw 'Redirected command output did not close after process exit.'
            }
        } catch {
            $captureError = ($captureError, $_.Exception.ToString() | Where-Object { $_ }) -join [Environment]::NewLine
        } finally {
            $process.StandardOutput.Close()
            $process.StandardError.Close()
            $stdoutFile.Dispose()
            $stderrFile.Dispose()
        }
        $text = [IO.File]::ReadAllText($stdoutPath) + [IO.File]::ReadAllText($stderrPath)
        if ($captureError) { $text += "`nHarness diagnostic: $captureError`n" }
        [IO.File]::WriteAllText($log, $text)
        $exitCode = $(if ($process.HasExited) { $process.ExitCode } else { $null })
        $commands.Add(@{ Label = $Label; Arguments = $Arguments; ExitCode = $exitCode; TimedOut = $timedOut;
            Log = $log; StdoutLog = $stdoutPath; StderrLog = $stderrPath; TimeoutDiagnostics = $diagnosticsPath })
        Write-Host "$Label returned $exitCode; timed out: $timedOut; log: $log"
        if ($timedOut) { throw "Timed out after ${TimeoutSeconds}s: $Label; output: $log; owned process diagnostics: $diagnosticsPath" }
        if ($captureError) { throw "Could not complete $Label`: $captureError" }
        return @{ ExitCode = $exitCode; Text = $text; Log = $log }
    } finally {
        $stdoutFile.Dispose(); $stderrFile.Dispose()
        if ($null -ne $process) { $process.Dispose() }
    }
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
        elseif ($Application -eq 'rust-switcher') { $quitArguments += @('--parent-pid', "$($process.Id)") }
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
    if ($null -ne $trustedSource) { Add-TestSourceVersion $trustedSource $destination $Version }
    return $destination
}
function Read-FixtureManifest([string]$Manifest) {
    $files = @(Get-ChildItem -LiteralPath $Manifest -Filter '*.installer.yaml' -File)
    if ($files.Count -ne 1) { throw 'Expected one generated installer manifest.' }
    $yaml = [IO.File]::ReadAllText($files[0].FullName)
    $result = @{}
    foreach ($name in @('PackageIdentifier', 'PackageVersion', 'InstallerUrl', 'InstallerSha256')) {
        $matches = [regex]::Matches($yaml, '(?m)^\s*' + $name + ':\s*([^\r\n]+)\s*$')
        if ($matches.Count -ne 1) { throw "Expected one $name in the generated installer manifest." }
        $result[$name] = $matches[0].Groups[1].Value.Trim().Trim([char[]]@('"', "'"))
    }
    if ($result.PackageIdentifier -cne $packageId -or $result.PackageVersion -notmatch '^\d+\.\d+\.\d+$' -or
        $result.InstallerSha256 -notmatch '^[a-fA-F0-9]{64}$') { throw 'The fixture manifest identity/hash is invalid.' }
    return $result
}
function Assert-TrustedInstallTrace([string]$Manifest, [string]$StageLogs, [string]$Stage) {
    $fixture = Read-FixtureManifest $Manifest
    $sourceMarker = 'Named source requested, found: ' + $trustedSource.Name
    $traces = @(Get-ChildItem -LiteralPath $StageLogs -Filter '*.log' -File | ForEach-Object {
        $text = Read-SharedTextFile $_.FullName
        if ($text.Contains($sourceMarker)) { @{ Path = $_.FullName; Text = $text } }
    })
    if ($traces.Count -ne 1) { throw "$Stage did not yield one trace selecting the exact trusted fixture source." }
    $trace = $traces[0].Text
    $selectedIndex = $trace.IndexOf('Selected installer URL: ' + $fixture.InstallerUrl, [StringComparison]::Ordinal)
    $hashIndex = $trace.IndexOf('Installer hash verified', [StringComparison]::Ordinal)
    $zone = [regex]::Match($trace, 'Started applying motw to ([^\r\n]+) with zone: 2(?:\r?\n|$)')
    $startIndex = $trace.IndexOf('Starting: ', [StringComparison]::Ordinal)
    $expectedFilename = [IO.Path]::GetFileNameWithoutExtension(([uri]$fixture.InstallerUrl).AbsolutePath) + '.exe'
    if ($selectedIndex -lt 0 -or $hashIndex -lt $selectedIndex -or -not $zone.Success -or
        $zone.Index -lt $hashIndex -or $startIndex -lt $zone.Index -or
        [IO.Path]::GetFileName($zone.Groups[1].Value.Trim('"')) -cne $expectedFilename -or
        [IO.Path]::GetFileName([IO.Path]::GetDirectoryName($zone.Groups[1].Value.Trim('"'))) -cne ($packageId + '.' + $fixture.PackageVersion) -or
        $trace -notmatch [regex]::Escape($fixture.InstallerSha256.ToLowerInvariant())) {
        throw "$Stage did not prove source selection, expected installer SHA verification, and Zone 2 before launch."
    }
    $trustedSourceEvidence.Add(@{ Stage = $Stage; Name = $trustedSource.Name; Identifier = $trustedSource.Identifier;
        Version = $fixture.PackageVersion; InstallerUrl = $fixture.InstallerUrl; InstallerSha256 = $fixture.InstallerSha256;
        Registration = $trustedSource.Registration; HashVerifiedBeforeTrustedZone = $true; Trace = $traces[0].Path })
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
    # Use the same trusted-source path as published packages. WinGet itself
    # verifies the installer hash and assigns Zone 2 before ShellExecute.
    $fixture = Read-FixtureManifest $Manifest
    $null = Assert-TestSourceRegistration $trustedSource ("source-before-$Stage")
    $priorLogs = @{}
    foreach ($file in @(Get-ChildItem -LiteralPath $wingetLogDirectory -Filter '*.log' -File)) {
        $priorLogs[$file.FullName] = "$($file.Length):$($file.LastWriteTimeUtc.Ticks)"
    }
    $script:activeDiagnosticManifest = $Manifest
    try {
        $result = Invoke-WinGet @('upgrade', '--source', $trustedSource.Name, '--id', $packageId, '--exact',
            '--version', $fixture.PackageVersion, '--scope', 'user', '--silent',
            '--accept-package-agreements', '--accept-source-agreements', '--disable-interactivity',
            '--verbose-logs') $Stage -AllowFailure
    } finally {
        $script:activeDiagnosticManifest = $null
        # Invoke-Logged may throw on timeout. Still retain the client's trace
        # and any partial Inno log after the owned process tree has been stopped.
        $stageLogs = Join-Path $logDirectory "$Stage-winget"
        $null = New-Item -ItemType Directory -Path $stageLogs
        $installerLogs = @()
        foreach ($file in @(Get-ChildItem -LiteralPath $wingetLogDirectory -Filter '*.log' -File)) {
            $stamp = "$($file.Length):$($file.LastWriteTimeUtc.Ticks)"
            if ($priorLogs.ContainsKey($file.FullName) -and $priorLogs[$file.FullName] -ceq $stamp) { continue }
            $captured = Join-Path $stageLogs $file.Name
            Copy-Item -LiteralPath $file.FullName -Destination $captured
            $content = [IO.File]::ReadAllText($captured)
            if ($content -match 'Setup version: Inno Setup' -and $content -match [regex]::Escape($installedExe)) {
                $installerLogs += $captured
            }
        }
        if ($installerLogs.Count -gt 1) { throw "More than one Inno log targeted the owned executable during $Stage." }
        if ($installerLogs.Count -eq 1) {
            Copy-Item -LiteralPath $installerLogs[0] -Destination (Join-Path $logDirectory "$Stage-installer.log")
        }
        Write-Host "Captured default WinGet logs for ${Stage}: $stageLogs"
    }
    Assert-TrustedInstallTrace $Manifest $stageLogs $Stage
    if ($result.ExitCode -ne 0 -and -not $AllowFailure) { throw "$Stage failed: $($result.Text)" }
    return $result
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
    # DefaultLogLocation in the pinned client's Runtime.cpp. The packaged
    # client uses its own LocalState; the unpackaged client uses TEMP/WinGet.
    $wingetLogDirectory = if ($appInstaller) {
        Join-Path $env:LOCALAPPDATA "Packages/$($appInstaller.PackageFamilyName)/LocalState/DiagOutputDir"
    } else {
        Join-Path ([IO.Path]::GetTempPath()) 'WinGet/defaultState'
    }
    if (-not (Test-Path -LiteralPath $wingetLogDirectory -PathType Container)) {
        throw "The pinned client's default log directory was not found: $wingetLogDirectory"
    }
    $settingsSnapshot = $settings.Text | ConvertFrom-Json -AsHashtable

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
        function Test-TransferInterruption([Exception]$Exception) {
            while ($null -ne $Exception) {
                if ($Exception -is [Net.HttpListenerException] -or $Exception -is [IO.IOException]) { return $true }
                $Exception = $Exception.InnerException
            }
            return $false
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
                    $rangeHeader = [string]$context.Request.Headers['Range']
                    Write-ServerLog "Request $method /$name; Range='$rangeHeader'; User-Agent='$($context.Request.UserAgent)'"
                    if ($method -notin @('GET', 'HEAD')) {
                        $context.Response.StatusCode = 405
                        $context.Response.ContentLength64 = 0
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
                            $context.Response.Headers['Accept-Ranges'] = 'bytes'
                            $modified = [IO.File]::GetLastWriteTimeUtc((Join-Path $Directory $name))
                            $etag = '"{0:x}-{1:x}"' -f $stream.Length, $modified.Ticks
                            $lastModified = $modified.ToString('R', [Globalization.CultureInfo]::InvariantCulture)
                            $context.Response.Headers['ETag'] = $etag
                            $context.Response.Headers['Last-Modified'] = $lastModified
                            [long]$start = 0
                            [long]$end = $stream.Length - 1
                            $valid = $true
                            $ifRange = [string]$context.Request.Headers['If-Range']
                            if ($method -eq 'GET' -and $rangeHeader -and
                                (-not $ifRange -or $ifRange -ceq $etag -or $ifRange -ceq $lastModified)) {
                                # Delivery Optimization probes and downloads in ranges.
                                # Return exactly the requested bytes, not the entire file.
                                $range = [regex]::Match($rangeHeader.Trim(), '^bytes=(\d*)-(\d*)$')
                                if (-not $range.Success -or $stream.Length -eq 0) { $valid = $false }
                                elseif (-not $range.Groups[1].Value) {
                                    [long]$suffix = 0
                                    $valid = [long]::TryParse($range.Groups[2].Value, [ref]$suffix) -and $suffix -gt 0
                                    if ($valid) { $start = [Math]::Max(0L, $stream.Length - $suffix) }
                                } else {
                                    $valid = [long]::TryParse($range.Groups[1].Value, [ref]$start)
                                    if ($valid -and $range.Groups[2].Value) {
                                        $valid = [long]::TryParse($range.Groups[2].Value, [ref]$end)
                                    }
                                    $end = [Math]::Min($end, $stream.Length - 1)
                                    $valid = $valid -and $start -lt $stream.Length -and $end -ge $start
                                }
                                if ($valid) {
                                    $context.Response.StatusCode = 206
                                    $context.Response.Headers['Content-Range'] = "bytes $start-$end/$($stream.Length)"
                                } else {
                                    $context.Response.StatusCode = 416
                                    $context.Response.Headers['Content-Range'] = "bytes */$($stream.Length)"
                                }
                            }
                            $context.Response.ContentLength64 = $(if ($valid) { $end - $start + 1 } else { 0 })
                            Write-ServerLog "Response $method /$name -> $($context.Response.StatusCode), $($context.Response.ContentLength64) bytes"
                            if ($method -eq 'GET' -and $valid) {
                                $null = $stream.Seek($start, [IO.SeekOrigin]::Begin)
                                $buffer = [byte[]]::new(65536)
                                [long]$remaining = $end - $start + 1
                                while ($remaining -gt 0) {
                                    $count = $stream.Read($buffer, 0, [int][Math]::Min($remaining, $buffer.Length))
                                    if ($count -eq 0) { throw 'The staged installer changed during its download.' }
                                    $context.Response.OutputStream.Write($buffer, 0, $count)
                                    $remaining -= $count
                                }
                            }
                        } finally { $stream.Dispose() }
                    } else {
                        $context.Response.StatusCode = 404
                        $context.Response.ContentLength64 = 0
                    }
                    Write-ServerLog "Completed $method /$name -> $($context.Response.StatusCode), $($context.Response.ContentLength64) bytes"
                } catch {
                    if (-not (Test-TransferInterruption $_.Exception)) { throw }
                    # A downloader may cancel a probe or range. That connection
                    # is finished; the listener must remain available for retry.
                    Write-ServerLog ("Transfer interrupted for $method /$name`: " + $_.Exception.ToString())
                    try { $context.Response.Abort() } catch { }
                } finally {
                    try { $context.Response.Close() }
                    catch { Write-ServerLog ("Response close failed: " + $_.Exception.Message) }
                }
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
    $trustedSource = New-TestSourceState ([IO.Path]::GetTempPath()) $Application
    Add-TestSourceVersion $trustedSource $initialManifest $PackageVersion
    $python = (Get-Command python.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    Start-TestSource $trustedSource $SourceServerScriptPath $python

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
    $lockedPrevious = Wait-Ready
    try {
        # Hold the original process identity across the failed upgrade. An
        # installer that never stops it must not pass as successful recovery.
        $null = $lockedPrevious.Handle
        $lockedPreviousId = $lockedPrevious.Id
        $beforeToken = [PackageAcceptanceNative]::SnapshotToken($lockedPreviousId)
        $fileLock = [IO.File]::Open($installedExe, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        $result = Upgrade-Package $manifest 'inno-locked-update' -AllowFailure
        if ($result.ExitCode -eq 0) { throw 'WinGet reported success although the executable could not be replaced.' }
        if (-not $lockedPrevious.WaitForExit(10000)) { throw 'Locked-update failure did not stop the captured original process; recovery was not exercised.' }
        $installerLog = Join-Path $logDirectory 'inno-locked-update-installer.log'
        if (-not (Test-Path -LiteralPath $installerLog) -or
            [IO.File]::ReadAllText($installerLog) -notmatch [regex]::Escape("$Application.exe")) {
            throw 'Failure occurred before the candidate installer reached the locked application; this is not a rollback test.'
        }
        $current = Wait-Ready $lockedPreviousId
        Assert-TokenPreserved $beforeToken $current.Id 'failed-update-recovery'
        Assert-Installed $managed.Version $true
    } finally {
        if ($null -ne $fileLock) { $fileLock.Dispose(); $fileLock = $null }
        $lockedPrevious.Dispose()
    }
    Record-Pass "A locked payload stops captured process $lockedPreviousId and restores replacement $($current.Id) with the previous payload, ARP version, preferences, and working tray"

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
    $null = Assert-TestSourceRegistration $trustedSource 'source-before-fresh-install'
    $priorLogs = @{}
    foreach ($file in @(Get-ChildItem -LiteralPath $wingetLogDirectory -Filter '*.log' -File)) {
        $priorLogs[$file.FullName] = "$($file.Length):$($file.LastWriteTimeUtc.Ticks)"
    }
    $script:activeDiagnosticManifest = $manifest
    try {
        $null = Invoke-WinGet @('install', '--source', $trustedSource.Name, '--id', $packageId, '--exact', '--version', $next.Version,
            '--scope', 'user', '--location', $installDirectory, '--silent', '--accept-package-agreements',
            '--accept-source-agreements', '--disable-interactivity', '--verbose-logs',
            '--log', (Join-Path $logDirectory 'fresh-installer.log')) 'fresh-silent-install'
    } finally {
        $script:activeDiagnosticManifest = $null
        $freshLogs = Join-Path $logDirectory 'fresh-silent-install-winget'
        $null = New-Item -ItemType Directory -Path $freshLogs
        foreach ($file in @(Get-ChildItem -LiteralPath $wingetLogDirectory -Filter '*.log' -File)) {
            $stamp = "$($file.Length):$($file.LastWriteTimeUtc.Ticks)"
            if ($priorLogs.ContainsKey($file.FullName) -and $priorLogs[$file.FullName] -ceq $stamp) { continue }
            Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $freshLogs $file.Name)
        }
    }
    Assert-TrustedInstallTrace $manifest $freshLogs 'fresh-silent-install'
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
    try { Stop-TestSource $trustedSource }
    catch { $cleanupErrors.Add("Trusted source cleanup: $_") }
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
            TrustedSourceEvidence = $trustedSourceEvidence.ToArray();
            SourceFixture = $(if ($null -ne $trustedSource) { @{ Name = $trustedSource.Name; Identifier = $trustedSource.Identifier;
                Url = $trustedSource.Url; Registration = $trustedSource.Registration;
                Dependencies = $trustedSource.DependencyEvidence;
                CertificateStore = 'LocalMachine/Root';
                CertificateThumbprint = $(if ($trustedSource.Certificate) { $trustedSource.Certificate.Thumbprint }) } });
            Failure = $(if ($failure) { $failure.ToString() } else { $null }); CleanupErrors = $cleanupErrors.ToArray();
            Limitation = 'Candidates use an isolated explicit trusted REST source built from production manifests; public winget publication/index propagation is not exercised.' }
        $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $work 'result.json') -Encoding utf8
        Write-Host "Package-manager acceptance report: $work"
    }
}
if ($failure) { throw $failure }
if ($cleanupErrors.Count) { throw ($cleanupErrors -join [Environment]::NewLine) }
