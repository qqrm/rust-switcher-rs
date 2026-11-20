param([string]$HarnessPath = (Join-Path $PSScriptRoot 'run-browser.ps1'))

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Load the real helpers without launching browsers or any Windows input API.
# The child script below reproduces run-browser -> browser-security-cases ->
# Read-Browser -> Invoke-Cdp, including PowerShell's dynamic script scope.
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Resolve-Path -LiteralPath $HarnessPath), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors) { throw ($parseErrors | Out-String) }
foreach ($name in @('Invoke-Cdp', 'Read-Browser')) {
    $functions = @($ast.FindAll({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $false))
    if ($functions.Count -ne 1) { throw "Expected one production helper named $name" }
    . ([scriptblock]::Create($functions[0].Extent.Text))
}

class FixtureCdpConnection {
    [int]$RequestId = 0
    [string]$Failure = ''
    [System.Collections.Generic.List[int]]$SentIds = [System.Collections.Generic.List[int]]::new()
    [System.Collections.Generic.Queue[string]]$Replies = [System.Collections.Generic.Queue[string]]::new()

    [int] NextRequestId() { $this.RequestId++; return $this.RequestId }
    [void] Send([string]$Message) {
        $request = $Message | ConvertFrom-Json
        if ($request.method -ne 'Runtime.evaluate') { throw 'Unexpected fixture method' }
        $this.SentIds.Add([int]$request.id)
        $this.Replies.Enqueue('{"method":"Runtime.executionContextCreated","params":{}}')
        $this.Replies.Enqueue('{"id":-1,"result":{}}')
        $response = @{id = $request.id; result = @{result = @{value = $request.params.expression}}}
        if ($this.Failure -eq 'protocol') {
            $response = @{id = $request.id; error = @{message = 'fixture protocol failure'}}
        } elseif ($this.Failure -eq 'evaluation') {
            $response = @{id = $request.id; result = @{exceptionDetails = @{text = 'fixture evaluation failure'}}}
        }
        $this.Replies.Enqueue(($response | ConvertTo-Json -Depth 8 -Compress))
    }
    [string] Receive() {
        if ($this.Replies.Count -eq 0) { throw 'Fixture response queue exhausted' }
        return $this.Replies.Dequeue()
    }
}

$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ('switcher-cdp-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temporary | Out-Null
$child = Join-Path $temporary 'security-child.ps1'
@'
param($Session, [string]$Expression)
Set-StrictMode -Version Latest
Read-Browser $Session $Expression
'@ | Set-Content -LiteralPath $child -Encoding utf8NoBOM

try {
    # Match the old parent initialization: it cannot make script-scoped state
    # available to the same function when invoked through an & child script.
    $script:requestId = 0
    $first = [FixtureCdpConnection]::new()
    $second = [FixtureCdpConnection]::new()
    if ((Read-Browser @{Connection = $first} 'parent') -ne 'parent') { throw 'Parent CDP result mismatch' }
    foreach ($expression in @('child-first', 'child-second')) {
        $actual = & $child -Session @{Connection = $first} -Expression $expression
        if ($actual -ne $expression) { throw 'Child-script CDP result mismatch' }
    }
    if (($first.SentIds -join ',') -ne '1,2,3') { throw 'CDP request IDs did not persist across script scopes' }
    if ($first.Replies.Count -ne 0) { throw 'CDP did not drain events and unrelated responses before its result' }
    Write-Output 'PASS: production CDP helpers preserve connection IDs through child scripts'

    if ((& $child -Session @{Connection = $second} -Expression 'other-connection') -ne 'other-connection') {
        throw 'Independent connection result mismatch'
    }
    if (($second.SentIds -join ',') -ne '1' -or $first.RequestId -ne 3) {
        throw 'One connection changed another connection request counter'
    }
    Write-Output 'PASS: independent browser connections keep independent request IDs'

    foreach ($kind in @('protocol', 'evaluation')) {
        $first.Failure = $kind
        $caught = $false
        try { & $child -Session @{Connection = $first} -Expression 'must-fail' | Out-Null } catch {
            if (-not $_.Exception.Message.Contains("fixture $kind failure")) { throw }
            $caught = $true
        }
        if (-not $caught) { throw "CDP $kind failure was swallowed" }
        Write-Output "PASS: child-script CDP $kind errors remain failures"
    }
} finally { Remove-Item -LiteralPath $temporary -Recurse -Force }
