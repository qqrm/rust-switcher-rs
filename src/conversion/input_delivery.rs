//! Validate replacement ownership when Windows delivers each queued event.
//!
//! SendInput only reports insertion into the input queue. Navigation already
//! ahead of that batch can invalidate the command after submission succeeds.
//! The hook and conversion run on the same installed thread, so its bounded
//! registry needs no lock, provider call or AppState borrow in the hook.
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    time::{Duration, Instant},
};

use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
        VK_PACKET,
    },
    WindowsAndMessaging::{
        KBDLLHOOKSTRUCT, LLKHF_INJECTED, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    },
};

use crate::{
    input::ring_buffer,
    platform::win::{keyboard, secure_input::DeliveryContext},
};

const MARKER_PREFIX: usize = 0xD500_0000;
const MAX_TICKET: usize = 0x00FF_FFFF;
const MAX_PENDING_BATCHES: usize = 64;
const BATCH_LIFETIME: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Registry {
    last_ticket: usize,
    batches: BTreeMap<usize, Batch>,
    rejected_context: Option<u64>,
}

struct Batch {
    context: DeliveryContext,
    created: Instant,
    expected: usize,
    observed: usize,
    cancelled: Rc<Cell<bool>>,
    held_keys: [u64; 4],
}

impl Batch {
    fn set_held(&mut self, vk: u32, down: bool) -> bool {
        if vk >= 256 || vk == u32::from(VK_PACKET.0) {
            return false;
        }
        let slot = (vk / 64) as usize;
        let bit = 1_u64 << (vk % 64);
        let was_held = self.held_keys[slot] & bit != 0;
        if down {
            self.held_keys[slot] |= bit;
        } else {
            self.held_keys[slot] &= !bit;
        }
        was_held
    }

    fn cleanup_keys(&self) -> Vec<u16> {
        (0..256_u16)
            .filter(|vk| self.held_keys[usize::from(*vk / 64)] & (1_u64 << (*vk % 64)) != 0)
            .collect()
    }
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

fn context_is_current(context: DeliveryContext) -> bool {
    context.is_current()
}

fn replacement_marker(marker: usize) -> bool {
    marker > MARKER_PREFIX && marker <= (MARKER_PREFIX | MAX_TICKET)
}

fn remember_rejection(registry: &mut Registry, generation: u64) {
    registry.rejected_context = Some(
        registry
            .rejected_context
            .map_or(generation, |old| old.max(generation)),
    );
}

/// Outside the hook, discard only the cancelled batch's original journal.
/// A navigation that already advanced the epoch protects newer input here.
pub(crate) fn flush_rejected_context() {
    let rejected = REGISTRY.with_borrow_mut(|registry| registry.rejected_context.take());
    if let Some(generation) = rejected {
        ring_buffer::reject_secure_context(generation);
    }
}

fn register(context: DeliveryContext, expected: usize) -> Option<(usize, Rc<Cell<bool>>)> {
    if expected == 0 {
        return None;
    }
    REGISTRY.with_borrow_mut(|registry| {
        // Unknown or expired markers remain rejected forever. Do not evict a
        // batch that still owes a key-up for an admitted synthetic key-down.
        let expired: Vec<_> = registry
            .batches
            .iter()
            .filter(|(_, batch)| {
                batch.created.elapsed() >= BATCH_LIFETIME && batch.held_keys == [0; 4]
            })
            .map(|(&marker, _)| marker)
            .collect();
        for marker in expired {
            if let Some(batch) = registry.batches.remove(&marker) {
                batch.cancelled.set(true);
                remember_rejection(registry, batch.context.generation());
            }
        }
        if registry.batches.len() >= MAX_PENDING_BATCHES || registry.last_ticket >= MAX_TICKET {
            return None;
        }
        registry.last_ticket += 1;
        let marker = MARKER_PREFIX | registry.last_ticket;
        let cancelled = Rc::new(Cell::new(false));
        registry.batches.insert(
            marker,
            Batch {
                context,
                created: Instant::now(),
                expected,
                observed: 0,
                cancelled: Rc::clone(&cancelled),
                held_keys: [0; 4],
            },
        );
        Some((marker, cancelled))
    })
}

fn cleanup(keys: &[u16]) {
    if keys.is_empty() {
        return;
    }
    let events: Vec<_> = keys
        .iter()
        .map(|&vk| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    dwFlags: KEYEVENTF_KEYUP,
                    dwExtraInfo: keyboard::OWN_INPUT_MARKER,
                    ..Default::default()
                },
            },
        })
        .collect();
    if let Ok(size) = i32::try_from(std::mem::size_of::<INPUT>()) {
        let _ = unsafe { SendInput(&events, size) };
    }
}

