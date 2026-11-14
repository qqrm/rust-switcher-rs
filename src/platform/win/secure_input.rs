//! Password-field capture guard. Foreign UI Automation providers are queried
//! only on a persistent MTA worker, never on either low-level input hook.
//!
//! The capture coordinator owns a bounded FIFO of event-time key snapshots.
//! Only a fresh, explicit non-password result can authorize decoding. A hung
//! provider receives handles and epochs, not queued key data.
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{GetLastError, HWND, LPARAM, POINT, SetLastError, WIN32_ERROR, WPARAM},
    System::{
        Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        Variant::{VARIANT, VT_BOOL, VariantClear},
    },
    UI::{
        Accessibility::{
            CUIAutomation, HWINEVENTHOOK, IUIAutomation, IUIAutomationElement,
            IUIAutomationFocusChangedEventHandler, IUIAutomationFocusChangedEventHandler_Impl,
            SetWinEventHook, UIA_IsKeyboardFocusablePropertyId, UIA_IsPasswordPropertyId,
            UIA_PROPERTY_ID,
        },
        Input::KeyboardAndMouse::VIRTUAL_KEY,
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
const POINTER_ANCESTOR_LIMIT: usize = 8;
const POINTER_FOCUS_ATTEMPTS: usize = 75;
const POINTER_FOCUS_INTERVAL: Duration = Duration::from_millis(1);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(500);
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
// At most the bounded capture queue plus its current consumer can own entries.
// Old queued work must not mark unrelated later native contexts incomplete.
static PENDING_CAPTURES: Mutex<BTreeMap<u64, usize>> = Mutex::new(BTreeMap::new());
static WORKER: OnceLock<Option<BoundedWorker<Command, Status>>> = OnceLock::new();
static FOCUS_HOOKS_READY: OnceLock<bool> = OnceLock::new();
// UIA callbacks run on another thread. Preserve snapshot-to-submission ordering
// against their focus refreshes without ever waiting on the keyboard hook.
static CAPTURE_DISPATCH: Mutex<()> = Mutex::new(());

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
    _pending: PendingCapture,
}

struct PendingCapture(u64);

impl PendingCapture {
    fn new(generation: u64) -> Self {
        let mut pending = PENDING_CAPTURES
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *pending.entry(generation).or_default() += 1;
        Self(generation)
    }
}

impl Drop for PendingCapture {
    fn drop(&mut self) {
        let mut pending = PENDING_CAPTURES
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(count) = pending.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                pending.remove(&self.0);
            }
        }
    }
}

fn pending_capture_for_generation(generation: u64) -> bool {
    PENDING_CAPTURES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .contains_key(&generation)
}

#[derive(Clone, Copy)]
struct Probe {
    target: Target,
    generation: u64,
}

#[derive(Clone, Copy)]
enum ProbeSource {
    Focused,
    Pointer(POINT),
}

struct ProviderRequest {
    probe: Probe,
    source: ProbeSource,
    deadline: Instant,
}

impl ProviderRequest {
    fn is_current(&self) -> bool {
        Instant::now() < self.deadline
            && validate_capture_generation(self.probe.generation)
            && self.probe.target.is_current()
    }
}

enum Command {
    Capture(Box<Capture>),
    /// FIFO barrier: all earlier captures finish or are invalidated first.
    Protect(Probe),
    Refresh(Probe),
    PointerRefresh {
        probe: Probe,
        point: POINT,
    },
}

#[derive(Clone, Copy)]
struct Verdict {
    status: Status,
    element_changed: bool,
    // A pointer provider may have waited while raw keys entered the FIFO. Those
    // snapshots must not inherit an identity established only after their input.
    pointer_anchor: bool,
}

