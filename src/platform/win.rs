//! Window creation and message dispatching.
//!
//! The `win` module encapsulates the main window procedure and the
//! event loop for the application.  It wires together the helper
//! routines, the application state, and the UI construction code to
//! present a settings window and respond to user actions.

mod autostart;
mod commands;
pub(crate) mod hotkey_format;
pub(crate) mod keyboard;
pub(crate) mod layouts;
pub(crate) mod menu_theme;
pub(crate) mod mouse;
pub mod state;
pub(crate) mod tray;
mod tray_dispatch;
mod visuals;
mod window;
mod winutil;
use std::sync::OnceLock;

pub(crate) use hotkey_format::{format_hotkey, format_hotkey_sequence};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{DeleteObject, HFONT, HGDIOBJ, UpdateWindow},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::{
                DefWindowProcW, FindWindowW, GWLP_USERDATA, GetWindowLongPtrW, IsWindow,
                IsWindowVisible, KillTimer, PostMessageW, PostQuitMessage, RegisterWindowMessageW,
                SC_CLOSE, SC_MINIMIZE, SIZE_MINIMIZED, SW_HIDE, SW_RESTORE, SW_SHOWNORMAL,
                SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow, WM_APP, WM_CLOSE,
                WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLORSTATIC,
                WM_DESTROY, WM_DRAWITEM, WM_HOTKEY, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_PAINT,
                WM_PARENTNOTIFY, WM_RBUTTONDOWN, WM_SIZE, WM_SYSCOMMAND, WM_TIMER, WS_MAXIMIZEBOX,
                WS_OVERLAPPEDWINDOW, WS_THICKFRAME,
            },
        },
    },
    core::{PCWSTR, Result},
};

use self::{
    state::{with_state_mut, with_state_mut_do},
    window::{
        compute_window_size, create_main_window, message_loop, register_main_class,
        set_window_icons,
    },
};
pub(crate) const AUTOSTART_ARG: &str = "--autostart";
use windows::Win32::UI::WindowsAndMessaging::{WM_CTLCOLOREDIT, WM_ERASEBKGND};

use crate::{
    app::{AppState, RuntimeCommand},
    app_identity, config,
    domain::text::{last_word::autoconvert_last_word, switch_keyboard_layout},
    input::hotkeys::{HotkeyAction, action_from_id},
    platform::{
        ui::{
            self,
            error_notifier::{T_CONFIG, T_UI, drain_one_and_present},
            notify::on_wm_app_notify,
            themes::*,
        },
        win::{
            tray::{WM_APP_TRAY, remove_icon},
            tray_dispatch::handle_tray_timer,
        },
    },
    utils::helpers,
};

const WM_APP_APPLY_THEME: u32 = WM_APP + 1;
const WM_APP_RUN_RUNTIME_COMMAND: u32 = WM_APP + 2;
const PLAYGROUND_HIDE_TIMER_ID: usize = 0x5300;
const PLAYGROUND_VISIBLE_MS: u32 = 5 * 60 * 1000;

#[rustfmt::skip]
#[cfg(debug_assertions)]
use crate::platform::win::keyboard::debug_timers::handle_timer;

fn set_hwnd_text(hwnd: HWND, s: &str) -> windows::core::Result<()> {
    helpers::set_edit_text(hwnd, s)
}

pub(crate) fn touch_hotkey_settings_control(hwnd: HWND, state: &crate::app::AppState) {
    crate::platform::ui::set_playground_visible(state, true);
    unsafe {
        let _ = KillTimer(Some(hwnd), PLAYGROUND_HIDE_TIMER_ID);
        let _ = SetTimer(
            Some(hwnd),
            PLAYGROUND_HIDE_TIMER_ID,
            PLAYGROUND_VISIBLE_MS,
            None,
        );
    }
}

fn hide_playground(hwnd: HWND, state: &crate::app::AppState) {
    unsafe {
        let _ = KillTimer(Some(hwnd), PLAYGROUND_HIDE_TIMER_ID);
    }
    crate::platform::ui::set_playground_visible(state, false);
}

#[cfg(debug_assertions)]
fn init_e2e_playground(hwnd: HWND, state: &crate::app::AppState) {
    if std::env::var_os("RUST_SWITCHER_E2E_PLAYGROUND").is_none() {
        return;
    }

    crate::platform::ui::set_playground_visible(state, true);
    unsafe {
        let _ = SetFocus(Some(state.edits.playground));
        let _ = SetTimer(
            Some(hwnd),
            PLAYGROUND_HIDE_TIMER_ID,
            PLAYGROUND_VISIBLE_MS,
            None,
        );
    }
}

#[cfg(not(debug_assertions))]
fn init_e2e_playground(_hwnd: HWND, _state: &crate::app::AppState) {}

pub(crate) fn apply_theme_from_tray(hwnd: HWND, dark: bool) {
    // Apply visuals immediately.
    crate::platform::ui::themes::set_window_theme(hwnd, dark);

    // Keep the main window checkbox state in sync, so Apply does not revert it.
    with_state_mut_do(hwnd, |state| {
        helpers::set_checkbox(state.checkboxes.theme_dark, dark);
    });

    // Persist only this setting, do not overwrite other pending UI edits.
    let mut cfg = config::load().unwrap_or_default();
    cfg.theme_dark = dark;

    if let Err(e) = config::save(&cfg) {
        with_state_mut_do(hwnd, |state| {
            crate::platform::ui::error_notifier::push(
                hwnd,
                state,
                T_CONFIG,
                "Failed to save config",
                &io_to_win(e),
            );
        });
    }
}

