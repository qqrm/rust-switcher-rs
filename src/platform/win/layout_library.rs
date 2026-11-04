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
            CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FreeLibrary,
            GENERIC_READ, HANDLE, HMODULE, HWND, INVALID_HANDLE_VALUE,
        },
        Security::{
            Cryptography::Catalog::{
                CATALOG_INFO, CryptCATAdminAcquireContext2, CryptCATAdminCalcHashFromFileHandle2,
                CryptCATAdminEnumCatalogFromHash, CryptCATAdminReleaseCatalogContext,
                CryptCATAdminReleaseContext, CryptCATCatalogInfoFromContext,
            },
            WinTrust::{
                DRIVER_ACTION_VERIFY, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_CATALOG_INFO,
                WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_DATA_UNION_CHOICE, WINTRUST_FILE_INFO,
                WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_CATALOG, WTD_CHOICE_FILE,
                WTD_DISABLE_MD2_MD4, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
                WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrust,
            },
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BEGIN,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, GetFileInformationByHandle,
            OPEN_EXISTING, SetFilePointerEx,
        },
        System::{
            Diagnostics::Debug::ReadProcessMemory,
            LibraryLoader::{
                GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
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
    },
    core::{PCWSTR, PWSTR, s, w},
};

const LAYOUTS_KEY: PCWSTR = w!("SYSTEM\\CurrentControlSet\\Control\\Keyboard Layouts");
const MAX_LAYOUTS: usize = 4096;
const MAX_STRING_BYTES: usize = 1024;
const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
const MAX_KEYBOARD_DLL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CATALOG_HASH_BYTES: usize = 64;
const MAX_CATALOGS_PER_HASH: usize = 16;

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

struct ReadFile(HANDLE);

impl Drop for ReadFile {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl ReadFile {
    fn open(path: PCWSTR) -> Option<Self> {
        // Do not share write or delete access: verification and loading must use
        // the same bytes. Reject a final-component reparse point rather than
        // following a keyboard DLL outside its registered System32 location.
        let file = Self(unsafe {
            CreateFileW(
                path,
                GENERIC_READ.0,
                FILE_SHARE_READ,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
            .ok()?
        });
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(file.0, &mut info).ok()? };
        let size = (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow);
        if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY.0 | FILE_ATTRIBUTE_REPARSE_POINT.0)
            != 0
            || size == 0
            || size > MAX_KEYBOARD_DLL_BYTES
        {
            return None;
        }
        Some(file)
    }

    fn rewind(&self) -> Option<()> {
        unsafe { SetFilePointerEx(self.0, 0, None, FILE_BEGIN).ok() }
    }
}

struct CatalogAdmin(isize);

impl Drop for CatalogAdmin {
    fn drop(&mut self) {
        let _ = unsafe { CryptCATAdminReleaseContext(self.0, 0) };
    }
}

struct CatalogSearch<'a> {
    admin: &'a CatalogAdmin,
    current: isize,
}

impl CatalogSearch<'_> {
    fn next(&mut self, hash: &[u8]) -> Option<isize> {
        // EnumCatalogFromHash advances/releases the previous enumeration
        // context. Release only the current context when stopping early, as in
        // Microsoft's Windows-classic-samples Security/CodeSigning sample.
        let previous = (self.current != 0).then_some(&mut self.current as *mut _);
        self.current =
            unsafe { CryptCATAdminEnumCatalogFromHash(self.admin.0, hash, None, previous) };
        (self.current != 0).then_some(self.current)
    }
}

impl Drop for CatalogSearch<'_> {
    fn drop(&mut self) {
        if self.current != 0 {
            let _ = unsafe { CryptCATAdminReleaseCatalogContext(self.admin.0, self.current, 0) };
        }
    }
}

