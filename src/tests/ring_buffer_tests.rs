use std::sync::MutexGuard;

use crate::input::ring_buffer::{self, InputRun, LayoutTag, RunKind, RunOrigin};

fn test_lock() -> MutexGuard<'static, ()> {
    ring_buffer::test_guard()
}

fn journal_text() -> String {
    ring_buffer::runs_snapshot()
        .iter()
        .map(|r| r.text.as_str())
        .collect()
}

#[test]
fn run_journal_merges_contiguous_same_metadata() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_run(InputRun {
        text: "ab".to_string(),
        layout: LayoutTag::En,
        origin: RunOrigin::Physical,
        kind: RunKind::Text,
    });
    ring_buffer::push_run(InputRun {
        text: "cd".to_string(),
        layout: LayoutTag::En,
        origin: RunOrigin::Physical,
        kind: RunKind::Text,
    });

    let runs = ring_buffer::runs_snapshot();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "abcd");
}

#[test]
fn run_journal_splits_on_layout_change() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_run(InputRun {
        text: "ABC".to_string(),
        layout: LayoutTag::En,
        origin: RunOrigin::Physical,
        kind: RunKind::Text,
    });
    ring_buffer::push_run(InputRun {
        text: ",".to_string(),
        layout: LayoutTag::Ru,
        origin: RunOrigin::Physical,
        kind: RunKind::Text,
    });

    let runs = ring_buffer::runs_snapshot();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].layout, LayoutTag::En);
    assert_eq!(runs[1].layout, LayoutTag::Ru);
}

#[test]
fn take_last_layout_run_with_suffix_basic() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_run(InputRun {
        text: "hello".to_string(),
        layout: LayoutTag::En,
        origin: RunOrigin::Physical,
        kind: RunKind::Text,
    });
    ring_buffer::push_run(InputRun {
        text: "   ".to_string(),
        layout: LayoutTag::En,
        origin: RunOrigin::Physical,
        kind: RunKind::Whitespace,
    });

    let (run, suffix) = ring_buffer::take_last_layout_run_with_suffix().expect("payload");
    assert_eq!(run.text, "hello");
    assert_eq!(suffix.len(), 1);
    assert_eq!(suffix[0].text, "   ");
}

#[test]
fn take_last_layout_run_with_suffix_returns_none_for_whitespace_only() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("  \t\n");
    assert!(ring_buffer::take_last_layout_run_with_suffix().is_none());
}

#[test]
fn legacy_push_text_segments_internally() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("hello world   ");

    let (run, suffix) = ring_buffer::take_last_layout_run_with_suffix().expect("payload");
    assert_eq!(run.text, "world");
    assert_eq!(
        suffix.iter().map(|r| r.text.as_str()).collect::<String>(),
        "   "
    );
}

#[test]
fn take_last_layout_sequence_with_suffix_spans_same_origin_layout() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_runs([
        InputRun {
            text: "hello".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        },
        InputRun {
            text: " ".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Whitespace,
        },
        InputRun {
            text: "world".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        },
        InputRun {
            text: "  ".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Whitespace,
        },
    ]);

    let (runs, suffix) = ring_buffer::take_last_layout_sequence_with_suffix().expect("sequence");
    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<String>(),
        "hello world"
    );
    assert_eq!(
        suffix
            .iter()
            .map(|run| run.text.as_str())
            .collect::<String>(),
        "  "
    );
}

#[test]
fn take_last_sequence_keeps_physical_prefix_before_programmatic_tail() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_runs([
        InputRun {
            text: "hfp".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        },
        InputRun {
            text: " ".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Whitespace,
        },
        InputRun {
            text: "ldf".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        },
        InputRun {
            text: " ".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Whitespace,
        },
        InputRun {
            text: "три".to_string(),
            layout: LayoutTag::Ru,
            origin: RunOrigin::Programmatic,
            kind: RunKind::Text,
        },
    ]);

    let (runs, suffix) = ring_buffer::take_last_sequence_with_suffix().expect("sequence");
    assert!(suffix.is_empty());
    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<String>(),
        "hfp ldf три"
    );
    assert_eq!(runs[0].origin, RunOrigin::Physical);
    assert_eq!(runs[4].origin, RunOrigin::Programmatic);
}

