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