fn finish_submission(marker: usize, sent: usize) -> Vec<u16> {
    REGISTRY.with_borrow_mut(|registry| {
        let Some(batch) = registry.batches.get_mut(&marker) else {
            return Vec::new();
        };
        let partial = sent < batch.expected;
        let rejected =
            (partial && (sent > 0 || batch.observed > 0)).then_some(batch.context.generation());
        if partial {
            batch.cancelled.set(true);
        }
        batch.expected = sent;
        let finished = batch.observed >= sent;
        if let Some(generation) = rejected {
            remember_rejection(registry, generation);
        }
        if finished {
            registry
                .batches
                .remove(&marker)
                .map_or_else(Vec::new, |batch| batch.cleanup_keys())
        } else {
            Vec::new()
        }
    })
}

/// Returns the accepted count and whether submission is still valid. Delivery
/// can complete later; a later cancellation is applied before the next command.
pub(super) fn submit(inputs: &mut [INPUT], context: DeliveryContext) -> (usize, bool) {
    flush_rejected_context();
    if !keyboard::is_hook_thread() || !context_is_current(context) {
        return (0, false);
    }
    let Ok(size) = i32::try_from(std::mem::size_of::<INPUT>()) else {
        return (0, false);
    };
    let Some((marker, cancelled)) = register(context, inputs.len()) else {
        return (0, false);
    };
    for input in inputs.iter_mut() {
        // Replacement builders only produce INPUT_KEYBOARD members.
        input.Anonymous.ki.dwExtraInfo = marker;
    }
    let sent = unsafe { SendInput(inputs, size) } as usize;
    let releases = finish_submission(marker, sent);
    cleanup(&releases);
    flush_rejected_context();
    (
        sent,
        sent == inputs.len() && !cancelled.get() && context_is_current(context),
    )
}

fn filter_event(
    kb: &KBDLLHOOKSTRUCT,
    msg: u32,
    current: impl FnOnce(DeliveryContext) -> bool,
) -> Option<(bool, Vec<u16>)> {
    if !kb.flags.contains(LLKHF_INJECTED) || !replacement_marker(kb.dwExtraInfo) {
        return None;
    }
    let context = REGISTRY.with_borrow(|registry| {
        registry
            .batches
            .get(&kb.dwExtraInfo)
            .map(|batch| batch.context)
    });
    // No registry borrow crosses native focus inspection or an external call.
    let current = context.is_some_and(current);
    Some(REGISTRY.with_borrow_mut(|registry| {
        let Some(batch) = registry.batches.get_mut(&kb.dwExtraInfo) else {
            return (false, Vec::new());
        };
        if !current
            || registry.rejected_context == Some(batch.context.generation())
            || batch.created.elapsed() >= BATCH_LIFETIME
        {
            batch.cancelled.set(true);
        }
        let down = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(msg, WM_KEYUP | WM_SYSKEYUP);
        let allow = if batch.cancelled.get() {
            // Cancelling reselection after Shift-down must still release it.
            // Never release a key whose synthetic down was rejected.
            up && batch.set_held(kb.vkCode, false)
        } else if down {
            batch.set_held(kb.vkCode, true);
            true
        } else if up {
            batch.set_held(kb.vkCode, false);
            true
        } else {
            batch.cancelled.set(true);
            false
        };
        batch.observed += 1;
        let finished = batch.observed >= batch.expected;
        let rejected = batch.cancelled.get().then_some(batch.context.generation());
        if let Some(generation) = rejected {
            remember_rejection(registry, generation);
        }
        let releases = if finished {
            registry
                .batches
                .remove(&kb.dwExtraInfo)
                .map_or_else(Vec::new, |batch| batch.cleanup_keys())
        } else {
            Vec::new()
        };
        (allow, releases)
    }))
}

