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
$editor = [System.Windows.Forms.TextBox]::new()
$editor.AccessibleName = 'Isolated editor'
$editor.Multiline = $true
$editor.SetBounds(10, 10, 500, 100)
$form.Controls.Add($editor)
[System.Windows.Forms.Clipboard]::SetText('switcher-clipboard-sentinel')
try { [System.Windows.Forms.Application]::Run($form) } finally { $form.Dispose() }