impl Verdict {
    const UNKNOWN: Self = Self {
        status: Status::Unknown,
        element_changed: false,
        pointer_anchor: false,
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

// UIA focus events cover descendants that do not send native HWND focus
// changes (including browser DOM fields). The callback is a wake-up only: it
// neither authorizes pending keys nor calls a provider or changes the journal.
#[windows::core::implement(IUIAutomationFocusChangedEventHandler)]
struct FocusChangedHandler {
    enabled: Arc<AtomicBool>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl IUIAutomationFocusChangedEventHandler_Impl for FocusChangedHandler_Impl {
    fn HandleFocusChangedEvent(
        &self,
        _sender: windows::core::Ref<IUIAutomationElement>,
    ) -> windows::core::Result<()> {
        if self.enabled.load(Ordering::Acquire) {
            (self.notify)();
        }
        Ok(())
    }
}

// Release COM interfaces before uninitializing their owning MTA.
struct Provider {
    automation: Option<IUIAutomation>,
    // Identity only, never a cached password verdict. Bounded independently of
    // the text journal, so returning to a known editor preserves its cache.
    elements: HashMap<Target, IUIAutomationElement>,
    focus_handler: Option<IUIAutomationFocusChangedEventHandler>,
    focus_events_enabled: Arc<AtomicBool>,
    apartment: ComGuard,
}

impl Provider {
    fn new() -> Self {
        let apartment = ComGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok());
        let automation: Option<IUIAutomation> = if apartment.0 {
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.ok()
        } else {
            None
        };
        let focus_events_enabled = Arc::new(AtomicBool::new(true));
        let focus_handler = automation.as_ref().and_then(|automation| {
            let handler: IUIAutomationFocusChangedEventHandler = FocusChangedHandler {
                enabled: Arc::clone(&focus_events_enabled),
                notify: Box::new(|| {
                    let queued = with_uia_focus_dispatch(enqueue_refresh_for_generation);
                    tracing::debug!(queued, "secure input: UIA focus notification");
                }),
            }
            .into();
            // Subscription and removal both belong to this persistent MTA.
            // A provider that stalls here cannot block either input hook.
            match unsafe { automation.AddFocusChangedEventHandler(None, &handler) } {
                Ok(()) => {
                    tracing::debug!("secure input: UIA focus subscription installed");
                    Some(handler)
                }
                Err(error) => {
                    focus_events_enabled.store(false, Ordering::Release);
                    tracing::debug!(hresult = ?error.code(), "secure input: UIA focus subscription unavailable");
                    None
                }
            }
        });
        Self {
            automation,
            elements: HashMap::new(),
            focus_handler,
            focus_events_enabled,
            apartment,
        }
    }

    fn probe(&mut self, request: ProviderRequest) -> Verdict {
        if !self.apartment.0 || !request.is_current() {
            return Verdict::UNKNOWN;
        }
        let Some(automation) = &self.automation else {
            return Verdict::UNKNOWN;
        };
        let element = match request.source {
            ProbeSource::Focused => unsafe { automation.GetFocusedElement() }.ok(),
            ProbeSource::Pointer(point) => pointer_element(automation, &request, point),
        };
        let Some(element) = element else {
            return Verdict::UNKNOWN;
        };
        let element_changed = self
            .elements
            .get(&request.probe.target)
            .is_none_or(|previous| {
                !unsafe { automation.CompareElements(previous, &element) }
                    .is_ok_and(|same| same.as_bool())
            });
        let status = element_status(&element);
        // A property lookup can itself race a same-HWND DOM focus change.
        let stable = unsafe { automation.GetFocusedElement() }
            .ok()
            .and_then(|current| unsafe { automation.CompareElements(&element, &current) }.ok())
            .is_some_and(|same| same.as_bool());
        if !stable || !request.is_current() {
            return Verdict::UNKNOWN;
        }
        if self.elements.len() >= 64 && !self.elements.contains_key(&request.probe.target) {
            self.elements.clear();
        }
        self.elements.insert(request.probe.target, element);
        Verdict {
            status,
            element_changed,
            pointer_anchor: matches!(request.source, ProbeSource::Pointer(_)),
        }
    }
}

/// Freeze the clicked control before observing focus. A different focused
/// ancestor must never replace a password field's anchor while delivery lags.
fn pointer_element(
    automation: &IUIAutomation,
    request: &ProviderRequest,
    point: POINT,
) -> Option<IUIAutomationElement> {
    let Ok(hit) = (unsafe { automation.ElementFromPoint(point) }) else {
        tracing::debug!(
            reason = "hit_test_unavailable",
            "secure input: pointer anchor rejected"
        );
        return None;
    };
    let Ok(walker) = (unsafe { automation.ControlViewWalker() }) else {
        tracing::debug!(
            reason = "tree_unavailable",
            "secure input: pointer anchor rejected"
        );
        return None;
    };
    let Some(anchor) = focusable_pointer_anchor(
        hit,
        |element| {
            if !request.is_current() {
                return None;
            }
            element_boolean_property(element, UIA_IsKeyboardFocusablePropertyId)
        },
        |element| {
            if !request.is_current() {
                return None;
            }
            unsafe { walker.GetParentElement(element) }.ok()
        },
    ) else {
        tracing::debug!(
            reason = "focusable_anchor_unavailable",
            "secure input: pointer anchor rejected"
        );
        return None;
    };
    let focused = wait_for_pointer_focus(
        anchor,
        |anchor| {
            if !request.is_current() {
                return None;
            }
            let focused = unsafe { automation.GetFocusedElement() }.ok()?;
            unsafe { automation.CompareElements(anchor, &focused) }
                .ok()
                .map(|same| same.as_bool())
        },
        || {
            if !request.is_current() {
                return false;
            }
            // Only the provider worker waits for this observable condition.
            // One deadline includes hit testing, traversal and focus delivery.
            std::thread::sleep(
                POINTER_FOCUS_INTERVAL
                    .min(request.deadline.saturating_duration_since(Instant::now())),
            );
            request.is_current()
        },
    );
    if focused.is_none() {
        tracing::debug!(
            reason = "anchored_focus_unavailable",
            "secure input: pointer anchor rejected"
        );
    }
    focused
}

fn focusable_pointer_anchor<E>(
    mut element: E,
    mut focusable: impl FnMut(&E) -> Option<bool>,
    mut parent: impl FnMut(&E) -> Option<E>,
) -> Option<E> {
    for _ in 0..POINTER_ANCESTOR_LIMIT {
        if focusable(&element)? {
            return Some(element);
        }
        element = parent(&element)?;
    }
    None
}

fn wait_for_pointer_focus<E>(
    anchor: E,
    mut focused: impl FnMut(&E) -> Option<bool>,
    mut wait: impl FnMut() -> bool,
) -> Option<E> {
    for _ in 0..POINTER_FOCUS_ATTEMPTS {
        if focused(&anchor)? {
            return Some(anchor);
        }
        if !wait() {
            return None;
        }
    }
    None
}

impl Drop for Provider {
    fn drop(&mut self) {
        // COM can deliver an already in-flight event during unsubscription.
        // Disable its notification before releasing the handler/apartment.
        self.focus_events_enabled.store(false, Ordering::Release);
        if let (Some(automation), Some(handler)) = (&self.automation, &self.focus_handler) {
            let _ = unsafe { automation.RemoveFocusChangedEventHandler(handler) };
        }
    }
}

/// Explicitly request the property's non-default value. Unsupported providers
/// return a non-BOOL reserved value, not a trustworthy `false`.
/// Caller must be on a UIA worker, never on an input hook.
pub(crate) fn element_status(element: &IUIAutomationElement) -> Status {
    match element_boolean_property(element, UIA_IsPasswordPropertyId) {
        Some(true) => Status::Secure,
        Some(false) => Status::NotSecure,
        None => Status::Unknown,
    }
}

fn element_boolean_property(
    element: &IUIAutomationElement,
    property: UIA_PROPERTY_ID,
) -> Option<bool> {
    let Ok(mut value) = (unsafe { element.GetCurrentPropertyValueEx(property, true) }) else {
        return None;
    };
    let result = variant_boolean(&value);
    let _ = unsafe { VariantClear(&raw mut value) };
    result
}

fn variant_boolean(value: &VARIANT) -> Option<bool> {
    let value = unsafe { &value.Anonymous.Anonymous };
    if value.vt != VT_BOOL {
        return None;
    }
    Some(unsafe { value.Anonymous.boolVal }.0 != 0)
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

fn reject_probe(request: Probe) {
    ring_buffer::reject_secure_capture_for_focus(
        request.generation,
        request.target.foreground,
        request.target.focus,
    );
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

fn enqueue_refresh() -> bool {
    enqueue_refresh_for_generation(capture_generation())
}

fn enqueue_refresh_for_generation(generation: u64) -> bool {
    if let (Some(Some(worker)), Some(target)) = (WORKER.get(), Target::capture()) {
        worker.submit(
            Command::Refresh(Probe { target, generation }),
            CAPTURE_LIFETIME,
        )
    } else {
        false
    }
}

fn begin_capture_dispatch(expected: u64) -> Option<MutexGuard<'static, ()>> {
    match CAPTURE_DISPATCH.try_lock() {
        Ok(guard) => Some(guard),
        Err(_) => {
            // The physical key still reaches the editor. Its journal prefix
            // becomes unavailable because this event could not be ordered.
            tracing::debug!(
                reason = "dispatch_contention",
                "secure input: capture rejected"
            );
            reject_capture(expected);
            None
        }
    }
}

fn with_uia_focus_dispatch<R>(submit: impl FnOnce(u64) -> R) -> R {
    // Keep the epoch from before locking: a hook that loses try_lock can reject
    // this epoch while we own the lock. Its missing first key must not be made
    // complete by a refresh stamped with the resulting newer epoch.
    let expected = capture_generation();
    let _dispatch = CAPTURE_DISPATCH
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // The production closure only snapshots native handles and uses try_send;
    // it never calls a provider, waits for a response, or locks the journal.
    submit(expected)
}

/// Called for navigation and wheel changes, including changes within the same
/// native HWND. No provider calls or waits occur here.
pub(crate) fn invalidate_context() {
    ring_buffer::invalidate_capture_context();
    enqueue_refresh();
}

/// Clear the previous input context before a pointer press reaches its target.
/// The post-click focus probe is queued separately when the button is released.
pub(crate) fn invalidate_pointer_context() {
    ring_buffer::invalidate_pointer_capture_context();
    tracing::debug!("secure input: pointer press invalidated context");
}

/// Queue the pointer's focus probe after the press, without resetting the epoch
/// or input already captured while the button was held.
pub(crate) fn refresh_pointer_context(point: POINT) {
    let generation = capture_generation();
    let queued = if let (Some(Some(worker)), Some(target)) = (WORKER.get(), Target::capture()) {
        worker.submit(
            Command::PointerRefresh {
                probe: Probe { target, generation },
                point,
            },
            CAPTURE_LIFETIME,
        )
    } else {
        false
    };
    tracing::debug!(queued, "secure input: pointer release refresh");
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
        ring_buffer::refresh_capture_focus(pending_capture_for_generation);
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
                    tracing::debug!(reason = "queue_expired", "secure input: capture rejected");
                    reject_probe(Probe {
                        target: capture.target,
                        generation: capture.generation,
                    });
                }
            },
        )
        .ok()
    });
    enqueue_refresh();
}

