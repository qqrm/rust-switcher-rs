use windows::Win32::{
    Foundation::E_FAIL,
    UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
        SendInput, VIRTUAL_KEY,
    },
};

use super::OWN_INPUT_MARKER;

// Unassigned VK rather than a printable key or another modifier: the target
// observes an intervening key without text, a command, or a changed modifier.
const MENU_MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

fn event(key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: MENU_MASK_KEY,
                wScan: 0,
                dwFlags: if key_up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS::default()
                },
                time: 0,
                dwExtraInfo: OWN_INPUT_MARKER,
            },
        },
    }
}

/// Called inside the low-level hook before forwarding a consumed Alt release.
/// The nested input is delivered while Alt is still logically down, so the
/// eventual release does not activate the target application's menu bar.
pub(super) fn send() -> windows::core::Result<()> {
    let size = i32::try_from(std::mem::size_of::<INPUT>())
        .map_err(|_| windows::core::Error::from_hresult(E_FAIL))?;
    let events = [event(false), event(true)];
    let sent = unsafe { SendInput(&events, size) };
    if sent == 2 {
        return Ok(());
    }
    if sent == 1 {
        // Keep even the no-action mask key balanced after a partial submission.
        let _ = unsafe { SendInput(&events[1..], size) };
    }
    Err(windows::core::Error::new(E_FAIL, "Failed to mask consumed Alt release"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_mask_is_a_balanced_owned_non_modifier_key() {
        let down = unsafe { event(false).Anonymous.ki };
        let up = unsafe { event(true).Anonymous.ki };
        assert_eq!(down.wVk, up.wVk);
        assert_eq!(super::super::vk::mod_bit_for_vk(u32::from(down.wVk.0)), None);
        assert_eq!(down.dwExtraInfo, OWN_INPUT_MARKER);
        assert_eq!(up.dwExtraInfo, OWN_INPUT_MARKER);
        assert_eq!(down.dwFlags, KEYBD_EVENT_FLAGS::default());
        assert_eq!(up.dwFlags, KEYEVENTF_KEYUP);
    }
}
