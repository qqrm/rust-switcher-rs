# Input ownership and password conversion

Rust Switcher can convert recently typed text in password fields as well as
ordinary editors. There is no Advanced Security setting and password metadata
is not permission to capture or convert text. Processing remains local; input
is not written to files, logged, uploaded, or passed through the clipboard.
See [the privacy policy](../PRIVACY.md) for retention and network behavior.

## Where converted text comes from

The keyboard journal records input with its keyboard layout and editing context.
It lets word and sequence conversion work without reading the application's
text back. This includes password input. Journal contents are ordinary process
memory, not encrypted storage or securely erased memory.

Explicit selections use native Edit/RichEdit messages or UI Automation. Windows
denies cross-process `WM_GETTEXT` reads of native `ES_PASSWORD` Edit controls;
these fields use recently typed journal input instead. Some browser password
providers also withhold the text or expose mask characters. When a provider
identifies such a field, the shared selection/last-word shortcut checks selection
endpoints without reading the mask. A collapsed selection can use the typed
journal. A noncollapsed selection without readable source text cancels rather
than replacing an assumed last word. The password hint does not deny capture
or journal conversion. A password that was autofilled or typed before Rust
Switcher started may have no usable journal; the program cannot reconstruct
unobserved text from a mask.

## Commands belong to the original input context

A sequence shortcut such as Double Shift carries the native target and input
generation observed when the shortcut was recognized. Deferred shortcuts and
queued runtime commands keep that origin rather than choosing a new destination
when they eventually run. The active command origin is checked again during
conversion and before replacement input.

Tab, pointer presses and other navigation boundaries invalidate the current
input generation. A command for the preceding context must cancel. Its extracted
text must not be restored into another context or used as a new field's prefix.
This applies to all text, including passwords. A cancelled conversion leaves
ordinary physical navigation available.

Before replacement, the native window/focus and generation must still match.
For captured selections, the exact range and text are revalidated. Input batches
keep deletion, insertion and reselection together. These checks do not require
the target to claim that it is a non-password field.

## Background capture and custom controls

Keyboard hooks do not call foreign UI Automation providers. They take bounded
raw snapshots preserving keyboard state, layout and native target. Background
workers validate native or UIA identity before decoding and inserting text into
the journal. Custom controls do not need to expose a usable `IsPassword` value
for capture. UIA identity observations remain separate from text acquisition.

Queued input keeps its original epoch and field identity. A later observation
cannot reassign an old key to another editor. Missing input, an unavailable
identity, an expired request or a changed target invalidates the incomplete
segment instead of permitting conversion of a suffix as though it were a whole
word. An initial burst immediately after a browser focus change can therefore
still be unavailable for journal conversion; removing password exclusion does
not invent input ownership that was never observed.

Native handles, accessibility observations and input generations are best-effort
information. A completely unreported programmatic focus excursion or caret/range
change inside one native window cannot always be reconstructed. Windows input delivery and focus
changes in another process are not a transaction with the checks in this
process. The application does not claim an operating-system security boundary.

## Temporary data and diagnostics

Raw snapshots, journal text and selection buffers can contain secrets. Queue and
journal capacity are bounded. Navigation and invalidation prevent stale reuse;
clearing a Rust collection is not secure erasure. Cached histories and their
expiry are described in the privacy policy.

Diagnostics retain operation status, lengths and target metadata. Original,
selected, converted and journal text are excluded. The debug acceptance fixture
uses synthetic canaries and aggregate capture counters; it cannot return journal
text. Release builds do not expose that diagnostic endpoint.

## Verification

Tests exercise password conversion and cancellation as separate requirements:
password input must convert in its own field, and an old command must never
spill original or converted text into a different field after Tab. Ordinary
editors provide positive controls. Unit tests cover delayed shortcut execution,
raw/context generations, target identity, incomplete journals and stale restores.
Native/browser acceptance uses the default Double Shift gesture and verifies
exact source text, unchanged neighboring fields and an unchanged clipboard.

Mutation verification deliberately removes target/command ownership checks in a
test build. It must observe the specific wrong-target write assertion; a failed
build, timeout or unrelated harness error does not count. Password capture itself
is expected behavior and is not treated as a security violation.

Hosted `SendInput` events carry `LLKHF_INJECTED`. The diagnostic fixture explicitly
admits them for journal tests; the unmodified release keeps its injected-input
filter. Release acceptance therefore verifies selection conversion and target
ownership using prepared controls, while typed-journal acceptance uses the
instrumented build. Neither result proves physical keyboard capture on every
application or computer. See [the E2E guide](../scripts/e2e/README.md).

## Attribution

The original trace redaction and context-clearing work was adapted from
[Pavel Sherer's contribution](https://github.com/sherer-pro/rust-switcher-rs/commit/9949ae6c23c41da54d51cbefc198e83ae0e789c4).
The current behavior retains data-minimization and input-ownership safeguards
while allowing password conversion.
