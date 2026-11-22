#requires -Version 7.0
# Run the production built-in helper against a locally built installer, with
# only the GitHub transport replaced by a fixture in a separate PowerShell 5.1
# process. Installation, process lifecycle, hashes and registry writes are real.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('brightwheel', 'rust-switcher')]
    [string]$Application,
    [Parameter(Mandatory = $true)][string]$InstalledExecutable,
    [Parameter(Mandatory = $true)][string]$InstallerPath,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,
    [Parameter(Mandatory = $true)][string]$WorkDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$applicationName = if ($Application -eq 'brightwheel') { 'BrightWheel' } else { 'RustSwitcher' }
$repository = if ($Application -eq 'brightwheel') { 'qqrm/brightwheel' } else { 'qqrm/rust-switcher-rs' }
$parentParameter = if ($Application -eq 'brightwheel') { 'ParentProcessId' } else { 'ParentPid' }
$packageId = "QQRM.$applicationName"
$setupName = "$Application-setup.exe"
$exeName = "$Application.exe"
$apiUrl = "https://api.github.com/repos/$repository/releases/latest"
$downloadRoot = "https://github.com/$repository/releases/download/v$Version"
$InstalledExecutable = (Resolve-Path -LiteralPath $InstalledExecutable).Path
$InstallerPath = (Resolve-Path -LiteralPath $InstallerPath).Path
$testRoot = Join-Path $WorkDirectory "managed-updater-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $testRoot | Out-Null
$windowsPowerShell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$sourceHelper = (Resolve-Path (Join-Path $PSScriptRoot '../windows-updater.ps1')).Path

function Get-InstalledVersion {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser,
        [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey("Software\Microsoft\Windows\CurrentVersion\Uninstall\${packageId}_is1")
        if ($null -eq $key) { throw 'The managed-update test requires an installed Inno package.' }
        try {
            $registered = [IO.Path]::GetFullPath((Join-Path ([string]$key.GetValue('InstallLocation')) $exeName))
            if ($registered -ine $InstalledExecutable) { throw 'The Inno registration does not own the test executable.' }
            return [string]$key.GetValue('DisplayVersion')
        } finally { $key.Dispose() }
    } finally { $base.Dispose() }
}

function Get-OwnedProcesses {
    @(Get-Process -Name $Application -ErrorAction SilentlyContinue | Where-Object {
        try { $_.Path -ieq $InstalledExecutable } catch { $false }
    })
}