/// None means another producer owns the event. Replacement markers always
/// receive an explicit decision, including expired, retired and unknown IDs.
pub(crate) fn allow_hook_event(kb: &KBDLLHOOKSTRUCT, msg: u32) -> Option<bool> {
    let (allow, releases) = filter_event(kb, msg, context_is_current)?;
    cleanup(&releases);
    Some(allow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        REGISTRY.with_borrow_mut(|registry| *registry = Registry::default());
        ring_buffer::clear_secure_input_data();
    }

    fn event(marker: usize, vk: u32, up: bool) -> (KBDLLHOOKSTRUCT, u32) {
        (
            KBDLLHOOKSTRUCT {
                flags: LLKHF_INJECTED,
                dwExtraInfo: marker,
                vkCode: vk,
                ..Default::default()
            },
            if up { WM_KEYUP } else { WM_KEYDOWN },
        )
    }

    fn deliver(marker: usize, vk: u32, up: bool, target_current: bool) -> bool {
        let (kb, msg) = event(marker, vk, up);
        let (allow, releases) = filter_event(&kb, msg, |context| {
            target_current && context.epochs_are_current()
        })
        .expect("replacement event must receive an explicit decision");
        assert!(
            releases.is_empty(),
            "complete pairs do not need synthetic cleanup"
        );
        allow
    }

    #[test]
    fn queued_navigation_rejects_an_already_accepted_replacement() {
        let _guard = ring_buffer::test_guard();
        reset();
        ring_buffer::push_text("source");
        let (marker, cancelled) = register(DeliveryContext::for_test(), 2).expect("register");
        assert!(finish_submission(marker, 2).is_empty());
        assert!(!cancelled.get());
        // The native target can still be the same HWND when its Tab hook runs.
        ring_buffer::invalidate_capture_context();
        ring_buffer::push_text("next-field");
        let newer = ring_buffer::runs_snapshot();
        assert!(!deliver(marker, 0x2E, false, true));
        assert!(!deliver(marker, 0x2E, true, true));
        assert!(cancelled.get());
        flush_rejected_context();
        assert_eq!(ring_buffer::runs_snapshot(), newer);
        reset();
    }

    #[test]
    fn cancelled_batch_never_revives_when_focus_returns() {
        let _guard = ring_buffer::test_guard();
        reset();
        let (marker, cancelled) = register(DeliveryContext::for_test(), 4).expect("register");
        assert!(!deliver(marker, 0x2E, false, false));
        assert!(!deliver(marker, 0x2E, true, true));
        assert!(!deliver(marker, u32::from(VK_PACKET.0), false, true));
        assert!(!deliver(marker, u32::from(VK_PACKET.0), true, true));
        assert!(cancelled.get());
        reset();
    }

    #[test]
    fn cancellation_releases_only_modifiers_whose_down_was_delivered() {
        let _guard = ring_buffer::test_guard();
        reset();
        let (marker, _) = register(DeliveryContext::for_test(), 6).expect("register");
        assert!(deliver(marker, 0xA0, false, true));
        assert!(!deliver(marker, 0x27, false, false));
        assert!(!deliver(marker, 0x27, true, true));
        assert!(deliver(marker, 0xA0, true, true));
        assert!(!deliver(marker, 0xA0, false, true));
        assert!(!deliver(marker, 0xA0, true, true));
        reset();
    }

    #[test]
    fn valid_delivery_survives_submission_return_and_retires_its_token() {
        let _guard = ring_buffer::test_guard();
        reset();
        let (marker, cancelled) = register(DeliveryContext::for_test(), 2).expect("register");
        assert!(finish_submission(marker, 2).is_empty());
        assert!(deliver(marker, 0x2E, false, true));
        assert!(deliver(marker, 0x2E, true, true));
        assert!(!cancelled.get());
        assert!(!deliver(marker, 0x2E, false, true));
        let (next, _) = register(DeliveryContext::for_test(), 2).expect("register later");
        assert_ne!(marker, next);
        assert!(!deliver(marker, 0x2E, false, true));
        assert!(finish_submission(next, 0).is_empty());
        reset();
    }

    #[test]
    fn partial_submission_cancels_queued_prefix_and_cleans_admitted_down() {
        let _guard = ring_buffer::test_guard();
        reset();
        let (marker, cancelled) = register(DeliveryContext::for_test(), 4).expect("register");
        assert!(deliver(marker, 0xA0, false, true));
        assert_eq!(finish_submission(marker, 1), vec![0xA0]);
        assert!(cancelled.get());
        flush_rejected_context();
        let (marker, cancelled) = register(DeliveryContext::for_test(), 4).expect("register");
        assert!(finish_submission(marker, 1).is_empty());
        assert!(!deliver(marker, 0xA0, false, true));
        assert!(cancelled.get());
        reset();
    }

    #[test]
    fn zero_submission_keeps_source_journal_restorable() {
        let _guard = ring_buffer::test_guard();
        reset();
        ring_buffer::push_text("unchanged-source");
        let context = DeliveryContext::for_test();
        let original = ring_buffer::runs_snapshot();
        let (marker, cancelled) = register(context, 2).expect("register");
        assert!(finish_submission(marker, 0).is_empty());
        assert!(cancelled.get());
        flush_rejected_context();
        assert!(context.epochs_are_current());
        assert_eq!(ring_buffer::runs_snapshot(), original);
        assert!(!deliver(marker, 0x2E, false, true));
        reset();
    }

    #[test]
    fn ticket_exhaustion_and_full_registry_never_reuse_live_markers() {
        let _guard = ring_buffer::test_guard();
        reset();
        for _ in 0..MAX_PENDING_BATCHES {
            assert!(register(DeliveryContext::for_test(), 2).is_some());
        }
        assert!(register(DeliveryContext::for_test(), 2).is_none());
        REGISTRY.with_borrow_mut(|registry| {
            registry.batches.clear();
            registry.last_ticket = MAX_TICKET;
        });
        assert!(register(DeliveryContext::for_test(), 2).is_none());
        reset();
    }
}
