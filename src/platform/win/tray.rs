use std::sync::{Mutex, OnceLock};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, POINT},
        UI::{
            Shell::{
                NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_ERROR, NIIF_INFO,
                NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFY_ICON_INFOTIP_FLAGS,
                NOTIFY_ICON_MESSAGE, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, DestroyMenu, GWLP_HINSTANCE, GetCursorPos,
                GetWindowLongPtrW, HICON, HMENU, IMAGE_ICON, LR_SHARED, LoadImageW, MF_SEPARATOR,
                SW_HIDE, SW_RESTORE, SetForegroundWindow, ShowWindow, TPM_BOTTOMALIGN,
                TPM_NOANIMATION, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
                WM_APP,
            },
        },
    },
    core::{PCWSTR, Result},
};

use super::winutil::make_int_resource;

pub enum TrayMenuAction {
    None,
    ToggleAutoConvert,
    Update,
}

pub const WM_APP_TRAY: u32 = WM_APP + 3;
const TRAY_UID: u32 = 1;
const ID_EXIT: u32 = 1001;
const ID_SHOW_HIDE: u32 = 1002;
const ID_AUTOCONVERT_TOGGLE: u32 = 1003;
const ID_CHANGE_THEME: u32 = 1004;
const ID_UPDATE: u32 = 1005;
const DEFAULT_TRAY_TOOLTIP: &str = "Rust Switcher";

unsafe fn show_popup_menu_at_cursor(hwnd: HWND, hmenu: HMENU) -> u32 {
    let mut pt = POINT { x: 0, y: 0 };
    let _ = unsafe { GetCursorPos(&raw mut pt) };

    let _ = unsafe { SetForegroundWindow(hwnd) };

    let result = unsafe {
        TrackPopupMenu(
            hmenu,
            TPM_RETURNCMD | TPM_BOTTOMALIGN | TPM_RIGHTALIGN | TPM_NOANIMATION | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        )
    };
    result.0 as u32
}

unsafe fn toggle_window_visibility(hwnd: HWND, window_visible: bool) {
    if window_visible {
        let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    } else {
        // Restore is important if the window was minimized internally
        let _ = unsafe { ShowWindow(hwnd, SW_RESTORE) };
        let _ = unsafe { SetForegroundWindow(hwnd) };
    }
}

unsafe fn request_process_exit(hwnd: HWND) -> Result<()> {
    // Сначала убрать иконку, чтобы Shell перестал слать callbacks.
    remove_icon(hwnd);

    // Жестко закрыть окно и остановить message loop.
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd) };
    unsafe { windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0) };

    Ok(())
}

fn fill_wide(dst: &mut [u16], s: &str) {
    if let Some((last, body)) = dst.split_last_mut() {
        for (d, ch) in body
            .iter_mut()
            .zip(s.encode_utf16().chain(std::iter::repeat(0)))
        {
            *d = ch;
        }
        *last = 0;
    }
}

fn tray_tooltip_cache() -> &'static Mutex<String> {
    static CACHE: OnceLock<Mutex<String>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(DEFAULT_TRAY_TOOLTIP.to_string()))
}

fn current_tray_tooltip() -> String {
    match tray_tooltip_cache().lock() {
        Ok(tooltip) => tooltip.clone(),
        Err(_) => {
            tracing::warn!(msg = "tray_tooltip_lock_poisoned_read");
            DEFAULT_TRAY_TOOLTIP.to_string()
        }
    }
}

fn update_cached_tray_tooltip(tooltip: &str) {
    match tray_tooltip_cache().lock() {
        Ok(mut cached) => {
            cached.clear();
            cached.push_str(tooltip);
        }
        Err(_) => tracing::warn!(msg = "tray_tooltip_lock_poisoned_write"),
    }
}

fn shell_notify(
    action: NOTIFY_ICON_MESSAGE,
    nid: &NOTIFYICONDATAW,
    what: &str,
) -> windows::core::Result<()> {
    unsafe {
        if Shell_NotifyIconW(action, nid).as_bool() {
            Ok(())
        } else {
            Err(windows::core::Error::new(
                windows::core::HRESULT(0x8000_4005_u32.cast_signed()),
                format!("Shell_NotifyIconW returned FALSE: {what}"),
            ))
        }
    }
}

