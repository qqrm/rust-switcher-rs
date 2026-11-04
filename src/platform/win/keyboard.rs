mod capture;
#[cfg(debug_assertions)]
pub(crate) mod debug_timers;
#[cfg(debug_assertions)]
mod e2e_scope;
#[cfg(test)]
pub(crate) mod isolated_env;
mod keydown;
mod keyup;
mod menu_mask;
pub(crate) mod mods;
pub(crate) mod sequence;
pub(crate) mod vk;

use std::sync::atomic::{AtomicIsize, Ordering};

#[cfg(debug_assertions)]
use windows::Win32::UI::WindowsAndMessaging::{GW_OWNER, GetForegroundWindow, GetWindow, IsChild};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::SystemInformation::GetTickCount64,
    UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, PostMessageW,
        SetWindowsHookExW, WH_KEYBOARD_LL,
    },
};

use self::vk::{is_keydown_msg, is_keyup_msg, mod_bit_for_vk, normalize_vk};
use crate::{
    input,
    platform::win::keyboard::{keydown::handle_keydown, keyup::handle_keyup},
};

static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// Marks input emitted by Switcher so it cannot capture its own replacements or
/// advance a shortcut while releasing modifiers. Fits both 32-bit and 64-bit Windows.
pub(crate) const OWN_INPUT_MARKER: usize = 0x5253_5749;

fn is_own_input(kb: &KBDLLHOOKSTRUCT) -> bool {
    kb.flags.contains(LLKHF_INJECTED) && kb.dwExtraInfo == OWN_INPUT_MARKER
}

fn now_tick_ms() -> u64 {
    unsafe { GetTickCount64() }
}

fn main_hwnd() -> Option<HWND> {
    let raw = MAIN_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        None
    } else {
        Some(HWND(raw as *mut _))
    }
}

#[cfg(debug_assertions)]
fn foreground_is_owned_by_main() -> bool {
    let Some(main) = main_hwnd() else {
        return false;
    };
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return false;
    }

    foreground_matches_owner(
        main,
        fg,
        |parent, child| unsafe { IsChild(parent, child).as_bool() },
        |hwnd| unsafe { GetWindow(hwnd, GW_OWNER).ok() },
    )
}

