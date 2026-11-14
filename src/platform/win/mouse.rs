use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, HHOOK, MSLLHOOKSTRUCT, SetWindowsHookExW, WH_MOUSE_LL,
        WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN,
        WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN,
        WM_RBUTTONUP,
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

    if let Ok(msg) = msg {
        update_input_context(msg);
    }

    let h = HOOK_HANDLE.load(Ordering::Relaxed);
    let hook = (h != 0).then_some(HHOOK(h as *mut _));
    unsafe { CallNextHookEx(hook, code, wparam, lparam) }
}

fn update_input_context(message: u32) {
    match message {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONDBLCLK | WM_MBUTTONDOWN
        | WM_MBUTTONDBLCLK => {
            super::secure_input::invalidate_pointer_context();
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            super::secure_input::refresh_pointer_context();
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => super::secure_input::invalidate_context(),
        _ => {}
    }
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

#[cfg(test)]
mod tests {
    use super::{super::secure_input, *};
    use crate::input::ring_buffer;

    #[test]
    fn pointer_release_preserves_the_press_boundary_and_intervening_input() {
        let _guard = ring_buffer::test_guard();
        for (down, up) in [
            (WM_LBUTTONDOWN, WM_LBUTTONUP),
            (WM_LBUTTONDBLCLK, WM_LBUTTONUP),
            (WM_RBUTTONDOWN, WM_RBUTTONUP),
            (WM_RBUTTONDBLCLK, WM_RBUTTONUP),
            (WM_MBUTTONDOWN, WM_MBUTTONUP),
            (WM_MBUTTONDBLCLK, WM_MBUTTONUP),
        ] {
            ring_buffer::clear_secure_input_data();
            ring_buffer::push_text("previous-field");
            let previous = secure_input::generation();
            update_input_context(down);
            assert!(!secure_input::validate_generation(previous));
            assert!(ring_buffer::runs_snapshot().is_empty());

            let pressed = secure_input::generation();
            ring_buffer::push_text("new-field");
            let captured = ring_buffer::runs_snapshot();
            assert!(!captured.is_empty());
            update_input_context(up);
            assert!(secure_input::validate_generation(pressed));
            assert_eq!(ring_buffer::runs_snapshot(), captured);
        }
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn pointer_release_cannot_reset_a_rejected_input_prefix() {
        let _guard = ring_buffer::test_guard();
        update_input_context(WM_LBUTTONDOWN);
        ring_buffer::secure_capture_unavailable();
        let rejected = secure_input::generation();
        update_input_context(WM_LBUTTONUP);
        assert!(secure_input::validate_generation(rejected));
        ring_buffer::push_text("incomplete-suffix");
        assert!(ring_buffer::runs_snapshot().is_empty());
        ring_buffer::clear_secure_input_data();
    }
}