fn tray_toggle_hotkey_text(state: &AppState) -> String {
    let hotkey = if state.active_pause_hotkey_sequence.is_some() {
        format_hotkey_sequence(state.active_pause_hotkey_sequence)
    } else {
        format_hotkey(state.active_pause_hotkey)
    };

    if hotkey == "None" {
        "not set".to_string()
    } else {
        hotkey
    }
}

fn refresh_tray_tooltip(hwnd: HWND, state: &AppState) {
    let status = if !state.autoconvert_feature_enabled {
        "DISABLED"
    } else if state.autoconvert_enabled {
        "ON"
    } else {
        "OFF"
    };
    let hotkey = if state.autoconvert_feature_enabled {
        tray_toggle_hotkey_text(state)
    } else {
        "disabled".to_string()
    };
    let tooltip = format!(
        "AutoConvert: {status}\r\nToggle hotkey: {}\r\nClick: show/hide",
        hotkey
    );

    if let Err(e) = crate::platform::win::tray::set_tooltip(hwnd, &tooltip) {
        tracing::warn!(error = ?e, "tray set_tooltip failed");
    }
}

fn hotkey_capture_focus_fallback(hwnd: HWND, state: &AppState) -> HWND {
    if !state.buttons.apply.0.is_null() {
        state.buttons.apply
    } else {
        hwnd
    }
}

pub(crate) fn stop_hotkey_capture_ui(hwnd: HWND, state: &mut AppState) {
    let was_active = state.hotkey_capture.active;
    state.hotkey_capture.stop();

    if was_active && !hwnd.0.is_null() {
        let target = hotkey_capture_focus_fallback(hwnd, state);
        unsafe {
            if IsWindow(Some(target)).as_bool() {
                let _ = SetFocus(Some(target));
            } else if IsWindow(Some(hwnd)).as_bool() {
                let _ = SetFocus(Some(hwnd));
            }
        }
    }
}

pub fn refresh_autostart_checkbox(state: &mut AppState) -> windows::core::Result<()> {
    let enabled = crate::platform::win::autostart::is_enabled()?;
    crate::utils::helpers::set_checkbox(state.checkboxes.autostart, enabled);
    Ok(())
}

fn apply_config_to_ui(
    hwnd: HWND,
    state: &mut AppState,
    cfg: &config::Config,
) -> windows::core::Result<()> {
    refresh_autostart_checkbox(state)?;
    helpers::set_checkbox(state.checkboxes.start_minimized, cfg.start_minimized);
    helpers::set_checkbox(state.checkboxes.theme_dark, cfg.theme_dark);
    helpers::set_checkbox(
        state.checkboxes.smarter_hotkeys,
        cfg.smarter_hotkeys_enabled,
    );
    helpers::set_checkbox(
        state.checkboxes.autoconvert_feature,
        cfg.autoconvert_feature_enabled,
    );

    state.hotkey_values = crate::app::HotkeyValues::from_config(cfg);
    state.hotkey_sequence_values = crate::app::HotkeySequenceValues::from_config_all(cfg);

    let last_word_text = if cfg.hotkey_convert_last_word_sequence.is_some() {
        format_hotkey_sequence(cfg.hotkey_convert_last_word_sequence)
    } else {
        format_hotkey(cfg.hotkey_convert_last_word)
    };
    set_hwnd_text(state.hotkeys.last_word, &last_word_text)?;

    let last_sequence_text = if cfg.hotkey_convert_last_sequence_sequence.is_some() {
        format_hotkey_sequence(cfg.hotkey_convert_last_sequence_sequence)
    } else {
        format_hotkey(cfg.hotkey_convert_last_sequence)
    };
    set_hwnd_text(state.hotkeys.last_sequence, &last_sequence_text)?;

    let pause_text = if cfg.hotkey_pause_sequence.is_some() {
        format_hotkey_sequence(cfg.hotkey_pause_sequence)
    } else {
        format_hotkey(cfg.hotkey_pause)
    };
    set_hwnd_text(state.hotkeys.pause, &pause_text)?;

    let selection_text = if cfg.hotkey_convert_selection_sequence.is_some() {
        format_hotkey_sequence(cfg.hotkey_convert_selection_sequence)
    } else {
        format_hotkey(cfg.hotkey_convert_selection)
    };
    set_hwnd_text(state.hotkeys.selection, &selection_text)?;

    let switch_layout_text = if cfg.hotkey_switch_layout_sequence.is_some() {
        format_hotkey_sequence(cfg.hotkey_switch_layout_sequence)
    } else {
        format_hotkey(cfg.hotkey_switch_layout)
    };
    set_hwnd_text(state.hotkeys.switch_layout, &switch_layout_text)?;

    set_hwnd_text(
        state.hotkeys.smart_last_word,
        &format_hotkey_sequence(cfg.smart_hotkey_convert_last_word_sequence),
    )?;
    set_hwnd_text(
        state.hotkeys.smart_last_sequence,
        &format_hotkey_sequence(cfg.smart_hotkey_convert_last_sequence_sequence),
    )?;
    set_hwnd_text(
        state.hotkeys.smart_selection,
        &format_hotkey_sequence(cfg.smart_hotkey_convert_selection_sequence),
    )?;
    crate::platform::ui::sync_smarter_hotkey_controls(hwnd, state, cfg.smarter_hotkeys_enabled)?;
    crate::platform::ui::sync_autoconvert_controls(state, cfg.autoconvert_feature_enabled);

    Ok(())
}

