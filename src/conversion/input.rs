use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    VIRTUAL_KEY,
};

/// Virtual key code for the Left Arrow key.
///
/// Used to move the caret left when selecting previously inserted text.
const VK_LEFT_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0x25);

/// Virtual key code for the Right Arrow key.
///
/// Used to move the caret right when selecting previously inserted text.
const VK_RIGHT_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0x27);

/// Virtual key code for the Shift key.
///
/// Used as a selection modifier.
const VK_SHIFT_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0x10);

/// Sends a Unicode string using `KEYEVENTF_UNICODE` keyboard input.
///
/// Notes:
/// - The Windows API consumes UTF-16 code units. Supplementary characters are sent
///   as surrogate pairs (two units), which is expected for `KEYEVENTF_UNICODE`.
/// - Returns `true` if all input events were successfully sent.
fn unicode_events(text: &str) -> Vec<INPUT> {
    use windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_UNICODE;

    fn unicode_input(unit: u16, key_up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: unit,
                    dwFlags: if key_up {
                        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                    } else {
                        KEYEVENTF_UNICODE
                    },
                    time: 0,
                    dwExtraInfo: crate::platform::win::keyboard::OWN_INPUT_MARKER,
                },
            },
        }
    }

    let units: Vec<u16> = text.encode_utf16().collect();
    units
        .into_iter()
        .flat_map(|u| [unicode_input(u, false), unicode_input(u, true)])
        .collect()
}

/// Constructs one virtual key event for a later input batch.
fn key_event(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if key_up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS::default()
                },
                time: 0,
                dwExtraInfo: crate::platform::win::keyboard::OWN_INPUT_MARKER,
            },
        },
    }
}

/// Partial insertion can already change the editor. Its previous journal must
/// not be restored, while a failed batch that inserted nothing can recover.
fn complete_input_submission(sent: usize, expected: usize, generation: u64) -> bool {
    if sent > 0 && sent < expected {
        crate::input::ring_buffer::reject_secure_context(generation);
    }
    sent == expected
}

/// Submit replacement as a single ordered input batch after a final target check.
/// Windows does not interleave other keyboard/mouse input with this batch.
pub fn replace_selection_unicode(text: &str, target_is_current: impl Fn() -> bool) -> bool {
    let units = text.encode_utf16().count();
    if units == 0 || units > 4096 {
        return false;
    }
    let delete = VIRTUAL_KEY(0x2E);
    let mut inputs = vec![key_event(delete, false), key_event(delete, true)];
    inputs.extend(unicode_events(text));
    for _ in 0..units {
        inputs.extend([key_event(VK_LEFT_KEY, false), key_event(VK_LEFT_KEY, true)]);
    }
    inputs.push(key_event(VK_SHIFT_KEY, false));
    for _ in 0..units {
        inputs.extend([
            key_event(VK_RIGHT_KEY, false),
            key_event(VK_RIGHT_KEY, true),
        ]);
    }
    inputs.push(key_event(VK_SHIFT_KEY, true));
    let Some(delivery) = crate::platform::win::secure_input::DeliveryContext::capture() else {
        return false;
    };
    let generation = delivery.generation();
    // The provider check can outlive a focus change; validate the target after it.
    if !target_is_current()
        || !crate::platform::win::secure_input::protect_current_context().is_allowed()
        || !target_is_current()
        || !crate::platform::win::secure_input::validate_generation(generation)
    {
        return false;
    }
    let (sent, accepted) = super::input_delivery::submit(&mut inputs, delivery);
    let complete = complete_input_submission(sent, inputs.len(), generation);
    complete && accepted
}

