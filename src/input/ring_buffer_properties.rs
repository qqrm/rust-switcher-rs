//! Generated histories exercise the production journal with an independent
//! editor model. The model records physical input even when capture loses it;
//! a later known suffix must never become a complete conversion candidate.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use proptest::prelude::*;

use super::*;

fn glyph() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['.', 'g', 'h', 'b', 'd', 't', 'n', 'я', 'é', '😀'])
}

fn append_physical(text: &str) {
    with_journal_mut(|journal| {
        journal.push_text_internal(text, LayoutTag::En, RunOrigin::Physical);
    });
}

fn text_of(runs: impl IntoIterator<Item = InputRun>) -> String {
    runs.into_iter().map(|run| run.text).collect()
}

fn observe_unavailable_for_generation(
    generation: u64,
    pending_capture: impl FnOnce(u64) -> bool,
) -> Option<bool> {
    with_journal_mut(|journal| {
        reject_observation_in_journal(journal, generation, pending_capture, true)
    })
}

#[derive(Clone, Debug)]
enum Event {
    Type(char),
    Lose(char),
    Navigate,
    Remember,
    RejectSaved { index: usize, context: bool },
    ProbeStale(usize),
    ExtractAndRestore,
}

fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        5 => glyph().prop_map(Event::Type),
        1 => glyph().prop_map(Event::Lose),
        2 => Just(Event::Navigate),
        2 => Just(Event::Remember),
        2 => (0_usize..64, any::<bool>())
            .prop_map(|(index, context)| Event::RejectSaved { index, context }),
        2 => (0_usize..64).prop_map(Event::ProbeStale),
        3 => Just(Event::ExtractAndRestore),
    ]
}

#[derive(Clone, Copy)]
struct SavedToken {
    raw: u64,
    context: u64,
    document_revision: usize,
}

fn token(document_revision: usize) -> SavedToken {
    SavedToken {
        raw: secure_input_generation(),
        context: secure_context_generation(),
        document_revision,
    }
}

enum Segment {
    Complete(String),
    MissingInput(String),
}

impl Segment {
    fn type_key(&mut self, character: char) {
        match self {
            Self::Complete(text) | Self::MissingInput(text) => text.push(character),
        }
    }

    fn lose_capture(&mut self) {
        if let Self::Complete(text) = self {
            *self = Self::MissingInput(std::mem::take(text));
        }
    }

    fn retained_text(&self) -> &str {
        match self {
            Self::Complete(text) => text,
            Self::MissingInput(_) => "",
        }
    }

