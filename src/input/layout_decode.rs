//! Decode physical input using prepared immutable tables and private composition.
//!
//! Trust verification and table parsing run on a separate preparation worker.
//! The ordered decoder never waits for preparation: a missing table invalidates
//! capture until it is ready, so an unknown first glyph cannot leave a known suffix.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MAPVK_VK_TO_VSC, MapVirtualKeyExW, ToUnicodeEx, VK_SPACE,
};
use windows::Win32::UI::{
    Input::KeyboardAndMouse::{GetKeyboardLayout, GetKeyboardLayoutList, HKL},
    WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

use crate::layout_tables::{Composition, KeyboardTables, Translation};

const DECODE_TIMEOUT: Duration = Duration::from_millis(100);
const PREPARATION_TIMEOUT: Duration = Duration::from_secs(3);
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const REFRESH_BUDGET: Duration = Duration::from_millis(250);
const FAILED_RETRY_INTERVAL: Duration = Duration::from_secs(60);
const PREPARATION_QUEUE_CAPACITY: usize = 8;
const MAX_BACKGROUND_PREPARATIONS: usize = 4;
const MAX_INSTALLED_LAYOUTS: usize = 1024;
#[cfg(test)]
const UTF16_CAPACITY: usize = 16;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum DecodeResult {
    Text(String),
    PendingDeadKey,
    NoText,
    /// The entire journal must remain invalid, including later known suffixes.
    Unavailable,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Context {
    layout: usize,
    focus: (usize, usize),
    generation: u64,
}

struct Request {
    vk: u32,
    scan: u32,
    state: [u8; 256],
    context: Context,
    deadline: Instant,
    reply: SyncSender<DecodeResult>,
}

struct CachedTable {
    table: Option<Arc<KeyboardTables>>,
    retry_after: Instant,
}

#[derive(Default)]
struct TableCache {
    tables: HashMap<usize, CachedTable>,
    pending: HashSet<usize>,
}

impl TableCache {
    fn needs_preparation(&self, layout: usize, now: Instant) -> bool {
        self.tables
            .get(&layout)
            .is_none_or(|entry| entry.table.is_none() && now >= entry.retry_after)
    }
}

struct PreparationRequest {
    layouts: Vec<usize>,
    deadline: Instant,
    reply: Option<SyncSender<bool>>,
}

struct Decoder {
    sender: SyncSender<Request>,
    prepare_sender: SyncSender<PreparationRequest>,
    generation: Arc<AtomicU64>,
}

static DECODER: OnceLock<Option<Decoder>> = OnceLock::new();

/// Prepare the current catalog before installing the keyboard hook. This is a
/// bounded startup operation, never a key-event operation. False means the
/// deadline or an unsupported layout left only partial (possibly no) readiness;
/// already prepared layouts remain usable and every other layout fails closed.
pub(crate) fn warm() -> bool {
    let layouts = current_layouts();
    if let [layout] = layouts.as_slice() {
        return prepare_layout(*layout);
    }
    decoder().is_some_and(|decoder| decoder.prepare_many(layouts))
}

/// Explicitly prepare a valid HKL outside keyboard capture. This also supports
/// handles returned by LoadKeyboardLayout but absent from a worker's catalog.
pub(crate) fn prepare_layout(layout: usize) -> bool {
    decoder().is_some_and(|decoder| decoder.prepare_many(vec![layout]))
}

/// Discard pending composition after edits that invalidate the input journal.
pub(crate) fn reset() {
    if let Some(Some(decoder)) = DECODER.get() {
        decoder.invalidate();
    }
}

pub(crate) fn decode(
    vk: u32,
    scan: u32,
    state: [u8; 256],
    hkl: usize,
    focus_key: (usize, usize),
) -> DecodeResult {
    // Starting threads or waiting for startup readiness belongs before the
    // hook. A caller that skipped initialization must not initialize here.
    let Some(Some(decoder)) = DECODER.get() else {
        return DecodeResult::Unavailable;
    };
    decoder.decode(vk, scan, state, hkl, focus_key)
}

/// Exercise a first key through the production decoder without sharing the
/// application's journal generation with unrelated parallel test fixtures.
#[cfg(test)]
pub(super) fn decode_probe(
    vk: u32,
    scan: u32,
    state: [u8; 256],
    hkl: usize,
    focus_key: (usize, usize),
) -> DecodeResult {
    thread_local! {
        static PROBE: std::cell::OnceCell<Option<Decoder>> = const { std::cell::OnceCell::new() };
    }
    PROBE.with(|probe| {
        let Some(decoder) = probe.get_or_init(|| Decoder::start().ok()) else {
            return DecodeResult::Unavailable;
        };
        decoder.invalidate();
        if !decoder.prepare_many(vec![hkl]) {
            return DecodeResult::Unavailable;
        }
        decoder.decode(vk, scan, state, hkl, focus_key)
    })
}

fn decoder() -> Option<&'static Decoder> {
    DECODER.get_or_init(|| Decoder::start().ok()).as_ref()
}

impl Decoder {
    fn start() -> std::io::Result<Self> {
        let tables = Arc::new(Mutex::new(TableCache::default()));
        let preparation_tables = Arc::clone(&tables);
        let (prepare_sender, prepare_receiver) =
            mpsc::sync_channel::<PreparationRequest>(PREPARATION_QUEUE_CAPACITY);
        thread::Builder::new()
            .name("keyboard-table-preparation".to_owned())
            .spawn(move || {
                loop {
                    match prepare_receiver.recv_timeout(REFRESH_INTERVAL) {
                        Ok(request) => {
                            let ready = prepare_tables(
                                &preparation_tables,
                                &request.layouts,
                                request.deadline,
                                MAX_INSTALLED_LAYOUTS,
                            );
                            if let Some(reply) = request.reply {
                                let _ = reply.send(ready);
                            } else {
                                // Only the queued asynchronous request owns
                                // these markers. A background or synchronous
                                // batch must not release another request's slot.
                                let mut cache = preparation_tables
                                    .lock()
                                    .unwrap_or_else(|poison| poison.into_inner());
                                for layout in request.layouts {
                                    cache.pending.remove(&layout);
                                }
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let _ = prepare_tables(
                                &preparation_tables,
                                &current_layouts(),
                                Instant::now() + REFRESH_BUDGET,
                                MAX_BACKGROUND_PREPARATIONS,
                            );
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;

        let (sender, receiver) = mpsc::sync_channel::<Request>(1);
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        let worker_preparer = prepare_sender.clone();
        thread::Builder::new()
            .name("input-composition".to_owned())
            .spawn(move || {
                let mut context: Option<Context> = None;
                let mut current_table: Option<Arc<KeyboardTables>> = None;
                let mut composition = Composition::default();
                while let Ok(request) = receiver.recv() {
                    if request.deadline <= Instant::now()
                        || request.context.generation != worker_generation.load(Ordering::Acquire)
                    {
                        context = None;
                        current_table = None;
                        composition = Composition::default();
                        continue;
                    }

                    // Cache contention is resolved off the hook thread, within
                    // the existing caller deadline. Reuse the current immutable
                    // table without locking while the input context is stable.
                    let table = current_table
                        .as_ref()
                        .filter(|_| context == Some(request.context))
                        .cloned()
                        .or_else(|| ready_table(&tables, &worker_preparer, request.context.layout));
                    let Some(table) = table else {
                        context = None;
                        current_table = None;
                        composition = Composition::default();
                        let _ = request.reply.send(DecodeResult::Unavailable);
                        continue;
                    };
                    if context != Some(request.context)
                        || current_table
                            .as_ref()
                            .is_none_or(|old| !Arc::ptr_eq(old, &table))
                    {
                        composition = Composition::default();
                        context = Some(request.context);
                        current_table = Some(Arc::clone(&table));
                    }

                    let result = match table.translate(
                        request.vk,
                        request.scan,
                        &request.state,
                        &mut composition,
                    ) {
                        Some(Translation::Text(text)) => DecodeResult::Text(text),
                        Some(Translation::PendingDeadKey) => {
                            debug_assert!(composition.is_pending());
                            DecodeResult::PendingDeadKey
                        }
                        Some(Translation::NoText) => DecodeResult::NoText,
                        None => DecodeResult::Unavailable,
                    };
                    if request.deadline > Instant::now()
                        && request.context.generation == worker_generation.load(Ordering::Acquire)
                    {
                        let _ = request.reply.send(result);
                    } else {
                        context = None;
                        current_table = None;
                        composition = Composition::default();
                    }
                }
            })?;
        Ok(Self {
            sender,
            prepare_sender,
            generation,
        })
    }

    fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    fn prepare_many(&self, mut layouts: Vec<usize>) -> bool {
        let mut seen = HashSet::new();
        layouts.retain(|layout| *layout != 0 && seen.insert(*layout));
        if layouts.is_empty() || layouts.len() > MAX_INSTALLED_LAYOUTS {
            return false;
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let request = PreparationRequest {
            layouts,
            deadline: Instant::now() + PREPARATION_TIMEOUT,
            reply: Some(reply),
        };
        if self.prepare_sender.try_send(request).is_err() {
            return false;
        }
        receiver.recv_timeout(PREPARATION_TIMEOUT).unwrap_or(false)
    }

    fn decode(
        &self,
        vk: u32,
        scan: u32,
        state: [u8; 256],
        layout: usize,
        focus: (usize, usize),
    ) -> DecodeResult {
        let started = Instant::now();
        if layout == 0 {
            self.invalidate();
            return DecodeResult::Unavailable;
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let generation = self.generation.load(Ordering::Acquire);
        let request = Request {
            vk,
            scan,
            state,
            context: Context {
                layout,
                focus,
                generation,
            },
            deadline: started + DECODE_TIMEOUT,
            reply,
        };
        if self.sender.try_send(request).is_err() {
            self.invalidate();
            return DecodeResult::Unavailable;
        }
        let Some(remaining) = DECODE_TIMEOUT.checked_sub(started.elapsed()) else {
            self.invalidate();
            return DecodeResult::Unavailable;
        };
        match receiver.recv_timeout(remaining) {
            Ok(result)
                if result != DecodeResult::Unavailable
                    && generation == self.generation.load(Ordering::Acquire) =>
            {
                result
            }
            _ => {
                self.invalidate();
                DecodeResult::Unavailable
            }
        }
    }
}

/// Called only by the decoder worker. The shared mutex protects small cache
/// operations, never registry access, trust verification or table parsing.
fn ready_table(
    tables: &Mutex<TableCache>,
    prepare_sender: &SyncSender<PreparationRequest>,
    layout: usize,
) -> Option<Arc<KeyboardTables>> {
    let mut cache = tables.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(table) = cache
        .tables
        .get(&layout)
        .and_then(|entry| entry.table.as_ref())
    {
        return Some(Arc::clone(table));
    }
    if cache.needs_preparation(layout, Instant::now()) && cache.pending.insert(layout) {
        let request = PreparationRequest {
            layouts: vec![layout],
            deadline: Instant::now() + PREPARATION_TIMEOUT,
            reply: None,
        };
        if prepare_sender.try_send(request).is_err() {
            cache.pending.remove(&layout);
        }
    }
    None
}

fn prepare_tables(
    tables: &Mutex<TableCache>,
    layouts: &[usize],
    deadline: Instant,
    maximum_preparations: usize,
) -> bool {
    let mut prepared = 0;
    let mut all_ready = !layouts.is_empty();
    for &layout in layouts {
        let needs_preparation = {
            let cache = tables.lock().unwrap_or_else(|poison| poison.into_inner());
            cache.needs_preparation(layout, Instant::now())
        };
        if needs_preparation && prepared < maximum_preparations && Instant::now() < deadline {
            // No cache mutex or decoder state is held during registry access,
            // cryptographic verification, DLL loading or bounded table parsing.
            let table = KeyboardTables::load(layout).map(Arc::new);
            let mut cache = tables.lock().unwrap_or_else(|poison| poison.into_inner());
            cache.tables.insert(
                layout,
                CachedTable {
                    table,
                    retry_after: Instant::now() + FAILED_RETRY_INTERVAL,
                },
            );
            prepared += 1;
        }
        let cache = tables.lock().unwrap_or_else(|poison| poison.into_inner());
        all_ready &= cache
            .tables
            .get(&layout)
            .is_some_and(|entry| entry.table.is_some());
    }
    all_ready && Instant::now() < deadline
}

fn current_layouts() -> Vec<usize> {
    let count = unsafe { GetKeyboardLayoutList(None) };
    let mut layouts = Vec::new();
    if let Ok(count) = usize::try_from(count)
        && (1..=MAX_INSTALLED_LAYOUTS).contains(&count)
    {
        let mut handles = vec![HKL::default(); count];
        let copied = unsafe { GetKeyboardLayoutList(Some(&mut handles)) };
        if let Ok(copied) = usize::try_from(copied) {
            layouts.extend(
                handles
                    .into_iter()
                    .take(copied)
                    .filter_map(|layout| (!layout.0.is_null()).then_some(layout.0 as usize)),
            );
        }
    }
    // Explicit foreground identity supplements the calling thread's catalog.
    let foreground = unsafe { GetForegroundWindow() };
    let thread_id = if foreground.0.is_null() {
        0
    } else {
        unsafe { GetWindowThreadProcessId(foreground, None) }
    };
    let foreground_layout = unsafe { GetKeyboardLayout(thread_id) };
    if !foreground_layout.0.is_null() {
        let raw = foreground_layout.0 as usize;
        layouts.retain(|layout| *layout != raw);
        layouts.insert(0, raw);
    }
    layouts
}

#[cfg(test)]
fn clear_dead_keys(layout: usize) -> bool {
    if layout == 0 {
        return true;
    }
    let hkl = HKL(layout as *mut _);
    let scan = unsafe { MapVirtualKeyExW(u32::from(VK_SPACE.0), MAPVK_VK_TO_VSC, Some(hkl)) };
    let state = [0u8; 256];
    let mut output = [0u16; UTF16_CAPACITY];
    for _ in 0..8 {
        if unsafe {
            ToUnicodeEx(
                u32::from(VK_SPACE.0),
                scan,
                &state,
                &mut output,
                0,
                Some(hkl),
            )
        } >= 0
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use windows::{
        Win32::UI::Input::KeyboardAndMouse::{
            KLF_NOTELLSHELL, LoadKeyboardLayoutW, MAPVK_VSC_TO_VK_EX, VK_CONTROL, VK_LCONTROL,
            VK_MENU, VK_RMENU,
        },
        core::w,
    };

    use super::*;

    fn german_layout() -> usize {
        unsafe { LoadKeyboardLayoutW(w!("00000407"), KLF_NOTELLSHELL) }
            .expect("German layout must be available on the Windows runner")
            .0 as usize
    }

    fn decode_scan(
        decoder: &Decoder,
        scan: u32,
        layout: usize,
        focus: (usize, usize),
    ) -> DecodeResult {
        let vk = unsafe { MapVirtualKeyExW(scan, MAPVK_VSC_TO_VK_EX, Some(HKL(layout as *mut _))) };
        decoder.decode(vk, scan, [0; 256], layout, focus)
    }

    fn prepare(decoder: &Decoder, layout: usize) {
        assert!(
            decoder.prepare_many(vec![layout]),
            "trust and table readiness must finish before the first captured key"
        );
    }

    #[test]
    fn native_decoder_rejects_reply_after_capture_generation_changes() {
        let (sender, requests) = mpsc::sync_channel::<Request>(1);
        let (prepare_sender, _preparations) = mpsc::sync_channel(1);
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        let decoder = Decoder {
            sender,
            prepare_sender,
            generation,
        };
        let worker = thread::spawn(move || {
            let request = requests.recv().unwrap();
            let result = DecodeResult::Text("e".to_owned());
            // Simulate invalidation after result production but before the
            // caller accepts the reply. No scheduler timing or sleep is needed.
            worker_generation.fetch_add(1, Ordering::AcqRel);
            request.reply.send(result).unwrap();
        });
        assert_eq!(
            decoder.decode(u32::from(b'E'), 0x12, [0; 256], 1, (1, 2)),
            DecodeResult::Unavailable
        );
        worker.join().unwrap();
    }

    #[test]
    fn native_decoder_background_preparation_preserves_queued_request_ownership() {
        let tables = Mutex::new(TableCache::default());
        let (sender, requests) = mpsc::sync_channel(PREPARATION_QUEUE_CAPACITY);
        let layout = 1;
        assert!(ready_table(&tables, &sender, layout).is_none());
        let _queued_request = requests.try_recv().unwrap();

        // A refresh may encounter an HKL that already has an explicit async
        // request waiting. It must not release that request's deduplication slot.
        assert!(!prepare_tables(&tables, &[layout], Instant::now(), 0));
        assert!(ready_table(&tables, &sender, layout).is_none());
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(tables.lock().unwrap().pending.contains(&layout));
    }

    #[test]
    fn native_decoder_composes_dead_key_on_its_own_thread() {
        let _guard = crate::input::ring_buffer::test_guard();
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
        prepare(&decoder, german);
        assert_eq!(
            decode_scan(&decoder, 0x29, german, (1, 2)),
            DecodeResult::PendingDeadKey
        );
        assert_eq!(
            decode_scan(&decoder, 0x12, german, (1, 2)),
            DecodeResult::Text("ê".to_owned())
        );
    }

    #[test]
    fn native_decoder_discards_pending_accent_when_focus_changes() {
        let _guard = crate::input::ring_buffer::test_guard();
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
        prepare(&decoder, german);
        assert_eq!(
            decode_scan(&decoder, 0x29, german, (1, 2)),
            DecodeResult::PendingDeadKey
        );
        assert_eq!(
            decode_scan(&decoder, 0x12, german, (1, 3)),
            DecodeResult::Text("e".to_owned())
        );
    }

    #[test]
    fn native_decoder_discards_pending_accent_after_invalidation() {
        let _guard = crate::input::ring_buffer::test_guard();
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
        prepare(&decoder, german);
        assert_eq!(
            decode_scan(&decoder, 0x29, german, (1, 2)),
            DecodeResult::PendingDeadKey
        );
        decoder.invalidate();
        assert_eq!(
            decode_scan(&decoder, 0x12, german, (1, 2)),
            DecodeResult::Text("e".to_owned())
        );
    }

    #[test]
    fn native_decoder_keeps_altgr_text() {
        let _guard = crate::input::ring_buffer::test_guard();
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
        prepare(&decoder, german);
        let mut state = [0; 256];
        for key in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
            state[usize::from(key.0)] = 0x80;
        }
        assert_eq!(
            decoder.decode(u32::from(b'Q'), 0x10, state, german, (1, 2)),
            DecodeResult::Text("@".to_owned())
        );
    }

    #[test]
    fn native_decoder_workers_keep_independent_pending_accents() {
        let _guard = crate::input::ring_buffer::test_guard();
        let first = Decoder::start().unwrap();
        let second = Decoder::start().unwrap();
        let german = german_layout();
        prepare(&first, german);
        prepare(&second, german);
        assert_eq!(
            decode_scan(&first, 0x29, german, (1, 2)),
            DecodeResult::PendingDeadKey
        );
        assert_eq!(
            decode_scan(&second, 0x12, german, (1, 2)),
            DecodeResult::Text("e".to_owned())
        );
        assert_eq!(
            decode_scan(&first, 0x12, german, (1, 2)),
            DecodeResult::Text("ê".to_owned())
        );
    }

    #[test]
    fn native_decoder_does_not_consume_callers_pending_dead_key() {
        // Native ToUnicodeEx state is shared with the first-glyph oracle. The
        // actual decoder below still has its own independent private state.
        let _guard = crate::input::ring_buffer::test_guard();
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
        assert!(clear_dead_keys(german));
        let hkl = HKL(german as *mut _);
        let dead_vk = unsafe { MapVirtualKeyExW(0x29, MAPVK_VSC_TO_VK_EX, Some(hkl)) };
        let mut output = [0u16; UTF16_CAPACITY];
        assert!(unsafe { ToUnicodeEx(dead_vk, 0x29, &[0; 256], &mut output, 0, Some(hkl)) } < 0);

        // Real cold preparation must preserve the caller's native dead key,
        // just as the first captured key does after readiness is established.
        prepare(&decoder, german);
        assert_eq!(
            decode_scan(&decoder, 0x12, german, (1, 2)),
            DecodeResult::Text("e".to_owned())
        );
        let count =
            unsafe { ToUnicodeEx(u32::from(b'E'), 0x12, &[0; 256], &mut output, 0, Some(hkl)) };
        assert!(clear_dead_keys(german));
        assert_eq!(count, 1);
        assert_eq!(output[0], 'ê' as u16);
    }

    #[test]
    fn native_decoder_unknown_layout_fails_closed_until_prepared() {
        let _guard = crate::input::ring_buffer::test_guard();
        let german = german_layout();
        let decoder = Decoder::start().unwrap();
        assert_eq!(
            decode_scan(&decoder, 0x29, german, (1, 2)),
            DecodeResult::Unavailable,
            "an unprepared first glyph must invalidate the whole capture"
        );
        prepare(&decoder, german);
        assert_eq!(
            decode_scan(&decoder, 0x12, german, (1, 2)),
            DecodeResult::Text("e".to_owned()),
            "preparation must not replay an unavailable key into private composition"
        );
    }

    #[test]
    fn native_decoder_preparing_another_layout_preserves_pending_composition() {
        let _guard = crate::input::ring_buffer::test_guard();
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
        prepare(&decoder, german);
        assert_eq!(
            decode_scan(&decoder, 0x29, german, (1, 2)),
            DecodeResult::PendingDeadKey
        );
        let english = unsafe { LoadKeyboardLayoutW(w!("00000409"), KLF_NOTELLSHELL) }
            .expect("the native English layout must be available")
            .0 as usize;
        prepare(&decoder, english);
        assert_eq!(
            decode_scan(&decoder, 0x12, german, (1, 2)),
            DecodeResult::Text("ê".to_owned())
        );
    }
}
