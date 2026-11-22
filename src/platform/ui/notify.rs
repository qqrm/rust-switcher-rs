mod backend;

use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotifyKind {
    Info,
    Error,
    UpdateFailure,
    Roar,
}
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Notification {
    pub kind: NotifyKind,
    pub title: String,
    pub text: String,
    pub created_ms: u64,
    pub dedupe_key: u64,
}

pub const WM_APP_NOTIFY: u32 = WM_APP + 103;

const DEDUPE_WINDOW_MS: u64 = 2_000;
const MAX_QUEUE_LEN: usize = 256;

static QUEUE: OnceLock<Mutex<VecDeque<Notification>>> = OnceLock::new();
static POSTED: AtomicBool = AtomicBool::new(false);

static LAST_KEY: AtomicU64 = AtomicU64::new(0);
static LAST_MS: AtomicU64 = AtomicU64::new(0);

pub fn on_wm_app_notify(hwnd: windows::Win32::Foundation::HWND) {
    // WM_APP_NOTIFY доставлен, значит "сообщение в полете" больше не актуально.
    // Если очередь за этот тик опустеет полностью, новые push должны снова иметь
    // возможность запостить WM_APP_NOTIFY.
    POSTED.store(false, Ordering::Release);

    backend::on_wm_app_notify(hwnd);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis() as u64
}

fn queue() -> &'static Mutex<VecDeque<Notification>> {
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn hash_key(kind: NotifyKind, title: &str, text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut h);
    title.hash(&mut h);
    text.hash(&mut h);
    h.finish()
}

fn is_fast_duplicate(key: u64, ts_ms: u64) -> bool {
    let last_key = LAST_KEY.load(Ordering::Relaxed);
    let last_ms = LAST_MS.load(Ordering::Relaxed);

    if key == last_key && ts_ms.saturating_sub(last_ms) <= DEDUPE_WINDOW_MS {
        return true;
    }

    LAST_KEY.store(key, Ordering::Relaxed);
    LAST_MS.store(ts_ms, Ordering::Relaxed);
    false
}

fn post_once(hwnd: HWND) {
    if POSTED.swap(true, Ordering::AcqRel) {
        return;
    }

    unsafe {
        if let Err(e) = PostMessageW(Some(hwnd), WM_APP_NOTIFY, WPARAM(0), LPARAM(0)) {
            tracing::warn!(error=?e, "PostMessageW(WM_APP_NOTIFY) failed");
            POSTED.store(false, Ordering::Release);
        }
    }
}

fn lock_queue() -> std::sync::MutexGuard<'static, VecDeque<Notification>> {
    match queue().lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::error!("notify queue mutex poisoned");
            poisoned.into_inner()
        }
    }
}

pub fn push(hwnd: HWND, kind: NotifyKind, title: &str, text: &str) {
    let ts_ms = now_ms();
    let key = hash_key(kind, title, text);

    // Each completed update attempt must re-arm its action, even when its
    // text matches a recently consumed failure notification.
    if is_fast_duplicate(key, ts_ms) && kind != NotifyKind::UpdateFailure {
        return;
    }

    let mut q = lock_queue();

    if q.len() >= MAX_QUEUE_LEN {
        q.pop_front();
    }

    q.push_back(Notification {
        kind,
        title: title.to_owned(),
        text: text.to_owned(),
        created_ms: ts_ms,
        dedupe_key: key,
    });

    drop(q);
    post_once(hwnd);
}

pub fn push_info(hwnd: HWND, title: &str, text: &str) {
    push(hwnd, NotifyKind::Info, title, text);
}

pub fn push_error(hwnd: HWND, title: &str, text: &str) {
    push(hwnd, NotifyKind::Error, title, text);
}

pub(crate) fn push_update_failure(hwnd: HWND, text: &str) {
    push(
        hwnd,
        NotifyKind::UpdateFailure,
        "Rust Switcher update failed",
        text,
    );
}

#[allow(dead_code)]
pub fn push_roar(hwnd: HWND, title: &str, text: &str) {
    push(hwnd, NotifyKind::Roar, title, text);
}

fn drain_for_worker() -> Vec<Notification> {
    const MAX_PER_TICK: usize = 32;

    let mut q = lock_queue();

    let mut out = Vec::with_capacity(MAX_PER_TICK);

    for _ in 0..MAX_PER_TICK {
        let Some(n) = q.pop_front() else {
            break;
        };
        out.push(n);
    }

    out
}

fn has_more() -> bool {
    !lock_queue().is_empty()
}

pub fn repost_if_needed(hwnd: HWND) {
    if has_more() {
        POSTED.store(false, Ordering::Release);
        post_once(hwnd);
    }
}
