use windows::Win32::{
    Foundation::{HWND, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{BN_CLICKED, DestroyWindow, EN_KILLFOCUS, EN_SETFOCUS},
};

use super::state::with_state_mut_do;
use crate::{app::ControlId, platform::ui::error_notifier::T_UI, utils::helpers};

#[cfg_attr(
    debug_assertions,
    tracing::instrument(level = "info", skip_all, fields(msg, id, notif))
)]
pub(crate) fn on_command(hwnd: HWND, wparam: WPARAM) -> LRESULT {
    let id = i32::from(helpers::loword(wparam.0));
    let notif = u32::from(helpers::hiword(wparam.0));

    if let Some(r) = handle_hotkey_capture_focus(hwnd, id, notif) {
        return r;
    }

    if notif != BN_CLICKED {
        return LRESULT(0);
    }

    handle_buttons(hwnd, id)
}

fn handle_hotkey_capture_focus(hwnd: HWND, id: i32, notif: u32) -> Option<LRESULT> {
    let cid = ControlId::from_i32(id)?;

    let slot = match cid {
        ControlId::HotkeyLastWord => crate::app::HotkeySlot::LastWord,
        ControlId::HotkeyLastSequence => crate::app::HotkeySlot::LastSequence,
        ControlId::HotkeyPause => crate::app::HotkeySlot::Pause,
        ControlId::HotkeySelection => crate::app::HotkeySlot::Selection,
        ControlId::HotkeySwitchLayout => crate::app::HotkeySlot::SwitchLayout,
        ControlId::HotkeySmartLastWord => crate::app::HotkeySlot::SmartLastWord,
        ControlId::HotkeySmartLastSequence => crate::app::HotkeySlot::SmartLastSequence,
        ControlId::HotkeySmartSelection => crate::app::HotkeySlot::SmartSelection,
        _ => return None,
    };

    match notif {
        EN_SETFOCUS => {
            with_state_mut_do(hwnd, |state| {
                super::touch_hotkey_settings_control(hwnd, state);
                state.hotkey_capture.start(slot);

                #[cfg(debug_assertions)]
                tracing::debug!(slot = ?slot, "hotkey.capture.start");
            });
            Some(LRESULT(0))
        }

        EN_KILLFOCUS => {
            with_state_mut_do(hwnd, |state| {
                super::touch_hotkey_settings_control(hwnd, state);
                super::stop_hotkey_capture_ui(hwnd, state);

                #[cfg(debug_assertions)]
                tracing::debug!(slot = ?slot, "hotkey.capture.stop");
            });
            Some(LRESULT(0))
        }

        _ => Some(LRESULT(0)),
    }
}

fn handle_buttons(hwnd: HWND, id: i32) -> LRESULT {
    let Some(cid) = ControlId::from_i32(id) else {
        return LRESULT(0);
    };

    match cid {
        ControlId::Autostart => with_state_mut_do(hwnd, |state| {
            super::handle_autostart_toggle(hwnd, state);
        }),

        ControlId::StartMinimized
        | ControlId::DarkTheme
        | ControlId::SmarterHotkeys
        | ControlId::AutoconvertFeature => with_state_mut_do(hwnd, |state| {
            super::handle_setting_toggle(hwnd, state, cid);
        }),

        ControlId::Apply => with_state_mut_do(hwnd, |state| {
            super::handle_apply(hwnd, state);
        }),

        ControlId::Cancel => with_state_mut_do(hwnd, |state| {
            super::handle_cancel(hwnd, state);
        }),

        ControlId::Exit => with_state_mut_do(hwnd, |state| {
            if let Err(e) = unsafe { DestroyWindow(hwnd) } {
                crate::platform::ui::error_notifier::push(
                    hwnd,
                    state,
                    T_UI,
                    "Failed to close the window",
                    &e,
                );
                super::on_app_error(hwnd);
            }
        }),

        _ => {}
    }

    LRESULT(0)
}