fn read_ui_to_config(state: &AppState, mut cfg: config::Config) -> config::Config {
    cfg.delay_ms = config::CONVERSION_DELAY_MS;

    cfg.start_minimized = helpers::get_checkbox(state.checkboxes.start_minimized);
    cfg.theme_dark = helpers::get_checkbox(state.checkboxes.theme_dark);
    cfg.smarter_hotkeys_enabled = helpers::get_checkbox(state.checkboxes.smarter_hotkeys);
    cfg.autoconvert_feature_enabled = helpers::get_checkbox(state.checkboxes.autoconvert_feature);

    cfg.hotkey_convert_last_word_sequence = state.hotkey_sequence_values.last_word;
    cfg.hotkey_convert_last_sequence_sequence = state.hotkey_sequence_values.last_sequence;
    cfg.hotkey_pause_sequence = state.hotkey_sequence_values.pause;
    cfg.hotkey_convert_selection_sequence = state.hotkey_sequence_values.selection;
    cfg.hotkey_switch_layout_sequence = state.hotkey_sequence_values.switch_layout;
    cfg.smart_hotkey_convert_last_word_sequence = state.hotkey_sequence_values.smart_last_word;
    cfg.smart_hotkey_convert_last_sequence_sequence =
        state.hotkey_sequence_values.smart_last_sequence;
    cfg.smart_hotkey_convert_selection_sequence = state.hotkey_sequence_values.smart_selection;

    fn hk_or_none_if_double(
        seq: Option<config::HotkeySequence>,
        hk: Option<config::Hotkey>,
    ) -> Option<config::Hotkey> {
        match seq {
            Some(s) if s.second.is_some() => None,
            _ => hk,
        }
    }

    cfg.hotkey_convert_last_word = hk_or_none_if_double(
        cfg.hotkey_convert_last_word_sequence,
        state.hotkey_values.last_word,
    );
    cfg.hotkey_convert_last_sequence = hk_or_none_if_double(
        cfg.hotkey_convert_last_sequence_sequence,
        state.hotkey_values.last_sequence,
    );
    cfg.hotkey_pause = hk_or_none_if_double(cfg.hotkey_pause_sequence, state.hotkey_values.pause);
    cfg.hotkey_convert_selection = hk_or_none_if_double(
        cfg.hotkey_convert_selection_sequence,
        state.hotkey_values.selection,
    );
    cfg.hotkey_switch_layout = match cfg.hotkey_switch_layout_sequence {
        Some(_) => None,
        None => state.hotkey_values.switch_layout,
    };

    cfg
}

fn apply_config_runtime(
    hwnd: HWND,
    state: &mut AppState,
    cfg: &config::Config,
) -> windows::core::Result<()> {
    state.active_hotkey_sequences = crate::app::HotkeySequenceValues::from_config(cfg);
    state.active_pause_hotkey = cfg.hotkey_pause;
    state.active_pause_hotkey_sequence = cfg.hotkey_pause_sequence;
    let cancelled_deferred_pause =
        state.set_autoconvert_feature_enabled(cfg.autoconvert_feature_enabled);
    if cancelled_deferred_pause {
        unsafe {
            let _ = KillTimer(
                Some(hwnd),
                crate::platform::win::keyboard::sequence::DEFERRED_SEQUENCE_TIMER_ID,
            );
        }
    }

    state.runtime_chord_capture = crate::app::RuntimeChordCapture::default();
    state.hotkey_sequence_progress = crate::app::HotkeySequenceProgress::default();

    state.active_switch_layout_sequence = cfg.hotkey_switch_layout_sequence;
    state.switch_layout_waiting_second = false;
    state.switch_layout_first_tick_ms = 0;

    ui::error_notifier::report_unit(
        hwnd,
        state,
        T_CONFIG,
        "Failed to register hotkeys",
        crate::input::hotkeys::register_from_config(hwnd, cfg),
    );

    // ВАЖНО: зафиксировать тему в state до любых WM_CTLCOLOR*.
    // А само применение Visual Styles для чекбоксов делать отложенно на старте.
    let dark = cfg.theme_dark;

    state.current_theme_dark = dark;

    if unsafe { IsWindowVisible(hwnd).as_bool() } {
        set_window_theme(hwnd, dark);
    } else {
        unsafe {
            let _ = PostMessageW(
                Some(hwnd),
                WM_APP_APPLY_THEME,
                WPARAM(if dark { 1 } else { 0 }),
                LPARAM(0),
            );
        }
    }

    refresh_tray_tooltip(hwnd, state);

    if let Err(e) = crate::platform::win::tray::switch_tray_icon(hwnd, false) {
        tracing::warn!(error = ?e, "switch_tray_icon failed while applying config");
    }

    Ok(())
}

