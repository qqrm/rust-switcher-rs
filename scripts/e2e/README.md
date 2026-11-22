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
| Browser selected text with Double Shift | Release EXE with its default Double Shift binding converts prepopulated two-word input/contenteditable selections both ways through UIA, once per gesture, preserving exact text, selection, focus, other fields and clipboard | Chrome/Edge local fixture only; Firefox and the user's actual web applications remain unverified |
| Browser password fields | Six debug cases require a real UIA `IsPassword` signal without an Edit/RichEdit HWND, complete journal capture, exact default Double Shift/Double Alt conversion both ways, a dynamic type change, a known partial selection, and Double Shift immediately followed by Tab | Release physical password typing remains unverified; debug uses the existing hosted-only injection opt-in |
| Native password journal | Debug captures typed input in a real `ES_PASSWORD` Edit and converts it exactly both ways with default Double Shift; checks the neighboring field, clipboard, immediate Tab ownership, and diagnostic/profile redaction | Windows denies cross-process password text reads; setup and exact boolean comparisons run inside the owned fixture. Physical password typing in release remains unverified |
| Wrong-target test sensitivity | An unmodified executable must cancel a verified source-read/focus race; a temporary combined target-guard mutation must write into the actual destination and fail the specific assertion | Proves that the race assertion catches the combined fault, not that each overlapping guard is independently necessary |
| Journal capacity | Native Edit checks 99/100/101 characters; browser checks 101 with a letter and a leading dot; journal unit tests cover 4096 and overflow | Overflow refuses any conversion that reaches the discarded prefix; a complete last word after a retained boundary can still convert |
| Automatic last-word conversion | Debug runtime toggle followed by a word and space | Other delimiters are not exercised by this GUI scenario |
| Selected text | Release default Double Shift in a separate native Edit, including after restart; converts a prepopulated two-word phrase once per gesture in both directions, preserving selection, focus, the other field and clipboard | Other applications, rich editors and elevated targets are not exercised |
| Clipboard isolation | Clipboard sequence unchanged during selection conversion | Does not audit unrelated applications on the desktop |
| Unicode boundary | A 510-character selection containing emoji must remain complete, followed by a short conversion | Does not prove every UI Automation provider's truncation behavior |
| Startup and relaunch | A second release launch stops the old PID and starts a usable replacement; three rapid launches converge to one new process | Instances without a discoverable window and other Windows sessions are rejected rather than force-killed |
| Tray lifecycle | Delete the real owned tray registration, broadcast TaskbarCreated/resume/display-change messages, and verify Shell_NotifyIconGetRect succeeds again while the main window is hidden | Real Explorer termination, actual sleep/hibernate, and Windows 11 display-driver timing are not exercised |
| Unresponsive predecessor | Suspend the owned release UI thread, launch again, and require old-process termination plus a usable replacement | A process running at a higher integrity level may not grant termination rights |
| Stale updater | A PID-bound shutdown rejects the previous PID; updater recovery does not replace an already running app | Network/download overlap is not injected by the desktop harness |
| Settings and theme | Release dark-theme save, restart and reload | Visual rendering, all settings and hotkey customization are not exercised |
| Autostart | Release registration of the exact executable and removal | Actual logon/reboot is not exercised on the disposable job VM |
| Exit and cleanup | Release and debug Exit, forced predecessor termination, bounded owned-process cleanup | Unexpected crash recovery is not exercised |
| CapsLock layout switching | Two ordinary browser click cases switch layouts twice before journal conversion | Pause and tray menu actions are not exercised by these scenarios |
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

The 53 cases cover five editor types, a single period, words with a leading
period, 0/1/30 ms first-key gaps, repeated fast typing, and transitions between
fields/windows while Ctrl or Alt is released. Two click cases switch the layout
with CapsLock and restore English before typing and Double Alt. Textarea and
contenteditable cases also require complete conversion of 101 characters, both with a leading letter
and a leading period. All cases check exact input and unchanged neighboring/source
fields in both windows. The 51 cases outside immediate-click transitions require
the exact converted output. The two click cases allow the verified conservative
refusal described below as well as exact conversion. Their original input timing
and first-key gaps are unchanged. Original input and Double Alt are never retried. Native focus,
caret-owner HWND, caret rectangle and keyboard-layout snapshots are included in
the JSON results so a journal failure can be distinguished from incorrect input
or browser focus setup. A failure stays red while the remaining cases collect
evidence. Missing browsers or desktop capabilities also fail instead of skipping.

