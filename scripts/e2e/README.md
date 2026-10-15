# Interactive E2E

## GitHub-hosted validation (preferred)

The `windows-full` required CI job builds and runs the debug test application
and the release executable on a fresh standard `windows-2025` GitHub VM.
`run-hosted.ps1` refuses to run outside that explicitly enabled hosted job.
The environment guard prevents accidental local launches; it is not a sandbox.
Do not spoof its variables or run interactive checks on a working desktop.
All compilation, native input and UI Automation stay on the remote runner.

The hosted scenarios cover debug keyboard input, overlapping triple/double
Shift conversion, and release startup, single-instance handoff, saving/loading
settings and Exit. The debug path accepts injected typing through the existing
debug-only switch; this is not evidence of physical keyboard capture in release.
Assertions fail on missing desktop capabilities or timeouts, never skip to green.
Only unique temporary profiles and processes created by the scenario are used.
No local Sandbox, shutdown, driver installation or paid runner is required.
Results are printed in the job log without adding stored workflow artifacts.

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
