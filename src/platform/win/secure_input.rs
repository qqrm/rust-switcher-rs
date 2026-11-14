//! Password-field capture guard. Foreign UI Automation providers are queried
//! only on a persistent MTA worker, never on either low-level input hook.
//!
//! The capture coordinator owns a bounded FIFO of event-time key snapshots.
//! Only a fresh, explicit non-password result can authorize decoding. A hung
//! provider receives handles and epochs, not queued key data.
use std::{
    collections::HashMap,
    sync::{
        OnceLock,
        atomic::{AtomicIsize, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{GetLastError, HWND, LPARAM, SetLastError, WIN32_ERROR, WPARAM},
    System::{
        Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        Variant::{VARIANT, VT_BOOL, VariantClear},
    },
    UI::{
        Accessibility::{
            CUIAutomation, HWINEVENTHOOK, IUIAutomation, IUIAutomationElement, SetWinEventHook,
            UIA_IsPasswordPropertyId,
        },
        WindowsAndMessaging::{
            ES_PASSWORD, EVENT_OBJECT_FOCUS, EVENT_SYSTEM_FOREGROUND, GUITHREADINFO, GWL_STYLE,
            GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowLongPtrW,
            GetWindowThreadProcessId, IsChild, KBDLLHOOKSTRUCT, PostMessageW,
            WINEVENT_OUTOFCONTEXT,
        },
    },
};

use crate::{
    input::ring_buffer::{self, CapturedKey},
    utils::bounded_worker::BoundedWorker,
};

// A burst must fit the existing 101-character browser acceptance cases without
// blocking SendInput. Both memory and residence time remain strictly bounded.
const CAPTURE_QUEUE_CAPACITY: usize = 512;
const CAPTURE_LIFETIME: Duration = Duration::from_secs(2);
const PROVIDER_TIMEOUT: Duration = Duration::from_millis(75);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(500);
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static PENDING_CAPTURES: AtomicUsize = AtomicUsize::new(0);
static WORKER: OnceLock<Option<BoundedWorker<Command, Status>>> = OnceLock::new();
static FOCUS_HOOKS_READY: OnceLock<bool> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Secure,
    NotSecure,
    Unknown,
}

impl Status {
    pub(crate) const fn is_allowed(self) -> bool {
        matches!(self, Self::NotSecure)
    }
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct Target {
    foreground: usize,
    focus: usize,
    thread: u32,
    process: u32,
}

impl Target {
    fn capture() -> Option<Self> {
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.0.is_null() {
            return None;
        }
        let thread = unsafe { GetWindowThreadProcessId(foreground, None) };
        if thread == 0 {
            return None;
        }
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(thread, &mut info) }.is_err() || info.hwndFocus.0.is_null() {
            return None;
        }
        let mut process = 0;
        let thread = unsafe { GetWindowThreadProcessId(info.hwndFocus, Some(&mut process)) };
        if process == 0 || thread == 0 {
            return None;
        }
        Some(Self {
            foreground: foreground.0 as usize,
            focus: info.hwndFocus.0 as usize,
            thread,
            process,
        })
    }
    fn is_current(self) -> bool {
        Self::capture() == Some(self)
    }
    fn hwnd(self) -> HWND {
        HWND(self.focus as *mut _)
    }
}

struct Capture {
    key: CapturedKey,
    target: Target,
    generation: u64,
    captured: Instant,
}

impl Drop for Capture {
    fn drop(&mut self) {
        PENDING_CAPTURES.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy)]
struct Probe {
    target: Target,
    generation: u64,
}

enum Command {
    Capture(Box<Capture>),
    /// FIFO barrier: all earlier captures finish or are invalidated first.
    Protect(Probe),
    Refresh(Probe),
}

#[derive(Clone, Copy)]
struct Verdict {
    status: Status,
    element_changed: bool,
}

impl Verdict {
    const UNKNOWN: Self = Self {
        status: Status::Unknown,
        element_changed: false,
    };
}

struct ComGuard(bool);
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

// Release COM interfaces before uninitializing their owning MTA.
struct Provider {
    automation: Option<IUIAutomation>,
    // Identity only, never a cached password verdict. Bounded independently of
    // the text journal, so returning to a known editor preserves its cache.
    elements: HashMap<Target, IUIAutomationElement>,
    apartment: ComGuard,
}

impl Provider {
    fn new() -> Self {
        let apartment = ComGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok());
        let automation = if apartment.0 {
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.ok()
        } else {
            None
        };
        Self {
            automation,
            elements: HashMap::new(),
            apartment,
        }
    }

    fn probe(&mut self, request: Probe) -> Verdict {
        if !self.apartment.0
            || !validate_capture_generation(request.generation)
            || !request.target.is_current()
        {
            return Verdict::UNKNOWN;
        }
        let Some(automation) = &self.automation else {
            return Verdict::UNKNOWN;
        };
        let Ok(element) = (unsafe { automation.GetFocusedElement() }) else {
            return Verdict::UNKNOWN;
        };
        let element_changed = self.elements.get(&request.target).is_none_or(|previous| {
            !unsafe { automation.CompareElements(previous, &element) }
                .is_ok_and(|same| same.as_bool())
        });
        let status = element_status(&element);
        // A property lookup can itself race a same-HWND DOM focus change.
        let stable = unsafe { automation.GetFocusedElement() }
            .ok()
            .and_then(|current| unsafe { automation.CompareElements(&element, &current) }.ok())
            .is_some_and(|same| same.as_bool());
        if !stable
            || !validate_capture_generation(request.generation)
            || !request.target.is_current()
        {
            return Verdict::UNKNOWN;
        }
        if self.elements.len() >= 64 && !self.elements.contains_key(&request.target) {
            self.elements.clear();
        }
        self.elements.insert(request.target, element);
        Verdict {
            status,
            element_changed,
        }
    }
}

