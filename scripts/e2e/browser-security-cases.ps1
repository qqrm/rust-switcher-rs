param(
    [Parameter(Mandatory = $true)][hashtable]$Sessions,
    [Parameter(Mandatory = $true)][System.Diagnostics.Process]$SwitcherProcess,
    [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[object]]$Results,
    [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Failures
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Password checks require the dedicated GitHub-hosted Windows job.'
}

# Only synthetic fixture passwords are inspected, through the owned page's DOM.
# The application's debug endpoint exposes counts, never input contents.
$switcherHandle = $SwitcherProcess.MainWindowHandle
$session = $Sessions.a

function Read-SecurityState {
    if ($SwitcherProcess.HasExited) { throw 'Switcher exited during password checks' }
    return [pscustomobject]@{
        activeChars = [BrowserInput]::SecurityMetadata($switcherHandle, 1)
        cachedChars = [BrowserInput]::SecurityMetadata($switcherHandle, 2)
        capturedChars = [BrowserInput]::SecurityMetadata($switcherHandle, 3)
        autoConvert = [BrowserInput]::SecurityMetadata($switcherHandle, 4)
    }
}

function Invoke-PasswordCase([string]$Name, [scriptblock]$Action) {
    $record = [ordered]@{name = "browser password $Name"; error = $null; uncovered = $null; details = @{}}
    try { & $Action $record.details } catch { $record.error = $_.Exception.Message }
    if ($record.details.ContainsKey('uncovered')) { $record.uncovered = $record.details.uncovered }
    if ($null -ne $record.error) { $Failures.Add("$($record.name): $($record.error)") }
    $Results.Add([pscustomobject]$record)
    $record | ConvertTo-Json -Depth 8 -Compress | Write-Output
    if ($null -ne $record.error) { Write-Output "FAIL: $($record.name)" }
    elseif ($null -ne $record.uncovered) { Write-Output "UNCOVERED: $($record.name): $($record.uncovered)" }
    else { Write-Output "PASS: $($record.name)" }
}

function Reset-PasswordFixture {
    $expectedOther = @{}
    foreach ($id in @('a', 'b')) {
        Read-Browser $Sessions[$id] "fixture.reset('secure')" | Out-Null
        Set-BrowserEnglish $Sessions[$id]
        $values = Read-Browser $Sessions[$id] 'fixture.values()'
        foreach ($field in @('a', 'b', 'guard')) {
            if ($id -ne 'a' -or $field -ne 'a') { $expectedOther["$id/$field"] = $values.$field }
        }
    }
    Focus-BrowserEditor $session 'a'
    # This is setup for a stable-field test, not an immediate-click timing test.
    Tap-BrowserKey 0x1B
    Wait-Browser {
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        $null -ne $focused -and $focused.Current.Name -eq 'a' -and
            $focused.Current.HasKeyboardFocus -and $focused.Current.IsPassword
    } 'real focused browser password field'
    if ([BrowserInput]::ClassOf([IntPtr]([BrowserInput]::Snapshot($session.Handle)).focus) -match '^(Edit|RichEdit.*|RICHEDIT.*)$') {
        throw 'Browser password fixture unexpectedly uses a native Edit control'
    }
    return $expectedOther
}

function Type-CapturedPassword([string]$Text) {
    $before = Read-SecurityState
    [BrowserInput]::Type($session.Handle, $Text, 0, 0) | Out-Null
    Wait-Browser { (Read-Browser $session 'fixture.values()').a -ceq $Text } 'complete synthetic password input'
    Wait-Browser { (Read-SecurityState).capturedChars -eq $before.capturedChars + $Text.Length } 'complete password journal capture'
    $after = Read-SecurityState
    if ($after.activeChars -lt $Text.Length) { throw 'Password journal did not retain the complete input' }
    return $after
}

function Assert-PasswordTabOutcome($Proof, $Values, [string]$Original, [string]$Converted, $Timing) {
    # Validate the stimulus first. A failed focus transition is never a passed
    # cancellation, nor a detected wrong-target mutation.
    if ($null -eq $Proof -or $Proof.sourceBefore -cne $Original -or
        $Proof.destinationBefore -cne 'b-sentinel' -or $Proof.sourceOwner -cne 'a' -or
        $Proof.finalOwner -cne 'b' -or $Proof.complete -isnot [bool] -or -not $Proof.complete) {
        throw 'Shortcut/Tab stimulus did not move from the prepared source to the destination'
    }
    $tabs = @($Proof.events | Where-Object { $_.type -ceq 'keydown' -and $_.key -ceq 'Tab' })
    $focus = @($Proof.events | Where-Object { $_.type -ceq 'focusin' -and $_.target -ceq 'b' })
    $shifts = @($Proof.events | Where-Object { $_.type -ceq 'keyup' -and $_.key -ceq 'Shift' })
    if ($tabs.Count -ne 1 -or $focus.Count -ne 1 -or
        $tabs[0].target -cne 'a' -or $tabs[0].trusted -isnot [bool] -or -not $tabs[0].trusted -or
        $focus[0].trusted -isnot [bool] -or -not $focus[0].trusted -or
        $null -eq $tabs[0].at -or $null -eq $focus[0].at -or
        -not [double]::IsFinite([double]$tabs[0].at) -or -not [double]::IsFinite([double]$focus[0].at) -or
        $focus[0].at -lt $tabs[0].at) {
        throw 'Shortcut/Tab stimulus lacks ordered trusted Tab and destination focus events'
    }
    # Modifier keyups are always forwarded by the application's hook. Require
    # the actual source releases, not merely a requested sleep or injection.
    if ($shifts.Count -ne 2) { throw 'Shortcut/Tab stimulus must contain both Shift releases' }
    foreach ($release in $shifts) {
        if ($release.target -cne 'a' -or $release.trusted -isnot [bool] -or -not $release.trusted) {
            throw 'Shortcut/Tab stimulus lacks trusted source Shift releases'
        }
    }
    if ($null -eq $Timing -or $Timing.requestedMs -isnot [int] -or $Timing.requestedMs -notin @(0, 30) -or
        $Timing.nativeMs -isnot [double] -or -not [double]::IsFinite($Timing.nativeMs)) {
        throw 'Shortcut/Tab stimulus has no valid native timing observation'
    }
    foreach ($event in @($shifts[0], $shifts[1], $tabs[0])) {
        foreach ($property in @('at', 'eventAt')) {
            $stamp = $event.$property
            if ($null -eq $stamp -or ($stamp -isnot [double] -and $stamp -isnot [long] -and $stamp -isnot [int]) -or
                -not [double]::IsFinite([double]$stamp) -or $stamp -lt 0) {
                throw 'Shortcut/Tab stimulus has a missing or invalid DOM timing observation'
            }
        }
    }
    $eventGap = $tabs[0].eventAt - $shifts[1].eventAt
    $handlerGap = $tabs[0].at - $shifts[1].at
    # Leave margin below the 100 ms conversion delay in submission/event time.
    # Handler time may lag while the application blocks in its keyboard hook;
    # that is delayed processing of the tested input, not a late submission.
    $minimumGap = if ($Timing.requestedMs -eq 0) { 0 } else { 25 }
    if (($Timing.requestedMs -eq 0 -and $Timing.nativeMs -ne 0) -or
        $Timing.nativeMs -lt $minimumGap -or $Timing.nativeMs -ge 75 -or
        $shifts[1].eventAt -lt $shifts[0].eventAt -or $shifts[1].at -lt $shifts[0].at -or
        $eventGap -lt $minimumGap -or $eventGap -ge 75 -or $handlerGap -lt 0) {
        throw 'Shortcut/Tab stimulus missed its bounded timing window before conversion'
    }
    if ($Values.b -cne 'b-sentinel') {
        throw 'WRONG_TARGET_WRITE: conversion changed the field reached by Tab'
    }
    if ($Values.a -cne $Original -and $Values.a -cne $Converted) {
        throw 'PARTIAL_SOURCE_WRITE: shortcut/Tab left a partially replaced source'
    }
    if ($Values.guard -cne 'guard-sentinel') { throw 'Shortcut/Tab changed the unrelated guard field' }
}

if ([BrowserInput]::SecurityMetadata($switcherHandle, 0) -ne 0x52535031) {
    throw 'The debug executable has no hosted journal inspection API'
}

foreach ($command in @(
    @{name = 'Double Shift'; key = 0xA0; text = 'ghbdtn'; expected = 'привет'},
    @{name = 'Double Alt'; key = 0xA4; text = 'ghbdtn vbh'; expected = 'привет мир'}
)) {
    Invoke-PasswordCase "$($command.name) converts captured input in both directions" {
        param($details)
        $other = Reset-PasswordFixture
        $details.before = Type-CapturedPassword $command.text
        $native = [BrowserInput]::Snapshot($session.Handle)
        $clipboard = [BrowserInput]::GetClipboardSequenceNumber()
        if ($clipboard -eq 0) { throw 'Clipboard sequence is unavailable' }
        $details.results = @()
        foreach ($expected in @($command.expected, $command.text)) {
            Tap-BrowserKey $command.key
            Tap-BrowserKey $command.key
            if (-not (Test-BrowserValueWithin $session 'a' $expected 4000)) {
                throw "PASSWORD_CONVERSION_VIOLATION: $($command.name) did not convert the complete captured password"
            }
            Start-Sleep -Milliseconds 150
            if ((Read-Browser $session 'fixture.values()').a -cne $expected) { throw 'Password conversion was not stable' }
            Assert-BrowserTargetFocus $native (Get-BrowserFocusSnapshot $session) 'a' ($session.Handle.ToInt64())
            Assert-BrowserOtherFields $Sessions $other
            if ([BrowserInput]::GetClipboardSequenceNumber() -ne $clipboard) { throw 'Password conversion changed the clipboard' }
            $details.results += $expected
        }
    }
}

Invoke-PasswordCase 'dynamic text-to-password transition preserves typed input' {
    param($details)
    $other = Reset-PasswordFixture
    Read-Browser $session "fixture.setType('a', 'text')" | Out-Null
    Tap-BrowserKey 0x1B
    $details.before = Type-CapturedPassword 'ghbdtn'
    $native = [BrowserInput]::Snapshot($session.Handle)
    $clipboard = [BrowserInput]::GetClipboardSequenceNumber()
    if ($clipboard -eq 0) { throw 'Dynamic password clipboard sequence is unavailable' }
    # No metadata wait between the type change and the next physical-style key.
    Read-Browser $session "fixture.setType('a', 'password')" | Out-Null
    [BrowserInput]::Type($session.Handle, 'vbh', 0, 0) | Out-Null
    Wait-Browser { (Read-Browser $session 'fixture.values()').a -ceq 'ghbdtnvbh' } 'complete dynamic password input'
    Tap-BrowserDoubleShift
    Wait-Browser { (Read-Browser $session 'fixture.values()').a -ceq 'приветмир' } 'dynamic password converts its complete journal'
    Assert-BrowserTargetFocus $native (Get-BrowserFocusSnapshot $session) 'a' ($session.Handle.ToInt64())
    Assert-BrowserOtherFields $Sessions $other
    if ([BrowserInput]::GetClipboardSequenceNumber() -ne $clipboard) { throw 'Dynamic password conversion changed clipboard' }
    $details.after = Read-SecurityState
}

Invoke-PasswordCase 'known partial selection does not replace the whole journal' {
    param($details)
    $other = Reset-PasswordFixture
    $details.before = Type-CapturedPassword 'ghbdtn'
    $native = [BrowserInput]::Snapshot($session.Handle)
    $clipboard = [BrowserInput]::GetClipboardSequenceNumber()
    if ($clipboard -eq 0) { throw 'Selected-password clipboard sequence is unavailable' }
    Read-Browser $session "document.getElementById('a').setSelectionRange(0, 2); true" | Out-Null
    if (-not (Read-Browser $session "fixture.focused('a') && document.getElementById('a').selectionStart === 0 && document.getElementById('a').selectionEnd === 2")) {
        throw 'Partial password selection was not established in the original field'
    }
    # Password providers may withhold TextPattern or its selected ranges. Only
    # claim this regression when the actual UIA endpoints expose the selection.
    # Provider exceptions remain failures; a declared absence is a coverage gap.
    $deadline = [Environment]::TickCount64 + 2000
    $noncollapsed = $false
    do {
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        if ($null -eq $focused -or $focused.Current.Name -cne 'a' -or -not $focused.Current.IsPassword) {
            throw 'UIA focus left the selected password field'
        }
        $pattern = $null
        $supported = $focused.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$pattern)
        $details.textPattern = $supported
        if (-not $supported) { break }
        $ranges = @(([System.Windows.Automation.TextPattern]$pattern).GetSelection())
        $details.rangeCount = $ranges.Count
        foreach ($range in $ranges) {
            if ($range.CompareEndpoints([System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start,
                    $range, [System.Windows.Automation.Text.TextPatternRangeEndpoint]::End) -ne 0) {
                $noncollapsed = $true
            }
        }
        if (-not $noncollapsed) { Start-Sleep -Milliseconds 25 }
    } while (-not $noncollapsed -and [Environment]::TickCount64 -lt $deadline)
    if (-not $noncollapsed) {
        $details.uncovered = 'Installed browser does not expose the password selection endpoints through UIA; no conversion assertion was run'
        return
    }
    Tap-BrowserDoubleShift
    Start-Sleep -Milliseconds 1600
    if ((Read-Browser $session 'fixture.values()').a -cne 'ghbdtn') {
        throw 'PARTIAL_SELECTION_VIOLATION: a known password selection was replaced using unrelated whole-journal text'
    }
    Assert-BrowserTargetFocus $native (Get-BrowserFocusSnapshot $session) 'a' ($session.Handle.ToInt64())
    Assert-BrowserOtherFields $Sessions $other
    if ([BrowserInput]::GetClipboardSequenceNumber() -ne $clipboard) { throw 'Selected-password command changed clipboard' }
}

foreach ($delay in @(0, 30)) {
    Invoke-PasswordCase "Double Shift then Tab at $delay ms retains source ownership" {
        param($details)
        $other = Reset-PasswordFixture
        $original = 'ghbdtn '
        $details.before = Type-CapturedPassword $original
        $clipboard = [BrowserInput]::GetClipboardSequenceNumber()
        if ($clipboard -eq 0) { throw 'Clipboard sequence is unavailable before shortcut/Tab' }
        Read-Browser $session 'fixture.armShortcutTabProbe()' | Out-Null
        $details.timing = [pscustomobject]@{requestedMs = $delay;
            nativeMs = [BrowserInput]::DoubleShiftThenTab($session.Handle, $delay)}
        Wait-Browser { Read-Browser $session "fixture.focused('b')" } 'native Tab reaches the neighboring browser field'
        # Observe through the entire deferred-hotkey and conversion lifetime.
        Start-Sleep -Milliseconds 1600
        $details.proof = Read-Browser $session 'fixture.shortcutTabState()'
        $details.values = Read-Browser $session 'fixture.values()'
        Assert-PasswordTabOutcome $details.proof $details.values $original 'привет ' $details.timing
        Assert-BrowserOtherFields $Sessions $other
        if ([BrowserInput]::GetClipboardSequenceNumber() -ne $clipboard) { throw 'Shortcut/Tab changed the clipboard' }
        # Begin a separate stale-journal check only after the original race has
        # been fully observed. Tab may select B's existing sentinel; collapse
        # that selection so the next command cannot legitimately convert it.
        # This changes no value, focus or journal and does not repair the race.
        if (-not (Read-Browser $session "fixture.collapseEnd('b')")) { throw 'Destination lost focus before the stale-journal check' }
        Tap-BrowserDoubleShift
        Start-Sleep -Milliseconds 1300
        $later = Read-Browser $session 'fixture.values()'
        if ($later.b -cne 'b-sentinel' -or $later.a -cne $details.values.a) {
            throw 'WRONG_TARGET_WRITE: a later command resurrected another field journal'
        }
        Assert-BrowserOtherFields $Sessions $other
        if ([BrowserInput]::GetClipboardSequenceNumber() -ne $clipboard) { throw 'Later target command changed clipboard' }
        $details.after = Read-SecurityState
    }
}
