use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, HHOOK, MSLLHOOKSTRUCT, SetWindowsHookExW, WH_MOUSE_LL,
        WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MOUSEHWHEEL,
        WM_MOUSEWHEEL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN,
    },
};

static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

extern "system" fn proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION.cast_signed() {
        let h = HOOK_HANDLE.load(Ordering::Relaxed);
        let hook = (h != 0).then_some(HHOOK(h as *mut _));
        return unsafe { CallNextHookEx(hook, code, wparam, lparam) };
    }

    let msg = u32::try_from(wparam.0);
    let _ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };

    let should_invalidate = matches!(
        msg,
        Ok(WM_LBUTTONDOWN
            | WM_LBUTTONDBLCLK
            | WM_RBUTTONDOWN
            | WM_RBUTTONDBLCLK
            | WM_MBUTTONDOWN
            | WM_MBUTTONDBLCLK
            | WM_MOUSEWHEEL
            | WM_MOUSEHWHEEL)
    );

    if should_invalidate {
        super::secure_input::invalidate_context();
    }

    let h = HOOK_HANDLE.load(Ordering::Relaxed);
    let hook = (h != 0).then_some(HHOOK(h as *mut _));
    unsafe { CallNextHookEx(hook, code, wparam, lparam) }
}

pub fn install() {
    if HOOK_HANDLE.load(Ordering::Relaxed) != 0 {
        return;
    }

    if let Ok(h) = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(proc), None, 0) } {
        HOOK_HANDLE.store(h.0 as isize, Ordering::Relaxed);
        #[cfg(debug_assertions)]
        tracing::info!("WH_MOUSE_LL installed");
    }
}