/// Makes the AutoConvert checkbox effective immediately, including every
/// runtime entry point. The persisted value is still changed only by Apply.
pub(crate) fn set_autoconvert_feature_enabled_from_ui(
    hwnd: HWND,
    state: &mut AppState,
    enabled: bool,
) {
    let cancelled_deferred_pause = state.set_autoconvert_feature_enabled(enabled);
    if cancelled_deferred_pause {
        unsafe {
            let _ = KillTimer(
                Some(hwnd),
                crate::platform::win::keyboard::sequence::DEFERRED_SEQUENCE_TIMER_ID,
            );
        }
    }

    let hotkey = enabled.then_some(state.active_pause_hotkey).flatten();
    ui::error_notifier::report_unit(
        hwnd,
        state,
        T_CONFIG,
        "Failed to update AutoConvert hotkey",
        crate::input::hotkeys::register_pause_hotkey(hwnd, hotkey),
    );

    refresh_tray_tooltip(hwnd, state);
    if let Err(e) = crate::platform::win::tray::switch_tray_icon(hwnd, false) {
        tracing::warn!(error = ?e, "switch_tray_icon failed while toggling AutoConvert feature");
    }
}

fn init_font_and_visuals(hwnd: HWND, state: &mut AppState) {
    unsafe {
        match visuals::create_message_font() {
            Ok(font) => state.font = font,
            Err(_) => state.font = HFONT::default(),
        }
        if !state.font.0.is_null() {
            visuals::apply_modern_look(hwnd, state.font);
        }
    }
}

macro_rules! startup_or_return0 {
    ($hwnd:expr, $state:expr, $text:expr, $expr:expr) => {{
        match $expr {
            Ok(v) => v,
            Err(e) => {
                $crate::platform::ui::error_notifier::push($hwnd, $state, "", $text, &e);
                on_app_error($hwnd);
                // `on_create` stores a raw `AppState` pointer in GWLP_USERDATA before
                // running startup steps. On early return, clear it to avoid leaving
                // a dangling pointer after local `Box<AppState>` is dropped.
                unsafe {
                    SetWindowLongPtrW($hwnd, GWLP_USERDATA, 0);
                }
                return LRESULT(0);
            }
        }
    }};
}

pub fn handle_autostart_toggle(hwnd: HWND, state: &mut AppState) {
    let desired = crate::utils::helpers::get_checkbox(state.checkboxes.autostart);

    if let Err(e) = crate::platform::win::autostart::set_enabled(desired) {
        crate::platform::ui::error_notifier::push(
            hwnd,
            state,
            T_UI,
            "Failed to update autostart setting",
            &e,
        );

        let _ = refresh_autostart_checkbox(state);
    }
}

/// Loads config and validates it for runtime use.
///
/// On invalid config, this function notifies the user and falls back to defaults.
/// This keeps the application operational even when the config file was edited manually.
fn load_config_or_default(hwnd: HWND, state: &mut AppState) -> config::Config {
    config::load()
        .map_err(|e| {
            crate::platform::ui::error_notifier::push(
                hwnd,
                state,
                T_CONFIG,
                "Failed to load config, using defaults",
                &io_to_win(e),
            );
        })
        .ok()
        .and_then(|cfg| match cfg.validate_hotkey_sequences() {
            Ok(()) => Some(cfg),
            Err(msg) => {
                let user_text = msg.clone();
                let source = io_to_win(std::io::Error::new(std::io::ErrorKind::InvalidInput, msg));
                crate::platform::ui::error_notifier::push(
                    hwnd, state, T_CONFIG, &user_text, &source,
                );
                None
            }
        })
        .unwrap_or_default()
}

fn on_create(hwnd: HWND) -> LRESULT {
    let mut state = Box::new(AppState::default());

    // ВАЖНО: state должен быть доступен через get_state/with_state_mut_do
    // уже во время создания контролов и их первого paint.
    unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            state.as_mut() as *mut AppState as isize,
        );
    }

    #[rustfmt::skip]
    startup_or_return0!(hwnd, &mut state, "Failed to create UI controls", ui::create_controls(hwnd, &mut state));
    let cfg = load_config_or_default(hwnd, state.as_mut());

    startup_or_return0!(
        hwnd,
        &mut state,
        "Failed to migrate autostart setting",
        autostart::migrate_legacy_shortcut()
    );

    state.hotkey_values = crate::app::HotkeyValues::from_config(&cfg);
    state.active_hotkey_sequences = crate::app::HotkeySequenceValues::from_config(&cfg);

    startup_or_return0!(
        hwnd,
        &mut state,
        "Failed to apply config to UI",
        apply_config_to_ui(hwnd, state.as_mut(), &cfg)
    );
    startup_or_return0!(
        hwnd,
        &mut state,
        "Failed to read autostart state",
        refresh_autostart_checkbox(state.as_mut())
    );
    startup_or_return0!(
        hwnd,
        &mut state,
        "Failed to apply config at runtime",
        apply_config_runtime(hwnd, state.as_mut(), &cfg)
    );

    keyboard::install(hwnd, state.as_mut());
    mouse::install();

    init_font_and_visuals(hwnd, &mut state);
    init_e2e_playground(hwnd, state.as_ref());

    if let Err(e) = crate::platform::win::tray::ensure_icon(hwnd) {
        tracing::warn!(error = ?e, "tray ensure_icon failed");
    }

    #[cfg(debug_assertions)]
    with_state_mut_do(hwnd, |state| {
        helpers::debug_startup_notification(hwnd, state);
    });

    // Протекаем Box, чтобы AppState жил столько же, сколько окно.
    let _ = Box::into_raw(state);

    LRESULT(0)
}

