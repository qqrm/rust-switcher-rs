param([string]$HarnessPath = (Join-Path $PSScriptRoot 'run-browser.ps1'))

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Exercise the production predicates without launching an app or injecting input.
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path -LiteralPath $HarnessPath), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors) { throw ($parseErrors | Out-String) }
foreach ($name in @('Test-BrowserValueWithin', 'Assert-BrowserTargetFocus', 'Assert-BrowserSafeFocusRefusal', 'Assert-DiagnosticRedaction')) {
    $functions = @($ast.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $false))
    if ($functions.Count -ne 1) { throw "Expected one production helper named $name" }
    . ([scriptblock]::Create($functions[0].Extent.Text))
}
$securityPath = Join-Path (Split-Path -Parent $HarnessPath) 'browser-security-cases.ps1'
$securityAst = [System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path -LiteralPath $securityPath), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors) { throw ($parseErrors | Out-String) }
foreach ($name in @('Assert-PasswordTabOutcome')) {
    $helpers = @($securityAst.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $false))
    if ($helpers.Count -ne 1) { throw "Expected one password acceptance helper named $name" }
    . ([scriptblock]::Create($helpers[0].Extent.Text))
}

function New-FocusFixture {
    return @{native = [pscustomobject]@{foreground = 123; focus = 456; error = 0};
        dom = [pscustomobject]@{documentFocused = $true; field = 'b'; activeId = 'b'}}
}
function New-RefusalFixture {
    return @{
        Case = @{Mode = 'click'; Kind = 'editable'; Text = '.ghbdtn'; Expected = 'юпривет'}
        Record = [ordered]@{actualBefore = '.ghbdtn'; actualAfter = '.ghbdtn'; otherFieldsUnchanged = $true;
            focusAfterInput = (New-FocusFixture).native; firstAltFocus = New-FocusFixture;
            secondAltFocus = New-FocusFixture; finalFocus = New-FocusFixture}
        Journal = [pscustomobject]@{activeChars = 0; cachedChars = 0; capturedChars = [long]42;
            pointerBaseline = [long]42; captureUnavailable = 1}
    }
}
$checks = [System.Collections.Generic.List[string]]::new()
function Assert-RefusalRejected([string]$Name, [scriptblock]$Change) {
    $fixture = New-RefusalFixture
    & $Change $fixture
    $rejected = $false
    try { Assert-BrowserSafeFocusRefusal $fixture.Case $fixture.Record $fixture.Journal 'b' 123 }
    catch { $rejected = $true }
    if (-not $rejected) { throw "Unsafe refusal was accepted: $Name" }
    $checks.Add($Name)
}

foreach ($kind in @('textarea', 'editable')) {
    $fixture = New-RefusalFixture
    $fixture.Case.Kind = $kind
    Assert-BrowserSafeFocusRefusal $fixture.Case $fixture.Record $fixture.Journal 'b' 123
    $checks.Add("valid $kind click proof")
}
foreach ($mode in @('fresh', 'tab', 'click-layout', 'window', 'ctrl-release', 'alt-release')) {
    Assert-RefusalRejected "$mode cannot refuse" { param($f) $f.Case.Mode = $mode }
}
Assert-RefusalRejected 'other editor type cannot refuse' { param($f) $f.Case.Kind = 'input' }
Assert-RefusalRejected 'partial output' { param($f) $f.Record.actualAfter = 'юghbdtn' }
Assert-RefusalRejected 'altered initial input' { param($f) $f.Record.actualBefore = '.GHBDTN' }
Assert-RefusalRejected 'other field changed' { param($f) $f.Record.otherFieldsUnchanged = $false }
Assert-RefusalRejected 'other field proof missing' { param($f) $f.Record.otherFieldsUnchanged = $null }
Assert-RefusalRejected 'native focus moved' { param($f) $f.Record.finalFocus.native.focus = 457 }
Assert-RefusalRejected 'native foreground moved' { param($f) $f.Record.finalFocus.native.foreground = 124 }
Assert-RefusalRejected 'native snapshot failed' { param($f) $f.Record.finalFocus.native.error = 5 }
Assert-RefusalRejected 'DOM focus moved' { param($f) $f.Record.finalFocus.dom.field = 'a' }
Assert-RefusalRejected 'DOM document blurred' { param($f) $f.Record.finalFocus.dom.documentFocused = $false }
Assert-RefusalRejected 'temporary Alt focus loss' { param($f) $f.Record.firstAltFocus.dom.documentFocused = $false }
Assert-RefusalRejected 'unavailable is false' { param($f) $f.Journal.captureUnavailable = 0 }
Assert-RefusalRejected 'active history retained' { param($f) $f.Journal.activeChars = 1 }
Assert-RefusalRejected 'cached history retained' { param($f) $f.Journal.cachedChars = 1 }
Assert-RefusalRejected 'captured then cleared' { param($f) $f.Journal.capturedChars++ }
Assert-RefusalRejected 'missing metadata' { param($f) $f.Journal = $null }
Assert-RefusalRejected 'missing counter' { param($f) $f.Journal.PSObject.Properties.Remove('pointerBaseline') }
Assert-RefusalRejected 'null counter' { param($f) $f.Journal.pointerBaseline = $null }
Assert-RefusalRejected 'negative counter' { param($f) $f.Journal.capturedChars = -1 }
Assert-RefusalRejected 'string counter' { param($f) $f.Journal.capturedChars = '42' }
Assert-RefusalRejected 'null unavailable' { param($f) $f.Journal.captureUnavailable = $null }

