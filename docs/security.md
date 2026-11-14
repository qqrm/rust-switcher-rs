# Password-field protection

Rust Switcher checks password metadata before adding keyboard input to its
journal and before reading or replacing text. This does not change the updater
or verified language-model downloads described in [the privacy policy](../PRIVACY.md).

## Detection and behavior

- Standard Windows Edit/RichEdit controls are checked for `ES_PASSWORD`.
- Other controls use the focused UI Automation element's `IsPassword` property.
  The property is requested without substituting a default value: unsupported,
  malformed, unavailable, and timed-out answers remain unknown.
- Password and unknown results block capture and all text-conversion commands.
  Physical typing, the AutoConvert pause toggle, and plain layout switching remain
  available. A layout shortcut shared with conversion only switches the layout
  when conversion is blocked.
- Rejection clears the active journal, all cached contexts, and pending decoder
  composition. A security generation prevents queued work or an extracted-text
  restore guard from reviving data after that invalidation.
- Selection checks run before requesting a control's text. The target is checked
  again after the final password query and immediately before replacement input.

The appearance of a control does not determine support. A styled browser password
input can expose the correct property; a custom control with no usable metadata
cannot be authorized for capture. A provider that explicitly mislabels secret
content as ordinary text cannot be corrected by this mechanism.

Journal reset reasons are explicit. Navigation clears the active segment and
advances its guards while retaining unrelated editor histories. Sensitive input
purges all retained histories. Rejected capture also marks the active segment
incomplete, so later keys cannot make a missing prefix convertible. A pointer
release only requests fresh metadata; it does not reset any of those states.
An observation that retains the raw-input epoch also retains its incomplete
marker, even when the UIA descendant changes. Only a new input boundary can
start a complete segment after capture has lost a key.

### Immediate focus changes

For pointer-driven focus changes, the UIA worker resolves the release position
once and freezes the nearest explicitly keyboard-focusable control as its
anchor. It accepts metadata only after that exact element becomes focused,
within the existing provider deadline. A different focused field or ancestor
cannot replace the anchor. Capture still rejects a changed element or unknown
password metadata. Hit testing is asynchronous; it does not establish the
identity of an element replaced before the worker resolves that position.

A click can move between browser fields before their asynchronous accessibility
notification reaches Rust Switcher. If the first queued key observes a different
UIA element, the guard cannot establish which field owned that key when it was
pressed. It discards the incomplete input history even when the newly focused
field currently reports `IsPassword = false`. This can leave an ordinary burst
typed immediately after a click unavailable for last-word or sequence conversion.
The physical input still reaches the application, and the conversion leaves it
unchanged.

Once the ordinary field has been identified, explicitly selecting the visible
text allows the selection-conversion path to validate and convert that text
without recovering rejected keystrokes. A new input segment after an explicit
navigation/reset boundary can also be captured normally. Repeating the sequence
shortcut alone does not reconstruct the discarded history. This conservative
behavior is a compatibility tradeoff, not a guarantee that every first burst
after a focus change can be converted.

## Responsiveness and temporary data

The keyboard hook does not call UI Automation or wait for its provider. It takes
a raw keyboard snapshot and submits bounded work. The background capture queue
preserves event order and the keyboard state/layout at the time of each event.
A separate, persistent COM MTA worker reads only focus and password metadata.
It never receives raw key events or text.

Queue overflow, expired work, changed focus, or a stalled provider discards
capture rather than accepting a partial word. A conversion waits for preceding
capture work within a bounded deadline; it is cancelled if that work cannot be
validated. Providers cannot cause unbounded worker creation or queue growth.

Raw snapshots are temporary sensitive data even though they have not been
decoded into text. Neither snapshots nor journal contents are persisted or
logged. Clearing a Rust collection is not secure erasure. This protection is not
an operating-system security boundary, and it cannot eliminate all races with
another application's focus changes or Windows input delivery.

## Verification

Unit tests cover password metadata, unsupported properties, bounded workers,
stale generations, journal/cache clearing, and restoration guards. Windows
acceptance tests exercise real browser controls through physical-style injected
test input on disposable GitHub-hosted runners, including password transitions
within one native browser window and positive ordinary-text conversions.
Stable-field cases require exact conversion. The two immediate-click cases also
permit a verified refusal: unchanged text and focus, untouched other fields,
empty active and cached journals, an unavailable-prefix marker, and no physical
character appended since the pointer-down boundary. A refusal must then recover
through selection conversion and a fresh input segment in the same control.
These cases do not insert a readiness wait before the tested first key.

`proptest` generates event histories and shrinks failing traces. Three portable
properties compare real journal operations with an independent physical-input
model, insert stale conversion callbacks into a newer context, and ensure
metadata observations cannot reopen an incomplete raw generation. Windows
properties cover queued keys across three native owners and require arbitrary
pointer releases/moves to preserve every retained prefix and guard. The models
include input delivered to an editor but lost by capture; a captured suffix is
never accepted as the complete physical sequence.

The debug fixture exposes only aggregate counters, including a cumulative count
of characters actually appended from physical input. That counter detects
transient capture even if a later check clears the journal. Its message endpoint
requires the existing hosted-fixture opt-ins and target process; it is absent
from release builds and cannot return text.

A mutation check temporarily disables the UI Automation password classification
in a test build and requires the browser capture assertion to fail. A compilation
error or unrelated browser failure does not count as detecting that mutation.
See [the E2E guide](../scripts/e2e/README.md) for execution details.

## Attribution

Password-field exclusion, clearing retained input, and removing text from tracing
were adapted from Pavel Sherer's work in
[`sherer-pro/rust-switcher-rs`, commit `9949ae6`](https://github.com/sherer-pro/rust-switcher-rs/commit/9949ae6c23c41da54d51cbefc198e83ae0e789c4).
The upstream adaptation adds bounded background validation, generation checks,
and browser acceptance tests while retaining application updates and model
downloads. The adapting commit also credits Pavel with a `Co-authored-by` trailer.