/// Start the main window and enter the message loop.
///
/// This function is called from `main` after the single instance
/// guard has been acquired. It performs all initialization that
/// requires unsafe code and returns any error to the caller.
pub fn run(start_hidden: bool) -> Result<()> {
    unsafe {
        visuals::init_visuals();
        let class_name_w: Vec<u16> = app_identity::MAIN_WINDOW_CLASS
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let class_name = PCWSTR(class_name_w.as_ptr());
        let hinstance = GetModuleHandleW(PCWSTR::null())?.into();

        register_main_class(class_name, hinstance)?;

        let style = WS_OVERLAPPEDWINDOW & !WS_THICKFRAME & !WS_MAXIMIZEBOX;
        let (window_w, window_h) = compute_window_size(style)?;

        let (x, y) = helpers::default_window_pos(window_w, window_h);

        let hwnd = create_main_window(class_name, hinstance, style, x, y, window_w, window_h)?;
        set_window_icons(hwnd, hinstance);

        if start_hidden {
            let _ = ShowWindow(hwnd, SW_HIDE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = UpdateWindow(hwnd);
        }

        message_loop()?;
    }
    Ok(())
}

/// The window procedure.  Handles creation, command and destroy
/// messages.  Any unhandled messages are forwarded to the default
/// procedure.
pub extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    const WM_NCDESTROY: u32 = 0x0082;

    if msg == show_window_message_id() {
        show_main_window(hwnd);
        return LRESULT(0);
    }

    match msg {
        WM_CREATE => on_create(hwnd),
        WM_COMMAND => commands::on_command(hwnd, wparam),
        WM_HOTKEY => on_hotkey(hwnd, wparam),
        WM_TIMER => on_timer(hwnd, wparam, lparam),
        WM_PAINT => crate::platform::ui::themes::on_paint(hwnd, wparam, lparam),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            with_state_mut_do(hwnd, |state| stop_hotkey_capture_ui(hwnd, state));
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_PARENTNOTIFY => {
            with_state_mut_do(hwnd, |state| stop_hotkey_capture_ui(hwnd, state));
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        //Purpose: Customize the background color of a dialog box itself.
        //When sent: When the dialog box background is about to be painted.
        WM_CTLCOLORDLG => on_color_dialog(hwnd, wparam, lparam),

        //Purpose: Customize colors of static controls (labels, group boxes, icons, text fields).
        //When sent: Before a static control is drawn.
        WM_CTLCOLORSTATIC => on_color_static(hwnd, wparam, lparam),

        //Purpose: Customize colors of edit controls (text boxes).
        //When sent: Before an edit control is drawn.
        WM_CTLCOLOREDIT => on_color_edit(hwnd, wparam, lparam),

        //Allows the application to customize the window background instead of using the default system color
        //Sent by Windows when a window's background needs to be cleared/repainted
        WM_ERASEBKGND => on_erase_background(hwnd, wparam, lparam),

        //For buttons
        WM_DRAWITEM => on_draw_item(hwnd, wparam, lparam),
        WM_APP_APPLY_THEME => {
            let dark = wparam.0 != 0;
            set_window_theme(hwnd, dark);
            LRESULT(0)
        }

        WM_CTLCOLORBTN => on_ctlcolor(hwnd, wparam, lparam),
        WM_SIZE => {
            if wparam.0 == SIZE_MINIMIZED as usize {
                with_state_mut_do(hwnd, |state| hide_playground(hwnd, state));
                let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SYSCOMMAND => {
            let cmd = wparam.0 & 0xFFF0usize;

            if cmd == SC_CLOSE as usize || cmd == SC_MINIMIZE as usize {
                with_state_mut_do(hwnd, |state| hide_playground(hwnd, state));
                let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
                return LRESULT(0);
            }

            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_CLOSE => {
            with_state_mut_do(hwnd, |state| hide_playground(hwnd, state));
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => unsafe { on_ncdestroy(hwnd) },

        crate::platform::ui::notify::WM_APP_NOTIFY => {
            on_wm_app_notify(hwnd);
            LRESULT(0)
        }

        crate::platform::ui::error_notifier::WM_APP_ERROR => {
            with_state_mut_do(hwnd, |state| {
                drain_one_and_present(hwnd, state);
            });
            LRESULT(0)
        }

        crate::platform::ui::error_notifier::WM_APP_AUTOCONVERT => {
            if crate::input::ring_buffer::last_token_autoconverted() {
                return LRESULT(0);
            }

            with_state_mut_do(hwnd, |state| {
                if state.autoconvert_feature_enabled && state.autoconvert_enabled {
                    enqueue_runtime_command(hwnd, state, RuntimeCommand::AutoconvertLastWord);
                }
            });

            LRESULT(0)
        }

        WM_APP_RUN_RUNTIME_COMMAND => {
            with_state_mut_do(hwnd, |state| {
                process_next_runtime_command(hwnd, state);
            });
            LRESULT(0)
        }

        WM_APP_TRAY => tray_dispatch::handle_tray_message(hwnd, wparam, lparam),

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn io_to_win(e: std::io::Error) -> windows::core::Error {
    use windows::core::{Error, HRESULT};
    Error::new(HRESULT(0x8000_4005_u32 as i32), e.to_string())
}

/// Error returned by `build_and_save_config_from_ui`.
///
/// This type carries two payloads:
/// - `user_text`: a short message intended to be shown to the user
/// - `source`: the underlying Win32 error used for debug details
struct ApplyConfigError {
    user_text: String,
    source: windows::core::Error,
}

impl ApplyConfigError {
    /// Enqueues the error into the UI notifier queue and schedules UI processing.
    ///
    /// This method is intentionally side effecting and should be called only from
    /// the UI boundary code (for example a window command handler).
    fn notify(self, hwnd: HWND, state: &mut AppState) {
        crate::platform::ui::error_notifier::push(
            hwnd,
            state,
            T_CONFIG,
            &self.user_text,
            &self.source,
        );
    }
}

fn build_and_save_config_from_ui(
    state: &mut AppState,
) -> std::result::Result<config::Config, ApplyConfigError> {
    // Apply must not depend on reading existing config file.
    // Build a fresh config from UI and overwrite on disk.
    let cfg = read_ui_to_config(state, config::Config::default());

    config::save(&cfg).map_err(|e| {
        let user_text = match e.kind() {
            std::io::ErrorKind::InvalidInput => e.to_string(),
            _ => "Failed to save config".to_string(),
        };

        ApplyConfigError {
            user_text,
            source: io_to_win(e),
        }
    })?;

    Ok(cfg)
}

/// Applies configuration changes requested by the UI.
///
/// Architectural boundary:
/// This handler is a UI boundary. It coordinates:
/// - persistence (load, merge, save)
/// - runtime application of the new config
/// - updating the UI to reflect the saved config
///
/// All user notifications are emitted here. Lower level helpers return `Result` only.
fn handle_apply(hwnd: HWND, state: &mut AppState) {
    let cfg = match build_and_save_config_from_ui(state) {
        Ok(cfg) => cfg,
        Err(e) => {
            e.notify(hwnd, state);
            return;
        }
    };

    let apply_runtime = apply_config_runtime(hwnd, state, &cfg);
    ui::error_notifier::report_unit(
        hwnd,
        state,
        T_CONFIG,
        "Failed to apply config at runtime",
        apply_runtime,
    );

    let apply_ui = apply_config_to_ui(hwnd, state, &cfg);
    ui::error_notifier::report_unit(
        hwnd,
        state,
        T_UI,
        "Failed to update UI from config",
        apply_ui,
    );
}

fn handle_cancel(hwnd: HWND, state: &mut AppState) {
    let cfg = config::load().unwrap_or_default();

    let apply_runtime = apply_config_runtime(hwnd, state, &cfg);
    ui::error_notifier::report_unit(
        hwnd,
        state,
        T_CONFIG,
        "Failed to apply config at runtime",
        apply_runtime,
    );

    let apply_ui = apply_config_to_ui(hwnd, state, &cfg);
    ui::error_notifier::report_unit(
        hwnd,
        state,
        T_UI,
        "Failed to update UI from config",
        apply_ui,
    );
}

fn show_window_message_id() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(|| {
        let name: Vec<u16> = app_identity::SHOW_MAIN_WINDOW_MESSAGE
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) }
    })
}

pub fn activate_running_instance() -> windows::core::Result<bool> {
    unsafe {
        let class_name: Vec<u16> = app_identity::MAIN_WINDOW_CLASS
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let hwnd = FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR::null()).unwrap_or_default();
        if hwnd.0.is_null() {
            return Ok(false);
        }

        let msg = show_window_message_id();
        let _ = PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0));
        Ok(true)
    }
}

fn show_main_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
}

