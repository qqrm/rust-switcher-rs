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

# The parent owns every process, browser connection, and temporary profile.
# Only the synthetic local fixture's DOM is inspected. No password is obtained
# through UIA, and the application's inspection channel returns counts only.
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

function Read-PasswordCaptureState($Before) {
    $after = Read-SecurityState
    if ($after.capturedChars -ne $Before.capturedChars) {
        # The mutation gate requires this specific failure. A broken browser,
        # absent inspection API, or unrelated timeout is not a detected mutant.
        throw "SECURITY_CAPTURE_VIOLATION: password input entered the journal (capture delta=$($after.capturedChars - $Before.capturedChars))"
    }
    return $after
}

function Assert-NoPasswordCapture($Before, [int]$CleanupMilliseconds = 0) {
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        # Capture is forbidden immediately, including while a background UIA
        # verdict is still clearing ordinary history from the previous field.
        $after = Read-PasswordCaptureState $Before
        if ($after.activeChars -eq 0 -and $after.cachedChars -eq 0) { return $after }
        if ($clock.ElapsedMilliseconds -ge $CleanupMilliseconds) {
            throw "Password context retained journal data: active=$($after.activeChars), cached=$($after.cachedChars)"
        }
        Start-Sleep -Milliseconds 10
    } while ($true)
}

function Assert-BrowserPassword([bool]$Expected, [string]$Id = 'a') {
    Wait-Browser {
        try {
            $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
            $null -ne $focused -and $focused.Current.HasKeyboardFocus -and
                $focused.Current.Name -eq $Id -and $focused.Current.IsPassword -eq $Expected
        } catch { $false }
    } "real focused browser UIA IsPassword=$Expected ($Id)"
    $native = [BrowserInput]::Snapshot($session.Handle)
    $class = [BrowserInput]::ClassOf([IntPtr]$native.focus)
    if ($class -match '^(Edit|RichEdit.*|RICHEDIT.*)$') {
        throw "Browser password fixture unexpectedly uses native password-style detection: $class"
    }
    return @{focus = $native.focus; nativeClass = $class; isPassword = $Expected}
}

function Reset-PasswordFixture([string]$Kind = 'secure') {
    foreach ($target in @($Sessions.a, $Sessions.b)) {
        Read-Browser $target "fixture.reset('$Kind')" | Out-Null
        Set-BrowserEnglish $target
        Read-Browser $target "fixture.set('b', '')" | Out-Null
        Read-Browser $target "fixture.set('guard', '')" | Out-Null
        Focus-BrowserEditor $target 'b'
        Tap-BrowserKey 0x1B
    }
    Focus-BrowserEditor $session 'b'
}

function Type-NormalFixture($Target, [string]$Id, [string]$Text) {
    $before = Read-SecurityState
    [BrowserInput]::Type($Target.Handle, $Text, 0, 0) | Out-Null
    Wait-Browser { (Read-Browser $Target 'fixture.values()').$Id -eq $Text } 'exact ordinary input before password check'
    Wait-Browser { (Read-SecurityState).capturedChars -ge $before.capturedChars + $Text.Length } 'every ordinary character actually journaled'
}

function Type-PasswordFixture([string]$Text, $Before) {
    # No polling of the application's guard precedes the first tested key.
    [BrowserInput]::Type($session.Handle, $Text, 0, 0) | Out-Null
    Read-PasswordCaptureState $Before | Out-Null
    Wait-Browser {
        Read-PasswordCaptureState $Before | Out-Null
        (Read-Browser $session 'fixture.values()').a -eq $Text
    } 'exact synthetic password input'
    return Assert-NoPasswordCapture $Before 1000
}

function Assert-PasswordStable([string]$Text, $Before, [int]$Milliseconds = 1250) {
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        if ((Read-Browser $session 'fixture.values()').a -ne $Text) {
            throw 'A text conversion modified the synthetic password'
        }
        Assert-NoPasswordCapture $Before | Out-Null
        Start-Sleep -Milliseconds 25
    } while ($clock.ElapsedMilliseconds -lt $Milliseconds)
}