pub fn ensure_icon(hwnd: HWND) -> windows::core::Result<()> {
    unsafe {
        let mut nid = base_tray_nid(hwnd)?;
        apply_tray_identity(&mut nid, hwnd)?;
        add_or_modify_tray_icon(&nid)?;
        set_tray_version(&mut nid)?;
        Ok(())
    }
}

unsafe fn base_tray_nid(hwnd: HWND) -> windows::core::Result<NOTIFYICONDATAW> {
    Ok(NOTIFYICONDATAW {
        cbSize: u32::try_from(core::mem::size_of::<NOTIFYICONDATAW>())?,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    })
}

unsafe fn apply_tray_identity(nid: &mut NOTIFYICONDATAW, hwnd: HWND) -> windows::core::Result<()> {
    nid.uCallbackMessage = WM_APP_TRAY;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;

    nid.hIcon = unsafe { default_icon(hwnd) }?;
    fill_wide(&mut nid.szTip, &current_tray_tooltip());

    Ok(())
}

unsafe fn add_or_modify_tray_icon(nid: &NOTIFYICONDATAW) -> windows::core::Result<()> {
    if unsafe { Shell_NotifyIconW(NIM_ADD, &raw const *nid).as_bool() } {
        return Ok(());
    }

    shell_notify(
        NIM_MODIFY,
        nid,
        "ensure_icon: NIM_MODIFY after NIM_ADD failure",
    )
}

unsafe fn set_tray_version(nid: &mut NOTIFYICONDATAW) -> windows::core::Result<()> {
    nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;

    if unsafe { Shell_NotifyIconW(NIM_SETVERSION, &raw const *nid).as_bool() } {
        Ok(())
    } else {
        Err(windows::core::Error::new(
            windows::core::HRESULT(0x8000_4005_u32.cast_signed()),
            "Shell_NotifyIconW returned FALSE: ensure_icon NIM_SETVERSION",
        ))
    }
}

fn balloon_common(
    hwnd: HWND,
    title: &str,
    text: &str,
    flags: u32,
    what: &str,
) -> windows::core::Result<()> {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };

    #[derive(Default)]
    struct Guard {
        last_fp: u64,
        last_at: Option<Instant>,
        suppressed: u64,
    }

    static GUARD: OnceLock<Mutex<Guard>> = OnceLock::new();

    fn fingerprint(title: &str, text: &str, flags: u32) -> u64 {
        let mut h = DefaultHasher::new();
        title.hash(&mut h);
        text.hash(&mut h);
        flags.hash(&mut h);
        h.finish()
    }

    let fp = fingerprint(title, text, flags);

    tracing::debug!(
        msg = "tray_balloon_attempt",
        title = title,
        text = text,
        flags = flags,
    );

    let now = Instant::now();

    // No unwrap: clippy::unwrap-used is denied.
    let guard_lock = GUARD.get_or_init(|| Mutex::new(Guard::default())).lock();
    let mut guard = match guard_lock {
        Ok(g) => Some(g),
        Err(_) => {
            tracing::warn!(msg = "tray_balloon_guard_lock_poisoned");
            None
        }
    };

    if let Some(g) = guard.as_mut() {
        let too_soon = g
            .last_at
            .map(|t| now.duration_since(t) < Duration::from_millis(1500))
            .unwrap_or(false);

        if too_soon && g.last_fp == fp {
            g.suppressed += 1;
            tracing::debug!(
                msg = "tray_balloon_suppressed",
                suppressed_total = g.suppressed,
                title = title
            );
            return Ok(());
        }

        g.last_fp = fp;
        g.last_at = Some(now);
    }

    ensure_icon(hwnd)?;

    let mut nid = NOTIFYICONDATAW {
        cbSize: u32::try_from(core::mem::size_of::<NOTIFYICONDATAW>())?,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };

    nid.uCallbackMessage = WM_APP_TRAY;
    nid.uFlags = NIF_INFO | NIF_MESSAGE;
    nid.dwInfoFlags = NOTIFY_ICON_INFOTIP_FLAGS(flags);
    nid.Anonymous.uTimeout = 10_000;

    fill_wide(&mut nid.szInfoTitle, title);
    fill_wide(&mut nid.szInfo, text);

    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const nid).as_bool() } {
        tracing::debug!(msg = "tray_balloon_shown", title = title);
        return Ok(());
    }

    remove_icon(hwnd);
    ensure_icon(hwnd)?;

    shell_notify(NIM_MODIFY, &nid, what)
}

