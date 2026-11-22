param(
    [string]$ExecutablePath = './target/release/rust-switcher.exe',
    [string]$Version = '',
    [string]$OutputDirectory = './target/installer'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repository = Split-Path -Parent $PSScriptRoot
$ExecutablePath = (Resolve-Path -LiteralPath $ExecutablePath).Path
if (-not $Version) {
    $Version = & python -c "import pathlib,sys,tomllib; print(tomllib.loads(pathlib.Path(sys.argv[1]).read_text())['package']['version'])" (Join-Path $repository 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the package version' }
}
if ($Version -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
    throw 'The installer version must have numeric X.Y.Z form'
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$compiler = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($null -eq $compiler) {
    $compilerPath = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
    if (-not (Test-Path -LiteralPath $compilerPath)) { throw 'Inno Setup 6 is required to build the installer' }
} else { $compilerPath = $compiler.Source }
if ([System.Diagnostics.FileVersionInfo]::GetVersionInfo($compilerPath).FileMajorPart -ne 6) {
    throw 'The native lifecycle declarations require the 32-bit Inno Setup 6 compiler'
}
& $compilerPath '/Qp' "/DAppVersion=$Version" "/DApplicationBinary=$ExecutablePath" "/DInstallerOutputDirectory=$OutputDirectory" (Join-Path $repository 'packaging/windows/rust-switcher.iss')
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed' }
$installer = Join-Path $OutputDirectory 'rust-switcher-setup.exe'
if (-not (Test-Path -LiteralPath $installer)) { throw 'The compiler did not produce the installer' }
& python (Join-Path $repository '.github/scripts/write_installer_manifest.py') --installer $installer --version $Version --output (Join-Path $OutputDirectory 'winget')
if ($LASTEXITCODE -ne 0) { throw 'Could not generate the production WinGet manifests' }
Write-Output "Built $installer for $Version"