    fn convertible_text(&self) -> Option<&str> {
        match self {
            Self::Complete(text) if !text.is_empty() => Some(text),
            _ => None,
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]

    #[test]
    fn generated_history_never_converts_a_suffix_after_losing_input(
        events in prop::collection::vec(event(), 1..65),
    ) {
        let _guard = test_guard();
        clear_secure_input_data();
        let mut document_revision = 0;
        let mut segment = Segment::Complete(String::new());
        let mut saved = vec![token(document_revision)];

        for event in events {
            match event {
                Event::Type(character) => {
                    segment.type_key(character);
                    append_physical(&character.to_string());
                }
                Event::Lose(character) => {
                    // The editor received this key; the capture path did not.
                    segment.type_key(character);
                    segment.lose_capture();
                    reject_secure_capture(secure_input_generation());
                    document_revision += 1;
                }
                Event::Navigate => {
                    let previous = token(document_revision);
                    invalidate_capture_context();
                    prop_assert!(!validate_secure_input_generation(previous.raw));
                    prop_assert!(!validate_secure_context_generation(previous.context));
                    segment = Segment::Complete(String::new());
                    document_revision += 1;
                }
                Event::Remember => saved.push(token(document_revision)),
                Event::RejectSaved { index, context } => {
                    let previous = saved[index % saved.len()];
                    if context {
                        reject_secure_context(previous.context);
                    } else {
                        reject_secure_capture(previous.raw);
                    }
                    if previous.document_revision == document_revision {
                        segment.lose_capture();
                        document_revision += 1;
                    }
                }
                Event::ProbeStale(index) => {
                    let previous = saved[index % saved.len()];
                    if previous.document_revision != document_revision {
                        let current = token(document_revision);
                        let before = runs_snapshot();
                        prop_assert!(take_last_sequence_with_suffix_if_generation(previous.context).is_none());
                        prop_assert!(take_last_layout_run_with_suffix_if_generation(previous.context).is_none());
                        prop_assert!(!invalidate_capture_context_if_current(previous.context));
                        prop_assert!(!mark_last_token_autoconverted_if_generation(previous.context));
                        reject_secure_capture(previous.raw);
                        reject_secure_context(previous.context);
                        prop_assert!(validate_secure_input_generation(current.raw));
                        prop_assert!(validate_secure_context_generation(current.context));
                        prop_assert_eq!(runs_snapshot(), before);
                    }
                }
                Event::ExtractAndRestore => {
                    let generation = secure_context_generation();
                    let extracted = take_last_sequence_with_suffix_if_generation(generation);
                    match (segment.convertible_text(), extracted) {
                        (Some(expected), Some((sequence, suffix))) => {
                            let extracted: Vec<_> = sequence.into_iter().chain(suffix).collect();
                            prop_assert_eq!(text_of(extracted.clone()), expected);
                            prop_assert!(push_runs_if_generation(generation, extracted));
                        }
                        (None, None) => {}
                        (expected, actual) => prop_assert!(false, "expected {expected:?}, extracted {actual:?}"),
                    }
                }
            }
            prop_assert_eq!(text_of(runs_snapshot()), segment.retained_text());
        }
        clear_secure_input_data();
    }

    #[test]
    fn stale_conversion_callbacks_cannot_change_a_new_segment(
        prefix in prop::collection::vec(glyph(), 1..25),
        suffix in prop::collection::vec(glyph(), 0..25),
        reject_new_segment in any::<bool>(),
        callbacks in prop::collection::vec(0_u8..7, 1..33),
    ) {
        let _guard = test_guard();
        clear_secure_input_data();
        append_physical(&prefix.into_iter().collect::<String>());
        let old = token(0);
        let (sequence, trailing) = take_last_sequence_with_suffix_if_generation(old.context)
            .expect("the complete old segment must be extractable");
        let extracted: Vec<_> = sequence.into_iter().chain(trailing).collect();

        invalidate_capture_context();
        if reject_new_segment {
            reject_secure_capture(secure_input_generation());
        }
        let suffix: String = suffix.into_iter().collect();
        append_physical(&suffix);
        let current = token(1);
        let before = runs_snapshot();
        prop_assert_eq!(text_of(before.clone()), if reject_new_segment { "" } else { &suffix });

        // Inserting or duplicating any delayed callback from the previous
        // conversion must be observationally equivalent to omitting it.
        for callback in callbacks {
            match callback {
                0 => reject_secure_capture(old.raw),
                1 => reject_secure_context(old.context),
                2 => prop_assert!(!push_runs_if_generation(old.context, extracted.clone())),
                3 => prop_assert!(take_last_sequence_with_suffix_if_generation(old.context).is_none()),
                4 => prop_assert!(take_last_layout_run_with_suffix_if_generation(old.context).is_none()),
                5 => prop_assert!(!invalidate_capture_context_if_current(old.context)),
                _ => prop_assert!(!mark_last_token_autoconverted_if_generation(old.context)),
            }
            prop_assert_eq!(runs_snapshot(), before.clone());
            prop_assert!(validate_secure_input_generation(current.raw));
            prop_assert!(validate_secure_context_generation(current.context));
        }
        clear_secure_input_data();
    }

    #[test]
    fn descendant_refresh_cannot_reopen_an_incomplete_raw_generation(
        prefix in prop::collection::vec(glyph(), 0..17),
        lost in glyph(),
        suffix in prop::collection::vec((glyph(), 0_u8..3), 1..17),
    ) {
        let _guard = test_guard();
        clear_secure_input_data();
        let prefix: String = prefix.into_iter().collect();
        append_physical(&prefix);
        let mut physical = Segment::Complete(prefix);
        // The editor receives a key that never reaches the metadata worker.
        physical.type_key(lost);
        physical.lose_capture();
        let before_loss = token(0);
        reject_secure_capture(before_loss.raw);
        let rejected = token(1);

        // A later pointer-release refresh sees a changed, non-password UIA
        // descendant. Exercise the worker's accepted-verdict journal path:
        // fresh metadata cannot supply the physical key missing from this epoch.
        discard_changed_capture_context(rejected.raw);
        prop_assert!(validate_secure_input_generation(rejected.raw));
        prop_assert!(!validate_secure_context_generation(rejected.context));

        for &(character, refreshes) in &suffix {
            physical.type_key(character);
            append_physical(&character.to_string());
            for _ in 0..refreshes {
                let previous_context = secure_context_generation();
                discard_changed_capture_context(rejected.raw);
                prop_assert!(validate_secure_input_generation(rejected.raw));
                prop_assert!(!validate_secure_context_generation(previous_context));
            }
            prop_assert_eq!(text_of(runs_snapshot()), physical.retained_text());
            let current = secure_context_generation();
            prop_assert!(take_last_sequence_with_suffix_if_generation(current).is_none());
            prop_assert!(take_last_layout_run_with_suffix_if_generation(current).is_none());
        }

        // A real navigation starts a complete segment. Fresh metadata remains
        // usable, and a delayed observation from the rejected epoch is inert.
        let rejected_context = secure_context_generation();
        invalidate_capture_context();
        prop_assert!(!validate_secure_input_generation(rejected.raw));
        prop_assert!(!validate_secure_context_generation(rejected_context));
        let fresh = token(2);
        discard_changed_capture_context(fresh.raw);
        prop_assert!(validate_secure_input_generation(fresh.raw));
        prop_assert!(!validate_secure_context_generation(fresh.context));
        let fresh_context = secure_context_generation();
        let expected: String = suffix.into_iter().map(|(character, _)| character).collect();
        append_physical(&expected);
        discard_changed_capture_context(rejected.raw);
        prop_assert!(validate_secure_input_generation(fresh.raw));
        prop_assert!(validate_secure_context_generation(fresh_context));
        let (sequence, trailing) = take_last_sequence_with_suffix_if_generation(fresh_context)
            .expect("navigation must restore a complete, convertible segment");
        prop_assert_eq!(text_of(sequence.into_iter().chain(trailing)), expected);
        clear_secure_input_data();
    }

    #[test]
    fn empty_observations_preserve_future_input_but_never_a_missing_prefix(
        text in prop::collection::vec(glyph(), 1..17),
        observations in 1_usize..9,
        queued_key in any::<bool>(),
        lost_key in any::<bool>(),
    ) {
        let _guard = test_guard();
        clear_secure_input_data();
        let text: String = text.into_iter().collect();
        let initial = token(0);

        // Inserting unavailable observations before any physical input must
        // preserve the first subsequent key, even after empty conversion probes.
        for _ in 0..observations {
            let context = secure_context_generation();
            prop_assert!(take_last_sequence_with_suffix_if_generation(context).is_none());
            prop_assert!(take_last_layout_run_with_suffix_if_generation(context).is_none());
            prop_assert_eq!(observe_unavailable_for_generation(initial.raw, |_| false), Some(true));
            prop_assert!(validate_secure_input_generation(initial.raw));
            prop_assert!(!validate_secure_context_generation(context));
        }

        // The editor can have received a prefix which is still queued, or a
        // key lost before queue admission. Neither permits suffix conversion.
        let pending_epoch = secure_input_generation();
        if lost_key {
            reject_secure_capture(pending_epoch);
        }
        let before = token(1);
        let cleared = observe_unavailable_for_generation(before.raw, |epoch| {
            queued_key && epoch == pending_epoch
        });
        let complete = !queued_key && !lost_key;
        prop_assert_eq!(cleared, Some(complete));
        prop_assert_eq!(validate_secure_input_generation(before.raw), complete);
        prop_assert!(!validate_secure_context_generation(before.context));
        if !complete {
            for _ in 0..observations {
                let current = secure_input_generation();
                prop_assert_eq!(observe_unavailable_for_generation(current, |_| false), Some(false));
                prop_assert!(!validate_secure_input_generation(current));
                discard_changed_capture_context(secure_input_generation());
            }
        }
        append_physical(&text);
        let extracted = take_last_sequence_with_suffix_if_generation(secure_context_generation());
        if complete {
            let (sequence, trailing) = extracted.expect("the entire fresh input must remain convertible");
            prop_assert_eq!(text_of(sequence.into_iter().chain(trailing)), text.as_str());
        } else {
            prop_assert!(extracted.is_none());
            prop_assert!(runs_snapshot().is_empty());
        }

        let previous = secure_input_generation();
        invalidate_capture_context();
        let fresh = token(2);
        prop_assert_eq!(observe_unavailable_for_generation(previous, |_| true), None);
        prop_assert!(validate_secure_input_generation(fresh.raw));
        prop_assert!(validate_secure_context_generation(fresh.context));
        prop_assert_eq!(observe_unavailable_for_generation(fresh.raw, |_| false), Some(true));
        append_physical(&text);
        let (sequence, trailing) = take_last_sequence_with_suffix_if_generation(secure_context_generation())
            .expect("fresh navigation must recover after either history");
        prop_assert_eq!(text_of(sequence.into_iter().chain(trailing)), text);
        clear_secure_input_data();
    }

    #[test]
    fn extracted_history_is_activity_without_a_new_raw_capture(
        prefix in prop::collection::vec(glyph(), 1..17),
        suffix in prop::collection::vec(glyph(), 1..17),
        sequence in any::<bool>(),
        observations in 1_usize..9,
        programmatic in any::<bool>(),
    ) {
        let _guard = test_guard();
        clear_secure_input_data();
        let prefix: String = prefix.into_iter().collect();
        let suffix: String = suffix.into_iter().collect();
        let original = token(0);
        // A converted or restored native history can predate every raw key in
        // this epoch. Do not fake an admission to make the activity check pass.
        let restored = InputRun {
            text: prefix.clone(),
            layout: LayoutTag::En,
            origin: if programmatic { RunOrigin::Programmatic } else { RunOrigin::Physical },
            kind: RunKind::Text,
        };
        prop_assert!(push_runs_if_generation(original.context, [restored]));
        let extracted: Vec<_> = if sequence {
            let (runs, trailing) = take_last_sequence_with_suffix_if_generation(original.context)
                .expect("the complete history must be extractable");
            runs.into_iter().chain(trailing).collect()
        } else {
            let (run, trailing) = take_last_layout_run_with_suffix_if_generation(original.context)
                .expect("the complete history must be extractable");
            std::iter::once(run).chain(trailing).collect()
        };
        prop_assert_eq!(text_of(extracted.clone()), prefix);
        prop_assert!(runs_snapshot().is_empty());

        // The editor still contains the extracted prefix. An empty journal
        // cannot authorize forgetting it merely because its provider is unsure.
        for _ in 0..observations {
            let current = token(1);
            prop_assert_eq!(observe_unavailable_for_generation(current.raw, |_| false), Some(false));
            prop_assert!(!validate_secure_input_generation(current.raw));
            prop_assert!(!validate_secure_context_generation(current.context));
            discard_changed_capture_context(secure_input_generation());
        }
        prop_assert!(!push_runs_if_generation(original.context, extracted));
        append_physical(&suffix);
        prop_assert!(runs_snapshot().is_empty());
        prop_assert!(take_last_sequence_with_suffix_if_generation(secure_context_generation()).is_none());
        clear_secure_input_data();
    }
}

#[cfg(windows)]
mod native {
    use std::collections::VecDeque;

