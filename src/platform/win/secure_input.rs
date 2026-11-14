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
        Arc, LazyLock, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
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
const IDENTITY_CACHE_CAPACITY: usize = 64;
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
// At most the bounded capture queue plus its current consumer can own entries.
// Old queued work must not mark unrelated later native contexts incomplete.
static PENDING_CAPTURES: Mutex<BTreeMap<u64, usize>> = Mutex::new(BTreeMap::new());
static WORKER: OnceLock<Option<BoundedWorker<Command, Status>>> = OnceLock::new();
static FOCUS_HOOKS_READY: OnceLock<bool> = OnceLock::new();
// UIA callbacks run on another thread. Preserve snapshot-to-submission ordering
// against their focus refreshes without ever waiting on the keyboard hook.
static CAPTURE_DISPATCH: Mutex<()> = Mutex::new(());
static PUBLISHED_IDENTITIES: LazyLock<IdentityPublications> =
    LazyLock::new(IdentityPublications::new);

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

/// Only native target metadata and monotonic revisions cross into the hook.
/// COM interfaces remain owned by the provider worker.
struct IdentityPublications {
    frontier: AtomicU64,
    targets: Mutex<HashMap<Target, u64>>,
}

impl IdentityPublications {
    fn new() -> Self {
        Self {
            frontier: AtomicU64::new(0),
            targets: Mutex::new(HashMap::new()),
        }
    }

    fn frontier(&self) -> u64 {
        self.frontier.load(Ordering::Acquire)
    }

    fn at_admission(&self, target: Target, entry_frontier: u64) -> Option<u64> {
        // Never wait behind a provider publication on the keyboard hook. A
        // revision established during target lookup cannot label an older key.
        let targets = self.targets.try_lock().ok()?;
        targets
            .get(&target)
            .copied()
            .filter(|revision| *revision <= entry_frontier)
    }

    fn publish(&self, target: Target, changed: bool, forget: bool) -> u64 {
        let mut targets = self
            .targets
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if forget || (targets.len() >= IDENTITY_CACHE_CAPACITY && !targets.contains_key(&target)) {
            targets.clear();
        }
        if !changed && let Some(revision) = targets.get(&target) {
            return *revision;
        }
        let revision = self.frontier.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        targets.insert(target, revision);
        revision
    }

    fn retire(&self, target: Target, revision: u64) {
        let mut targets = self
            .targets
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if targets.get(&target) == Some(&revision) {
            targets.remove(&target);
        }
        // Never restore a previous revision. The provider can retain the COM
        // element, but its next observation must mint a different publication.
    }
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
    identity_revision: Option<u64>,
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