#[test]
fn last_char_triggers_autoconvert_still_works_across_runs() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_run(InputRun {
        text: "abc".to_string(),
        layout: LayoutTag::En,
        origin: RunOrigin::Physical,
        kind: RunKind::Text,
    });
    ring_buffer::push_run(InputRun {
        text: ".".to_string(),
        layout: LayoutTag::En,
        origin: RunOrigin::Physical,
        kind: RunKind::Text,
    });
    assert!(ring_buffer::last_char_triggers_autoconvert());

    ring_buffer::push_run(InputRun {
        text: " ".to_string(),
        layout: LayoutTag::En,
        origin: RunOrigin::Physical,
        kind: RunKind::Whitespace,
    });
    assert!(ring_buffer::last_char_triggers_autoconvert());
}

#[test]
fn backspace_removes_from_last_run_and_drops_empty_run() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("a");
    ring_buffer::test_backspace();
    assert!(ring_buffer::take_last_layout_run_with_suffix().is_none());
}

#[test]
fn caret_left_keeps_last_run_before_caret_available() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("ghbdtn world");

    ring_buffer::test_move_caret_left(5);

    let (run, suffix) = ring_buffer::take_last_layout_run_with_suffix().expect("payload");
    assert_eq!(run.text, "ghbdtn");
    assert_eq!(
        suffix.iter().map(|r| r.text.as_str()).collect::<String>(),
        " "
    );

    ring_buffer::push_run(run);
    ring_buffer::push_runs(suffix);
    assert_eq!(journal_text(), "ghbdtn world");
}

#[test]
fn caret_left_keeps_last_sequence_before_caret_available() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("ghbdtn rjynhjkm done");

    ring_buffer::test_move_caret_left(4);

    let (runs, suffix) = ring_buffer::take_last_layout_sequence_with_suffix().expect("payload");
    assert_eq!(
        runs.iter().map(|r| r.text.as_str()).collect::<String>(),
        "ghbdtn rjynhjkm"
    );
    assert_eq!(
        suffix.iter().map(|r| r.text.as_str()).collect::<String>(),
        " "
    );
}

#[test]
fn caret_left_keeps_programmatic_sequence_before_caret_available() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_runs([
        InputRun {
            text: "привет".to_string(),
            layout: LayoutTag::Ru,
            origin: RunOrigin::Programmatic,
            kind: RunKind::Text,
        },
        InputRun {
            text: " ".to_string(),
            layout: LayoutTag::Ru,
            origin: RunOrigin::Programmatic,
            kind: RunKind::Whitespace,
        },
        InputRun {
            text: "world".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        },
    ]);

    ring_buffer::test_move_caret_left(5);

    let (runs, suffix) =
        ring_buffer::take_last_programmatic_sequence_with_suffix().expect("payload");
    assert_eq!(
        runs.iter().map(|r| r.text.as_str()).collect::<String>(),
        "привет"
    );
    assert_eq!(
        suffix.iter().map(|r| r.text.as_str()).collect::<String>(),
        " "
    );
}

#[test]
fn caret_right_moves_back_toward_journal_end() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("one two");

    ring_buffer::test_move_caret_left(5);
    ring_buffer::test_move_caret_right(5);

    let (run, suffix) = ring_buffer::take_last_layout_run_with_suffix().expect("payload");
    assert_eq!(run.text, "two");
    assert!(suffix.is_empty());
}

#[test]
fn typing_after_caret_left_inserts_before_suffix() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("abef");

    ring_buffer::test_move_caret_left(2);
    ring_buffer::push_text("cd");

    assert_eq!(journal_text(), "abcdef");
}

#[test]
fn backspace_after_caret_left_removes_before_caret() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("abcdef");

    ring_buffer::test_move_caret_left(2);
    ring_buffer::test_backspace();

    assert_eq!(journal_text(), "abcef");
}

#[test]
fn caret_left_insertion_preserves_utf8_boundaries() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("привмир");

    ring_buffer::test_move_caret_left(3);
    ring_buffer::push_text("ет ");

    assert_eq!(journal_text(), "привет мир");
}

#[test]
fn excessive_caret_left_clamps_to_start() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("tail");

    ring_buffer::test_move_caret_left(999);
    ring_buffer::push_text("head ");

    assert_eq!(journal_text(), "head tail");
}

#[test]
fn replacement_after_caret_left_preserves_suffix_after_caret() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("ghbdtn world");

    ring_buffer::test_move_caret_left(5);
    let (_run, suffix) = ring_buffer::take_last_layout_run_with_suffix().expect("payload");
    ring_buffer::push_text("привет");
    ring_buffer::push_runs(suffix);

    assert_eq!(journal_text(), "привет world");
}