pub fn balloon_error(hwnd: HWND, title: &str, text: &str) -> windows::core::Result<()> {
    balloon_common(hwnd, title, text, NIIF_ERROR.0, "balloon_error: NIM_MODIFY")
}

pub fn balloon_info(hwnd: HWND, title: &str, text: &str) -> windows::core::Result<()> {
    balloon_common(hwnd, title, text, NIIF_INFO.0, "balloon_info: NIM_MODIFY")
}

pub fn set_tooltip(hwnd: HWND, tooltip: &str) -> windows::core::Result<()> {
    update_cached_tray_tooltip(tooltip);

    let mut nid = NOTIFYICONDATAW {
        cbSize: u32::try_from(core::mem::size_of::<NOTIFYICONDATAW>())?,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };

    nid.uFlags = NIF_TIP | NIF_SHOWTIP;
    fill_wide(&mut nid.szTip, &current_tray_tooltip());

    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const nid).as_bool() } {
        return Ok(());
    }

    ensure_icon(hwnd)
}

pub fn switch_tray_icon(hwnd: HWND, use_green: bool) -> windows::core::Result<()> {
    unsafe {
        let icon = if use_green {
            green_icon(hwnd)?
        } else {
            default_icon(hwnd)?
        };

        let mut nid = NOTIFYICONDATAW {
            cbSize: u32::try_from(core::mem::size_of::<NOTIFYICONDATAW>())?,
            hWnd: hwnd,
            uID: TRAY_UID,
            ..Default::default()
        };

        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = icon;
        fill_wide(&mut nid.szTip, &current_tray_tooltip());

        if Shell_NotifyIconW(NIM_MODIFY, &raw const nid).as_bool() {
            return Ok(());
        }

        remove_icon(hwnd);
        ensure_icon(hwnd)?;
        shell_notify(NIM_MODIFY, &nid, "switch_tray_icon")
    }
}

unsafe fn window_hinstance(hwnd: HWND) -> HINSTANCE {
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_HINSTANCE) };
    HINSTANCE(raw as *mut core::ffi::c_void)
}

unsafe fn green_icon(hwnd: HWND) -> windows::core::Result<HICON> {
    let hinst = unsafe { window_hinstance(hwnd) };

    let h = unsafe {
        LoadImageW(
            Some(hinst),
            make_int_resource(2),
            IMAGE_ICON,
            0,
            0,
            LR_SHARED,
        )
    }?;

    Ok(HICON(h.0))
}

unsafe fn default_icon(hwnd: HWND) -> windows::core::Result<HICON> {
    let hinst = unsafe { window_hinstance(hwnd) };

    let h = unsafe {
        LoadImageW(
            Some(hinst),
            make_int_resource(1),
            IMAGE_ICON,
            0,
            0,
            LR_SHARED,
        )
    }?;

    Ok(HICON(h.0))
}

pub fn remove_icon(hwnd: HWND) {
    unsafe {
        let nid = NOTIFYICONDATAW {
            cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_UID,
            ..Default::default()
        };

        let _ = Shell_NotifyIconW(NIM_DELETE, &raw const nid);
    }
}

pub fn show_tray_context_menu(
    hwnd: HWND,
    window_visible: bool,
    autoconvert_enabled: bool,
    autoconvert_feature_enabled: bool,
    current_theme_dark: bool,
) -> Result<TrayMenuAction> {
    unsafe {
        crate::platform::win::menu_theme::set_tray_menu_preferred_theme(current_theme_dark);

        let hmenu = build_tray_menu(
            window_visible,
            autoconvert_enabled,
            autoconvert_feature_enabled,
            current_theme_dark,
        )?;

        crate::platform::win::menu_theme::flush_tray_menu_theme();

        let cmd = show_popup_menu_at_cursor(hwnd, hmenu);
        let _ = DestroyMenu(hmenu);
        handle_tray_menu_cmd(
            hwnd,
            window_visible,
            autoconvert_enabled,
            autoconvert_feature_enabled,
            current_theme_dark,
            cmd,
        )
    }
}

