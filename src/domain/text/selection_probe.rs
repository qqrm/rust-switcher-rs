use std::{sync::OnceLock, time::Duration};

use windows::{
    Win32::{
        Foundation::{E_NOINTERFACE, HWND, LPARAM, WPARAM},
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize},
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
        },
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
                IUIAutomationTextRange, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
                UIA_E_NOTSUPPORTED, UIA_TextPatternId,
            },
            Controls::EM_GETSEL,
            WindowsAndMessaging::{
                GUITHREADINFO, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
                GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW,
                WM_GETTEXT, WM_GETTEXTLENGTH,
            },
        },
    },
};
use crate::utils::bounded_worker::BoundedWorker;

const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const MESSAGE_TIMEOUT_MS: u32 = 50;
const MAX_WINDOW_TEXT_UNITS: usize = 1_048_576;

/// Plain handle values may cross the worker boundary; COM objects never do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FocusTarget {
    foreground: usize,
    focus: usize,
    thread: u32,
    process: u32,
}

impl FocusTarget {
    pub(super) fn capture() -> Option<Self> {
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.0.is_null() { return None; }
        let thread = unsafe { GetWindowThreadProcessId(foreground, None) };
        if thread == 0 { return None; }
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(thread, &mut info) }.is_err() || info.hwndFocus.0.is_null() {
            return None;
        }
        let mut process = 0;
        let focus_thread = unsafe { GetWindowThreadProcessId(info.hwndFocus, Some(&mut process)) };
        if process == 0 || focus_thread == 0 { return None; }
        Some(Self {
            foreground: foreground.0 as usize, focus: info.hwndFocus.0 as usize,
            thread: focus_thread, process,
        })
    }

    pub(super) fn is_current(self) -> bool { Self::capture() == Some(self) }

    pub(super) fn foreground_hwnd(self) -> HWND { HWND(self.foreground as *mut _) }

    fn focus_hwnd(self) -> HWND { HWND(self.focus as *mut _) }

    fn is_own_native_edit(self) -> bool {
        self.process == unsafe { GetCurrentProcessId() }
            && self.thread == unsafe { GetCurrentThreadId() }
            && is_native_edit(self.focus_hwnd())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectionIdentity {
    Native { start: u32, end: u32 },
    Automation(u64),
}

#[derive(Clone, Debug)]
pub(super) struct CapturedSelection {
    pub(super) text: String,
    pub(super) target: FocusTarget,
    identity: SelectionIdentity,
    max_chars: usize,
}

impl CapturedSelection {
    /// Validate both target identity and the exact selection after the conversion delay.
    pub(super) fn is_current(&self) -> bool {
        if !self.target.is_current() { return false; }
        if self.target.is_own_native_edit() {
            return same_native_selection(self);
        }
        matches!(request(Command::Validate(self.clone())), Some(Response::Valid(true)))
            && self.target.is_current()
    }
}

#[derive(Debug)]
pub(super) enum SelectionProbe {
    Absent,
    Selected(CapturedSelection),
    Blocked,
}

enum Command {
    Capture(FocusTarget, usize),
    Validate(CapturedSelection),
}

enum Response {
    Probe(SelectionProbe),
    Valid(bool),
}

struct AutomationSnapshot {
    generation: u64,
    element: IUIAutomationElement,
    range: IUIAutomationTextRange,
}

#[derive(Default)]
struct ProbeState {
    generation: u64,
    automation: Option<IUIAutomation>,
    snapshot: Option<AutomationSnapshot>,
}

struct ComGuard(bool);
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 { unsafe { CoUninitialize() }; }
    }
}

// Fields drop in declaration order: release all COM interfaces before the apartment.
struct ProbeWorker {
    state: ProbeState,
    apartment: ComGuard,
}

impl ProbeWorker {
    fn process(&mut self, command: Command) -> Response {
        match command {
            Command::Capture(target, max_chars) if self.apartment.0 => {
                Response::Probe(self.state.capture(target, max_chars))
            }
            Command::Validate(selection) if self.apartment.0 => {
                Response::Valid(self.state.validate(&selection))
            }
            Command::Capture(..) => Response::Probe(SelectionProbe::Blocked),
            Command::Validate(..) => Response::Valid(false),
        }
    }
}

