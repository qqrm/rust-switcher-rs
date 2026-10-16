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

/// A small RAII helper that tracks pressed keys and releases them on drop.
///
/// Intended for modifier keys (Ctrl, Shift, Alt). If `down` succeeds, the key is
/// recorded and will be released in reverse order when the sequence is dropped.
pub struct KeySequence {
    pressed: Vec<VIRTUAL_KEY>,
}

impl KeySequence {
    /// Creates an empty key sequence.
    pub fn new() -> Self {
        Self {
            pressed: Vec::new(),
        }
    }

    /// Sends a key down event and records the key for automatic release.
    ///
    /// Returns `true` if the event was successfully sent.
    pub fn down(&mut self, vk: VIRTUAL_KEY) -> bool {
        send_key(vk, false).then(|| self.pressed.push(vk)).is_some()
    }

    /// Taps a key (down then up).
    ///
    /// Returns `true` if both events were successfully sent.
    pub fn tap(vk: VIRTUAL_KEY) -> bool {
        send_key(vk, false) && send_key(vk, true)
    }
}

impl Default for KeySequence {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeySequence {
    fn drop(&mut self) {
        for vk in self.pressed.drain(..).rev() {
            let _ = send_key(vk, true);
        }
    }
}

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
                    dwExtraInfo: 0,
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

pub fn send_text_unicode(text: &str) -> bool {
    let inputs = unicode_events(text);
    if inputs.is_empty() { return true; }
    let Some(input_size) = input_struct_size_i32() else {
        return false;
    };

    let sent = unsafe { SendInput(&inputs, input_size) } as usize;
    sent == inputs.len()
}

/// Reselects the last inserted text by moving the caret left and selecting right.
///
/// `units` is the number of UTF-16 code units to reselect. This matches the unit
/// count produced by `str::encode_utf16()`.
///
/// Behavior:
/// - Move caret left `units` times
/// - Hold Shift
/// - Move caret right `units` times (selecting the range)
///
/// The selection length is capped to avoid excessive key events.
pub fn reselect_last_inserted_text_utf16_units(units: usize) -> bool {
    const MAX_UNITS: usize = 4096;

    if units == 0 {
        return true;
    }

    let units = units.min(MAX_UNITS);
    let mut seq = KeySequence::new();

    (0..units).all(|_| KeySequence::tap(VK_LEFT_KEY))
        && seq.down(VK_SHIFT_KEY)
        && (0..units).all(|_| KeySequence::tap(VK_RIGHT_KEY))
}

fn input_struct_size_i32() -> Option<i32> {
    i32::try_from(std::mem::size_of::<INPUT>()).ok()
}

/// Sends a single virtual key event via `SendInput`.
///
/// `key_up = false` sends key down, `key_up = true` sends key up.
///
/// Returns `true` if `SendInput` reports that at least one event was inserted.
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
                dwExtraInfo: 0,
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
    if units == 0 || units > 4096 { return false; }
    let delete = VIRTUAL_KEY(0x2E);
    let mut inputs = vec![key_event(delete, false), key_event(delete, true)];
    inputs.extend(unicode_events(text));
    for _ in 0..units {
        inputs.extend([key_event(VK_LEFT_KEY, false), key_event(VK_LEFT_KEY, true)]);
    }
    let shift_index = inputs.len();
    inputs.push(key_event(VK_SHIFT_KEY, false));
    for _ in 0..units {
        inputs.extend([key_event(VK_RIGHT_KEY, false), key_event(VK_RIGHT_KEY, true)]);
    }
    inputs.push(key_event(VK_SHIFT_KEY, true));
    let Some(size) = input_struct_size_i32() else { return false; };
    if !target_is_current() { return false; }
    let sent = unsafe { SendInput(&inputs, size) } as usize;
    if sent > shift_index && sent < inputs.len() {
        let _ = send_key(VK_SHIFT_KEY, true);
    }
    sent == inputs.len()
}

#[cfg(test)]
mod selection_tests {
    #[test]
    fn changed_target_cancels_before_native_input() {
        assert!(!super::replace_selection_unicode("привет", || false));
    }
}
