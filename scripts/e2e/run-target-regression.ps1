param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [ValidateSet('SourceRead', 'Delivery')][string]$Race = 'SourceRead'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1' -or
    -not [Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
    throw 'Target regression requires the isolated interactive GitHub-hosted Windows job.'
}
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$runRoot = Join-Path $env:RUNNER_TEMP ('switcher-target-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runRoot | Out-Null
$ownedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$signalPath = Join-Path $runRoot 'signal.txt'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

# Reuse the production native harness's input and focus helpers. Extract only
# definitions; evaluating the whole hosted runner would start unrelated tests.
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'run-hosted.ps1'), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors) { throw ($parseErrors | Out-String) }
$definitions = @($ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.CommandAst] -and
        $node.GetCommandName() -eq 'Add-Type' -and $node.Extent.Text.Contains('public static class HostedInput')
}, $true))
if ($definitions.Count -ne 1) { throw 'Expected one native input definition' }
. ([scriptblock]::Create($definitions[0].Extent.Text))
foreach ($name in @('Wait-Condition', 'Find-AppWindow', 'Focus-App', 'Set-OwnedControlFocus', 'Tap-Key', 'Tap-Shift')) {
    $definitions = @($ast.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $false))
    if ($definitions.Count -ne 1) { throw "Expected one native helper: $name" }
    . ([scriptblock]::Create($definitions[0].Extent.Text))
}

