param(
    [Parameter(Mandatory = $true)][string]$DebugExe,
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
$runRoot = Join-Path $env:RUNNER_TEMP ('switcher-browser-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runRoot | Out-Null
$results = [System.Collections.Generic.List[object]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
$ownedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$connections = [System.Collections.Generic.List[System.IDisposable]]::new()
$switcherOutput = $null
$switcherErrors = $null
$failure = $null
$script:requestId = 0

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
    public BrowserSocket(string address) {
        using (var timeout = new CancellationTokenSource(5000))
            socket.ConnectAsync(new Uri(address), timeout.Token).GetAwaiter().GetResult();
    }
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
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
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
    $script:requestId++
    $id = $script:requestId
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
    @'
delay_ms = 100
smarter_hotkeys_enabled = true
'@ | Set-Content -LiteralPath (Join-Path $configDirectory 'config.json') -Encoding utf8NoBOM
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
        foreach ($mode in @('tab', 'click', 'window', 'ctrl-release', 'alt-release')) {
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
        if ($cases.Count -ne 51) { throw "Expected 51 ordinary browser cases, found $($cases.Count)" }
    }

    foreach ($case in $cases) {
        $name = "browser $($case.Kind) $($case.Mode) '$($case.Text)' firstGap=$($case.Gap) repeat=$($case.Repeat)"
        $record = [ordered]@{name = $name; input = $case.Text; expected = $case.Expected; actualBefore = $null;
            actualAfter = $null; inputTrace = $null; focusAfterInput = $null; firstAltFocus = $null;
            secondAltFocus = $null; finalFocus = $null; otherFieldsUnchanged = $null; error = $null}
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
            if ($case.Mode -in @('tab', 'click', 'window')) {
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
            Wait-Browser { (Read-Browser $destination 'fixture.values()').$targetField -eq $case.Expected } 'exact Double Alt browser result' 4000
            Start-Sleep -Milliseconds 75
            $record.actualAfter = (Read-Browser $destination 'fixture.values()').$targetField
            if ($record.actualAfter -ne $case.Expected) { throw 'Browser conversion did not remain stable' }
            if (-not (Read-Browser $destination "fixture.focused('$targetField')")) {
                throw 'Double Alt moved focus away from the target editor'
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
                    $record.otherFieldsUnchanged = $true
                    foreach ($sessionId in @('a', 'b')) {
                        $values = Read-Browser $sessions[$sessionId] 'fixture.values()'
                        foreach ($field in @('a', 'b', 'guard')) {
                            $key = "$sessionId/$field"
                            if ($expectedOther.ContainsKey($key) -and $values.$field -ne $expectedOther[$key]) {
                                $record.otherFieldsUnchanged = $false
                                throw "Input changed another field ${key}: '$($values.$field)'"
                            }
                        }
                    }
                } catch {
                    $record.error = "$($record.error) $($_.Exception.Message)".Trim()
                }
            }
        }
        if ($null -ne $record.error) { $failures.Add("${name}: $($record.error)") }
        $results.Add([pscustomobject]$record)
        $record | ConvertTo-Json -Depth 6 -Compress | Write-Output
        if ($null -eq $record.error) { Write-Output "PASS: $name" } else { Write-Output "FAIL: $name" }
    }
    $securityStart = $results.Count
    & (Join-Path $PSScriptRoot 'browser-security-cases.ps1') -Sessions $sessions `
        -SwitcherProcess $switcher -Results $results -Failures $failures
    if ($results.Count - $securityStart -ne 6) {
        throw "Expected 6 browser password cases, found $($results.Count - $securityStart)"
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
    $report = [ordered]@{passed = @($results | Where-Object { $null -eq $_.error }).Count;
        total = $results.Count; cases = $results.ToArray(); failures = $failures.ToArray(); failure = $failure;
        cleanupErrors = $cleanupErrors.ToArray(); commit = $env:GITHUB_SHA; run = $env:GITHUB_RUN_ID}
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $runRoot 'result.json')
    $report | ConvertTo-Json -Depth 8 | Write-Output
    if ($cleanupErrors.Count -gt 0 -and $null -eq $failure) { throw ($cleanupErrors -join '; ') }
}
