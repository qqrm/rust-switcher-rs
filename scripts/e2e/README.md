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
| Last typed sequence | Debug double Alt converts two words and short leading-dot inputs in external native Edit controls | Physical typing in release requires a separate VM with keyboard access |
| First character after focus/modifiers | Debug journal matrix switches between two unowned external windows and two controls, releases Ctrl/Alt in the destination, and asserts exact before/after strings | Synthetic input and native Edit controls do not reproduce every Telegram/Qt or physical-keyboard interaction |
| Browser fields and first-character timing | Debug Double Alt in a real Chrome/Edge process, two windows, input/textarea/contenteditable/shadow DOM/iframe fields, first-key timing and Tab/click navigation | Synthetic input and a local fixture do not prove behavior in the user's exact browser, extensions or web application |
| Browser password fields | Six debug cases require a real UIA `IsPassword` signal without an Edit/RichEdit HWND, zero captured password characters, empty active/cache journals, blocked text hotkeys, same-HWND transitions, and ordinary-field recovery | Local synthetic passwords and the installed Chrome/Edge version; unknown custom controls and OS races are not universal guarantees |
| Password test sensitivity | A temporary source mutation makes UIA report ordinary input; the real browser suite must detect journal capture | This mutation targets the UIA password signal; native password styles and other guard layers have separate tests |
| Journal capacity | Native Edit checks 99/100/101 characters; browser checks 101 with a letter and a leading dot; journal unit tests cover 4096 and overflow | Overflow refuses any conversion that reaches the discarded prefix; a complete last word after a retained boundary can still convert |
| Automatic last-word conversion | Debug runtime toggle followed by a word and space | Other delimiters are not exercised by this GUI scenario |
| Selected text | Release double Shift in a separate native Edit, both directions, restored selection | Other applications, rich editors and elevated targets are not exercised |
| Clipboard isolation | Clipboard sequence unchanged during selection conversion | Does not audit unrelated applications on the desktop |
| Unicode boundary | A 510-character selection containing emoji must remain complete, followed by a short conversion | Does not prove every UI Automation provider's truncation behavior |
| Startup and single instance | Release launch and duplicate-instance rejection | Focus activation by a second launch and a published release download are not exercised |
| Settings and theme | Release dark-theme save, restart and reload | Visual rendering, all settings and hotkey customization are not exercised |
| Autostart | Release registration of the exact executable and removal | Actual logon/reboot is not exercised on the disposable job VM |
| Exit and cleanup | Release and debug Exit, bounded owned-process cleanup | Forced termination and crash recovery are not exercised |
| Pause and CapsLock layout switching | Browser password cases verify runtime pause state changes and two layout switches while text conversion remains blocked | Tray menu actions are not exercised |
| Focus and selection changes | Release conversion cancels after the external fixture changes focus or selection, then recovers | Does not make programmatic OS focus changes atomic with input delivery |
| Unresponsive editors | External Edit blocks for three seconds; release UI stays responsive, does not type later and recovers | A permanently hung UIA provider consumes the one bounded probe worker until restart |
| Interrupted config writes | Windows unit tests inject partial writes and denied replacement; prior bytes and cleanup are checked | Sudden power loss and non-local filesystems are not simulated |
| Operating system and devices | Fresh Windows Server 2025 hosted VM | Windows 11 client, physical keyboard, device drivers and real user profiles remain unverified |

The manifest describes assertions implemented by the harness, not a permanent
claim of success: use the `windows-full` log for the exact tested commit. All
rows without a hosted assertion remain coverage gaps even when CI is green.

### Windows native-input regression workflow

The dedicated **Windows Input Regression** workflow runs the
`native_input_` regression tests on a Windows runner, separately from the larger
GUI suite. The decoding cases compare the production decoder with the real Win32
`ToUnicodeEx` API using a controlled stale modifier snapshot on the first character,
then feed decoded input through the journal. This makes normalization defects
reproducible without asking anyone to type on a working desktop. The group also
verifies the 4096-character capacity,
rejection of truncated prefixes, and recovery after clearing the journal.
A controlled native-API reproduction establishes the tested
decoder behavior; it does not establish that a particular Telegram window
produced that snapshot during physical typing.

