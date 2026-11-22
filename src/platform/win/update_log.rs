//! Opens the exact updater log through the user's Windows file association.

use std::{
    fs, io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use windows::{
    Win32::{
        Foundation::{ERROR_NO_ASSOCIATION, HWND},
        System::{
            Com::{
                COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
            },
            SystemInformation::GetSystemDirectoryW,
        },
        UI::{
            Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWNORMAL},
        },
    },
    core::{HRESULT, PCWSTR, w},
};

static OPENING: AtomicBool = AtomicBool::new(false);

pub(super) fn open(hwnd: HWND) {
    if OPENING.swap(true, Ordering::AcqRel) {
        return;
    }
    let hwnd_value = hwnd.0 as isize;
    if let Err(error) = thread::Builder::new()
        .name("update-log-opener".to_owned())
        .spawn(move || {
            let hwnd = HWND(hwnd_value as *mut std::ffi::c_void);
            let result = super::updater::log_path().and_then(|path| {
                open_path(hwnd, &path).map_err(|error| {
                    io::Error::other(format!("Could not open {}\n\n{error}", path.display()))
                })
            });
            if let Err(error) = result {
                let message: Vec<u16> = error.to_string().encode_utf16().chain(Some(0)).collect();
                let _ = unsafe {
                    MessageBoxW(
                        Some(hwnd),
                        PCWSTR(message.as_ptr()),
                        w!("Rust Switcher: cannot open update log"),
                        MB_OK | MB_ICONERROR,
                    )
                };
            }
            OPENING.store(false, Ordering::Release);
        })
    {
        OPENING.store(false, Ordering::Release);
        crate::platform::ui::notify::push_error(hwnd, "Cannot open update log", &error.to_string());
    }
}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn open_path(hwnd: HWND, path: &Path) -> io::Result<()> {
    // Opening diagnostics must remain read-only. A missing log is an error,
    // and a readable log must not require write access or change permissions.
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("The update log path is not a file"));
    }
    drop(file);
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
        .ok()
        .map_err(io::Error::other)?;
    let _apartment = ComApartment;
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        hwnd,
        lpFile: PCWSTR(file.as_ptr()),
        // Null verb selects the configured default; a document has no args.
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    match unsafe { ShellExecuteExW(&raw mut execute) } {
        Ok(()) => Ok(()),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_ASSOCIATION.0) => {
            // Use the real system directory and structured argv. Neither the
            // reader nor a Unicode/space-containing path goes through a shell.
            Command::new(system_notepad()?)
                .arg(path)
                .spawn()
                .map(|_| ())
        }
        Err(error) => Err(io::Error::other(error)),
    }
}

fn system_notepad() -> io::Result<PathBuf> {
    let mut directory = vec![0_u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(Some(&mut directory)) } as usize;
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= directory.len() {
        return Err(io::Error::other(
            "Windows system directory exceeds its buffer",
        ));
    }
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&directory[..length])).join("notepad.exe"))
}
