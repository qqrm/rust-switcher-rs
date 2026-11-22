//! Serialize launches and replace the existing application process on demand.

use std::{
    os::windows::ffi::OsStringExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WAIT_ABANDONED,
            WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
        },
        System::{
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{
                CreateMutexW, GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
                QueryFullProcessImageNameW, ReleaseMutex, TerminateProcess, WaitForSingleObject,
            },
        },
        UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW},
    },
    core::{Error, HRESULT, PCWSTR, PWSTR, Result},
};

use crate::{app_identity, platform::win::WM_APP_EXIT_FOR_UPDATE};

const STARTUP_WAIT_MS: u32 = 15_000;
const GRACEFUL_EXIT_MS: u32 = 2_000;
const FORCED_EXIT_MS: u32 = 3_000;

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the handle and closes it exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct StartupLock(Handle);

impl StartupLock {
    fn acquire() -> Result<Self> {
        let name = format!(
            "{}.Restart",
            app_identity::SINGLE_INSTANCE_MUTEX.replacen("Global\\", "Local\\", 1)
        );
        let name = wide(&name);
        // SAFETY: the name is null-terminated and no custom security record is supplied.
        let handle = Handle(unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr()))? });
        // An abandoned launch lock is safe: the previous launcher is already dead.
        match unsafe { WaitForSingleObject(handle.0, STARTUP_WAIT_MS) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
            WAIT_TIMEOUT => Err(failure(
                "Another Rust Switcher launch did not finish in time",
            )),
            _ => Err(Error::from_thread()),
        }
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        // SAFETY: the startup thread successfully acquired this mutex.
        let _ = unsafe { ReleaseMutex(self.0.0) };
    }
}

/// Keep the legacy singleton alive and serialize startup until the new window is ready.
pub struct SingleInstanceGuard {
    _instance: Handle,
    startup: Option<StartupLock>,
}

impl SingleInstanceGuard {
    /// A normal launch replaces its predecessor; updater recovery only starts if absent.
    pub fn acquire(replace_existing: bool) -> Result<Option<Self>> {
        let startup = StartupLock::acquire()?;
        let name = wide(app_identity::SINGLE_INSTANCE_MUTEX);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let instance = Handle(unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr()))? });
            let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if !already_running {
                return Ok(Some(Self {
                    _instance: instance,
                    startup: Some(startup),
                }));
            }
            if !replace_existing {
                return Ok(None);
            }
            // Do not keep the legacy mutex alive ourselves while waiting for its owner.
            drop(instance);
            if Instant::now() >= deadline {
                return Err(failure(
                    "The running Rust Switcher instance could not be located in this session",
                ));
            }
            if !replace_running_instance()? {
                // Old versions have no launch gate. Allow their window creation or exit
                // to finish, then recheck the singleton rather than trusting a stale HWND.
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// Release the launch gate only after the new window can receive shutdown requests.
    pub fn ready(&mut self) {
        self.startup.take();
    }
}

fn replace_running_instance() -> Result<bool> {
    let class_name = wide(app_identity::MAIN_WINDOW_CLASS);
    // FindWindow only searches this desktop. Debug and release use different classes.
    let window =
        unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR::null()) }.unwrap_or_default();
    if window.0.is_null() {
        return Ok(false);
    }
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) } == 0 || pid == 0 {
        return Ok(false);
    }
    if pid == unsafe { GetCurrentProcessId() } {
        return Err(failure(
            "Could not identify the previous Rust Switcher process",
        ));
    }
    // Keep the process handle through shutdown so a recycled PID is never terminated.
    let process = match unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
            false,
            pid,
        )
    } {
        Ok(process) => Handle(process),
        Err(error) => {
            let mut current_pid = 0;
            unsafe { GetWindowThreadProcessId(window, Some(&mut current_pid)) };
            if current_pid != pid {
                return Ok(false);
            }
            return Err(error);
        }
    };
    if let Err(error) = verify_application_process(process.0, pid) {
        if unsafe { WaitForSingleObject(process.0, 0) } == WAIT_OBJECT_0 {
            return Ok(true);
        }
        return Err(error);
    }
    let mut current_pid = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut current_pid)) };
    if current_pid != pid {
        return Ok(false);
    }
    tracing::info!(pid, "replacing existing Rust Switcher instance");
    // WM_CLOSE only hides this app. Use its existing cleanup path instead.
    let _ = unsafe { PostMessageW(Some(window), WM_APP_EXIT_FOR_UPDATE, WPARAM(0), LPARAM(0)) };
    match unsafe { WaitForSingleObject(process.0, GRACEFUL_EXIT_MS) } {
        WAIT_OBJECT_0 => return Ok(true),
        WAIT_TIMEOUT => {}
        _ => return Err(Error::from_thread()),
    }
    tracing::warn!(pid, "previous Rust Switcher did not exit; terminating it");
    if let Err(error) = unsafe { TerminateProcess(process.0, 0) }
        && unsafe { WaitForSingleObject(process.0, 0) } != WAIT_OBJECT_0
    {
        return Err(error);
    }
    match unsafe { WaitForSingleObject(process.0, FORCED_EXIT_MS) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Err(failure(
            "The previous Rust Switcher process could not be stopped",
        )),
        _ => Err(Error::from_thread()),
    }
}

fn verify_application_process(process: HANDLE, pid: u32) -> Result<()> {
    let mut own_session = 0;
    let mut peer_session = 0;
    unsafe {
        ProcessIdToSessionId(GetCurrentProcessId(), &mut own_session)?;
        ProcessIdToSessionId(pid, &mut peer_session)?;
    }
    if own_session != peer_session {
        return Err(failure(
            "The existing Rust Switcher belongs to another Windows session",
        ));
    }
    let mut image = vec![0_u16; 32_768];
    let mut length = image.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(image.as_mut_ptr()),
            &mut length,
        )?;
    }
    let image = std::ffi::OsString::from_wide(&image[..length as usize]);
    let current = std::env::current_exe().map_err(|error| failure(&error.to_string()))?;
    let expected = current.file_name().and_then(|name| name.to_str());
    let actual = Path::new(&image).file_name().and_then(|name| name.to_str());
    if !matches!((expected, actual), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b)) {
        return Err(failure(
            "The existing window belongs to a different executable",
        ));
    }
    Ok(())
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn failure(message: &str) -> Error {
    Error::new(HRESULT(0x8000_4005_u32.cast_signed()), message)
}