try {
    $editorInfo = [System.Diagnostics.ProcessStartInfo]::new('powershell.exe')
    $editorInfo.UseShellExecute = $false
    $editorInfo.CreateNoWindow = $true
    $editorInfo.Environment['RUST_SWITCHER_E2E_SIGNAL_PATH'] = $signalPath
    foreach ($argument in @('-NoProfile', '-STA', '-File', (Join-Path $PSScriptRoot 'editor-fixture.ps1'))) {
        $editorInfo.ArgumentList.Add($argument)
    }
    $editor = [System.Diagnostics.Process]::Start($editorInfo)
    $ownedProcesses.Add($editor)
    Wait-Condition {
        if ($editor.HasExited) { throw 'Native target fixture exited before startup' }
        $null -ne (Find-AppWindow $editor)
    } 'native target fixture startup' 30000
    $editorWindow = Find-AppWindow $editor
    $editorHandle = [IntPtr]$editorWindow.Current.NativeWindowHandle
    $source = [HostedInput]::GetDlgItem($editorHandle, 1001)
    $destination = [HostedInput]::GetDlgItem($editorHandle, 1002)
    if ($source -eq [IntPtr]::Zero -or $destination -eq [IntPtr]::Zero -or
        [HostedInput]::WindowProcess($source) -ne $editor.Id -or
        [HostedInput]::WindowProcess($destination) -ne $editor.Id) {
        throw 'Native target fixture handles are not owned by the launched process'
    }
    $sourceElement = [System.Windows.Automation.AutomationElement]::FromHandle($source)
    $info = [System.Diagnostics.ProcessStartInfo]::new($Exe)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.Environment['APPDATA'] = Join-Path $runRoot 'profile'
    $info.Environment['RUST_SWITCHER_E2E_PLAYGROUND'] = '1'
    $info.Environment['RUST_SWITCHER_E2E_ALLOW_INJECTED'] = '1'
    $info.Environment['RUST_SWITCHER_E2E_TARGET_PID'] = $editor.Id.ToString()
    $switcher = [System.Diagnostics.Process]::Start($info)
    $ownedProcesses.Add($switcher)
    Wait-Condition { $null -ne (Find-AppWindow $switcher) } 'target-test Switcher startup'
    $switcherWindow = Find-AppWindow $switcher
    $switcherHandle = [IntPtr]$switcherWindow.Current.NativeWindowHandle
    $english = [HostedInput]::LoadKeyboardLayout('00000409', 0)
    $russian = [HostedInput]::LoadKeyboardLayout('00000419', 0)
    if ($english -eq [IntPtr]::Zero -or $russian -eq [IntPtr]::Zero) { throw 'Target-test layouts are missing' }
    [HostedInput]::PostMessage($editorHandle, 0x50, [UIntPtr]::Zero, $english) | Out-Null
    Wait-Condition { [HostedInput]::WindowLayout($editorHandle) -eq $english } 'native target English layout'
    [HostedInput]::SetFixtureText($source, 'ghbdtn')
    [HostedInput]::SetFixtureText($destination, 'untouched')
    Set-OwnedControlFocus $editorWindow $sourceElement | Out-Null
    [HostedInput]::SelectFixtureText($source, 6)
    Tap-Shift 2
    Wait-Condition { [HostedInput]::FixtureText($source) -ceq 'привет' } 'normal default Double Shift positive control'
    Wait-Condition { [HostedInput]::HasSelection($source, 6) } 'normal positive-control reselection'
    if ([HostedInput]::FixtureText($destination) -cne 'untouched') { throw 'Positive control altered its neighbor' }
    Write-Output 'PASS: native target positive control converted once'

    Start-Sleep -Milliseconds 1200
    [HostedInput]::SetFixtureText($source, 'ghbdtn ')
    [HostedInput]::SetFixtureText($destination, 'untouched')
    [HostedInput]::SelectFixtureText($destination, 3)
    Set-OwnedControlFocus $editorWindow $sourceElement | Out-Null
    [HostedInput]::SelectFixtureText($source, 7)
    if (([HostedInput]::GetAsyncKeyState(0x10) -band 0x8000) -ne 0) {
        throw 'Target-test setup has Shift held down'
    }
    $clipboard = [HostedInput]::GetClipboardSequenceNumber()
    if ($clipboard -eq 0) { throw 'Target-test clipboard sequence is unavailable' }
    Set-Content -LiteralPath $signalPath -Value 'ready' -NoNewline
    if ($Race -eq 'Delivery') {
        # A fixture-owned hook runs immediately before Switcher's hook and moves
        # focus on the first actual replacement event, after SendInput begins.
        [HostedInput]::ArmFixture($source, 4)
        $expectedSignal = 'delivery-4'
    } else {
        # The first real WM_GETTEXT starts the fixture's 40 ms timer. Production
        # conversion has acquired its source before that focus transition.
        [HostedInput]::ArmFixture($source, 2)
        $expectedSignal = 'changed-2'
    }
    Tap-Shift 2
    Wait-Condition { (Get-Content -LiteralPath $signalPath -Raw) -ceq $expectedSignal -and
        [HostedInput]::WindowFocus($editorHandle) -eq $destination } "verified focus change at $Race boundary"
    Start-Sleep -Milliseconds 1600
    if ($switcher.HasExited) { throw 'Switcher exited during the target race' }
    $actualSource = [HostedInput]::FixtureText($source)
    $actualDestination = [HostedInput]::FixtureText($destination)
    if ($actualDestination -cne 'untouched') {
        throw 'WRONG_TARGET_WRITE: delayed conversion wrote source text into the independently focused destination'
    }
    if ($actualSource -cne 'ghbdtn ') { throw 'Cancelled target race changed its source text' }
    if (-not [HostedInput]::HasSelectionRange($source, 0, 7) -or
        -not [HostedInput]::HasSelectionRange($destination, 0, 3)) {
        throw 'Cancelled replacement changed the source or neighboring selection'
    }
    if (([HostedInput]::GetAsyncKeyState(0x10) -band 0x8000) -ne 0) {
        throw 'Cancelled replacement left Shift held down'
    }
    if ([HostedInput]::GetClipboardSequenceNumber() -ne $clipboard) { throw 'Target race changed clipboard' }
    Write-Output "PASS: verified $Race race left both native fields unchanged"
    Set-OwnedControlFocus $editorWindow $sourceElement | Out-Null
    Tap-Shift 2
    Wait-Condition { [HostedInput]::FixtureText($source) -ceq 'привет ' -and
        [HostedInput]::HasSelectionRange($source, 0, 7) } 'conversion recovery after cancelled delivery'
    if ([HostedInput]::FixtureText($destination) -cne 'untouched' -or
        [HostedInput]::GetClipboardSequenceNumber() -ne $clipboard) {
        throw 'Recovery conversion changed its neighbor or clipboard'
    }
    Write-Output "PASS: conversion recovered after cancelled $Race race"
    [HostedInput]::PostMessage($switcherHandle, 0x111, [UIntPtr]1103, [IntPtr]::Zero) | Out-Null
    if (-not $switcher.WaitForExit(8000) -or $switcher.ExitCode -ne 0) { throw 'Target-test Switcher did not exit cleanly' }
} finally {
    foreach ($process in $ownedProcesses) {
        if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit(5000) | Out-Null }
        $process.Dispose()
    }
    Remove-Item -LiteralPath $runRoot -Recurse -Force
}
