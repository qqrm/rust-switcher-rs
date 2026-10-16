# Interactive E2E

## GitHub-hosted validation (preferred)

The `windows-full` required CI job builds and runs the debug test application
and the release executable on a fresh standard `windows-2025` GitHub VM.
`run-hosted.ps1` refuses to run outside that explicitly enabled hosted job.
The environment guard prevents accidental local launches; it is not a sandbox.
Do not spoof its variables or run interactive checks on a working desktop.
All compilation, native input and UI Automation stay on the remote runner.

The hosted scenarios run real executables and inspect real windows. The debug
path accepts injected typing through the existing debug-only switch; this is
not evidence of physical keyboard capture in release. Release selection checks
use a separate native Windows Edit control, without debug switches. Setting
fixture text and its initial selection only prepares the input: actual hotkeys
must cause the application to replace and reselect it.
Assertions fail on missing desktop capabilities or timeouts, never skip to green.
Only unique temporary profiles and processes created by the scenario are used.
No local Sandbox, shutdown, driver installation or paid runner is required.
Results are printed in the job log without adding stored workflow artifacts.

### Coverage manifest

| Feature or boundary | Hosted check | Remaining limit |
| --- | --- | --- |
| Last word and overlapping smart hotkeys | Debug triple Shift and deferred double Shift, both directions | Injected input accepted through the debug-only opt-in |
| Last typed sequence | Debug double Alt converts two words | Physical typing in release requires a separate VM with keyboard access |
| Automatic last-word conversion | Debug runtime toggle followed by a word and space | Other delimiters are not exercised by this GUI scenario |
| Selected text | Release double Shift in a separate native Edit, both directions, restored selection | Other applications, rich editors and elevated targets are not exercised |
| Clipboard isolation | Clipboard sequence unchanged during selection conversion | Does not audit unrelated applications on the desktop |
| Unicode boundary | A 510-character selection containing emoji must remain complete, followed by a short conversion | Does not prove every UI Automation provider's truncation behavior |
| Startup and single instance | Release launch and duplicate-instance rejection | Focus activation by a second launch and a published release download are not exercised |
| Settings and theme | Release dark-theme save, restart and reload | Visual rendering, all settings and hotkey customization are not exercised |
| Autostart | Release registration of the exact executable and removal | Actual logon/reboot is not exercised on the disposable job VM |
| Exit and cleanup | Release and debug Exit, bounded owned-process cleanup | Forced termination and crash recovery are not exercised |
| Tray actions, pause and CapsLock layout switching | No hosted GUI assertion yet | Requires additional scenarios; source/unit checks are not runtime proof |
| Focus and selection changes | Release conversion cancels after the external fixture changes focus or selection, then recovers | Does not make programmatic OS focus changes atomic with input delivery |
| Unresponsive editors | External Edit blocks for three seconds; release UI stays responsive, does not type later and recovers | A permanently hung UIA provider consumes the one bounded probe worker until restart |
| Interrupted config writes | Source audit only | Fault-injection scenarios remain open |
| Operating system and devices | Fresh Windows Server 2025 hosted VM | Windows 11 client, physical keyboard, device drivers and real user profiles remain unverified |

The manifest describes assertions implemented by the harness, not a permanent
claim of success: use the `windows-full` log for the exact tested commit. All
rows without a hosted assertion remain coverage gaps even when CI is green.

## Existing Windows Sandbox scenario

These checks exercise the real Windows keyboard hook, foreground focus, and
`SendInput` behavior in an isolated Windows Sandbox desktop.

They are intentionally separate from normal unit tests because they require an
interactive Windows session. A regular container or non-interactive service
session cannot reliably validate `WH_KEYBOARD_LL`, foreground-window focus, or
global input injection.

## Run

From the repository root:

```powershell
.\scripts\e2e\run-sandbox.ps1
```

The host script:

1. Builds `rust-switcher-debug.exe` with `debug-tracing,debug-binary`.
2. Creates a temporary bundle under `%TEMP%`.
3. Launches Windows Sandbox with that bundle mapped to the sandbox desktop.
4. Runs `sandbox-runner.ps1` inside the sandbox.
5. Waits for the sandbox to shut down and prints `result.json`.

## Scenario Covered

The current scenario validates the overlapping double/triple Shift path in the
real app UI:

1. Start `rust-switcher-debug.exe` inside Windows Sandbox with isolated
   `%APPDATA%`.
2. Enable E2E-only debug flags so the app shows/focuses its own `Playground`
   field and accepts injected input only inside the sandbox run.
3. Type `ghbdtn` into `Playground`.
4. Tap left Shift three times and assert `Playground` becomes `привет`.
5. Tap left Shift two times and assert `Playground` becomes `ghbdtn` after the
   deferred double-Shift timeout.

## Requirements

- Windows 11 host.
- Windows Sandbox feature enabled.
- Rust nightly/MSVC available on the host for the build step.

The sandbox is shut down automatically after the run. Use `-KeepOpen` while
debugging the sandbox desktop. The script does not start the app on the host.