fn verify_subject(choice: WINTRUST_DATA_UNION_CHOICE, subject: WINTRUST_DATA_0) -> i32 {
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: choice,
        Anonymous: subject,
        dwStateAction: WTD_STATEACTION_VERIFY,
        // No network retrieval, UI, hash-only mode or disabled usage policy.
        // Keep the normal Authenticode chain/publisher policy. The worker's
        // request deadline remains unchanged.
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_DISABLE_MD2_MD4,
        ..Default::default()
    };
    let status = unsafe {
        WinVerifyTrust(
            HWND(INVALID_HANDLE_VALUE.0),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    // CLOSE is required after every VERIFY, including a failed verification.
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    let _ = unsafe {
        WinVerifyTrust(
            HWND(INVALID_HANDLE_VALUE.0),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    // This is a LONG trust-provider status, not an HRESULT: only zero succeeds.
    status
}

fn verify_file_signature(file: &ReadFile, path: PCWSTR) -> bool {
    if file.rewind().is_none() {
        return false;
    }
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path,
        hFile: file.0,
        ..Default::default()
    };
    let status = verify_subject(WTD_CHOICE_FILE, WINTRUST_DATA_0 { pFile: &mut info });
    #[cfg(test)]
    eprintln!("native keyboard trust embedded status={:#x}", status as u32);
    if status == 0 {
        return true;
    }
    // System DLLs can be catalog signed without an embedded PE signature:
    // https://learn.microsoft.com/windows/win32/seccrypto/using-signtool-to-verify-a-file-signature
    // Use the system default catalog hash policy, then SHA-256 for catalogs
    // indexed with that algorithm on systems whose default remains SHA-1.
    [PCWSTR::null(), w!("SHA256")]
        .into_iter()
        .any(|algorithm| verify_catalog_signature(file, path, algorithm).is_some())
}

fn verify_catalog_signature(file: &ReadFile, path: PCWSTR, algorithm: PCWSTR) -> Option<()> {
    let mut raw_admin = 0;
    unsafe {
        CryptCATAdminAcquireContext2(
            &mut raw_admin,
            Some(&DRIVER_ACTION_VERIFY),
            algorithm,
            None,
            None,
        )
        .ok()?;
    }
    if raw_admin == 0 {
        return None;
    }
    let admin = CatalogAdmin(raw_admin);
    file.rewind()?;
    let mut length = 0;
    unsafe {
        CryptCATAdminCalcHashFromFileHandle2(admin.0, file.0, &mut length, None, None).ok()?;
    }
    let capacity = usize::try_from(length).ok()?;
    if capacity == 0 || capacity > MAX_CATALOG_HASH_BYTES {
        return None;
    }
    let mut hash = vec![0; capacity];
    file.rewind()?;
    unsafe {
        CryptCATAdminCalcHashFromFileHandle2(
            admin.0,
            file.0,
            &mut length,
            Some(hash.as_mut_ptr()),
            None,
        )
        .ok()?;
    }
    if usize::try_from(length).ok()? != capacity {
        return None;
    }
    let member_tag = catalog_member_tag(&hash);
    let mut search = CatalogSearch {
        admin: &admin,
        current: 0,
    };
    for _ in 0..MAX_CATALOGS_PER_HASH {
        let catalog = search.next(&hash)?;
        let mut info = CATALOG_INFO {
            cbStruct: size_of::<CATALOG_INFO>() as u32,
            ..Default::default()
        };
        if unsafe { CryptCATCatalogInfoFromContext(catalog, &mut info, 0) }.is_err()
            || !info.wszCatalogFile.contains(&0)
            || info.wszCatalogFile[0] == 0
        {
            continue;
        }
        let mut subject = WINTRUST_CATALOG_INFO {
            cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
            pcwszCatalogFilePath: PCWSTR(info.wszCatalogFile.as_ptr()),
            pcwszMemberTag: PCWSTR(member_tag.as_ptr()),
            pcwszMemberFilePath: path,
            hMemberFile: file.0,
            pbCalculatedFileHash: hash.as_mut_ptr(),
            cbCalculatedFileHash: length,
            hCatAdmin: admin.0,
            ..Default::default()
        };
        file.rewind()?;
        let status = verify_subject(
            WTD_CHOICE_CATALOG,
            WINTRUST_DATA_0 {
                pCatalog: &mut subject,
            },
        );
        #[cfg(test)]
        eprintln!(
            "native keyboard trust catalog hash_bytes={length} status={:#x}",
            status as u32
        );
        if status == 0 {
            return Some(());
        }
    }
    None
}

fn catalog_member_tag(hash: &[u8]) -> Vec<u16> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    hash.iter()
        .flat_map(|byte| {
            [
                u16::from(HEX[(byte >> 4) as usize]),
                u16::from(HEX[(byte & 0x0f) as usize]),
            ]
        })
        .chain(std::iter::once(0))
        .collect()
}

/// Keeps a trusted keyboard DLL mapped until the caller has copied its tables.
/// This object stays on the parsing thread; only the resulting owned tables
/// should be sent to other workers.
pub(crate) struct KeyboardLibrary {
    module: Module,
    _verified_file: ReadFile,
    descriptor: *const c_void,
}

impl KeyboardLibrary {
    /// Resolve a complete Windows HKL to one registered System32 DLL.
    /// Custom unsigned DLLs, IMEs and unsupported HKL encodings are unavailable.
    /// The provider validates its live enabled-layout catalog before requesting
    /// conversion; the decoder supplies the actual foreground HKL. A worker's
    /// own catalog need not include handles loaded by a different thread.
    pub(crate) fn load(hkl: usize) -> Option<Self> {
        macro_rules! required {
            ($stage:literal, $value:expr) => {
                match $value {
                    Some(value) => value,
                    None => {
                        #[cfg(test)]
                        eprintln!(
                            "native keyboard loader hkl={hkl:#x} stage={} error={}",
                            $stage,
                            std::io::Error::last_os_error()
                        );
                        return None;
                    }
                }
            };
        }
        let canonical = required!("canonical HKL", canonical_hkl(hkl));
        // ImmIsIME reports IME capability for ordinary layouts on modern Windows
        // too. Resolve the physical registration and reject its actual IME file
        // below instead of treating that broad capability as an IME identity.
        // Chromium documents the same distinction in imm32_manager.cc:
        // https://chromium.googlesource.com/chromium/src/+/5e16d46c92747ae76914b9a5db114a19cdb00bde/ui/base/ime/win/imm32_manager.cc
        let registry = required!(
            "layout registry",
            open_key(
                HKEY_LOCAL_MACHINE,
                LAYOUTS_KEY,
                KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS,
            )
        );
        let klid = required!(
            "physical KLID resolution",
            resolve_klid(canonical, &registry)
        );
        let klid = wide(&klid);
        let layout = required!(
            "physical layout registration",
            open_key(registry.0, PCWSTR(klid.as_ptr()), KEY_QUERY_VALUE)
        );
        let ime = required!(
            "IME registry value",
            registry_string(&layout, w!("Ime File"))
        );
        required!("non-IME registration", ime.is_none().then_some(()));
        let filename = required!(
            "Layout File value",
            registry_string(&layout, w!("Layout File")).flatten()
        );
        required!("DLL basename", valid_dll_basename(&filename).then_some(()));
        let directory = required!("system directory", system_directory());
        let path = format!("{directory}\\{filename}");
        let path_wide = wide(&path);
        let file = required!(
            "bounded read-only keyboard file",
            ReadFile::open(PCWSTR(path_wide.as_ptr()))
        );
        required!(
            "trusted embedded or catalog signature",
            verify_file_signature(&file, PCWSTR(path_wide.as_ptr())).then_some(())
        );
        // Use an absolute system path for the DLL and System32 only for its
        // dependencies. Do not search the application, working directory or PATH.
        // REQUIRE_SIGNED_TARGET rejects the inbox keyboard DLLs on the native
        // runner. The exact open file has already passed embedded or catalog
        // Authenticode verification above, before any DLL code is executed.
        let loaded = unsafe {
            LoadLibraryExW(
                PCWSTR(path_wide.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        #[cfg(test)]
        if let Err(error) = &loaded {
            eprintln!("native keyboard loader path={path:?} LoadLibraryExW error={error:?}");
        }
        let module = Module(required!("verified System32 load", loaded.ok()));
        let actual_path = required!("loaded module path", module_path(module.0));
        required!(
            "matching module path",
            actual_path.eq_ignore_ascii_case(&path).then_some(())
        );
        let mut library = Self {
            module,
            _verified_file: file,
            descriptor: std::ptr::null(),
        };
        let entry = required!("KbdLayerDescriptor export", unsafe {
            GetProcAddress(library.module.0, s!("KbdLayerDescriptor"))
        });
        // Reject forwarded exports and data addresses before calling the WDK ABI.
        required!(
            "descriptor executable code",
            library.validate_range(entry as usize, 1, true)
        );
        let descriptor: unsafe extern "system" fn() -> *const c_void =
            unsafe { std::mem::transmute(entry) };
        library.descriptor = unsafe { descriptor() };
        required!(
            "descriptor readable image",
            library.validate_range(library.descriptor as usize, 1, false)
        );
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
    fn native_layout_library_rejects_unsigned_files_and_locks_signature_input() {
        let Ok(directory) = tempfile::tempdir() else {
            panic!("create isolated signature fixture directory");
        };
        let path = directory.path().join("unsigned.dll");
        assert!(std::fs::write(&path, b"MZ unsigned keyboard table fixture").is_ok());
        let path_wide = wide(&path.to_string_lossy());
        let Some(file) = ReadFile::open(PCWSTR(path_wide.as_ptr())) else {
            panic!("open isolated signature fixture");
        };
        assert!(!verify_file_signature(&file, PCWSTR(path_wide.as_ptr())));
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        drop(file);
        assert!(std::fs::remove_file(&path).is_ok());
        assert_eq!(catalog_member_tag(&[0x00, 0xab, 0xff]), wide("00ABFF"));
    }

    // Select an initialized code byte, outside all PE headers and the excluded
    // certificate table. The fixture is already trusted, but still bound every
    // offset before modifying its temporary copy. PE32+ matches our 64-bit loader.
    fn pe_code_byte_to_tamper(image: &[u8]) -> Option<usize> {
        fn word(bytes: &[u8], offset: usize) -> Option<u16> {
            Some(u16::from_le_bytes(
                bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
            ))
        }

        fn dword(bytes: &[u8], offset: usize) -> Option<usize> {
            usize::try_from(u32::from_le_bytes(
                bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
            ))
            .ok()
        }

        if image.get(..2)? != b"MZ" {
            return None;
        }
        let pe = dword(image, 0x3c)?;
        let coff_start = pe.checked_add(4)?;
        if image.get(pe..coff_start)? != b"PE\0\0" {
            return None;
        }
        let optional_start = coff_start.checked_add(20)?;
        let coff = image.get(coff_start..optional_start)?;
        let section_count = usize::from(word(coff, 2)?);
        if !(1..=96).contains(&section_count) {
            return None;
        }
        let section_start = optional_start.checked_add(usize::from(word(coff, 16)?))?;
        let optional = image.get(optional_start..section_start)?;
        if word(optional, 0)? != 0x20b || dword(optional, 108)? < 5 {
            return None;
        }
        let headers_size = dword(optional, 60)?;
        // IMAGE_DIRECTORY_ENTRY_SECURITY is entry 4. Its address is a file
        // offset, unlike the RVAs used by other optional-header directories.
        let certificate_start = dword(optional, 112 + 4 * 8)?;
        let certificate_end = certificate_start.checked_add(dword(optional, 112 + 4 * 8 + 4)?)?;
        let section_end = section_start.checked_add(section_count.checked_mul(40)?)?;
        if section_end > headers_size || headers_size > image.len() || certificate_end > image.len()
        {
            return None;
        }
        for section in image.get(section_start..section_end)?.as_chunks::<40>().0 {
            // IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_EXECUTE.
            if dword(section, 36)? & 0x2000_0020 != 0x2000_0020 {
                continue;
            }
            let raw_start = dword(section, 20)?;
            let raw_size = dword(section, 16)?;
            let raw_end = raw_start.checked_add(raw_size)?;
            let initialized_size = raw_size.min(dword(section, 8)?);
            if initialized_size == 0
                || raw_start < headers_size
                || raw_end > image.len()
                || (raw_start < certificate_end && certificate_start < raw_end)
            {
                continue;
            }
            return raw_start.checked_add(initialized_size / 2);
        }
        None
    }

    #[test]
    fn native_layout_library_rejects_tampered_signed_keyboard_image()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::windows::ffi::OsStrExt;

        use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;

        let _guard = crate::input::ring_buffer::test_guard();
        let before = unsafe { GetKeyboardLayout(0) };
        let library = KeyboardLibrary::load(before.0 as usize)
            .ok_or("the active ordinary layout must have a trusted keyboard DLL")?;
        let source = module_path(library.module.0).ok_or("resolve the trusted keyboard DLL")?;
        let mut image = std::fs::read(source)?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("keyboard.dll");
        // Copy the bytes without inheriting System32 file attributes. All writes
        // target the temporary file, which is never loaded as code.
        std::fs::write(&path, &image)?;
        let path_wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        {
            let file = ReadFile::open(PCWSTR(path_wide.as_ptr()))
                .ok_or("open the untouched keyboard DLL copy")?;
            assert!(
                verify_file_signature(&file, PCWSTR(path_wide.as_ptr())),
                "the untouched PE copy must retain embedded or catalog trust"
            );
        }

        let offset = pe_code_byte_to_tamper(&image)
            .ok_or("find a bounded, hashed code section in the trusted PE image")?;
        image[offset] ^= 1;
        std::fs::write(&path, &image)?;
        let file = ReadFile::open(PCWSTR(path_wide.as_ptr()))
            .ok_or("open the tampered keyboard DLL copy as data")?;
        assert!(
            !verify_file_signature(&file, PCWSTR(path_wide.as_ptr())),
            "changing signed code bytes must invalidate both embedded and catalog trust"
        );
        assert_eq!(unsafe { GetKeyboardLayout(0) }, before);
        Ok(())
    }

    #[test]
    fn native_layout_library_reads_installed_table_without_changing_active_layout() {
        use windows::Win32::UI::Input::{
            Ime::{ImmGetIMEFileNameW, ImmIsIME},
            KeyboardAndMouse::GetKeyboardLayout,
        };
        let _guard = crate::input::ring_buffer::test_guard();
        let before = unsafe { GetKeyboardLayout(0) };
        eprintln!(
            "native keyboard loader hkl={:#x} ImmIsIME={} IME filename length={}",
            before.0 as usize,
            unsafe { ImmIsIME(before) }.0,
            unsafe { ImmGetIMEFileNameW(before, None) },
        );
        let started = std::time::Instant::now();
        let Some(library) = KeyboardLibrary::load(before.0 as usize) else {
            panic!("the Windows test runner's active ordinary layout must expose keyboard tables");
        };
        eprintln!(
            "native keyboard loader elapsed_ms={}",
            started.elapsed().as_millis()
        );
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
