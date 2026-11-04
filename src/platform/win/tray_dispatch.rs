use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::GetDoubleClickTime,
        WindowsAndMessaging::{
            IsWindowVisible, KillTimer, SetTimer, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
            WM_MOUSEMOVE, WM_RBUTTONUP,
        },
    },
};

use crate::platform::win::state::with_state_mut_do;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum TrayEvent {
    LeftClick,
    RightClick,
    DoubleClick,
    Unknown,
}

fn next_autoconvert_state(feature_enabled: bool, autoconvert_enabled: bool) -> Option<bool> {
    feature_enabled.then_some(!autoconvert_enabled)
}

fn tray_event_from_lparam(raw: u32) -> TrayEvent {
    let msg = raw & 0xFFFF;

    match msg {
        WM_LBUTTONUP => TrayEvent::LeftClick,
        WM_LBUTTONDBLCLK => TrayEvent::DoubleClick,
        WM_RBUTTONUP | WM_CONTEXTMENU => TrayEvent::RightClick,
        _ => TrayEvent::Unknown,
    }
}

fn should_open_tray_menu(lo_msg: u32) -> bool {
    if lo_msg != WM_RBUTTONUP && lo_msg != WM_CONTEXTMENU {
        return false;
    }

    static LAST_OPEN_AT: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

    let now = Instant::now();
    let Ok(mut last) = LAST_OPEN_AT.get_or_init(|| Mutex::new(None)).lock() else {
        tracing::warn!(msg = "tray_menu_dedup_lock_poisoned");
        return true;
    };

    if let Some(prev) = *last
        && now.duration_since(prev) < Duration::from_millis(250)
    {
        tracing::debug!(msg = "tray_menu_dedup_suppressed", lo = lo_msg);
        return false;
    }

    *last = Some(now);
    true
}

const TRAY_SINGLE_CLICK_TIMER_ID: usize = 0x5157_0001;

fn pending_single_click_cell() -> &'static OnceLock<Mutex<bool>> {
    static PENDING_SINGLE_CLICK: OnceLock<Mutex<bool>> = OnceLock::new();
    &PENDING_SINGLE_CLICK
}

fn set_pending_single_click(value: bool) {
    let lock = pending_single_click_cell()
        .get_or_init(|| Mutex::new(false))
        .lock();

    if let Ok(mut g) = lock {
        *g = value;
    } else {
        tracing::warn!(msg = "tray_single_click_lock_poisoned_set");
    }
}

fn take_pending_single_click() -> bool {
    let lock = pending_single_click_cell()
        .get_or_init(|| Mutex::new(false))
        .lock();

    if let Ok(mut g) = lock {
        let v = *g;
        *g = false;
        return v;
    }

    tracing::warn!(msg = "tray_single_click_lock_poisoned_take");
    false
}

fn suppress_next_left_click_cell() -> &'static OnceLock<Mutex<bool>> {
    static SUPPRESS_NEXT_LEFT_CLICK: OnceLock<Mutex<bool>> = OnceLock::new();
    &SUPPRESS_NEXT_LEFT_CLICK
}

fn set_suppress_next_left_click(value: bool) {
    let lock = suppress_next_left_click_cell()
        .get_or_init(|| Mutex::new(false))
        .lock();

    if let Ok(mut g) = lock {
        *g = value;
    } else {
        tracing::warn!(msg = "tray_suppress_left_click_lock_poisoned_set");
    }
}

fn take_suppress_next_left_click() -> bool {
    let lock = suppress_next_left_click_cell()
        .get_or_init(|| Mutex::new(false))
        .lock();

    if let Ok(mut g) = lock {
        let v = *g;
        *g = false;
        return v;
    }

    tracing::warn!(msg = "tray_suppress_left_click_lock_poisoned_take");
    false
}

