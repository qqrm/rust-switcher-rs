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
            if ($registered -ne $InstalledExecutable) { throw 'The Inno registration does not own the test executable.' }
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

function Receive-FixtureOutput([hashtable]$Stream) {
    # Bound each pass so a noisy child cannot postpone the process deadline.
    for ($chunkIndex = 0; $chunkIndex -lt 8 -and -not $Stream.Closed -and $Stream.Read.IsCompleted; $chunkIndex++) {
        $count = $Stream.Read.GetAwaiter().GetResult()
        if ($count -eq 0) { $Stream.Closed = $true; break }
        $chunk = [string]::new($Stream.Buffer, 0, $count)
        $Stream.Writer.Write($chunk)
        $Stream.Characters += $count
        Write-Host -NoNewline $chunk
        if ($Stream.Characters -gt 8MB) { throw "Fixture $($Stream.Name) exceeded its diagnostic output limit." }
        $Stream.Read = $Stream.Reader.ReadAsync($Stream.Buffer, 0, $Stream.Buffer.Length)
    }
}

function Invoke-FixtureProcess(
    [string]$FilePath, [string[]]$Arguments, [string]$OutputPrefix,
    [int]$TimeoutSeconds = 240, [int]$DrainMilliseconds = 2000
) {
    $info = [Diagnostics.ProcessStartInfo]::new($FilePath)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $info.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    # Direct Process.Start bypasses PowerShell 7's native-command compatibility
    # handling. Let only this Windows PowerShell child construct its own module
    # paths instead of inheriting incompatible PowerShell 7 Utility modules.
    $null = $info.Environment.Remove('PSModulePath')
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $resultPath = "$OutputPrefix.result.json"
    $record = [ordered]@{
        StartedUtc = [DateTime]::UtcNow.ToString('o')
        Executable = $FilePath
        Arguments = $Arguments
        TimeoutSeconds = $TimeoutSeconds
        ModulePathPolicy = 'Windows PowerShell startup defaults; parent environment unchanged'
        HelperPid = $null
        ExitCode = $null
        TimedOut = $false
        TerminatedOwnedHelper = $false
        Stdout = "$OutputPrefix.stdout.log"
        Stderr = "$OutputPrefix.stderr.log"
        StreamsClosed = $false
        Failure = $null
        CleanupErrors = @()
    }
    $streams = [Collections.Generic.List[hashtable]]::new()
    $cleanupErrors = [Collections.Generic.List[string]]::new()
    $helperProcess = [Diagnostics.Process]::new()
    $helperProcess.StartInfo = $info
    $started = $false
    $failureRecord = $null
    $elapsed = [Diagnostics.Stopwatch]::StartNew()
    try {
        if (-not $helperProcess.Start()) { throw 'The fixture helper process did not start.' }
        $started = $true
        $null = $helperProcess.Handle # Hold this process identity through cleanup.
        $record.HelperPid = $helperProcess.Id
        $helperProcess.StandardInput.Close()
        foreach ($entry in @(
            @{ Name = 'stdout'; Reader = $helperProcess.StandardOutput; Path = $record.Stdout },
            @{ Name = 'stderr'; Reader = $helperProcess.StandardError; Path = $record.Stderr }
        )) {
            $stream = @{
                Name = $entry.Name; Reader = $entry.Reader; Buffer = [char[]]::new(4096)
                Writer = $null; Read = $null; Closed = $false; Characters = 0L
            }
            $streams.Add($stream)
            $stream.Writer = [IO.StreamWriter]::new(
                [IO.File]::Open($entry.Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read),
                [Text.UTF8Encoding]::new($false))
            $stream.Writer.AutoFlush = $true
            $stream.Read = $stream.Reader.ReadAsync($stream.Buffer, 0, $stream.Buffer.Length)
        }
        while (-not $helperProcess.HasExited) {
            foreach ($stream in $streams) { Receive-FixtureOutput $stream }
            if ($elapsed.Elapsed.TotalSeconds -ge $TimeoutSeconds) {
                $record.TimedOut = $true
                throw "Managed updater fixture exceeded ${TimeoutSeconds}s; helper PID $($record.HelperPid); result: $resultPath"
            }
            $null = $helperProcess.WaitForExit(50)
        }
        $record.ExitCode = $helperProcess.ExitCode
        # A failing child can leave inherited pipes open; retain its exit result
        # immediately, before attempting the bounded output drain.
        $record | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding utf8NoBOM
    } catch {
        $failureRecord = $_
        $record.Failure = $_.Exception.ToString()
        # Save the failure before cleanup: a descendant may retain a pipe.
        $record | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding utf8NoBOM
        Write-Host "Managed updater fixture failed: $($record.Failure)"
    } finally {
        if ($started) {
            try {
                if (-not $helperProcess.HasExited) {
                    # Only this held helper is ours to terminate. Never terminate
                    # its installer or the restarted application as a timeout fix.
                    $helperProcess.Kill()
                    $record.TerminatedOwnedHelper = $true
                    if (-not $helperProcess.WaitForExit(5000)) { throw 'The owned helper did not stop within 5 seconds.' }
                }
                if ($helperProcess.HasExited) { $record.ExitCode = $helperProcess.ExitCode }
            } catch { $cleanupErrors.Add($_.Exception.ToString()) }
        }
        $drain = [Diagnostics.Stopwatch]::StartNew()
        try {
            do {
                foreach ($stream in $streams) {
                    if ($null -ne $stream.Read) { Receive-FixtureOutput $stream }
                }
                $record.StreamsClosed = $streams.Count -eq 2 -and @($streams | Where-Object { -not $_.Closed }).Count -eq 0
                if ($record.StreamsClosed -or $drain.ElapsedMilliseconds -ge $DrainMilliseconds) { break }
                Start-Sleep -Milliseconds 20
            } while ($true)
            if ($started -and -not $record.StreamsClosed) { $cleanupErrors.Add('Redirected output remained open after the bounded drain; a descendant may hold a pipe.') }
        } catch { $cleanupErrors.Add($_.Exception.ToString()) }
        foreach ($stream in $streams) {
            try { $stream.Reader.Dispose() } catch { $cleanupErrors.Add($_.Exception.ToString()) }
            if ($null -ne $stream.Writer) {
                try { $stream.Writer.Dispose() } catch { $cleanupErrors.Add($_.Exception.ToString()) }
            }
        }
        $helperProcess.Dispose()
        $elapsed.Stop()
        $record.ElapsedMilliseconds = $elapsed.ElapsedMilliseconds
        $record.CleanupErrors = $cleanupErrors.ToArray()
        $record | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding utf8NoBOM
        Write-Host "Managed updater helper result: $resultPath; exit=$($record.ExitCode); timedOut=$($record.TimedOut)"
    }
    if ($null -ne $failureRecord) { throw $failureRecord }
    if ($cleanupErrors.Count) { throw "Managed updater fixture diagnostics/cleanup failed: $($cleanupErrors -join '; '); result: $resultPath" }
    return [int]$record.ExitCode
}

