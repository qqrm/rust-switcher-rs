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
using System.Text;
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
    private delegate IntPtr KeyboardHook(int code, IntPtr w, IntPtr l);
    private readonly KeyboardHook deliveryCallback;
    private IntPtr deliveryHook;
    [StructLayout(LayoutKind.Sequential)] private struct KeyboardEvent {
        public uint key, scan, flags, time;
        public UIntPtr extra;
    }
    [DllImport("user32.dll", EntryPoint="SetWindowsHookExW", SetLastError=true)]
    private static extern IntPtr SetWindowsHookEx(int kind, KeyboardHook callback, IntPtr module, uint thread);
    [DllImport("user32.dll")] private static extern bool UnhookWindowsHookEx(IntPtr hook);
    [DllImport("user32.dll")] private static extern IntPtr CallNextHookEx(IntPtr hook, int code, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] private static extern IntPtr GetFocus();
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] private static extern IntPtr GetModuleHandle(string name);
    [DllImport("user32.dll", EntryPoint="SendMessageW")] private static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll", EntryPoint="GetWindowTextW", ExactSpelling=true, CharSet=CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr window, StringBuilder text, int capacity);
    [DllImport("user32.dll")] private static extern IntPtr SetFocus(IntPtr window);
    public ProbeTrap(IntPtr editor, IntPtr otherEditor, string signalPath) {
        AssignHandle(editor); other = otherEditor; signal = signalPath;
        deliveryCallback = BeforeReplacementDelivery;
        timer.Interval = 40;
        timer.Tick += delegate {
            timer.Stop();
            int triggeredMode = mode;
            mode = 0;
            if (triggeredMode == 1) SendMessage(Handle, 0xB1, IntPtr.Zero, new IntPtr(3));
            if (triggeredMode == 2) SetFocus(other);
            System.IO.File.WriteAllText(signal, "changed-" + triggeredMode);
        };
    }
    public void ReleaseDeliveryHook() {
        if (deliveryHook != IntPtr.Zero) {
            UnhookWindowsHookEx(deliveryHook);
            deliveryHook = IntPtr.Zero;
        }
    }
    private IntPtr BeforeReplacementDelivery(int code, IntPtr w, IntPtr l) {
        if (code == 0 && mode == 4 && w.ToInt64() == 0x100) {
            var key = (KeyboardEvent)Marshal.PtrToStructure(l, typeof(KeyboardEvent));
            ulong marker = key.extra.ToUInt64();
            // Match the production replacement-batch namespace, not unrelated
            // injected Delete keys or Switcher's modifier-cleanup marker.
            if (key.key == 0x2E && (key.flags & 0x10) != 0 &&
                marker > 0xD5000000UL && marker <= 0xD5FFFFFFUL && GetFocus() == Handle) {
                // This hook is installed after Switcher's. The replacement has
                // already passed its final checks and reached SendInput, but
                // Switcher's delivery filter has not seen the first Delete.
                mode = 0;
                SetFocus(other);
                try {
                    System.IO.File.WriteAllText(signal,
                        GetFocus() == other ? "delivery-4" : "delivery-focus-failed");
                } catch (System.IO.IOException) {
                    // The runner requires the exact signal, so failed fixture
                    // reporting is a setup failure, never a passing regression.
                }
                ReleaseDeliveryHook();
            }
        }
        return CallNextHookEx(deliveryHook, code, w, l);
    }
    protected override void WndProc(ref Message message) {
        // Password setup and the oracle run inside the owning fixture process.
        // Windows denies cross-process EM_SETPASSWORDCHAR/WM_GETTEXT once the
        // native Edit has ES_PASSWORD. This protocol is not used by the app.
        if (message.Msg == 0x8013) {
            SendMessage(Handle, 0xCC, message.WParam == IntPtr.Zero ? IntPtr.Zero : new IntPtr(42), IntPtr.Zero);
            message.Result = new IntPtr(1);
            return;
        }
        if (message.Msg == 0x8014) {
            string expected;
            switch (message.WParam.ToInt32()) {
                case 0: expected = ""; break;
                case 1: expected = "ghbdtn"; break;
                case 2: expected = "\u043f\u0440\u0438\u0432\u0435\u0442"; break;
                case 3: expected = "ghbdtn "; break;
                case 4: expected = "\u043f\u0440\u0438\u0432\u0435\u0442 "; break;
                default: message.Result = new IntPtr(-1); return;
            }
            // Every allowed sample is shorter than this buffer. A truncated
            // value cannot equal a sample. Only an exact boolean match leaves
            // the fixture; password text is never returned through the protocol.
            var actual = new StringBuilder(16);
            int length = GetWindowText(Handle, actual, actual.Capacity);
            message.Result = new IntPtr(length == expected.Length &&
                String.Equals(actual.ToString(), expected, StringComparison.Ordinal) ? 1 : 0);
            return;
        }
        if (message.Msg == 0x87) {
            base.WndProc(ref message);
            // Receive an actual Tab key so this native-only fixture implements
            // the same A-to-B navigation as a normal dialog's tab order.
            message.Result = new IntPtr(message.Result.ToInt64() | 2);
            return;
        }
        if (message.Msg == 0x100 && message.WParam.ToInt32() == 9) {
            SetFocus(other);
            System.IO.File.WriteAllText(signal, "tab");
            message.Result = IntPtr.Zero;
            return;
        }
        if (message.Msg == 0x8011) {
            timer.Stop();
            ReleaseDeliveryHook();
            mode = message.WParam.ToInt32();
            if (mode == 4) {
                deliveryHook = SetWindowsHookEx(13, deliveryCallback, GetModuleHandle(null), 0);
                if (deliveryHook == IntPtr.Zero) {
                    mode = 0;
                    message.Result = IntPtr.Zero;
                    return;
                }
            }
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
try { [System.Windows.Forms.Application]::Run($form) } finally {
    $trap.ReleaseDeliveryHook(); $trap.ReleaseHandle(); $form.Dispose()
}