unsafe fn on_ncdestroy(hwnd: HWND) -> LRESULT {
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppState;
    if p.is_null() {
        return LRESULT(0);
    }

    remove_icon(hwnd);

    let state = unsafe { &mut *p };

    if !state.font.0.is_null() {
        let _ = unsafe { DeleteObject(HGDIOBJ(state.font.0)) };
    }

    // Delete cached dark theme brushes
    if !state.dark_brush_window_bg.0.is_null() {
        let _ = unsafe { DeleteObject(HGDIOBJ::from(state.dark_brush_window_bg)) };
        state.dark_brush_window_bg = Default::default();
    }
    if !state.dark_brush_control_bg.0.is_null() {
        let _ = unsafe { DeleteObject(HGDIOBJ::from(state.dark_brush_control_bg)) };
        state.dark_brush_control_bg = Default::default();
    }
    if !state.dark_brush_edit_bg.0.is_null() {
        let _ = unsafe { DeleteObject(HGDIOBJ::from(state.dark_brush_edit_bg)) };
        state.dark_brush_edit_bg = Default::default();
    }

    drop(unsafe { Box::from_raw(p) });
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
    LRESULT(0)
}

pub fn hotkey_id_from_wparam(wparam: WPARAM) -> i32 {
    wparam.0 as i32
}

fn handle_pause_toggle(hwnd: HWND, state: &mut AppState) {
    if !state.autoconvert_feature_enabled {
        return;
    }
    let enabled = !state.autoconvert_enabled;
    set_autoconvert_enabled_from_tray(hwnd, state, enabled, true);
}

fn last_word_hotkey_should_try_selection_first(state: &AppState) -> bool {
    let last_word = state.active_hotkey_sequences.last_word;
    let selection = state.active_hotkey_sequences.selection;
    last_word.is_some() && last_word == selection
}

fn smart_last_word_hotkey_should_try_selection_first(state: &AppState) -> bool {
    let last_word = state.active_hotkey_sequences.smart_last_word;
    let selection = state.active_hotkey_sequences.smart_selection;
    last_word.is_some() && last_word == selection
}

fn layout_hotkey_shares_sequence_with(
    state: &AppState,
    sequence: Option<crate::config::HotkeySequence>,
) -> bool {
    let layout = state.active_hotkey_sequences.switch_layout;
    layout.is_some() && layout == sequence
}

