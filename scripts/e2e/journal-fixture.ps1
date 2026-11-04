param(
    [Parameter(Mandatory = $true)][string]$ManifestPath
)

$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or
    $env:RUNNER_OS -ne 'Windows' -or $env:RUST_SWITCHER_HOSTED_E2E -ne '1') {
    throw 'The journal fixture is restricted to the isolated GitHub-hosted Windows job.'
}
Add-Type -AssemblyName System.Windows.Forms
Add-Type -ReferencedAssemblies System.Windows.Forms -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Forms;

public sealed class JournalFixture : Form {
    private readonly Form second = new Form();
    private readonly IntPtr[] editors = new IntPtr[4];
    private readonly string manifest;
    private int generation;

    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern IntPtr CreateWindowEx(uint exStyle, string className, string text,
        uint style, int x, int y, int width, int height, IntPtr parent, IntPtr menu,
        IntPtr instance, IntPtr parameter);
    [DllImport("user32.dll", SetLastError=true)] private static extern bool DestroyWindow(IntPtr window);
    public JournalFixture(string manifestPath) {
        manifest = manifestPath;
        Text = "Rust Switcher journal fixture A";
        second.Text = "Rust Switcher journal fixture B";
        Width = second.Width = 550;
        Height = second.Height = 300;
        StartPosition = second.StartPosition = FormStartPosition.Manual;
        Left = 40; Top = 70; second.Left = 230; second.Top = 240;
        // Leave both windows unowned: cross-thread owner/owned relationships
        // implicitly attach input queues and would change the state under test.
        Shown += delegate {
            second.Show();
            ResetEditors();
        };
        FormClosed += delegate { second.Close(); };
    }

    private void ResetEditors() {
        for (int i = 0; i < editors.Length; i++) {
            IntPtr parent = i < 2 ? Handle : second.Handle;
            // Allocate before destroying the previous control so the identity
            // cannot be reused for this case. The harness also checks history.
            IntPtr replacement = CreateWindowEx(0x200, "EDIT", "untouched", 0x50010080,
                10, 10 + (i % 2) * 100, 500, 70, parent, new IntPtr(1001 + i % 2),
                IntPtr.Zero, IntPtr.Zero);
            if (replacement == IntPtr.Zero)
                throw new Win32Exception(Marshal.GetLastWin32Error(), "Fixture editor creation failed.");
            if (editors[i] != IntPtr.Zero && !DestroyWindow(editors[i]))
                throw new Win32Exception(Marshal.GetLastWin32Error(), "Fixture editor destruction failed.");
            editors[i] = replacement;
        }
        generation++;
        string temporary = manifest + ".tmp";
        File.WriteAllText(temporary, "{\"generation\":" + generation +
            ",\"a\":" + Handle.ToInt64() + ",\"b\":" + second.Handle.ToInt64() +
            ",\"a1\":" + editors[0].ToInt64() + ",\"a2\":" + editors[1].ToInt64() +
            ",\"b1\":" + editors[2].ToInt64() + ",\"b2\":" + editors[3].ToInt64() + "}");
        if (File.Exists(manifest)) File.Replace(temporary, manifest, null);
        else File.Move(temporary, manifest);
    }

    protected override void WndProc(ref Message message) {
        if (message.Msg == 0x8012) {
            ResetEditors();
            message.Result = new IntPtr(generation);
            return;
        }
        base.WndProc(ref message);
    }

    protected override void Dispose(bool disposing) {
        if (disposing) second.Dispose();
        base.Dispose(disposing);
    }
}
'@
[System.Windows.Forms.Application]::SetUnhandledExceptionMode([System.Windows.Forms.UnhandledExceptionMode]::ThrowException)
$fixture = [JournalFixture]::new($ManifestPath)
try { [System.Windows.Forms.Application]::Run($fixture) } finally { $fixture.Dispose() }
