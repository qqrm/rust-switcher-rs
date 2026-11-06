$ErrorActionPreference = 'Stop'

# Pin the official Microsoft bootstrap module and client used in CI and publishing.
$version = '1.29.380'
Install-Module Microsoft.WinGet.Client -RequiredVersion $version -Scope CurrentUser -Repository PSGallery -Force
Import-Module Microsoft.WinGet.Client -RequiredVersion $version
Repair-WinGetPackageManager -Version $version -Force

$windowsApps = Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps'
$env:PATH = "$windowsApps;$env:PATH"
if ($env:GITHUB_PATH) {
    $windowsApps | Out-File -FilePath $env:GITHUB_PATH -Encoding utf8 -Append
}
$installed = winget --version
if ($LASTEXITCODE -ne 0 -or $installed.Trim() -ne "v$version") {
    throw "Expected WinGet v$version, got $installed"
}
Write-Output "Using WinGet $installed"