/// Explicitly request the property's non-default value. Unsupported providers
/// return a non-BOOL reserved value, not a trustworthy `false`.
/// Caller must be on a UIA worker, never on an input hook.
pub(crate) fn element_status(element: &IUIAutomationElement) -> Status {
    let Ok(mut value) =
        (unsafe { element.GetCurrentPropertyValueEx(UIA_IsPasswordPropertyId, true) })
    else {
        return Status::Unknown;
    };
    let status = variant_status(&value);
    let _ = unsafe { VariantClear(&raw mut value) };
    status
}

fn variant_status(value: &VARIANT) -> Status {
    let value = unsafe { &value.Anonymous.Anonymous };
    if value.vt != VT_BOOL {
        return Status::Unknown;
    }
    if unsafe { value.Anonymous.boolVal }.0 != 0 {
        Status::Secure
    } else {
        Status::NotSecure
    }
}

/// This fallback applies only to known standard Edit/RichEdit window classes.
pub(crate) fn win32_password_state(hwnd: HWND) -> Option<bool> {
    let mut class_name = [0u16; 128];
    let length = unsafe { GetClassNameW(hwnd, &mut class_name) };
    if length <= 0 {
        return None;
    }
    let class = String::from_utf16_lossy(&class_name[..length as usize]);
    if class != "Edit" && !class.starts_with("RichEdit") && !class.starts_with("RICHEDIT") {
        return None;
    }
    unsafe { SetLastError(WIN32_ERROR::default()) };
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    if style == 0 && unsafe { GetLastError() } != WIN32_ERROR::default() {
        return None;
    }
    Some(style & ES_PASSWORD as isize != 0)
}

fn native_status(target: Target) -> Option<Status> {
    win32_password_state(target.hwnd()).map(|password| {
        if password {
            Status::Secure
        } else {
            Status::NotSecure
        }
    })
}

fn capture_generation() -> u64 {
    ring_buffer::secure_input_generation()
}

fn validate_capture_generation(expected: u64) -> bool {
    ring_buffer::validate_secure_input_generation(expected)
}

fn reject_capture(expected: u64) {
    ring_buffer::reject_secure_capture(expected);
}

/// Token for in-flight conversion, selection and restoration operations.
pub(crate) fn generation() -> u64 {
    ring_buffer::secure_context_generation()
}

pub(crate) fn validate_generation(expected: u64) -> bool {
    ring_buffer::validate_secure_context_generation(expected)
}

pub(crate) fn clear_sensitive_context_if_current(expected: u64) {
    if validate_generation(expected) {
        ring_buffer::reject_secure_context(expected);
    }
}

fn enqueue_refresh() {
    if let (Some(Some(worker)), Some(target)) = (WORKER.get(), Target::capture()) {
        let _ = worker.submit(
            Command::Refresh(Probe {
                target,
                generation: capture_generation(),
            }),
            CAPTURE_LIFETIME,
        );
    }
}

/// Called for navigation, mouse changes and focus events, including events with
/// the same native HWND. No provider calls or waits occur here.
pub(crate) fn invalidate_context() {
    ring_buffer::invalidate_capture_context();
    enqueue_refresh();
}

