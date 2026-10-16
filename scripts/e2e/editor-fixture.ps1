$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'The editor fixture is restricted to the isolated GitHub-hosted Windows job.'
}
Add-Type -AssemblyName System.Windows.Forms
$form = [System.Windows.Forms.Form]::new()
$form.Text = 'Rust Switcher isolated editor'
$form.Width = 550
$form.Height = 310
Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Windows.Forms;
public static class EditorFixture {
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern IntPtr CreateWindowEx(uint exStyle, string className, string text,
        uint style, int x, int y, int width, int height, IntPtr parent, IntPtr menu,
        IntPtr instance, IntPtr parameter);
}
public sealed class ProbeTrap : NativeWindow {
    private int mode;
    private readonly IntPtr other;
    private readonly string signal;
    private readonly Timer timer = new Timer();
    [DllImport("user32.dll", EntryPoint="SendMessageW")] private static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] private static extern IntPtr SetFocus(IntPtr window);
    public ProbeTrap(IntPtr editor, IntPtr otherEditor, string signalPath) {
        AssignHandle(editor); other = otherEditor; signal = signalPath;
        timer.Interval = 40;
        timer.Tick += delegate {
            timer.Stop();
            if (mode == 1) SendMessage(Handle, 0xB1, IntPtr.Zero, new IntPtr(3));
            if (mode == 2) SetFocus(other);
            System.IO.File.WriteAllText(signal, "changed-" + mode);
            mode = 0;
        };
    }
    protected override void WndProc(ref Message message) {
        if (message.Msg == 0x8011) {
            mode = message.WParam.ToInt32();
            if (mode == 3) {
                System.IO.File.WriteAllText(signal, "blocked");
                System.Threading.Thread.Sleep(3000);
                System.IO.File.WriteAllText(signal, "released");
                mode = 0;
            }
            message.Result = new IntPtr(1);
            return;
        }
        base.WndProc(ref message);
        // Arm after test setup; the application's first selection read starts the race.
        if (message.Msg == 0xD && (mode == 1 || mode == 2) && !timer.Enabled) timer.Start();
    }
}
'@
# Use the OS Edit class: the application explicitly supports Edit/RichEdit and UIA.
$editor = [EditorFixture]::CreateWindowEx(0x200, 'EDIT', '', 0x50010080,
    10, 10, 500, 70, $form.Handle, [IntPtr]1001, [IntPtr]::Zero, [IntPtr]::Zero)
if ($editor -eq [IntPtr]::Zero) { throw 'Native Edit fixture creation failed' }
$other = [EditorFixture]::CreateWindowEx(0x200, 'EDIT', 'untouched',
    0x50010080, 10, 110, 500, 70, $form.Handle, [IntPtr]1002, [IntPtr]::Zero, [IntPtr]::Zero)
if ($other -eq [IntPtr]::Zero) { throw 'Secondary Edit fixture creation failed' }
$trap = [ProbeTrap]::new($editor, $other, $env:RUST_SWITCHER_E2E_SIGNAL_PATH)
[System.Windows.Forms.Clipboard]::SetText('switcher-clipboard-sentinel')
try { [System.Windows.Forms.Application]::Run($form) } finally { $trap.ReleaseHandle(); $form.Dispose() }
