//! Starts the detached, verified Windows update helper from the tray.
//!
//! A separate PowerShell process downloads the release and waits for this
//! process to exit before replacing its executable. No shell interpolation is
//! used for paths or arguments.

use std::{
    fs,
    io::{self, Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use windows::Win32::Foundation::HWND;

use crate::platform::ui::notify;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);
const MAX_HELPER_STDERR: usize = 16 * 1024;

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

    let _ = super::tray::balloon_info(hwnd, "Rust Switcher", "Checking for a new release");
    let hwnd_value = hwnd.0 as isize;
    if let Err(error) = thread::Builder::new()
        .name("release-updater-monitor".to_owned())
        .spawn(move || {
            let hwnd = HWND(hwnd_value as *mut std::ffi::c_void);
            match log_path() {
                Ok(path) => match run_updater(&path) {
                    Ok(()) => notify::push_info(hwnd, "Rust Switcher", "Already up to date"),
                    Err(error) => report_failure(hwnd, Some(&path), &error.to_string()),
                },
                Err(error) => report_failure(hwnd, None, &error.to_string()),
            }
            UPDATE_RUNNING.store(false, Ordering::Release);
        })
    {
        UPDATE_RUNNING.store(false, Ordering::Release);
        report_failure(
            hwnd,
            log_path().ok().as_deref(),
            &format!("Could not start update worker: {error}"),
        );
    }
}

pub(super) fn log_path() -> io::Result<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is unavailable"))?;
    Ok(local_app_data.join("RustSwitcher").join("updater.log"))
}

fn append_log(path: &Path, message: &str) -> io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(
        file,
        "Rust Switcher {}: {message}",
        env!("CARGO_PKG_VERSION")
    )
}

fn report_failure(hwnd: HWND, path: Option<&Path>, error: &str) {
    let logged = path.map(|path| append_log(path, &format!("Update failed: {error}")));
    match logged {
        Some(Ok(())) => notify::push_update_failure(
            hwnd,
            "Update failed. Click to open the update log, or choose Open update log from the tray menu.",
        ),
        Some(Err(log_error)) => notify::push_error(
            hwnd,
            "Rust Switcher update failed",
            &format!("{error}\nCould not write the update log: {log_error}"),
        ),
        None => notify::push_error(hwnd, "Rust Switcher update failed", error),
    }
}

pub(crate) fn record_failure_notification(result: windows::core::Result<()>) {
    let message = match result {
        Ok(()) => "Update failure notification ready; click to open updater.log".to_owned(),
        Err(error) => format!(
            "Could not show update failure notification: {error}. Use Open update log in the tray menu."
        ),
    };
    if let Ok(path) = log_path() {
        let _ = append_log(&path, &message);
    }
}

fn run_updater(log_path: &Path) -> io::Result<()> {
    append_log(log_path, "Checking for a new release")?;
    let mut child = spawn_updater(log_path).map_err(|error| {
        io::Error::other(format!("Could not launch PowerShell updater: {error}"))
    })?;
    // Drain stderr before waiting so a full pipe cannot deadlock the helper.
    // Retain only its bounded tail, and do not hold the log open while the
    // helper is writing it. This also records parser/startup/policy errors.
    let stderr = child.stderr.take().map(read_stderr_tail).transpose();
    let status = child.wait().map_err(|error| {
        io::Error::other(format!("Could not wait for PowerShell updater: {error}"))
    })?;
    let stderr = stderr?.unwrap_or_default();
    if !status.success() {
        return Err(io::Error::other(format!(
            "PowerShell updater exited with {status}.\n{}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(())
}

fn read_stderr_tail(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut tail = Vec::with_capacity(MAX_HELPER_STDERR);
    let mut buffer = [0_u8; 4096];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => return Ok(tail),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let excess = (tail.len() + count).saturating_sub(MAX_HELPER_STDERR);
        tail.drain(..excess);
        tail.extend_from_slice(&buffer[..count]);
    }
}

pub(super) fn open_log(hwnd: HWND) {
    super::update_log::open(hwnd);
}

fn spawn_updater(log_path: &Path) -> io::Result<Child> {
    let target = std::env::current_exe()?;

    let temporary = tempfile::tempdir()?;
    let script = temporary.path().join("windows-updater.ps1");
    fs::write(
        &script,
        include_str!("../../../scripts/windows-updater.ps1"),
    )?;

    let child = Command::new("powershell.exe")
        // Windows PowerShell must not inherit incompatible PowerShell 7 module paths.
        .env_remove("PSModulePath")
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
        .stderr(Stdio::piped())
        .spawn()?;

    let _ = temporary.keep();
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::{MAX_HELPER_STDERR, read_stderr_tail};

    #[test]
    fn helper_stderr_is_drained_but_only_its_tail_is_retained() {
        let mut bytes = vec![b'x'; MAX_HELPER_STDERR * 3];
        bytes.extend_from_slice(b"PowerShell parser failure");
        let tail = read_stderr_tail(bytes.as_slice()).unwrap_or_default();
        assert_eq!(tail.len(), MAX_HELPER_STDERR);
        assert_eq!(tail, bytes[bytes.len() - MAX_HELPER_STDERR..]);
    }
}