fn handle_convert_last_word_hotkey(state: &mut AppState) {
    if last_word_hotkey_should_try_selection_first(state)
        && crate::domain::text::convert::convert_selection_if_any(state)
    {
        return;
    }

    crate::conversion::convert_last_word(state);
}

fn handle_smart_convert_last_word_hotkey(state: &mut AppState) {
    if smart_last_word_hotkey_should_try_selection_first(state)
        && crate::domain::text::convert::smart_convert_selection_if_any(state)
    {
        return;
    }

    crate::conversion::smart_convert_last_word(state);
}

fn handle_switch_layout_hotkey(state: &mut AppState) {
    if layout_hotkey_shares_sequence_with(state, state.active_hotkey_sequences.selection)
        && crate::domain::text::convert::convert_selection_if_any(state)
    {
        return;
    }

    if layout_hotkey_shares_sequence_with(state, state.active_hotkey_sequences.last_word)
        && crate::conversion::convert_last_word_if_any(state)
    {
        return;
    }

    let _ = switch_keyboard_layout();
}

fn execute_runtime_command(hwnd: HWND, state: &mut AppState, command: RuntimeCommand) {
    match command {
        RuntimeCommand::AutoconvertLastWord => {
            if state.autoconvert_feature_enabled && state.autoconvert_enabled {
                autoconvert_last_word(state);
            }
        }
        RuntimeCommand::Hotkey(action) => match action {
            HotkeyAction::PauseToggle => {
                if !state.autoconvert_feature_enabled {
                    return;
                }
                tracing::warn!(msg = "autoconvert_toggle", source = "hotkey_pause_toggle");
                handle_pause_toggle(hwnd, state);
            }
            HotkeyAction::ConvertLastWord => handle_convert_last_word_hotkey(state),
            HotkeyAction::ConvertLastSequence => crate::conversion::convert_last_sequence(state),
            HotkeyAction::ConvertSelection => crate::conversion::convert_selection(state),
            HotkeyAction::SmartConvertLastWord => handle_smart_convert_last_word_hotkey(state),
            HotkeyAction::SmartConvertLastSequence => {
                crate::conversion::smart_convert_last_sequence(state);
            }
            HotkeyAction::SmartConvertSelection => {
                crate::conversion::smart_convert_selection(state)
            }
            HotkeyAction::SwitchLayout => handle_switch_layout_hotkey(state),
        },
    }
}

fn enqueue_runtime_command(hwnd: HWND, state: &mut AppState, command: RuntimeCommand) {
    state.pending_runtime_commands.push_back(command);

    if state.runtime_command_running || state.pending_runtime_commands.len() != 1 {
        return;
    }

    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_RUN_RUNTIME_COMMAND, WPARAM(0), LPARAM(0));
    }
}

fn process_next_runtime_command(hwnd: HWND, state: &mut AppState) {
    if state.runtime_command_running {
        return;
    }

    let Some(command) = state.pending_runtime_commands.pop_front() else {
        return;
    };

    state.runtime_command_running = true;
    execute_runtime_command(hwnd, state, command);
    state.runtime_command_running = false;

    if !state.pending_runtime_commands.is_empty() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_RUN_RUNTIME_COMMAND, WPARAM(0), LPARAM(0));
        }
    }
}

#[cfg(test)]
pub(crate) fn hotkey_action_from_wparam(wparam: WPARAM) -> Option<HotkeyAction> {
    let id = hotkey_id_from_wparam(wparam);
    action_from_id(id)
}

#[cfg(not(test))]
fn hotkey_action_from_wparam(wparam: WPARAM) -> Option<HotkeyAction> {
    let id = hotkey_id_from_wparam(wparam);
    action_from_id(id)
}

fn on_hotkey(hwnd: HWND, wparam: WPARAM) -> LRESULT {
    let _id = hotkey_id_from_wparam(wparam);

    #[cfg(debug_assertions)]
    crate::utils::helpers::debug_log(&format!("WM_HOTKEY id={_id}"));

    let Some(action) = hotkey_action_from_wparam(wparam) else {
        return LRESULT(0);
    };

    with_state_mut(hwnd, |state| {
        if action == HotkeyAction::PauseToggle && !state.autoconvert_feature_enabled {
            return;
        }
        enqueue_runtime_command(hwnd, state, RuntimeCommand::Hotkey(action));
    });

    LRESULT(0)
}

/// Handles `WM_APP_ERROR` by draining and presenting a single queued UI error.
///
/// Returns `LRESULT(0)` to satisfy the window procedure contract.
fn on_app_error(hwnd: HWND) -> LRESULT {
    with_state_mut_do(hwnd, |state| {
        drain_one_and_present(hwnd, state);
    });

    LRESULT(0)
}

fn on_timer(hwnd: HWND, wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
    if wparam.0 == PLAYGROUND_HIDE_TIMER_ID {
        with_state_mut_do(hwnd, |state| hide_playground(hwnd, state));
        return LRESULT(0);
    }

    if wparam.0 == crate::platform::win::keyboard::sequence::DEFERRED_SEQUENCE_TIMER_ID {
        with_state_mut_do(hwnd, |state| {
            if let Err(e) = crate::platform::win::keyboard::sequence::handle_deferred_sequence_timer(
                hwnd, state,
            ) {
                crate::platform::ui::error_notifier::push(
                    hwnd,
                    state,
                    crate::platform::ui::error_notifier::T_UI,
                    "Hotkey handling failed",
                    &e,
                );
            }
        });
        return LRESULT(0);
    }

    let _ = handle_tray_timer(hwnd, wparam);
    #[cfg(debug_assertions)]
    let _ = handle_timer(hwnd, wparam.0);
    LRESULT(0)
}