fn build_tray_menu(
    window_visible: bool,
    autoconvert_enabled: bool,
    autoconvert_feature_enabled: bool,
    current_theme_dark: bool,
) -> Result<HMENU> {
    let hmenu = unsafe { CreatePopupMenu() }?;

    unsafe {
        append_autoconvert_toggle_item(hmenu, autoconvert_enabled, autoconvert_feature_enabled)
    }?;
    unsafe { AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()) }?;

    unsafe { append_show_hide_item(hmenu, window_visible) }?;
    unsafe { AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()) }?;

    unsafe { append_change_theme_item(hmenu, current_theme_dark) }?;
    unsafe { AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()) }?;

    unsafe { append_update_item(hmenu) }?;
    unsafe { AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()) }?;

    unsafe { append_exit_item(hmenu) }?;

    Ok(hmenu)
}

unsafe fn append_autoconvert_toggle_item(
    hmenu: HMENU,
    autoconvert_enabled: bool,
    autoconvert_feature_enabled: bool,
) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, MF_CHECKED, MF_GRAYED, MF_STRING, MF_UNCHECKED,
    };

    let text = "AutoConvert\0";
    let wide: Vec<u16> = text.encode_utf16().collect();

    let check = if autoconvert_enabled {
        MF_CHECKED
    } else {
        MF_UNCHECKED
    };

    (unsafe {
        AppendMenuW(
            hmenu,
            MF_STRING
                | check
                | if autoconvert_feature_enabled {
                    windows::Win32::UI::WindowsAndMessaging::MENU_ITEM_FLAGS(0)
                } else {
                    MF_GRAYED
                },
            ID_AUTOCONVERT_TOGGLE as usize,
            PCWSTR(wide.as_ptr()),
        )
    })?;

    Ok(())
}

unsafe fn append_change_theme_item(hmenu: HMENU, current_theme_dark: bool) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{AppendMenuW, MF_STRING};

    let text = if current_theme_dark {
        "Light\0"
    } else {
        "Dark\0"
    };
    let wide: Vec<u16> = text.encode_utf16().collect();

    (unsafe {
        AppendMenuW(
            hmenu,
            MF_STRING,
            ID_CHANGE_THEME as usize,
            PCWSTR(wide.as_ptr()),
        )
    })?;

    Ok(())
}

unsafe fn append_show_hide_item(hmenu: HMENU, window_visible: bool) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{AppendMenuW, MF_STRING};

    let text = if window_visible { "Hide\0" } else { "Show\0" };
    let wide: Vec<u16> = text.encode_utf16().collect();

    (unsafe {
        AppendMenuW(
            hmenu,
            MF_STRING,
            ID_SHOW_HIDE as usize,
            PCWSTR(wide.as_ptr()),
        )
    })?;

    Ok(())
}

unsafe fn append_update_item(hmenu: HMENU) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{AppendMenuW, MF_STRING};

    let wide: Vec<u16> = "Check for updates\0".encode_utf16().collect();
    (unsafe { AppendMenuW(hmenu, MF_STRING, ID_UPDATE as usize, PCWSTR(wide.as_ptr())) })?;
    Ok(())
}

unsafe fn append_exit_item(hmenu: HMENU) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{AppendMenuW, MF_STRING};

    let text = "Exit\0";
    let wide: Vec<u16> = text.encode_utf16().collect();

    (unsafe { AppendMenuW(hmenu, MF_STRING, ID_EXIT as usize, PCWSTR(wide.as_ptr())) })?;

    Ok(())
}

unsafe fn handle_tray_menu_cmd(
    hwnd: HWND,
    window_visible: bool,
    _autoconvert_enabled: bool,
    autoconvert_feature_enabled: bool,
    current_theme_dark: bool,
    cmd: u32,
) -> Result<TrayMenuAction> {
    match cmd {
        ID_AUTOCONVERT_TOGGLE if autoconvert_feature_enabled => Ok(TrayMenuAction::ToggleAutoConvert),

        ID_SHOW_HIDE => {
            unsafe { toggle_window_visibility(hwnd, window_visible) };
            Ok(TrayMenuAction::None)
        }

        ID_CHANGE_THEME => {
            super::apply_theme_from_tray(hwnd, !current_theme_dark);
            Ok(TrayMenuAction::None)
        }

        ID_UPDATE => Ok(TrayMenuAction::Update),

        ID_EXIT => {
            (unsafe { request_process_exit(hwnd) })?;
            Ok(TrayMenuAction::None)
        }

        _ => Ok(TrayMenuAction::None),
    }
}
