//! Decode physical input on a thread with its own keyboard composition state.
//!
//! `ToUnicodeEx` with state changes enabled is required for dead keys. Keeping it
//! away from the hook/UI thread avoids consuming another window's pending accent.

use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HKL, MAPVK_VK_TO_VSC, MapVirtualKeyExW, ToUnicodeEx, VK_SPACE,
};

const DECODE_TIMEOUT: Duration = Duration::from_millis(100);
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
                // This thread never attaches its input queue to another thread.
                let mut context: Option<Context> = None;
                while let Ok(request) = receiver.recv() {
                    if request.deadline <= Instant::now()
                        || request.context.generation != worker_generation.load(Ordering::Acquire)
                    {
                        clear_context(&mut context);
                        continue;
                    }

                    if context != Some(request.context) {
                        let old_cleared = clear_context(&mut context);
                        if !old_cleared || !clear_dead_keys(request.context.layout) {
                            let _ = request.reply.send(DecodeResult::Unavailable);
                            continue;
                        }
                        context = Some(request.context);
                    }

                    let result = translate(&request);
                    if request.deadline > Instant::now()
                        && request.context.generation == worker_generation.load(Ordering::Acquire)
                    {
                        let _ = request.reply.send(result);
                    } else {
                        clear_context(&mut context);
                    }
                }
                clear_context(&mut context);
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

fn translate(request: &Request) -> DecodeResult {
    let mut output = [0u16; UTF16_CAPACITY];
    let count = unsafe {
        ToUnicodeEx(
            request.vk,
            request.scan,
            &request.state,
            &mut output,
            0,
            Some(HKL(request.context.layout as *mut _)),
        )
    };
    if count < 0 {
        return DecodeResult::PendingDeadKey;
    }
    if count == 0 {
        return DecodeResult::NoText;
    }
    let Ok(count) = usize::try_from(count) else {
        return DecodeResult::Unavailable;
    };
    let Some(units) = output.get(..count) else {
        return DecodeResult::Unavailable;
    };
    match String::from_utf16(units) {
        Ok(text) => DecodeResult::Text(text),
        Err(_) => DecodeResult::Unavailable,
    }
}

fn clear_context(context: &mut Option<Context>) -> bool {
    context.take().is_none_or(|old| clear_dead_keys(old.layout))
}

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