#[test]
fn production_sequence_extraction_stops_at_caret_and_preserves_the_tail() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_run(InputRun {
        text: "ghbdtn".to_string(),
        layout: LayoutTag::Native(0x0409_0409),
        origin: RunOrigin::Physical,
        kind: RunKind::Text,
    });
    ring_buffer::test_move_caret_left(3);

    let (runs, suffix) = ring_buffer::take_last_sequence_with_suffix().expect("payload");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "ghb");
    assert_eq!(runs[0].layout, LayoutTag::Native(0x0409_0409));
    assert!(suffix.is_empty());
    assert_eq!(journal_text(), "dtn");

    ring_buffer::push_run(InputRun {
        text: "при".to_string(),
        layout: LayoutTag::Native(0x0419_0419),
        origin: RunOrigin::Programmatic,
        kind: RunKind::Text,
    });
    assert_eq!(journal_text(), "приdtn");
    let (runs, suffix) = ring_buffer::take_last_sequence_with_suffix().expect("repeat payload");
    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<String>(),
        "при"
    );
    assert!(suffix.is_empty());
    assert_eq!(journal_text(), "dtn");
}

#[test]
fn production_sequence_extraction_keeps_the_mixed_origin_prefix_before_caret() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_runs([
        InputRun {
            text: "hfp".to_string(),
            layout: LayoutTag::Native(0x0409_0409),
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        },
        InputRun {
            text: " ".to_string(),
            layout: LayoutTag::Native(0x0409_0409),
            origin: RunOrigin::Physical,
            kind: RunKind::Whitespace,
        },
        InputRun {
            text: "два".to_string(),
            layout: LayoutTag::Native(0x0419_0419),
            origin: RunOrigin::Programmatic,
            kind: RunKind::Text,
        },
        InputRun {
            text: " ".to_string(),
            layout: LayoutTag::Native(0x0419_0419),
            origin: RunOrigin::Programmatic,
            kind: RunKind::Whitespace,
        },
        InputRun {
            text: "три".to_string(),
            layout: LayoutTag::Native(0x0419_0419),
            origin: RunOrigin::Programmatic,
            kind: RunKind::Text,
        },
    ]);
    ring_buffer::test_move_caret_left(3);
    let original = ring_buffer::runs_snapshot();

    let (runs, suffix) = ring_buffer::take_last_sequence_with_suffix().expect("payload");
    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<String>(),
        "hfp два"
    );
    assert_eq!(
        suffix
            .iter()
            .map(|run| run.text.as_str())
            .collect::<String>(),
        " "
    );
    assert_eq!(journal_text(), "три");

    // A cancelled plan restores the exact metadata and the caret, not just text.
    ring_buffer::push_runs(runs);
    ring_buffer::push_runs(suffix);
    assert_eq!(ring_buffer::runs_snapshot(), original);
    ring_buffer::push_text("X");
    assert_eq!(journal_text(), "hfp два Xтри");
}

#[test]
fn production_sequence_without_text_before_caret_leaves_the_journal_unchanged() {
    let _guard = test_lock();
    for (text, left, expected) in [("tail", 4, "Xtail"), ("  tail", 4, "  Xtail")] {
        ring_buffer::invalidate();
        ring_buffer::push_text(text);
        ring_buffer::test_move_caret_left(left);
        let original = ring_buffer::runs_snapshot();

        assert!(ring_buffer::take_last_sequence_with_suffix().is_none());
        assert_eq!(ring_buffer::runs_snapshot(), original);
        ring_buffer::push_text("X");
        assert_eq!(journal_text(), expected);
    }
}

#[test]
fn autoconvert_trigger_uses_character_before_caret() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("abc def");

    ring_buffer::test_move_caret_left(3);

    assert!(ring_buffer::last_char_triggers_autoconvert());
}

#[test]
fn foreground_invalidation_state_reset_via_invalidate() {
    let _guard = test_lock();
    ring_buffer::invalidate();
    ring_buffer::push_text("abc");
    ring_buffer::mark_last_token_autoconverted();
    assert!(ring_buffer::last_token_autoconverted());

    ring_buffer::invalidate();
    assert!(!ring_buffer::last_token_autoconverted());
    assert!(ring_buffer::take_last_layout_run_with_suffix().is_none());
}