fn toggle_window_visibility_from_tray(hwnd: HWND) {
    unsafe {
        let visible = IsWindowVisible(hwnd).as_bool();
        if visible {
            with_state_mut_do(hwnd, |state| hide_playground(hwnd, state));
            let _ = ShowWindow(hwnd, SW_HIDE);
        } else {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn set_autoconvert_enabled_from_tray(
    hwnd: HWND,
    state: &mut crate::app::AppState,
    enabled: bool,
    show_balloon: bool,
) {
    if !state.autoconvert_feature_enabled {
        return;
    }
    if state.autoconvert_enabled == enabled {
        return;
    }

    state.autoconvert_enabled = enabled;

    if let Err(e) = crate::platform::win::tray::switch_tray_icon(hwnd, enabled) {
        tracing::warn!(error = ?e, "switch_tray_icon failed");
    }

    refresh_tray_tooltip(hwnd, state);

    if !show_balloon {
        return;
    }

    let hotkey_text = tray_toggle_hotkey_text(state);

    let body = if enabled {
        format!("Status: active.\nAuto convert: ON.\nToggle: {hotkey_text}")
    } else {
        format!("Status: paused.\nAuto convert: OFF.\nToggle: {hotkey_text}")
    };

    if let Err(e) = crate::platform::win::tray::balloon_info(hwnd, "Rust Switcher", &body) {
        tracing::warn!(error = ?e, "tray balloon failed");
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::*;
    use crate::{app::HotkeySequenceValues, config};

    fn seq(vk: u32) -> config::HotkeySequence {
        config::HotkeySequence {
            first: config::HotkeyChord {
                mods: 0,
                mods_vks: 0,
                vk: Some(vk),
            },
            second: None,
            third: None,
            max_gap_ms: 250,
        }
    }

    #[test]
    fn last_word_hotkey_tries_selection_first_when_sequences_match() {
        let state = AppState {
            active_hotkey_sequences: HotkeySequenceValues {
                last_word: Some(seq(u32::from(b'A'))),
                selection: Some(seq(u32::from(b'A'))),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(last_word_hotkey_should_try_selection_first(&state));
    }

    #[test]
    fn last_word_hotkey_does_not_try_selection_when_sequences_differ() {
        let state = AppState {
            active_hotkey_sequences: HotkeySequenceValues {
                last_word: Some(seq(u32::from(b'A'))),
                selection: Some(seq(u32::from(b'B'))),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(!last_word_hotkey_should_try_selection_first(&state));
    }

    #[test]
    fn stop_hotkey_capture_clears_active_capture_state() {
        let mut state = AppState::default();
        state
            .hotkey_capture
            .start(crate::app::HotkeySlot::LastSequence);
        assert!(state.hotkey_capture.active);

        stop_hotkey_capture_ui(HWND::default(), &mut state);

        assert!(!state.hotkey_capture.active);
        assert_eq!(state.hotkey_capture.slot, None);
        assert_eq!(state.hotkey_capture.pending_mods, 0);
        assert_eq!(state.hotkey_capture.pending_mods_vks, 0);
    }

    #[test]
    fn hotkey_capture_focus_fallback_prefers_apply_button() {
        let mut state = AppState::default();
        state.buttons.apply = HWND(ptr::dangling_mut());

        let target = hotkey_capture_focus_fallback(HWND(ptr::dangling_mut()), &state);
        assert_eq!(target, state.buttons.apply);
    }

    #[test]
    fn hotkey_capture_focus_fallback_uses_window_when_apply_missing() {
        let state = AppState::default();
        let hwnd = HWND(ptr::dangling_mut());

        let target = hotkey_capture_focus_fallback(hwnd, &state);
        assert_eq!(target, hwnd);
    }

    #[test]
    fn enqueue_runtime_command_records_fifo_series() {
        let mut state = AppState::default();

        enqueue_runtime_command(
            HWND::default(),
            &mut state,
            RuntimeCommand::Hotkey(HotkeyAction::ConvertLastSequence),
        );
        enqueue_runtime_command(
            HWND::default(),
            &mut state,
            RuntimeCommand::AutoconvertLastWord,
        );
        enqueue_runtime_command(
            HWND::default(),
            &mut state,
            RuntimeCommand::Hotkey(HotkeyAction::ConvertLastWord),
        );

        assert_eq!(
            state
                .pending_runtime_commands
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![
                RuntimeCommand::Hotkey(HotkeyAction::ConvertLastSequence),
                RuntimeCommand::AutoconvertLastWord,
                RuntimeCommand::Hotkey(HotkeyAction::ConvertLastWord),
            ]
        );
        assert!(!state.runtime_command_running);
    }

    #[test]
    fn enqueue_runtime_command_keeps_items_queued_while_running() {
        let mut state = AppState {
            runtime_command_running: true,
            ..Default::default()
        };

        enqueue_runtime_command(
            HWND::default(),
            &mut state,
            RuntimeCommand::Hotkey(HotkeyAction::SwitchLayout),
        );

        assert_eq!(
            state
                .pending_runtime_commands
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![RuntimeCommand::Hotkey(HotkeyAction::SwitchLayout)]
        );
        assert!(state.runtime_command_running);
    }
}
