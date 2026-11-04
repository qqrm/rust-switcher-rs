//! Read-only access to the keyboard tables registered and installed by Windows.
//!
//! The physical-layout word of an HKL is significant. For the special Fxxx
//! device IDs, resolve the unique HKLM `Layout Id`, as Microsoft WinForms does:
//! https://source.dot.net/System.Windows.Forms/System/Windows/Forms/Input/InputLanguage.cs.html
//! That special-ID encoding is not a documented Win32 contract, so unknown or
//! ambiguous registrations fail closed. No layout is loaded or activated here.
//!
//! The WDK keyboard-layout sample documents the DLL table interface:
//! https://learn.microsoft.com/samples/microsoft/windows-driver-samples/keyboard-layout-samples/

use std::{ffi::c_void, mem::size_of};

use windows::{
    Win32::{
        Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FreeLibrary, HMODULE,
        },
        System::{
            Diagnostics::Debug::ReadProcessMemory,
            LibraryLoader::{
                GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_REQUIRE_SIGNED_TARGET,
                LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
            },
            Memory::{
                MEM_COMMIT, MEM_IMAGE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ,
                PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS,
                PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY, VirtualQuery,
            },
            Registry::{
                HKEY, HKEY_LOCAL_MACHINE, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, REG_SZ,
                RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
            },
            SystemInformation::GetSystemDirectoryW,
            Threading::GetCurrentProcess,
        },
        UI::Input::{Ime::ImmIsIME, KeyboardAndMouse::HKL},
    },
    core::{PCWSTR, PWSTR, s, w},
};

const LAYOUTS_KEY: PCWSTR = w!("SYSTEM\\CurrentControlSet\\Control\\Keyboard Layouts");
const MAX_LAYOUTS: usize = 4096;
const MAX_STRING_BYTES: usize = 1024;
const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

struct Module(HMODULE);

