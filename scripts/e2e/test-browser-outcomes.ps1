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
if ($checks.Count -lt 30) { throw 'Browser outcome regression discovery was incomplete' }
Write-Output "PASS: $($checks.Count) browser outcome helper assertions"