    fn metadata_is_current(&self) -> bool {
        Instant::now() < self.deadline && self.probe.target.is_current()
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
    identity: MetadataIdentity,
}

#[derive(Clone, Copy, Debug)]
enum MetadataIdentity {
    Native,
    Published(u64),
    Unavailable,
}

impl Verdict {
    const UNKNOWN: Self = Self {
        status: Status::Unknown,
        element_changed: false,
        identity: MetadataIdentity::Unavailable,
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
        probe_observed_identity(
            element_changed,
            || validate_capture_generation(request.probe.generation),
            || request.metadata_is_current(),
            || {
                let forget = self.elements.len() >= IDENTITY_CACHE_CAPACITY
                    && !self.elements.contains_key(&request.probe.target);
                if forget {
                    self.elements.clear();
                }
                self.elements.insert(request.probe.target, element.clone());
                PUBLISHED_IDENTITIES.publish(request.probe.target, element_changed, forget)
            },
            |revision| PUBLISHED_IDENTITIES.retire(request.probe.target, revision),
            || element_status(&element),
            || {
                // A property lookup can itself race a same-HWND DOM focus change.
                unsafe { automation.GetFocusedElement() }
                    .ok()
                    .and_then(|current| {
                        unsafe { automation.CompareElements(&element, &current) }.ok()
                    })
                    .is_some_and(|same| same.as_bool())
            },
        )
    }
}

fn probe_observed_identity(
    element_changed: bool,
    generation_is_current: impl Fn() -> bool,
    metadata_is_current: impl Fn() -> bool,
    publish_identity: impl FnOnce() -> u64,
    retire_identity: impl Fn(u64),
    password_status: impl FnOnce() -> Status,
    remains_focused: impl FnOnce() -> bool,
) -> Verdict {
    if !generation_is_current() || !metadata_is_current() {
        return Verdict::UNKNOWN;
    }
    // The exact focused element is known now. Publish its identity before
    // slower metadata queries, so later keys can retain that observation.
    // This is not authorization: the final verdict and every queued Capture
    // still need fresh ordinary metadata and stable focus. Older snapshots
    // keep their original revision, and failed probes never roll it back.
    let revision = publish_identity();
    let status = password_status();
    let denied = !status.is_allowed();
    if denied {
        // A coordinator may already have timed out with no pending input. Its
        // clean observation keeps the raw epoch, so keys arriving afterward
        // must not inherit this unconfirmed or sensitive publication. Retire
        // before another provider call can stall or its result can be dropped.
        retire_identity(revision);
    }
    if !remains_focused() || !metadata_is_current() {
        if !denied {
            retire_identity(revision);
        }
        return Verdict::UNKNOWN;
    }
    if !generation_is_current() {
        // Navigation can supersede the old journal request while its ordinary
        // element, native target and deadline remain valid. The old command
        // cannot mutate the new journal, but retiring this still-valid identity
        // would also invalidate new-generation keys that observed it in time.
        if !denied {
            tracing::debug!(
                revision,
                "secure input: superseded probe kept valid ordinary identity"
            );
        }
        return Verdict::UNKNOWN;
    }
    Verdict {
        status,
        element_changed,
        identity: MetadataIdentity::Published(revision),
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
    LazyLock::force(&PUBLISHED_IDENTITIES);
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
            identity: MetadataIdentity::Native,
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
            if !capture_metadata_allows(capture.identity_revision, result) {
                tracing::debug!(status = ?result.status, element_changed = result.element_changed,
                    identity_matches = identity_matches(capture.identity_revision, result.identity),
                    captured_revision = ?capture.identity_revision, observed_identity = ?result.identity,
                    "secure input: capture metadata rejected");
                reject_probe(Probe {
                    target: capture.target,
                    generation: capture.generation,
                });
                return Status::Unknown;
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
    // Denied observations must account for snapshots whose pending lease is
    // still being registered. Successful observations publish an identity
    // revision; an older snapshot cannot inherit that later publication.
    let _dispatch = (!result.status.is_allowed()).then(|| {
        CAPTURE_DISPATCH
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    });
    if !validate_capture_generation(request.generation) || !target_is_current() {
        return Status::Unknown;
    }
    apply_refresh_metadata(request, result, |request| {
        ring_buffer::reject_secure_observation_for_focus(
            request.generation,
            request.target.foreground,
            request.target.focus,
            pending_capture_for_generation,
        );
    })
}

fn apply_refresh_metadata(
    request: Probe,
    result: Verdict,
    reject_observation: impl FnOnce(Probe),
) -> Status {
    match result.status {
        Status::Secure => {
            // A confirmed password context remains unavailable even if the
            // same element reports ordinary text before a queued key is checked.
            tracing::debug!(status = ?result.status, "secure input: focus metadata rejected");
            reject_probe(request);
        }
        Status::Unknown => {
            tracing::debug!(status = ?result.status, "secure input: focus metadata rejected");
            reject_observation(request);
        }
        Status::NotSecure if result.element_changed => {
            tracing::debug!("secure input: focused UIA element changed");
            ring_buffer::discard_changed_capture_context(request.generation);
        }
        Status::NotSecure => {}
    }
    result.status
}

fn identity_matches(captured: Option<u64>, observed: MetadataIdentity) -> bool {
    match observed {
        // Standard Edit/RichEdit already binds input to its native HWND.
        MetadataIdentity::Native => true,
        MetadataIdentity::Published(revision) => captured == Some(revision),
        MetadataIdentity::Unavailable => false,
    }
}

fn capture_metadata_allows(captured: Option<u64>, result: Verdict) -> bool {
    result.status.is_allowed()
        && !result.element_changed
        && identity_matches(captured, result.identity)
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
    // Sample before native lookup or the raw snapshot. The target-specific
    // lookup below refuses any revision published after this hook entry.
    let identity_frontier = PUBLISHED_IDENTITIES.frontier();
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
    let identity_revision = PUBLISHED_IDENTITIES.at_admission(target, identity_frontier);
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
        identity_revision,
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

    fn published_test_verdict(revision: u64, element_changed: bool) -> Verdict {
        Verdict {
            status: Status::NotSecure,
            element_changed,
            identity: MetadataIdentity::Published(revision),
        }
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum ProbePause {
        Password,
        Stability,
    }

    fn run_early_publication_case(
        pause: ProbePause,
        earlier_capture: bool,
        status: Status,
        stable: bool,
        current: bool,
    ) -> (Status, usize, bool) {
        use std::sync::mpsc;

        enum Action {
            Refresh,
            Capture {
                identity_revision: Option<u64>,
                pending: PendingCapture,
            },
        }

        ring_buffer::clear_secure_input_data();
        let generation = capture_generation();
        let request = pointer_test_probe(generation);
        let publications = Arc::new(IdentityPublications::new());
        let old_revision = publications.publish(request.target, true, false);
        let worker_publications = Arc::clone(&publications);
        let request_current = Arc::new(AtomicBool::new(true));
        let worker_current = Arc::clone(&request_current);
        let appended = Arc::new(AtomicUsize::new(0));
        let worker_appended = Arc::clone(&appended);
        let (observing, observation_started) = mpsc::channel();
        let (confirm, confirmation) = mpsc::channel();
        let (querying, query_started) = mpsc::channel();
        let (finish, resume_query) = mpsc::channel();
        let (completed, results) = mpsc::channel();
        let worker = BoundedWorker::start_with_expiration(
            "early-identity-test",
            3,
            move || {
                move |action| {
                    let result = match action {
                        Action::Refresh => {
                            observing.send(()).unwrap();
                            confirmation.recv_timeout(Duration::from_secs(2)).unwrap();
                            // This is the production ordering seam used by
                            // Provider::probe, with its property/focus query
                            // blocked after the exact element was observed.
                            let verdict = probe_observed_identity(
                                true,
                                || validate_capture_generation(generation),
                                || worker_current.load(Ordering::Acquire),
                                || worker_publications.publish(request.target, true, false),
                                |revision| worker_publications.retire(request.target, revision),
                                || {
                                    if pause == ProbePause::Password {
                                        querying.send(()).unwrap();
                                        resume_query.recv_timeout(Duration::from_secs(2)).unwrap();
                                    }
                                    status
                                },
                                || {
                                    if pause == ProbePause::Stability {
                                        querying.send(()).unwrap();
                                        resume_query.recv_timeout(Duration::from_secs(2)).unwrap();
                                    }
                                    stable
                                },
                            );
                            if stable && current {
                                assert!(verdict.element_changed);
                            }
                            complete_refresh(request, verdict, || true)
                        }
                        Action::Capture {
                            identity_revision,
                            pending,
                        } => {
                            let result = if !validate_capture_generation(generation) {
                                Status::Unknown
                            } else {
                                // Each later key still receives its own fresh
                                // ordinary verdict for the unchanged element.
                                let fresh = probe_observed_identity(
                                    false,
                                    || validate_capture_generation(generation),
                                    || true,
                                    || worker_publications.publish(request.target, false, false),
                                    |revision| worker_publications.retire(request.target, revision),
                                    || Status::NotSecure,
                                    || true,
                                );
                                if capture_metadata_allows(identity_revision, fresh) {
                                    ring_buffer::push_text("x");
                                    if !ring_buffer::runs_snapshot().is_empty() {
                                        worker_appended.fetch_add(1, Ordering::Relaxed);
                                    }
                                    Status::NotSecure
                                } else {
                                    reject_probe(request);
                                    Status::Unknown
                                }
                            };
                            drop(pending);
                            result
                        }
                    };
                    completed.send(result).unwrap();
                    result
                }
            },
            |_| {},
        )
        .unwrap();
        assert!(worker.submit(Action::Refresh, Duration::from_secs(2)));
        observation_started
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let enqueue_capture = || {
            let frontier = publications.frontier();
            let _dispatch = begin_capture_dispatch(generation).unwrap();
            let identity_revision = publications.at_admission(request.target, frontier);
            assert!(worker.submit(
                Action::Capture {
                    identity_revision,
                    pending: PendingCapture::new(generation),
                },
                Duration::from_secs(2),
            ));
            identity_revision
        };
        if earlier_capture {
            assert_eq!(enqueue_capture(), Some(old_revision));
        }
        confirm.send(()).unwrap();
        query_started.recv_timeout(Duration::from_secs(2)).unwrap();
        // Moving publication after the blocked query fails here: a later
        // snapshot would still carry A, although B's focus was already observed.
        let later_revision = enqueue_capture();
        if pause == ProbePause::Stability && !status.is_allowed() {
            // Known denied metadata is retired before stability can stall.
            assert_eq!(later_revision, None);
        } else {
            assert!(later_revision.is_some());
            assert_ne!(later_revision, Some(old_revision));
        }
        assert_eq!(enqueue_capture(), later_revision);
        assert!(ring_buffer::runs_snapshot().is_empty());
        request_current.store(current, Ordering::Release);
        finish.send(()).unwrap();
        let refresh_result = results.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..(2 + usize::from(earlier_capture)) {
            let _ = results.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let appended = appended.load(Ordering::Relaxed);
        let current = validate_capture_generation(generation);
        if appended == 0 {
            ring_buffer::push_text("late-suffix");
            assert!(ring_buffer::runs_snapshot().is_empty());
        }
        ring_buffer::clear_secure_input_data();
        (refresh_result, appended, current)
    }

    #[test]
    fn observed_identity_is_available_while_fresh_metadata_is_pending() {
        let _guard = ring_buffer::test_guard();
        for pause in [ProbePause::Password, ProbePause::Stability] {
            assert_eq!(
                run_early_publication_case(pause, false, Status::NotSecure, true, true),
                (Status::NotSecure, 2, true),
            );
            // An earlier A key poisons the whole prefix, including B keys
            // admitted after publication while metadata was still pending.
            assert_eq!(
                run_early_publication_case(pause, true, Status::NotSecure, true, true),
                (Status::NotSecure, 0, false),
            );
        }
    }

    #[test]
    fn early_identity_does_not_authorize_denied_unstable_or_expired_metadata() {
        let _guard = ring_buffer::test_guard();
        for pause in [ProbePause::Password, ProbePause::Stability] {
            for (status, stable, current, expected) in [
                (Status::Secure, true, true, Status::Secure),
                (Status::Unknown, true, true, Status::Unknown),
                (Status::NotSecure, false, true, Status::Unknown),
                (Status::NotSecure, true, false, Status::Unknown),
            ] {
                assert_eq!(
                    run_early_publication_case(pause, false, status, stable, current),
                    (expected, 0, false),
                );
            }
        }
    }

    #[test]
    fn stale_observation_cannot_publish_an_identity_or_query_metadata() {
        let verdict = probe_observed_identity(
            true,
            || false,
            || panic!("a stale observation must not query native metadata"),
            || panic!("a stale observation must not publish an identity"),
            |_| panic!("a stale observation must not retire another identity"),
            || panic!("a stale observation must not query password metadata"),
            || panic!("a stale observation must not query focus"),
        );
        assert_eq!(verdict.status, Status::Unknown);
        assert!(matches!(verdict.identity, MetadataIdentity::Unavailable));
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum TimeoutPause {
        Password,
        Stability,
        Reply,
    }

    fn run_clean_timeout_case(
        pause: TimeoutPause,
        status: Status,
        stable: bool,
        current_after_timeout: bool,
        retirement: bool,
    ) -> usize {
        use std::{sync::mpsc, thread};

        enum Query {
            Observe,
            FreshCapture,
        }

        ring_buffer::clear_secure_input_data();
        let raw = capture_generation();
        let request = pointer_test_probe(raw);
        let publications = Arc::new(IdentityPublications::new());
        let preceding = publications.publish(request.target, true, false);
        let worker_publications = Arc::clone(&publications);
        let current = Arc::new(AtomicBool::new(true));
        let worker_current = Arc::clone(&current);
        let (ready, initialized) = mpsc::channel();
        let (blocked, block_observed) = mpsc::channel();
        let (finish, resume) = mpsc::channel();
        let provider = Arc::new(
            BoundedWorker::start("clean-observation-timeout-test", move || {
                ready.send(()).unwrap();
                move |query| {
                    let wait = || {
                        blocked.send(()).unwrap();
                        resume.recv_timeout(Duration::from_secs(2)).unwrap();
                    };
                    match query {
                        Query::Observe => {
                            let verdict = probe_observed_identity(
                                true,
                                || validate_capture_generation(raw),
                                || worker_current.load(Ordering::Acquire),
                                || worker_publications.publish(request.target, true, false),
                                |revision| {
                                    if retirement {
                                        worker_publications.retire(request.target, revision);
                                    }
                                },
                                || {
                                    if pause == TimeoutPause::Password {
                                        wait();
                                    }
                                    status
                                },
                                || {
                                    if pause == TimeoutPause::Stability {
                                        wait();
                                    }
                                    stable
                                },
                            );
                            if pause == TimeoutPause::Reply {
                                // The helper's final check succeeded, but the
                                // transport can still discard this result.
                                assert_eq!(verdict.status, status);
                                wait();
                            }
                            verdict
                        }
                        Query::FreshCapture => probe_observed_identity(
                            false,
                            || true,
                            || true,
                            || worker_publications.publish(request.target, false, false),
                            |revision| worker_publications.retire(request.target, revision),
                            || Status::NotSecure,
                            || true,
                        ),
                    }
                }
            })
            .unwrap(),
        );
        initialized.recv_timeout(Duration::from_secs(2)).unwrap();
        let requester = Arc::clone(&provider);
        let response =
            thread::spawn(move || requester.request(Query::Observe, Duration::from_millis(100)));
        block_observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(response.join().unwrap().is_none());
        assert!(!pending_capture_for_generation(raw));
        assert!(ring_buffer::runs_snapshot().is_empty());

        // The real transport timed out before any input. Isolate the pristine
        // completion from fake native HWNDs: this equivalent empty-journal
        // cleanup expires conversion guards while deliberately retaining G.
        // A hard native-target rejection here would make the test vacuous.
        let context = generation();
        assert_eq!(
            apply_refresh_metadata(request, Verdict::UNKNOWN, |probe| {
                assert!(!pending_capture_for_generation(probe.generation));
                ring_buffer::discard_changed_capture_context(probe.generation);
            }),
            Status::Unknown
        );
        assert!(validate_capture_generation(raw));
        assert!(!validate_generation(context));

        // A password-era key arrives only AFTER that clean timeout, while the
        // provider is still finishing the old observation.
        let frontier = publications.frontier();
        let (captured, pending) = {
            let _dispatch = begin_capture_dispatch(raw).unwrap();
            let captured = publications.at_admission(request.target, frontier);
            (captured, PendingCapture::new(raw))
        };
        if retirement && pause != TimeoutPause::Password && !status.is_allowed() {
            // Secure/Unknown must retire before a stability call or reply can
            // stall, rather than waiting to discover that the request expired.
            assert_eq!(captured, None);
        } else {
            assert!(captured.is_some());
            assert_ne!(captured, Some(preceding));
        }
        current.store(current_after_timeout, Ordering::Release);
        finish.send(()).unwrap();
        // The same element now reports ordinary text. The real provider FIFO
        // finishes/drops the expired response before handling this fresh query.
        let fresh = provider
            .request(Query::FreshCapture, Duration::from_secs(2))
            .unwrap();
        let appended = if capture_metadata_allows(captured, fresh) {
            ring_buffer::push_text("x");
            assert!(!ring_buffer::runs_snapshot().is_empty());
            1
        } else {
            reject_probe(request);
            assert!(!validate_capture_generation(raw));
            ring_buffer::push_text("late-suffix");
            assert!(ring_buffer::runs_snapshot().is_empty());
            0
        };
        drop(pending);
        ring_buffer::clear_secure_input_data();
        appended
    }

    #[test]
    fn clean_timeout_cannot_preserve_a_later_password_capture_binding() {
        let _guard = ring_buffer::test_guard();
        for (pause, status, stable, current) in [
            (TimeoutPause::Password, Status::Secure, true, false),
            (TimeoutPause::Password, Status::Unknown, true, false),
            (TimeoutPause::Password, Status::NotSecure, true, false),
            (TimeoutPause::Stability, Status::NotSecure, false, true),
            (TimeoutPause::Stability, Status::Secure, true, false),
            (TimeoutPause::Reply, Status::Secure, true, true),
            (TimeoutPause::Reply, Status::Unknown, true, true),
        ] {
            // Disabling retirement must reproduce an actual append after the
            // clean timeout, not refusal caused by an invalid native target.
            assert_eq!(
                run_clean_timeout_case(pause, status, stable, current, false),
                1
            );
            assert_eq!(
                run_clean_timeout_case(pause, status, stable, current, true),
                0
            );
        }
        // A valid ordinary observation can outlive reply delivery; a later key
        // still has matching observed identity and its own fresh allowed check.
        assert_eq!(
            run_clean_timeout_case(TimeoutPause::Reply, Status::NotSecure, true, true, true),
            1
        );
    }

    #[test]
    fn retirement_never_removes_a_newer_or_another_targets_publication() {
        let publications = IdentityPublications::new();
        let target = pointer_test_probe(0).target;
        let other = Target {
            focus: target.focus + 1,
            ..target
        };
        let old = publications.publish(target, true, false);
        let other_revision = publications.publish(other, true, false);
        let newer = publications.publish(target, true, false);
        publications.retire(target, old);
        publications.retire(other, newer);
        assert_eq!(
            publications.at_admission(target, publications.frontier()),
            Some(newer)
        );
        assert_eq!(
            publications.at_admission(other, publications.frontier()),
            Some(other_revision)
        );
        publications.retire(target, newer);
        assert_eq!(
            publications.at_admission(target, publications.frontier()),
            None
        );
        let next = publications.publish(target, false, false);
        assert_ne!(next, old);
        assert_ne!(next, newer);
        publications.retire(target, newer);
        assert_eq!(
            publications.at_admission(target, publications.frontier()),
            Some(next)
        );
        assert_eq!(
            publications.at_admission(other, publications.frontier()),
            Some(other_revision)
        );
        assert!(!capture_metadata_allows(
            Some(old),
            published_test_verdict(next, false)
        ));
        assert!(!capture_metadata_allows(
            Some(newer),
            published_test_verdict(next, false)
        ));
        assert!(capture_metadata_allows(
            Some(next),
            published_test_verdict(next, false)
        ));
    }

    fn run_superseded_observation_case(
        status: Status,
        stable: bool,
        deadline_valid: bool,
        native_current: bool,
        element_changed: bool,
        old_policy: bool,
    ) -> (usize, usize) {
        use std::sync::mpsc;

        enum Action {
            Observe,
            Capture {
                generation: u64,
                revision: Option<u64>,
                pending: PendingCapture,
            },
        }

        ring_buffer::clear_secure_input_data();
        let old_generation = capture_generation();
        let request = pointer_test_probe(old_generation);
        let publications = Arc::new(IdentityPublications::new());
        publications.publish(request.target, true, false);
        let worker_publications = Arc::clone(&publications);
        let deadline = Arc::new(AtomicBool::new(true));
        let native = Arc::new(AtomicBool::new(true));
        let worker_deadline = Arc::clone(&deadline);
        let worker_native = Arc::clone(&native);
        let (blocked, observing) = mpsc::channel();
        let (finish, resume) = mpsc::channel();
        let (completed, results) = mpsc::channel();
        let worker = BoundedWorker::start_with_expiration(
            "superseded-identity-test",
            2,
            move || {
                move |action| {
                    let appended = match action {
                        Action::Observe => {
                            let verdict = probe_observed_identity(
                                element_changed,
                                || validate_capture_generation(old_generation),
                                || {
                                    worker_deadline.load(Ordering::Acquire)
                                        && worker_native.load(Ordering::Acquire)
                                        // The old policy incorrectly included
                                        // navigation in metadata invalidity.
                                        && (!old_policy || validate_capture_generation(old_generation))
                                },
                                || worker_publications.publish(request.target, element_changed, false),
                                |revision| worker_publications.retire(request.target, revision),
                                || {
                                    blocked.send(()).unwrap();
                                    resume.recv_timeout(Duration::from_secs(2)).unwrap();
                                    status
                                },
                                || stable,
                            );
                            assert_eq!(verdict.status, Status::Unknown);
                            assert_eq!(complete_refresh(request, verdict, || true), Status::Unknown);
                            0
                        }
                        Action::Capture { generation, revision, pending } => {
                            let appended = if !validate_capture_generation(generation) {
                                // An old key remains invalid after navigation,
                                // even when its metadata revision was retained.
                                0
                            } else {
                                let fresh = probe_observed_identity(
                                    false,
                                    || validate_capture_generation(generation),
                                    || true,
                                    || worker_publications.publish(request.target, false, false),
                                    |revision| worker_publications.retire(request.target, revision),
                                    || Status::NotSecure,
                                    || true,
                                );
                                if capture_metadata_allows(revision, fresh) {
                                    ring_buffer::push_text("x");
                                    1
                                } else {
                                    reject_probe(Probe { generation, ..request });
                                    0
                                }
                            };
                            drop(pending);
                            appended
                        }
                    };
                    completed.send(appended).unwrap();
                    appended
                }
            },
            |_| {},
        ).unwrap();
        assert!(worker.submit(Action::Observe, Duration::from_secs(2)));
        observing.recv_timeout(Duration::from_secs(2)).unwrap();
        let enqueue = |generation| {
            let frontier = publications.frontier();
            let _dispatch = begin_capture_dispatch(generation).unwrap();
            let revision = publications.at_admission(request.target, frontier);
            assert!(revision.is_some());
            assert!(worker.submit(
                Action::Capture {
                    generation,
                    revision,
                    pending: PendingCapture::new(generation),
                },
                Duration::from_secs(2)
            ));
            revision
        };
        let old_revision = enqueue(old_generation);
        // A redundant pointer/navigation boundary advances the real raw epoch
        // while the provider still observes the same native/UIA field.
        ring_buffer::invalidate_capture_context();
        let new_generation = capture_generation();
        assert_ne!(old_generation, new_generation);
        assert_eq!(enqueue(new_generation), old_revision);
        deadline.store(deadline_valid, Ordering::Release);
        native.store(native_current, Ordering::Release);
        finish.send(()).unwrap();
        assert_eq!(results.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        let old_appended = results.recv_timeout(Duration::from_secs(2)).unwrap();
        let new_appended = results.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!validate_capture_generation(old_generation));
        if new_appended == 0 {
            assert!(!validate_capture_generation(new_generation));
            ring_buffer::push_text("late-suffix");
            assert!(ring_buffer::runs_snapshot().is_empty());
        } else {
            assert!(validate_capture_generation(new_generation));
            assert_eq!(
                ring_buffer::runs_snapshot()
                    .iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>(),
                "x"
            );
        }
        ring_buffer::clear_secure_input_data();
        (old_appended, new_appended)
    }

    #[test]
    fn navigation_does_not_retire_otherwise_valid_observed_identity() {
        let _guard = ring_buffer::test_guard();
        for changed in [false, true] {
            // The old retirement rule really refuses an otherwise valid new
            // segment; retaining identity must not resurrect the old raw key.
            assert_eq!(
                run_superseded_observation_case(Status::NotSecure, true, true, true, changed, true),
                (0, 0)
            );
            assert_eq!(
                run_superseded_observation_case(
                    Status::NotSecure,
                    true,
                    true,
                    true,
                    changed,
                    false
                ),
                (0, 1)
            );
            for (status, stable, deadline, native) in [
                (Status::Secure, true, true, true),
                (Status::Unknown, true, true, true),
                (Status::NotSecure, false, true, true),
                (Status::NotSecure, true, false, true),
                (Status::NotSecure, true, true, false),
            ] {
                assert_eq!(
                    run_superseded_observation_case(
                        status, stable, deadline, native, changed, false
                    ),
                    (0, 0)
                );
            }
        }
    }

    #[test]
    fn publication_after_hook_entry_cannot_label_its_target_or_snapshot() {
        let publications = IdentityPublications::new();
        let target = pointer_test_probe(0).target;
        let initial = publications.publish(target, true, false);
        let entry = publications.frontier();
        let snapshot = publications.at_admission(target, entry);
        assert_eq!(snapshot, Some(initial));
        let changed = publications.publish(target, true, false);
        assert_ne!(initial, changed);
        // Publication can occur while native target lookup is still running,
        // or after its raw snapshot. Neither path may inherit the new identity.
        assert_eq!(publications.at_admission(target, entry), None);
        let fresh = published_test_verdict(changed, false);
        assert!(!capture_metadata_allows(None, fresh));
        assert!(!capture_metadata_allows(snapshot, fresh));
        let later = publications.at_admission(target, publications.frontier());
        assert!(capture_metadata_allows(later, fresh));
        assert_eq!(publications.publish(target, false, false), changed);
    }

    #[test]
    fn absent_or_contended_publications_never_supply_a_default_identity() {
        let publications = IdentityPublications::new();
        let target = pointer_test_probe(0).target;
        assert_eq!(
            publications.at_admission(target, publications.frontier()),
            None
        );
        let revision = publications.publish(target, true, false);
        let locked = publications.targets.lock().unwrap();
        let start = Instant::now();
        let missing = publications.at_admission(target, publications.frontier());
        assert!(start.elapsed() < Duration::from_millis(100));
        assert_eq!(missing, None);
        drop(locked);
        assert!(!capture_metadata_allows(
            missing,
            published_test_verdict(revision, false)
        ));
        assert!(!capture_metadata_allows(Some(revision), Verdict::UNKNOWN));
        for status in [Status::Secure, Status::Unknown] {
            assert!(!capture_metadata_allows(
                Some(revision),
                Verdict {
                    status,
                    ..published_test_verdict(revision, false)
                }
            ));
        }
    }

    #[test]
    fn evicted_or_reused_native_targets_never_reuse_a_publication() {
        let publications = IdentityPublications::new();
        let target = pointer_test_probe(0).target;
        let original = publications.publish(target, true, false);
        for index in 0..IDENTITY_CACHE_CAPACITY {
            publications.publish(
                Target {
                    focus: index + 2,
                    ..target
                },
                true,
                false,
            );
        }
        assert!(publications.targets.lock().unwrap().len() <= IDENTITY_CACHE_CAPACITY);
        assert_eq!(
            publications.at_admission(target, publications.frontier()),
            None
        );
        let republished = publications.publish(target, false, false);
        assert_ne!(original, republished);
        assert!(!capture_metadata_allows(
            Some(original),
            published_test_verdict(republished, false)
        ));
        let reused_handle = Target {
            process: target.process + 1,
            ..target
        };
        assert_eq!(
            publications.at_admission(reused_handle, publications.frontier()),
            None
        );
        let replacement = publications.publish(reused_handle, true, false);
        assert_ne!(republished, replacement);
        let forgotten = publications.publish(target, false, true);
        assert_ne!(republished, forgotten);
        assert_eq!(
            publications.at_admission(reused_handle, publications.frontier()),
            None
        );
    }

    #[test]
    fn publication_before_snapshot_submission_cannot_reauthorize_the_key() {
        use std::{sync::mpsc, thread};

        let _guard = ring_buffer::test_guard();
        ring_buffer::clear_secure_input_data();
        let generation = capture_generation();
        let request = pointer_test_probe(generation);
        let publications = Arc::new(IdentityPublications::new());
        let old = publications.publish(request.target, true, false);
        let snapshot_publications = Arc::clone(&publications);
        let (started, observed) = mpsc::channel();
        let (finish, resume) = mpsc::channel();
        let snapshot = thread::spawn(move || {
            let frontier = snapshot_publications.frontier();
            let _dispatch = begin_capture_dispatch(generation).unwrap();
            let revision = snapshot_publications.at_admission(request.target, frontier);
            started.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(2)).unwrap();
            (revision, PendingCapture::new(generation))
        });
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!pending_capture_for_generation(generation));
        // A provider can finish an older Refresh while the raw snapshot has
        // not yet registered its lease or entered the capture FIFO.
        let promoted = publications.publish(request.target, true, false);
        assert_ne!(old, promoted);
        assert_eq!(
            complete_refresh(request, published_test_verdict(promoted, true), || true),
            Status::NotSecure
        );
        finish.send(()).unwrap();
        let (captured, pending) = snapshot.join().unwrap();
        assert_eq!(captured, Some(old));
        assert!(!capture_metadata_allows(
            captured,
            published_test_verdict(promoted, false)
        ));
        reject_probe(request);
        assert!(!validate_capture_generation(generation));
        ring_buffer::push_text("late-suffix");
        assert!(ring_buffer::runs_snapshot().is_empty());
        drop(pending);
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn confirmed_password_observation_cannot_use_pristine_recovery() {
        use std::cell::Cell;

        let _guard = ring_buffer::test_guard();
        for status in [Status::Secure, Status::Unknown] {
            ring_buffer::clear_secure_input_data();
            let raw = capture_generation();
            let context = generation();
            let recovered_observation = Cell::new(false);
            assert_eq!(
                apply_refresh_metadata(
                    pointer_test_probe(raw),
                    Verdict {
                        status,
                        element_changed: false,
                        identity: MetadataIdentity::Published(1),
                    },
                    |request| {
                        // Isolate the shared status policy from live native
                        // focus. Model the successful pristine-observation path:
                        // raw input survives, but old conversion guards expire.
                        recovered_observation.set(true);
                        ring_buffer::discard_changed_capture_context(request.generation);
                    },
                ),
                status
            );
            assert!(!validate_generation(context));
            assert_eq!(recovered_observation.get(), status == Status::Unknown);
            assert_eq!(validate_capture_generation(raw), status == Status::Unknown);

            // The key entered the confirmed password field. Its queued query
            // now sees the same element after it was changed to ordinary text.
            let captured = capture_generation();
            let pending = {
                let _dispatch = begin_capture_dispatch(captured).unwrap();
                PendingCapture::new(captured)
            };
            assert_eq!(
                complete_refresh(
                    pointer_test_probe(captured),
                    Verdict {
                        status: Status::NotSecure,
                        element_changed: false,
                        identity: MetadataIdentity::Published(1),
                    },
                    || true,
                ),
                Status::NotSecure
            );
            assert!(validate_capture_generation(captured));
            ring_buffer::push_text("queued-key");
            drop(pending);
            // Unknown is a positive control: synthetic decoding must really
            // append, so the Secure rejection cannot pass through a fake HWND.
            assert_eq!(
                ring_buffer::runs_snapshot().is_empty(),
                status == Status::Secure
            );
        }
        ring_buffer::clear_secure_input_data();
    }

    fn run_publication_fifo_case(
        pointer: bool,
        changed: bool,
        key_during_probe: bool,
        enforce_revision: bool,
    ) -> (Status, usize, bool) {
        use std::sync::mpsc;

        enum Action {
            Refresh,
            Capture {
                generation: u64,
                identity_revision: Option<u64>,
                pending: PendingCapture,
            },
        }

        ring_buffer::clear_secure_input_data();
        let generation = capture_generation();
        let request = pointer_test_probe(generation);
        let publications = Arc::new(IdentityPublications::new());
        publications.publish(request.target, true, false);
        let worker_publications = Arc::clone(&publications);
        let focused = Arc::new(AtomicUsize::new(usize::from(!changed)));
        let worker_focus = Arc::clone(&focused);
        let appended = Arc::new(AtomicUsize::new(0));
        let worker_appended = Arc::clone(&appended);
        let (waiting, observed_wait) = mpsc::channel();
        let (focus_changed, resume_probe) = mpsc::channel();
        let (completed, results) = mpsc::channel();
        let worker = BoundedWorker::start_with_expiration(
            "identity-admission-test",
            8,
            move || {
                move |action| {
                    let result = match action {
                        Action::Refresh => {
                            // Pause the real FIFO while the old identity is still
                            // published. This models either a focused Refresh or
                            // pointer COM work preceding anchor confirmation.
                            if pointer && changed {
                                assert_eq!(
                                    wait_for_pointer_focus(
                                        1,
                                        |anchor| Some(
                                            *anchor == worker_focus.load(Ordering::Acquire)
                                        ),
                                        || {
                                            waiting.send(()).unwrap();
                                            resume_probe
                                                .recv_timeout(Duration::from_secs(2))
                                                .unwrap();
                                            true
                                        },
                                    ),
                                    Some(1)
                                );
                            } else {
                                waiting.send(()).unwrap();
                                resume_probe.recv_timeout(Duration::from_secs(2)).unwrap();
                            }
                            if pointer && !changed {
                                assert_eq!(
                                    wait_for_pointer_focus(
                                        1,
                                        |anchor| Some(
                                            *anchor == worker_focus.load(Ordering::Acquire)
                                        ),
                                        || panic!("the frozen anchor should now own focus"),
                                    ),
                                    Some(1)
                                );
                            }
                            let revision =
                                worker_publications.publish(request.target, changed, false);
                            complete_refresh(
                                request,
                                published_test_verdict(revision, changed),
                                || true,
                            )
                        }
                        Action::Capture {
                            generation,
                            identity_revision,
                            pending,
                        } => {
                            // Synthetic decoding isolates admission from Windows
                            // focus/keyboard setup. Use the production revision
                            // policy, real epoch and journal, with an old-policy
                            // positive control that must actually append.
                            let revision =
                                worker_publications.publish(request.target, false, false);
                            let fresh = published_test_verdict(revision, false);
                            let allowed = !enforce_revision
                                || capture_metadata_allows(identity_revision, fresh);
                            let result = if !validate_capture_generation(generation) {
                                Status::Unknown
                            } else if allowed {
                                ring_buffer::push_text("x");
                                if !ring_buffer::runs_snapshot().is_empty() {
                                    worker_appended.fetch_add(1, Ordering::Relaxed);
                                }
                                Status::NotSecure
                            } else {
                                reject_probe(Probe {
                                    generation,
                                    ..request
                                });
                                Status::Unknown
                            };
                            drop(pending);
                            result
                        }
                    };
                    completed.send(result).unwrap();
                    result
                }
            },
            |_| {},
        )
        .unwrap();
        assert!(worker.submit(Action::Refresh, Duration::from_secs(2)));
        observed_wait.recv_timeout(Duration::from_secs(2)).unwrap();
        let enqueue_capture = || {
            let frontier = publications.frontier();
            let _dispatch = begin_capture_dispatch(generation).unwrap();
            let identity_revision = publications.at_admission(request.target, frontier);
            for _ in 0..3 {
                assert!(worker.submit(
                    Action::Capture {
                        generation,
                        identity_revision,
                        pending: PendingCapture::new(generation),
                    },
                    Duration::from_secs(2)
                ));
            }
        };
        if key_during_probe {
            enqueue_capture();
        }
        focused.store(1, Ordering::Release);
        focus_changed.send(()).unwrap();
        let refresh_result = results.recv_timeout(Duration::from_secs(2)).unwrap();
        if !key_during_probe {
            enqueue_capture();
        }
        for _ in 0..3 {
            let _ = results.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let appended = appended.load(Ordering::Relaxed);
        let current = validate_capture_generation(generation);
        if appended == 0 {
            ring_buffer::push_text("late-suffix");
            assert!(ring_buffer::runs_snapshot().is_empty());
        }
        ring_buffer::clear_secure_input_data();
        (refresh_result, appended, current)
    }

    #[test]
    fn identity_promotion_cannot_relabel_an_already_queued_key() {
        let _guard = ring_buffer::test_guard();
        for pointer in [false, true] {
            // An older queued Refresh and a waiting pointer anchor must both
            // reject password-A input after they publish ordinary B. Disabling
            // only revision binding reproduces the old erroneous append.
            assert_eq!(
                run_publication_fifo_case(pointer, true, true, false),
                (Status::NotSecure, 3, true)
            );
            assert_eq!(
                run_publication_fifo_case(pointer, true, true, true),
                (Status::NotSecure, 0, false)
            );
            assert_eq!(
                run_publication_fifo_case(pointer, true, false, true),
                (Status::NotSecure, 3, true)
            );
            // A known, unchanged ordinary B remains usable while its pointer
            // metadata is queried. Every key still needs its own fresh verdict.
            assert_eq!(
                run_publication_fifo_case(pointer, false, true, true),
                (Status::NotSecure, 3, true)
            );
        }
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
                identity: MetadataIdentity::Native,
            },
            || true,
        );
        assert_eq!(result, Status::NotSecure);
        let native = Verdict {
            status: Status::NotSecure,
            element_changed: false,
            identity: MetadataIdentity::Native,
        };
        assert!(capture_metadata_allows(None, native));
        assert!(capture_metadata_allows(Some(u64::MAX), native));
        for status in [Status::Secure, Status::Unknown] {
            assert!(!capture_metadata_allows(None, Verdict { status, ..native }));
        }
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
        assert_eq!(completion.join().unwrap(), verdict.status);
        assert!(!validate_capture_generation(generation));
        ring_buffer::push_text("late-suffix");
        assert!(ring_buffer::runs_snapshot().is_empty());
        drop(pending);
        ring_buffer::clear_secure_input_data();
    }

    #[test]
    fn refresh_completion_waits_for_started_snapshot_to_register_its_lease() {
        for status in [Status::Secure, Status::Unknown] {
            let verdict = Verdict {
                status,
                ..Verdict::UNKNOWN
            };
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
    fn contended_key_cannot_be_reopened_by_a_denied_observation() {
        for status in [Status::Secure, Status::Unknown] {
            let verdict = Verdict {
                status,
                ..Verdict::UNKNOWN
            };
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