$driver = Join-Path $testRoot 'invoke-helper.ps1'
@'
#requires -Version 5.1
param([Parameter(Mandatory = $true)][string]$FixturePath)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$fixture = Get-Content -LiteralPath $FixturePath -Raw -Encoding UTF8 | ConvertFrom-Json
$preflight = [ordered]@{
    PSEdition = $PSVersionTable.PSEdition
    PSVersion = $PSVersionTable.PSVersion.ToString()
    HashCommandModule = $null
    HashCommandType = $null
    ActualInstallerHash = $null
    ExpectedInstallerHash = $fixture.InstallerHash
    HashMatches = $false
    Error = $null
}
try {
    if ($PSVersionTable.PSEdition -cne 'Desktop' -or $PSVersionTable.PSVersion.Major -ne 5 -or
        $PSVersionTable.PSVersion.Minor -ne 1) { throw 'The managed helper fixture requires Windows PowerShell Desktop 5.1.' }
    $hashCommand = Get-Command Get-FileHash -CommandType Cmdlet, Function -ErrorAction Stop
    $preflight.HashCommandModule = $hashCommand.Source
    $preflight.HashCommandType = [string]$hashCommand.CommandType
    if ($hashCommand.Source -cne 'Microsoft.PowerShell.Utility') { throw 'Get-FileHash did not resolve to the Windows PowerShell Utility module.' }
    $preflight.ActualInstallerHash = (& $hashCommand -LiteralPath $fixture.InstallerPath -Algorithm SHA256).Hash
    $preflight.HashMatches = $preflight.ActualInstallerHash -ieq $fixture.InstallerHash
    if (-not $preflight.HashMatches) { throw 'The real Windows PowerShell SHA-256 preflight did not match the fixture installer.' }
} catch {
    $preflight.Error = $_.Exception.ToString()
    throw
} finally {
    $preflight | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $fixture.PreflightLog -Encoding UTF8
}
Write-Host "Managed helper preflight: Desktop $($preflight.PSVersion); $($preflight.HashCommandModule); SHA-256 matched."
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
        InstallerHash = $setupHash
        TargetPath = $InstalledExecutable
        CurrentVersion = $originalVersion
        ParentParameter = $parentParameter
        ParentId = $original.Id
        HelperPath = $helperPath
        LogPath = (Join-Path $testRoot "$caseName.log")
        RequestLog = (Join-Path $testRoot "$caseName.requests.log")
        PreflightLog = (Join-Path $testRoot "$caseName.preflight.json")
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
    $code = Invoke-FixtureProcess $windowsPowerShell @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $driver, '-FixturePath', $fixturePath
    ) (Join-Path $testRoot "$caseName.helper")
    $requests = @(Get-Content -LiteralPath $fixture.RequestLog)
    if ($requests.Count -ne 2 -or $requests[0] -cne $apiUrl -or $requests[1] -cne "$downloadRoot/$setupName") {
        throw 'The managed update did not use the expected GitHub API and installer download paths.'
    }
    return $code
}

function Assert-DigestRejection([string]$LogPath) {
    $expectedMessage = if ($Application -eq 'brightwheel') {
        'Update failed: Executable SHA-256 mismatch: brightwheel-setup.exe'
    } else { 'Update failed: Downloaded executable SHA-256 mismatch' }
    if (-not (Test-Path -LiteralPath $LogPath -PathType Leaf) -or
        @([IO.File]::ReadAllLines($LogPath) | Where-Object {
            $_.EndsWith(' ' + $expectedMessage, [StringComparison]::Ordinal)
        }).Count -ne 1) {
        throw "The negative case did not record the expected installer checksum rejection; inspect $LogPath."
    }
}

$badCode = Invoke-HelperFixture 'bad-digest' $true
if ($badCode -eq 0) { throw 'The updater accepted an installer with a mismatching digest.' }
Assert-DigestRejection (Join-Path $testRoot 'bad-digest.log')
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