fn verdict(
    provider: Option<&BoundedWorker<ProviderRequest, Verdict>>,
    request: Probe,
    source: ProbeSource,
) -> Verdict {
    if !validate_capture_generation(request.generation) || !request.target.is_current() {
        return Verdict::UNKNOWN;
    }
    if let Some(status) = native_status(request.target) {
        return Verdict {
            status,
            element_changed: false,
            pointer_anchor: false,
        };
    }
    if FOCUS_HOOKS_READY.get() != Some(&true) {
        return Verdict::UNKNOWN;
    }
    provider
        .and_then(|worker| {
            worker.request(
                ProviderRequest {
                    probe: request,
                    source,
                    deadline: Instant::now() + PROVIDER_TIMEOUT,
                },
                PROVIDER_TIMEOUT,
            )
        })
        .unwrap_or(Verdict::UNKNOWN)
}

fn process_command(
    provider: Option<&BoundedWorker<ProviderRequest, Verdict>>,
    command: Command,
) -> Status {
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
                ProbeSource::Focused,
            );
            if !capture_is_current(&capture) {
                return Status::Unknown;
            }
            if !result.status.is_allowed() || result.element_changed {
                tracing::debug!(status = ?result.status, element_changed = result.element_changed,
                    "secure input: capture metadata rejected");
                reject_probe(Probe {
                    target: capture.target,
                    generation: capture.generation,
                });
                return result.status;
            }
            let typed = ring_buffer::record_captured_keydown(&capture.key, capture.generation);
            // Native focus can change while decoding, before its WinEvent is
            // delivered. Reject that incomplete prefix before pending work
            // drops to zero and a later focus switch could cache it as valid.
            if !capture_is_current(&capture) {
                return Status::Unknown;
            }
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
            let result = verdict(provider, request, ProbeSource::Focused);
            if !validate_capture_generation(request.generation) || !request.target.is_current() {
                return Status::Unknown;
            }
            if !result.status.is_allowed() || result.element_changed {
                tracing::debug!(status = ?result.status, element_changed = result.element_changed,
                    "secure input: conversion metadata rejected");
                reject_probe(request);
                return Status::Unknown;
            }
            result.status
        }
        Command::Refresh(request) => refresh_context(provider, request, ProbeSource::Focused),
        Command::PointerRefresh { probe, point } => {
            refresh_context(provider, probe, ProbeSource::Pointer(point))
        }
    }
}

