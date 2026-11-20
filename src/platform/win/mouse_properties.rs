//! Generated event traces exercise the production pointer handler. Inserting
//! observation-only events must not alter capture ownership or retained input.

use proptest::prelude::*;

use super::*;
use crate::input::ring_buffer;

type TraceEvent = (u8, u8, Vec<u8>);
type Snapshot = (Vec<ring_buffer::InputRun>, u64, u64);

fn run_trace(events: &[TraceEvent], insert_observations: bool) -> Vec<Snapshot> {
    ring_buffer::clear_secure_input_data();
    let initial_raw = ring_buffer::secure_input_generation();
    let initial_context = ring_buffer::secure_context_generation();
    let mut snapshots = Vec::new();
    for (action, glyph, observations) in events {
        match action {
            0 => update_input_context(WM_LBUTTONDOWN, POINT::default()),
            1 => update_input_context(WM_LBUTTONDBLCLK, POINT::default()),
            2 => update_input_context(WM_RBUTTONDOWN, POINT::default()),
            3 => update_input_context(WM_RBUTTONDBLCLK, POINT::default()),
            4 => update_input_context(WM_MBUTTONDOWN, POINT::default()),
            5 => update_input_context(WM_MBUTTONDBLCLK, POINT::default()),
            6 => update_input_context(WM_MOUSEWHEEL, POINT::default()),
            7 => update_input_context(WM_MOUSEHWHEEL, POINT::default()),
            8 => ring_buffer::secure_capture_unavailable(),
            _ => ring_buffer::push_text(&char::from(b'a' + glyph % 26).to_string()),
        }
        if insert_observations {
            for observation in observations {
                update_input_context(
                    match observation {
                        0 => WM_LBUTTONUP,
                        1 => WM_RBUTTONUP,
                        2 => WM_MBUTTONUP,
                        _ => windows::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE,
                    },
                    POINT::default(),
                );
            }
        }
        snapshots.push((
            ring_buffer::runs_snapshot(),
            ring_buffer::secure_input_generation().wrapping_sub(initial_raw),
            ring_buffer::secure_context_generation().wrapping_sub(initial_context),
        ));
    }
    ring_buffer::clear_secure_input_data();
    snapshots
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn adding_releases_and_moves_preserves_every_prefix_and_guard(
        events in prop::collection::vec(
            (0_u8..12, any::<u8>(), prop::collection::vec(0_u8..4, 0..5)),
            1..65,
        ),
    ) {
        let _guard = ring_buffer::test_guard();
        // Compare whole histories, including input following a rejected key.
        // A release that resets the "incomplete" state would permit a suffix
        // in the second trace and produce a smaller failing counterexample.
        let baseline = run_trace(&events, false);
        let with_observations = run_trace(&events, true);
        prop_assert_eq!(with_observations, baseline);
    }
}
