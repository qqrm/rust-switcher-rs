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
foreach ($name in @('Assert-DelayedPasswordStimulus', 'Assert-CompleteOrdinarySegment', 'Get-OrdinaryTransitionOutcome')) {
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

function New-OrdinaryTransitionFixture {
    $focus = New-FocusFixture
    $focus.dom.field = 'guard'
    $focus.dom.activeId = 'guard'
    return @{Purpose = 'password-recovery'; Record = @{
        field = 'guard'; text = 'ghbdtn'; actual = 'ghbdtn'; otherFieldsUnchanged = $true;
        before = [pscustomobject]@{capturedChars = [long]100}; focusAfter = $focus;
        inputTrace = @{before = (New-FocusFixture).native; afterFirst = (New-FocusFixture).native;
            afterRest = (New-FocusFixture).native}
    }; Journal = [pscustomobject]@{capturedChars = [long]100; pointerBaseline = [long]100;
        activeChars = 0; cachedChars = 0; captureUnavailable = 1}}
}
function Assert-OrdinaryTransitionRejected([string]$Name, [scriptblock]$Change) {
    $fixture = New-OrdinaryTransitionFixture
    & $Change $fixture
    $rejected = $false
    try { Get-OrdinaryTransitionOutcome $fixture.Purpose $fixture.Record $fixture.Journal 123 | Out-Null }
    catch { $rejected = $true }
    if (-not $rejected) { throw "Invalid ordinary transition was accepted: $Name" }
    $checks.Add($Name)
}
foreach ($purpose in @('password-recovery', 'type-change-prefix')) {
    $fixture = New-OrdinaryTransitionFixture
    $fixture.Purpose = $purpose
    if ($purpose -eq 'type-change-prefix') {
        $fixture.Record.field = 'a'; $fixture.Record.text = 'keep'; $fixture.Record.actual = 'keep'
        $fixture.Record.focusAfter.dom.field = 'a'; $fixture.Record.focusAfter.dom.activeId = 'a'
    }
    if ((Get-OrdinaryTransitionOutcome $purpose $fixture.Record $fixture.Journal 123) -cne 'safe_focus_refusal') {
        throw 'A complete zero-capture ordinary refusal was not identified'
    }
    $checks.Add("verified initial refusal: $purpose")
    $fixture.Journal.capturedChars += $fixture.Record.text.Length
    $fixture.Journal.activeChars = $fixture.Record.text.Length
    $fixture.Journal.captureUnavailable = 0
    if ((Get-OrdinaryTransitionOutcome $purpose $fixture.Record $fixture.Journal 123) -cne 'captured') {
        throw 'A complete initial ordinary capture was not identified'
    }
    $checks.Add("complete initial capture: $purpose")
}
Assert-OrdinaryTransitionRejected 'unrelated case cannot use ordinary refusal' { param($f) $f.Purpose = 'last-word-positive' }
Assert-OrdinaryTransitionRejected 'wrong ordinary field' { param($f) $f.Record.field = 'a' }
Assert-OrdinaryTransitionRejected 'ordinary input never arrived' { param($f) $f.Record.actual = '' }
Assert-OrdinaryTransitionRejected 'partial ordinary input' { param($f) $f.Record.actual = 'hbdtn' }
Assert-OrdinaryTransitionRejected 'ordinary neighbor changed' { param($f) $f.Record.otherFieldsUnchanged = $false }
Assert-OrdinaryTransitionRejected 'ordinary native focus changed' { param($f) $f.Record.focusAfter.native.focus = 457 }
Assert-OrdinaryTransitionRejected 'ordinary DOM focus changed' { param($f) $f.Record.focusAfter.dom.field = 'a' }
Assert-OrdinaryTransitionRejected 'ordinary document blurred' { param($f) $f.Record.focusAfter.dom.documentFocused = $false }
Assert-OrdinaryTransitionRejected 'partial physical capture before refusal' { param($f) $f.Journal.capturedChars++ }
Assert-OrdinaryTransitionRejected 'transient complete capture before refusal' { param($f) $f.Journal.capturedChars += 6 }
Assert-OrdinaryTransitionRejected 'ordinary refusal was not explicit' { param($f) $f.Journal.captureUnavailable = 0 }
Assert-OrdinaryTransitionRejected 'ordinary refusal retains active text' { param($f) $f.Journal.activeChars = 1 }
Assert-OrdinaryTransitionRejected 'ordinary refusal retains cached text' { param($f) $f.Journal.cachedChars = 1 }
Assert-OrdinaryTransitionRejected 'ordinary pointer baseline differs' { param($f) $f.Journal.pointerBaseline-- }
Assert-OrdinaryTransitionRejected 'ordinary metadata missing' { param($f) $f.Journal = $null }
Assert-OrdinaryTransitionRejected 'ordinary counter missing' { param($f) $f.Journal.PSObject.Properties.Remove('capturedChars') }
Assert-OrdinaryTransitionRejected 'ordinary counter null' { param($f) $f.Journal.capturedChars = $null }
Assert-OrdinaryTransitionRejected 'ordinary counter string' { param($f) $f.Journal.capturedChars = '100' }

$freshBefore = [pscustomobject]@{capturedChars = [long]100}
$freshAfter = [pscustomobject]@{capturedChars = [long]106; activeChars = 6; cachedChars = 0; captureUnavailable = 0}
Assert-CompleteOrdinarySegment $freshBefore $freshAfter 6
$checks.Add('fresh segment captured exactly six characters')
foreach ($change in @(
    @{name = 'fresh segment lost one character'; property = 'capturedChars'; value = [long]105},
    @{name = 'fresh segment captured an extra character'; property = 'capturedChars'; value = [long]107},
    @{name = 'fresh segment journal is incomplete'; property = 'activeChars'; value = 5},
    @{name = 'fresh segment remains unavailable'; property = 'captureUnavailable'; value = 1},
    @{name = 'fresh segment retains another cache'; property = 'cachedChars'; value = 1}
)) {
    $after = [pscustomobject]@{capturedChars = [long]106; activeChars = 6; cachedChars = 0; captureUnavailable = 0}
    $after.($change.property) = $change.value
    $rejected = $false
    try { Assert-CompleteOrdinarySegment $freshBefore $after 6 } catch { $rejected = $true }
    if (-not $rejected) { throw "Invalid fresh ordinary segment accepted: $($change.name)" }
    $checks.Add($change.name)
}
if ($checks.Count -ne 81) { throw "Browser outcome regression discovery was incomplete: $($checks.Count)" }
Write-Output "PASS: $($checks.Count) browser outcome helper assertions"