fn refresh_context(
    provider: Option<&BoundedWorker<ProviderRequest, Verdict>>,
    request: Probe,
    source: ProbeSource,
) -> Status {
    let result = verdict(provider, request, source);
    complete_refresh(request, result, || request.target.is_current())
}

fn complete_refresh(
    request: Probe,
    result: Verdict,
    target_is_current: impl FnOnce() -> bool,
) -> Status {
    // Provider work has finished before this barrier. A hook snapshot already
    // in progress must finish registering its pending lease and submitting it
    // before we inspect the FIFO's ownership. New hooks still use try_lock and
    // reject their prefix instead of waiting for this coordinator.
    let _dispatch = (result.pointer_anchor || !result.status.is_allowed()).then(|| {
        CAPTURE_DISPATCH
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    });
    if !validate_capture_generation(request.generation) || !target_is_current() {
        return Status::Unknown;
    }
    if result.pointer_anchor && pending_capture_for_generation(request.generation) {
        tracing::debug!(
            reason = "input_during_pointer_probe",
            "secure input: pointer anchor rejected"
        );
        reject_probe(request);
        return Status::Unknown;
    }
    if !result.status.is_allowed() {
        tracing::debug!(status = ?result.status, "secure input: focus metadata rejected");
        ring_buffer::reject_secure_observation_for_focus(
            request.generation,
            request.target.foreground,
            request.target.focus,
            pending_capture_for_generation,
        );
    } else if result.element_changed {
        tracing::debug!("secure input: focused UIA element changed");
        ring_buffer::discard_changed_capture_context(request.generation);
    }
    result.status
}

fn capture_is_current(capture: &Capture) -> bool {
    if !validate_capture_generation(capture.generation) {
        tracing::debug!(reason = "stale_epoch", "secure input: capture rejected");
        return false;
    }
    let expired = capture.captured.elapsed() >= CAPTURE_LIFETIME;
    let target_current = capture.target.is_current() && capture.key.is_current();
    if expired || !target_current {
        tracing::debug!(
            expired,
            target_current,
            "secure input: capture context rejected"
        );
        reject_probe(Probe {
            target: capture.target,
            generation: capture.generation,
        });
        return false;
    }
    true
}