#[cfg(debug_assertions)]
fn foreground_matches_owner(
    main: HWND,
    foreground: HWND,
    is_child: impl Fn(HWND, HWND) -> bool,
    owner_of: impl Fn(HWND) -> Option<HWND>,
) -> bool {
    if foreground == main || is_child(main, foreground) {
        return true;
    }

    let mut current = foreground;
    for _ in 0..16 {
        let Some(owner) = owner_of(current) else {
            return false;
        };
        if owner.0.is_null() {
            return false;
        }
        if owner == main || is_child(main, owner) {
            return true;
        }
        current = owner;
    }

    false
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum HookDecision {
    Pass,
    Swallow,
}

impl HookDecision {
    fn should_swallow(self) -> bool {
        matches!(self, Self::Swallow)
    }
}

fn report_hook_error(hwnd: HWND, state: &mut crate::app::AppState, e: &windows::core::Error) {
    crate::platform::ui::error_notifier::push(
        hwnd,
        state,
        crate::platform::ui::error_notifier::T_UI,
        "Hotkey handling failed",
        e,
    );
}

extern "system" fn proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code != HC_ACTION.cast_signed() {
        let h = HOOK_HANDLE.load(Ordering::Relaxed);
        let hook = (h != 0).then_some(HHOOK(h as *mut _));
        return unsafe { CallNextHookEx(hook, code, wparam, lparam) };
    }

    let h = HOOK_HANDLE.load(Ordering::Relaxed);
    let hook = (h != 0).then_some(HHOOK(h as *mut _));

    let Ok(msg) = u32::try_from(wparam.0) else {
        return unsafe { CallNextHookEx(hook, code, wparam, lparam) };
    };

    let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    // This must precede shortcut handling, not just the journal's injected-input
    // filter. Debug E2E deliberately admits other injected input as test typing.
    if is_own_input(kb) {
        return unsafe { CallNextHookEx(hook, code, wparam, lparam) };
    }
    let vk = normalize_vk(kb);
    let is_mod = mod_bit_for_vk(vk).is_some();

    let is_keydown = is_keydown_msg(msg);
    let is_keyup = is_keyup_msg(msg);

    #[cfg(debug_assertions)]
    if !foreground_is_owned_by_main()
        && !e2e_scope::allows_foreground(unsafe { GetForegroundWindow() })
    {
        return unsafe { CallNextHookEx(hook, code, wparam, lparam) };
    }

    let decision = if is_keydown {
        handle_keydown(vk, is_mod)
    } else if is_keyup {
        handle_keyup(vk, is_mod)
    } else {
        Ok(HookDecision::Pass)
    };

    if is_keydown && matches!(decision.as_ref(), Ok(HookDecision::Pass)) {
        let typed = input::ring_buffer::record_keydown(kb, vk);

        if typed.is_some()
            && crate::input::ring_buffer::last_char_triggers_autoconvert()
            && let Some(hwnd) = main_hwnd()
        {
            let _ = unsafe {
                PostMessageW(
                    Some(hwnd),
                    crate::platform::ui::error_notifier::WM_APP_AUTOCONVERT,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
    }

    match decision {
        Ok(HookDecision::Swallow) if is_keyup && matches!(vk, 0xA4 | 0xA5) => {
            // Alt-down already reached the target. Pass its matching release so
            // Windows cannot retain a stuck modifier, but first make it a used
            // Alt press so a consumed tap does not focus the application's menu.
            if let Err(e) = menu_mask::send()
                && let Some(hwnd) = main_hwnd()
            {
                super::with_state_mut_do(hwnd, |state| {
                    report_hook_error(hwnd, state, &e);
                });
            }
        }
        Ok(d) if d.should_swallow() && !(is_mod && is_keyup) => return LRESULT(1),
        Ok(_) => {}
        Err(e) => {
            if let Some(hwnd) = main_hwnd() {
                super::with_state_mut_do(hwnd, |state| {
                    report_hook_error(hwnd, state, &e);
                });
            }
        }
    }

    unsafe { CallNextHookEx(hook, code, wparam, lparam) }
}

/// Installs the low level keyboard hook used for hotkey capture.
///
/// On failure, the error is routed through the UI error notifier. This keeps the
/// release build observable even without logs.
pub fn install(hwnd: HWND, state: &mut crate::app::AppState) {
    MAIN_HWND.store(hwnd.0 as isize, Ordering::Relaxed);

    if HOOK_HANDLE.load(Ordering::Relaxed) != 0 {
        return;
    }

    crate::input::ring_buffer::warm_decoder();
    crate::domain::text::multilang::warm();

    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), None, 0) } {
        Ok(h) => {
            HOOK_HANDLE.store(h.0 as isize, Ordering::Relaxed);
            #[cfg(debug_assertions)]
            tracing::info!("WH_KEYBOARD_LL installed");
        }
        Err(e) => {
            crate::platform::ui::error_notifier::push(
                hwnd,
                state,
                crate::platform::ui::error_notifier::T_UI,
                "Failed to install keyboard hook",
                &e,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hwnd(raw: isize) -> HWND {
        HWND(raw as *mut _)
    }

    #[test]
    fn own_injected_input_is_distinct_from_physical_and_test_input() {
        let mut kb = KBDLLHOOKSTRUCT {
            flags: LLKHF_INJECTED,
            dwExtraInfo: OWN_INPUT_MARKER,
            ..Default::default()
        };
        assert!(is_own_input(&kb));
        kb.dwExtraInfo = 0;
        assert!(!is_own_input(&kb));
        kb.dwExtraInfo = OWN_INPUT_MARKER;
        kb.flags = Default::default();
        assert!(!is_own_input(&kb));
    }

    #[test]
    fn owner_scope_accepts_main_window() {
        assert!(foreground_matches_owner(
            hwnd(100),
            hwnd(100),
            |_, _| false,
            |_| None,
        ));
    }

    #[test]
    fn owner_scope_accepts_child_window() {
        assert!(foreground_matches_owner(
            hwnd(100),
            hwnd(200),
            |parent, child| parent == hwnd(100) && child == hwnd(200),
            |_| None,
        ));
    }

    #[test]
    fn owner_scope_accepts_owned_popup() {
        assert!(foreground_matches_owner(
            hwnd(100),
            hwnd(300),
            |_, _| false,
            |window| (window == hwnd(300)).then_some(hwnd(100)),
        ));
    }

    #[test]
    fn owner_scope_rejects_unrelated_window() {
        assert!(!foreground_matches_owner(
            hwnd(100),
            hwnd(400),
            |_, _| false,
            |_| None,
        ));
    }
}