# Preserve exact 64-bit equality; a floating-point comparison could hide an append.
$fixture = New-RefusalFixture
$fixture.Journal.capturedChars = [long]9007199254740993
$fixture.Journal.pointerBaseline = [long]9007199254740993
Assert-BrowserSafeFocusRefusal $fixture.Case $fixture.Record $fixture.Journal 'b' 123
$checks.Add('exact large physical counter')
Assert-RefusalRejected 'large counter append' {
    param($f)
    $f.Journal.capturedChars = [long]9007199254740993
    $f.Journal.pointerBaseline = [long]9007199254740992
}

function Read-Browser($Session, [string]$Expression) {
    if ($Expression -ne 'fixture.values()') { throw 'Unexpected fixture expression' }
    if ($null -ne $Session.Failure) { throw $Session.Failure }
    return $Session.Values
}
$session = @{Values = [pscustomobject]@{b = 'привет'}; Failure = $null}
if (-not (Test-BrowserValueWithin $session 'b' 'привет' 0)) { throw 'Exact value did not pass' }
$checks.Add('exact value wait succeeds')
if (Test-BrowserValueWithin $session 'b' '.ghbdtn' 0) { throw 'Exhausted wait did not report false' }
$checks.Add('only an exhausted value wait reports false')
foreach ($message in @('fixture protocol error', 'Timed out: exact Double Alt browser result')) {
    $session.Failure = $message
    $propagated = $false
    try { Test-BrowserValueWithin $session 'b' 'привет' 0 | Out-Null } catch {
        if ($_.Exception.Message -ne $message) { throw }
        $propagated = $true
    }
    if (-not $propagated) { throw 'An unrelated browser failure was treated as exhausted value wait' }
    $checks.Add("browser error propagated: $message")
}

