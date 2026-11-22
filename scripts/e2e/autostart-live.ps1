param(
    [Parameter(Mandatory = $true)][string]$ReleaseExe
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# This accident guard is not a sandbox. The disposable GitHub VM is the boundary.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Autostart acceptance requires the dedicated GitHub-hosted Windows job.'
}
if (-not [Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
    throw 'An interactive Windows session is required.'
}
if (@(Get-Process -Name rust-switcher -ErrorAction SilentlyContinue).Count -ne 0) {
    throw 'A pre-existing Rust Switcher process would invalidate this isolated scenario.'
}
$ReleaseExe = (Resolve-Path -LiteralPath $ReleaseExe).Path
$runRoot = Join-Path $env:RUNNER_TEMP ('Rust Switcher autostart ' + [guid]::NewGuid().ToString('N'))
$installDirectory = Join-Path $runRoot 'Installed application with spaces'
$profileDirectory = Join-Path $runRoot 'Isolated profile'
$testExe = Join-Path $installDirectory 'rust-switcher.exe'
$runKeyPath = 'Software\Microsoft\Windows\CurrentVersion\Run'
$valueName = 'RustSwitcher'
$legacyShortcut = Join-Path ([Environment]::GetFolderPath('Startup')) 'RustSwitcher Autostart.lnk'
$legacyBackup = Join-Path $runRoot 'original-startup.lnk'
$ownedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$checks = [System.Collections.Generic.List[string]]::new()
$failure = $null
$registrySnapshot = $null
$legacyMoved = $false

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public static class AutostartAcceptance {
    private delegate bool EnumWindowCallback(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetClassName(IntPtr window, StringBuilder name, int capacity);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true, SetLastError=true)]
    private static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr w, IntPtr l, uint flags, uint timeout, out UIntPtr result);
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] private struct STARTUPINFO {
        public uint cb; public string reserved, desktop, title;
        public uint x, y, xSize, ySize, xCountChars, yCountChars, fillAttribute, flags;
        public ushort showWindow, reservedSize;
        public IntPtr reservedBytes, stdInput, stdOutput, stdError;
    }
    [StructLayout(LayoutKind.Sequential)] private struct PROCESS_INFORMATION {
        public IntPtr process, thread; public uint processId, threadId;
    }
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern bool CreateProcess(string application, StringBuilder commandLine,
        IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags,
        IntPtr environment, string currentDirectory, ref STARTUPINFO startup, out PROCESS_INFORMATION process);
    [DllImport("kernel32.dll")] private static extern bool CloseHandle(IntPtr handle);
    public static IntPtr MainWindow(int processId) {
        var windows = new List<IntPtr>();
        EnumWindows((window, parameter) => {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner == (uint)processId) {
                var name = new StringBuilder(256);
                GetClassName(window, name, name.Capacity);
                if (name.ToString() == "RustSwitcherMainWindow") windows.Add(window);
            }
            return true;
        }, IntPtr.Zero);
        if (windows.Count > 1) throw new InvalidOperationException("Multiple main windows for the owned process");
        return windows.Count == 1 ? windows[0] : IntPtr.Zero;
    }
    public static bool Responsive(IntPtr window) {
        UIntPtr result;
        return SendMessageTimeout(window, 0, UIntPtr.Zero, IntPtr.Zero, 2, 500, out result) != IntPtr.Zero;
    }
    public static Process StartStoredCommand(string commandLine, string directory) {
        var startup = new STARTUPINFO();
        startup.cb = (uint)Marshal.SizeOf<STARTUPINFO>();
        PROCESS_INFORMATION created;
        // Pass the complete stored string to Windows. Reconstructing FileName and
        // ArgumentList would hide broken quoting in the actual Run registration.
        if (!CreateProcess(null, new StringBuilder(commandLine), IntPtr.Zero, IntPtr.Zero,
                false, 0, IntPtr.Zero, directory, ref startup, out created))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Stored Run command failed");
        try { return Process.GetProcessById(checked((int)created.processId)); }
        finally { CloseHandle(created.thread); CloseHandle(created.process); }
    }
}
'@

