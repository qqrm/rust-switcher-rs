//! Starts the detached, verified Windows update helper from the tray.
//!
//! A separate PowerShell process downloads the release and waits for this
//! process to exit before replacing its executable. No shell interpolation is
//! used for paths or arguments.

use std::{
    fs, io,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use windows::Win32::Foundation::HWND;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);

pub(super) fn start(hwnd: HWND) {
    if cfg!(any(debug_assertions, feature = "debug-binary")) {
        let _ = super::tray::balloon_info(
            hwnd,
            "Rust Switcher",
            "Updates are only available from the release application",
        );
        return;
    }

    if UPDATE_RUNNING.swap(true, Ordering::AcqRel) {
        let _ = super::tray::balloon_info(hwnd, "Rust Switcher", "Update check is in progress");
        return;
    }

    match spawn_updater() {
        Ok(mut child) => {
            let _ =
                super::tray::balloon_info(hwnd, "Rust Switcher", "Checking for a new release");
            if thread::Builder::new()
                .name("release-updater-monitor".to_owned())
                .spawn(move || {
                    let _ = child.wait();
                    UPDATE_RUNNING.store(false, Ordering::Release);
                })
                .is_err()
            {
                UPDATE_RUNNING.store(false, Ordering::Release);
            }
        }
        Err(error) => {
            UPDATE_RUNNING.store(false, Ordering::Release);
            let _ = super::tray::balloon_error(
                hwnd,
                "Rust Switcher update failed",
                &error.to_string(),
            );
        }
    }
}

fn spawn_updater() -> io::Result<Child> {
    let target = std::env::current_exe()?;
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is unavailable"))?;
    let log_path = local_app_data.join("RustSwitcher").join("updater.log");

    let temporary = tempfile::tempdir()?;
    let script = temporary.path().join("windows-updater.ps1");
    fs::write(&script, include_str!("../../../scripts/windows-updater.ps1"))?;

    let child = Command::new("powershell.exe")
        .arg("-NoLogo")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&script)
        .arg("-TargetPath")
        .arg(&target)
        .arg("-CurrentVersion")
        .arg(env!("CARGO_PKG_VERSION"))
        .arg("-ParentPid")
        .arg(std::process::id().to_string())
        .arg("-LogPath")
        .arg(log_path)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let _ = temporary.keep();
    Ok(child)
}