function Get-PreferenceSnapshot {
    $run = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run')
    try {
        $runValue = if ($null -eq $run) { $null } else {
            $run.GetValue($applicationName, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        }
        $values = [ordered]@{ Run = $runValue }
    } finally { if ($null -ne $run) { $run.Dispose() } }
    if ($Application -eq 'brightwheel') {
        $config = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\BrightWheel')
        try {
            foreach ($name in @('AutoStartConfigured', 'AutoUpdateEnabled')) {
                $values[$name] = if ($null -eq $config) { $null } else { $config.GetValue($name) }
            }
        } finally { if ($null -ne $config) { $config.Dispose() } }
    } else {
        $configPath = Join-Path $env:APPDATA 'RustSwitcher/config.json'
        $values['ConfigurationHash'] = if (Test-Path -LiteralPath $configPath) {
            (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash
        } else { $null }
    }
    return ($values | ConvertTo-Json -Compress)
}

$driver = Join-Path $testRoot 'invoke-helper.ps1'
@'
#requires -Version 5.1
param([Parameter(Mandatory = $true)][string]$FixturePath)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$fixture = Get-Content -LiteralPath $FixturePath -Raw -Encoding UTF8 | ConvertFrom-Json
function Invoke-RestMethod {
    param([string]$Uri, $Headers, [int]$TimeoutSec)
    if ($Uri -cne $fixture.ApiUrl) { throw "Unexpected API request: $Uri" }
    Add-Content -LiteralPath $fixture.RequestLog -Value $Uri
    return $fixture.Release
}
function Invoke-WebRequest {
    param([string]$Uri, $Headers, [string]$OutFile, [switch]$UseBasicParsing, [int]$TimeoutSec)
    if ($Uri -cne $fixture.SetupUrl) { throw "The managed updater requested a raw executable or an unexpected URL: $Uri" }
    Add-Content -LiteralPath $fixture.RequestLog -Value $Uri
    Copy-Item -LiteralPath $fixture.InstallerPath -Destination $OutFile
}
$parameters = @{
    TargetPath = $fixture.TargetPath
    CurrentVersion = $fixture.CurrentVersion
    LogPath = $fixture.LogPath
}
$parameters[$fixture.ParentParameter] = [int]$fixture.ParentId
$global:LASTEXITCODE = 0
& $fixture.HelperPath @parameters
exit $LASTEXITCODE
'@ | Set-Content -LiteralPath $driver -Encoding utf8NoBOM

$processes = @(Get-OwnedProcesses)
if ($processes.Count -ne 1) { throw 'Managed updater E2E requires exactly one running test instance.' }
$original = $processes[0]
$originalVersion = Get-InstalledVersion
if ([version]$Version -le [version]$originalVersion) { throw 'The fixture version must be newer than the installed package.' }
$originalHash = (Get-FileHash -LiteralPath $InstalledExecutable -Algorithm SHA256).Hash
$originalPreferences = Get-PreferenceSnapshot
$setupHash = (Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()

function Invoke-HelperFixture([string]$caseName, [bool]$badDigest) {
    $helperDirectory = Join-Path $testRoot $caseName
    New-Item -ItemType Directory -Path $helperDirectory | Out-Null
    $helperPath = Join-Path $helperDirectory 'updater.ps1'
    Copy-Item -LiteralPath $sourceHelper -Destination $helperPath
    $digest = if ($badDigest) { '0' * 64 } else { $setupHash }
    $fixturePath = Join-Path $testRoot "$caseName.json"
    $fixture = @{
        ApiUrl = $apiUrl
        SetupUrl = "$downloadRoot/$setupName"
        InstallerPath = $InstallerPath
        TargetPath = $InstalledExecutable
        CurrentVersion = $originalVersion
        ParentParameter = $parentParameter
        ParentId = $original.Id
        HelperPath = $helperPath
        LogPath = (Join-Path $testRoot "$caseName.log")
        RequestLog = (Join-Path $testRoot "$caseName.requests.log")
        Release = @{
            tag_name = "v$Version"
            draft = $false
            prerelease = $false
            assets = @(
                @{
                    name = $setupName
                    browser_download_url = "$downloadRoot/$setupName"
                    digest = "sha256:$digest"
                    size = (Get-Item -LiteralPath $InstallerPath).Length
                },
                @{
                    name = $exeName
                    browser_download_url = "$downloadRoot/$exeName"
                    digest = "sha256:$($originalHash.ToLowerInvariant())"
                    size = (Get-Item -LiteralPath $InstalledExecutable).Length
                }
            )
        }
    }
    $fixture | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $fixturePath -Encoding utf8NoBOM
    & $windowsPowerShell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $driver -FixturePath $fixturePath | Out-Host
    $code = $LASTEXITCODE
    $requests = @(Get-Content -LiteralPath $fixture.RequestLog)
    if ($requests.Count -ne 2 -or $requests[0] -cne $apiUrl -or $requests[1] -cne "$downloadRoot/$setupName") {
        throw 'The managed update did not use the expected GitHub API and installer download paths.'
    }
    return $code
}

$badCode = Invoke-HelperFixture 'bad-digest' $true
if ($badCode -eq 0) { throw 'The updater accepted an installer with a mismatching digest.' }
if ($original.HasExited -or (Get-FileHash -LiteralPath $InstalledExecutable -Algorithm SHA256).Hash -cne $originalHash -or
    (Get-InstalledVersion) -cne $originalVersion -or (Get-PreferenceSnapshot) -cne $originalPreferences) {
    throw 'An unverified installer changed the running application, installed version, or preferences.'
}
Write-Host 'PASS: invalid installer digest preserves the original PID, executable, installed version and preferences.'

$updateCode = Invoke-HelperFixture 'verified-installer' $false
if ($updateCode -ne 0) { throw "The production managed updater failed with exit code $updateCode." }
if (-not $original.WaitForExit(20000)) { throw 'The installer did not stop the original process.' }
$deadline = [DateTime]::UtcNow.AddSeconds(20)
do {
    $replacement = @(Get-OwnedProcesses)
    if ($replacement.Count -eq 1 -and $replacement[0].Id -ne $original.Id) { break }
    Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
if ($replacement.Count -ne 1 -or $replacement[0].Id -eq $original.Id) {
    throw 'The managed updater did not leave exactly one replacement process at the installed path.'
}
if ((Get-FileHash -LiteralPath $InstalledExecutable -Algorithm SHA256).Hash -cne $originalHash -or
    (Get-InstalledVersion) -cne $Version -or (Get-PreferenceSnapshot) -cne $originalPreferences) {
    throw 'The managed updater left an incorrect payload, stale installed version, or changed preferences.'
}
Write-Host 'PASS: the production helper ran the verified installer, restarted the app and updated WinGet registration while preserving preferences.'
Write-Host "Transport fixture logs: $testRoot"
