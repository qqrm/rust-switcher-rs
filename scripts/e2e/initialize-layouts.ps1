param(
    [ValidateSet('All', 'Pair')]
    [string]$Mode = 'All'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'Input-layout initialization is restricted to the disposable GitHub-hosted Windows job.'
}
if (-not [Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
    throw 'An interactive Windows session is required to initialize the hosted test layouts.'
}

# The International module and the native fixture use the same Windows user as
# cargo and the desktop tests. Never run this profile setup on a user's machine.
if ($PSVersionTable.PSEdition -eq 'Core') {
    $windowsPowerShell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
    & $windowsPowerShell -NoLogo -NoProfile -NonInteractive -STA -File $PSCommandPath -Mode $Mode
    if ($LASTEXITCODE -ne 0) { throw "Windows input-layout initialization failed ($LASTEXITCODE)" }
    return
}

Import-Module International -ErrorAction Stop
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class LayoutSetupNative {
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern IntPtr LoadKeyboardLayout(string name, uint flags);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern IntPtr ActivateKeyboardLayout(IntPtr layout, uint flags);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern bool UnloadKeyboardLayout(IntPtr layout);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern bool GetKeyboardLayoutName(StringBuilder name);
    [DllImport("user32.dll")]
    private static extern int GetKeyboardLayoutList(int count, [Out] IntPtr[] layouts);
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")]
    public static extern IntPtr SetFocus(IntPtr window);
    public static IntPtr[] Loaded() {
        int count = GetKeyboardLayoutList(0, null);
        if (count <= 0) throw new InvalidOperationException("No Windows keyboard layouts were reported");
        var layouts = new IntPtr[count];
        int copied = GetKeyboardLayoutList(layouts.Length, layouts);
        if (copied <= 0) throw new InvalidOperationException("Windows keyboard layout enumeration failed");
        Array.Resize(ref layouts, Math.Min(copied, layouts.Length));
        return layouts;
    }
    public static string CurrentName() {
        var name = new StringBuilder(9);
        if (!GetKeyboardLayoutName(name))
            throw new InvalidOperationException("GetKeyboardLayoutName failed");
        return name.ToString();
    }
}
'@

$definitions = @(
    @{ Tag = 'en-US'; Tips = @('0409:00000409') },
    @{ Tag = 'ru-RU'; Tips = @('0419:00000419') }
)
if ($Mode -eq 'All') {
    $definitions[0].Tips += '0409:00020409'
    $definitions += @(
        @{ Tag = 'uk-UA'; Tips = @('0422:00000422') },
        @{ Tag = 'de-DE'; Tips = @('0407:00000407') },
        @{ Tag = 'fr-FR'; Tips = @('040C:0000040C') },
        @{ Tag = 'pl-PL'; Tips = @('0415:00000415') }
    )
}

$languageList = New-WinUserLanguageList -Language $definitions[0].Tag
$languageList.Clear()
$expectedNames = [System.Collections.Generic.List[string]]::new()
foreach ($definition in $definitions) {
    $entry = (New-WinUserLanguageList -Language $definition.Tag)[0]
    $entry.InputMethodTips.Clear()
    foreach ($tip in $definition.Tips) {
        $entry.InputMethodTips.Add($tip)
        $expectedNames.Add($tip.Split(':')[1])
    }
    $languageList.Add($entry)
}
Set-WinUserLanguageList $languageList -Force -WarningAction SilentlyContinue
Set-WinDefaultInputMethodOverride -InputTip '0409:00000409'

# LoadKeyboardLayout has system-wide effects on current Windows only when the
# calling process owns keyboard focus. The short-lived form establishes that
# precondition and verifies exact variants, not only non-null HKL results.
$script:layoutSetupError = $null
$form = [System.Windows.Forms.Form]::new()
$form.Text = 'Rust Switcher isolated layout setup'
$form.Width = 360
$form.Height = 100
$form.TopMost = $true
$form.ShowInTaskbar = $false
$form.add_Shown({
    try {
        $form.Activate()
        [LayoutSetupNative]::SetForegroundWindow($form.Handle) | Out-Null
        [LayoutSetupNative]::SetFocus($form.Handle) | Out-Null
        if ([LayoutSetupNative]::GetForegroundWindow() -ne $form.Handle) {
            throw 'The layout setup process could not obtain keyboard focus'
        }

        $handles = [System.Collections.Generic.List[IntPtr]]::new()
        foreach ($name in $expectedNames) {
            $handle = [LayoutSetupNative]::LoadKeyboardLayout($name, 1)
            if ($handle -eq [IntPtr]::Zero) { throw "Keyboard layout $name could not be loaded" }
            [LayoutSetupNative]::ActivateKeyboardLayout($handle, 0) | Out-Null
            if ([LayoutSetupNative]::CurrentName() -ne $name) {
                throw "Windows substituted keyboard layout $name with $([LayoutSetupNative]::CurrentName())"
            }
            $handles.Add($handle)
        }

        [LayoutSetupNative]::ActivateKeyboardLayout($handles[0], 0) | Out-Null
        if ([LayoutSetupNative]::CurrentName() -ne '00000409') {
            throw 'English US could not be restored as the initial test layout'
        }
        foreach ($loaded in [LayoutSetupNative]::Loaded()) {
            if (-not $handles.Contains($loaded) -and -not [LayoutSetupNative]::UnloadKeyboardLayout($loaded)) {
                throw ('Unexpected input layout 0x{0:X} could not be unloaded' -f $loaded.ToInt64())
            }
        }
        $visible = [LayoutSetupNative]::Loaded()
        foreach ($handle in $handles) {
            if ($visible -notcontains $handle) {
                throw ('Requested input layout 0x{0:X} is missing from GetKeyboardLayoutList' -f $handle.ToInt64())
            }
        }
        if ($visible.Count -ne $handles.Count) {
            throw "Expected $($handles.Count) layouts, observed $($visible.Count)"
        }
        Write-Host "Configured Windows input layouts ($Mode): $($expectedNames -join ', ')"
    } catch {
        $script:layoutSetupError = $_
    } finally {
        $form.Close()
    }
})
try {
    [System.Windows.Forms.Application]::Run($form)
} finally {
    $form.Dispose()
}
if ($null -ne $script:layoutSetupError) { throw $script:layoutSetupError }
