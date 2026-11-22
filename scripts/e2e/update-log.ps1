param(
    [Parameter(Mandatory = $true)][string]$Application,
    [Parameter(Mandatory = $true)][string]$Root
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# Called by run-hosted.ps1, using its owned-process list and desktop helpers.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Update log association checks require the disposable hosted Windows desktop.'
}

$rootPath = Join-Path $Root 'update log проверка'
$capture = Join-Path $rootPath 'reader receipts'
$helperDirectory = Join-Path $rootPath 'helper'
$localData = Join-Path $rootPath 'Local AppData данные'
$logPath = Join-Path $localData 'RustSwitcher\updater.log'
$holdHelper = Join-Path $helperDirectory 'hold'
New-Item -ItemType Directory -Path $capture, $helperDirectory -Force | Out-Null
$recorder = Join-Path $rootPath 'log reader.exe'
$source = Join-Path $rootPath 'fixture.cs'
@'
using System;
using System.IO;
using System.Text;
using System.Threading;
public static class UpdateLogFixture {
    public static int Main(string[] args) {
        if (String.Equals(Path.GetFileName(Environment.GetCommandLineArgs()[0]), "powershell.exe", StringComparison.OrdinalIgnoreCase)) {
            string root = Environment.GetEnvironmentVariable("SWITCHER_OWNED_HELPER_DIRECTORY");
            if (String.IsNullOrEmpty(root)) return 90;
            int index = Array.IndexOf(args, "-LogPath");
            if (index < 0 || index + 1 >= args.Length) return 91;
            File.WriteAllText(Path.Combine(root, "helper-" + Guid.NewGuid().ToString("N") + ".txt"), args[index + 1]);
            DateTime deadline = DateTime.UtcNow.AddSeconds(20);
            while (File.Exists(Path.Combine(root, "hold")) && DateTime.UtcNow < deadline) Thread.Sleep(25);
            Console.Error.WriteLine("owned helper startup failure: stderr must reach updater.log");
            return 23;
        }
        if (args.Length != 2) return 92;
        string text = File.ReadAllText(args[1]);
        string[] receipt = {args.Length.ToString(), Convert.ToBase64String(Encoding.UTF8.GetBytes(args[1])), Convert.ToBase64String(Encoding.UTF8.GetBytes(text))};
        File.WriteAllLines(Path.Combine(args[0], Guid.NewGuid().ToString("N") + ".txt"), receipt);
        return 0;
    }
}
'@ | Set-Content -LiteralPath $source -Encoding utf8
$compiler = Join-Path ([Environment]::GetFolderPath('Windows')) 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
& $compiler /nologo /target:winexe "/out:$recorder" $source
if ($LASTEXITCODE -ne 0) { throw 'Could not compile owned log reader/updater fixture' }
Copy-Item -LiteralPath $recorder -Destination (Join-Path $helperDirectory 'powershell.exe')

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class UpdateLogShell {
    [DllImport("shell32.dll")] static extern void SHChangeNotify(uint change, uint flags, IntPtr first, IntPtr second);
    [DllImport("shlwapi.dll", CharSet=CharSet.Unicode)] static extern int AssocQueryString(uint flags, uint kind, string extension, string extra, StringBuilder output, ref uint length);
    [DllImport("shell32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr CommandLineToArgvW(string line, out int count);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr memory);
    public static void Refresh() { SHChangeNotify(0x08000000, 0x1000, IntPtr.Zero, IntPtr.Zero); }
    public static string Reader() {
        var value = new StringBuilder(32768); uint size = (uint)value.Capacity;
        return AssocQueryString(0x400, 2, ".log", null, value, ref size) == 0 ? value.ToString() : null;
    }
    public static string ProgId() {
        var value = new StringBuilder(32768); uint size = (uint)value.Capacity;
        return AssocQueryString(0x400, 20, ".log", null, value, ref size) == 0 ? value.ToString() : null;
    }
    public static uint AssociationResult() {
        var value = new StringBuilder(32768); uint size = (uint)value.Capacity;
        return unchecked((uint)AssocQueryString(0x400, 20, ".log", null, value, ref size));
    }
    public static string Describe(string association, uint flags, uint kind) {
        var value = new StringBuilder(32768); uint size = (uint)value.Capacity;
        int result = AssocQueryString(flags, kind, association, null, value, ref size);
        return "hr=0x" + unchecked((uint)result).ToString("X8") + "; value=" + value.ToString();
    }
    public static string[] Arguments(string line) {
        int count; IntPtr memory = CommandLineToArgvW(line, out count);
        if (memory == IntPtr.Zero) throw new InvalidOperationException("Could not parse owned Notepad command line");
        try {
            var args = new string[count];
            for (int i = 0; i < count; i++) args[i] = Marshal.PtrToStringUni(Marshal.ReadIntPtr(memory, i * IntPtr.Size));
            return args;
        } finally { LocalFree(memory); }
    }
}
'@

function Find-LogTrayItem([int]$ProcessId, [string]$Name) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $ProcessId)
    $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Children, $condition)
    foreach ($candidate in $windows) {
        if ($candidate.Current.ClassName -ne '#32768') { continue }
        $nameCondition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
        $item = $candidate.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $nameCondition)
        if ($null -ne $item) { return $item }
    }
    return $null
}
function Invoke-LogTrayItem($Process, [IntPtr]$Window, [string]$Name) {
    # Real v4 tray callback -> popup menu -> UI Automation invocation.
    # Separate deliberate openings beyond the application's 250 ms dedupe.
    Start-Sleep -Milliseconds 300
    if (-not [HostedInput]::PostMessage($Window, 0x8003, [UIntPtr]::Zero, [IntPtr]0x1007b)) {
        throw 'Could not open the owned tray menu'
    }
    Wait-Condition { $null -ne (Find-LogTrayItem $Process.Id $Name) } "tray item $Name"
    $item = Find-LogTrayItem $Process.Id $Name
    (Get-Pattern $item ([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
}
function Send-LogCallback([IntPtr]$Window, [int]$Event, [int]$IconId = 1) {
    if (-not [HostedInput]::PostMessage($Window, 0x8003, [UIntPtr]::Zero, [IntPtr](($IconId -shl 16) -bor $Event))) {
        throw 'Could not send the owned v4 notification callback'
    }
}
function Failure-NotificationCount {
    if (-not (Test-Path -LiteralPath $logPath)) { return 0 }
    return [regex]::Matches([IO.File]::ReadAllText($logPath), 'Update failure notification ready;').Count
}
function Receipt-Count { return @(Get-ChildItem -LiteralPath $capture -Filter '*.txt').Count }
function Assert-Receipts([int]$Count) {
    Wait-Condition { (Receipt-Count) -eq $Count } "$Count default log reader invocations"
    foreach ($file in Get-ChildItem -LiteralPath $capture -Filter '*.txt') {
        $lines = [IO.File]::ReadAllLines($file.FullName)
        if ($lines.Length -ne 3 -or $lines[0] -ne '2') { throw 'Log reader received unexpected arguments' }
        $actualPath = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($lines[1]))
        $actualText = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($lines[2]))
        if ($actualPath -cne $logPath) { throw "Reader received a different path: $actualPath" }
        if ($actualText -notlike '*owned helper startup failure: stderr must reach updater.log*') {
            throw 'Reader did not read the actual updater failure diagnostics'
        }
    }
}

$classes = [Microsoft.Win32.Registry]::CurrentUser
$extensionPath = 'Software\Classes\.log'
$baselineProgId = 'txtfile'
$systemNotepad = Join-Path ([Environment]::GetFolderPath('System')) 'notepad.exe'
$unassociatedProgId = 'RSLog.' + [guid]::NewGuid().ToString('N')
$unassociatedClassPath = 'Software\Classes\' + $unassociatedProgId
$existingClass = [Microsoft.Win32.Registry]::ClassesRoot.OpenSubKey($unassociatedProgId)
if ($null -ne $existingClass) { $existingClass.Dispose(); throw 'Owned association fixture class unexpectedly exists' }
$unassociatedClassCreated = $false
# Read snapshots without creating an empty per-user extension shadow key.
$extension = $classes.OpenSubKey($extensionPath, $true)
$extensionExisted = $null -ne $extension
$hadDefault = $false
$originalDefault = $null
$originalKind = [Microsoft.Win32.RegistryValueKind]::String
if ($null -ne $extension) {
    $hadDefault = $extension.GetValueNames() -contains ''
    if ($hadDefault) {
        $originalDefault = $extension.GetValue('', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $originalKind = $extension.GetValueKind('')
    }
    $extension.Dispose()
}
$handlerRoot = 'Software\Classes\' + $baselineProgId
$handlerPaths = @('Software\Classes', $handlerRoot, ($handlerRoot + '\shell'), ($handlerRoot + '\shell\open'), ($handlerRoot + '\shell\open\command'))
$handlerParents = foreach ($path in $handlerPaths) {
    $key = $classes.OpenSubKey($path)
    @{ Path = $path; Existed = $null -ne $key }
    if ($null -ne $key) { $key.Dispose() }
}
$handlerCommandPath = $handlerPaths[-1]
$handlerCommand = $classes.OpenSubKey($handlerCommandPath)
$hadCommandDefault = $false
$originalCommand = $null
$originalCommandKind = [Microsoft.Win32.RegistryValueKind]::String
if ($null -ne $handlerCommand) {
    try {
        $hadCommandDefault = $handlerCommand.GetValueNames() -contains ''
        if ($hadCommandDefault) {
            $originalCommand = $handlerCommand.GetValue('', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $originalCommandKind = $handlerCommand.GetValueKind('')
        }
    } finally { $handlerCommand.Dispose() }
}
$process = $null
$notepad = $null
function Read-LogAssociationValue([Microsoft.Win32.RegistryKey]$Hive, [string]$Path) {
    $key = $Hive.OpenSubKey($Path)
    if ($null -eq $key) { return '<absent key>' }
    try { return $key.GetValue('', '<absent value>', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }
    finally { $key.Dispose() }
}
function Write-LogAssociationDiagnostics([string]$Stage) {
    $choice = $classes.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.log\UserChoice')
    $choiceValue = '<absent>'
    if ($null -ne $choice) {
        try { $choiceValue = [string]$choice.GetValue('ProgId', '<absent>') } finally { $choice.Dispose() }
    }
    Write-Output "Association $Stage : ProgID=$([UpdateLogShell]::Describe('.log', 0x400, 20)); reader=$([UpdateLogShell]::Describe('.log', 0x400, 2)); baseline=$([UpdateLogShell]::Describe($baselineProgId, 0x800, 2)); UserChoice=$choiceValue"
    $merged = [Microsoft.Win32.Registry]::ClassesRoot
    $machine = [Microsoft.Win32.Registry]::LocalMachine
    Write-Output "Association registry $Stage : HKCU .log=$(Read-LogAssociationValue $classes $extensionPath); HKLM .log=$(Read-LogAssociationValue $machine $extensionPath); HKCR .log=$(Read-LogAssociationValue $merged '.log'); HKCR baseline command=$(Read-LogAssociationValue $merged ($baselineProgId + '\shell\open\command'))"
}
try {
    Write-LogAssociationDiagnostics 'before setup'
    if ([UpdateLogShell]::ProgId() -ne $baselineProgId -or [UpdateLogShell]::Reader() -ne $systemNotepad) {
        throw 'Fixture requires the verified txtfile/System32 Notepad baseline association'
    }
    $mergedCommand = [Microsoft.Win32.Registry]::ClassesRoot.OpenSubKey($baselineProgId + '\shell\open\command')
    if ($null -eq $mergedCommand) { throw 'Fixture requires an existing static open-command key' }
    try {
        if ([string]::IsNullOrWhiteSpace([string]$mergedCommand.GetValue(''))) { throw 'Fixture requires an existing static open command' }
        if ($mergedCommand.GetValueNames() -contains 'DelegateExecute') { throw 'Fixture requires a plain command; delegated handlers are never modified' }
    } finally { $mergedCommand.Dispose() }
    # Keep the selected ProgID and extension mapping intact. Configure only
    # the disposable user's existing plain-command reader for this test.
    $command = $classes.CreateSubKey($handlerCommandPath)
    try { $command.SetValue('', '"' + $recorder + '" "' + $capture + '" "%1"') } finally { $command.Dispose() }
    [UpdateLogShell]::Refresh()
    try { Wait-Condition { [UpdateLogShell]::Reader() -eq $recorder } 'effective .log default reader; UserChoice is never changed' }
    finally { Write-LogAssociationDiagnostics 'after setup' }

    $info = [Diagnostics.ProcessStartInfo]::new($Application)
    $info.UseShellExecute = $false
    $info.Environment['APPDATA'] = Join-Path $rootPath 'Roaming'
    $info.Environment['LOCALAPPDATA'] = $localData
    $info.Environment['PATH'] = $helperDirectory + ';' + $env:PATH
    $info.Environment['SWITCHER_OWNED_HELPER_DIRECTORY'] = $helperDirectory
    $process = [Diagnostics.Process]::Start($info)
    $ownedProcesses.Add($process)
    Wait-Condition { $null -ne (Find-AppWindow $process) } 'update log test application'
    $window = Find-AppWindow $process
    $handle = [IntPtr]$window.Current.NativeWindowHandle
    Wait-Condition { [HostedInput]::TrayIconPresent($handle) } 'update log test tray'

    Invoke-LogTrayItem $process $handle 'Check for updates'
    Wait-Condition { (Failure-NotificationCount) -eq 1 } 'real helper exit -> monitor -> armed failure notification'
    if ([IO.File]::ReadAllText($logPath) -notmatch 'PowerShell updater exited with.*23') {
        throw 'Helper failure exit status was not recorded'
    }
    $helperArguments = @(Get-ChildItem -LiteralPath $helperDirectory -Filter 'helper-*.txt')
    if ($helperArguments.Count -ne 1 -or [IO.File]::ReadAllText($helperArguments[0].FullName) -cne $logPath) {
        throw 'Helper did not receive the exact update log path'
    }
    Send-LogCallback $handle 0x405 2 # Wrong icon ID cannot consume the action.
    Start-Sleep -Milliseconds 350
    if ((Receipt-Count) -ne 0) { throw 'A different tray icon opened the update log' }
    Send-LogCallback $handle 0x403 # HIDE and TIMEOUT do not invalidate current-owner history.
    Send-LogCallback $handle 0x404
    Send-LogCallback $handle 0x405
    Assert-Receipts 1
    Send-LogCallback $handle 0x405
    Start-Sleep -Milliseconds 350
    if ((Receipt-Count) -ne 1) { throw 'A consumed failure notification opened the log twice' }
    Record-Pass 'release failed updater callback opens the exact Unicode log in the configured reader'

    [IO.File]::SetAttributes($logPath, [IO.FileAttributes]::ReadOnly)
    try {
        Invoke-LogTrayItem $process $handle 'Open update log'
        Assert-Receipts 2
    } finally { [IO.File]::SetAttributes($logPath, [IO.FileAttributes]::Normal) }
    Record-Pass 'release Open update log menu reads an existing read-only log with its default reader'

    Invoke-LogTrayItem $process $handle 'Check for updates'
    Wait-Condition { (Failure-NotificationCount) -eq 2 } 'second armed update failure'
    Set-Content -LiteralPath $holdHelper -Value 'hold'
    Invoke-LogTrayItem $process $handle 'Check for updates'
    Wait-Condition { @(Get-ChildItem -LiteralPath $helperDirectory -Filter 'helper-*.txt').Count -eq 3 } 'ordinary checking notification replaces the failure before helper completion'
    Send-LogCallback $handle 0x405
    Start-Sleep -Milliseconds 700
    if ((Receipt-Count) -ne 2) { throw 'An ordinary notification executed the stale update log action' }
    if (-not [HostedInput]::Responsive($handle)) { throw 'Updater monitoring blocked the application window' }
    Remove-Item -LiteralPath $holdHelper
    Wait-Condition { (Failure-NotificationCount) -eq 3 } 'held helper completion'
    Record-Pass 'release ordinary notification clears the update log action while the UI stays responsive'

    # A missing class mapping is ignored by this Windows image. Register the
    # owned candidate used in the earlier native reproduction, then require
    # Windows itself to report no effective association before testing fallback.
    # This is the first extension-mapping write; no user-choice data is touched.
    $type = $classes.CreateSubKey($unassociatedClassPath)
    $unassociatedClassCreated = $true
    try { $type.SetValue('', 'Rust Switcher unselected log reader fixture') } finally { $type.Dispose() }
    $shell = $classes.CreateSubKey($unassociatedClassPath + '\shell')
    try { $shell.SetValue('', 'open') } finally { $shell.Dispose() }
    $command = $classes.CreateSubKey($unassociatedClassPath + '\shell\open\command')
    try { $command.SetValue('', '"' + $recorder + '" "' + $capture + '" "%1"') } finally { $command.Dispose() }
    $extension = $classes.CreateSubKey($extensionPath)
    try { $extension.SetValue('', $unassociatedProgId) } finally { $extension.Dispose() }
    [UpdateLogShell]::Refresh()
    try {
        Wait-Condition { [UpdateLogShell]::AssociationResult() -eq [uint32]2147943555 } 'exact ERROR_NO_ASSOCIATION for .log'
    } finally { Write-LogAssociationDiagnostics 'after selecting unassociated fixture ProgID' }
    Invoke-LogTrayItem $process $handle 'Open update log'
    Wait-Condition {
        @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($process.Id)" | Where-Object {
            $_.ExecutablePath -eq $systemNotepad -and $_.CommandLine
        }).Count -eq 1
    } 'System32 Notepad fallback for missing association'
    $fallback = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($process.Id)" | Where-Object { $_.ExecutablePath -eq $systemNotepad })[0]
    $arguments = [UpdateLogShell]::Arguments($fallback.CommandLine)
    if ($arguments.Length -ne 2 -or $arguments[1] -cne $logPath) { throw 'Notepad fallback did not receive the exact log path' }
    $notepad = Get-Process -Id $fallback.ProcessId
    $ownedProcesses.Add($notepad)
    Wait-Condition { $notepad.Refresh(); $notepad.MainWindowHandle -ne [IntPtr]::Zero } 'owned Notepad window'
    [HostedInput]::PostMessage($notepad.MainWindowHandle, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    if (-not $notepad.WaitForExit(5000)) { throw 'Owned Notepad did not close' }
    Record-Pass 'release opens System32 Notepad with the exact path when Windows reports no effective .log association'

    Remove-Item -LiteralPath $logPath
    Invoke-LogTrayItem $process $handle 'Open update log'
    Wait-Condition {
        [HostedInput]::FindWindow('#32770', 'Rust Switcher: cannot open update log') -ne [IntPtr]::Zero
    } 'visible missing log error'
    if (Test-Path -LiteralPath $logPath) { throw 'Opening a missing log created a misleading empty file' }
    if ((Receipt-Count) -ne 2) { throw 'Missing log unexpectedly launched a reader' }
    $dialog = [HostedInput]::FindWindow('#32770', 'Rust Switcher: cannot open update log')
    [HostedInput]::PostMessage($dialog, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    Wait-Condition { [HostedInput]::FindWindow('#32770', 'Rust Switcher: cannot open update log') -eq [IntPtr]::Zero } 'missing log dialog dismissed'
    Record-Pass 'release missing updater log reports an error without creating a file or launching a reader'
    Quit-Owned $process $window
} finally {
    Remove-Item -LiteralPath $holdHelper -ErrorAction SilentlyContinue
    $cleanupFailures = [Collections.Generic.List[string]]::new()
    foreach ($owned in @($notepad, $process)) {
        try {
            if ($null -ne $owned -and -not $owned.HasExited) {
                $owned.Kill()
                if (-not $owned.WaitForExit(5000)) { $cleanupFailures.Add("Owned process $($owned.Id) did not exit") }
            }
        } catch { $cleanupFailures.Add($_.Exception.Message) }
    }
    try {
        $handlerCommand = $classes.OpenSubKey($handlerCommandPath, $true)
        if ($null -ne $handlerCommand) {
            try {
                if ($hadCommandDefault) { $handlerCommand.SetValue('', $originalCommand, $originalCommandKind) }
                else { $handlerCommand.DeleteValue('', $false) }
            } finally { $handlerCommand.Dispose() }
        }
    } catch { $cleanupFailures.Add($_.Exception.Message) }
    try {
        $extension = $classes.OpenSubKey($extensionPath, $true)
        if ($null -ne $extension) {
            try {
                if ($hadDefault) { $extension.SetValue('', $originalDefault, $originalKind) }
                else { $extension.DeleteValue('', $false) }
                $empty = $extension.SubKeyCount -eq 0 -and $extension.ValueCount -eq 0
            } finally { $extension.Dispose() }
            if (-not $extensionExisted -and $empty) { $classes.DeleteSubKey($extensionPath, $false) }
        }
    } catch { $cleanupFailures.Add($_.Exception.Message) }
    try {
        if ($unassociatedClassCreated) { $classes.DeleteSubKeyTree($unassociatedClassPath, $false) }
    } catch { $cleanupFailures.Add($_.Exception.Message) }
    for ($index = $handlerParents.Count - 1; $index -ge 0; $index--) {
        $parent = $handlerParents[$index]
        if ($parent.Existed) { continue }
        try {
            $key = $classes.OpenSubKey($parent.Path)
            if ($null -ne $key) {
                try { $empty = $key.SubKeyCount -eq 0 -and $key.ValueCount -eq 0 } finally { $key.Dispose() }
                if ($empty) { $classes.DeleteSubKey($parent.Path, $false) }
            }
        } catch { $cleanupFailures.Add($_.Exception.Message) }
    }
    [UpdateLogShell]::Refresh()
    if ($cleanupFailures.Count -gt 0) { throw ($cleanupFailures -join '; ') }
}
