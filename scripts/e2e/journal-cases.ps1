param(
    [Parameter(Mandatory = $true)][System.Diagnostics.Process]$DebugProcess,
    [Parameter(Mandatory = $true)]$AppWindow,
    [Parameter(Mandatory = $true)]$AppPlayground,
    [Parameter(Mandatory = $true)][IntPtr]$EnglishLayout,
    [Parameter(Mandatory = $true)][System.Diagnostics.Process]$FixtureProcess,
    [Parameter(Mandatory = $true)][string]$ManifestPath,
    [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[string]]$Failures,
    [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.List[object]]$Results
)

# Called by run-hosted.ps1; its native input and UIA helpers stay in scope.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Journal input checks require the isolated GitHub-hosted Windows job.'
}

function Get-JournalVirtualKey([char]$Character) {
    if ($Character -eq '.') { return [ushort]0xBE } # VK_OEM_PERIOD; ASCII 0x2E is Delete.
    if ($Character -eq ' ') { return [ushort]0x20 }
    if ($Character -ge 'a' -and $Character -le 'z') {
        return [ushort][char]::ToUpperInvariant($Character)
    }
    throw "Unsupported journal fixture character: $Character"
}
function Set-JournalEnglish([IntPtr]$Window) {
    if (-not [HostedInput]::PostMessage($Window, 0x50, [UIntPtr]::Zero, $EnglishLayout)) {
        throw 'Fixture layout request was rejected'
    }
    Wait-Condition { [HostedInput]::WindowLayout($Window) -eq $EnglishLayout } 'fixture English layout'
}
function Set-JournalModifierSourceFocus($SourceWindow, $SourceEdit) {
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

$cases = [System.Collections.Generic.List[object]]::new()
foreach ($sample in @(
    @{ Text = '.'; Expected = 'ю' },
    @{ Text = 'ghbdtn'; Expected = 'привет' },
    @{ Text = '.ghbdtn'; Expected = 'юпривет' },
    @{ Text = 'ghbdtn vbh'; Expected = 'привет мир' }
)) {
    $cases.Add(@{ Mode = 'stable'; Text = $sample.Text; Expected = $sample.Expected; Modifier = 0; Delay = 0; Repeat = 1 })
}
foreach ($mode in @('window', 'control')) {
    foreach ($sample in @(@{ Text = 'ghbdtn'; Expected = 'привет' }, @{ Text = '.ghbdtn'; Expected = 'юпривет' })) {
        foreach ($repeat in 1..2) {
            $cases.Add(@{ Mode = $mode; Text = $sample.Text; Expected = $sample.Expected; Modifier = 0; Delay = 0; Repeat = $repeat })
        }
    }
}
foreach ($modifier in @(@{ Name = 'ctrl-release'; Key = 0xA2 }, @{ Name = 'alt-release'; Key = 0xA4 })) {
    foreach ($sample in @(@{ Text = 'ghbdtn'; Expected = 'привет' }, @{ Text = '.ghbdtn'; Expected = 'юпривет' })) {
        foreach ($delay in @(0, 1, 20)) {
            foreach ($repeat in 1..2) {
                $cases.Add(@{ Mode = $modifier.Name; Text = $sample.Text; Expected = $sample.Expected; Modifier = $modifier.Key; Delay = $delay; Repeat = $repeat })
            }
        }
    }
}
# These characterize the unchanged 100-character policy separately from short
# first-character failures. They do not claim that converting only a tail is ideal.
$cases.Add(@{ Mode = 'capacity-100'; Text = 'a' * 100; Expected = 'ф' * 100; Modifier = 0; Delay = 0; Repeat = 1 })
$cases.Add(@{ Mode = 'capacity-101'; Text = 'a' * 101; Expected = 'a' + ('ф' * 100); Modifier = 0; Delay = 0; Repeat = 1 })
$cases.Add(@{ Mode = 'capacity-leading-dot'; Text = '.' + ('a' * 100); Expected = '.' + ('ф' * 100); Modifier = 0; Delay = 0; Repeat = 1 })

$appHandle = [IntPtr]$AppWindow.Current.NativeWindowHandle
$appValue = Get-Pattern $AppPlayground ([System.Windows.Automation.ValuePattern]::Pattern)
$fixture = $FixtureProcess
try {
    Wait-Condition {
        if ($fixture.HasExited) { throw "Journal fixture exited: $($fixture.ExitCode)" }
        if (-not (Test-Path -LiteralPath $ManifestPath)) { return $false }
        try { return (Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json).generation -gt 0 }
        catch { return $false }
    } 'journal fixture windows' 30000
    $manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
    $firstWindow = [IntPtr]$manifest.a
    $seenEditors = [System.Collections.Generic.HashSet[long]]::new()
    foreach ($case in $cases) {
        $name = "journal $($case.Mode) '$($case.Text)' delay=$($case.Delay) repeat=$($case.Repeat)"
        $record = [ordered]@{
            scenario = $case.Mode; iteration = $case.Repeat; requestedReleaseDelayMs = $case.Delay
            observedReleaseDelayMs = $null; input = $case.Text; expected = $case.Expected
            sourceFocusRequests = $null
            actualBefore = $null; actualAfter = $null; otherEditorsUnchanged = $null; error = $null
        }
        $heldModifier = [ushort]0
        $value = $null
        $values = @{}
        $target = $null
        try {
            if ($DebugProcess.HasExited -or $fixture.HasExited) { throw 'An owned journal process exited' }
            $generation = [HostedInput]::ResetJournalFixture($firstWindow)
            $manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
            if ($manifest.generation -ne $generation) { throw 'Fixture generation mismatch' }
            $windows = @{}
            foreach ($id in @('a', 'b')) {
                $handle = [IntPtr]$manifest.$id
                if ([HostedInput]::GetWindow($handle, 4) -ne [IntPtr]::Zero -or
                    [HostedInput]::WindowProcess($handle) -ne $fixture.Id -or $fixture.Id -eq $DebugProcess.Id) {
                    throw 'Fixture must be unowned and belong to the separate allowlisted process'
                }
                $windows[$id] = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
            }
            $edits = @{}
            $values = @{}
            foreach ($id in @('a1', 'a2', 'b1', 'b2')) {
                $handle = [long]$manifest.$id
                if (-not $seenEditors.Add($handle)) { throw "Fixture reused an editor HWND: $handle" }
                $edits[$id] = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$handle)
                $values[$id] = Get-Pattern $edits[$id] ([System.Windows.Automation.ValuePattern]::Pattern)
            }
            $target = switch ($case.Mode) { 'window' { 'b1' } 'control' { 'a2' } default { 'a1' } }
            $targetWindow = if ($target.StartsWith('b')) { $windows.b } else { $windows.a }
            # Addressed setup sends no keyboard events to the destination.
            $values[$target].SetValue('')
            $appValue.SetValue('app-untouched')
            Set-JournalEnglish ([IntPtr]$targetWindow.Current.NativeWindowHandle)
            Set-JournalEnglish $appHandle
            if ($case.Modifier -ne 0) {
                $record.sourceFocusRequests = Set-JournalModifierSourceFocus $AppWindow $AppPlayground
                $heldModifier = [ushort]$case.Modifier
                [HostedInput]::Key($heldModifier, $false)
                # F24 establishes a real modifier chord in the source queue,
                # changes no fixture text, and prevents its release counting as
                # an extra standalone Alt tap in the upcoming Double Alt.
                Tap-Key 0x87
            } elseif ($case.Mode -in @('window', 'control')) {
                Focus-App $windows.a
                $edits.a1.SetFocus()
                Wait-Condition { $edits.a1.Current.HasKeyboardFocus } 'journal source focus'
            }
            Focus-App $targetWindow
            $edits[$target].SetFocus()
            Wait-Condition { $edits[$target].Current.HasKeyboardFocus } 'first-character destination focus'
            if ([HostedInput]::WindowLayout([IntPtr]$targetWindow.Current.NativeWindowHandle) -ne $EnglishLayout) {
                throw 'Destination layout changed before its first key'
            }
            $offset = 0
            if ($heldModifier -ne 0) {
                $firstKey = Get-JournalVirtualKey $case.Text[0]
                $record.observedReleaseDelayMs = [HostedInput]::ReleaseThenFirstKey($heldModifier, $firstKey, $case.Delay)
                $heldModifier = 0
                $offset = 1
            }
            for ($index = $offset; $index -lt $case.Text.Length; $index++) {
                Tap-Key (Get-JournalVirtualKey $case.Text[$index])
            }
            $value = $values[$target]
            try {
                Wait-Condition { $value.Current.Value -eq $case.Text } 'exact journal input before conversion' 2000
            } finally {
                $record.actualBefore = $value.Current.Value
            }
            Tap-Key 0xA4; Start-Sleep -Milliseconds 100; Tap-Key 0xA4
            Wait-Condition { $value.Current.Value -eq $case.Expected } 'exact journal conversion result' 2000
            Start-Sleep -Milliseconds 100
            $record.actualAfter = $value.Current.Value
            if ($record.actualAfter -ne $case.Expected) { throw 'Journal conversion did not remain stable' }
        } catch {
            $record.error = $_.Exception.Message
            # Preserve exact strings even when an assertion times out.
            if ($null -ne $value) {
                try { $record.actualAfter = $value.Current.Value } catch { }
            }
        } finally {
            if ($heldModifier -ne 0) { [HostedInput]::Key($heldModifier, $true) }
            if ($values.Count -eq 4 -and $null -ne $target) {
                try {
                    $unchanged = $appValue.Current.Value -eq 'app-untouched'
                    foreach ($id in @('a1', 'a2', 'b1', 'b2')) {
                        if ($id -ne $target -and $values[$id].Current.Value -ne 'untouched') { $unchanged = $false }
                    }
                    $record.otherEditorsUnchanged = $unchanged
                    if (-not $unchanged) { $record.error = "$($record.error) Input modified an editor outside the destination.".Trim() }
                } catch {
                    $record.error = "$($record.error) Unable to verify other editors: $($_.Exception.Message)".Trim()
                }
            }
        }
        if ($null -eq $record.error) {
            Record-Pass $name
        } else {
            $Failures.Add("${name}: $($record.error)")
            Write-Output "FAIL: ${name}: $($record.error)"
        }
        $Results.Add([pscustomobject]$record)
        $record | ConvertTo-Json -Compress | Write-Output
    }
    [HostedInput]::PostMessage($firstWindow, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    if (-not $fixture.WaitForExit(5000) -or $fixture.ExitCode -ne 0) { throw 'Journal fixture did not exit successfully' }
} finally {
    if (-not $fixture.HasExited) {
        $fixture.Kill()
        if (-not $fixture.WaitForExit(5000)) { throw 'Journal fixture cleanup timed out' }
    }
}
