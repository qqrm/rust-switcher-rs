param([string]$HarnessPath = (Join-Path $PSScriptRoot 'run-browser.ps1'))

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Exercise the production predicates without launching an app or injecting input.
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path -LiteralPath $HarnessPath), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors) { throw ($parseErrors | Out-String) }
foreach ($name in @('Test-BrowserValueWithin', 'Assert-BrowserTargetFocus', 'Assert-BrowserSafeFocusRefusal')) {
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
$stimulusHelpers = @($securityAst.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Assert-DelayedPasswordStimulus'
}, $false))
if ($stimulusHelpers.Count -ne 1) { throw 'Expected one delayed-password stimulus validator' }
. ([scriptblock]::Create($stimulusHelpers[0].Extent.Text))

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

function New-DelayedPasswordFixture {
    return @{Values = [pscustomobject]@{a = 'g'; b = ''}; Proof = [pscustomobject]@{
        complete = $true; prevented = $true; delayMs = 45; downOwner = 'a'; finalOwner = 'b';
        downAt = 100.0; focusAt = 145.0; events = @(
            [pscustomobject]@{type = 'mouseup'; target = 'b'; key = ''; data = ''; trusted = $true; at = 101.0},
            [pscustomobject]@{type = 'keydown'; target = 'a'; key = 'g'; data = ''; trusted = $true; at = 102.0},
            [pscustomobject]@{type = 'input'; target = 'a'; key = ''; data = 'g'; trusted = $true; at = 103.0},
            [pscustomobject]@{type = 'keyup'; target = 'a'; key = 'g'; data = ''; trusted = $true; at = 104.0},
            [pscustomobject]@{type = 'focusin'; target = 'b'; key = ''; data = ''; trusted = $true; at = 145.0}
        )
    }}
}
function Assert-DelayedStimulusRejected([string]$Name, [scriptblock]$Change) {
    $fixture = New-DelayedPasswordFixture
    & $Change $fixture
    $rejected = $false
    try { Assert-DelayedPasswordStimulus $fixture.Proof $fixture.Values | Out-Null }
    catch {
        if ($_.Exception.Message.Contains('SECURITY_CAPTURE_VIOLATION:')) { throw 'Invalid stimulus masqueraded as a security finding' }
        $rejected = $true
    }
    if (-not $rejected) { throw "Invalid delayed password stimulus was accepted: $Name" }
    $checks.Add($Name)
}
$fixture = New-DelayedPasswordFixture
if ((Assert-DelayedPasswordStimulus $fixture.Proof $fixture.Values) -ne 45) {
    throw 'Valid delayed password stimulus did not preserve observed timing'
}
$checks.Add('trusted password input precedes delayed ordinary focus')
Assert-DelayedStimulusRejected 'password never received input' { param($f) $f.Values.a = '' }
Assert-DelayedStimulusRejected 'ordinary field received test key' { param($f) $f.Values.b = 'g' }
Assert-DelayedStimulusRejected 'mousedown did not prevent focus' { param($f) $f.Proof.prevented = $false }
Assert-DelayedStimulusRejected 'password was not original focus owner' { param($f) $f.Proof.downOwner = 'b' }
Assert-DelayedStimulusRejected 'ordinary field never gained focus' { param($f) $f.Proof.finalOwner = 'a' }
Assert-DelayedStimulusRejected 'focus timer too early' { param($f) $f.Proof.focusAt = 124.0 }
Assert-DelayedStimulusRejected 'focus timer outside provider budget' { param($f) $f.Proof.focusAt = 175.0 }
Assert-DelayedStimulusRejected 'actual focus event later than timer timestamp' { param($f) $f.Proof.events[4].at = 180.0 }
Assert-DelayedStimulusRejected 'missing focus timestamp' { param($f) $f.Proof.focusAt = $null }
Assert-DelayedStimulusRejected 'nonfinite focus timestamp' { param($f) $f.Proof.focusAt = [double]::NaN }
Assert-DelayedStimulusRejected 'scripted keyboard event' { param($f) $f.Proof.events[1].trusted = $false }
Assert-DelayedStimulusRejected 'scripted input event' { param($f) $f.Proof.events[2].trusted = $false }
Assert-DelayedStimulusRejected 'key targeted ordinary field' { param($f) $f.Proof.events[1].target = 'b' }
Assert-DelayedStimulusRejected 'input targeted ordinary field' { param($f) $f.Proof.events[2].target = 'b' }
Assert-DelayedStimulusRejected 'input occurred after focus moved' { param($f) $f.Proof.events[2].at = 146.0 }
Assert-DelayedStimulusRejected 'input event absent' { param($f) $f.Proof.events = @($f.Proof.events | Where-Object type -ne 'input') }
Assert-DelayedStimulusRejected 'duplicate input event' { param($f) $f.Proof.events += $f.Proof.events[2] }
if ($checks.Count -ne 53) { throw "Browser outcome regression discovery was incomplete: $($checks.Count)" }
Write-Output "PASS: $($checks.Count) browser outcome helper assertions"
