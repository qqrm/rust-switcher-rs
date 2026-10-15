$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'The editor fixture is restricted to the isolated GitHub-hosted Windows job.'
}
Add-Type -AssemblyName System.Windows.Forms
$form = [System.Windows.Forms.Form]::new()
$form.Text = 'Rust Switcher isolated editor'
$form.Width = 550
$form.Height = 200
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class EditorFixture {
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern IntPtr CreateWindowEx(uint exStyle, string className, string text,
        uint style, int x, int y, int width, int height, IntPtr parent, IntPtr menu,
        IntPtr instance, IntPtr parameter);
}
'@
# Use the OS Edit class: the application explicitly supports Edit/RichEdit and UIA.
$editor = [EditorFixture]::CreateWindowEx(0x200, 'EDIT', '', 0x50010080,
    10, 10, 500, 100, $form.Handle, [IntPtr]1001, [IntPtr]::Zero, [IntPtr]::Zero)
if ($editor -eq [IntPtr]::Zero) { throw 'Native Edit fixture creation failed' }
[System.Windows.Forms.Clipboard]::SetText('switcher-clipboard-sentinel')
try { [System.Windows.Forms.Application]::Run($form) } finally { $form.Dispose() }