    use super::*;

    #[test]
    fn empty_observation_keeps_native_owner_but_not_caches_or_admitted_input() {
        let _guard = test_guard();
        clear_secure_input_data();
        let a = focus_key_for_test(101, 0).unwrap();
        let b = focus_key_for_test(202, 0).unwrap();
        with_journal_mut(|journal| {
            apply_secure_focus_transition(journal, 101, Some(a), 1_000, false);
            journal.push_text_internal("cached", LayoutTag::En, RunOrigin::Physical);
            apply_secure_focus_transition(journal, 202, Some(b), 1_010, false);
        });
        assert!(with_journal_cache_mut(|cache| cache.contains_key(&a)));
        let fresh = token(0);
        assert_eq!(
            observe_unavailable_for_generation(fresh.raw, |_| false),
            Some(true)
        );
        assert!(validate_secure_input_generation(fresh.raw));
        assert!(!validate_secure_context_generation(fresh.context));
        assert!(with_journal_cache_mut(|cache| cache.is_empty()));
        with_journal_mut(|journal| {
            assert_eq!(journal.last_fg_hwnd, 202);
            assert_eq!(journal.last_focus_key, Some(b));
            assert_eq!(
                capture_generation_for_focus(journal, b, fresh.raw, false, 1_020),
                Some(fresh.raw)
            );
        });
        // A completed no-text key leaves neither runs nor queued work, but its
        // admitted epoch must remain distinguishable from a pristine one.
        let admitted = token(1);
        assert_eq!(
            observe_unavailable_for_generation(admitted.raw, |_| false),
            Some(false)
        );
        assert!(!validate_secure_input_generation(admitted.raw));
        assert!(!validate_secure_context_generation(admitted.context));
        clear_secure_input_data();
    }

