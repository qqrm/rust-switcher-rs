use std::{ptr::null_mut, thread, time::Duration};

use mapping::{ConversionDirection, conversion_direction_for_text, convert_ru_en_with_direction};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, GetKeyboardLayout, GetKeyboardLayoutList, HKL,
            VK_LSHIFT, VK_RSHIFT,
        },
        WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId, PostMessageW, WM_INPUTLANGCHANGEREQUEST,
        },
    },
};

use super::{
    mapping,
    selection_probe::{CapturedSelection, SelectionProbe, probe_selection},
    smart,
};
use crate::{
    app::AppState,
    conversion::input::replace_selection_unicode,
};

const MAX_SELECTION_CHARS: usize = 512;

/// Converts the currently selected text, if there is any selection.
///
/// Returns `true` if a non empty eligible selection was found (conversion attempted),
/// otherwise `false`.
#[allow(dead_code)]
#[tracing::instrument(level = "trace", skip(state))]
pub fn convert_selection_if_any(state: &mut AppState) -> bool {
    convert_selection_if_any_impl(state, SelectionDirectionMode::TextThenForeground)
}

pub fn smart_convert_selection_if_any(state: &mut AppState) -> bool {
    convert_selection_if_any_impl(state, SelectionDirectionMode::TextOnly)
}

fn convert_selection_if_any_impl(state: &mut AppState, mode: SelectionDirectionMode) -> bool {
    match convert_selection_outcome(state, MAX_SELECTION_CHARS, mode) {
        ConvertOutcome::Noop => false,
        ConvertOutcome::Ok | ConvertOutcome::Cancelled => true,
        ConvertOutcome::Err(e) => {
            tracing::warn!(user_text = e.user_text(), error = ?e, "selection conversion failed");
            true
        }
    }
}

pub fn convert_selection(state: &mut AppState) {
    convert_selection_impl(state, SelectionDirectionMode::TextThenForeground);
}

pub fn smart_convert_selection(state: &mut AppState) {
    convert_selection_impl(state, SelectionDirectionMode::TextOnly);
}

fn convert_selection_impl(state: &mut AppState, mode: SelectionDirectionMode) {
    tracing::trace!("convert_selection called");
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        tracing::warn!("foreground window is null");
        return;
    }

    if !wait_shift_released(150) {
        tracing::info!("wait_shift_released returned false");
        return;
    }

    match convert_selection_outcome(state, MAX_SELECTION_CHARS, mode) {
        ConvertOutcome::Noop => tracing::trace!("no selection"),
        ConvertOutcome::Ok | ConvertOutcome::Cancelled => {}
        ConvertOutcome::Err(e) => {
            tracing::warn!(user_text = e.user_text(), error = ?e, "selection conversion failed");
        }
    }
}

#[derive(Copy, Clone, Debug)]
enum SelectionDirectionMode {
    TextThenForeground,
    TextOnly,
}

/// High level outcome of a conversion attempt.
///
/// This is designed for UI boundary code to decide whether to notify the user.
#[derive(Debug)]
enum ConvertOutcome {
    Noop,
    Cancelled,
    Ok,
    Err(ConvertSelectionError),
}

/// Attempts to convert selection and returns a high level outcome.
///
/// This function does not perform UI safety checks.
fn convert_selection_outcome(
    state: &mut AppState,
    max_chars: usize,
    mode: SelectionDirectionMode,
) -> ConvertOutcome {
    let selection = match probe_selection(max_chars) {
        SelectionProbe::Absent => return ConvertOutcome::Noop,
        // A busy or unreadable target must not fall through to last-word deletion.
        SelectionProbe::Blocked => return ConvertOutcome::Cancelled,
        SelectionProbe::Selected(selection) => selection,
    };

    match convert_selection_from_text(state, &selection, mode) {
        Ok(()) => ConvertOutcome::Ok,
        Err(e) => ConvertOutcome::Err(e),
    }
}

