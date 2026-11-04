//! A translation worker's never-activated desktop and GUI input queue.
//!
//! A thread alone did not isolate `ToUnicodeEx` dead-key state on native Windows.
//! The worker must enter this desktop before any stateful translation. Native
//! caller/worker composition tests enforce that the input desktop stays intact.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use windows::{
    Win32::{
        System::{
            StationsAndDesktops::{
                CloseDesktop, CreateDesktopW, DESKTOP_CREATEWINDOW, DESKTOP_READOBJECTS,
                DESKTOP_WRITEOBJECTS, GetThreadDesktop, HDESK, SetThreadDesktop,
            },
            Threading::GetCurrentThreadId,
        },
        UI::WindowsAndMessaging::{MSG, PM_NOREMOVE, PeekMessageW},
    },
    core::PCWSTR,
};

pub(crate) struct KeyboardDesktop {
    original: HDESK,
    isolated: HDESK,
}

impl KeyboardDesktop {
    pub(crate) fn enter() -> windows::core::Result<Self> {
        static NEXT_DESKTOP: AtomicU64 = AtomicU64::new(0);
        let thread = unsafe { GetCurrentThreadId() };
        let original = unsafe { GetThreadDesktop(thread) }?;
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let sequence = NEXT_DESKTOP.fetch_add(1, Ordering::Relaxed);
        let name: Vec<u16> = format!(
            "RustSwitcherKeyboard-{:x}-{thread:x}-{time:x}-{sequence:x}",
            std::process::id()
        )
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let isolated = unsafe {
            CreateDesktopW(
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                None,
                Default::default(),
                DESKTOP_CREATEWINDOW.0 | DESKTOP_READOBJECTS.0 | DESKTOP_WRITEOBJECTS.0,
                None,
            )
        }?;
        if let Err(error) = unsafe { SetThreadDesktop(isolated) } {
            unsafe {
                let _ = SetThreadDesktop(original);
                let _ = CloseDesktop(isolated);
            }
            return Err(error);
        }

        // Establish the queue on the private desktop explicitly. The worker
        // never creates a visible window, installs a hook, or calls SwitchDesktop.
        let mut message = MSG::default();
        unsafe {
            let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
        }
        Ok(Self { original, isolated })
    }
}

impl Drop for KeyboardDesktop {
    fn drop(&mut self) {
        // GetThreadDesktop returned a borrowed handle. Only our newly created
        // desktop is closed, after this worker has stopped using it.
        if unsafe { SetThreadDesktop(self.original) }.is_ok() {
            let _ = unsafe { CloseDesktop(self.isolated) };
        }
    }
}
