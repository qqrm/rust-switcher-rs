$ErrorActionPreference = 'Stop'

# Pin the official Microsoft bootstrap module and client used in CI and publishing.
$version = '1.29.380'
Install-Module Microsoft.WinGet.Client -RequiredVersion $version -Scope CurrentUser -Repository PSGallery -Force
Import-Module Microsoft.WinGet.Client -RequiredVersion $version
Repair-WinGetPackageManager -Version $version -Force

# Do not prepend WindowsApps: its Store aliases shadow the runner's Python.
$package = Get-AppxPackage -Name Microsoft.DesktopAppInstaller
if (-not $package -or -not $package.InstallLocation) {
    throw 'WinGet App Installer package was not registered'
}
$wingetDirectory = $package.InstallLocation
if (-not (Test-Path (Join-Path $wingetDirectory 'winget.exe'))) {
    throw 'Registered WinGet package has no winget.exe'
}
$env:PATH = "$wingetDirectory;$env:PATH"
if ($env:GITHUB_PATH) {
    $wingetDirectory | Out-File -FilePath $env:GITHUB_PATH -Encoding utf8 -Append
}
$installed = winget --version
if ($LASTEXITCODE -ne 0 -or $installed.Trim() -ne "v$version") {
    throw "Expected WinGet v$version, got $installed"
}
Write-Output "Using WinGet $installed"