`test-browser-cdp.ps1` checks the production CDP helpers without a browser or
Windows input. It invokes them through a child script under StrictMode, verifies
connection-owned request IDs, and checks event filtering and error propagation.
This runs in the PowerShell CI gate before the interactive scenarios.

#### Conservative refusal after an immediate click

A new focused UIA element can be observed after a raw key snapshot was taken.
The protection contract rejects a snapshot whose element identity changed. Only
the two original textarea/contenteditable `click` cases may accept that refusal,
and only after the original four-second conversion wait has expired normally.
Browser, input, provider, or metadata exceptions remain failures. No additional
readiness wait or guard polling occurs before the first tested key.

Before any recovery action, the original text must remain exactly intact, native
foreground/focus and DOM focus must still identify the original target, and every
other field in both windows must remain unchanged. Read-only debug metadata must
show capture unavailable, zero active/cache characters, and a cumulative physical
append count equal to the count sampled atomically at the last pointer-down
boundary. Selectors `5` (that baseline) and `6` (unavailable as `0`/`1`) supplement
the existing inspection channel. These checks reject partial conversion and
capture followed by clearing. They prove conservative refusal; they do not claim
to establish the precise timing cause of the refusal.

Acceptance also requires two recoveries on the same control. First, select the
existing text and use the normal selection shortcut to produce the exact
original expected conversion. Then collapse the caret at the end of that same
field, send Escape, select the English layout, and immediately type physical
`ghbdtn`. Double Alt must produce the original converted text followed by
`привет`. CDP does not replace text during either recovery. Focus and every other
field are checked before recovery, after selection conversion, and after fresh
journal conversion. Each ordinary result records `converted` or
`safe_focus_refusal`, with the latter including counter evidence and recovery
results; the summary counts both outcomes separately. Password assertions and
the mutation gate do not accept conservative refusal in place of their checks.

`test-browser-outcomes.ps1` exercises the production refusal predicate and value
wait without Windows input. It rejects partial text, capture deltas, retained
history, focus changes, false unavailable state, and missing/null metadata. It
also proves that non-click cases cannot use this outcome and that transport or
timeout exceptions cannot be mistaken for a normal exhausted value wait.

The separate focus unit regressions reproduce a caret-owner change under
unchanged keyboard focus after the first glyph, and assert that changing the
focused control still starts a separate journal. This tests the state transition
directly; browser results establish which transitions occurred on that runner.

### Password conversion and target ownership

`browser-security-cases.ps1` adds five mandatory cases and one provider-dependent
selection check to the browser suite. The
browser starts with `--force-renderer-accessibility`; setup requires its real
Windows UI Automation provider to report the focused HTML input as a password
and verifies that the native HWND is not Edit/RichEdit. Password classification
is a fixture prerequisite, not a reason for the application to refuse input.
All values are synthetic and belong to the launched local fixture.

The first two cases type through native input delivery, require a complete
journal append, and use the default Double Shift or Double Alt gesture to produce
an exact conversion and then the exact original text. They check native/DOM
focus, all neighboring fields in both browser windows, and the clipboard
sequence after each conversion. A third case changes the focused control from
text to password without moving focus, immediately appends more input, and
requires conversion of the complete original-plus-new sequence.

The additional selection check changes only the DOM selection to the first two
characters of an already journaled password. When the real UIA provider exposes
noncollapsed selection endpoints, Double Shift must leave that source intact:
masked or unavailable selection text cannot justify replacing it with the whole
journal. There is no refocus or selection repair after the gesture. If the
installed browser withholds these endpoints, the result is explicitly
`UNCOVERED` and excluded from the passed count. Programmatic caret or selection
changes that the provider does not expose remain a coverage and behavior limit.

