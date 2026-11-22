param(
    [Parameter(Mandatory = $true)][string]$DebugExe,
    [Parameter(Mandatory = $true)][string]$ReleaseExe
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# This is an accident guard, not a sandbox. The GitHub VM supplies isolation.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Interactive checks require the dedicated GitHub-hosted Windows job. Never spoof these values on a working desktop.'
}
if (-not [Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
    throw 'An interactive Windows session is required; service sessions cannot pass.'
}
$DebugExe = (Resolve-Path -LiteralPath $DebugExe).Path
$ReleaseExe = (Resolve-Path -LiteralPath $ReleaseExe).Path
$runRoot = Join-Path $env:RUNNER_TEMP ('switcher-desktop-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runRoot | Out-Null
$checks = [System.Collections.Generic.List[string]]::new()
$scenarioFailures = [System.Collections.Generic.List[string]]::new()
$journalCases = [System.Collections.Generic.List[object]]::new()
$ownedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$switcherOutputTasks = [System.Collections.Generic.List[object]]::new()
$failure = $null

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
public static class HostedInput {
    [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public UNION data; }
    [StructLayout(LayoutKind.Explicit)] public struct UNION {
        [FieldOffset(0)] public KEYBOARD keyboard;
        [FieldOffset(0)] public MOUSE mouse;
    }
    [StructLayout(LayoutKind.Sequential)] public struct KEYBOARD {
        public ushort key, scan; public uint flags, time; public UIntPtr extra;
    }
    [StructLayout(LayoutKind.Sequential)] public struct MOUSE {
        public int x, y; public uint data, flags, time; public UIntPtr extra;
    }
    [StructLayout(LayoutKind.Sequential)] private struct RECT {
        public int left, top, right, bottom;
    }
    [StructLayout(LayoutKind.Sequential)] private struct ICON_ID {
        public uint size; public IntPtr window; public uint id; public Guid guid;
    }
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] private struct ICON_DATA {
        public uint size; public IntPtr window; public uint id, flags, callback; public IntPtr icon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst=128)] public string tip;
        public uint state, stateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst=256)] public string info;
        public uint version;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst=64)] public string title;
        public uint infoFlags; public Guid guid; public IntPtr balloonIcon;
    }
    [DllImport("shell32.dll")] private static extern int Shell_NotifyIconGetRect(ref ICON_ID icon, out RECT rectangle);
    [DllImport("shell32.dll", CharSet=CharSet.Unicode)] private static extern bool Shell_NotifyIcon(uint message, ref ICON_DATA icon);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern uint RegisterWindowMessage(string name);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern IntPtr OpenThread(uint access, bool inherit, uint id);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern uint SuspendThread(IntPtr thread);
    [DllImport("kernel32.dll")] private static extern bool CloseHandle(IntPtr handle);
    public static bool TrayIconPresent(IntPtr window) {
        var icon = new ICON_ID { size = (uint)Marshal.SizeOf<ICON_ID>(), window = window, id = 1 };
        RECT rectangle;
        return Shell_NotifyIconGetRect(ref icon, out rectangle) == 0;
    }
    public static void DeleteTrayIcon(IntPtr window) {
        var icon = new ICON_DATA { size = (uint)Marshal.SizeOf<ICON_DATA>(), window = window, id = 1 };
        if (!Shell_NotifyIcon(2, ref icon)) throw new InvalidOperationException("Could not remove the owned app's tray icon");
        if (TrayIconPresent(window)) throw new InvalidOperationException("Owned tray icon was not removed");
    }
    public static void Broadcast(uint message, uint value) {
        if (message == 0 || !PostMessage(new IntPtr(0xffff), message, new UIntPtr(value), IntPtr.Zero))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Tray recovery broadcast failed");
    }
    public static void SuspendOwnedWindowThread(IntPtr window, uint expectedProcess) {
        uint process;
        uint id = GetWindowThreadProcessId(window, out process);
        if (id == 0 || process != expectedProcess) throw new InvalidOperationException("Refusing to suspend an unowned window");
        IntPtr thread = OpenThread(2, false, id);
        if (thread == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
        try {
            if (SuspendThread(thread) == uint.MaxValue) throw new Win32Exception(Marshal.GetLastWin32Error());
        } finally { CloseHandle(thread); }
    }
    [StructLayout(LayoutKind.Sequential)] private struct GUI_THREAD_INFO {
        public uint size, flags;
        public IntPtr active, focus, capture, menuOwner, moveSize, caret;
        public RECT caretRect;
    }
    [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint count, INPUT[] inputs, int size);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int key);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] public static extern IntPtr GetKeyboardLayout(uint thread);
    [DllImport("user32.dll", SetLastError=true)] private static extern bool GetGUIThreadInfo(uint thread, ref GUI_THREAD_INFO info);
    [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr parent, int id);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string className, string title);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(IntPtr window, uint message, UIntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr LoadKeyboardLayout(string layout, uint flags);
    public static void Key(ushort key, bool up) {
        var input = new INPUT(); input.type = 1; input.data.keyboard.key = key;
        input.data.keyboard.flags = up ? 2u : 0u;
        if (SendInput(1, new[] {input}, Marshal.SizeOf<INPUT>()) != 1)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "SendInput did not inject one event");
    }
    public static int InputSize() { return Marshal.SizeOf<INPUT>(); }
    private static INPUT InputKey(ushort key, bool up) {
        var input = new INPUT(); input.type = 1; input.data.keyboard.key = key;
        input.data.keyboard.flags = up ? 2u : 0u; return input;
    }
    public static void DoubleShiftThenTab() {
        var input = new[] {InputKey(0xA0, false), InputKey(0xA0, true),
            InputKey(0xA0, false), InputKey(0xA0, true), InputKey(9, false), InputKey(9, true)};
        if (SendInput((uint)input.Length, input, Marshal.SizeOf<INPUT>()) != (uint)input.Length)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Shortcut/Tab batch was incomplete");
    }
    private static void ReleaseAfterIncompleteBatch(ushort modifier, ushort firstKey) {
        try { Key(firstKey, true); } catch { }
        try { Key(modifier, true); } catch { }
    }
    public static double ReleaseThenFirstKey(ushort modifier, ushort firstKey, int delayMs) {
        var release = new INPUT(); release.type = 1;
        release.data.keyboard.key = modifier; release.data.keyboard.flags = 2;
        var down = new INPUT(); down.type = 1; down.data.keyboard.key = firstKey;
        var up = down; up.data.keyboard.flags = 2;
        if (delayMs == 0) {
            if (SendInput(3, new[] {release, down, up}, Marshal.SizeOf<INPUT>()) != 3) {
                int error = Marshal.GetLastWin32Error();
                ReleaseAfterIncompleteBatch(modifier, firstKey);
                throw new Win32Exception(error, "Modifier/first-key batch was incomplete");
            }
            return 0;
        }
        Key(modifier, true);
        var clock = System.Diagnostics.Stopwatch.StartNew();
        System.Threading.Thread.Sleep(delayMs);
        double elapsed = clock.Elapsed.TotalMilliseconds;
        if (SendInput(2, new[] {down, up}, Marshal.SizeOf<INPUT>()) != 2) {
            int error = Marshal.GetLastWin32Error();
            ReleaseAfterIncompleteBatch(modifier, firstKey);
            throw new Win32Exception(error, "First-key batch was incomplete");
        }
        return elapsed;
    }
    public static IntPtr WindowLayout(IntPtr window) {
        uint process;
        uint thread = GetWindowThreadProcessId(window, out process);
        if (thread == 0) throw new InvalidOperationException("Window has no GUI thread");
        return GetKeyboardLayout(thread);
    }
    public static uint WindowProcess(IntPtr window) {
        uint process;
        if (GetWindowThreadProcessId(window, out process) == 0)
            throw new InvalidOperationException("Window has no process");
        return process;
    }
    public static IntPtr WindowFocus(IntPtr window) {
        uint process;
        uint thread = GetWindowThreadProcessId(window, out process);
        if (thread == 0) throw new InvalidOperationException("Focus target has no GUI thread");
        var info = new GUI_THREAD_INFO();
        info.size = checked((uint)Marshal.SizeOf<GUI_THREAD_INFO>());
        if (!GetGUIThreadInfo(thread, ref info))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Source focus snapshot failed");
        return info.focus;
    }
    [DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true, SetLastError=true)] static extern IntPtr SendMessageTimeout(
        IntPtr window, uint message, UIntPtr w, IntPtr l, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true, SetLastError=true)] static extern IntPtr ReadSelection(
        IntPtr window, uint message, out uint start, out uint end, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true, CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr ReadText(IntPtr window, uint message, UIntPtr capacity, StringBuilder text, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true, CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr WriteText(IntPtr window, uint message, UIntPtr w, string text, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW", SetLastError=true)] static extern IntPtr WindowStyle(IntPtr window, int index);
    public static string FixtureText(IntPtr window) {
        var text = new StringBuilder(256); UIntPtr result;
        if (ReadText(window, 0xD, new UIntPtr((uint)text.Capacity), text, 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Owned fixture text read failed");
        return text.ToString();
    }
    public static void SetFixtureText(IntPtr window, string text) {
        UIntPtr result;
        if (WriteText(window, 0xC, UIntPtr.Zero, text, 2, 1000, out result) == IntPtr.Zero || result == UIntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Owned fixture text setup failed");
    }
    public static void SetFixturePassword(IntPtr window, bool enabled) {
        UIntPtr result;
        if (SendMessageTimeout(window, 0x8013, new UIntPtr(enabled ? 1u : 0u), IntPtr.Zero, 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Owned password fixture setup failed");
        if (result.ToUInt64() != 1) throw new InvalidOperationException("Owned password fixture did not acknowledge setup");
        if (((WindowStyle(window, -16).ToInt64() & 0x20) != 0) != enabled)
            throw new InvalidOperationException("Owned fixture did not change its ES_PASSWORD style");
    }
    public static bool PasswordFixtureTextMatches(IntPtr window, uint sample) {
        if ((WindowStyle(window, -16).ToInt64() & 0x20) == 0)
            throw new InvalidOperationException("Password assertion requires a real ES_PASSWORD Edit");
        UIntPtr result;
        if (SendMessageTimeout(window, 0x8014, new UIntPtr(sample), IntPtr.Zero, 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Owned password fixture comparison failed");
        if (result.ToUInt64() > 1) throw new InvalidOperationException("Unknown synthetic password sample");
        return result.ToUInt64() == 1;
    }
    public static void SelectFixtureText(IntPtr window, int length) {
        UIntPtr result;
        if (SendMessageTimeout(window, 0xB1, UIntPtr.Zero, new IntPtr(length), 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Fixture EM_SETSEL failed");
        uint start, end;
        if (ReadSelection(window, 0xB0, out start, out end, 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Fixture EM_GETSEL failed");
        if (start != 0 || end != (uint)length)
            throw new InvalidOperationException("Fixture selection mismatch: hwnd=" + window + ", expected=" + length + ", actual=" + start + ".." + end + ", packed=0x" + result.ToUInt64().ToString("X"));
    }
    public static bool HasSelection(IntPtr window, int length) {
        return HasSelectionRange(window, 0, length);
    }
    public static bool HasSelectionRange(IntPtr window, int expectedStart, int expectedEnd) {
        UIntPtr result;
        uint start, end;
        if (ReadSelection(window, 0xB0, out start, out end, 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Fixture selection read failed");
        return start == (uint)expectedStart && end == (uint)expectedEnd;
    }
    public static bool Responsive(IntPtr window) {
        UIntPtr result;
        return SendMessageTimeout(window, 0, UIntPtr.Zero, IntPtr.Zero, 2, 500, out result) != IntPtr.Zero;
    }
    public static long SecurityMetadata(IntPtr window, uint selector) {
        UIntPtr result;
        if (SendMessageTimeout(window, 0x83F0, new UIntPtr(selector), IntPtr.Zero, 2, 200, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Journal metadata read failed");
        return unchecked((long)result.ToUInt64());
    }
    public static void ArmFixture(IntPtr window, uint mode) {
        UIntPtr result;
        if (SendMessageTimeout(window, 0x8011, new UIntPtr(mode), IntPtr.Zero, 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Fixture trap setup failed");
        if (result.ToUInt64() != 1)
            throw new InvalidOperationException("Fixture trap did not acknowledge setup");
    }
    public static int ResetJournalFixture(IntPtr window) {
        UIntPtr result;
        if (SendMessageTimeout(window, 0x8012, UIntPtr.Zero, IntPtr.Zero, 2, 1000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Journal fixture reset failed");
        return checked((int)result.ToUInt64());
    }
}
'@
if ([HostedInput]::InputSize() -ne $(if ([IntPtr]::Size -eq 8) {40} else {28})) {
    throw 'INPUT ABI does not match Windows.'
}

function Wait-Condition([scriptblock]$Condition, [string]$Description, [int]$Milliseconds = 8000) {
    $deadline = [DateTime]::UtcNow.AddMilliseconds($Milliseconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out: $Description"
}
function Record-Pass([string]$Name) { $checks.Add($Name); Write-Output "PASS: $Name" }
function Get-AutostartCommand {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run')
    if ($null -eq $key) { return $null }
    try { return $key.GetValue('RustSwitcher', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }
    finally { $key.Dispose() }
}
function Start-Switcher([string]$Exe, [string]$ProfilePath, [bool]$DebugMode, [uint]$JournalFixturePid = 0, [string[]]$ExtraArguments = @()) {
    $info = [System.Diagnostics.ProcessStartInfo]::new($Exe)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WorkingDirectory = $runRoot
    foreach ($argument in $ExtraArguments) { $info.ArgumentList.Add($argument) }
    $info.Environment['APPDATA'] = $ProfilePath
    $info.Environment['RUST_LOG'] = 'warn'
    $info.Environment.Remove('RUST_SWITCHER_E2E_TARGET_PID') | Out-Null
    if ($JournalFixturePid -ne 0) {
        if (-not $DebugMode) { throw 'Only the debug app can allow a journal fixture PID' }
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        $info.Environment['RUST_SWITCHER_E2E_TARGET_PID'] = $JournalFixturePid.ToString()
        $info.Environment['RUST_LOG'] = 'warn,rust_switcher_debug::platform::win::secure_input=debug,rust_switcher_debug::input::ring_buffer=debug,rust_switcher_debug::domain::text::last_word=debug'
    }
    if ($DebugMode) {
        $info.Environment['RUST_SWITCHER_E2E_PLAYGROUND'] = '1'
        $info.Environment['RUST_SWITCHER_E2E_ALLOW_INJECTED'] = '1'
    } else {
        $info.Environment.Remove('RUST_SWITCHER_E2E_PLAYGROUND') | Out-Null
        $info.Environment.Remove('RUST_SWITCHER_E2E_ALLOW_INJECTED') | Out-Null
    }
    $process = [System.Diagnostics.Process]::Start($info)
    $ownedProcesses.Add($process)
    if ($JournalFixturePid -ne 0) {
        # Drain both pipes from startup so diagnostic output cannot block the
        # app or add a synchronization roundtrip before a tested input event.
        $switcherOutputTasks.Add($process.StandardOutput.ReadToEndAsync())
        $switcherOutputTasks.Add($process.StandardError.ReadToEndAsync())
    }
    return $process
}
function Find-AppWindow([System.Diagnostics.Process]$Process) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $Process.Id)
    return [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
        [System.Windows.Automation.TreeScope]::Children, $condition)
}
function Focus-App($Window) {
    $handle = [IntPtr]$Window.Current.NativeWindowHandle
    [HostedInput]::ShowWindow($handle, 9) | Out-Null
    [HostedInput]::SetForegroundWindow($handle) | Out-Null
    Wait-Condition { [HostedInput]::GetForegroundWindow() -eq $handle } 'owned app foreground'
}
function Focus-SettingsEditor($Window, $Editor) {
    # Apply/Cancel fixture setup must finish before its single hotkey injection.
    $appHandle = [IntPtr]$Window.Current.NativeWindowHandle
    $editorHandle = [IntPtr]$Editor.Current.NativeWindowHandle
    if ($editorHandle -eq [IntPtr]::Zero) { throw 'Settings editor has no native window' }
    Focus-App $Window
    $Editor.SetFocus()
    Wait-Condition {
        [HostedInput]::GetForegroundWindow() -eq $appHandle -and
            [HostedInput]::WindowFocus($appHandle) -eq $editorHandle
    } 'settings hotkey editor native focus'
}
function Set-OwnedControlFocus($SourceWindow, $SourceEdit) {
    $windowHandle = [IntPtr]$SourceWindow.Current.NativeWindowHandle
    $editHandle = [IntPtr]$SourceEdit.Current.NativeWindowHandle
    $deadline = [DateTime]::UtcNow.AddSeconds(4)
    $stableSamples = 0
    $requests = 0
    $actualForeground = [IntPtr]::Zero
    $actualFocus = [IntPtr]::Zero
    $uiaFocused = $false
    $lastError = $null
    [HostedInput]::ShowWindow($windowHandle, 9) | Out-Null
    do {
        try {
            $actualForeground = [HostedInput]::GetForegroundWindow()
            $actualFocus = [HostedInput]::WindowFocus($windowHandle)
            $uiaFocused = $SourceEdit.Current.HasKeyboardFocus
            if ($actualForeground -eq $windowHandle -and $actualFocus -eq $editHandle -and $uiaFocused) {
                $stableSamples++
                if ($stableSamples -ge 3) { return $requests }
            } else {
                $stableSamples = 0
                if ($actualForeground -ne $windowHandle) {
                    [HostedInput]::SetForegroundWindow($windowHandle) | Out-Null
                }
                # Activation and UIA focus are asynchronous across processes.
                # Reissue setup focus only; no tested key has been sent yet.
                $SourceEdit.SetFocus()
                $requests++
            }
        } catch {
            $stableSamples = 0
            $lastError = $_.Exception.Message
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Modifier source focus setup failed: expectedWindow=$windowHandle expectedEdit=$editHandle actualForeground=$actualForeground actualFocus=$actualFocus uiaFocused=$uiaFocused requests=$requests lastError=$lastError"
}
function Find-Control($Window, [string]$Name) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $element = $Window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($null -eq $element) { throw "Missing control: $Name" }
    return $element
}
function Get-Pattern($Element, $PatternId) {
    $value = $null
    if (-not $Element.TryGetCurrentPattern($PatternId, [ref]$value)) {
        throw "UI Automation pattern $($PatternId.ProgrammaticName) unavailable: name='$($Element.Current.Name)', class='$($Element.Current.ClassName)'"
    }
    return $value
}
function Toggle-Control($Window, [string]$Name, [bool]$On) {
    $element = Find-Control $Window $Name
    $pattern = Get-Pattern $element ([System.Windows.Automation.TogglePattern]::Pattern)
    $desired = if ($On) { [System.Windows.Automation.ToggleState]::On } else { [System.Windows.Automation.ToggleState]::Off }
    if ($pattern.Current.ToggleState -ne $desired) { $pattern.Toggle() }
    Wait-Condition { $pattern.Current.ToggleState -eq $desired } "toggle $Name"
}
function Apply-Settings($Window) {
    $element = Find-Control $Window 'Apply'
    (Get-Pattern $element ([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
}
function Assert-ConfigFlag([string]$Path, [string]$Flag, [bool]$Enabled) {
    $expected = $Enabled.ToString().ToLowerInvariant()
    Wait-Condition {
        (Test-Path -LiteralPath $Path) -and
        (Get-Content -LiteralPath $Path -Raw) -match "(?m)^$Flag\s*=\s*$expected\s*$"
    } "$Flag saved immediately as $expected"
}
function Find-Playground($Window) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Edit)
    $edits = $Window.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
    foreach ($edit in $edits) {
        $pattern = Get-Pattern $edit ([System.Windows.Automation.ValuePattern]::Pattern)
        if (-not $pattern.Current.IsReadOnly -and $pattern.Current.Value -eq '' -and -not $edit.Current.IsOffscreen) { return $edit }
    }
    throw 'Visible editable playground was not found'
}
function Tap-Key([ushort]$Key) {
    [HostedInput]::Key($Key, $false)
    try { Start-Sleep -Milliseconds 30 } finally { [HostedInput]::Key($Key, $true) }
}
function Tap-Shift([int]$Count) {
    for ($index = 0; $index -lt $Count; $index++) { Tap-Key 0xA0; Start-Sleep -Milliseconds 100 }
}
function Assert-Value($Pattern, [string]$Expected, [string]$Name) {
    try { Wait-Condition { $Pattern.Current.Value -eq $Expected } "$Name; expected '$Expected'" }
    catch { throw "$Name; expected '$Expected'; actual '$($Pattern.Current.Value)'" }
    Record-Pass $Name
}
function Quit-Owned($Process, $Window) {
    (Get-Pattern (Find-Control $Window 'Exit') ([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
    if (-not $Process.WaitForExit(8000)) { throw 'Application did not exit within cleanup deadline' }
    if ($Process.ExitCode -ne 0) { throw "Application exit code: $($Process.ExitCode)" }
}

$fixtureStdout = $null
$fixtureStderr = $null
$journalFixtureStdout = $null
$journalFixtureStderr = $null
try {
    [ordered]@{
        os = (Get-CimInstance Win32_OperatingSystem).Caption
        machine = (Get-CimInstance Win32_ComputerSystem).Model
        session = (Get-Process -Id $PID).SessionId
        inputSize = [HostedInput]::InputSize()
    } | ConvertTo-Json | Write-Output
    $debugProfile = Join-Path $runRoot 'debug-profile'
    $debug = Start-Switcher $DebugExe $debugProfile $true
    Wait-Condition { $null -ne (Find-AppWindow $debug) } 'debug main window'
    $window = Find-AppWindow $debug
    Focus-App $window
    Toggle-Control $window 'Smarter hotkeys' $true
    $debugConfig = Join-Path $debugProfile 'RustSwitcherDebug/config.json'
    Assert-ConfigFlag $debugConfig 'smarter_hotkeys_enabled' $true
    $savedConfig = Get-Content -LiteralPath $debugConfig -Raw
    $lastWordEdit = [System.Windows.Automation.AutomationElement]::FromHandle(
        [HostedInput]::GetDlgItem([IntPtr]$window.Current.NativeWindowHandle, 1201))
    $lastWordValue = Get-Pattern $lastWordEdit ([System.Windows.Automation.ValuePattern]::Pattern)
    $savedLastWord = $lastWordValue.Current.Value
    Focus-SettingsEditor $window $lastWordEdit
    Tap-Key 0x77 # F8 is a draft only until Apply.
    Wait-Condition { $lastWordValue.Current.Value -match 'F8' } 'pending hotkey edit'
    $minimized = Get-Pattern (Find-Control $window 'Start minimized') ([System.Windows.Automation.TogglePattern]::Pattern)
    $heldConfig = [System.IO.File]::Open($debugConfig, [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    try {
        $minimized.Toggle()
        Wait-Condition { $minimized.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::Off } 'failed save restores checkbox'
        if ((Get-Content -LiteralPath $debugConfig -Raw) -ne $savedConfig) { throw 'Failed save changed config bytes' }
        if ($lastWordValue.Current.Value -notmatch 'F8') { throw 'Failed save changed the hotkey draft' }
        Record-Pass 'failed checkbox save preserves settings and drafts'
    } finally {
        $heldConfig.Dispose()
    }
    foreach ($setting in @(
        @('Start minimized', 'start_minimized', $true),
        @('Start minimized', 'start_minimized', $false),
        @('Dark theme', 'theme_dark', $true),
        @('Dark theme', 'theme_dark', $false),
        @('Smarter hotkeys', 'smarter_hotkeys_enabled', $false),
        @('Smarter hotkeys', 'smarter_hotkeys_enabled', $true),
        @('Enable AutoConvert', 'autoconvert_feature_enabled', $false),
        @('Enable AutoConvert', 'autoconvert_feature_enabled', $true)
    )) {
        Toggle-Control $window $setting[0] $setting[2]
        Assert-ConfigFlag $debugConfig $setting[1] $setting[2]
        if ($lastWordValue.Current.Value -notmatch 'F8') { throw 'Checkbox click discarded the hotkey draft' }
    }
    if ((Get-Content -LiteralPath $debugConfig -Raw) -ne $savedConfig) {
        throw 'Checkbox clicks saved an unapplied hotkey edit'
    }
    Toggle-Control $window 'Dark theme' $true
    Assert-ConfigFlag $debugConfig 'theme_dark' $true
    (Get-Pattern (Find-Control $window 'Cancel') ([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
    Assert-Value $lastWordValue $savedLastWord 'Cancel discards only the hotkey draft'
    Assert-ConfigFlag $debugConfig 'theme_dark' $true
    $theme = Get-Pattern (Find-Control $window 'Dark theme') ([System.Windows.Automation.TogglePattern]::Pattern)
    if ($theme.Current.ToggleState -ne [System.Windows.Automation.ToggleState]::On) { throw 'Cancel reverted a saved checkbox' }
    Record-Pass 'checkboxes save both states immediately and preserve hotkey drafts'
    Focus-SettingsEditor $window $lastWordEdit
    Tap-Key 0x77
    Wait-Condition { $lastWordValue.Current.Value -match 'F8' } 'new hotkey draft for Apply'
    Apply-Settings $window
    Wait-Condition { (Get-Content -LiteralPath $debugConfig -Raw) -match '(?s)\[hotkey_convert_last_word_sequence.first\].*?vk = 119' } 'Apply saves the new hotkey'
    Assert-ConfigFlag $debugConfig 'theme_dark' $true
    Focus-SettingsEditor $window $lastWordEdit
    Tap-Shift 2
    Assert-Value $lastWordValue $savedLastWord 'restore original hotkey draft'
    Apply-Settings $window
    Wait-Condition {
        ((Get-Content -LiteralPath $debugConfig -Raw) -replace 'theme_dark = true', 'theme_dark = false') -eq $savedConfig
    } 'Apply restores original hotkeys and preserves saved settings'
    $playground = Find-Playground $window
    $playground.SetFocus()
    $value = Get-Pattern $playground ([System.Windows.Automation.ValuePattern]::Pattern)
    $english = [HostedInput]::LoadKeyboardLayout('00000409', 0)
    $russian = [HostedInput]::LoadKeyboardLayout('00000419', 0)
    if ($english -eq [IntPtr]::Zero -or $russian -eq [IntPtr]::Zero) {
        throw 'Required English/Russian test layouts could not be loaded'
    }
    if ($english -eq [IntPtr]::Zero) { throw 'English keyboard layout unavailable' }
    [HostedInput]::PostMessage([IntPtr]$window.Current.NativeWindowHandle, 0x50, [UIntPtr]::Zero, $english) | Out-Null
    Start-Sleep -Milliseconds 300
    foreach ($character in 'ghbdtn'.ToUpperInvariant().ToCharArray()) { Tap-Key ([ushort]$character) }
    Assert-Value $value 'ghbdtn' 'debug real keyboard input'
    Tap-Shift 3
    Assert-Value $value 'привет' 'debug triple Shift smart last-word conversion'
    Tap-Shift 2
    Assert-Value $value 'ghbdtn' 'debug deferred double Shift reverse conversion'
    Quit-Owned $debug $window
    Record-Pass 'debug cleanup and exit'
    foreach ($scenario in @('sequence', 'autoconvert')) {
        $debug = Start-Switcher $DebugExe $debugProfile $true
        Wait-Condition { $null -ne (Find-AppWindow $debug) } "debug $scenario startup"
        $window = Find-AppWindow $debug
        Focus-App $window
        foreach ($name in @('Dark theme', 'Smarter hotkeys', 'Enable AutoConvert')) {
            $toggle = Get-Pattern (Find-Control $window $name) ([System.Windows.Automation.TogglePattern]::Pattern)
            if ($toggle.Current.ToggleState -ne [System.Windows.Automation.ToggleState]::On) { throw "$name did not survive restart" }
        }
        $playground = Find-Playground $window
        $playground.SetFocus()
        $value = Get-Pattern $playground ([System.Windows.Automation.ValuePattern]::Pattern)
        [HostedInput]::PostMessage([IntPtr]$window.Current.NativeWindowHandle, 0x50, [UIntPtr]::Zero, $english) | Out-Null
        Start-Sleep -Milliseconds 300
        if ($scenario -eq 'autoconvert') {
            Tap-Key 0xA3; Start-Sleep -Milliseconds 100; Tap-Key 0xA3
            Start-Sleep -Milliseconds 300
            $appHandle = [IntPtr]$window.Current.NativeWindowHandle
            $cancelDraft = [System.Windows.Automation.AutomationElement]::FromHandle(
                [HostedInput]::GetDlgItem($appHandle, 1201))
            $cancelValue = Get-Pattern $cancelDraft ([System.Windows.Automation.ValuePattern]::Pattern)
            $originalHotkey = $cancelValue.Current.Value
            $cancelDraft.SetFocus()
            Wait-Condition { $cancelDraft.Current.HasKeyboardFocus } 'hotkey draft focus while AutoConvert is active'
            Tap-Key 0x77
            Wait-Condition { $cancelValue.Current.Value -match 'F8' } 'hotkey draft while AutoConvert is active'
            (Get-Pattern (Find-Control $window 'Cancel') ([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
            Assert-Value $cancelValue $originalHotkey 'Cancel completed while AutoConvert is active'
            $playground.SetFocus()
            $playgroundHandle = [IntPtr]$playground.Current.NativeWindowHandle
            Wait-Condition {
                $playground.Current.HasKeyboardFocus -and
                [HostedInput]::WindowFocus($appHandle) -eq $playgroundHandle
            } 'playground focus restored after Cancel'
            [HostedInput]::PostMessage($appHandle, 0x50, [UIntPtr]::Zero, $english) | Out-Null
            Wait-Condition { [HostedInput]::WindowLayout($appHandle) -eq $english } 'English layout restored after Cancel'
            foreach ($character in 'GHBDTN'.ToCharArray()) { Tap-Key ([ushort]$character) }
            Assert-Value $value 'ghbdtn' 'debug input delivered after Cancel'
            Tap-Key 0x20
            Assert-Value $value 'привет ' 'debug Cancel preserves active AutoConvert'
        } else {
            foreach ($character in 'GHBDTN VBH'.ToCharArray()) { Tap-Key ([ushort]$character) }
            Assert-Value $value 'ghbdtn vbh' 'debug last-sequence initial input'
            Tap-Key 0xA4; Start-Sleep -Milliseconds 100; Tap-Key 0xA4
            Assert-Value $value 'привет мир' 'debug double Alt last-sequence conversion'
        }
        Quit-Owned $debug $window
    }

    $journalManifest = Join-Path $runRoot 'journal-fixture.json'
    $journalInfo = [System.Diagnostics.ProcessStartInfo]::new('powershell.exe')
    $journalInfo.UseShellExecute = $false
    $journalInfo.CreateNoWindow = $true
    $journalInfo.RedirectStandardOutput = $true
    $journalInfo.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-STA', '-File', (Join-Path $PSScriptRoot 'journal-fixture.ps1'),
        '-ManifestPath', $journalManifest)) {
        $journalInfo.ArgumentList.Add($argument)
    }
    $journalFixture = [System.Diagnostics.Process]::Start($journalInfo)
    $ownedProcesses.Add($journalFixture)
    $journalFixtureStdout = $journalFixture.StandardOutput.ReadToEndAsync()
    $journalFixtureStderr = $journalFixture.StandardError.ReadToEndAsync()
    Wait-Condition {
        if ($journalFixture.HasExited) { throw "Journal fixture exited before opening: $($journalFixture.ExitCode)" }
        if (-not (Test-Path -LiteralPath $journalManifest)) { return $false }
        try { return (Get-Content -LiteralPath $journalManifest -Raw | ConvertFrom-Json).generation -gt 0 }
        catch { return $false }
    } 'unowned journal fixture windows' 30000
    $debug = Start-Switcher $DebugExe (Join-Path $runRoot 'journal-profile') $true -JournalFixturePid $journalFixture.Id
    Wait-Condition { $null -ne (Find-AppWindow $debug) } 'debug journal matrix startup'
    $window = Find-AppWindow $debug
    Focus-App $window
    Toggle-Control $window 'Smarter hotkeys' $true
    $playground = Find-Playground $window
    & (Join-Path $PSScriptRoot 'journal-cases.ps1') -DebugProcess $debug -AppWindow $window `
        -AppPlayground $playground -EnglishLayout $english -FixtureProcess $journalFixture `
        -ManifestPath $journalManifest -Failures $scenarioFailures -Results $journalCases
    Quit-Owned $debug $window
    Record-Pass 'debug external journal fixture cleanup and exit'

    $releaseProfile = Join-Path $runRoot 'release-profile'
    $release = Start-Switcher $ReleaseExe $releaseProfile $false
    Wait-Condition { $null -ne (Find-AppWindow $release) } 'release main window'
    $window = Find-AppWindow $release
    Focus-App $window
    Record-Pass 'release application startup'
    $originalPid = $release.Id
    $second = Start-Switcher $ReleaseExe $releaseProfile $false
    if (-not $release.WaitForExit(8000)) { throw 'Relaunch did not stop the original process' }
    Wait-Condition { -not $second.HasExited -and $null -ne (Find-AppWindow $second) } 'replacement main window'
    $release = $second
    $window = Find-AppWindow $release
    Record-Pass 'release relaunch replaces the original PID'

    # Remove the real shell registration, then use an actual broadcast. Sending
    # TaskbarCreated directly to the app would incorrectly pass with HWND_MESSAGE.
    $trayWindow = [IntPtr]$window.Current.NativeWindowHandle
    [HostedInput]::ShowWindow($trayWindow, 0) | Out-Null
    $taskbarCreated = [HostedInput]::RegisterWindowMessage('TaskbarCreated')
    foreach ($event in @(
        @{ Name = 'TaskbarCreated'; Message = $taskbarCreated; Value = 0 },
        @{ Name = 'resume'; Message = 0x218; Value = 0x12 },
        @{ Name = 'display change'; Message = 0x7e; Value = 32 },
        @{ Name = 'repeated TaskbarCreated'; Message = $taskbarCreated; Value = 0 }
    )) {
        Wait-Condition { [HostedInput]::TrayIconPresent($trayWindow) } 'owned tray registration'
        [HostedInput]::DeleteTrayIcon($trayWindow)
        [HostedInput]::Broadcast($event.Message, $event.Value)
        Wait-Condition { [HostedInput]::TrayIconPresent($trayWindow) } "tray recovery after $($event.Name)"
        if ($release.HasExited) { throw "Application exited during $($event.Name)" }
        Record-Pass "release hidden-window tray recovery after $($event.Name)"
    }
    [HostedInput]::ShowWindow($trayWindow, 9) | Out-Null

    $staleQuit = Start-Switcher $ReleaseExe $releaseProfile $false -ExtraArguments @('--quit-for-update', '--parent-pid', "$originalPid")
    if (-not $staleQuit.WaitForExit(3000) -or $staleQuit.ExitCode -eq 0 -or $release.HasExited) {
        throw 'A stale updater shutdown was not rejected without touching the replacement'
    }
    $ensure = Start-Switcher $ReleaseExe $releaseProfile $false -ExtraArguments @('--ensure-running')
    if (-not $ensure.WaitForExit(3000) -or $ensure.ExitCode -ne 0 -or $release.HasExited) {
        throw 'Updater recovery replaced an already running application'
    }
    Record-Pass 'stale updater and updater recovery preserve the replacement PID'

    [HostedInput]::SuspendOwnedWindowThread($trayWindow, $release.Id)
    $replacement = Start-Switcher $ReleaseExe $releaseProfile $false
    if (-not $release.WaitForExit(8000)) { throw 'Relaunch did not terminate the unresponsive predecessor' }
    Wait-Condition { -not $replacement.HasExited -and $null -ne (Find-AppWindow $replacement) } 'replacement after suspended UI thread'
    $release = $replacement
    Record-Pass 'release relaunch replaces an unresponsive predecessor'

    $previous = $release
    $rapid = @(1..3 | ForEach-Object { Start-Switcher $ReleaseExe $releaseProfile $false })
    Wait-Condition {
        $running = @($rapid | Where-Object { -not $_.HasExited })
        $previous.HasExited -and $running.Count -eq 1 -and $null -ne (Find-AppWindow $running[0])
    } 'rapid relaunches converge to one new process' 20000
    foreach ($process in $rapid) {
        if ($process.HasExited -and $process.ExitCode -ne 0) { throw 'A rapid relaunch failed instead of handing over' }
    }
    $release = @($rapid | Where-Object { -not $_.HasExited })[0]
    $window = Find-AppWindow $release
    $trayWindow = [IntPtr]$window.Current.NativeWindowHandle
    Wait-Condition { [HostedInput]::TrayIconPresent($trayWindow) } 'tray after rapid relaunches'
    Focus-App $window
    Record-Pass 'rapid relaunches leave one usable process and tray icon'
    $editorInfo = [System.Diagnostics.ProcessStartInfo]::new('powershell.exe')
    $editorInfo.UseShellExecute = $false
    $editorInfo.CreateNoWindow = $true
    $editorInfo.RedirectStandardOutput = $true
    $editorInfo.RedirectStandardError = $true
    $signalPath = Join-Path $runRoot 'editor-signal.txt'
    $editorInfo.Environment['RUST_SWITCHER_E2E_SIGNAL_PATH'] = $signalPath
    foreach ($argument in @('-NoProfile', '-STA', '-File', (Join-Path $PSScriptRoot 'editor-fixture.ps1'))) {
        $editorInfo.ArgumentList.Add($argument)
    }
    $editor = [System.Diagnostics.Process]::Start($editorInfo)
    $ownedProcesses.Add($editor)
    $fixtureStdout = $editor.StandardOutput.ReadToEndAsync()
    $fixtureStderr = $editor.StandardError.ReadToEndAsync()
    Wait-Condition {
        if ($editor.HasExited) { throw "Editor fixture exited before opening: $($editor.ExitCode)" }
        $null -ne (Find-AppWindow $editor)
    } 'isolated external editor (including cold framework/compiler startup)' 30000
    $editorWindow = Find-AppWindow $editor
    # The fixture is hosted by a console process; WaitForInputIdle rejects it.
    Wait-Condition { [HostedInput]::Responsive([IntPtr]$editorWindow.Current.NativeWindowHandle) } 'editor message loop'
    Focus-App $editorWindow
    $editCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ClassNameProperty, 'Edit')
    $edit = $editorWindow.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $editCondition)
    if ($null -eq $edit) { throw 'Native Edit fixture not found' }
    $edit.SetFocus()
    $editValue = Get-Pattern $edit ([System.Windows.Automation.ValuePattern]::Pattern)
    $editValue.SetValue('ghbdtn')
    Wait-Condition { $editValue.Current.Value -eq 'ghbdtn' } 'fixture initial text'
    $edit.SetFocus()
    Wait-Condition { $edit.Current.HasKeyboardFocus } 'fixture keyboard focus'
    Write-Output "Fixture native handle=$($edit.Current.NativeWindowHandle); value='$($editValue.Current.Value)'; focused=$($edit.Current.HasKeyboardFocus)"
    [HostedInput]::SelectFixtureText([IntPtr]$edit.Current.NativeWindowHandle, 6)
    $clipboardBefore = [HostedInput]::GetClipboardSequenceNumber()
    if ($clipboardBefore -eq 0) { throw 'Clipboard sequence is unavailable' }
    Tap-Shift 2
    Assert-Value $editValue 'привет' 'release selected text converted in external editor'
    Wait-Condition { [HostedInput]::HasSelection([IntPtr]$edit.Current.NativeWindowHandle, 6) } 'converted selection restored'
    Tap-Shift 2
    Assert-Value $editValue 'ghbdtn' 'release selected text reverse conversion'
    Wait-Condition { [HostedInput]::HasSelection([IntPtr]$edit.Current.NativeWindowHandle, 6) } 'reverse converted selection restored'
    $clipboardAfter = [HostedInput]::GetClipboardSequenceNumber()
    if ($clipboardAfter -eq 0) { throw 'Clipboard sequence became unavailable' }
    if ($clipboardAfter -ne $clipboardBefore) { throw 'Selection conversion changed the clipboard' }
    Record-Pass 'release selection leaves clipboard unchanged'
    # Either a complete conversion or refusal is safe at the UTF-16/character limit.
    # Partial replacement would destroy the rest of the selected text.
    $unicodeInput = ('a' * 500) + ('😀' * 10)
    $unicodeExpected = ('ф' * 500) + ('😀' * 10)
    $editValue.SetValue($unicodeInput)
    Wait-Condition { $editValue.Current.Value -eq $unicodeInput } 'fixture complete Unicode text'
    $edit.SetFocus()
    [HostedInput]::SelectFixtureText([IntPtr]$edit.Current.NativeWindowHandle, $unicodeInput.Length)
    Tap-Shift 2
    Start-Sleep -Milliseconds 1500
    $unicodeActual = $editValue.Current.Value
    Write-Output "Unicode boundary: input UTF-16=$($unicodeInput.Length), output=$($unicodeActual.Length), converted=$($unicodeActual -eq $unicodeExpected), unchanged=$($unicodeActual -eq $unicodeInput)"
    if ($unicodeActual -ne $unicodeInput -and $unicodeActual -ne $unicodeExpected) {
        $scenarioFailures.Add("Unicode selection was partially replaced: input UTF-16 length=$($unicodeInput.Length), output=$($unicodeActual.Length)")
    } else {
        # The replacement text arrives before the queued caret-restoration
        # keys finish. Require the complete final selection before changing
        # the fixture for recovery, including when conversion safely refused.
        Wait-Condition {
            if ($editValue.Current.Value -ne $unicodeActual) {
                throw 'Unicode selection text changed after its accepted result'
            }
            [HostedInput]::HasSelection([IntPtr]$edit.Current.NativeWindowHandle, $unicodeInput.Length)
        } 'complete Unicode selection restoration before recovery'
        Record-Pass 'release Unicode selection boundary preserves complete text'
    }
    if ($release.HasExited -or -not [HostedInput]::Responsive([IntPtr]$window.Current.NativeWindowHandle)) {
        throw 'Release app stopped responding after Unicode selection'
    }
    $editValue.SetValue('ghbdtn')
    Wait-Condition { $editValue.Current.Value -eq 'ghbdtn' } 'fixture recovery text'
    [HostedInput]::SelectFixtureText([IntPtr]$edit.Current.NativeWindowHandle, 6)
    Tap-Shift 2
    Assert-Value $editValue 'привет' 'release short selection still works after Unicode boundary'
    Wait-Condition { [HostedInput]::HasSelection([IntPtr]$edit.Current.NativeWindowHandle, 6) } 'recovery selection restored'
    $editHandle = [IntPtr]$edit.Current.NativeWindowHandle
    $otherHandle = [HostedInput]::GetDlgItem([IntPtr]$editorWindow.Current.NativeWindowHandle, 1002)
    if ($otherHandle -eq [IntPtr]::Zero) { throw 'Secondary input target not found' }
    $otherEdit = [System.Windows.Automation.AutomationElement]::FromHandle($otherHandle)
    $otherValue = Get-Pattern $otherEdit ([System.Windows.Automation.ValuePattern]::Pattern)
    foreach ($mode in @(1, 2, 3)) {
        # Each scenario starts after the preceding modifier-sequence window closes.
        Start-Sleep -Milliseconds 1200
        $editValue.SetValue('ghbdtn')
        $otherValue.SetValue('untouched')
        $edit.SetFocus()
        Wait-Condition { $edit.Current.HasKeyboardFocus } 'adversarial fixture focus'
        [HostedInput]::SelectFixtureText($editHandle, 6)
        Set-Content -LiteralPath $signalPath -Value 'ready' -NoNewline
        if ($mode -eq 3) {
            [HostedInput]::PostMessage($editHandle, 0x8011, [UIntPtr]3, [IntPtr]::Zero) | Out-Null
            Wait-Condition { (Get-Content -LiteralPath $signalPath -Raw) -eq 'blocked' } 'editor deliberately blocked'
        } else {
            [HostedInput]::ArmFixture($editHandle, $mode)
        }
        Tap-Shift 2
        if ($mode -eq 3) {
            Start-Sleep -Milliseconds 500
            if ((Get-Content -LiteralPath $signalPath -Raw) -ne 'blocked') { throw 'Hung-editor check missed the blocked interval' }
            if (-not [HostedInput]::Responsive([IntPtr]$window.Current.NativeWindowHandle)) {
                throw 'Release UI blocked behind an unresponsive editor'
            }
            Record-Pass 'release remains responsive while external editor is hung'
            Wait-Condition { (Get-Content -LiteralPath $signalPath -Raw) -eq 'released' } 'editor resumes'
        } else {
            Wait-Condition { (Get-Content -LiteralPath $signalPath -Raw) -eq "changed-$mode" } 'selection or focus race actually occurred'
        }
        Start-Sleep -Milliseconds 600
        if ($editValue.Current.Value -ne 'ghbdtn' -or $otherValue.Current.Value -ne 'untouched') {
            throw "Stale or blocked selection injected text (mode $mode)"
        }
        Record-Pass "release cancels unsafe selection (mode $mode)"
        $edit.SetFocus()
        [HostedInput]::SelectFixtureText($editHandle, 6)
        Tap-Shift 2
        Assert-Value $editValue 'привет' "release conversion recovers after adversarial mode $mode"
        Wait-Condition { [HostedInput]::HasSelection($editHandle, 6) } 'adversarial recovery input completed'
    }

    # Restart without changing the default Double Shift binding, then exercise
    # a complete phrase with a fresh selection provider.
    Quit-Owned $release $window
    $release = Start-Switcher $ReleaseExe $releaseProfile $false
    Wait-Condition { $null -ne (Find-AppWindow $release) } 'release Double Shift restart'
    $window = Find-AppWindow $release

    # Prepopulated two-word text cannot be recovered from the typing journal.
    # Do not retry the hotkey or restore focus/selection after either gesture.
    $phrase = 'ghbdtn vbh'
    $editValue.SetValue($phrase)
    $otherValue.SetValue('untouched')
    $editorHandle = [IntPtr]$editorWindow.Current.NativeWindowHandle
    Set-OwnedControlFocus $editorWindow $edit | Out-Null
    [HostedInput]::SelectFixtureText($editHandle, $phrase.Length)
    $selectionClipboard = [HostedInput]::GetClipboardSequenceNumber()
    if ($selectionClipboard -eq 0) { throw 'Clipboard sequence is unavailable before Double Shift' }
    foreach ($expected in @('привет мир', $phrase)) {
        Tap-Shift 2
        Wait-Condition { $editValue.Current.Value -ceq $expected } 'one Double Shift converts the complete selected phrase'
        Wait-Condition { [HostedInput]::HasSelection($editHandle, $expected.Length) } 'Double Shift restores the complete selection'
        # Catch a duplicate queued command reversing a briefly correct result.
        Start-Sleep -Milliseconds 150
        if ($editValue.Current.Value -cne $expected -or
            -not [HostedInput]::HasSelection($editHandle, $expected.Length)) {
            throw 'One Double Shift did not produce one stable phrase conversion and reselection'
        }
        if ([HostedInput]::GetForegroundWindow() -ne $editorHandle -or
            [HostedInput]::WindowFocus($editorHandle) -ne $editHandle) {
            throw 'Double Shift moved focus away from the selected phrase'
        }
        if ($otherValue.Current.Value -cne 'untouched') { throw 'Double Shift changed the other editor field' }
        if ([HostedInput]::GetClipboardSequenceNumber() -ne $selectionClipboard) { throw 'Double Shift changed the clipboard' }
        Record-Pass "release Double Shift converts exactly once with selection and focus preserved: $expected"
    }
    # Windows denies cross-process reads of a native password Edit. Exercise
    # actual typed-journal conversion instead, with the existing debug-only
    # injected-input opt-in restricted to this owned editor process. The release
    # selection path has already been verified above without that opt-in.
    Quit-Owned $release $window
    $passwordProfile = Join-Path $runRoot 'native-password-profile'
    $passwordOutputStart = $switcherOutputTasks.Count
    $debug = Start-Switcher $DebugExe $passwordProfile $true -JournalFixturePid $editor.Id
    Wait-Condition { $null -ne (Find-AppWindow $debug) } 'native password journal startup'
    $passwordWindow = Find-AppWindow $debug
    $passwordAppHandle = [IntPtr]$passwordWindow.Current.NativeWindowHandle
    Set-OwnedControlFocus $editorWindow $edit | Out-Null
    if ([HostedInput]::SecurityMetadata($passwordAppHandle, 0) -ne 0x52535031 -or
        [HostedInput]::SecurityMetadata($passwordAppHandle, 4) -ne 0) {
        throw 'Native password journal metadata or default AutoConvert state is invalid'
    }
    function Initialize-NativePassword([string]$Text, [uint]$Sample) {
        [HostedInput]::SetFixtureText($editHandle, '')
        [HostedInput]::SetFixtureText($otherHandle, 'untouched')
        if (-not [HostedInput]::PostMessage($editorHandle, 0x50, [UIntPtr]::Zero, $english)) {
            throw 'Native password English layout request was rejected'
        }
        Wait-Condition { [HostedInput]::WindowLayout($editorHandle) -eq $english } 'native password English layout'
        Set-OwnedControlFocus $editorWindow $edit | Out-Null
        Tap-Key 0x1B
        $uiaPassword = $edit.Current.IsPassword
        $fixtureEmpty = [HostedInput]::PasswordFixtureTextMatches($editHandle, 0)
        $activeChars = [HostedInput]::SecurityMetadata($passwordAppHandle, 1)
        Write-Output "Native password setup: IsPassword=$uiaPassword fixtureEmpty=$fixtureEmpty activeChars=$activeChars"
        if (-not $uiaPassword) { throw 'Refreshed native password UIA element is not classified as IsPassword' }
        if (-not $fixtureEmpty) { throw 'Native password fixture did not empty after explicit text reset' }
        Wait-Condition { [HostedInput]::SecurityMetadata($passwordAppHandle, 1) -eq 0 } 'empty native password journal after Escape'
        $capturedBefore = [HostedInput]::SecurityMetadata($passwordAppHandle, 3)
        foreach ($character in $Text.ToCharArray()) {
            if ($character -ne ' ' -and ($character -lt 'a' -or $character -gt 'z')) {
                throw 'Unsupported native password fixture key'
            }
            Tap-Key ([ushort][char]::ToUpperInvariant($character))
        }
        Wait-Condition { [HostedInput]::PasswordFixtureTextMatches($editHandle, $Sample) } 'complete native password fixture input'
        Wait-Condition { [HostedInput]::SecurityMetadata($passwordAppHandle, 3) -eq $capturedBefore + $Text.Length } 'complete native password journal capture'
        if ([HostedInput]::SecurityMetadata($passwordAppHandle, 1) -ne $Text.Length -or
            -not [HostedInput]::HasSelectionRange($editHandle, $Text.Length, $Text.Length)) {
            throw 'Native password journal or caret does not match the complete typed input'
        }
    }
    $passwordFailure = $null
    [HostedInput]::SetFixturePassword($editHandle, $true)
    # The native UIA Edit proxy caches its control type when it is created.
    # Rebind after changing the style: the earlier plain Edit element can keep
    # reporting IsPassword=false even through its Current property accessor.
    $edit = [System.Windows.Automation.AutomationElement]::FromHandle($editHandle)
    try {
        Initialize-NativePassword 'ghbdtn' 1
        $passwordClipboard = [HostedInput]::GetClipboardSequenceNumber()
        if ($passwordClipboard -eq 0) { throw 'Password-test clipboard sequence is unavailable' }
        foreach ($expected in @(@{ Sample = 2; Direction = 'EN to RU' }, @{ Sample = 1; Direction = 'RU to EN' })) {
            Tap-Shift 2
            Wait-Condition { [HostedInput]::PasswordFixtureTextMatches($editHandle, $expected.Sample) } 'native password journal converts exactly'
            Wait-Condition { [HostedInput]::HasSelectionRange($editHandle, 6, 6) } 'native password caret after conversion'
            Start-Sleep -Milliseconds 150
            if (-not [HostedInput]::PasswordFixtureTextMatches($editHandle, $expected.Sample)) {
                throw 'Native password conversion was not stable'
            }
            if ([HostedInput]::GetForegroundWindow() -ne $editorHandle -or
                [HostedInput]::WindowFocus($editorHandle) -ne $editHandle -or
                [HostedInput]::FixtureText($otherHandle) -cne 'untouched' -or
                [HostedInput]::GetClipboardSequenceNumber() -ne $passwordClipboard) {
                throw 'Native password conversion changed focus, another field or clipboard'
            }
            Record-Pass "native password journal converts exactly with default Double Shift: $($expected.Direction)"
        }
        Initialize-NativePassword 'ghbdtn ' 3
        Set-Content -LiteralPath $signalPath -Value 'ready' -NoNewline
        [HostedInput]::DoubleShiftThenTab()
        Wait-Condition { (Get-Content -LiteralPath $signalPath -Raw) -ceq 'tab' -and
            [HostedInput]::WindowFocus($editorHandle) -eq $otherHandle } 'actual native Tab reaches the neighboring editor'
        Start-Sleep -Milliseconds 1600
        if ([HostedInput]::FixtureText($otherHandle) -cne 'untouched') {
            throw 'WRONG_TARGET_WRITE: native password conversion reached the field selected by Tab'
        }
        if (-not [HostedInput]::PasswordFixtureTextMatches($editHandle, 3) -and
            -not [HostedInput]::PasswordFixtureTextMatches($editHandle, 4)) {
            throw 'PARTIAL_SOURCE_WRITE: native shortcut/Tab partially changed the password source'
        }
        if ([HostedInput]::GetClipboardSequenceNumber() -ne $passwordClipboard) {
            throw 'Native password shortcut/Tab changed the clipboard'
        }
        Record-Pass 'native password Double Shift followed immediately by Tab preserves source ownership'
    } catch {
        $passwordFailure = $_
        throw
    } finally {
        try { [HostedInput]::SetFixturePassword($editHandle, $false) }
        catch {
            if ($null -eq $passwordFailure) { throw }
            Write-Warning "Native password fixture cleanup also failed: $($_.Exception.Message)"
        }
    }
    Quit-Owned $debug $passwordWindow
    $passwordDiagnostics = ''
    foreach ($outputTask in $switcherOutputTasks.ToArray()[$passwordOutputStart..($passwordOutputStart + 1)]) {
        if (-not $outputTask.Wait(1000)) { throw 'Native password diagnostics did not drain after exit' }
        $passwordDiagnostics += $outputTask.Result
    }
    if ([string]::IsNullOrWhiteSpace($passwordDiagnostics)) { throw 'Native password redaction check requires actual diagnostics' }
    $passwordConfig = Get-Content -LiteralPath (Join-Path $passwordProfile 'RustSwitcherDebug/config.json') -Raw
    foreach ($sample in @('ghbdtn', 'привет')) {
        if ($passwordDiagnostics.Contains($sample) -or $passwordConfig.Contains($sample)) {
            throw 'INPUT_LOG_VIOLATION: native password diagnostics or saved profile contain synthetic input'
        }
    }
    Record-Pass 'native password diagnostics and saved profile exclude synthetic input'
    $release = Start-Switcher $ReleaseExe $releaseProfile $false
    Wait-Condition { $null -ne (Find-AppWindow $release) } 'release restart after native password checks'
    $window = Find-AppWindow $release
    # Closing our fixture through its real window message loop leaves no input target behind.
    [HostedInput]::PostMessage([IntPtr]$editorWindow.Current.NativeWindowHandle, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    if (-not $editor.WaitForExit(5000)) { throw 'Isolated editor did not close' }
    # Settings use addressed UI Automation patterns; they do not need foreground input.
    Toggle-Control $window 'Dark theme' $true
    $config = Join-Path $releaseProfile 'RustSwitcher/config.json'
    Assert-ConfigFlag $config 'theme_dark' $true
    Record-Pass 'release settings saved without Apply'
    if ($null -ne (Get-AutostartCommand)) {
        throw 'Unexpected pre-existing autostart entry on fresh VM'
    }
    Toggle-Control $window 'Start on startup' $true
    Wait-Condition { (Get-AutostartCommand) -eq ('"' + $ReleaseExe + '" --autostart') } 'autostart registry target'
    Toggle-Control $window 'Start on startup' $false
    Wait-Condition { $null -eq (Get-AutostartCommand) } 'autostart removal'
    Record-Pass 'release autostart registration and removal'
    Quit-Owned $release $window
    $release = Start-Switcher $ReleaseExe $releaseProfile $false
    Wait-Condition { $null -ne (Find-AppWindow $release) } 'release restart'
    $window = Find-AppWindow $release
    $theme = Get-Pattern (Find-Control $window 'Dark theme') ([System.Windows.Automation.TogglePattern]::Pattern)
    if ($theme.Current.ToggleState -ne [System.Windows.Automation.ToggleState]::On) { throw 'Dark theme did not survive restart' }
    Record-Pass 'release settings reloaded after restart'
    Quit-Owned $release $window
    Record-Pass 'release cleanup and exit'
    if ($scenarioFailures.Count -gt 0) { throw ($scenarioFailures -join '; ') }
} catch {
    $failure = $_.Exception.Message
    Write-Output "FAIL: $failure"
    Write-Output $_.ScriptStackTrace
    throw
} finally {
    $cleanupErrors = [System.Collections.Generic.List[string]]::new()
    foreach ($process in $ownedProcesses) {
        try {
            if (-not $process.HasExited) {
                $process.Kill()
                if (-not $process.WaitForExit(5000)) { $cleanupErrors.Add("Owned process $($process.Id) did not exit") }
            }
        } catch { $cleanupErrors.Add($_.Exception.Message) }
    }
    try {
        $ownedRunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
        $ownedCommand = '"' + $ReleaseExe + '" --autostart'
        if ((Get-AutostartCommand) -eq $ownedCommand) {
            Remove-ItemProperty -LiteralPath $ownedRunKey -Name RustSwitcher
        }
    } catch { $cleanupErrors.Add($_.Exception.Message) }
    foreach ($outputTask in @($fixtureStdout, $fixtureStderr, $journalFixtureStdout, $journalFixtureStderr)) {
        if ($null -ne $outputTask -and $outputTask.Wait(1000)) { Write-Output $outputTask.Result }
    }
    foreach ($outputTask in $switcherOutputTasks) {
        if ($outputTask.Wait(1000)) { Write-Output $outputTask.Result }
    }
    $result = [ordered]@{ passed = $checks.ToArray(); journalCases = $journalCases.ToArray(); failures = $scenarioFailures.ToArray(); failure = $failure; cleanupErrors = $cleanupErrors.ToArray(); commit = $env:GITHUB_SHA; run = $env:GITHUB_RUN_ID }
    $result | ConvertTo-Json -Depth 5 | Write-Output
    $result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $runRoot 'result.json')
    if ($cleanupErrors.Count -gt 0 -and $null -eq $failure) { throw ($cleanupErrors -join '; ') }
    # Only repository-owned fixture data was created; VM teardown disposes it.
}