function Tap-PasswordChord([ushort]$Key, [int]$Count) {
    for ($i = 0; $i -lt $Count; $i++) {
        if ($i -gt 0) { Start-Sleep -Milliseconds 100 }
        Tap-BrowserKey $Key
    }
}

function Invoke-PasswordCase([string]$Name, [scriptblock]$Action) {
    $record = [ordered]@{name = "browser password $Name"; error = $null; details = @{}}
    try { & $Action $record.details } catch { $record.error = $_.Exception.Message }
    if ($null -ne $record.error) { $Failures.Add("$($record.name): $($record.error)") }
    $Results.Add([pscustomobject]$record)
    $record | ConvertTo-Json -Depth 6 -Compress | Write-Output
    if ($null -eq $record.error) { Write-Output "PASS: $($record.name)" }
    else { Write-Output "FAIL: $($record.name)" }
}

Read-Browser $session "fixture.reset('secure')" | Out-Null
Focus-BrowserEditor $session 'b'
if ([BrowserInput]::SecurityMetadata($switcherHandle, 0) -ne 0x52535031) {
    throw 'The debug executable has no hosted password inspection API'
}

Invoke-PasswordCase 'UIA protection and all text hotkeys' {
    param($details)
    Reset-PasswordFixture
    Type-NormalFixture $session 'b' 'ghbdtn'
    # No click between windows: preserve the normal journal as a cached context.
    Focus-Browser $Sessions.b
    Read-Browser $Sessions.b "fixture.focus('b')" | Out-Null
    Type-NormalFixture $Sessions.b 'b' 'rfrltkf'
    $details.before = Read-SecurityState
    if ($details.before.activeChars -lt 7 -or $details.before.cachedChars -lt 6) {
        throw 'Password cache-clearing test requires populated active and cached ordinary contexts'
    }
    Focus-Browser $session
    Read-Browser $session "fixture.focus('a')" | Out-Null
    $before = $details.before
    Type-PasswordFixture 'ghbdtn' $before | Out-Null
    $details.provider = Assert-BrowserPassword $true
    $details.commands = @()
    foreach ($command in @(
        @{name = 'last-word'; key = 0xA0; count = 2; selection = $false},
        @{name = 'smart-last-word'; key = 0xA0; count = 3; selection = $false},
        @{name = 'last-sequence'; key = 0xA4; count = 2; selection = $false},
        @{name = 'smart-last-sequence'; key = 0xA4; count = 3; selection = $false},
        @{name = 'selection'; key = 0xA0; count = 2; selection = $true},
        @{name = 'smart-selection'; key = 0xA0; count = 3; selection = $true}
    )) {
        Read-Browser $session "fixture.focus('a')" | Out-Null
        if ($command.selection) { Read-Browser $session "fixture.select('a')" | Out-Null }
        Tap-PasswordChord $command.key $command.count
        Assert-PasswordStable 'ghbdtn' $before
        $details.commands += $command.name
    }
    $details.after = Assert-NoPasswordCapture $before
}

Invoke-PasswordCase 'same native HWND click transition' {
    param($details)
    Reset-PasswordFixture
    Type-NormalFixture $session 'b' 'keep'
    $nativeBefore = [BrowserInput]::Snapshot($session.Handle).focus
    $before = Read-SecurityState
    Focus-BrowserEditor $session 'a'
    Type-PasswordFixture '.ghbdtn' $before | Out-Null
    $details.provider = Assert-BrowserPassword $true
    if ($details.provider.focus -ne $nativeBefore) { throw 'Click fixture did not preserve its native focus HWND' }
    Assert-PasswordStable '.ghbdtn' $before 2250
    $details.before = $before
    $details.after = Assert-NoPasswordCapture $before
}

