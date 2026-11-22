param(
    [Parameter(Mandatory = $true)][string]$DebugExe,
    [string]$ReleaseExe,
    [switch]$SecurityOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# This is an accident guard, not a sandbox. Only the disposable VM is a sandbox.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Browser input checks require the dedicated GitHub-hosted Windows job.'
}
if (-not [Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
    throw 'An interactive Windows session is required.'
}
$DebugExe = (Resolve-Path -LiteralPath $DebugExe).Path
if ($ReleaseExe -and -not $SecurityOnly) { $ReleaseExe = (Resolve-Path -LiteralPath $ReleaseExe).Path }
$runRoot = Join-Path $env:RUNNER_TEMP ('switcher-browser-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runRoot | Out-Null
$results = [System.Collections.Generic.List[object]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
$ownedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$connections = [System.Collections.Generic.List[System.IDisposable]]::new()
$ordinaryOutcomes = [ordered]@{converted = 0; safe_focus_refusal = 0}
$switcherOutput = $null
$switcherErrors = $null
$failure = $null

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Net.WebSockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public sealed class BrowserSocket : IDisposable {
    private readonly ClientWebSocket socket = new ClientWebSocket();
    private int requestId;
    public BrowserSocket(string address) {
        using (var timeout = new CancellationTokenSource(5000))
            socket.ConnectAsync(new Uri(address), timeout.Token).GetAwaiter().GetResult();
    }
    public int NextRequestId() { return Interlocked.Increment(ref requestId); }
    public void Send(string message) {
        using (var timeout = new CancellationTokenSource(5000)) {
            var bytes = Encoding.UTF8.GetBytes(message);
            socket.SendAsync(new ArraySegment<byte>(bytes), WebSocketMessageType.Text, true, timeout.Token).GetAwaiter().GetResult();
        }
    }
    public string Receive() {
        using (var timeout = new CancellationTokenSource(5000))
        using (var stream = new MemoryStream()) {
            var buffer = new byte[8192];
            WebSocketReceiveResult part;
            do {
                part = socket.ReceiveAsync(new ArraySegment<byte>(buffer), timeout.Token).GetAwaiter().GetResult();
                if (part.MessageType == WebSocketMessageType.Close) throw new IOException("Browser debugging connection closed");
                stream.Write(buffer, 0, part.Count);
                if (stream.Length > 1024 * 1024) throw new IOException("Unexpectedly large browser fixture response");
            } while (!part.EndOfMessage);
            return Encoding.UTF8.GetString(stream.ToArray());
        }
    }
    public void Dispose() { socket.Dispose(); }
}
public static class BrowserInput {
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
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int left, top, right, bottom; }
    [StructLayout(LayoutKind.Sequential)] private struct GUI_THREAD_INFO {
        public uint size, flags;
        public IntPtr active, focus, capture, menuOwner, moveSize, caret;
        public RECT caretRect;
    }
    public sealed class FocusSnapshot {
        public long foreground, focus, caret, menuOwner;
        public uint guiFlags;
        public long windowLayout, focusLayout;
        public int left, top, right, bottom, error;
    }
    public sealed class InputTrace {
        public FocusSnapshot before, afterFirst, afterRest;
        public double firstGapMs;
    }
    [DllImport("user32.dll", SetLastError=true)] private static extern uint SendInput(uint count, INPUT[] inputs, int size);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] private static extern IntPtr GetKeyboardLayout(uint thread);
    [DllImport("user32.dll", SetLastError=true)] private static extern bool GetGUIThreadInfo(uint thread, ref GUI_THREAD_INFO info);
    [DllImport("user32.dll", SetLastError=true)] private static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr window, out RECT rect);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowText(IntPtr window, StringBuilder text, int capacity);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetClassName(IntPtr window, StringBuilder name, int capacity);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true, SetLastError=true)]
    private static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr w, IntPtr l,
        uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(IntPtr window, uint message, UIntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr LoadKeyboardLayout(string layout, uint flags);
    private static INPUT Event(ushort key, bool up) {
        var input = new INPUT(); input.type = 1; input.data.keyboard.key = key;
        input.data.keyboard.flags = up ? 2u : 0u; return input;
    }
    private static void Send(INPUT[] inputs) {
        if (SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<INPUT>()) != (uint)inputs.Length)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Incomplete test input batch");
    }
    public static int InputSize() { return Marshal.SizeOf<INPUT>(); }
    public static void Key(ushort key, bool up) { Send(new[] {Event(key, up)}); }
    public static uint ProcessOf(IntPtr window) {
        uint process;
        if (GetWindowThreadProcessId(window, out process) == 0) throw new InvalidOperationException("Window has no process");
        return process;
    }
    public static string TitleOf(IntPtr window) {
        var title = new StringBuilder(512);
        GetWindowText(window, title, title.Capacity);
        return title.ToString();
    }
    public static string ClassOf(IntPtr window) {
        var name = new StringBuilder(256);
        if (window == IntPtr.Zero || GetClassName(window, name, name.Capacity) == 0)
            throw new InvalidOperationException("Focused native window has no class name");
        return name.ToString();
    }
    public static long SecurityMetadata(IntPtr window, uint selector) {
        UIntPtr result;
        if (window == IntPtr.Zero || SendMessageTimeout(window, 0x83F0, new UIntPtr(selector),
            IntPtr.Zero, 2, 200, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Debug security metadata did not respond");
        return checked((long)result.ToUInt64());
    }
    public static IntPtr LayoutOf(IntPtr window) {
        uint process;
        uint thread = GetWindowThreadProcessId(window, out process);
        if (thread == 0) throw new InvalidOperationException("Window has no GUI thread");
        return GetKeyboardLayout(thread);
    }
    public static FocusSnapshot Snapshot(IntPtr window) {
        var result = new FocusSnapshot();
        result.foreground = GetForegroundWindow().ToInt64();
        uint process;
        uint thread = GetWindowThreadProcessId(window, out process);
        result.windowLayout = GetKeyboardLayout(thread).ToInt64();
        var info = new GUI_THREAD_INFO(); info.size = checked((uint)Marshal.SizeOf<GUI_THREAD_INFO>());
        if (!GetGUIThreadInfo(thread, ref info)) { result.error = Marshal.GetLastWin32Error(); return result; }
        result.focus = info.focus.ToInt64(); result.caret = info.caret.ToInt64();
        result.menuOwner = info.menuOwner.ToInt64(); result.guiFlags = info.flags;
        result.left = info.caretRect.left; result.top = info.caretRect.top;
        result.right = info.caretRect.right; result.bottom = info.caretRect.bottom;
        if (info.focus != IntPtr.Zero) {
            uint focusThread = GetWindowThreadProcessId(info.focus, out process);
            result.focusLayout = GetKeyboardLayout(focusThread).ToInt64();
        }
        return result;
    }
    private static ushort VirtualKey(char c) {
        if (c >= 'a' && c <= 'z') return (ushort)Char.ToUpperInvariant(c);
        if (c == '.') return 0xBE;
        if (c == ' ') return 0x20;
        throw new ArgumentException("Unexpected fixture character");
    }
    public static InputTrace Type(IntPtr window, string text, int gapMs, ushort releaseModifier) {
        if (text.Length == 0 || GetForegroundWindow() != window) throw new InvalidOperationException("Invalid typing destination");
        var result = new InputTrace {before = Snapshot(window)};
        ushort first = VirtualKey(text[0]);
        if (releaseModifier == 0) Send(new[] {Event(first, false), Event(first, true)});
        else Send(new[] {Event(releaseModifier, true), Event(first, false), Event(first, true)});
        result.afterFirst = Snapshot(window);
        var clock = System.Diagnostics.Stopwatch.StartNew();
        if (gapMs > 0) Thread.Sleep(gapMs);
        result.firstGapMs = clock.Elapsed.TotalMilliseconds;
        var rest = new INPUT[(text.Length - 1) * 2];
        for (int i = 1; i < text.Length; i++) {
            ushort key = VirtualKey(text[i]);
            rest[(i - 1) * 2] = Event(key, false); rest[(i - 1) * 2 + 1] = Event(key, true);
        }
        if (rest.Length > 0) Send(rest);
        result.afterRest = Snapshot(window);
        return result;
    }
    public static double DoubleShiftThenTab(IntPtr window, int delayMs) {
        if (GetForegroundWindow() != window || delayMs < 0)
            throw new InvalidOperationException("Invalid shortcut/Tab destination");
        var shortcut = new[] {Event(0xA0, false), Event(0xA0, true), Event(0xA0, false), Event(0xA0, true)};
        if (delayMs == 0) {
            Send(new[] {shortcut[0], shortcut[1], shortcut[2], shortcut[3], Event(0x09, false), Event(0x09, true)});
            return 0;
        }
        Send(shortcut);
        var clock = System.Diagnostics.Stopwatch.StartNew();
        Thread.Sleep(delayMs);
        double elapsed = clock.Elapsed.TotalMilliseconds;
        Send(new[] {Event(0x09, false), Event(0x09, true)});
        return elapsed;
    }
    public static void Click(IntPtr window, int x, int y) {
        RECT bounds;
        if (GetForegroundWindow() != window || !GetWindowRect(window, out bounds) ||
            x <= bounds.left || x >= bounds.right || y <= bounds.top || y >= bounds.bottom)
            throw new InvalidOperationException("Click is outside the owned browser window");
        if (!SetCursorPos(x, y)) throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not position fixture pointer");
        var down = new INPUT(); down.type = 0; down.data.mouse.flags = 2;
        var up = down; up.data.mouse.flags = 4;
        Send(new[] {down, up});
    }
    public static InputTrace ClickThenTypeKey(IntPtr window, int x, int y, ushort key) {
        RECT bounds;
        if (GetForegroundWindow() != window || !GetWindowRect(window, out bounds) ||
            x <= bounds.left || x >= bounds.right || y <= bounds.top || y >= bounds.bottom)
            throw new InvalidOperationException("Click is outside the owned browser window");
        if (!SetCursorPos(x, y)) throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not position fixture pointer");
        var result = new InputTrace {before = Snapshot(window)};
        var down = new INPUT(); down.type = 0; down.data.mouse.flags = 2;
        var up = down; up.data.mouse.flags = 4;
        // One ordered native batch: no CDP or guard observation between pointer and key.
        Send(new[] {down, up, Event(key, false), Event(key, true)});
        result.afterFirst = Snapshot(window);
        result.afterRest = result.afterFirst;
        return result;
    }
}
'@
if ([BrowserInput]::InputSize() -ne $(if ([IntPtr]::Size -eq 8) {40} else {28})) {
    throw 'INPUT ABI does not match Windows.'
}

function Wait-Browser([scriptblock]$Condition, [string]$Description, [int]$Milliseconds = 5000) {
    $deadline = [DateTime]::UtcNow.AddMilliseconds($Milliseconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out: $Description"
}
function Invoke-Cdp($Connection, [string]$Method, [hashtable]$Arguments = @{}) {
    # The security cases run in a child script scope. CDP state belongs to the
    # connection, so invoking this helper from another script cannot reset it.
    $id = $Connection.NextRequestId()
    $Connection.Send((@{id = $id; method = $Method; params = $Arguments} | ConvertTo-Json -Depth 8 -Compress))
    do {
        $reply = $Connection.Receive() | ConvertFrom-Json
    } while ($null -eq $reply.PSObject.Properties['id'] -or $reply.id -ne $id)
    if ($null -ne $reply.PSObject.Properties['error']) { throw ($reply.error | ConvertTo-Json -Compress) }
    return $reply.result
}
function Read-Browser($Session, [string]$Expression) {
    $reply = Invoke-Cdp $Session.Connection 'Runtime.evaluate' @{expression = $Expression; returnByValue = $true; awaitPromise = $true}
    if ($null -ne $reply.PSObject.Properties['exceptionDetails']) { throw ($reply.exceptionDetails | ConvertTo-Json -Depth 6 -Compress) }
    if ($null -eq $reply.result.PSObject.Properties['value']) { throw 'Browser fixture returned no value' }
    return $reply.result.value
}
function Focus-Browser($Session) {
    [BrowserInput]::ShowWindow($Session.Handle, 9) | Out-Null
    [BrowserInput]::SetForegroundWindow($Session.Handle) | Out-Null
    if ([BrowserInput]::GetForegroundWindow() -ne $Session.Handle) {
        Invoke-Cdp $Session.Connection 'Page.bringToFront' | Out-Null
    }
    Wait-Browser { [BrowserInput]::GetForegroundWindow() -eq $Session.Handle } 'owned browser foreground'
}
function Tap-BrowserKey([ushort]$Key) {
    [BrowserInput]::Key($Key, $false)
    try { Start-Sleep -Milliseconds 20 } finally { [BrowserInput]::Key($Key, $true) }
}
function Tap-BrowserDoubleShift {
    Tap-BrowserKey 0xA0
    Start-Sleep -Milliseconds 100
    Tap-BrowserKey 0xA0
}

function Assert-DiagnosticRedaction([string]$Text) {
    if ([string]::IsNullOrWhiteSpace($Text)) { throw 'Debug redaction check requires actual diagnostic output' }
    foreach ($secret in @('ghbdtnvbh', 'приветмир')) {
        if ($Text.Contains($secret, [StringComparison]::Ordinal)) {
            throw 'INPUT_LOG_VIOLATION: diagnostics contain the synthetic password or its conversion'
        }
    }
}
function Set-BrowserEnglish($Session) {
    if (-not [BrowserInput]::PostMessage($Session.Handle, 0x50, [UIntPtr]::Zero, $english)) { throw 'Browser layout request failed' }
    Wait-Browser { [BrowserInput]::LayoutOf($Session.Handle) -eq $english } 'browser English keyboard layout'
}
function Focus-BrowserEditor($Session, [string]$Id) {
    Focus-Browser $Session
    # A prior failed hotkey may leave the browser's toolbar focused. Start each
    # independent case with a real click; CDP element.focus() cannot leave native
    # browser chrome. No focus is restored after the tested hotkey.
    $point = Read-Browser $Session "fixture.point('$Id')"
    [BrowserInput]::Click($Session.Handle, $point.x, $point.y)
    Read-Browser $Session "fixture.focus('$Id')" | Out-Null
    Wait-Browser { Read-Browser $Session "fixture.focused('$Id')" } "browser field $Id focus"
}
function Test-BrowserValueWithin($Session, [string]$Field, [string]$Expected, [int]$Milliseconds = 4000) {
    # Only an exhausted value wait returns false. Transport, provider, and
    # process errors propagate and cannot become accepted conservative refusal.
    $deadline = [DateTime]::UtcNow.AddMilliseconds($Milliseconds)
    do {
        if ((Read-Browser $Session 'fixture.values()').$Field -ceq $Expected) { return $true }
        Start-Sleep -Milliseconds 25
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}
function Read-BrowserJournalState([IntPtr]$Window) {
    if ([BrowserInput]::SecurityMetadata($Window, 0) -ne 0x52535031) {
        throw 'The debug executable has no hosted journal inspection API'
    }
    return [pscustomobject]@{
        activeChars = [BrowserInput]::SecurityMetadata($Window, 1)
        cachedChars = [BrowserInput]::SecurityMetadata($Window, 2)
        capturedChars = [BrowserInput]::SecurityMetadata($Window, 3)
        pointerBaseline = [BrowserInput]::SecurityMetadata($Window, 5)
        captureUnavailable = [BrowserInput]::SecurityMetadata($Window, 6)
    }
}
function Get-BrowserFocusSnapshot($Session) {
    return @{native = [BrowserInput]::Snapshot($Session.Handle);
        dom = Read-Browser $Session 'fixture.diagnostics()'}
}
function Assert-BrowserTargetFocus($NativeBefore, $Observed, [string]$Field, [long]$Window) {
    if ($null -eq $NativeBefore -or $null -eq $Observed -or
        $null -eq $Observed.native -or $null -eq $Observed.dom) {
        throw 'Browser target focus proof is missing'
    }
    if ($Window -eq 0 -or $NativeBefore.error -ne 0 -or $NativeBefore.focus -eq 0 -or
        $NativeBefore.foreground -ne $Window -or $Observed.native.error -ne 0 -or
        $Observed.native.foreground -ne $Window -or $Observed.native.focus -ne $NativeBefore.focus -or
        $Observed.dom.documentFocused -isnot [bool] -or -not $Observed.dom.documentFocused -or
        $Observed.dom.field -cne $Field -or $Observed.dom.activeId -cne $Field) {
        throw 'Browser native or DOM focus moved from the original target'
    }
}
function Assert-BrowserOtherFields($Sessions, [hashtable]$ExpectedOther) {
    foreach ($sessionId in @('a', 'b')) {
        $values = Read-Browser $Sessions[$sessionId] 'fixture.values()'
        foreach ($field in @('a', 'b', 'guard')) {
            $key = "$sessionId/$field"
            if ($ExpectedOther.ContainsKey($key) -and $values.$field -cne $ExpectedOther[$key]) {
                throw "Input changed another field ${key}: '$($values.$field)'"
            }
        }
    }
}
function Assert-BrowserSafeFocusRefusal([hashtable]$Case, [System.Collections.IDictionary]$Record,
    $Journal, [string]$Field, [long]$Window) {
    if ($Case.Mode -cne 'click' -or $Case.Kind -cnotin @('textarea', 'editable')) {
        throw 'Conservative refusal is restricted to the two immediate-click cases'
    }
    if ($Record.actualBefore -cne $Case.Text -or $Record.actualAfter -cne $Case.Text -or
        $Record.otherFieldsUnchanged -isnot [bool] -or -not $Record.otherFieldsUnchanged) {
        throw 'Conservative refusal requires the complete original text and unchanged other fields'
    }
    foreach ($focus in @($Record.firstAltFocus, $Record.secondAltFocus, $Record.finalFocus)) {
        Assert-BrowserTargetFocus $Record.focusAfterInput $focus $Field $Window
    }
    if ($null -eq $Journal) { throw 'Conservative refusal journal metadata is missing' }
    foreach ($name in @('activeChars', 'cachedChars', 'capturedChars', 'pointerBaseline', 'captureUnavailable')) {
        $property = $Journal.PSObject.Properties[$name]
        if ($null -eq $property -or $null -eq $property.Value -or
            ($property.Value -isnot [long] -and $property.Value -isnot [int] -and
             $property.Value -isnot [uint] -and $property.Value -isnot [ulong]) -or $property.Value -lt 0) {
            throw "Conservative refusal has invalid journal metadata: $name"
        }
    }
    if ($Journal.captureUnavailable -ne 1 -or $Journal.activeChars -ne 0 -or $Journal.cachedChars -ne 0 -or
        $Journal.capturedChars -ne $Journal.pointerBaseline) {
        throw 'Conservative refusal requires unavailable capture, empty journals, and no append since pointer-down'
    }
}
function Invoke-BrowserClickRecovery($Session, [string]$Field, [hashtable]$Case, $NativeBefore,
    $Sessions, [hashtable]$ExpectedOther, [System.Collections.IDictionary]$Recovery) {
    # Preserve the existing text and focus. Selecting it exercises the normal
    # selection command rather than retrying the refused journal conversion.
    if (-not (Read-Browser $Session "fixture.select('$Field', true)")) {
        throw 'Refused click target lost focus before selection recovery'
    }
    Tap-BrowserDoubleShift
    Wait-Browser { (Read-Browser $Session 'fixture.values()').$Field -ceq $Case.Expected } 'same-control selection recovery'
    $Recovery.selectionResult = (Read-Browser $Session 'fixture.values()').$Field
    if ($Recovery.selectionResult -cne $Case.Expected) { throw 'Selection recovery did not preserve the exact converted text' }
    Assert-BrowserTargetFocus $NativeBefore (Get-BrowserFocusSnapshot $Session) $Field ($Session.Handle.ToInt64())
    Assert-BrowserOtherFields $Sessions $ExpectedOther
    if (-not (Read-Browser $Session "fixture.collapseEnd('$Field')")) { throw 'Same-control caret collapse failed' }
    Tap-BrowserKey 0x1B
    Set-BrowserEnglish $Session
    # No readiness polling or guard query between the boundary and fresh keys.
    $Recovery.inputTrace = [BrowserInput]::Type($Session.Handle, 'ghbdtn', 0, 0)
    $typed = $Case.Expected + 'ghbdtn'
    Wait-Browser { (Read-Browser $Session 'fixture.values()').$Field -ceq $typed } 'exact same-control fresh input' 2500
    Tap-BrowserKey 0xA4
    Start-Sleep -Milliseconds 100
    Tap-BrowserKey 0xA4
    $expected = $Case.Expected + 'привет'
    Wait-Browser { (Read-Browser $Session 'fixture.values()').$Field -ceq $expected } 'same-control fresh journal recovery' 4000
    Start-Sleep -Milliseconds 75
    $Recovery.actual = (Read-Browser $Session 'fixture.values()').$Field
    if ($Recovery.actual -cne $expected) { throw 'Fresh journal recovery did not remain exact and stable' }
    $Recovery.finalFocus = Get-BrowserFocusSnapshot $Session
    Assert-BrowserTargetFocus $NativeBefore $Recovery.finalFocus $Field ($Session.Handle.ToInt64())
    Assert-BrowserOtherFields $Sessions $ExpectedOther
    $Recovery.otherFieldsUnchanged = $true
}

try {
    $browserExe = @(
        (Join-Path $env:ProgramFiles 'Google/Chrome/Application/chrome.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Google/Chrome/Application/chrome.exe'),
        (Join-Path ${env:ProgramFiles(x86)} 'Microsoft/Edge/Application/msedge.exe'),
        (Join-Path $env:ProgramFiles 'Microsoft/Edge/Application/msedge.exe')
    ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if ($null -eq $browserExe) { throw 'Neither Chrome nor Edge is available on this Windows runner' }
    $browserProfile = Join-Path $runRoot 'browser-profile'
    $fixturePath = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'browser-fixture.html')).Path
    $fixtureUrl = [Uri]::new($fixturePath).AbsoluteUri
    $info = [System.Diagnostics.ProcessStartInfo]::new($browserExe)
    $info.UseShellExecute = $false
    foreach ($argument in @('--no-first-run', '--no-default-browser-check', '--disable-background-networking',
        '--disable-component-update', '--disable-sync', '--disable-extensions', '--force-device-scale-factor=1',
        '--force-renderer-accessibility',
        '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', '--window-size=1000,800',
        "--user-data-dir=$browserProfile", $fixtureUrl)) { $info.ArgumentList.Add($argument) }
    $browser = [System.Diagnostics.Process]::Start($info)
    $ownedProcesses.Add($browser)
    $portFile = Join-Path $browserProfile 'DevToolsActivePort'
    Wait-Browser {
        if ($browser.HasExited) { throw "Browser exited before initialization: $($browser.ExitCode)" }
        Test-Path -LiteralPath $portFile
    } 'owned browser debugging endpoint' 30000
    $port = [int](Get-Content -LiteralPath $portFile -TotalCount 1)
    if ($port -lt 1 -or $port -gt 65535) { throw 'Invalid local browser port' }
    $endpoint = "http://127.0.0.1:$port"
    $version = Invoke-RestMethod -Uri "$endpoint/json/version" -TimeoutSec 5
    $sessions = @{}
    foreach ($id in @('a', 'b')) {
        if ($id -eq 'b') {
            Invoke-Cdp $sessions.a.Connection 'Target.createTarget' @{url = "$fixtureUrl#second"; newWindow = $true} | Out-Null
        }
        $expectedUrl = if ($id -eq 'a') { $fixtureUrl } else { "$fixtureUrl#second" }
        $target = $null
        Wait-Browser {
            # Invoke-RestMethod emits a JSON array as one pipeline object. The
            # parentheses enumerate it before filtering the individual pages.
            $script:browserTargets = @((Invoke-RestMethod -Uri "$endpoint/json/list" -TimeoutSec 5) |
                Where-Object { $_.type -eq 'page' -and $_.url -eq $expectedUrl })
            $script:browserTargets.Count -eq 1
        } "owned browser page $id" 15000
        $target = $script:browserTargets[0]
        if ($target -is [array] -or $target.webSocketDebuggerUrl -isnot [string]) {
            throw "Browser page $id must resolve to one debugging endpoint"
        }
        $socketUri = [Uri]$target.webSocketDebuggerUrl
        if ($socketUri.Scheme -ne 'ws' -or -not $socketUri.IsLoopback -or $socketUri.Port -ne $port -or
            $socketUri.AbsolutePath -ne "/devtools/page/$($target.id)") {
            throw "Browser page $id returned an unexpected debugging endpoint"
        }
        [ordered]@{browserSession = $id; targetId = $target.id; url = $target.url;
            webSocket = $socketUri.AbsoluteUri} | ConvertTo-Json -Compress | Write-Output
        $connection = [BrowserSocket]::new($target.webSocketDebuggerUrl)
        $connections.Add($connection)
        Invoke-Cdp $connection 'Page.bringToFront' | Out-Null
        $session = @{Connection = $connection; Handle = [IntPtr]::Zero}
        Wait-Browser { Read-Browser $session "typeof fixture === 'object'" } 'browser fixture script loaded'
        $expectedTitle = 'Rust Switcher browser regression ' + $id.ToUpperInvariant()
        Wait-Browser {
            $foreground = [BrowserInput]::GetForegroundWindow()
            $foreground -ne [IntPtr]::Zero -and
                [BrowserInput]::ProcessOf($foreground) -eq $browser.Id -and
                [BrowserInput]::TitleOf($foreground).StartsWith($expectedTitle)
        } 'browser native window activation'
        $session.Handle = [BrowserInput]::GetForegroundWindow()
        if ($id -eq 'b' -and $session.Handle -eq $sessions.a.Handle) { throw 'Browser must create two distinct top-level windows' }
        $sessions[$id] = $session
    }
    $english = [BrowserInput]::LoadKeyboardLayout('00000409', 0)
    $russian = [BrowserInput]::LoadKeyboardLayout('00000419', 0)
    if ($english -eq [IntPtr]::Zero -or $russian -eq [IntPtr]::Zero) { throw 'EN/RU test layouts unavailable' }
    foreach ($session in $sessions.Values) { Set-BrowserEnglish $session }

    # Prepare the hotkey configuration before startup. The desktop suite tests
    # the settings controls; this suite needs deterministic browser input setup.
    $switcherProfile = Join-Path $runRoot 'switcher-profile'
    $configDirectory = Join-Path $switcherProfile 'RustSwitcherDebug'
    New-Item -ItemType Directory -Path $configDirectory | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'browser-config.toml') `
        -Destination (Join-Path $configDirectory 'config.json')
    $switcherInfo = [System.Diagnostics.ProcessStartInfo]::new($DebugExe)
    $switcherInfo.UseShellExecute = $false
    $switcherInfo.CreateNoWindow = $true
    $switcherInfo.RedirectStandardOutput = $true
    $switcherInfo.RedirectStandardError = $true
    $switcherInfo.Environment['APPDATA'] = $switcherProfile
    $switcherInfo.Environment['RUST_LOG'] = 'trace'
    $switcherInfo.Environment['RUST_SWITCHER_E2E_PLAYGROUND'] = '1'
    $switcherInfo.Environment['RUST_SWITCHER_E2E_ALLOW_INJECTED'] = '1'
    $switcherInfo.Environment['RUST_SWITCHER_E2E_TARGET_PID'] = $browser.Id.ToString()
    $switcher = [System.Diagnostics.Process]::Start($switcherInfo)
    $ownedProcesses.Add($switcher)
    $switcherOutput = $switcher.StandardOutput.ReadToEndAsync()
    $switcherErrors = $switcher.StandardError.ReadToEndAsync()
    Wait-Browser {
        if ($switcher.HasExited) { throw "Switcher exited before initialization: $($switcher.ExitCode)" }
        $switcher.Refresh()
        $switcher.MainWindowHandle -ne [IntPtr]::Zero
    } 'debug Switcher window'
    [ordered]@{browser = $version.Browser; executable = $browserExe; browserPid = $browser.Id;
        windowA = $sessions.a.Handle.ToInt64(); windowB = $sessions.b.Handle.ToInt64();
        commit = $env:GITHUB_SHA; run = $env:GITHUB_RUN_ID} | ConvertTo-Json -Compress | Write-Output

    $cases = [System.Collections.Generic.List[object]]::new()
    if (-not $SecurityOnly) {
        foreach ($kind in @('input', 'textarea', 'editable', 'shadow', 'iframe')) {
            foreach ($sample in @(@{Text = 'ghbdtn'; Expected = 'привет'}, @{Text = '.ghbdtn'; Expected = 'юпривет'})) {
                foreach ($gap in @(0, 30)) {
                    $cases.Add(@{Kind = $kind; Mode = 'fresh'; Text = $sample.Text; Expected = $sample.Expected; Gap = $gap; Repeat = 1})
                }
            }
            $cases.Add(@{Kind = $kind; Mode = 'fresh'; Text = '.'; Expected = 'ю'; Gap = 0; Repeat = 1})
        }
        foreach ($mode in @('tab', 'click', 'click-layout', 'window', 'ctrl-release', 'alt-release')) {
            foreach ($kind in @('textarea', 'editable')) {
                $cases.Add(@{Kind = $kind; Mode = $mode; Text = '.ghbdtn'; Expected = 'юпривет'; Gap = 0; Repeat = 1})
            }
        }
        foreach ($kind in @('textarea', 'editable')) {
            $cases.Add(@{Kind = $kind; Mode = 'fresh'; Text = 'a' * 101; Expected = 'ф' * 101; Gap = 0; Repeat = 1})
            $cases.Add(@{Kind = $kind; Mode = 'fresh'; Text = '.' + ('a' * 100); Expected = 'ю' + ('ф' * 100); Gap = 0; Repeat = 1})
        }
        foreach ($repeat in 1..6) {
            $cases.Add(@{Kind = 'editable'; Mode = 'fresh'; Text = 'ghbdtn'; Expected = 'привет'; Gap = 1; Repeat = $repeat})
            $cases.Add(@{Kind = 'textarea'; Mode = 'fresh'; Text = '.ghbdtn'; Expected = 'юпривет'; Gap = 1; Repeat = $repeat})
        }
        if ($cases.Count -ne 53) { throw "Expected 53 ordinary browser cases, found $($cases.Count)" }
    }

    foreach ($case in $cases) {
        $name = "browser $($case.Kind) $($case.Mode) '$($case.Text)' firstGap=$($case.Gap) repeat=$($case.Repeat)"
        $record = [ordered]@{name = $name; input = $case.Text; expected = $case.Expected; actualBefore = $null;
            actualAfter = $null; inputTrace = $null; focusAfterInput = $null; firstAltFocus = $null;
            secondAltFocus = $null; finalFocus = $null; otherFieldsUnchanged = $null;
            outcome = $null; refusal = $null; recovery = $null; error = $null}
        $heldModifier = [ushort]0
        $destination = $sessions.a
        $targetField = 'a'
        $expectedOther = $null
        try {
            if ($switcher.HasExited -or $browser.HasExited) { throw 'An owned test process exited' }
            foreach ($session in @($sessions.a, $sessions.b)) {
                Read-Browser $session "fixture.reset('$($case.Kind)')" | Out-Null
                Set-BrowserEnglish $session
                Focus-BrowserEditor $session 'a'
                # Reset each native window's journal before any tested input. No
                # conversion is retried and no tested glyph is injected via CDP.
                Tap-BrowserKey 0x1B
            }
            Focus-BrowserEditor $sessions.a 'a'
            $expectedOther = @{'a/a' = ''; 'a/b' = 'b-sentinel'; 'a/guard' = 'guard-sentinel';
                'b/a' = ''; 'b/b' = 'b-sentinel'; 'b/guard' = 'guard-sentinel'}
            $destinationId = 'a'
            if ($case.Mode -in @('tab', 'click', 'click-layout', 'window')) {
                [BrowserInput]::Type($sessions.a.Handle, 'keep', 0, 0) | Out-Null
                Wait-Browser { (Read-Browser $sessions.a 'fixture.values()').a -eq 'keep' } 'source field text'
                $expectedOther['a/a'] = 'keep'
                if ($case.Mode -eq 'window') {
                    $destination = $sessions.b
                    $destinationId = 'b'
                    Focus-BrowserEditor $destination 'a'
                } else {
                    $targetField = 'b'
                    Read-Browser $destination "fixture.set('b', '')" | Out-Null
                    if ($case.Mode -eq 'tab') { Tap-BrowserKey 0x09 }
                    else {
                        $point = Read-Browser $destination "fixture.point('b')"
                        [BrowserInput]::Click($destination.Handle, $point.x, $point.y)
                    }
                    Wait-Browser { Read-Browser $destination "fixture.focused('b')" } 'second DOM field focus'
                    if ($case.Mode -eq 'click-layout') {
                        # A protected layout command can be the first action
                        # after a click. It must not poison the next text key.
                        Tap-BrowserKey 0x14
                        Wait-Browser { [BrowserInput]::LayoutOf($destination.Handle) -ne $english } 'CapsLock after click'
                        Tap-BrowserKey 0x14
                        Wait-Browser { [BrowserInput]::LayoutOf($destination.Handle) -eq $english } 'CapsLock restores English before typing'
                    }
                }
            } elseif ($case.Mode -in @('ctrl-release', 'alt-release')) {
                # The shortcut is consumed by the source field; no text is altered.
                $heldModifier = [ushort]$(if ($case.Mode -eq 'ctrl-release') {0xA2} else {0xA4})
                [BrowserInput]::Key($heldModifier, $false)
                Tap-BrowserKey 0x87 # F24 prevents a held Alt from counting as a standalone tap.
                $destination = $sessions.b
                $destinationId = 'b'
                Focus-BrowserEditor $destination 'a'
            }
            if ([BrowserInput]::LayoutOf($destination.Handle) -ne $english) { throw 'Destination layout changed before input' }
            $expectedOther.Remove("$destinationId/$targetField")
            $record.inputTrace = [BrowserInput]::Type($destination.Handle, $case.Text, $case.Gap, $heldModifier)
            $heldModifier = 0
            try {
                Wait-Browser { (Read-Browser $destination 'fixture.values()').$targetField -eq $case.Text } 'exact browser input' 2500
            } finally {
                $record.actualBefore = (Read-Browser $destination 'fixture.values()').$targetField
                $record.focusAfterInput = [BrowserInput]::Snapshot($destination.Handle)
            }
            Tap-BrowserKey 0xA4
            Start-Sleep -Milliseconds 100
            $record.firstAltFocus = @{native = [BrowserInput]::Snapshot($destination.Handle);
                dom = Read-Browser $destination 'fixture.diagnostics()'}
            Tap-BrowserKey 0xA4
            $record.secondAltFocus = @{native = [BrowserInput]::Snapshot($destination.Handle);
                dom = Read-Browser $destination 'fixture.diagnostics()'}
            $refused = $false
            if ($case.Mode -eq 'click') {
                if (-not (Test-BrowserValueWithin $destination $targetField $case.Expected 4000)) {
                    $record.actualAfter = (Read-Browser $destination 'fixture.values()').$targetField
                    $record.finalFocus = Get-BrowserFocusSnapshot $destination
                    $record.otherFieldsUnchanged = $false
                    Assert-BrowserOtherFields $sessions $expectedOther
                    $record.otherFieldsUnchanged = $true
                    $journal = Read-BrowserJournalState $switcher.MainWindowHandle
                    Assert-BrowserSafeFocusRefusal $case $record $journal $targetField ($destination.Handle.ToInt64())
                    $record.refusal = @{actualBefore = $record.actualBefore; actualAfter = $record.actualAfter;
                        journal = $journal; focus = $record.finalFocus; otherFieldsUnchanged = $true}
                    $record.outcome = 'safe_focus_refusal'
                    $record.recovery = [ordered]@{}
                    Invoke-BrowserClickRecovery $destination $targetField $case $record.focusAfterInput `
                        $sessions $expectedOther $record.recovery
                    $record.recovery.journalAfter = Read-BrowserJournalState $switcher.MainWindowHandle
                    $refused = $true
                }
            } else {
                Wait-Browser { (Read-Browser $destination 'fixture.values()').$targetField -eq $case.Expected } 'exact Double Alt browser result' 4000
            }
            if (-not $refused) {
                Start-Sleep -Milliseconds 75
                $record.actualAfter = (Read-Browser $destination 'fixture.values()').$targetField
                if ($record.actualAfter -ne $case.Expected) { throw 'Browser conversion did not remain stable' }
                if (-not (Read-Browser $destination "fixture.focused('$targetField')")) {
                    throw 'Double Alt moved focus away from the target editor'
                }
                $record.outcome = 'converted'
            }
        } catch {
            $record.error = $_.Exception.Message
            try { $record.actualAfter = (Read-Browser $destination 'fixture.values()').$targetField } catch { }
        } finally {
            if ($heldModifier -ne 0) { [BrowserInput]::Key($heldModifier, $true) }
            try {
                $record.finalFocus = @{native = [BrowserInput]::Snapshot($destination.Handle);
                    dom = Read-Browser $destination 'fixture.diagnostics()'}
            } catch { }
            if ($null -ne $expectedOther) {
                try {
                    $record.otherFieldsUnchanged = $false
                    Assert-BrowserOtherFields $sessions $expectedOther
                    $record.otherFieldsUnchanged = $true
                } catch {
                    $record.error = "$($record.error) $($_.Exception.Message)".Trim()
                }
            }
        }
        if ($null -ne $record.error) { $failures.Add("${name}: $($record.error)") }
        else { $ordinaryOutcomes[$record.outcome]++ }
        $results.Add([pscustomobject]$record)
        $record | ConvertTo-Json -Depth 6 -Compress | Write-Output
        if ($null -eq $record.error) { Write-Output "PASS: $name" } else { Write-Output "FAIL: $name" }
    }
    $securityStart = $results.Count
    & (Join-Path $PSScriptRoot 'browser-security-cases.ps1') -Sessions $sessions `
        -SwitcherProcess $switcher -Results $results -Failures $failures
    if ($results.Count - $securityStart -ne 6) {
        throw "Expected 5 mandatory password cases and 1 provider-capability result, found $($results.Count - $securityStart)"
    }
    # End the debug phase cleanly before inspecting its actual diagnostics/profile.
    if (-not [BrowserInput]::PostMessage($switcher.MainWindowHandle, 0x111, [UIntPtr]1103, [IntPtr]::Zero) -or
        -not $switcher.WaitForExit(8000) -or $switcher.ExitCode -ne 0) {
        throw 'Debug Switcher did not exit after password checks'
    }
    $switcherOutput.Wait()
    $switcherErrors.Wait()
    Assert-DiagnosticRedaction ($switcherOutput.Result + "`n" + $switcherErrors.Result)
    foreach ($file in Get-ChildItem -LiteralPath $switcherProfile -File -Recurse) {
        $contents = [System.IO.File]::ReadAllText($file.FullName)
        if ($contents.Contains('ghbdtnvbh') -or $contents.Contains('приветмир')) {
            throw 'INPUT_PERSISTENCE_VIOLATION: profile contains the synthetic password'
        }
    }
    Write-Output 'PASS: actual debug diagnostics and saved profile exclude synthetic password text'
    if ($ReleaseExe -and -not $SecurityOnly) {
        $releaseProfile = Join-Path $runRoot 'release-double-shift-profile'
        New-Item -ItemType Directory -Path $releaseProfile | Out-Null
        # Let the application create its complete default configuration,
        # including the shared Double Shift selection and last-word binding.
        $releaseInfo = [System.Diagnostics.ProcessStartInfo]::new($ReleaseExe)
        $releaseInfo.UseShellExecute = $false
        $releaseInfo.CreateNoWindow = $true
        $releaseInfo.Environment['APPDATA'] = $releaseProfile
        foreach ($variable in @('RUST_SWITCHER_E2E_PLAYGROUND', 'RUST_SWITCHER_E2E_ALLOW_INJECTED', 'RUST_SWITCHER_E2E_TARGET_PID')) {
            $releaseInfo.Environment.Remove($variable) | Out-Null
        }
        $release = [System.Diagnostics.Process]::Start($releaseInfo)
        $ownedProcesses.Add($release)
        Wait-Browser {
            if ($release.HasExited) { throw "Release Switcher exited during startup: $($release.ExitCode)" }
            $release.Refresh()
            $release.MainWindowHandle -ne [IntPtr]::Zero
        } 'release Switcher window for Double Shift'
        $releaseWindow = $release.MainWindowHandle
        if ([BrowserInput]::ProcessOf($releaseWindow) -ne $release.Id) {
            throw 'Release Switcher window is not owned by the launched process'
        }
        # Queue the normal close-to-tray command after startup shows Settings.
        # Observe its completion before transferring focus to the browser.
        if (-not [BrowserInput]::PostMessage($releaseWindow, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero)) {
            throw 'Release Switcher Settings close-to-tray command failed'
        }
        Wait-Browser {
            if ($release.HasExited) { throw "Release Switcher exited while hiding Settings: $($release.ExitCode)" }
            -not [BrowserInput]::IsWindowVisible($releaseWindow)
        } 'release Switcher Settings hidden to tray'
        # Read only the selected range; never repair it or focus after Double Shift.
        $selectionProbe = @'
(() => {
  const element = document.getElementById('a');
  if (!element.isContentEditable) return {
    complete: element.selectionStart === 0 && element.selectionEnd === element.value.length,
    text: element.value.slice(element.selectionStart, element.selectionEnd)
  };
  const selection = document.getSelection();
  return {
    complete: selection.rangeCount === 1 && element.contains(selection.anchorNode) &&
      element.contains(selection.focusNode) && selection.toString() === element.textContent,
    text: selection.toString()
  };
})()
'@
        $stateProbe = "({value: fixture.values().a, selection: $selectionProbe, dom: fixture.diagnostics()})"
        foreach ($kind in @('input', 'editable')) {
            $record = [ordered]@{name = "release-double-shift-$kind"; kind = $kind; mode = 'release-double-shift';
                attempted = @(); conversions = @(); focusBefore = $null; actualBefore = $null;
                selectionBefore = $null; actualAfter = $null; selectionAfter = $null; finalFocus = $null;
                otherFieldsUnchanged = $null; clipboardBefore = $null; clipboardAfter = $null;
                clipboardUnchanged = $null; diagnosticErrors = @(); error = $null}
            $session = $sessions.a
            $expectedOtherReady = $false
            try {
                $expectedOther = @{}
                foreach ($sessionId in @('a', 'b')) {
                    Read-Browser $sessions[$sessionId] "fixture.reset('$kind')" | Out-Null
                    Set-BrowserEnglish $sessions[$sessionId]
                    $values = Read-Browser $sessions[$sessionId] 'fixture.values()'
                    foreach ($field in @('a', 'b', 'guard')) {
                        if ($sessionId -ne 'a' -or $field -ne 'a') { $expectedOther["$sessionId/$field"] = $values.$field }
                    }
                }
                $expectedOtherReady = $true
                Read-Browser $session "fixture.set('a', 'ghbdtn vbh')" | Out-Null
                Focus-BrowserEditor $session 'a'
                if (-not (Read-Browser $session "fixture.select('a', true)")) { throw 'Initial Double Shift selection lost focus' }
                $nativeBefore = [BrowserInput]::Snapshot($session.Handle)
                $record.focusBefore = @{native = $nativeBefore; dom = $null}
                try {
                    $initial = Read-Browser $session $stateProbe
                    $record.focusBefore.dom = $initial.dom
                    $record.actualBefore = $initial.value
                    $record.selectionBefore = $initial.selection
                } catch { $record.diagnosticErrors += "initial browser state: $($_.Exception.Message)" }
                if ([BrowserInput]::ClassOf([IntPtr]$nativeBefore.focus) -match '^(Edit|RichEdit.*|RICHEDIT.*)$') {
                    throw 'Release browser Double Shift must exercise UIA rather than the native Edit fallback'
                }
                $clipboardBefore = [BrowserInput]::GetClipboardSequenceNumber()
                $record.clipboardBefore = $clipboardBefore
                if ($clipboardBefore -eq 0) { throw 'Clipboard sequence is unavailable before browser Double Shift' }
                foreach ($expected in @('привет мир', 'ghbdtn vbh')) {
                    $record.attempted += $expected
                    Tap-BrowserDoubleShift
                    Wait-Browser { (Read-Browser $session 'fixture.values()').a -ceq $expected } 'one Double Shift converts the complete browser phrase'
                    Wait-Browser {
                        $selection = Read-Browser $session $selectionProbe
                        $selection.complete -eq $true -and $selection.text -ceq $expected
                    } 'release Double Shift restores the full browser selection'
                    Start-Sleep -Milliseconds 150
                    $selection = Read-Browser $session $selectionProbe
                    if ((Read-Browser $session 'fixture.values()').a -cne $expected -or
                        $selection.complete -ne $true -or $selection.text -cne $expected) {
                        throw 'One Double Shift did not produce one stable browser phrase conversion and reselection'
                    }
                    $record.finalFocus = Get-BrowserFocusSnapshot $session
                    Assert-BrowserTargetFocus $nativeBefore $record.finalFocus 'a' ($session.Handle.ToInt64())
                    Assert-BrowserOtherFields $sessions $expectedOther
                    if ([BrowserInput]::GetClipboardSequenceNumber() -ne $clipboardBefore) { throw 'Browser Double Shift changed the clipboard' }
                    $record.conversions += $expected
                }
            } catch {
                $record.error = $_.Exception.Message
                $failures.Add("$($record.name): $($record.error)")
            } finally {
                # Keep the original failure and observe the unrepaired target,
                # including a timeout before the first successful conversion.
                $record.finalFocus = @{native = $null; dom = $null}
                try { $record.finalFocus.native = [BrowserInput]::Snapshot($session.Handle) }
                catch { $record.diagnosticErrors += "final native focus: $($_.Exception.Message)" }
                try {
                    $final = Read-Browser $session $stateProbe
                    $record.actualAfter = $final.value
                    $record.selectionAfter = $final.selection
                    $record.finalFocus.dom = $final.dom
                } catch { $record.diagnosticErrors += "final browser state: $($_.Exception.Message)" }
                if ($expectedOtherReady) {
                    $record.otherFieldsUnchanged = $false
                    try {
                        Assert-BrowserOtherFields $sessions $expectedOther
                        $record.otherFieldsUnchanged = $true
                    } catch { $record.diagnosticErrors += "other fields: $($_.Exception.Message)" }
                }
                try {
                    $record.clipboardAfter = [BrowserInput]::GetClipboardSequenceNumber()
                    if ($null -ne $record.clipboardBefore) {
                        $record.clipboardUnchanged = $record.clipboardAfter -eq $record.clipboardBefore
                    }
                } catch { $record.diagnosticErrors += "final clipboard sequence: $($_.Exception.Message)" }
            }
            $results.Add([pscustomobject]$record)
            $record | ConvertTo-Json -Depth 6 -Compress | Write-Output
            if ($null -eq $record.error) { Write-Output "PASS: $($record.name)" } else { Write-Output "FAIL: $($record.name)" }
        }
        if (-not [BrowserInput]::PostMessage($releaseWindow, 0x111, [UIntPtr]1103, [IntPtr]::Zero) -or
            -not $release.WaitForExit(8000) -or $release.ExitCode -ne 0) {
            throw 'Release Switcher did not exit after browser Double Shift checks'
        }
    }
    if ($failures.Count -gt 0) { throw ($failures -join '; ') }
} catch {
    $failure = $_.Exception.Message
    Write-Output "FAIL: $failure"
    Write-Output $_.ScriptStackTrace
    throw
} finally {
    $cleanupErrors = [System.Collections.Generic.List[string]]::new()
    foreach ($connection in $connections) {
        try { $connection.Dispose() } catch { $cleanupErrors.Add($_.Exception.Message) }
    }
    foreach ($process in $ownedProcesses) {
        try {
            if (-not $process.HasExited) {
                $process.Kill($true)
                if (-not $process.WaitForExit(5000)) { $cleanupErrors.Add("Cleanup timeout for owned process $($process.Id)") }
            }
        } catch { $cleanupErrors.Add($_.Exception.Message) }
    }
    if ($null -ne $failure) {
        foreach ($outputTask in @($switcherOutput, $switcherErrors)) {
            if ($null -ne $outputTask -and $outputTask.Wait(1000)) { Write-Output $outputTask.Result }
        }
    }
    $report = [ordered]@{passed = @($results | Where-Object {
            $null -eq $_.error -and -not ($_.PSObject.Properties['uncovered'] -and $_.uncovered)
        }).Count;
        uncovered = @($results | Where-Object { $_.PSObject.Properties['uncovered'] -and $_.uncovered }).Count;
        total = $results.Count; ordinaryOutcomes = $ordinaryOutcomes; cases = $results.ToArray(); failures = $failures.ToArray(); failure = $failure;
        cleanupErrors = $cleanupErrors.ToArray(); commit = $env:GITHUB_SHA; run = $env:GITHUB_RUN_ID}
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $runRoot 'result.json')
    $report | ConvertTo-Json -Depth 8 | Write-Output
    if ($cleanupErrors.Count -gt 0 -and $null -eq $failure) { throw ($cleanupErrors -join '; ') }
}
