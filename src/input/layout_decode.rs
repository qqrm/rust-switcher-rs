//! Decode physical input from immutable driver tables on a bounded worker.
//!
//! Composition belongs to this worker and focus context. No translation or
//! reset can consume a dead key from the real Windows keyboard buffer.

use std::{
    collections::HashMap,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HKL, MAPVK_VK_TO_VSC, MapVirtualKeyExW, ToUnicodeEx, VK_SPACE,
};

use crate::layout_tables::{Composition, KeyboardTables, Translation};

const DECODE_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(test)]
const UTF16_CAPACITY: usize = 16;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum DecodeResult {
    Text(String),
    PendingDeadKey,
    NoText,
    /// The journal must be invalidated because the result is not known.
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

struct Decoder {
    sender: SyncSender<Request>,
    generation: Arc<AtomicU64>,
}

static DECODER: OnceLock<Option<Decoder>> = OnceLock::new();

/// Start the worker before installing the keyboard hook, without waiting for it.
pub(crate) fn warm() -> bool {
    decoder().is_some()
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
    let Some(decoder) = decoder() else {
        return DecodeResult::Unavailable;
    };
    decoder.decode(vk, scan, state, hkl, focus_key)
}

fn decoder() -> Option<&'static Decoder> {
    DECODER.get_or_init(|| Decoder::start().ok()).as_ref()
}

impl Decoder {
    fn start() -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Request>(1);
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&generation);
        thread::Builder::new()
            .name("input-composition".to_owned())
            .spawn(move || {
                let mut context: Option<Context> = None;
                let mut composition = Composition::default();
                let mut tables = HashMap::new();
                while let Ok(request) = receiver.recv() {
                    if request.deadline <= Instant::now()
                        || request.context.generation != worker_generation.load(Ordering::Acquire)
                    {
                        context = None;
                        composition = Composition::default();
                        continue;
                    }

                    if context != Some(request.context) {
                        composition = Composition::default();
                        context = Some(request.context);
                    }

                    let table = tables
                        .entry(request.context.layout)
                        .or_insert_with(|| KeyboardTables::load(request.context.layout));
                    let result = match table.as_ref().and_then(|table| {
                        table.translate(request.vk, request.scan, &request.state, &mut composition)
                    }) {
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
                        composition = Composition::default();
                    }
                }
            })?;
        Ok(Self { sender, generation })
    }

    fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    fn decode(
        &self,
        vk: u32,
        scan: u32,
        state: [u8; 256],
        layout: usize,
        focus: (usize, usize),
    ) -> DecodeResult {
        if layout == 0 {
            self.invalidate();
            return DecodeResult::Unavailable;
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        let request = Request {
            vk,
            scan,
            state,
            context: Context {
                layout,
                focus,
                generation: self.generation.load(Ordering::Acquire),
            },
            deadline: Instant::now() + DECODE_TIMEOUT,
            reply,
        };
        if self.sender.try_send(request).is_err() {
            self.invalidate();
            return DecodeResult::Unavailable;
        }
        match receiver.recv_timeout(DECODE_TIMEOUT) {
            Ok(result) if result != DecodeResult::Unavailable => result,
            _ => {
                self.invalidate();
                DecodeResult::Unavailable
            }
        }
    }
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

    #[test]
    fn native_decoder_composes_dead_key_on_its_own_thread() {
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
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
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
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
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
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
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
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
        let first = Decoder::start().unwrap();
        let second = Decoder::start().unwrap();
        let german = german_layout();
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
        let decoder = Decoder::start().unwrap();
        let german = german_layout();
        assert!(clear_dead_keys(german));
        let hkl = HKL(german as *mut _);
        let dead_vk = unsafe { MapVirtualKeyExW(0x29, MAPVK_VSC_TO_VK_EX, Some(hkl)) };
        let mut output = [0u16; UTF16_CAPACITY];
        assert!(unsafe { ToUnicodeEx(dead_vk, 0x29, &[0; 256], &mut output, 0, Some(hkl)) } < 0);

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
}
