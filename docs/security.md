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

Unavailable metadata before any input or history in a new segment can purge
retained data without declaring a missing prefix. This observation-only cleanup
keeps the raw generation so a simultaneous rejected hook event still marks that
same segment incomplete. Later input still requires a fresh, explicit
non-password verdict. A sticky history marker survives cache restoration,
conversion extraction, deletion, and trimming, so an empty buffer is not
mistaken for a segment that has never received input.
Confirmed password metadata always takes the strict rejection path, including
an untouched segment; later ordinary metadata cannot reopen that rejected context.

### Immediate focus changes

For pointer-driven focus changes, the UIA worker resolves the release position
once and freezes the nearest explicitly keyboard-focusable control as its
anchor. It accepts metadata only after that exact element becomes focused,
within the existing provider deadline. A different focused field or ancestor
cannot replace the anchor. A raw snapshot records the UIA identity revision
published for its native target before the hook entered. Its later,
fresh password check must report the same identity. Publishing a new identity
cannot reassign older queued keys to that field. Input for an already known,
unchanged ordinary element can survive an overlapping pointer observation;
an unclassified or changed identity cannot authorize it. Native Edit/RichEdit
style checks do not use this UIA identity path.

Identity is published after observing focus, before the remaining password and
stable-focus queries. Publication grants no capture permission: every key still
requires its own fresh, explicit non-password result for that exact revision.
A failed or sensitive metadata result retains the existing rejection path;
published revisions are never rolled back to make older keys current again.
The provider retires the matching revision on sensitive or unavailable metadata,
unstable focus, and expired observations. Retirement does not depend on delivery
of its reply: a rejected or expired observation cannot leave its provisional
identity usable merely because its reply was dropped. Retirement cannot remove
a newer revision or a different target.
The next successful observation must establish a fresh revision, so keys bound
to the retired one remain rejected.

A raw-input reset can supersede a query without changing an already accepted
field. That old query cannot affect the new journal generation. It may retain
its identity only when the observed element matches the accepted owner and its
explicit non-password metadata, UIA focus, native target, and deadline remain
valid. A fully validated ordinary observation may likewise retain its identity
when only reply delivery expires. Every later key still requires its own fresh
validation; neither case reauthorizes older input.

The provider remembers a changed ordinary element for journal ownership only
after clearing that target's retained history under its generation check. If
native focus delivery lags, it evicts the target's cache without clearing another
active editor or expiring that editor's guards. This ordering also covers a
native window switch before the coordinator receives the reply: either the
generation changed first and acceptance fails, or the old prefix was cleared
before it could be cached. Failed, sensitive, and superseded observations do not
replace the last accepted COM identity. UIA calls finish before the journal lock
is acquired, and the accepted COM interface is stored after that lock is released.
If a changed field never completes acceptance, its provisional revision is
retired, including when only a raw reset superseded otherwise valid metadata.
Returning to the old accepted field cannot reuse that unaccepted field's token.

This is best-effort protection based on observable metadata, not proof of the
element that received a physical key. UIA observations can miss a transient
focus change. For example, if an ordinary field B is known, focus moves to a
password field A without any usable observation, and returns to B before the
next query, an unchanged identity cannot reveal where an intervening key went.
Asynchronous hit testing also cannot establish the identity of an element
replaced before the worker resolves that position. Rejecting keys that overlap
a pointer query avoids that ambiguity for those keys but also rejects ordinary
typing after redundant clicks; this implementation does not make that tradeoff.

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

Two password scenarios also record the first ordinary burst used to prepare a
dynamic type change or recover after leaving a password field. They require
either complete capture or the same zero-capture refusal evidence; partial
capture fails. A refused burst stays visible. An explicit new segment in that
same field must then capture every fresh character, and the recovery scenario
must convert that fresh text exactly without changing the neighboring password.
The dynamic type change still follows a genuinely captured ordinary prefix,
with no wait between changing the field to password and sending its first key.
These two scenario preconditions do not expand the ordinary matrix's refusal
whitelist or relax any password assertion.

A separate delayed-focus case keeps password field A focused during a click on
ordinary field B, sends one native key in the same input batch, and then moves
focus to B. Trusted browser events must prove that A received the key before
the actual focus event. The test requires unchanged capture counters and empty
journals through conversion and the deferred-input lifetime, followed by exact
conversion of a new ordinary segment. Invalid timing or ownership is a failed
stimulus, not evidence of successful protection.

`proptest` generates event histories and shrinks failing traces. Five portable
properties compare real journal operations with an independent physical-input
model, insert stale conversion callbacks into a newer context, and ensure
metadata observations cannot reopen an incomplete raw generation. They also
check recovery before any input and reject the extracted-history exception for
both physical and programmatic histories, even without a new raw capture. Windows
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