    #[derive(Clone, Debug)]
    enum NativeEvent {
        Queue(char),
        Deliver,
        Focus(usize),
        Navigate,
        Reject,
    }

    fn native_event() -> impl Strategy<Value = NativeEvent> {
        prop_oneof![
            4 => glyph().prop_map(NativeEvent::Queue),
            4 => Just(NativeEvent::Deliver),
            4 => (0_usize..3).prop_map(NativeEvent::Focus),
            1 => Just(NativeEvent::Navigate),
            1 => Just(NativeEvent::Reject),
        ]
    }

    #[derive(Default)]
    struct Editor {
        physical: String,
        captured: String,
        revision: usize,
        missing: bool,
    }

    struct QueuedKey {
        owner: usize,
        revision: usize,
        character: char,
        raw: u64,
    }

    fn owner_key(owner: usize) -> FocusCacheKey {
        focus_key_for_test(101 + owner as isize, 0).unwrap()
    }

    fn focus(owner: usize, queue: &VecDeque<QueuedKey>, time: u64) {
        let key = owner_key(owner);
        with_journal_mut(|journal| {
            refresh_capture_focus_with_pending(
                journal,
                key.foreground_hwnd,
                Some(key),
                time,
                |generation| queue.iter().any(|pending| pending.raw == generation),
            );
        });
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(192))]

