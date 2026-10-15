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
$ownedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$failure = $null

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
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
    [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint count, INPUT[] inputs, int size);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
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
    [DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
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
function Start-Switcher([string]$Exe, [string]$ProfilePath, [bool]$DebugMode) {
    $info = [System.Diagnostics.ProcessStartInfo]::new($Exe)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WorkingDirectory = $runRoot
    $info.Environment['APPDATA'] = $ProfilePath
    $info.Environment['RUST_LOG'] = 'warn'
    if ($DebugMode) {
        $info.Environment['RUST_SWITCHER_E2E_PLAYGROUND'] = '1'
        $info.Environment['RUST_SWITCHER_E2E_ALLOW_INJECTED'] = '1'
    } else {
        $info.Environment.Remove('RUST_SWITCHER_E2E_PLAYGROUND') | Out-Null
        $info.Environment.Remove('RUST_SWITCHER_E2E_ALLOW_INJECTED') | Out-Null
    }
    $process = [System.Diagnostics.Process]::Start($info)
    $ownedProcesses.Add($process)
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
function Select-All {
    [HostedInput]::Key(0xA2, $false)
    try { Tap-Key 0x41 } finally { [HostedInput]::Key(0xA2, $true) }
    Start-Sleep -Milliseconds 100
}
function Assert-Value($Pattern, [string]$Expected, [string]$Name) {
    Wait-Condition { $Pattern.Current.Value -eq $Expected } "$Name; expected '$Expected'"
    Record-Pass $Name
}
function Quit-Owned($Process, $Window) {
    (Get-Pattern (Find-Control $Window 'Exit') ([System.Windows.Automation.InvokePattern]::Pattern)).Invoke()
    if (-not $Process.WaitForExit(8000)) { throw 'Application did not exit within cleanup deadline' }
    if ($Process.ExitCode -ne 0) { throw "Application exit code: $($Process.ExitCode)" }
}

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
    Apply-Settings $window
    $playground = Find-Playground $window
    $playground.SetFocus()
    $value = Get-Pattern $playground ([System.Windows.Automation.ValuePattern]::Pattern)
    $english = [HostedInput]::LoadKeyboardLayout('00000409', 0)
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

    $releaseProfile = Join-Path $runRoot 'release-profile'
    $release = Start-Switcher $ReleaseExe $releaseProfile $false
    Wait-Condition { $null -ne (Find-AppWindow $release) } 'release main window'
    $window = Find-AppWindow $release
    Focus-App $window
    Record-Pass 'release application startup'
    $second = Start-Switcher $ReleaseExe $releaseProfile $false
    if (-not $second.WaitForExit(8000) -or $second.ExitCode -ne 0) { throw 'Second instance did not exit successfully' }
    if ($release.HasExited) { throw 'Primary instance exited during second launch' }
    Record-Pass 'release single-instance handoff'
    $editorInfo = [System.Diagnostics.ProcessStartInfo]::new('powershell.exe')
    $editorInfo.UseShellExecute = $false
    $editorInfo.CreateNoWindow = $true
    foreach ($argument in @('-NoProfile', '-STA', '-File', (Join-Path $PSScriptRoot 'editor-fixture.ps1'))) {
        $editorInfo.ArgumentList.Add($argument)
    }
    $editor = [System.Diagnostics.Process]::Start($editorInfo)
    $ownedProcesses.Add($editor)
    Wait-Condition { $null -ne (Find-AppWindow $editor) } 'isolated external editor'
    $editorWindow = Find-AppWindow $editor
    Focus-App $editorWindow
    $editCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ClassNameProperty, 'Edit')
    $edit = $editorWindow.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $editCondition)
    if ($null -eq $edit) { throw 'Native Edit fixture not found' }
    $edit.SetFocus()
    $editValue = Get-Pattern $edit ([System.Windows.Automation.ValuePattern]::Pattern)
    $editValue.SetValue('ghbdtn')
    Select-All
    $clipboardBefore = [HostedInput]::GetClipboardSequenceNumber()
    Tap-Shift 2
    Assert-Value $editValue 'привет' 'release selected text converted in external editor'
    Tap-Shift 2
    Assert-Value $editValue 'ghbdtn' 'release selected text reverse conversion'
    if ([HostedInput]::GetClipboardSequenceNumber() -ne $clipboardBefore) { throw 'Selection conversion changed the clipboard' }
    Record-Pass 'release selection leaves clipboard unchanged'
    Focus-App $window
    # Closing our fixture through its real window message loop leaves no input target behind.
    [HostedInput]::PostMessage([IntPtr]$editorWindow.Current.NativeWindowHandle, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    if (-not $editor.WaitForExit(5000)) { throw 'Isolated editor did not close' }
    Toggle-Control $window 'Dark theme' $true
    Apply-Settings $window
    $config = Join-Path $releaseProfile 'RustSwitcher/config.json'
    Wait-Condition { (Test-Path -LiteralPath $config) -and (Get-Content -LiteralPath $config -Raw) -match 'theme_dark\s*=\s*true' } 'release setting persisted'
    Record-Pass 'release settings saved'
    $runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    if ($null -ne (Get-ItemPropertyValue -LiteralPath $runKey -Name RustSwitcher -ErrorAction SilentlyContinue)) {
        throw 'Unexpected pre-existing autostart entry on fresh VM'
    }
    Toggle-Control $window 'Start on startup' $true
    Wait-Condition { (Get-ItemPropertyValue -LiteralPath $runKey -Name RustSwitcher -ErrorAction SilentlyContinue) -eq ('"' + $ReleaseExe + '" --autostart') } 'autostart registry target'
    Toggle-Control $window 'Start on startup' $false
    Wait-Condition { $null -eq (Get-ItemPropertyValue -LiteralPath $runKey -Name RustSwitcher -ErrorAction SilentlyContinue) } 'autostart removal'
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
} catch {
    $failure = $_.Exception.Message
    Write-Output "FAIL: $failure"
    Write-Output $_.ScriptStackTrace
    throw
} finally {
    foreach ($process in $ownedProcesses) {
        if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit(5000) | Out-Null }
    }
    $result = [ordered]@{ passed = $checks.ToArray(); failure = $failure; commit = $env:GITHUB_SHA; run = $env:GITHUB_RUN_ID }
    $result | ConvertTo-Json -Depth 5 | Write-Output
    $result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $runRoot 'result.json')
    # Only repository-owned fixture data was created; VM teardown disposes it.
}
