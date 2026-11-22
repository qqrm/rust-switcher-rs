# Helpers for the isolated Windows acceptance fixture. Dot-source from the
# acceptance harness; this file does not change certificate/source state on load.

function Assert-TestSourceEnvironment {
    if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
        $env:RUNNER_OS -ne 'Windows' -or -not [Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
        throw 'The trusted source fixture requires the disposable interactive GitHub-hosted Windows runner.'
    }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        if (-not [Security.Principal.WindowsPrincipal]::new($identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
            throw 'The disposable runner must already be elevated for its temporary machine-root test certificate.'
        }
    } finally { $identity.Dispose() }
}
function ConvertTo-CertificatePem([byte[]]$Bytes, [string]$Label) {
    return "-----BEGIN $Label-----`n" + [Convert]::ToBase64String($Bytes, [Base64FormattingOptions]::InsertLineBreaks).Replace("`r`n", "`n") + "`n-----END $Label-----`n"
}
function New-TestSourceCertificate([string]$Directory, [string]$Identifier) {
    # The CA private key exists only in memory. The only persisted private key
    # is a short-lived server leaf valid for localhost/127.0.0.1.
    $caKey = [Security.Cryptography.RSA]::Create(2048)
    $serverKey = [Security.Cryptography.RSA]::Create(2048)
    $ca = $null; $server = $null
    try {
        $notBefore = [DateTimeOffset]::UtcNow.AddMinutes(-5)
        $notAfter = [DateTimeOffset]::UtcNow.AddHours(2)
        $caRequest = [Security.Cryptography.X509Certificates.CertificateRequest]::new("CN=QQRM WinGet E2E $Identifier", $caKey,
            [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)
        $caRequest.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($true, $true, 0, $true))
        $caRequest.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new(
            [Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyCertSign, $true))
        $caRequest.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509SubjectKeyIdentifierExtension]::new($caRequest.PublicKey, $false))
        $ca = $caRequest.CreateSelfSigned($notBefore, $notAfter)
        $request = [Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=localhost', $serverKey,
            [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)
        $request.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($false, $false, 0, $true))
        $request.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new(
            ([Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature -bor [Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyEncipherment), $true))
        $eku = [Security.Cryptography.OidCollection]::new()
        $null = $eku.Add([Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.1'))
        $request.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new($eku, $true))
        $san = [Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new()
        $san.AddDnsName('localhost'); $san.AddIpAddress([Net.IPAddress]::Loopback)
        $request.CertificateExtensions.Add($san.Build())
        $serial = [byte[]]::new(16)
        [Security.Cryptography.RandomNumberGenerator]::Fill($serial)
        $server = $request.Create($ca, $notBefore, $notAfter.AddMinutes(-1), $serial)
        $certificatePath = Join-Path $Directory 'server-cert.pem'
        $keyPath = Join-Path $Directory 'server-key.pem'
        $caPath = Join-Path $Directory 'ca-cert.pem'
        $caDerPath = Join-Path $Directory 'ca-cert.cer'
        $caBytes = $ca.Export([Security.Cryptography.X509Certificates.X509ContentType]::Cert)
        [IO.File]::WriteAllText($certificatePath, (ConvertTo-CertificatePem $server.Export([Security.Cryptography.X509Certificates.X509ContentType]::Cert) 'CERTIFICATE'))
        [IO.File]::WriteAllText($keyPath, (ConvertTo-CertificatePem $serverKey.ExportPkcs8PrivateKey() 'PRIVATE KEY'))
        [IO.File]::WriteAllText($caPath, (ConvertTo-CertificatePem $caBytes 'CERTIFICATE'))
        [IO.File]::WriteAllBytes($caDerPath, $caBytes)
        return @{ CertificatePath = $certificatePath; KeyPath = $keyPath; CaPath = $caPath; CaDerPath = $caDerPath;
            CaBytes = $caBytes; Thumbprint = $ca.Thumbprint; Subject = $ca.Subject; NotAfter = $ca.NotAfter.ToUniversalTime() }
    } finally {
        if ($null -ne $server) { $server.Dispose() }
        if ($null -ne $ca) { $ca.Dispose() }
        $serverKey.Dispose(); $caKey.Dispose()
    }
}
function Invoke-TestSourceCertificateOperation($State, [ValidateSet('Add', 'Remove')][string]$Operation) {
    # Provision exactly one short-lived public CA in this disposable VM's
    # machine root store. CurrentUser Root can prompt even for administrators.
    # Both operations are bounded; no security policy or HTTP.sys binding changes.
    if ($Operation -eq 'Add') {
        $store = [Security.Cryptography.X509Certificates.X509Store]::new('Root', 'LocalMachine')
        try {
            $store.Open([Security.Cryptography.X509Certificates.OpenFlags]::ReadOnly)
            if ($store.Certificates.Find([Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
                $State.Certificate.Thumbprint, $false).Count) { throw 'Fixture certificate unexpectedly already exists.' }
        } finally { $store.Dispose() }
    }
    $scriptText = @'
$ErrorActionPreference = 'Stop'
$expected = [Convert]::FromBase64String('__CA_DER__')
$certificate = [Security.Cryptography.X509Certificates.X509Certificate2]::new($expected)
$store = [Security.Cryptography.X509Certificates.X509Store]::new('Root', 'LocalMachine')
try {
    if ($certificate.Thumbprint -cne '__THUMBPRINT__') { throw 'Fixture CA thumbprint mismatch.' }
    $store.Open([Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
    $existing = $store.Certificates.Find([Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
        $certificate.Thumbprint, $false)
    if ('__OPERATION__' -eq 'Add') {
        if ($existing.Count) { throw 'Fixture CA already exists before import.' }
        $store.Add($certificate)
        $matches = $store.Certificates.Find([Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
            $certificate.Thumbprint, $false)
        if ($matches.Count -ne 1 -or [Convert]::ToBase64String($matches[0].RawData) -cne [Convert]::ToBase64String($expected)) {
            throw 'The exact fixture CA was not imported.'
        }
    } else {
        foreach ($candidate in $existing) {
            try {
                if ([Convert]::ToBase64String($candidate.RawData) -cne [Convert]::ToBase64String($expected)) {
                    throw 'Refusing to remove a certificate whose DER differs from the fixture.'
                }
                $store.Remove($candidate)
            } finally { $candidate.Dispose() }
        }
        if ($store.Certificates.Find([Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
            $certificate.Thumbprint, $false).Count) { throw 'The fixture CA survived removal.' }
    }
    Write-Output ('__OPERATION__ verified for unique fixture CA in LocalMachine Root: ' + $certificate.Thumbprint)
} finally { $store.Dispose(); $certificate.Dispose() }
'@
    $scriptText = $scriptText.Replace('__CA_DER__', [Convert]::ToBase64String($State.Certificate.CaBytes)).Replace('__THUMBPRINT__',
        $State.Certificate.Thumbprint).Replace('__OPERATION__', $Operation)
    if ($Operation -eq 'Add') { $State.CertificateAddAttempted = $true }
    $result = Invoke-Logged (Join-Path $PSHOME 'pwsh.exe') @('-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand',
        [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($scriptText))) ("source-certificate-$($Operation.ToLowerInvariant())") 20 -CaptureDiagnostics
    if ($result.ExitCode -ne 0) { throw "Temporary source CA $Operation failed: $($result.Text)" }
    $State.CertificateAdded = $Operation -eq 'Add'
}
function Read-TestSourceRegistration([string]$Name, [string]$Label, [switch]$AllowAbsent) {
    $result = Invoke-Logged $winget @('source', 'export', '--name', $Name, '--disable-interactivity') $Label 30
    if ($AllowAbsent -and $result.ExitCode -eq -1978335214) { return $null } # 0x8A150012: source name does not exist.
    if ($result.ExitCode -ne 0) { throw "Could not inspect fixture source: $($result.Text)" }
    return $result.Text | ConvertFrom-Json -AsHashtable
}
function Assert-TestSourceRegistration($State, [string]$Label) {
    $registration = Read-TestSourceRegistration $State.Name $Label
    if ($registration.Name -cne $State.Name -or $registration.Type -cne 'Microsoft.Rest' -or
        $registration.Arg.TrimEnd('/') -cne $State.Url.TrimEnd('/') -or $registration.Identifier -cne $State.Identifier -or
        @($registration.TrustLevel) -inotcontains 'trusted' -or -not $registration.Explicit) {
        throw 'WinGet did not retain the exact explicit trusted REST fixture registration.'
    }
    $State.Registration = $registration
    return $registration
}
function Add-TestSourceVersion($State, [string]$Manifest, [string]$Version) {
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid fixture version.' }
    $destination = Join-Path $State.ManifestRoot $Version
    if (Test-Path -LiteralPath $destination) { throw "Fixture version already staged: $Version" }
    # Rename a complete directory into the catalog. A request must never see
    # only part of the three production manifest files.
    $staging = Join-Path $State.Directory ('staging-' + [guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $staging
    try {
        $files = @(Get-ChildItem -LiteralPath $Manifest -Filter '*.yaml' -File)
        if ($files.Count -ne 3) { throw 'Expected three validated production manifest files.' }
        foreach ($file in $files) { Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $staging $file.Name) }
        Move-Item -LiteralPath $staging -Destination $destination
    } finally { if (Test-Path -LiteralPath $staging) { Remove-Item -LiteralPath $staging -Recurse -Force } }
    $State.Versions[$Version] = $Manifest
}
function Invoke-TestSourcePython($State, [string]$ServerScript, [string]$Python, [string[]]$Arguments, [string]$Label) {
    # Match the server's optional private PyYAML directory without changing the
    # parent process environment or the machine Python installation.
    $bootstrap = 'import runpy,sys; extra=sys.argv.pop(1); sys.path.insert(0,extra) if extra else None; sys.argv=sys.argv[1:]; runpy.run_path(sys.argv[0],run_name="__main__")'
    $result = Invoke-Logged $Python (@('-c', $bootstrap, [string]$State.PythonDependencyDirectory, $ServerScript) + $Arguments) $Label 30
    if ($result.ExitCode -ne 0) { throw "Dependency metadata inspection failed: $($result.Text)" }
}
function Get-TestDependencyRegistration($Dependency) {
    $matches = [Collections.Generic.List[object]]::new()
    foreach ($code in @($Dependency.ProductCodes)) {
        if ($code -notmatch '^[A-Za-z0-9_.{}+\-]{1,200}$') { throw 'An exported dependency product code is not a single registry key name.' }
        foreach ($hive in @([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryHive]::CurrentUser)) {
            foreach ($view in @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)) {
                $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $view)
                try {
                    $key = $base.OpenSubKey("Software\Microsoft\Windows\CurrentVersion\Uninstall\$code")
                    if ($null -eq $key) { continue }
                    try {
                        $installedVersion = [string]$key.GetValue('DisplayVersion')
                        if ($installedVersion -ceq $Dependency.PackageVersion) {
                            $matches.Add(@{ Hive = $hive.ToString(); View = $view.ToString(); ProductCode = $code;
                                DisplayVersion = $installedVersion; DisplayName = [string]$key.GetValue('DisplayName') })
                        }
                    } finally { $key.Dispose() }
                } finally { $base.Dispose() }
            }
        }
    }
    if ($matches.Count -eq 0) {
        throw "Dependency prerequisite is not correlated: $($Dependency.PackageIdentifier) $($Dependency.PackageVersion) has no exact exported product-code/version match in the runner's ARP entries."
    }
    return ,$matches.ToArray()
}
function Initialize-TestSourceDependencies($State, [string]$ServerScript, [string]$Python) {
    $planPath = Join-Path $State.Directory 'dependency-plan.json'
    Invoke-TestSourcePython $State $ServerScript $Python @('inspect-dependencies', '--manifest-root', $State.ManifestRoot,
        '--output', $planPath) 'source-dependency-plan'
    $plan = [IO.File]::ReadAllText($planPath) | ConvertFrom-Json -AsHashtable
    if ($plan.PackageIdentifier -cne $packageId -or @($plan.Dependencies).Count -gt 8) { throw 'Invalid production dependency plan.' }
    $State.DependencyPlan = @($plan.Dependencies)
    $null = New-Item -ItemType Directory -Path $State.DependencyRoot
    if ($State.DependencyPlan.Count -eq 0) { return }

    # Use the actual installed version, not today's latest release, so a newer
    # catalog entry does not turn hosted-runner image lag into a fixture failure.
    # The full inventory is private and deleted; retain only declared packages.
    $exportPath = Join-Path $State.Directory 'installed-packages.json'
    try {
        $result = Invoke-Logged $winget @('export', '--output', $exportPath, '--include-versions', '--source', 'winget',
            '--accept-source-agreements', '--disable-interactivity') 'source-dependency-installed-versions'
        if ($result.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $exportPath)) { throw 'Could not export installed dependency versions from the official WinGet source.' }
        if ((Get-Item -LiteralPath $exportPath).Length -gt 4MB) { throw 'Installed package export exceeded the fixture limit.' }
        $inventory = [IO.File]::ReadAllText($exportPath) | ConvertFrom-Json -AsHashtable
        $sources = @($inventory.Sources | Where-Object {
            $_.SourceDetails.Name -ceq 'winget' -and $_.SourceDetails.Identifier -ceq 'Microsoft.Winget.Source_8wekyb3d8bbwe'
        })
        if ($sources.Count -ne 1) { throw 'Installed dependency export did not identify the official WinGet source.' }
        $installedDependencies = @{}
        foreach ($dependency in $State.DependencyPlan) {
            $id = [string]$dependency.PackageIdentifier
            if ($id -notmatch '^[A-Za-z0-9][A-Za-z0-9_.+\-]{1,127}$') { throw 'Invalid production dependency identifier.' }
            $versions = @($sources[0].Packages | Where-Object { $_.PackageIdentifier -ceq $id } |
                ForEach-Object { [string]$_.Version } | Sort-Object -Unique)
            if ($versions.Count -ne 1 -or [string]$versions[0] -notmatch '^\d+(?:\.\d+){1,3}$') {
                throw "The runner must have one catalog-correlated installed version of declared dependency $id."
            }
            $installedDependencies[$id] = [string]$versions[0]
        }
    } finally {
        if (Test-Path -LiteralPath $exportPath) { Remove-Item -LiteralPath $exportPath -Force }
    }

    $index = 0
    foreach ($dependency in $State.DependencyPlan) {
        $index++
        $id = [string]$dependency.PackageIdentifier
        $startedUtc = [DateTime]::UtcNow
        try {
            # Download exports the selected original merged manifest only after
            # the official payload has passed WinGet's SHA256 verification.
            $result = Invoke-Logged $winget @('download', '--source', 'winget', '--id', $id, '--exact',
                '--version', $installedDependencies[$id], '--architecture', 'x64', '--download-directory', $State.DependencyRoot,
                '--accept-package-agreements', '--accept-source-agreements', '--disable-interactivity', '--verbose-logs') "source-dependency-download-$index"
        } finally {
            $traceDirectory = Join-Path $logDirectory "source-dependency-download-$index-winget"
            $null = New-Item -ItemType Directory -Path $traceDirectory
            foreach ($file in @(Get-ChildItem -LiteralPath $wingetLogDirectory -Filter '*.log' -File | Where-Object LastWriteTimeUtc -ge $startedUtc)) {
                Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $traceDirectory $file.Name)
            }
        }
        if ($result.ExitCode -ne 0) { throw "Official dependency download failed for $id`: $($result.Text)" }
    }
    $evidencePath = Join-Path $State.Directory 'dependency-verified.json'
    Invoke-TestSourcePython $State $ServerScript $Python @('inspect-dependencies', '--manifest-root', $State.ManifestRoot,
        '--dependency-root', $State.DependencyRoot, '--output', $evidencePath) 'source-dependency-verify'
    $verified = [IO.File]::ReadAllText($evidencePath) | ConvertFrom-Json -AsHashtable
    if ($verified.PackageIdentifier -cne $packageId -or @($verified.Exports).Count -gt 8) { throw 'Invalid verified dependency evidence.' }
    $retained = [Collections.Generic.List[object]]::new()
    $artifactDirectory = Join-Path $logDirectory 'dependency-manifests'
    $null = New-Item -ItemType Directory -Path $artifactDirectory
    foreach ($dependency in @($verified.Exports)) {
        $id = [string]$dependency.PackageIdentifier
        if ($installedDependencies.ContainsKey($id) -and $dependency.PackageVersion -cne $installedDependencies[$id]) {
            throw 'The official dependency export does not match the requested installed version.'
        }
        $registration = Get-TestDependencyRegistration $dependency
        $traces = @(Get-ChildItem -LiteralPath $logDirectory -Directory -Filter 'source-dependency-download-*-winget' |
            ForEach-Object { Get-ChildItem -LiteralPath $_.FullName -Filter '*.log' -File } | ForEach-Object {
                $text = Read-SharedTextFile $_.FullName
                $selected = $text.IndexOf('Selected installer URL: ' + $dependency.InstallerUrl, [StringComparison]::Ordinal)
                $exported = @([regex]::Matches($text, 'Successfully generated manifest yaml\. Path: ([^\r\n]+)') |
                    Where-Object { $_.Groups[1].Value.Trim().Trim('"').Replace('\\', '\') -ceq $dependency.Manifest })
                if ($text.Contains('Named source requested, found: winget') -and $selected -ge 0 -and
                    $text.IndexOf('Installer hash verified', $selected, [StringComparison]::Ordinal) -gt $selected -and
                    $text -match [regex]::Escape($dependency.InstallerSha256.ToLowerInvariant()) -and
                    $exported.Count -eq 1) { $_.FullName }
            })
        if ($traces.Count -ne 1) { throw "No unique official source/hash/export trace proves dependency $id." }
        $savedManifest = Join-Path $artifactDirectory ($id + '.yaml')
        Copy-Item -LiteralPath $dependency.Manifest -Destination $savedManifest
        $retained.Add(@{ PackageIdentifier = $id; PackageVersion = $dependency.PackageVersion; Source = 'winget';
            InstallerUrl = $dependency.InstallerUrl; InstallerSha256 = $dependency.InstallerSha256;
            DownloadedSha256 = $dependency.DownloadedSha256; DownloadedBytes = $dependency.DownloadedBytes;
            Architecture = $dependency.Architecture; Scope = $dependency.Scope; ProductCodes = $dependency.ProductCodes;
            InstalledRegistration = $registration; Manifest = $savedManifest; Trace = $traces[0] })
    }
    $State.DependencyEvidence = $retained.ToArray()
    @{ Dependencies = $State.DependencyPlan; OfficialExports = $State.DependencyEvidence } |
        ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $logDirectory 'dependency-evidence.json') -Encoding utf8
}
function New-TestSourceState([string]$RootDirectory, [string]$Application) {
    Assert-TestSourceEnvironment
    $identifier = [guid]::NewGuid().ToString('N')
    $directory = Join-Path $RootDirectory "qqrm-winget-source-$identifier"
    if ([IO.Path]::GetFullPath($directory).StartsWith([IO.Path]::GetFullPath($ReportDirectory).TrimEnd([IO.Path]::DirectorySeparatorChar) +
        [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Private TLS material must be outside the report/artifact directory.' }
    if (Test-Path -LiteralPath $directory) { throw 'Fixture source directory unexpectedly exists.' }
    $null = New-Item -ItemType Directory -Path $directory
    # Keep private TLS material outside the artifact/report tree and readable
    # only by this runner identity. The temporary public CA uses LocalMachine Root.
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        $acl = [Security.AccessControl.DirectorySecurity]::new()
        $acl.SetAccessRuleProtection($true, $false); $acl.SetOwner($identity.User)
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($identity.User,
            [Security.AccessControl.FileSystemRights]::FullControl,
            ([Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit),
            [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow))
        [IO.FileSystemAclExtensions]::SetAccessControl([IO.DirectoryInfo]::new($directory), $acl)
        $manifestRoot = Join-Path $directory 'manifests'
        $null = New-Item -ItemType Directory -Path $manifestRoot
    } catch {
        Remove-Item -LiteralPath $directory -Recurse -Force
        throw
    } finally { $identity.Dispose() }
    return @{ Identifier = "qqrm-e2e-$identifier"; Name = "qqrm-e2e-$Application-$identifier";
        Directory = $directory; ManifestRoot = $manifestRoot; Versions = @{}; Certificate = $null; CertificateAdded = $false; CertificateAddAttempted = $false;
        DependencyRoot = (Join-Path $directory 'dependencies'); DependencyPlan = @(); DependencyEvidence = @(); PythonDependencyDirectory = $null;
        Process = $null; ProcessStartTime = $null; SourceAddAttempted = $false; Url = $null; Registration = $null;
        ReadyPath = (Join-Path $directory 'ready.json'); StopPath = (Join-Path $directory 'stop');
        StdoutPath = (Join-Path $logDirectory 'trusted-source.stdout.log'); StderrPath = (Join-Path $logDirectory 'trusted-source.stderr.log');
        StdoutFile = $null; StderrFile = $null; StdoutTask = $null; StderrTask = $null }
}
function Start-TestSource($State, [string]$ServerScript, [string]$Python) {
    Assert-TestSourceEnvironment
    if ($State.Versions.Count -eq 0) { throw 'Stage at least one complete fixture version before starting its source.' }
    if ($null -ne (Read-TestSourceRegistration $State.Name 'source-name-before' -AllowAbsent)) {
        throw 'Refusing to replace a pre-existing WinGet source.'
    }
    $moduleProbe = Invoke-Logged $Python @('-c', 'import yaml; print(yaml.__version__)') 'source-python-yaml' 15
    $dependencyDirectory = $null
    if ($moduleProbe.ExitCode -ne 0 -or $moduleProbe.Text.Trim() -cne '6.0.3') {
        $dependencyDirectory = Join-Path $State.Directory 'python-dependencies'
        $result = Invoke-Logged $Python @('-m', 'pip', 'install', '--disable-pip-version-check', '--no-input', '--target',
            $dependencyDirectory, 'PyYAML==6.0.3') 'source-python-dependency' 90
        if ($result.ExitCode -ne 0) { throw "Could not install the pinned fixture parser: $($result.Text)" }
    }
    $State.PythonDependencyDirectory = $dependencyDirectory
    Initialize-TestSourceDependencies $State $ServerScript $Python
    $State.Certificate = New-TestSourceCertificate $State.Directory $State.Identifier
    Invoke-TestSourceCertificateOperation $State 'Add'
    $info = [Diagnostics.ProcessStartInfo]::new($Python)
    $info.UseShellExecute = $false; $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    if ($dependencyDirectory) { $info.Environment['PYTHONPATH'] = $dependencyDirectory }
    foreach ($argument in @('-u', $ServerScript, '--manifest-root', $State.ManifestRoot, '--dependency-root', $State.DependencyRoot,
        '--bind', '127.0.0.1', '--port', '0',
        '--cert-file', $State.Certificate.CertificatePath, '--key-file', $State.Certificate.KeyPath, '--source-id', $State.Identifier,
        '--ready-file', $State.ReadyPath, '--log-file', (Join-Path $logDirectory 'trusted-source.log'), '--stop-file', $State.StopPath)) {
        $info.ArgumentList.Add($argument)
    }
    $State.StdoutFile = [IO.File]::Open($State.StdoutPath, 'Create', 'Write', 'ReadWrite')
    $State.StderrFile = [IO.File]::Open($State.StderrPath, 'Create', 'Write', 'ReadWrite')
    $State.Process = [Diagnostics.Process]::Start($info)
    $State.ProcessStartTime = $State.Process.StartTime.ToUniversalTime()
    $State.StdoutTask = $State.Process.StandardOutput.BaseStream.CopyToAsync($State.StdoutFile)
    $State.StderrTask = $State.Process.StandardError.BaseStream.CopyToAsync($State.StderrFile)
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while (-not (Test-Path -LiteralPath $State.ReadyPath) -and -not $State.Process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    if ($State.Process.HasExited -or -not (Test-Path -LiteralPath $State.ReadyPath)) {
        throw "REST fixture did not become ready; inspect $($State.StderrPath) and trusted-source.log."
    }
    $ready = [IO.File]::ReadAllText($State.ReadyPath) | ConvertFrom-Json
    $url = [uri]$ready.baseUrl
    if ($ready.pid -ne $State.Process.Id -or $ready.sourceIdentifier -cne $State.Identifier -or
        $ready.packageIdentifier -cne $packageId -or $url.Scheme -ne 'https' -or $url.Host -cne 'localhost' -or
        $url.Port -lt 1 -or $url.AbsolutePath -ne '/' -or $url.Query -or $url.UserInfo) { throw 'REST fixture readiness identity did not match the owned process.' }
    $expectedDependencies = @($State.DependencyEvidence | ForEach-Object { $_.PackageIdentifier } | Sort-Object)
    if (($expectedDependencies -join "`n") -cne (@($ready.dependencyIdentifiers | Sort-Object) -join "`n")) {
        throw 'REST fixture readiness dependencies differ from the verified official exports.'
    }
    $State.Url = $url.AbsoluteUri.TrimEnd('/')
    # This probe uses normal TLS chain/hostname validation and the temporary CA.
    $information = Invoke-RestMethod -Uri ($State.Url + '/information') -NoProxy -TimeoutSec 5
    if ($information.Data.SourceIdentifier -cne $State.Identifier) { throw 'The TLS-validated source identity changed.' }
    $State.SourceAddAttempted = $true
    $startedUtc = [DateTime]::UtcNow
    try {
        $result = Invoke-Logged $winget @('source', 'add', '--name', $State.Name, '--arg', $State.Url,
            '--type', 'Microsoft.Rest', '--trust-level', 'trusted', '--explicit', '--accept-source-agreements', '--disable-interactivity', '--verbose-logs') 'source-add-fixture' 45
    } finally {
        $sourceLogs = Join-Path $logDirectory 'source-add-winget'
        $null = New-Item -ItemType Directory -Path $sourceLogs
        foreach ($file in @(Get-ChildItem -LiteralPath $wingetLogDirectory -Filter '*.log' -File | Where-Object LastWriteTimeUtc -ge $startedUtc)) {
            Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $sourceLogs $file.Name)
        }
    }
    if ($result.ExitCode -ne 0) { throw "Could not register the explicit trusted fixture: $($result.Text)" }
    $null = Assert-TestSourceRegistration $State 'source-registration'
}
function Stop-TestSource($State) {
    if ($null -eq $State) { return }
    $errors = [Collections.Generic.List[string]]::new()
    if ($State.SourceAddAttempted) {
        try {
            $current = Read-TestSourceRegistration $State.Name 'source-before-cleanup' -AllowAbsent
            if ($null -ne $current) {
                if ($current.Name -cne $State.Name -or $current.Type -cne 'Microsoft.Rest' -or
                    $current.Arg.TrimEnd('/') -cne $State.Url.TrimEnd('/') -or $current.Identifier -cne $State.Identifier) {
                    throw 'Refusing to remove a source whose identity differs from this fixture.'
                }
                $result = Invoke-Logged $winget @('source', 'remove', '--name', $State.Name, '--disable-interactivity') 'source-remove-fixture' 30
                if ($result.ExitCode -ne 0) { throw $result.Text }
                if ($null -ne (Read-TestSourceRegistration $State.Name 'source-after-cleanup' -AllowAbsent)) { throw 'The owned source survived removal.' }
            }
        } catch { $errors.Add("Source removal: $_") }
    }
    try {
        if ($null -ne $State.Process) {
            if (-not $State.Process.HasExited) {
                if ($State.Process.StartTime.ToUniversalTime() -ne $State.ProcessStartTime) { throw 'Fixture server process identity changed.' }
                [IO.File]::WriteAllText($State.StopPath, 'stop')
                if (-not $State.Process.WaitForExit(5000)) {
                    $State.Process.Kill($true)
                    if (-not $State.Process.WaitForExit(2000)) { throw 'The owned REST fixture process did not stop.' }
                }
            }
            if ($State.StdoutTask -and $State.StderrTask) {
                $null = [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($State.StdoutTask, $State.StderrTask)).Wait(2000)
            }
        }
    } catch { $errors.Add("Server shutdown: $_") }
    finally {
        if ($State.StdoutFile) { $State.StdoutFile.Dispose() }
        if ($State.StderrFile) { $State.StderrFile.Dispose() }
        if ($State.Process) { $State.Process.Dispose() }
    }
    if ($State.CertificateAddAttempted) {
        try { Invoke-TestSourceCertificateOperation $State 'Remove' }
        catch { $errors.Add("Certificate removal: $_") }
    }
    try {
        if ((Split-Path $State.Directory -Leaf) -cne ('qqrm-winget-source-' + $State.Identifier.Substring('qqrm-e2e-'.Length))) {
            throw 'Refusing to remove a directory whose name differs from the owned fixture.'
        }
        if (Test-Path -LiteralPath $State.Directory) { Remove-Item -LiteralPath $State.Directory -Recurse -Force }
    } catch { $errors.Add("Private fixture file removal: $_") }
    if ($errors.Count) { throw ($errors.ToArray() -join [Environment]::NewLine) }
}