The two remaining cases type a password with a trailing space, then send Double
Shift followed by Tab at a requested gap of 0 or 30 ms. The 0 ms scenario sends
all six key events in one `SendInput` batch. The page records trusted Shift
releases, Tab and focus events, their owners, event timestamps and handler times.
The 30 ms case requires both the native measured submission gap and the DOM gap
from the final Shift release to Tab to be at least 25 ms and below 75 ms. The
0 ms case requires a zero native gap and a nonnegative DOM event gap below 75 ms.
This leaves margin before the 100 ms conversion delay. Missing, nonfinite,
out-of-order or late event/submission observations fail the stimulus instead of
being accepted as a successful cancellation. Handler times must be ordered but
may lag while the application blocks in its keyboard hook; delayed processing
does not invalidate an otherwise timely input submission.
The assertion validates that the
prepared source actually received Tab and that the neighboring destination
received focus before evaluating output. The source must remain fully original
or become fully converted; partial replacement fails. The destination and every
other field must remain exactly unchanged, including after a later Double Shift
in the destination. Before that separate later-command check, any selection that
Tab created in the destination is collapsed without changing focus or values;
the original race is already fully observed. The clipboard must also remain unchanged. These cases
observe the command long enough for the deferred shortcut to execute or cancel;
they do not claim all possible operating-system scheduling interleavings.

The native password suite enables `ES_PASSWORD` on the external Windows Edit
and runs the debug application with injected typing allowed only for that owned
fixture process. Exact journal append counts and a collapsed caret establish
that the application received all typed input. Default Double Shift must then
convert the full password in both directions, with unchanged focus, neighbor
and clipboard. It also submits Double Shift and Tab in one input batch; the
fixture handles the actual Tab message to move focus to its second native Edit.
Source and destination are checked after the deferred-command lifetime, and the
application's real diagnostic output and saved profile must exclude input text.

Windows prevents another process from reading an `ES_PASSWORD` Edit with
`WM_GETTEXT` or removing its password style with `EM_SETPASSWORDCHAR`.
The fixture therefore handles password-mode setup and exact value comparisons
inside its own process. Its test protocol returns only a boolean match against
fixed synthetic samples, never the control's text. Every password assertion
requires the native password style to remain set; the app cannot use this
fixture protocol. The unmodified release still runs the ordinary selected-text
checks above and rejects injected journal input. Neither test path claims
physical-password capture in release.

The hosted debug inspection message (`0x83F0`) reports a version marker,
active/cache character counts, and a cumulative journal append count. It exposes
no input text and cannot clear state. The browser checks require exact append
deltas and actual conversions, so a disabled capture path or unbound shortcut
cannot satisfy them by doing nothing. The 53 ordinary browser cases remain part
of the same run. After the debug process exits, its real stdout/stderr and its
isolated application profile are scanned for the distinctive synthetic password
and its conversion. This covers those known diagnostic and persistence surfaces,
not arbitrary files or another application's storage.

`test-browser-outcomes.ps1` checks the production result predicates without
Windows input. It rejects invalid/untrusted Tab stimuli, wrong event owners,
missing or unordered timestamps, original/converted/suffix writes to the wrong
field, partial source edits, and a changed unrelated guard. It also checks that
the diagnostic canary predicate rejects plaintext and converted input and cannot
pass on absent diagnostics. These helper checks supplement actual input tests.

### Wrong-target negative control

`run-target-regression.ps1` first proves ordinary selected-text conversion using
the default Double Shift binding. It then arms the native fixture: the first
actual source `WM_GETTEXT` starts a 40 ms timer that moves focus to the second
Edit. The harness requires the fixture's completion signal and the independently
observed destination HWND before checking that both strings and the clipboard
are unchanged. This makes source acquisition followed by a focus change a
repeatable test condition; no command is retried or repaired.

`run-security-mutation.ps1` first requires that test to pass on the unmodified
executable. It temporarily bypasses the overlapping selection-target,
command-origin, and final-context guards, compiles one separate debug executable,
and restores both source bytes and the original debug executable. The same
native test must then complete its conversion positive control and fail with
`WRONG_TARGET_WRITE` because the actual destination text changed. Compile errors,
setup failures, process crashes, timeouts, and a generic nonzero exit do not count
as detecting the mutation. This deliberately combined fault tests the observed
wrong-target result; it does not attribute protection to any single guard.

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