pub(crate) fn invalidate_context_if_current(expected: u64) {
    if ring_buffer::invalidate_capture_context_if_current(expected) {
        enqueue_refresh();
    }
}

/// Confirmed sensitive fields and rejected text probes must purge every cache,
/// unlike ordinary mouse/navigation invalidation.
pub(crate) fn clear_sensitive_context() {
    ring_buffer::clear_secure_input_data();
}

unsafe extern "system" fn focus_changed(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let foreground = unsafe { GetForegroundWindow() };
    if event == EVENT_SYSTEM_FOREGROUND
        || hwnd == foreground
        || unsafe { IsChild(foreground, hwnd) }.as_bool()
    {
        ring_buffer::refresh_capture_focus(PENDING_CAPTURES.load(Ordering::Acquire) != 0);
        enqueue_refresh();
    }
}

/// Initialize before installing the keyboard hook. No lazy worker creation is
/// permitted on the hook path.
pub(crate) fn initialize(hwnd: HWND) {
    MAIN_HWND.store(hwnd.0 as isize, Ordering::Release);
    FOCUS_HOOKS_READY.get_or_init(|| {
        let foreground = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(focus_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        let focus = unsafe {
            SetWinEventHook(
                EVENT_OBJECT_FOCUS,
                EVENT_OBJECT_FOCUS,
                None,
                Some(focus_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        !foreground.0.is_null() && !focus.0.is_null()
    });
    WORKER.get_or_init(|| {
        let provider = BoundedWorker::start("password-property", || {
            let mut provider = Provider::new();
            move |request| provider.probe(request)
        })
        .ok();
        BoundedWorker::start_with_expiration(
            "password-capture",
            CAPTURE_QUEUE_CAPACITY,
            move || move |command| process_command(provider.as_ref(), command),
            |command| {
                if let Command::Capture(capture) = command {
                    reject_capture(capture.generation);
                }
            },
        )
        .ok()
    });
    enqueue_refresh();
}

fn verdict(provider: Option<&BoundedWorker<Probe, Verdict>>, request: Probe) -> Verdict {
    if !validate_capture_generation(request.generation) || !request.target.is_current() {
        return Verdict::UNKNOWN;
    }
    if let Some(status) = native_status(request.target) {
        return Verdict {
            status,
            element_changed: false,
        };
    }
    if FOCUS_HOOKS_READY.get() != Some(&true) {
        return Verdict::UNKNOWN;
    }
    provider
        .and_then(|worker| worker.request(request, PROVIDER_TIMEOUT))
        .unwrap_or(Verdict::UNKNOWN)
}

fn process_command(provider: Option<&BoundedWorker<Probe, Verdict>>, command: Command) -> Status {
    match command {
        Command::Capture(capture) => {
            if !capture_is_current(&capture) {
                return Status::Unknown;
            }
            let result = verdict(
                provider,
                Probe {
                    target: capture.target,
                    generation: capture.generation,
                },
            );
            if !capture_is_current(&capture) {
                return Status::Unknown;
            }
            if !result.status.is_allowed() || result.element_changed {
                reject_capture(capture.generation);
                return result.status;
            }
            let typed = ring_buffer::record_captured_keydown(&capture.key, capture.generation);
            if typed.is_some()
                && validate_capture_generation(capture.generation)
                && ring_buffer::last_char_triggers_autoconvert()
            {
                let hwnd = MAIN_HWND.load(Ordering::Acquire);
                if hwnd != 0 {
                    let _ = unsafe {
                        PostMessageW(
                            Some(HWND(hwnd as *mut _)),
                            crate::platform::ui::error_notifier::WM_APP_AUTOCONVERT,
                            WPARAM(0),
                            LPARAM(0),
                        )
                    };
                }
            }
            result.status
        }
        Command::Protect(request) => {
            let result = verdict(provider, request);
            if !validate_capture_generation(request.generation) || !request.target.is_current() {
                return Status::Unknown;
            }
            if !result.status.is_allowed() || result.element_changed {
                reject_capture(request.generation);
                return Status::Unknown;
            }
            result.status
        }
        Command::Refresh(request) => {
            let result = verdict(provider, request);
            if !validate_capture_generation(request.generation) || !request.target.is_current() {
                return Status::Unknown;
            }
            if !result.status.is_allowed() {
                reject_capture(request.generation);
            } else if result.element_changed {
                ring_buffer::discard_changed_capture_context(request.generation);
            }
            result.status
        }
    }
}

fn capture_is_current(capture: &Capture) -> bool {
    if !validate_capture_generation(capture.generation) {
        return false;
    }
    if capture.captured.elapsed() >= CAPTURE_LIFETIME
        || !capture.target.is_current()
        || !capture.key.is_current()
    {
        reject_capture(capture.generation);
        return false;
    }
    true
}

/// Nonblocking hook entry: preserve physical input and normal hotkey handling
/// even if the target's accessibility provider is unavailable or hung.
pub(crate) fn record_keydown(kb: &KBDLLHOOKSTRUCT, vk: u32) {
    // These actions can move focus inside one browser HWND or replace unknown
    // content. A queued observation from the preceding editor cannot survive.
    if matches!(
        vk,
        0x09 | 0x1B | 0x2D | 0x2E | 0x21 | 0x22 | 0x24 | 0x26 | 0x28
    ) {
        invalidate_context();
        return;
    }
    let Some(target) = Target::capture() else {
        reject_capture(capture_generation());
        return;
    };
    if native_status(target) == Some(Status::Secure) {
        clear_sensitive_context();
        return;
    }
    let expected = capture_generation();
    let Some(key) = ring_buffer::capture_keydown(kb, vk) else {
        return;
    };
    if !validate_capture_generation(expected) || !key.is_current() || !target.is_current() {
        return;
    }
    PENDING_CAPTURES.fetch_add(1, Ordering::AcqRel);
    let capture = Capture {
        key,
        target,
        generation: expected,
        captured: Instant::now(),
    };
    let submitted = WORKER
        .get()
        .and_then(Option::as_ref)
        .is_some_and(|worker| worker.submit(Command::Capture(Box::new(capture)), CAPTURE_LIFETIME));
    if !submitted {
        reject_capture(expected);
    }
}

/// Called only outside the hook. The FIFO barrier drains prior captures before
/// a fresh bounded probe authorizes any conversion or journal restoration.
pub(crate) fn protect_current_context() -> Status {
    let Some(target) = Target::capture() else {
        reject_capture(capture_generation());
        return Status::Unknown;
    };
    let expected = capture_generation();
    let status = WORKER
        .get()
        .and_then(Option::as_ref)
        .and_then(|worker| {
            worker.request(
                Command::Protect(Probe {
                    target,
                    generation: expected,
                }),
                COMMAND_TIMEOUT,
            )
        })
        .unwrap_or(Status::Unknown);
    if !validate_capture_generation(expected) || !target.is_current() {
        return Status::Unknown;
    }
    if !status.is_allowed() {
        reject_capture(expected);
    }
    status
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]
    use windows::{
        Win32::{
            Foundation::VARIANT_BOOL,
            System::Variant::{VARENUM, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_EMPTY},
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
            },
        },
        core::{PCWSTR, w},
    };

    use super::*;

    fn boolean_variant(value: bool, kind: VARENUM) -> VARIANT {
        VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                    vt: kind,
                    Anonymous: VARIANT_0_0_0 {
                        boolVal: VARIANT_BOOL(if value { -1 } else { 0 }),
                    },
                    ..Default::default()
                }),
            },
        }
    }

    #[test]
    fn only_explicit_boolean_false_authorizes_custom_input() {
        assert_eq!(
            variant_status(&boolean_variant(false, VT_BOOL)),
            Status::NotSecure
        );
        assert_eq!(
            variant_status(&boolean_variant(true, VT_BOOL)),
            Status::Secure
        );
        assert_eq!(
            variant_status(&boolean_variant(false, VT_EMPTY)),
            Status::Unknown
        );
        assert!(!Status::Unknown.is_allowed());
        assert!(!Status::Secure.is_allowed());
    }

    #[test]
    fn native_password_style_is_checked_without_reading_text() -> windows::core::Result<()> {
        for password in [false, true] {
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("Edit"),
                    PCWSTR::null(),
                    WINDOW_STYLE(if password {
                        ES_PASSWORD.cast_unsigned()
                    } else {
                        0
                    }),
                    0,
                    0,
                    100,
                    24,
                    None,
                    None,
                    None,
                    None,
                )
            }?;
            let result = win32_password_state(hwnd);
            unsafe { DestroyWindow(hwnd) }?;
            assert_eq!(result, Some(password));
        }
        Ok(())
    }

    #[test]
    fn late_denial_does_not_clear_a_new_capture_generation() {
        let _guard = ring_buffer::test_guard();
        let previous = capture_generation();
        ring_buffer::clear_secure_input_data();
        ring_buffer::push_text("new-context");
        reject_capture(previous);
        assert!(!ring_buffer::runs_snapshot().is_empty());
        ring_buffer::clear_secure_input_data();
    }
}