/// Errors that can occur while replacing the current selection with converted text.
#[derive(Debug)]
enum ConvertSelectionError {
    TargetChanged,
    InputRejected,
}

impl ConvertSelectionError {
    fn user_text(&self) -> &'static str {
        match self {
            Self::TargetChanged => "Selection changed before conversion",
            Self::InputRejected => "Selection input was rejected",
        }
    }
}

/// Replaces currently selected text with layout converted text.
///
/// Returns `Ok(())` when the target and selection were revalidated and Windows
/// accepted the complete replacement/reselection batch.
///
/// Keyboard layout switching is best effort and does not affect the result.
fn convert_selection_from_text(
    _state: &mut AppState,
    selection: &CapturedSelection,
    mode: SelectionDirectionMode,
) -> Result<(), ConvertSelectionError> {
    let text = &selection.text;
    if matches!(mode, SelectionDirectionMode::TextOnly) && smart::text_looks_correct(text) {
        tracing::trace!(%text, "smart selection convert skipped: text already looks correct");
        return Ok(());
    }

    let direction = match mode {
        SelectionDirectionMode::TextThenForeground => conversion_direction_for_text(text)
            .or_else(expected_direction_for_foreground_window)
            .unwrap_or(ConversionDirection::RuToEn),
        SelectionDirectionMode::TextOnly => {
            conversion_direction_for_text(text).unwrap_or(ConversionDirection::RuToEn)
        }
    };
    let converted = convert_ru_en_with_direction(text, direction);

    thread::sleep(Duration::from_millis(u64::from(
        crate::config::CONVERSION_DELAY_MS,
    )));

    if !selection.is_current() {
        return Err(ConvertSelectionError::TargetChanged);
    }

    // One SendInput call keeps Delete, insertion and reselection together.
    // Recheck cheap window identity immediately before submitting that batch.
    if !replace_selection_unicode(&converted, || selection.target.is_current()) {
        return Err(ConvertSelectionError::InputRejected);
    }

    if selection.target.is_current() {
        let fg = selection.target.foreground_hwnd();
        let layouts = installed_layouts();
        if !layouts.is_empty() {
            let _ = post_layout_change(fg, next_layout(&layouts, current_layout_for_window(fg)));
        }
    }
    Ok(())
}

/// Returns the current foreground window, or `None` if it is null.
fn foreground_window() -> Option<HWND> {
    let fg = unsafe { GetForegroundWindow() };
    (!fg.0.is_null()).then_some(fg)
}

