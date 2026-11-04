use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
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

fn input_struct_size_i32() -> Option<i32> {
    i32::try_from(std::mem::size_of::<INPUT>()).ok()
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

fn send_key(vk: VIRTUAL_KEY, key_up: bool) -> bool {
    let input = key_event(vk, key_up);

    let Some(input_size) = input_struct_size_i32() else {
        return false;
    };

    let sent = unsafe { SendInput(&[input], input_size) };
    usize::try_from(sent).is_ok_and(|n| n != 0)
}

/// Submit replacement as a single ordered input batch after a final target check.
/// Windows does not interleave other keyboard/mouse input with this batch.
pub fn replace_selection_unicode(text: &str, target_is_current: impl FnOnce() -> bool) -> bool {
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
    let shift_index = inputs.len();
    inputs.push(key_event(VK_SHIFT_KEY, false));
    for _ in 0..units {
        inputs.extend([
            key_event(VK_RIGHT_KEY, false),
            key_event(VK_RIGHT_KEY, true),
        ]);
    }
    inputs.push(key_event(VK_SHIFT_KEY, true));
    let Some(size) = input_struct_size_i32() else {
        return false;
    };
    if !target_is_current() {
        return false;
    }
    let sent = unsafe { SendInput(&inputs, size) } as usize;
    if sent > shift_index && sent < inputs.len() {
        let _ = send_key(VK_SHIFT_KEY, true);
    }
    sent == inputs.len()
}

/// Replace the journal's suffix in one input batch. Physical input cannot be
/// interleaved between individual Backspace events and the replacement text.
pub fn replace_before_caret(
    core_len: usize,
    suffix_len: usize,
    suffix_spaces_only: bool,
    suffix_text: &str,
    converted: &str,
    target_is_current: impl FnOnce() -> bool,
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
    let Some(size) = input_struct_size_i32() else {
        return false;
    };
    if !target_is_current() {
        return false;
    }
    let sent = unsafe { SendInput(&inputs, size) } as usize;
    sent == inputs.len()
}

#[cfg(test)]
mod selection_tests {
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
}
