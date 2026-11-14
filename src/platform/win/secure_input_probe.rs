//! Read-only counters for the disposable Windows acceptance fixture.
//! This module is absent from release builds and never exposes captured text.

use windows::Win32::Foundation::{HWND, LRESULT};

pub(super) const MESSAGE: u32 = 0x83F0;
const MAGIC: isize = 0x5253_5031;

pub(super) fn read(hwnd: HWND, selector: usize) -> LRESULT {
    if !super::keyboard::allows_e2e_metadata() {
        return LRESULT(-1);
    }
    if selector == 0 {
        return LRESULT(MAGIC);
    }
    if selector == 4 {
        return super::state::get_state(hwnd)
            .map(|state| LRESULT(isize::from(state.autoconvert_enabled)))
            .unwrap_or(LRESULT(-1));
    }
    // Observing a counter must not drain a worker, classify a field or clear a
    // journal: otherwise the test could hide transient password capture.
    let (active, cached, captured) = crate::input::ring_buffer::secure_input_test_metadata();
    let value = match selector {
        1 => isize::try_from(active),
        2 => isize::try_from(cached),
        3 => isize::try_from(captured),
        _ => return LRESULT(-1),
    };
    LRESULT(value.unwrap_or(isize::MAX))
}