function New-PasswordTabFixture {
    return @{Values = [pscustomobject]@{a = 'ghbdtn '; b = 'b-sentinel'; guard = 'guard-sentinel'};
        Timing = [pscustomobject]@{requestedMs = 30; nativeMs = 30.5};
        Proof = [pscustomobject]@{sourceBefore = 'ghbdtn '; destinationBefore = 'b-sentinel';
            sourceOwner = 'a'; finalOwner = 'b'; complete = $true; events = @(
                [pscustomobject]@{type = 'keyup'; target = 'a'; key = 'Shift'; trusted = $true; eventAt = 100.0; at = 100.0},
                [pscustomobject]@{type = 'keyup'; target = 'a'; key = 'Shift'; trusted = $true; eventAt = 101.0; at = 101.0},
                [pscustomobject]@{type = 'keydown'; target = 'a'; key = 'Tab'; trusted = $true; eventAt = 131.0; at = 131.0},
                [pscustomobject]@{type = 'focusin'; target = 'b'; key = ''; trusted = $true; eventAt = 132.0; at = 132.0})}}
}
function Assert-PasswordTabRejected([string]$Name, [scriptblock]$Change, [string]$ExpectedMarker = '') {
    $fixture = New-PasswordTabFixture
    & $Change $fixture
    $rejected = $false
    try { Assert-PasswordTabOutcome $fixture.Proof $fixture.Values 'ghbdtn ' 'привет ' $fixture.Timing }
    catch {
        if ($ExpectedMarker -and -not $_.Exception.Message.StartsWith($ExpectedMarker)) { throw }
        if (-not $ExpectedMarker -and $_.Exception.Message.Contains('WRONG_TARGET_WRITE:')) {
            throw 'An invalid stimulus was mistaken for a wrong-target write'
        }
        $rejected = $true
    }
    if (-not $rejected) { throw "Invalid shortcut/Tab outcome was accepted: $Name" }
    $checks.Add($Name)
}
foreach ($source in @('ghbdtn ', 'привет ')) {
    $fixture = New-PasswordTabFixture
    $fixture.Values.a = $source
    Assert-PasswordTabOutcome $fixture.Proof $fixture.Values 'ghbdtn ' 'привет ' $fixture.Timing
    $checks.Add('complete source with untouched Tab destination: ' + $source)
}
Assert-PasswordTabRejected 'Tab never happened' { param($f) $f.Proof.complete = $false }
Assert-PasswordTabRejected 'missing source proof' { param($f) $f.Proof.sourceBefore = '' }
Assert-PasswordTabRejected 'wrong source owner' { param($f) $f.Proof.sourceOwner = 'b' }
Assert-PasswordTabRejected 'wrong destination owner' { param($f) $f.Proof.finalOwner = 'a' }
Assert-PasswordTabRejected 'scripted Tab' { param($f) $f.Proof.events[2].trusted = $false }
Assert-PasswordTabRejected 'scripted focus' { param($f) $f.Proof.events[3].trusted = $false }
Assert-PasswordTabRejected 'Tab sent to wrong field' { param($f) $f.Proof.events[2].target = 'b' }
Assert-PasswordTabRejected 'focus preceded Tab' { param($f) $f.Proof.events[3].at = 130.0 }
Assert-PasswordTabRejected 'missing Tab timestamp' { param($f) $f.Proof.events[2].at = $null }
Assert-PasswordTabRejected 'nonfinite focus timestamp' { param($f) $f.Proof.events[3].at = [double]::NaN }
Assert-PasswordTabRejected 'missing Tab event' { param($f) $f.Proof.events = @($f.Proof.events[0], $f.Proof.events[1], $f.Proof.events[3]) }
Assert-PasswordTabRejected 'duplicate Tab event' { param($f) $f.Proof.events += $f.Proof.events[2] }
$fixture = New-PasswordTabFixture
$fixture.Timing.requestedMs = 0
$fixture.Timing.nativeMs = 0.0
$fixture.Proof.events[2].eventAt = 101.0
$fixture.Proof.events[2].at = 101.0
Assert-PasswordTabOutcome $fixture.Proof $fixture.Values 'ghbdtn ' 'привет ' $fixture.Timing
$checks.Add('same-batch timing with trusted final Shift release passes')
Assert-PasswordTabRejected 'missing timing proof' { param($f) $f.Timing = $null }
Assert-PasswordTabRejected 'missing native observation' { param($f) $f.Timing.PSObject.Properties.Remove('nativeMs') }
Assert-PasswordTabRejected 'null native observation' { param($f) $f.Timing.nativeMs = $null }
Assert-PasswordTabRejected 'nonfinite native observation' { param($f) $f.Timing.nativeMs = [double]::NaN }
Assert-PasswordTabRejected 'infinite native observation' { param($f) $f.Timing.nativeMs = [double]::PositiveInfinity }
Assert-PasswordTabRejected 'native observation is not numeric' { param($f) $f.Timing.nativeMs = '30.0' }
Assert-PasswordTabRejected 'native observation too early' { param($f) $f.Timing.nativeMs = 24.0 }
Assert-PasswordTabRejected 'native observation at upper bound' { param($f) $f.Timing.nativeMs = 75.0 }
Assert-PasswordTabRejected 'native observation after conversion delay' { param($f) $f.Timing.nativeMs = 150.0 }
Assert-PasswordTabRejected 'unsupported requested timing' { param($f) $f.Timing.requestedMs = 100 }
Assert-PasswordTabRejected 'missing final Shift release' { param($f) $f.Proof.events = @($f.Proof.events[0], $f.Proof.events[2], $f.Proof.events[3]) }
Assert-PasswordTabRejected 'scripted Shift release' { param($f) $f.Proof.events[1].trusted = $false }
Assert-PasswordTabRejected 'wrong Shift release owner' { param($f) $f.Proof.events[1].target = 'b' }
Assert-PasswordTabRejected 'duplicate Shift release' { param($f) $f.Proof.events += $f.Proof.events[1] }
Assert-PasswordTabRejected 'missing Shift event timestamp' { param($f) $f.Proof.events[1].eventAt = $null }
Assert-PasswordTabRejected 'missing Shift handler timestamp' { param($f) $f.Proof.events[1].at = $null }
Assert-PasswordTabRejected 'nonfinite Shift event timestamp' { param($f) $f.Proof.events[1].eventAt = [double]::NaN }
Assert-PasswordTabRejected 'missing Tab event timestamp' { param($f) $f.Proof.events[2].PSObject.Properties.Remove('eventAt') }
Assert-PasswordTabRejected 'nonfinite Tab event timestamp' { param($f) $f.Proof.events[2].eventAt = [double]::PositiveInfinity }
Assert-PasswordTabRejected 'DOM event interval too short' { param($f) $f.Proof.events[2].eventAt = 125.0 }
Assert-PasswordTabRejected 'DOM event interval at upper bound' { param($f) $f.Proof.events[2].eventAt = 176.0 }
Assert-PasswordTabRejected 'DOM event interval after conversion delay' { param($f) $f.Proof.events[2].eventAt = 251.0 }
$fixture = New-PasswordTabFixture
$fixture.Proof.events[2].at = 251.0
$fixture.Proof.events[3].at = 252.0
Assert-PasswordTabOutcome $fixture.Proof $fixture.Values 'ghbdtn ' 'привет ' $fixture.Timing
$checks.Add('timely input remains valid when DOM handlers are delayed')
Assert-PasswordTabRejected 'native zero batch carries delay' { param($f) $f.Timing.requestedMs = 0; $f.Timing.nativeMs = 1.0 }
Assert-PasswordTabRejected 'same-batch event interval too late' {
    param($f)
    $f.Timing.requestedMs = 0; $f.Timing.nativeMs = 0.0
    $f.Proof.events[2].eventAt = 176.0
}
Assert-PasswordTabRejected 'same-batch Tab preceded Shift release' {
    param($f)
    $f.Timing.requestedMs = 0; $f.Timing.nativeMs = 0.0
    $f.Proof.events[2].eventAt = 100.0
}
Assert-PasswordTabRejected 'original password leaked' { param($f) $f.Values.b += 'ghbdtn ' } 'WRONG_TARGET_WRITE:'
Assert-PasswordTabRejected 'converted password leaked' { param($f) $f.Values.b += 'привет ' } 'WRONG_TARGET_WRITE:'
Assert-PasswordTabRejected 'suffix leaked' { param($f) $f.Values.b += ' ' } 'WRONG_TARGET_WRITE:'
Assert-PasswordTabRejected 'partial source replacement' { param($f) $f.Values.a = 'пhbdtn ' } 'PARTIAL_SOURCE_WRITE:'
Assert-PasswordTabRejected 'unrelated field changed' { param($f) $f.Values.guard = '' }
Assert-DiagnosticRedaction 'TRACE conversion completed; converted_len=9'
$checks.Add('metadata-only diagnostics pass')
foreach ($text in @('TRACE original=ghbdtnvbh', 'TRACE converted=приветмир')) {
    $rejected = $false
    try { Assert-DiagnosticRedaction $text } catch {
        if (-not $_.Exception.Message.StartsWith('INPUT_LOG_VIOLATION:')) { throw }
        $rejected = $true
    }
    if (-not $rejected) { throw 'Raw fixture input was accepted in diagnostics' }
    $checks.Add('secret diagnostic rejected')
}
$rejected = $false
try { Assert-DiagnosticRedaction '' } catch { $rejected = $true }
if (-not $rejected) { throw 'Absent diagnostics cannot prove redaction' }
$checks.Add('missing diagnostics rejected')
Write-Output "PASS: $($checks.Count) browser outcome helper assertions"
