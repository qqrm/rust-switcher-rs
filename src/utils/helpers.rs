//! Helper routines for interacting with the Windows API.  These
//! functions wrap common patterns such as combining style flags or
//! retrieving the last OS error. A RAII guard is provided for
//! enforcing a single application instance.

use windows::{
    Win32::{
        Foundation::{GetLastError, HWND, LPARAM, WPARAM},
        UI::{
            Controls::{BST_CHECKED, BST_UNCHECKED},
            Shell::SetCurrentProcessExplicitAppUserModelID,
            WindowsAndMessaging::{
                BM_GETCHECK, BM_SETCHECK, SendMessageW, SetWindowTextW, WINDOW_STYLE,
            },
        },
    },
    core::{Error, HRESULT, PCWSTR},
};

use crate::app_identity;

/// Combine a base `WINDOW_STYLE` with an additional integer flag.
///
/// The Windows API often exposes style bits as separate types (e.g.
/// `WINDOW_STYLE` for window styles) while certain flags are defined
/// as plain integers.  This helper merges the two into a new
/// `WINDOW_STYLE` by OR‑ing the underlying values.
pub const fn ws_i32(base: WINDOW_STYLE, extra: i32) -> WINDOW_STYLE {
    WINDOW_STYLE(base.0 | extra as u32)
}

/// Retrieve the last OS error as a `windows::core::Error`.
///
/// Calling Windows API functions directly leaves any error details
/// accessible only through `GetLastError`.  Converting the return
/// value into an `Error` makes it easier to propagate failures from
/// leaf functions up the call chain.
pub fn last_error() -> Error {
    Error::from_hresult(HRESULT::from_win32(unsafe { GetLastError() }.0))
}

/// Extract the low 16 bits from a 32‑bit packed parameter.
///
/// In the Windows message system a single `WPARAM` or `LPARAM` often
/// encodes two separate 16‑bit values.  Use this helper to decode
/// the low word from such a parameter.
pub const fn loword(v: usize) -> u16 {
    (v & 0xffff) as u16
}

/// Extract the high 16 bits from a 32‑bit packed parameter.
///
/// See `loword` for more details.  This function shifts the input
/// right by 16 bits and masks off the low word to return the upper
/// portion of a packed parameter.
pub const fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xffff) as u16
}

pub fn default_window_pos(window_w: i32, window_h: i32) -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };

    #[cfg(debug_assertions)]
    {
        // 3x3 grid, cell 7 (left bottom), centered inside that cell
        let cell_w = sw / 3;
        let cell_h = sh / 3;

        let cx = cell_w / 2;
        let cy = cell_h * 2 + cell_h / 2;

        let mut x = cx - window_w / 2;
        let mut y = cy - window_h / 2;

        // Manual tweak for удобного места на твоем сетапе
        x -= 100;
        y -= 95;

        // Clamp to screen
        if x < 0 {
            x = 0;
        }
        if y < 0 {
            y = 0;
        }
        if x + window_w > sw {
            x = sw - window_w;
        }
        if y + window_h > sh {
            y = sh - window_h;
        }

        (x, y)
    }

    #[cfg(not(debug_assertions))]
    {
        let x = (sw - window_w) / 2;
        let y = (sh - window_h) / 2;
        (x, y)
    }
}

pub fn set_checkbox(hwnd: HWND, value: bool) {
    let v = if value { BST_CHECKED } else { BST_UNCHECKED };
    unsafe {
        SendMessageW(
            hwnd,
            BM_SETCHECK,
            Some(WPARAM(v.0 as usize)),
            Some(LPARAM(0)),
        );
    }
}

pub fn get_checkbox(hwnd: HWND) -> bool {
    let r = unsafe { SendMessageW(hwnd, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))) };
    r.0 as u32 == BST_CHECKED.0
}

pub fn set_edit_text(hwnd: HWND, s: &str) -> windows::core::Result<()> {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { SetWindowTextW(hwnd, PCWSTR(wide.as_ptr())) }
}

#[cfg(debug_assertions)]
pub const DEBUG_TIMER_ID_STARTUP_ERROR: usize = 0xC001;

#[cfg(debug_assertions)]
pub const DEBUG_TIMER_ID_STARTUP_INFO: usize = 0xC002;

pub fn init_app_user_model_id() -> windows::core::Result<()> {
    let id: Vec<u16> = app_identity::APP_USER_MODEL_ID
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr())) }
}

#[cfg(debug_assertions)]
pub fn debug_log(msg: &str) {
    tracing::debug!(message = %msg, "debug");
}

#[cfg(debug_assertions)]
pub fn debug_startup_notification(hwnd: HWND, _state: &mut crate::app::AppState) {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows::Win32::UI::WindowsAndMessaging::SetTimer;

    static STARTUP_NOTIFS_ARMED: AtomicBool = AtomicBool::new(false);
    if STARTUP_NOTIFS_ARMED.swap(true, Ordering::SeqCst) {
        return;
    }

    unsafe {
        let _ = SetTimer(Some(hwnd), DEBUG_TIMER_ID_STARTUP_ERROR, 200, None);
        let _ = SetTimer(Some(hwnd), DEBUG_TIMER_ID_STARTUP_INFO, 800, None);
    }
}