impl Drop for Module {
    fn drop(&mut self) {
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

/// Keeps a trusted keyboard DLL mapped until the caller has copied its tables.
/// This object stays on the parsing thread; only the resulting owned tables
/// should be sent to other workers.
pub(crate) struct KeyboardLibrary {
    module: Module,
    descriptor: *const c_void,
}

impl KeyboardLibrary {
    /// Resolve a complete Windows HKL to one registered System32 DLL.
    /// Custom unsigned DLLs, IMEs and unsupported HKL encodings are unavailable.
    /// The provider validates its live enabled-layout catalog before requesting
    /// conversion; the decoder supplies the actual foreground HKL. A worker's
    /// own catalog need not include handles loaded by a different thread.
    pub(crate) fn load(hkl: usize) -> Option<Self> {
        let canonical = canonical_hkl(hkl)?;
        if unsafe { ImmIsIME(HKL(hkl as *mut _)) }.as_bool() {
            return None;
        }
        let registry = open_key(
            HKEY_LOCAL_MACHINE,
            LAYOUTS_KEY,
            KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS,
        )?;
        let klid = resolve_klid(canonical, &registry)?;
        let klid = wide(&klid);
        let layout = open_key(registry.0, PCWSTR(klid.as_ptr()), KEY_QUERY_VALUE)?;
        if registry_string(&layout, w!("Ime File"))?.is_some() {
            return None;
        }
        let filename = registry_string(&layout, w!("Layout File"))??;
        if !valid_dll_basename(&filename) {
            return None;
        }
        let path = format!("{}\\{}", system_directory()?, filename);
        let path_wide = wide(&path);
        // Use an absolute system path for the DLL and System32 only for its
        // dependencies. Do not search the application, working directory or PATH.
        let module = Module(unsafe {
            LoadLibraryExW(
                PCWSTR(path_wide.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_REQUIRE_SIGNED_TARGET,
            )
            .ok()?
        });
        if !module_path(module.0)?.eq_ignore_ascii_case(&path) {
            return None;
        }
        let mut library = Self {
            module,
            descriptor: std::ptr::null(),
        };
        let entry = unsafe { GetProcAddress(library.module.0, s!("KbdLayerDescriptor")) }?;
        // Reject forwarded exports and data addresses before calling the WDK ABI.
        library.validate_range(entry as usize, 1, true)?;
        let descriptor: unsafe extern "system" fn() -> *const c_void =
            unsafe { std::mem::transmute(entry) };
        library.descriptor = unsafe { descriptor() };
        library.validate_range(library.descriptor as usize, 1, false)?;
        Some(library)
    }

    pub(crate) fn descriptor(&self) -> *const c_void {
        self.descriptor
    }

    /// Snapshot bounded readable bytes from this module image only. A module
    /// reference remains owned throughout validation and copying; OS-assisted
    /// reads also fail safely if page accessibility changes after VirtualQuery.
    pub(crate) fn read_bytes(&self, address: usize, length: usize) -> Option<Vec<u8>> {
        self.validate_range(address, length, false)?;
        let mut bytes = vec![0; length];
        let mut copied = 0;
        unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                address as *const c_void,
                bytes.as_mut_ptr().cast(),
                length,
                Some(&mut copied),
            )
            .ok()?;
        }
        (copied == length).then_some(bytes)
    }

    fn validate_range(&self, address: usize, length: usize, executable: bool) -> Option<()> {
        if address == 0 || length == 0 || length > MAX_SNAPSHOT_BYTES {
            return None;
        }
        let end = address.checked_add(length)?;
        let mut cursor = address;
        while cursor < end {
            let mut region = MEMORY_BASIC_INFORMATION::default();
            let size = unsafe {
                VirtualQuery(
                    Some(cursor as *const c_void),
                    &mut region,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if size != size_of::<MEMORY_BASIC_INFORMATION>() {
                return None;
            }
            cursor =
                checked_region_end(&region, self.module.0.0 as usize, cursor, executable)?.min(end);
        }
        Some(())
    }
}

fn checked_region_end(
    region: &MEMORY_BASIC_INFORMATION,
    module_base: usize,
    address: usize,
    executable: bool,
) -> Option<usize> {
    if region.AllocationBase as usize != module_base
        || region.State != MEM_COMMIT
        || region.Type != MEM_IMAGE
        || region.Protect.contains(PAGE_GUARD)
        || region.Protect.contains(PAGE_NOACCESS)
    {
        return None;
    }
    let protection = region.Protect.0 & 0xff;
    let readable = matches!(protection, p if p == PAGE_READONLY.0
        || p == PAGE_READWRITE.0 || p == PAGE_WRITECOPY.0
        || p == PAGE_EXECUTE_READ.0 || p == PAGE_EXECUTE_READWRITE.0
        || p == PAGE_EXECUTE_WRITECOPY.0);
    let can_execute = matches!(protection, p if p == PAGE_EXECUTE_READ.0
        || p == PAGE_EXECUTE_READWRITE.0 || p == PAGE_EXECUTE_WRITECOPY.0);
    if !readable || (executable && !can_execute) {
        return None;
    }
    let base = region.BaseAddress as usize;
    let end = base.checked_add(region.RegionSize)?;
    (base <= address && address < end).then_some(end)
}

fn canonical_hkl(hkl: usize) -> Option<u32> {
    let value = (hkl & 0xffff_ffff) as u32;
    let zero_extended = value as usize;
    let sign_extended = (value as i32 as isize) as usize;
    (value != 0 && (hkl == zero_extended || hkl == sign_extended)).then_some(value)
}

fn resolve_klid(hkl: u32, registry: &RegistryKey) -> Option<String> {
    let device = (hkl >> 16) as u16;
    if device & 0xf000 != 0xf000 {
        return ordinary_klid(hkl);
    }
    let id = device & 0x0fff;
    if id == 0 {
        return None;
    }
    let mut matches = Vec::new();
    for index in 0..MAX_LAYOUTS {
        let mut name = [0u16; 256];
        let mut length = u32::try_from(name.len()).ok()?;
        let status = unsafe {
            RegEnumKeyExW(
                registry.0,
                u32::try_from(index).ok()?,
                Some(PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            return unique_special_klid(id, &matches);
        }
        if status != ERROR_SUCCESS {
            return None;
        }
        let key_name = String::from_utf16(name.get(..usize::try_from(length).ok()?)?).ok()?;
        if !valid_klid(&key_name) {
            continue;
        }
        let key_name_wide = wide(&key_name);
        let key = open_key(registry.0, PCWSTR(key_name_wide.as_ptr()), KEY_QUERY_VALUE)?;
        if let Some(layout_id) = registry_string(&key, w!("Layout Id"))? {
            let layout_id = parse_layout_id(&layout_id)?;
            if layout_id == id {
                matches.push((key_name, layout_id));
            }
        }
    }
    None
}

fn ordinary_klid(hkl: u32) -> Option<String> {
    let device = (hkl >> 16) as u16;
    // E-prefixed device IDs identify legacy IMEs, not ordinary KBDTABLES.
    if device & 0xf000 == 0xe000 || device & 0xf000 == 0xf000 {
        return None;
    }
    let physical = if device == 0 { hkl as u16 } else { device };
    (physical != 0).then(|| format!("{physical:08X}"))
}

fn unique_special_klid(id: u16, matches: &[(String, u16)]) -> Option<String> {
    let mut matching = matches
        .iter()
        .filter(|(name, value)| *value == id && valid_klid(name));
    let first = matching.next()?.0.to_uppercase();
    matching.next().is_none().then_some(first)
}

fn valid_klid(name: &str) -> bool {
    name.len() == 8 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_layout_id(value: &str) -> Option<u16> {
    if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(value, 16).ok()
}

fn valid_dll_basename(filename: &str) -> bool {
    let Some(stem) = filename.get(..filename.len().saturating_sub(4)) else {
        return false;
    };
    !stem.is_empty()
        && filename.len() <= 128
        && filename
            .get(filename.len() - 4..)
            .is_some_and(|ext| ext.eq_ignore_ascii_case(".dll"))
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_key(
    parent: HKEY,
    name: PCWSTR,
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Option<RegistryKey> {
    let mut key = HKEY::default();
    let status = unsafe { RegOpenKeyExW(parent, name, Some(0), access, &mut key) };
    if status != ERROR_SUCCESS {
        return None;
    }
    Some(RegistryKey(key))
}

// Outer None is a read/format error; inner None means the value is absent.
fn registry_string(key: &RegistryKey, name: PCWSTR) -> Option<Option<String>> {
    let mut kind = REG_SZ;
    let mut size = 0;
    let status =
        unsafe { RegQueryValueExW(key.0, name, None, Some(&mut kind), None, Some(&mut size)) };
    if status == ERROR_FILE_NOT_FOUND {
        return Some(None);
    }
    let length = usize::try_from(size).ok()?;
    if status != ERROR_SUCCESS
        || kind != REG_SZ
        || !(2..=MAX_STRING_BYTES).contains(&length)
        || length % 2 != 0
    {
        return None;
    }
    let mut bytes = vec![0; length];
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name,
            None,
            Some(&mut kind),
            Some(bytes.as_mut_ptr()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS || kind != REG_SZ || usize::try_from(size).ok()? != length {
        return None;
    }
    let words: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let terminator = words.iter().position(|value| *value == 0)?;
    if words[terminator..].iter().any(|value| *value != 0) {
        return None;
    }
    Some(Some(String::from_utf16(&words[..terminator]).ok()?))
}

fn system_directory() -> Option<String> {
    let mut path = vec![0u16; 32768];
    let length = usize::try_from(unsafe { GetSystemDirectoryW(Some(&mut path)) }).ok()?;
    if length == 0 || length >= path.len() {
        return None;
    }
    let path = String::from_utf16(&path[..length]).ok()?;
    (path.len() >= 3 && path.as_bytes()[1] == b':' && path.as_bytes()[2] == b'\\').then_some(path)
}

fn module_path(module: HMODULE) -> Option<String> {
    let mut path = vec![0u16; 32768];
    let length = usize::try_from(unsafe { GetModuleFileNameW(Some(module), &mut path) }).ok()?;
    if length == 0 || length >= path.len() {
        return None;
    }
    String::from_utf16(&path[..length]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_layout_library_preserves_physical_alias_and_variant_ids() {
        assert_eq!(ordinary_klid(0x0409_0409).as_deref(), Some("00000409"));
        assert_eq!(ordinary_klid(0x040c_0409).as_deref(), Some("0000040C"));
        assert_eq!(ordinary_klid(0x0000_0409).as_deref(), Some("00000409"));
        assert_eq!(ordinary_klid(0xe001_0411), None);
        assert_eq!(ordinary_klid(0xf001_0409), None);
        let entries = vec![("00020409".to_owned(), 1), ("00010409".to_owned(), 2)];
        assert_eq!(
            unique_special_klid(1, &entries).as_deref(),
            Some("00020409")
        );
        assert_eq!(
            unique_special_klid(2, &entries).as_deref(),
            Some("00010409")
        );
        assert_eq!(unique_special_klid(3, &entries), None);
        assert_eq!(
            unique_special_klid(1, &[("00020409".to_owned(), 1), ("00010409".to_owned(), 1)]),
            None
        );
    }

    #[test]
    fn native_layout_library_accepts_sign_extended_handles_without_truncating_unknown_bits() {
        let value = 0xf001_0409_u32;
        assert_eq!(canonical_hkl(value as usize), Some(value));
        assert_eq!(canonical_hkl(value as i32 as isize as usize), Some(value));
        assert_eq!(canonical_hkl(0), None);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(canonical_hkl(0x1234_5678_f001_0409), None);
    }

    #[test]
    fn native_layout_library_rejects_registry_paths_and_invalid_identifiers() {
        for filename in [
            "",
            "a",
            ".dll",
            "../kbdus.dll",
            "C:\\kbdus.dll",
            "kbdus.dll:stream",
            "kbdus.dll ",
            "kbdus\0.dll",
            "a/b.dll",
        ] {
            assert!(!valid_dll_basename(filename), "{filename:?}");
        }
        for filename in ["kbdus.dll", "KBDUSX.DLL", "kbd-test_2.dll"] {
            assert!(valid_dll_basename(filename));
        }
        for value in ["", "1", "00001", "../1"] {
            assert_eq!(parse_layout_id(value), None);
        }
        assert_eq!(parse_layout_id("0001"), Some(1));
        assert_eq!(parse_layout_id("0000"), Some(0));
    }

    #[test]
    fn native_layout_library_rejects_unreadable_foreign_or_nonprogressing_regions() {
        let mut region = MEMORY_BASIC_INFORMATION {
            AllocationBase: 0x1000 as *mut _,
            BaseAddress: 0x2000 as *mut _,
            RegionSize: 0x1000,
            State: MEM_COMMIT,
            Type: MEM_IMAGE,
            Protect: PAGE_READONLY,
            ..Default::default()
        };
        assert_eq!(
            checked_region_end(&region, 0x1000, 0x2001, false),
            Some(0x3000)
        );
        assert_eq!(checked_region_end(&region, 0x1000, 0x2001, true), None);
        assert_eq!(checked_region_end(&region, 0x2000, 0x2001, false), None);
        assert_eq!(checked_region_end(&region, 0x1000, 0x3000, false), None);
        region.Protect |= PAGE_GUARD;
        assert_eq!(checked_region_end(&region, 0x1000, 0x2001, false), None);
        region.Protect = PAGE_NOACCESS;
        assert_eq!(checked_region_end(&region, 0x1000, 0x2001, false), None);
        region.Protect = PAGE_EXECUTE_READ;
        assert_eq!(
            checked_region_end(&region, 0x1000, 0x2001, true),
            Some(0x3000)
        );
        region.RegionSize = usize::MAX;
        assert_eq!(checked_region_end(&region, 0x1000, 0x2001, false), None);
    }

    #[test]
    fn native_layout_library_reads_installed_table_without_changing_active_layout() {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
        let before = unsafe { GetKeyboardLayout(0) };
        let Some(library) = KeyboardLibrary::load(before.0 as usize) else {
            panic!("the Windows test runner's active ordinary layout must expose keyboard tables");
        };
        assert!(
            library
                .read_bytes(library.descriptor() as usize, size_of::<usize>())
                .is_some()
        );
        assert!(library.read_bytes(0, 1).is_none());
        assert!(library.read_bytes(usize::MAX, 2).is_none());
        assert!(
            library
                .read_bytes(library.descriptor() as usize, MAX_SNAPSHOT_BYTES + 1)
                .is_none()
        );
        let heap = [0u8; 16];
        assert!(
            library
                .read_bytes(heap.as_ptr() as usize, heap.len())
                .is_none()
        );
        assert_eq!(unsafe { GetKeyboardLayout(0) }, before);
    }
}
