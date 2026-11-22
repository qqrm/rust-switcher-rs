param(
    [string]$ExecutablePath = './target/release/rust-switcher.exe',
    [string]$Version = '',
    [string]$OutputDirectory = './target/installer',
    [string]$CompilerPath = ''
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
if (-not $CompilerPath) {
    $installedCompiler = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
    if (Test-Path -LiteralPath $installedCompiler) { $CompilerPath = $installedCompiler }
    else {
        $compiler = Get-Command ISCC.exe -ErrorAction SilentlyContinue
        if ($null -eq $compiler) { throw 'Inno Setup 6 is required to build the installer' }
        $CompilerPath = $compiler.Source
    }
}
$CompilerPath = (Resolve-Path -LiteralPath $CompilerPath).Path
$compilerMetadata = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($CompilerPath)
Write-Output "Inno compiler: $CompilerPath; FileVersion=$($compilerMetadata.FileVersion); ProductVersion=$($compilerMetadata.ProductVersion)"
# ISCC's executable metadata is independent of the compiler DLL version. The
# script checks the actual compiler's VER with EncodeVer before compiling code.
& $CompilerPath '/Qp' "/DAppVersion=$Version" "/DApplicationBinary=$ExecutablePath" "/DInstallerOutputDirectory=$OutputDirectory" (Join-Path $repository 'packaging/windows/rust-switcher.iss')
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed' }
$installer = Join-Path $OutputDirectory 'rust-switcher-setup.exe'
if (-not (Test-Path -LiteralPath $installer)) { throw 'The compiler did not produce the installer' }
& python (Join-Path $repository '.github/scripts/write_installer_manifest.py') --installer $installer --version $Version --output (Join-Path $OutputDirectory 'winget')
if ($LASTEXITCODE -ne 0) { throw 'Could not generate the production WinGet manifests' }
Write-Output "Built $installer for $Version"