/// Nonblocking hook entry: preserve physical input and normal hotkey handling
/// even if the target's accessibility provider is unavailable or hung.
pub(crate) fn record_keydown(kb: &KBDLLHOOKSTRUCT, vk: u32) {
    let Ok(virtual_key) = u16::try_from(vk).map(VIRTUAL_KEY) else {
        return;
    };
    // Modifier taps reach hotkey handling but never enter the text journal.
    // Contention with a focus callback cannot lose a capture for these keys,
    // so it must not invalidate the text that Double Alt is about to convert.
    if ring_buffer::is_modifier_vk(virtual_key) {
        return;
    }
    let observed = capture_generation();
    let Some(_dispatch) = begin_capture_dispatch(observed) else {
        return;
    };
    if !validate_capture_generation(observed) {
        return;
    }
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
    let Some(key) = ring_buffer::capture_keydown(kb, vk) else {
        return;
    };
    if !target.is_current() {
        reject_capture(observed);
        return;
    }
    let Some(expected) =
        ring_buffer::capture_generation_for_key(&key, observed, pending_capture_for_generation)
    else {
        return;
    };
    let capture = Capture {
        key,
        target,
        generation: expected,
        captured: Instant::now(),
        _pending: PendingCapture::new(expected),
    };
    let submitted = WORKER
        .get()
        .and_then(Option::as_ref)
        .is_some_and(|worker| worker.submit(Command::Capture(Box::new(capture)), CAPTURE_LIFETIME));
    if !submitted {
        reject_probe(Probe {
            target,
            generation: expected,
        });
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
        reject_probe(Probe {
            target,
            generation: expected,
        });
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

    #[test]
    fn pending_capture_ownership_stays_with_its_epoch_until_last_drop() {
        let _guard = ring_buffer::test_guard();
        let first_generation = capture_generation();
        let first = PendingCapture::new(first_generation);
        let first_tail = PendingCapture::new(first_generation);
        ring_buffer::clear_secure_input_data();
        let next_generation = capture_generation();
        assert_ne!(first_generation, next_generation);
        assert!(pending_capture_for_generation(first_generation));
        assert!(!pending_capture_for_generation(next_generation));
        let next = PendingCapture::new(next_generation);
        drop(first);
        assert!(pending_capture_for_generation(first_generation));
        drop(first_tail);
        assert!(!pending_capture_for_generation(first_generation));
        assert!(pending_capture_for_generation(next_generation));
        drop(next);
        assert!(!pending_capture_for_generation(next_generation));
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn uia_refresh_cannot_overtake_a_started_raw_snapshot() {
        use std::{sync::mpsc, thread};

        let _guard = ring_buffer::test_guard();
        let (snapshot_started, snapshot_observed) = mpsc::channel();
        let (finish_snapshot, resume_snapshot) = mpsc::channel();
        let (focus_started, focus_observed) = mpsc::channel();
        let (submitted, order) = mpsc::channel();
        let capture_submission = submitted.clone();
        let capture = thread::spawn(move || {
            let expected = capture_generation();
            let _dispatch = begin_capture_dispatch(expected).unwrap();
            snapshot_started.send(()).unwrap();
            resume_snapshot
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            capture_submission.send("capture").unwrap();
        });
        snapshot_observed
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let focus = thread::spawn(move || {
            focus_started.send(()).unwrap();
            with_uia_focus_dispatch(|_| submitted.send("refresh").unwrap());
        });
        focus_observed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(order.recv_timeout(Duration::from_millis(50)).is_err());
        finish_snapshot.send(()).unwrap();
        capture.join().unwrap();
        focus.join().unwrap();
        assert_eq!(order.recv().unwrap(), "capture");
        assert_eq!(order.recv().unwrap(), "refresh");
    }

    #[test]
    fn contended_capture_cannot_be_reauthorized_by_the_owning_focus_refresh() {
        use std::{sync::mpsc, thread};

        let _guard = ring_buffer::test_guard();
        ring_buffer::clear_secure_input_data();
        let before = capture_generation();
        let (focus_locked, focus_observed) = mpsc::channel();
        let (finish_refresh, resume_refresh) = mpsc::channel();
        let focus = thread::spawn(move || {
            with_uia_focus_dispatch(|expected| {
                focus_locked.send(()).unwrap();
                resume_refresh.recv_timeout(Duration::from_secs(1)).unwrap();
                expected
            })
        });
        focus_observed.recv_timeout(Duration::from_secs(1)).unwrap();
        let start = Instant::now();
        assert!(begin_capture_dispatch(before).is_none());
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(!validate_capture_generation(before));
        finish_refresh.send(()).unwrap();
        let refresh_generation = focus.join().unwrap();
        assert_eq!(refresh_generation, before);
        // This is the existing post-probe Refresh mutation. Its old epoch must
        // not reset the unavailable prefix created by the missing physical key.
        ring_buffer::discard_changed_capture_context(refresh_generation);
        ring_buffer::push_text("suffix");
        assert!(ring_buffer::runs_snapshot().is_empty());
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn modifier_taps_preserve_the_journal_during_focus_dispatch_contention() {
        let _guard = ring_buffer::test_guard();
        ring_buffer::clear_secure_input_data();
        ring_buffer::push_text(".ghbdtn");
        let before = ring_buffer::runs_snapshot();
        let capture_before = capture_generation();
        let context_before = generation();
        with_uia_focus_dispatch(|_| {
            // Exercise the production entry point while a focus callback owns
            // dispatch. Include both Alt taps and every supported modifier.
            for vk in [
                0xA4, 0xA4, 0xA5, 0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0x5B, 0x5C,
            ] {
                let key = KBDLLHOOKSTRUCT {
                    vkCode: vk,
                    ..Default::default()
                };
                record_keydown(&key, vk);
                assert!(
                    validate_capture_generation(capture_before),
                    "modifier {vk:#x}"
                );
                assert!(validate_generation(context_before), "modifier {vk:#x}");
                assert_eq!(ring_buffer::runs_snapshot(), before, "modifier {vk:#x}");
            }
            // Losing a printable event must still reject the entire prefix.
            // Ignoring dispatch contention for every key is not a valid fix.
            let period = KBDLLHOOKSTRUCT {
                vkCode: 0xBE,
                ..Default::default()
            };
            record_keydown(&period, 0xBE);
            assert!(!validate_capture_generation(capture_before));
            assert!(!validate_generation(context_before));
            assert!(ring_buffer::runs_snapshot().is_empty());
        });
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn uia_focus_callback_is_bounded_and_late_delivery_is_disabled() {
        use std::sync::mpsc;

        let (release, blocked) = mpsc::channel();
        let (started, observed) = mpsc::channel();
        let worker = BoundedWorker::start("uia-focus-notification-test", move || {
            move |()| {
                let _ = started.send(());
                let _ = blocked.recv();
            }
        })
        .unwrap();
        let notifications = Arc::new(AtomicUsize::new(0));
        let callback_notifications = Arc::clone(&notifications);
        let enabled = Arc::new(AtomicBool::new(true));
        let handler: IUIAutomationFocusChangedEventHandler = FocusChangedHandler {
            enabled: Arc::clone(&enabled),
            notify: Box::new(move || {
                callback_notifications.fetch_add(1, Ordering::Relaxed);
                let _ = worker.submit((), Duration::from_secs(1));
            }),
        }
        .into();

        // The callback does not need a sender property, and a blocked consumer
        // cannot turn an accessibility event into a synchronous provider wait.
        unsafe { handler.HandleFocusChangedEvent(None) }.unwrap();
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        let before = Instant::now();
        for _ in 0..32 {
            unsafe { handler.HandleFocusChangedEvent(None) }.unwrap();
        }
        assert!(before.elapsed() < Duration::from_millis(100));
        enabled.store(false, Ordering::Release);
        let count = notifications.load(Ordering::Acquire);
        unsafe { handler.HandleFocusChangedEvent(None) }.unwrap();
        assert_eq!(notifications.load(Ordering::Acquire), count);
        let _ = release.send(());
        let _ = release.send(());
    }

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
            variant_boolean(&boolean_variant(false, VT_BOOL)),
            Some(false)
        );
        assert_eq!(variant_boolean(&boolean_variant(true, VT_BOOL)), Some(true));
        assert_eq!(variant_boolean(&boolean_variant(false, VT_EMPTY)), None);
        assert!(!Status::Unknown.is_allowed());
        assert!(!Status::Secure.is_allowed());
    }

    #[test]
    fn pointer_anchor_stops_at_the_nearest_explicitly_focusable_control() {
        let mut inspected = Vec::new();
        let anchor = focusable_pointer_anchor(
            0,
            |element| {
                inspected.push(*element);
                Some(*element >= 1)
            },
            |element| Some(element + 1),
        );
        // 0 is a text descendant, 1 the clicked control, 2 its focusable root.
        assert_eq!(anchor, Some(1));
        assert_eq!(inspected, [0, 1]);
        // Unknown is not a false property that permits climbing to the root.
        assert_eq!(
            focusable_pointer_anchor(0, |_| None, |_| panic!("unknown anchor")),
            None
        );
    }

    #[test]
    fn pointer_anchor_traversal_cannot_escape_its_bound() {
        let mut inspected = 0;
        let anchor = focusable_pointer_anchor(
            0,
            |_| {
                inspected += 1;
                Some(false)
            },
            |element| Some(element + 1),
        );
        assert_eq!(anchor, None);
        assert_eq!(inspected, POINTER_ANCESTOR_LIMIT);
    }

    #[test]
    fn pointer_refresh_waits_for_its_frozen_anchor_not_the_previous_focus() {
        let mut focus = [0, 0, 1].into_iter();
        let mut waits = 0;
        assert_eq!(
            wait_for_pointer_focus(
                1,
                |anchor| focus.next().map(|focused| *anchor == focused),
                || {
                    waits += 1;
                    true
                },
            ),
            Some(1)
        );
        assert_eq!(waits, 2);
    }

    #[test]
    fn pointer_refresh_never_rebinds_a_password_anchor_to_another_field_or_root() {
        // Anchor 1 may be a password control; 2 is a plain field or focusable
        // ancestor. Matching the latter would authorize keys from the former.
        let mut focus = [0, 2, 2].into_iter();
        let mut observed = Vec::new();
        let result = wait_for_pointer_focus(
            1,
            |anchor| {
                observed.push(*anchor);
                focus.next().map(|focused| *anchor == focused)
            },
            || true,
        );
        assert_eq!(result, None);
        assert!(observed.iter().all(|anchor| *anchor == 1));
    }

    #[test]
    fn pointer_refresh_stops_on_unknown_focus_or_expiration() {
        assert_eq!(
            wait_for_pointer_focus(1, |_| None, || panic!("unknown focus")),
            None
        );
        let mut observations = 0;
        assert_eq!(
            wait_for_pointer_focus(
                1,
                |_| {
                    observations += 1;
                    Some(false)
                },
                || false,
            ),
            None
        );
        assert_eq!(observations, 1);
        observations = 0;
        assert_eq!(
            wait_for_pointer_focus(
                1,
                |_| {
                    observations += 1;
                    Some(false)
                },
                || true,
            ),
            None
        );
        assert_eq!(observations, POINTER_FOCUS_ATTEMPTS);
    }

    fn pointer_test_probe(generation: u64) -> Probe {
        Probe {
            // The completion seam supplies native-target validity explicitly;
            // these tests do not require an interactive foreground window.
            target: Target {
                foreground: 1,
                focus: 1,
                thread: 1,
                process: 1,
            },
            generation,
        }
    }

    fn pointer_test_verdict(pointer_anchor: bool) -> Verdict {
        Verdict {
            status: Status::NotSecure,
            element_changed: true,
            pointer_anchor,
        }
    }

    fn run_pointer_fifo_case(pointer_anchor: bool, key_during_wait: bool) -> (Status, usize, bool) {
        use std::sync::mpsc;

        enum Action {
            Pointer,
            Capture {
                generation: u64,
                pending: PendingCapture,
            },
        }

        ring_buffer::clear_secure_input_data();
        let generation = capture_generation();
        let request = pointer_test_probe(generation);
        let focused = Arc::new(AtomicUsize::new(0)); // Password A still owns input.
        let worker_focus = Arc::clone(&focused);
        let appended = Arc::new(AtomicUsize::new(0));
        let worker_appended = Arc::clone(&appended);
        let (waiting, observed_wait) = mpsc::channel();
        let (focus_changed, resume_probe) = mpsc::channel();
        let (completed, results) = mpsc::channel();
        let worker = BoundedWorker::start("pointer-admission-test", move || {
            move |action| {
                let result = match action {
                    Action::Pointer => {
                        // The real anchor-wait helper confirms ordinary B only
                        // after the test has queued A's input when requested.
                        assert_eq!(
                            wait_for_pointer_focus(
                                1,
                                |anchor| Some(*anchor == worker_focus.load(Ordering::Acquire)),
                                || {
                                    waiting.send(()).unwrap();
                                    resume_probe.recv_timeout(Duration::from_secs(2)).unwrap();
                                    true
                                },
                            ),
                            Some(1)
                        );
                        complete_refresh(request, pointer_test_verdict(pointer_anchor), || true)
                    }
                    Action::Capture {
                        generation,
                        pending,
                    } => {
                        // Synthetic decoding isolates admission from Windows
                        // focus/keyboard setup. The real epoch and journal are
                        // used, and the negative control must actually append.
                        let result = if validate_capture_generation(generation) {
                            ring_buffer::push_text("x");
                            if !ring_buffer::runs_snapshot().is_empty() {
                                worker_appended.fetch_add(1, Ordering::Relaxed);
                            }
                            Status::NotSecure
                        } else {
                            Status::Unknown
                        };
                        drop(pending);
                        result
                    }
                };
                completed.send(result).unwrap();
                result
            }
        })
        .unwrap();
        assert!(worker.submit(Action::Pointer, Duration::from_secs(2)));
        observed_wait.recv_timeout(Duration::from_secs(2)).unwrap();
        let enqueue_capture = || {
            let _dispatch = begin_capture_dispatch(generation).unwrap();
            assert!(worker.submit(
                Action::Capture {
                    generation,
                    pending: PendingCapture::new(generation),
                },
                Duration::from_secs(2)
            ));
        };
        if key_during_wait {
            enqueue_capture();
        }
        focused.store(1, Ordering::Release);
        focus_changed.send(()).unwrap();
        let pointer_result = results.recv_timeout(Duration::from_secs(2)).unwrap();
        if !key_during_wait {
            enqueue_capture();
        }
        let _ = results.recv_timeout(Duration::from_secs(2)).unwrap();
        let appended = appended.load(Ordering::Relaxed);
        let current = validate_capture_generation(generation);
        if appended == 0 {
            ring_buffer::push_text("late-suffix");
            assert!(ring_buffer::runs_snapshot().is_empty());
        }
        ring_buffer::clear_secure_input_data();
        (pointer_result, appended, current)
    }

    #[test]
    fn pointer_completion_rejects_input_queued_before_its_anchor_was_confirmed() {
        let _guard = ring_buffer::test_guard();
        // Removing the pointer barrier reproduces the incorrect A-key append.
        // This control makes refusal non-vacuous despite synthetic decoding.
        assert_eq!(
            run_pointer_fifo_case(false, true),
            (Status::NotSecure, 1, true)
        );
        assert_eq!(
            run_pointer_fifo_case(true, true),
            (Status::Unknown, 0, false)
        );
        // A confirmed anchor with no older pending input still captures normally.
        assert_eq!(
            run_pointer_fifo_case(true, false),
            (Status::NotSecure, 1, true)
        );
    }

    #[test]
    fn native_pointer_fast_path_does_not_reject_its_known_pending_input() {
        let _guard = ring_buffer::test_guard();
        ring_buffer::clear_secure_input_data();
        let generation = capture_generation();
        let pending = PendingCapture::new(generation);
        let result = complete_refresh(
            pointer_test_probe(generation),
            Verdict {
                status: Status::NotSecure,
                element_changed: false,
                pointer_anchor: false,
            },
            || true,
        );
        assert_eq!(result, Status::NotSecure);
        assert!(validate_capture_generation(generation));
        ring_buffer::push_text("known-native-input");
        assert!(!ring_buffer::runs_snapshot().is_empty());
        drop(pending);
        ring_buffer::clear_secure_input_data();
    }

    fn assert_refresh_waits_for_started_snapshot(verdict: Verdict) {
        use std::{sync::mpsc, thread};

        let _guard = ring_buffer::test_guard();
        ring_buffer::clear_secure_input_data();
        let generation = capture_generation();
        let (snapshot_started, snapshot_observed) = mpsc::channel();
        let (finish_snapshot, resume_snapshot) = mpsc::channel();
        let (registered, lease) = mpsc::channel();
        let snapshot = thread::spawn(move || {
            let _dispatch = begin_capture_dispatch(generation).unwrap();
            snapshot_started.send(()).unwrap();
            resume_snapshot
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            registered.send(PendingCapture::new(generation)).unwrap();
        });
        snapshot_observed
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(!pending_capture_for_generation(generation));
        let (completion_started, completion_observed) = mpsc::channel();
        let (checked, check_observed) = mpsc::channel();
        let completion = thread::spawn(move || {
            completion_started.send(()).unwrap();
            complete_refresh(pointer_test_probe(generation), verdict, || {
                checked.send(()).unwrap();
                true
            })
        });
        completion_observed
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(
            check_observed
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
        finish_snapshot.send(()).unwrap();
        let pending = lease.recv_timeout(Duration::from_secs(2)).unwrap();
        snapshot.join().unwrap();
        assert_eq!(completion.join().unwrap(), Status::Unknown);
        assert!(!validate_capture_generation(generation));
        ring_buffer::push_text("late-suffix");
        assert!(ring_buffer::runs_snapshot().is_empty());
        drop(pending);
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn refresh_completion_waits_for_started_snapshot_to_register_its_lease() {
        for verdict in [pointer_test_verdict(true), Verdict::UNKNOWN] {
            assert_refresh_waits_for_started_snapshot(verdict);
        }
    }

    fn assert_contended_key_stays_rejected(verdict: Verdict) {
        use std::{sync::mpsc, thread};

        let _guard = ring_buffer::test_guard();
        ring_buffer::clear_secure_input_data();
        let generation = capture_generation();
        let (locked, lock_observed) = mpsc::channel();
        let (finish_check, resume_check) = mpsc::channel();
        let completion = thread::spawn(move || {
            complete_refresh(pointer_test_probe(generation), verdict, || {
                locked.send(()).unwrap();
                resume_check.recv_timeout(Duration::from_secs(2)).unwrap();
                true
            })
        });
        lock_observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(begin_capture_dispatch(generation).is_none());
        assert!(!validate_capture_generation(generation));
        finish_check.send(()).unwrap();
        let _ = completion.join().unwrap();
        ring_buffer::push_text("late-suffix");
        assert!(ring_buffer::runs_snapshot().is_empty());
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn contended_key_cannot_be_reopened_by_pointer_or_failed_observation() {
        for verdict in [pointer_test_verdict(true), Verdict::UNKNOWN] {
            assert_contended_key_stays_rejected(verdict);
        }
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