pub fn handle_tray_timer(hwnd: HWND, wparam: WPARAM) -> bool {
    if wparam.0 != TRAY_SINGLE_CLICK_TIMER_ID {
        return false;
    }

    let _ = unsafe { KillTimer(Some(hwnd), TRAY_SINGLE_CLICK_TIMER_ID) };

    if take_pending_single_click() {
        super::toggle_window_visibility_from_tray(hwnd);
    }

    true
}

pub fn handle_tray_message(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let raw = lparam.0 as u32;
    let lo = raw & 0xFFFF;

    if lo == WM_MOUSEMOVE {
        return LRESULT(0);
    }

    let event = tray_event_from_lparam(raw);

    tracing::debug!(
        msg = "wm_app_tray",
        wparam = wparam.0,
        lparam = lparam.0,
        raw = raw,
        lo = lo,
        event = ?event
    );

    match event {
        TrayEvent::LeftClick => {
            // После WM_LBUTTONDBLCLK Windows часто присылает WM_LBUTTONUP.
            // Этот up не должен запускать логику одиночного клика.
            if take_suppress_next_left_click() {
                return LRESULT(0);
            }

            // Одиночный клик откладываем на время double click.
            set_pending_single_click(true);

            let delay_ms = unsafe { GetDoubleClickTime() };

            // На всякий случай: не копим таймеры, переустанавливаем.
            let _ = unsafe { KillTimer(Some(hwnd), TRAY_SINGLE_CLICK_TIMER_ID) };
            let _ = unsafe { SetTimer(Some(hwnd), TRAY_SINGLE_CLICK_TIMER_ID, delay_ms, None) };

            LRESULT(0)
        }

        TrayEvent::DoubleClick => {
            // Отменить запланированный одиночный клик и подавить следующий WM_LBUTTONUP.
            set_pending_single_click(false);
            set_suppress_next_left_click(true);
            let _ = unsafe { KillTimer(Some(hwnd), TRAY_SINGLE_CLICK_TIMER_ID) };

            with_state_mut_do(hwnd, |state| {
                if let Some(next) = next_autoconvert_state(
                    state.autoconvert_feature_enabled,
                    state.autoconvert_enabled,
                ) {
                    super::set_autoconvert_enabled_from_tray(hwnd, state, next, false);
                }
            });

            LRESULT(0)
        }

        TrayEvent::RightClick => {
            if !should_open_tray_menu(lo) {
                return LRESULT(0);
            }

            let window_visible = unsafe { IsWindowVisible(hwnd).as_bool() };

            with_state_mut_do(hwnd, |state| {
                match super::tray::show_tray_context_menu(
                    hwnd,
                    window_visible,
                    state.autoconvert_enabled,
                    state.autoconvert_feature_enabled,
                    state.current_theme_dark,
                ) {
                    Ok(action) => match action {
                        super::tray::TrayMenuAction::None => {}
                        super::tray::TrayMenuAction::Update => super::updater::start(hwnd),
                        super::tray::TrayMenuAction::ToggleAutoConvert => {
                            if let Some(next) = next_autoconvert_state(
                                state.autoconvert_feature_enabled,
                                state.autoconvert_enabled,
                            ) {
                                super::set_autoconvert_enabled_from_tray(
                                    hwnd, state, next, false,
                                );
                            }
                        }
                    },
                    Err(e) => tracing::warn!(error = ?e, "tray menu failed"),
                }
            });

            LRESULT(0)
        }

        TrayEvent::Unknown => LRESULT(0),
    }
}

#[cfg(test)]
mod tests {
    use super::next_autoconvert_state;

    #[test]
    fn double_click_and_menu_cannot_toggle_a_disabled_autoconvert_feature() {
        assert_eq!(next_autoconvert_state(false, false), None);
        assert_eq!(next_autoconvert_state(false, true), None);
    }

    #[test]
    fn tray_toggle_inverts_only_an_enabled_autoconvert_feature() {
        assert_eq!(next_autoconvert_state(true, false), Some(true));
        assert_eq!(next_autoconvert_state(true, true), Some(false));
    }
}