function Wait-Condition([scriptblock]$Condition, [string]$Description) {
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out: $Description"
}
function Record-Pass([string]$Name) { $checks.Add($Name); Write-Output "PASS: $Name" }
function Read-Registration {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($runKeyPath)
    try {
        if ($null -eq $key -or $key.GetValueNames() -notcontains $valueName) {
            return @{ Exists = $false }
        }
        return @{
            Exists = $true
            Kind = $key.GetValueKind($valueName)
            Value = $key.GetValue($valueName, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        }
    } finally { if ($null -ne $key) { $key.Dispose() } }
}
function Set-Registration($Snapshot) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($runKeyPath)
    try {
        if ($Snapshot.Exists) { $key.SetValue($valueName, $Snapshot.Value, $Snapshot.Kind) }
        else { $key.DeleteValue($valueName, $false) }
    } finally { $key.Dispose() }
}
function Start-Release {
    $info = [System.Diagnostics.ProcessStartInfo]::new($testExe)
    $info.UseShellExecute = $false
    $info.WorkingDirectory = $runRoot
    $info.Environment['APPDATA'] = $profileDirectory
    foreach ($name in @('RUST_SWITCHER_E2E_PLAYGROUND', 'RUST_SWITCHER_E2E_ALLOW_INJECTED', 'RUST_SWITCHER_E2E_TARGET_PID')) {
        $info.Environment.Remove($name) | Out-Null
    }
    $process = [System.Diagnostics.Process]::Start($info)
    $ownedProcesses.Add($process)
    return $process
}
function Start-Registration([string]$Command) {
    $savedEnvironment = @{}
    foreach ($name in @('APPDATA', 'RUST_SWITCHER_E2E_PLAYGROUND', 'RUST_SWITCHER_E2E_ALLOW_INJECTED', 'RUST_SWITCHER_E2E_TARGET_PID')) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $null, 'Process')
    }
    try {
        [Environment]::SetEnvironmentVariable('APPDATA', $profileDirectory, 'Process')
        $process = [AutostartAcceptance]::StartStoredCommand($Command, $runRoot)
        $ownedProcesses.Add($process)
        return $process
    } finally {
        foreach ($name in $savedEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
        }
    }
}
function Get-Window($Process) {
    if ($Process.HasExited) { throw "Owned application exited early with $($Process.ExitCode)" }
    return [AutostartAcceptance]::MainWindow($Process.Id)
}
function Get-Control($Window, [string]$Name) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $control = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($null -eq $control) { throw "Missing control: $Name" }
    return $control
}
function Get-Toggle($Window, [string]$Name) {
    return (Get-Control $Window $Name).GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
}
function Set-Toggle($Window, [string]$Name, [bool]$Enabled) {
    $pattern = Get-Toggle $Window $Name
    $state = if ($Enabled) { [System.Windows.Automation.ToggleState]::On } else { [System.Windows.Automation.ToggleState]::Off }
    if ($pattern.Current.ToggleState -ne $state) { $pattern.Toggle() }
    Wait-Condition { $pattern.Current.ToggleState -eq $state } "toggle $Name"
}
function Quit-Release($Process, $Window) {
    (Get-Control $Window 'Exit').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    if (-not $Process.WaitForExit(10000)) { throw 'Application did not exit gracefully' }
    if ($Process.ExitCode -ne 0) { throw "Application exit code was $($Process.ExitCode)" }
}