Invoke-PasswordCase 'same native HWND keyboard transition' {
    param($details)
    Reset-PasswordFixture
    Type-NormalFixture $session 'b' 'keep'
    $nativeBefore = [BrowserInput]::Snapshot($session.Handle).focus
    $before = Read-SecurityState
    # Shift+Tab enters the preceding password field. The first glyph follows
    # immediately; neither a guard wait nor CDP focus restoration is allowed.
    [BrowserInput]::Key(0xA0, $false)
    try { Tap-BrowserKey 0x09 } finally { [BrowserInput]::Key(0xA0, $true) }
    Type-PasswordFixture 'ghbdtn' $before | Out-Null
    if (-not (Read-Browser $session "fixture.focused('a')")) { throw 'Shift+Tab did not focus the password field' }
    $details.provider = Assert-BrowserPassword $true
    if ($details.provider.focus -ne $nativeBefore) { throw 'Tab fixture did not preserve its native focus HWND' }
    Assert-PasswordStable 'ghbdtn' $before 2250
    $details.after = Assert-NoPasswordCapture $before
}

Invoke-PasswordCase 'focused input changes from text to password' {
    param($details)
    Reset-PasswordFixture 'input'
    Focus-BrowserEditor $session 'a'
    Type-NormalFixture $session 'a' 'keep'
    $details.normalProvider = Assert-BrowserPassword $false
    $before = Read-SecurityState
    Read-Browser $session "fixture.set('a', ''); fixture.setType('a', 'password')" | Out-Null
    Type-PasswordFixture 'ghbdtn' $before | Out-Null
    $details.passwordProvider = Assert-BrowserPassword $true
    if ($details.normalProvider.focus -ne $details.passwordProvider.focus) {
        throw 'Changing the HTML input type unexpectedly changed native focus'
    }
    Assert-PasswordStable 'ghbdtn' $before 2250
    $details.after = Assert-NoPasswordCapture $before
}

Invoke-PasswordCase 'layout switching pause and automatic conversion' {
    param($details)
    Reset-PasswordFixture
    $before = Read-SecurityState
    Focus-BrowserEditor $session 'a'
    $details.provider = Assert-BrowserPassword $true
    $layout = [BrowserInput]::LayoutOf($session.Handle)
    Tap-BrowserKey 0x14
    Wait-Browser { [BrowserInput]::LayoutOf($session.Handle) -ne $layout } 'CapsLock still switches layout in password field'
    Tap-BrowserKey 0x14
    Wait-Browser { [BrowserInput]::LayoutOf($session.Handle) -eq $layout } 'CapsLock restores layout in password field'
    $initialAuto = (Read-SecurityState).autoConvert
    Tap-PasswordChord 0xA3 2
    Wait-Browser { (Read-SecurityState).autoConvert -ne $initialAuto } 'Right Ctrl pause toggle in password field'
    if ((Read-SecurityState).autoConvert -ne 1) {
        Tap-PasswordChord 0xA3 2
        Wait-Browser { (Read-SecurityState).autoConvert -eq 1 } 'AutoConvert enabled for protected-input test'
    }
    Type-PasswordFixture 'ghbdtn ' $before | Out-Null
    Assert-PasswordStable 'ghbdtn ' $before
    if ((Read-SecurityState).autoConvert -ne $initialAuto) {
        Tap-PasswordChord 0xA3 2
        Wait-Browser { (Read-SecurityState).autoConvert -eq $initialAuto } 'restore runtime pause state'
    }
    $details.after = Assert-NoPasswordCapture $before
}

Invoke-PasswordCase 'ordinary custom field recovers with every character' {
    param($details)
    Reset-PasswordFixture
    $before = Read-SecurityState
    Focus-BrowserEditor $session 'a'
    Type-PasswordFixture 'secretfixture' $before | Out-Null
    $details.passwordProvider = Assert-BrowserPassword $true
    Assert-PasswordStable 'secretfixture' $before 2250
    Focus-BrowserEditor $session 'guard'
    Type-NormalFixture $session 'guard' 'ghbdtn'
    $details.normalProvider = Assert-BrowserPassword $false 'guard'
    Tap-PasswordChord 0xA4 2
    Wait-Browser { (Read-Browser $session 'fixture.values()').guard -eq 'привет' } 'all six ordinary characters convert after password focus'
    if ((Read-Browser $session 'fixture.values()').a -ne 'secretfixture') { throw 'Normal conversion changed the neighboring password field' }
    $details.result = 'ghbdtn -> привет'
    $details.after = Read-SecurityState
}