fn request(command: Command) -> Option<Response> {
    static WORKER: OnceLock<Option<BoundedWorker<Command, Response>>> = OnceLock::new();
    WORKER.get_or_init(|| {
        BoundedWorker::start("selection-probe", || {
            let mut worker = ProbeWorker {
                state: ProbeState::default(),
                apartment: ComGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok()),
            };
            move |command| worker.process(command)
        }).ok()
    }).as_ref()?.request(command, PROBE_TIMEOUT)
}

pub(super) fn probe_selection(max_chars: usize) -> SelectionProbe {
    let Some(target) = FocusTarget::capture() else { return SelectionProbe::Blocked; };
    // Our known standard Edit is on this UI thread. Calling it directly avoids
    // waiting for a worker which would in turn wait for this same message loop.
    if target.process == unsafe { GetCurrentProcessId() } {
        return if target.is_own_native_edit() {
            probe_native(target, max_chars)
        } else {
            // Same-process messages do not get OS pointer marshalling. Never
            // time out a call whose other thread could still use our buffers.
            SelectionProbe::Blocked
        };
    }
    match request(Command::Capture(target, max_chars)) {
        Some(Response::Probe(result)) if target.is_current() => result,
        _ => SelectionProbe::Blocked,
    }
}

impl ProbeState {
    fn capture(&mut self, target: FocusTarget, max_chars: usize) -> SelectionProbe {
        self.snapshot = None;
        if !target.is_current() { return SelectionProbe::Blocked; }
        if is_native_edit(target.focus_hwnd()) {
            return probe_native(target, max_chars);
        }
        self.capture_automation(target, max_chars).unwrap_or(SelectionProbe::Blocked)
    }

    fn capture_automation(&mut self, target: FocusTarget, max_chars: usize) -> windows::core::Result<SelectionProbe> {
        if self.automation.is_none() {
            self.automation = Some(unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?);
        }
        let Some(automation) = self.automation.as_ref() else { return Ok(SelectionProbe::Blocked); };
        let element = unsafe { automation.GetFocusedElement() }?;
        let pattern: IUIAutomationTextPattern = match unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) } {
            Ok(pattern) => pattern,
            // Preserve fallback for an explicitly unsupported pattern only.
            // Provider failures must never authorize last-word deletion.
            Err(error) => return Ok(pattern_failure(error.code())),
        };
        let ranges = unsafe { pattern.GetSelection() }?;
        let count = unsafe { ranges.Length() }?;
        if count == 0 { return Ok(SelectionProbe::Absent); }
        if count != 1 { return Ok(SelectionProbe::Blocked); }
        let range = unsafe { ranges.GetElement(0) }?;
        let text = read_automation_text(&range, max_chars)?;
        if text.is_empty() { return Ok(SelectionProbe::Absent); }
        if !super::convert::is_convertible_selection(&text, max_chars) || !target.is_current() {
            return Ok(SelectionProbe::Blocked);
        }
        self.generation = self.generation.wrapping_add(1);
        self.snapshot = Some(AutomationSnapshot { generation: self.generation, element, range });
        Ok(SelectionProbe::Selected(CapturedSelection {
            text, target, identity: SelectionIdentity::Automation(self.generation), max_chars,
        }))
    }

    fn validate(&self, selection: &CapturedSelection) -> bool {
        if !selection.target.is_current() { return false; }
        if matches!(selection.identity, SelectionIdentity::Native { .. }) {
            return same_native_selection(selection);
        }
        self.validate_automation(selection).unwrap_or(false) && selection.target.is_current()
    }

    fn validate_automation(&self, selection: &CapturedSelection) -> windows::core::Result<bool> {
        let (Some(automation), Some(snapshot)) = (&self.automation, &self.snapshot) else {
            return Ok(false);
        };
        if selection.identity != SelectionIdentity::Automation(snapshot.generation) { return Ok(false); }
        let element = unsafe { automation.GetFocusedElement() }?;
        if !unsafe { automation.CompareElements(&snapshot.element, &element) }?.as_bool() {
            return Ok(false);
        }
        let pattern: IUIAutomationTextPattern = unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) }?;
        let ranges = unsafe { pattern.GetSelection() }?;
        if unsafe { ranges.Length() }? != 1 { return Ok(false); }
        let range = unsafe { ranges.GetElement(0) }?;
        let same_start = unsafe { snapshot.range.CompareEndpoints(
            TextPatternRangeEndpoint_Start, &range, TextPatternRangeEndpoint_Start,
        ) }? == 0;
        let same_end = unsafe { snapshot.range.CompareEndpoints(
            TextPatternRangeEndpoint_End, &range, TextPatternRangeEndpoint_End,
        ) }? == 0;
        Ok(same_start && same_end && read_automation_text(&range, selection.max_chars)? == selection.text)
    }
}