        #[test]
        fn queued_keys_and_native_focus_preserve_only_complete_owner_histories(
            events in prop::collection::vec(native_event(), 1..65),
        ) {
            let _guard = test_guard();
            clear_secure_input_data();
            let mut editors: [Editor; 3] = std::array::from_fn(|_| Editor::default());
            let mut queue = VecDeque::<QueuedKey>::new();
            let mut owner = 0;
            focus(owner, &queue, 1_000);

            // Visit every owner at the end to check the histories actually
            // restored from cache, not merely the current active journal.
            let events = events.into_iter().chain((0..3).map(NativeEvent::Focus));
            for (step, event) in events.enumerate() {
                match event {
                    NativeEvent::Queue(character) => {
                        editors[owner].physical.push(character);
                        queue.push_back(QueuedKey {
                            owner,
                            revision: editors[owner].revision,
                            character,
                            raw: secure_input_generation(),
                        });
                    }
                    NativeEvent::Deliver => {
                        if let Some(key) = queue.pop_front() {
                            if validate_secure_input_generation(key.raw) {
                                append_physical(&key.character.to_string());
                            }
                            // This oracle uses the editor and its physical
                            // segment, independently of the raw epoch check.
                            if key.owner == owner
                                && key.revision == editors[owner].revision
                                && !editors[owner].missing
                            {
                                editors[owner].captured.push(key.character);
                            }
                        }
                    }
                    NativeEvent::Focus(next) => {
                        if next != owner && queue.iter().any(|key| {
                            key.owner == owner && key.revision == editors[owner].revision
                        }) {
                            editors[owner].missing = true;
                        }
                        let previous = token(0);
                        focus(next, &queue, 1_001 + step as u64);
                        if next == owner {
                            prop_assert!(validate_secure_input_generation(previous.raw));
                            prop_assert!(validate_secure_context_generation(previous.context));
                        }
                        owner = next;
                    }
                    NativeEvent::Navigate => {
                        invalidate_capture_context();
                        editors[owner].physical.clear();
                        editors[owner].captured.clear();
                        editors[owner].revision += 1;
                        editors[owner].missing = false;
                    }
                    NativeEvent::Reject => {
                        let key = owner_key(owner);
                        reject_secure_capture_with_current_focus(
                            secure_input_generation(),
                            key.foreground_hwnd as usize,
                            key.focus_hwnd as usize,
                            || Some(key),
                        );
                        for editor in &mut editors {
                            editor.physical.clear();
                            editor.captured.clear();
                            editor.revision += 1;
                            editor.missing = false;
                        }
                        editors[owner].missing = true;
                        prop_assert!(with_journal_cache_mut(|cache| cache.is_empty()));
                    }
                }
                prop_assert_eq!(text_of(runs_snapshot()), editors[owner].captured.as_str());
                prop_assert_eq!(with_journal(|journal| journal.capture_unavailable), editors[owner].missing);
                if !editors[owner].missing && !queue.iter().any(|key| {
                    key.owner == owner && key.revision == editors[owner].revision
                }) {
                    prop_assert_eq!(&editors[owner].captured, &editors[owner].physical);
                }
            }
            clear_secure_input_data();
        }
    }
}