/// Replace the journal's suffix in one input batch. Physical input cannot be
/// interleaved between individual Backspace events and the replacement text.
pub fn replace_before_caret(
    core_len: usize,
    suffix_len: usize,
    suffix_spaces_only: bool,
    suffix_text: &str,
    converted: &str,
    target_is_current: impl Fn() -> bool,
) -> bool {
    const MAX_CHARS: usize = 4096;
    if core_len == 0
        || core_len.saturating_add(suffix_len) > MAX_CHARS
        || converted
            .encode_utf16()
            .count()
            .saturating_add(suffix_text.encode_utf16().count())
            > MAX_CHARS
    {
        return false;
    }
    let mut inputs = Vec::new();
    let append_taps = |inputs: &mut Vec<INPUT>, vk: VIRTUAL_KEY, count: usize| {
        for _ in 0..count {
            inputs.extend([key_event(vk, false), key_event(vk, true)]);
        }
    };
    if suffix_spaces_only {
        append_taps(&mut inputs, VK_LEFT_KEY, suffix_len);
        append_taps(&mut inputs, VIRTUAL_KEY(0x08), core_len);
        inputs.extend(unicode_events(converted));
        append_taps(&mut inputs, VK_RIGHT_KEY, suffix_len);
    } else {
        append_taps(&mut inputs, VIRTUAL_KEY(0x08), core_len + suffix_len);
        inputs.extend(unicode_events(converted));
        inputs.extend(unicode_events(suffix_text));
    }
    let Some(delivery) = crate::platform::win::secure_input::DeliveryContext::capture() else {
        return false;
    };
    let generation = delivery.generation();
    if !target_is_current()
        || !crate::platform::win::secure_input::protect_current_context().is_allowed()
        || !target_is_current()
        || !crate::platform::win::secure_input::validate_generation(generation)
    {
        return false;
    }
    let (sent, accepted) = super::input_delivery::submit(&mut inputs, delivery);
    complete_input_submission(sent, inputs.len(), generation) && accepted
}

#[cfg(test)]
mod selection_tests {
    use crate::input::ring_buffer;

    #[test]
    fn changed_target_cancels_before_native_input() {
        assert!(!super::replace_selection_unicode("привет", || false));
        assert!(!super::replace_before_caret(
            6,
            0,
            false,
            "",
            "привет",
            || false
        ));
        assert!(!super::replace_before_caret(
            4097,
            0,
            false,
            "",
            "привет",
            || true
        ));
    }

    #[test]
    fn partial_insertion_cancels_original_journal_restoration() {
        let _guard = ring_buffer::test_guard();
        for (sent, complete, can_restore) in [
            (0, false, true),
            (1, false, false),
            (7, false, false),
            (8, true, true),
        ] {
            ring_buffer::clear_secure_input_data();
            ring_buffer::push_text("ghbdtn");
            let generation = ring_buffer::secure_context_generation();
            let Some((runs, suffix)) =
                ring_buffer::take_last_sequence_with_suffix_if_generation(generation)
            else {
                panic!("typed input must provide a restorable journal");
            };

            assert_eq!(
                super::complete_input_submission(sent, 8, generation),
                complete
            );
            assert_eq!(
                ring_buffer::push_runs_if_generation(generation, runs.into_iter().chain(suffix)),
                can_restore,
            );
            assert_eq!(ring_buffer::runs_snapshot().is_empty(), !can_restore);
            if !can_restore {
                // A suffix typed after a partially applied edit cannot become a
                // complete word until an explicit new-input boundary occurs.
                ring_buffer::push_text("suffix");
                assert!(ring_buffer::runs_snapshot().is_empty());
                ring_buffer::invalidate_capture_context();
                ring_buffer::push_text("fresh");
                assert!(!ring_buffer::runs_snapshot().is_empty());
            }
        }
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn late_partial_result_cannot_clear_a_newer_editing_context() {
        let _guard = ring_buffer::test_guard();
        ring_buffer::clear_secure_input_data();
        ring_buffer::push_text("old-field");
        let old_generation = ring_buffer::secure_context_generation();
        ring_buffer::invalidate_capture_context();
        ring_buffer::push_text("new-field");
        let current_generation = ring_buffer::secure_context_generation();
        let before = ring_buffer::runs_snapshot();

        assert!(!super::complete_input_submission(1, 8, old_generation));
        assert!(ring_buffer::validate_secure_context_generation(
            current_generation
        ));
        assert_eq!(ring_buffer::runs_snapshot(), before);
        ring_buffer::clear_secure_input_data();
    }
}