try {
    New-Item -ItemType Directory -Path $installDirectory, $profileDirectory | Out-Null
    Copy-Item -LiteralPath $ReleaseExe -Destination $testExe
    $registrySnapshot = Read-Registration
    Set-Registration @{ Exists = $false }
    if (Test-Path -LiteralPath $legacyShortcut) {
        Move-Item -LiteralPath $legacyShortcut -Destination $legacyBackup
        $legacyMoved = $true
    }
    $process = Start-Release
    Wait-Condition { (Get-Window $process) -ne [IntPtr]::Zero } 'normal release startup'
    $window = Get-Window $process
    if (-not [AutostartAcceptance]::IsWindowVisible($window)) { throw 'Normal launch must show the default window' }
    if ((Read-Registration).Exists) { throw 'A fresh manual launch enabled autostart unexpectedly' }
    Set-Toggle $window 'Start minimized' $false
    Set-Toggle $window 'Dark theme' $true
    Set-Toggle $window 'Start on startup' $true
    $expectedCommand = '"' + $testExe + '" --autostart'
    Wait-Condition {
        $registration = Read-Registration
        $registration.Exists -and $registration.Kind -eq [Microsoft.Win32.RegistryValueKind]::String -and
            $registration.Value -ceq $expectedCommand
    } 'exact quoted Run command for a path containing spaces'
    Quit-Release $process $window
    $config = Join-Path $profileDirectory 'RustSwitcher/config.json'
    $configHash = (Get-FileHash -LiteralPath $config -Algorithm SHA256).Hash
    Record-Pass 'UI enables the exact quoted persistent Run command'

    $stored = Read-Registration
    if (-not $stored.Exists -or $stored.Value -cne $expectedCommand) { throw 'Run registration did not survive exit' }
    $process = Start-Registration $stored.Value
    Wait-Condition { (Get-Window $process) -ne [IntPtr]::Zero } 'stored Run command creates the real application window'
    $window = Get-Window $process
    $hiddenUntil = [DateTime]::UtcNow.AddSeconds(1)
    do {
        if ($process.HasExited -or [AutostartAcceptance]::IsWindowVisible($window) -or
            -not [AutostartAcceptance]::Responsive($window)) {
            throw 'Autostart must leave a live responsive application with its main window hidden'
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $hiddenUntil)
    if ((Get-FileHash -LiteralPath $config -Algorithm SHA256).Hash -cne $configHash) {
        throw 'Executing the Run command changed persisted settings'
    }
    Record-Pass 'Windows executes the stored Run command from a space-containing path and starts hidden'

    [AutostartAcceptance]::ShowWindow($window, 9) | Out-Null
    Wait-Condition { [AutostartAcceptance]::IsWindowVisible($window) } 'reveal owned settings window'
    if ((Get-Toggle $window 'Start on startup').Current.ToggleState -ne [System.Windows.Automation.ToggleState]::On -or
        (Get-Toggle $window 'Start minimized').Current.ToggleState -ne [System.Windows.Automation.ToggleState]::Off) {
        throw 'Autostart state or independent start-minimized preference was not preserved'
    }
    Set-Toggle $window 'Start on startup' $false
    Wait-Condition { -not (Read-Registration).Exists } 'UI removes Run registration'
    Quit-Release $process $window
    $process = Start-Release
    Wait-Condition { (Get-Window $process) -ne [IntPtr]::Zero } 'manual restart after disabling autostart'
    $window = Get-Window $process
    if ((Read-Registration).Exists -or
        (Get-Toggle $window 'Start on startup').Current.ToggleState -ne [System.Windows.Automation.ToggleState]::Off -or
        -not [AutostartAcceptance]::IsWindowVisible($window)) {
        throw 'Disabled autostart did not remain disabled across a normal restart'
    }
    if ((Get-FileHash -LiteralPath $config -Algorithm SHA256).Hash -cne $configHash) {
        throw 'Autostart changes altered unrelated persisted settings'
    }
    Quit-Release $process $window
    Record-Pass 'disabled autostart persists across restart without changing other settings'
} catch {
    $failure = $_.Exception.Message
    Write-Output $_.ScriptStackTrace
    throw
} finally {
    $cleanupErrors = [System.Collections.Generic.List[string]]::new()
    foreach ($process in $ownedProcesses) {
        try {
            if (-not $process.HasExited) {
                $process.Kill()
                if (-not $process.WaitForExit(5000)) { throw "Owned process $($process.Id) did not exit" }
            }
            $process.Dispose()
        } catch { $cleanupErrors.Add($_.Exception.Message) }
    }
    try {
        if ($null -ne $registrySnapshot) { Set-Registration $registrySnapshot }
        if ($legacyMoved) { Move-Item -LiteralPath $legacyBackup -Destination $legacyShortcut }
    } catch { $cleanupErrors.Add($_.Exception.Message) }
    [ordered]@{
        passed = $checks.ToArray(); failure = $failure; cleanupErrors = $cleanupErrors.ToArray()
        commit = $env:GITHUB_SHA; run = $env:GITHUB_RUN_ID
    } | ConvertTo-Json -Depth 4 | Write-Output
    if ($cleanupErrors.Count -gt 0 -and $null -eq $failure) { throw ($cleanupErrors -join '; ') }
}