### Double Alt journal matrix

`journal-cases.ps1` runs 45 bounded cases through the actual debug executable.
`journal-fixture.ps1` creates two top-level windows in a separate process, each
with two native Windows Edit controls. A debug-only hosted-E2E PID allowlist
admits this one fixture process; release capture behavior is unchanged. The
harness verifies that both windows are unowned and belong to the exact fixture
PID. It does not call `AttachThreadInput` or create cross-process parent/owner
relationships: those relationships implicitly attach input queues and would
change the keyboard state being tested. See Microsoft's
[explanation of cross-process window relationships](https://devblogs.microsoft.com/oldnewthing/20130412-00/?p=4683).

The fixture allocates new Edit HWNDs before destroying the preceding controls,
and the harness rejects a reused HWND. Destination setup uses addressed UIA and
layout requests while unfocused; the first tested glyph is the first keyboard
input delivered after the focus transition. Period is sent as `VK_OEM_PERIOD`,
not ASCII `0x2E` (the Delete virtual key).

Short cases cover `.`, `ghbdtn`, `.ghbdtn`, and two words; window/control changes
repeat twice. Ctrl/Alt cases start with the modifier held in the application's
own playground, then focus the external destination and release the modifier
immediately before typing. A harmless F24 chord establishes source keyboard
state and prevents the release from counting as an additional Alt tap. The
source-focus setup is bounded and may reissue UIA focus before any tested key;
it requires three consecutive native/UIA focus observations and reports actual
HWNDs on failure. Input and conversion are never retried. The
0 ms case submits release and the first key in one `SendInput` batch; 1 and
20 ms cases record the observed elapsed delay. Each modifier/text/delay pair
runs twice. All cases assert exact original text before Double Alt and exact
converted text afterward, plus unchanged source and neighboring editors.

Every result includes actual strings, requested/observed timing, and any error in
the job log and `result.json`. A failed case remains a CI failure while later
cases run to collect useful evidence. The capacity cases require complete
conversion at 99, 100, and 101 characters, including the leading period. These
regressions distinguish the former 100-character truncation defect from
short-input focus or keyboard-state failures.

Three caret cases move Left through real input before Double Alt: into a word,
before the final word, and to the start of the captured text. They check the
native caret before and after conversion, the exact converted prefix, and the
unchanged text after the caret. The unit regressions call the production sequence
extractor and also cover restoration after rejection and mixed-origin input.
Two selection cases use Shift+Left followed by Backspace or replacement typing.
They require Double Alt to leave deleted selections alone and convert only fresh
typing after a selection replacement.

### Browser input matrix

`run-browser.ps1` launches the runner's Chrome (or Edge when Chrome is absent)
with a unique temporary profile and two windows on the repository-owned local
`browser-fixture.html`. It does not open external sites, reuse a browser profile,
or contact a text service. The debug PID allowlist admits only this browser
process. CDP prepares and reads the fixture; all tested text, modifier events,
Tab navigation, physical mouse clicks and Double Alt use native input delivery.

The 51 cases cover five editor types, a single period, words with a leading
period, 0/1/30 ms first-key gaps, repeated fast typing, and transitions between
fields/windows while Ctrl or Alt is released. Textarea and contenteditable cases
also require complete conversion of 101 characters, both with a leading letter
and a leading period. Every case checks exact input
before conversion, exact output afterward, and unchanged neighboring/source
fields in both windows. Input and conversion are never retried. Native focus,
caret-owner HWND, caret rectangle and keyboard-layout snapshots are included in
the JSON results so a journal failure can be distinguished from incorrect input
or browser focus setup. A failure stays red while the remaining cases collect
evidence. Missing browsers or desktop capabilities also fail instead of skipping.

The separate focus unit regressions reproduce a caret-owner change under
unchanged keyboard focus after the first glyph, and assert that changing the
focused control still starts a separate journal. This tests the state transition
directly; browser results establish which transitions occurred on that runner.

### Password input acceptance

`browser-security-cases.ps1` adds exactly six cases to the browser suite. The
browser starts with `--force-renderer-accessibility` so the runner exercises its
real Windows UI Automation provider. Each protected fixture is an HTML password
input: the harness verifies the focused element's `IsPassword` property and that
the focused native HWND is not Edit/RichEdit. This prevents a native-style-only
implementation from accidentally satisfying the browser test.

All tested input and hotkeys use `SendInput`. CDP changes only the owned fixture's
setup and reads its synthetic values. Tests cover normal-to-password clicks,
Shift+Tab under the same native HWND, and changing the currently focused input
from `text` to `password` without changing focus. No wait for the application's
guard is inserted before the first password character. Last word, last sequence,
selected text, their smart variants, and AutoConvert must preserve the complete
password. Pause and CapsLock layout switching must remain functional. Returning
to ordinary contenteditable must journal and convert all of `ghbdtn` to `привет`.

A debug-only, explicitly hosted inspection message (`0x83F0`) returns a version
marker, active/cache character counts, a monotonic count of physical characters
appended to the journal, and the runtime AutoConvert state. It exposes no text
and does not invoke detection or clear state. Comparing the monotonic counter
before and after password entry detects transient recording even if a later
guard clears the buffer. Normal-input controls prove the counter is live. Empty
buffers or an absence of plaintext logs alone are not accepted as proof. The
cache-clearing case first requires nonempty active and cached journals in two
ordinary browser windows, then requires both counts to become zero. Old ordinary
history may take up to one second to clear while the asynchronous verdict arrives;
the capture counter must remain unchanged from the first check after input and
on every delivery/cleanup poll. Transition cases then observe for 2.25 seconds,
longer than the two-second deferred-input lifetime, to detect delayed capture.

The mandatory `run-security-mutation.ps1` gate temporarily makes the production
UIA property helper report `NotSecure`, compiles a separate test executable, and
restores both source bytes and the protected executable before continuing. It
runs the same six browser cases with `-SecurityOnly`. The gate accepts only a
nonzero exit containing the specific journal-capture assertion; build failures,
missing inspection support, provider setup errors and timeouts do not count.
No setting or feature to disable protection is added to release builds.

### Installed layouts and language models

`initialize-layouts.ps1 -Mode All` prepares an exact seven-layout catalog on the
disposable hosted desktop: US English, US International, Russian, Ukrainian,
German, French, and Polish. The script verifies the live catalog while its own
window has focus. It changes the test user's Windows language settings and is
guarded against use on a working desktop. `-Mode Pair` restores EN/RU before
the existing application and browser scenarios.

The mandatory CI step explicitly runs the ignored `native_planner_` integration
tests against that complete catalog. These cover language ranking, ambiguity,
valid-text preservation, full layout identities, repeated conversion, mixed
language runs, and dead-key composition that consumes a space. A separate
guarded test adds and removes Swedish after the provider starts and verifies
that a removed layout cannot remain a target through its cached table. Ordinary
developer test runs do not require changing the user's installed language list.

The public-provider gate also compares the pure keyboard-table interpreter with
the native `ToUnicodeEx` API across all seven layouts: 49 printable key positions
and eight Shift/Caps Lock/AltGr states give 2,744 initial combinations. Every
discovered dead-key stroke is paired with every printable stroke and followed
by neutral probes to check remaining composition state. These guarded oracle
calls run only in the disposable test process; production decoding uses owned
driver tables and never calls the stateful translation API.

The native model-download test calls the production WinHTTP path for a pinned
German model, verifies its size and SHA-256, and then proves that the temporary
cache loads without another download. The other model tests use published model
fixtures locally, including integrity checks for all 75 catalog entries. These
fixtures are development dependencies; release builds bundle only EN/RU models.

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