fn pattern_failure(code: windows::core::HRESULT) -> SelectionProbe {
    if code == E_NOINTERFACE || code.0 as u32 == UIA_E_NOTSUPPORTED {
        SelectionProbe::Absent
    } else {
        SelectionProbe::Blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{E_ACCESSDENIED, E_FAIL};

    #[test]
    fn provider_errors_do_not_authorize_last_word_fallback() {
        for error in [E_ACCESSDENIED, E_FAIL] {
            assert!(matches!(pattern_failure(error), SelectionProbe::Blocked));
        }
        assert!(matches!(pattern_failure(E_NOINTERFACE), SelectionProbe::Absent));
    }
}

fn read_automation_text(range: &IUIAutomationTextRange, max_chars: usize) -> windows::core::Result<String> {
    // UIA counts UTF-16 units; two units per scalar plus a sentinel prevents
    // accepting a truncated supplementary-character selection as complete.
    let limit = max_chars.saturating_mul(2).saturating_add(1);
    let limit = i32::try_from(limit).unwrap_or(i32::MAX);
    Ok(unsafe { range.GetText(limit) }?.to_string())
}

fn same_native_selection(selection: &CapturedSelection) -> bool {
    match probe_native(selection.target, selection.max_chars) {
        SelectionProbe::Selected(current) => current.identity == selection.identity
            && current.text == selection.text && selection.target.is_current(),
        _ => false,
    }
}

fn probe_native(target: FocusTarget, max_chars: usize) -> SelectionProbe {
    if !target.is_current() { return SelectionProbe::Blocked; }
    let hwnd = target.focus_hwnd();
    let mut start = 0u32;
    let mut end = 0u32;
    // System messages below WM_USER marshal these pointers across processes.
    if unsafe { bounded_message(hwnd, EM_GETSEL,
        WPARAM((&raw mut start) as usize), LPARAM((&raw mut end) as isize)) }.is_none() {
        return SelectionProbe::Blocked;
    }
    if start == end { return SelectionProbe::Absent; }
    let Some(length) = end.checked_sub(start) else { return SelectionProbe::Blocked; };
    if length as usize > max_chars.saturating_mul(2) { return SelectionProbe::Blocked; }
    let Some(text) = read_window_text(hwnd) else { return SelectionProbe::Blocked; };
    let Some(selected) = text.get(start as usize..end as usize) else { return SelectionProbe::Blocked; };
    let Ok(selected) = String::from_utf16(selected) else { return SelectionProbe::Blocked; };
    if !super::convert::is_convertible_selection(&selected, max_chars) || !target.is_current() {
        return SelectionProbe::Blocked;
    }
    SelectionProbe::Selected(CapturedSelection {
        text: selected, target, identity: SelectionIdentity::Native { start, end }, max_chars,
    })
}

fn is_native_edit(hwnd: HWND) -> bool {
    let mut class_name = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut class_name) };
    if len <= 0 { return false; }
    let class = String::from_utf16_lossy(&class_name[..len as usize]);
    class == "Edit" || class.starts_with("RichEdit") || class.starts_with("RICHEDIT")
}

fn read_window_text(hwnd: HWND) -> Option<Vec<u16>> {
    let length = unsafe { bounded_message(hwnd, WM_GETTEXTLENGTH, WPARAM(0), LPARAM(0)) }?;
    if length > MAX_WINDOW_TEXT_UNITS { return None; }
    let capacity = length.checked_add(1)?;
    let mut text = vec![0u16; capacity];
    let copied = unsafe { bounded_message(hwnd, WM_GETTEXT, WPARAM(capacity), LPARAM(text.as_mut_ptr() as isize)) }?;
    if copied > length { return None; }
    text.truncate(copied);
    Some(text)
}

unsafe fn bounded_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<usize> {
    let mut result = 0usize;
    let status = unsafe { SendMessageTimeoutW(
        hwnd, message, wparam, lparam, SMTO_ABORTIFHUNG | SMTO_BLOCK,
        MESSAGE_TIMEOUT_MS, Some(&raw mut result),
    ) };
    (status.0 != 0).then_some(result)
}