/// Returns the current keyboard layout for the thread owning `fg`.
fn current_layout_for_window(fg: HWND) -> HKL {
    unsafe {
        let tid = GetWindowThreadProcessId(fg, None);
        GetKeyboardLayout(tid)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LayoutLanguage {
    English,
    Russian,
}

const LANG_ID_PRIMARY_MASK: u16 = 0x03ff;
const LANG_ENGLISH: u16 = 0x0009;
const LANG_RUSSIAN: u16 = 0x0019;

fn lang_id_from_hkl(hkl: HKL) -> u16 {
    (hkl.0 as usize & 0xffff) as u16
}

fn primary_lang_id(lang_id: u16) -> u16 {
    lang_id & LANG_ID_PRIMARY_MASK
}

/// Returns the active language of the layout (based on LANGID in HKL).
fn active_layout_language(hkl: HKL) -> Option<LayoutLanguage> {
    match primary_lang_id(lang_id_from_hkl(hkl)) {
        LANG_RUSSIAN => Some(LayoutLanguage::Russian),
        LANG_ENGLISH => Some(LayoutLanguage::English),
        _ => None,
    }
}

/// Returns the expected conversion direction for a given keyboard layout.
fn expected_direction_for_layout(hkl: HKL) -> Option<ConversionDirection> {
    match active_layout_language(hkl) {
        Some(LayoutLanguage::Russian) => Some(ConversionDirection::RuToEn),
        Some(LayoutLanguage::English) => Some(ConversionDirection::EnToRu),
        None => None,
    }
}

pub(crate) fn expected_direction_for_foreground_window() -> Option<ConversionDirection> {
    let fg = foreground_window()?;
    let layout = current_layout_for_window(fg);
    expected_direction_for_layout(layout)
}

/// Enumerates installed keyboard layouts for the current desktop.
///
/// Returns an empty vector when enumeration fails or yields no results.
fn installed_layouts() -> Vec<HKL> {
    let n = unsafe { GetKeyboardLayoutList(None) };
    let Ok(layout_count) = usize::try_from(n) else {
        return Vec::new();
    };
    if layout_count == 0 {
        return Vec::new();
    }

    let mut layouts = vec![HKL(null_mut()); layout_count];

    let n2 = unsafe { GetKeyboardLayoutList(Some(layouts.as_mut_slice())) };
    let Ok(filled) = usize::try_from(n2) else {
        return Vec::new();
    };
    if filled == 0 {
        return Vec::new();
    }

    layouts.truncate(filled);
    layouts
}

/// Switches the keyboard layout for the current foreground window to the next installed layout.
///
/// Algorithm:
/// - obtains the foreground window
/// - reads the current layout for that window thread
/// - enumerates installed layouts
/// - selects the next one cyclically
/// - posts `WM_INPUTLANGCHANGEREQUEST` to the window
///
/// Returns `Ok(())` when the operation is completed or skipped:
/// - no foreground window
/// - no layouts available
///
/// Returns `Err` only if posting the request fails.
pub fn switch_keyboard_layout() -> windows::core::Result<()> {
    let Some(fg) = foreground_window() else {
        return Ok(());
    };

    let cur = current_layout_for_window(fg);
    let layouts = installed_layouts();
    if layouts.is_empty() {
        return Ok(());
    }

    let next = next_layout(&layouts, cur);
    post_layout_change(fg, next)
}

/// Returns the next layout in `layouts` after `cur`, cycling back to the first.
///
/// If `cur` is not found, returns `cur`.
fn next_layout(layouts: &[HKL], cur: HKL) -> HKL {
    layouts
        .iter()
        .position(|&h| h == cur)
        .and_then(|i| layouts.get((i + 1) % layouts.len()).copied())
        .unwrap_or(cur)
}

/// Posts a layout change request message to the foreground window.
///
/// Uses `WM_INPUTLANGCHANGEREQUEST`. The `hkl` is passed through `LPARAM`.
fn post_layout_change(fg: HWND, hkl: HKL) -> windows::core::Result<()> {
    unsafe {
        PostMessageW(
            Some(fg),
            WM_INPUTLANGCHANGEREQUEST,
            WPARAM(0),
            LPARAM(hkl.0 as isize),
        )?;
    }
    Ok(())
}

/// Waits until both left and right Shift keys are released or the timeout elapses.
///
/// Returns `true` as soon as neither Shift key is currently pressed.
pub fn wait_shift_released(timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);

    while std::time::Instant::now() < deadline {
        let l = unsafe { GetAsyncKeyState(i32::from(VK_LSHIFT.0)) }.cast_unsigned();
        let r = unsafe { GetAsyncKeyState(i32::from(VK_RSHIFT.0)) }.cast_unsigned();

        let released = (l & 0x8000) == 0 && (r & 0x8000) == 0;
        if released {
            return true;
        }

        thread::sleep(Duration::from_millis(1));
    }

    false
}

/// Checks whether text is eligible for selection conversion.
///
/// Optimization:
/// `s.chars().nth(max_chars).is_none()` stops early for long strings, unlike `chars().count()`.
pub(super) fn is_convertible_selection(s: &str, max_chars: usize) -> bool {
    !s.is_empty() && !s.contains('\n') && !s.contains('\r') && s.chars().nth(max_chars).is_none()
}
