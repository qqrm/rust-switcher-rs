use std::sync::{OnceLock, mpsc};

use windows::Win32::Foundation::HWND;

use super::{Notification, NotifyKind};

#[derive(Clone)]
struct Job {
    hwnd_value: isize,
    n: Notification,
}

static TX: OnceLock<mpsc::Sender<Job>> = OnceLock::new();

fn sender() -> Option<&'static mpsc::Sender<Job>> {
    if TX.get().is_some() {
        return TX.get();
    }

    let (tx, rx) = mpsc::channel::<Job>();
    if TX.set(tx).is_err() {
        return TX.get();
    }

    let _ = std::thread::Builder::new()
        .name("rust-switcher-notify-worker".to_owned())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                let hwnd = hwnd_from_value(job.hwnd_value);
                present_one(hwnd, &job.n);
            }
        });

    TX.get()
}

fn hwnd_to_value(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

fn hwnd_from_value(v: isize) -> HWND {
    HWND(v as *mut std::ffi::c_void)
}

fn present_one(hwnd: HWND, n: &Notification) {
    match n.kind {
        NotifyKind::Info => {
            let _ = crate::platform::win::tray::balloon_info(hwnd, &n.title, &n.text);
        }
        NotifyKind::Error => {
            let _ = crate::platform::win::tray::balloon_error(hwnd, &n.title, &n.text);
        }
        NotifyKind::UpdateFailure => {
            let result =
                crate::platform::win::tray::balloon_update_failure(hwnd, &n.title, &n.text);
            crate::platform::win::updater::record_failure_notification(result);
        }
        NotifyKind::Roar => {
            let _ = crate::platform::win::tray::balloon_info(hwnd, &n.title, &n.text);
        }
    }
}

pub(super) fn on_wm_app_notify(hwnd: HWND) {
    let Some(tx) = sender() else {
        return;
    };

    let hwnd_value = hwnd_to_value(hwnd);

    for n in super::drain_for_worker() {
        let _ = tx.send(Job { hwnd_value, n });
    }

    super::repost_if_needed(hwnd);
}
