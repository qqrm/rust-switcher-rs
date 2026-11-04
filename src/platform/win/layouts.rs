//! Keyboard-layout conversion using the layouts already loaded by Windows.
//!
//! The inverse tables retain physical scan codes, not virtual-key names: for
//! example, the same position is Y in US QWERTY and Z in German QWERTZ.
//! Stateful dead-key translation runs only on a worker whose input queue is never
//! attached to the hook, UI or foreground threads.

#[cfg(test)]
use std::sync::{Mutex, MutexGuard};
use std::{collections::HashMap, sync::OnceLock, time::Duration};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    Globalization::LCIDToLocaleName,
    UI::{
        Input::{
            Ime::ImmIsIME,
            KeyboardAndMouse::{
                GetKeyboardLayout, GetKeyboardLayoutList, HKL, MAPVK_VK_TO_VSC_EX,
                MAPVK_VSC_TO_VK_EX, MapVirtualKeyExW, ToUnicodeEx, VK_CAPITAL, VK_CONTROL,
                VK_LCONTROL, VK_LSHIFT, VK_MENU, VK_RMENU, VK_SHIFT, VK_SPACE, VkKeyScanExW,
            },
        },
        WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId, IsWindow, PostMessageW,
            WM_INPUTLANGCHANGEREQUEST,
        },
    },
};

use crate::utils::bounded_worker::BoundedWorker;

const SHIFT: u8 = 1;
const CONTROL: u8 = 2;
const ALT: u8 = 4;
const CAPS_LOCK: u8 = 8;
const ALT_GR: u8 = CONTROL | ALT;
const KEY_STATES: [u8; 8] = [
    0,
    SHIFT,
    ALT_GR,
    ALT_GR | SHIFT,
    CAPS_LOCK,
    CAPS_LOCK | SHIFT,
    CAPS_LOCK | ALT_GR,
    CAPS_LOCK | ALT_GR | SHIFT,
];
const UTF16_CAPACITY: usize = 16;
const NO_STATE_CHANGE: u32 = 4;
const CONVERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// A complete HKL, including its keyboard variant, valid for this Windows session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LayoutId(pub usize);

/// Public-provider tests share its deliberately bounded production worker.
#[cfg(test)]
pub(crate) fn test_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl LayoutId {
    fn hkl(self) -> HKL {
        HKL(self.0 as *mut _)
    }
}

/// Query every time a conversion is planned, so Windows remains the only list of
/// enabled layouts. IMEs require composition APIs rather than key substitution.
pub(crate) fn installed_layouts() -> Vec<LayoutId> {
    let count = unsafe { GetKeyboardLayoutList(None) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    if count == 0 || count > 1024 {
        return Vec::new();
    }
    let mut handles = vec![HKL::default(); count];
    let copied = unsafe { GetKeyboardLayoutList(Some(&mut handles)) };
    let Ok(copied) = usize::try_from(copied) else {
        return Vec::new();
    };
    let mut layouts = Vec::with_capacity(copied.min(count));
    for hkl in handles.into_iter().take(copied) {
        if hkl.0.is_null() || unsafe { ImmIsIME(hkl) }.as_bool() {
            continue;
        }
        let layout = LayoutId(hkl.0 as usize);
        if !layouts.contains(&layout) {
            layouts.push(layout);
        }
    }
    layouts
}

pub(crate) fn foreground_layout() -> Option<LayoutId> {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() {
        return None;
    }
    let thread_id = unsafe { GetWindowThreadProcessId(foreground, None) };
    if thread_id == 0 {
        return None;
    }
    let hkl = unsafe { GetKeyboardLayout(thread_id) };
    if hkl.0.is_null() || unsafe { ImmIsIME(hkl) }.as_bool() {
        return None;
    }
    Some(LayoutId(hkl.0 as usize))
}

/// The locale is used only to select a language model. It never identifies the
/// physical layout: multiple HKLs may have the same language tag.
pub(crate) fn language_tag(layout: LayoutId) -> Option<String> {
    if layout.0 == 0 {
        return None;
    }
    let mut name = [0u16; 85];
    let length = unsafe { LCIDToLocaleName((layout.0 & 0xffff) as u32, Some(&mut name), 0) };
    let length = usize::try_from(length).ok()?.checked_sub(1)?;
    String::from_utf16(name.get(..length)?).ok()
}

/// Start table preparation at application startup; never wait inside the hook.
pub(crate) fn warm_layouts() {
    let _ = worker();
}

/// Returns `None` when a complete, printable transformation cannot be produced.
/// In particular, unsupported letters are never silently copied into a partly
/// converted word. Emoji and other non-keyboard symbols remain unchanged.
pub(crate) fn convert_text(text: &str, source: LayoutId, target: LayoutId) -> Option<String> {
    let available = installed_layouts();
    if !available.contains(&source) || !available.contains(&target) {
        return None;
    }
    if source == target || text.is_empty() {
        return Some(text.to_owned());
    }
    worker()?.request(
        ConversionRequest {
            text: text.to_owned(),
            source,
            target,
            available,
        },
        CONVERSION_TIMEOUT,
    )?
}

/// Activate the exact result layout only while the captured window is current.
pub(crate) fn activate_layout_for_window(target: LayoutId, foreground: HWND) -> Result<(), String> {
    if foreground.0.is_null()
        || !unsafe { IsWindow(Some(foreground)) }.as_bool()
        || unsafe { GetForegroundWindow() } != foreground
    {
        return Err("The foreground window changed before switching layouts".to_owned());
    }
    if !installed_layouts().contains(&target) {
        return Err("The requested keyboard layout is no longer available".to_owned());
    }
    unsafe {
        PostMessageW(
            Some(foreground),
            WM_INPUTLANGCHANGEREQUEST,
            WPARAM(0),
            LPARAM(target.0 as isize),
        )
    }
    .map_err(|error| error.to_string())
}

struct ConversionRequest {
    text: String,
    source: LayoutId,
    target: LayoutId,
    available: Vec<LayoutId>,
}

type Worker = BoundedWorker<ConversionRequest, Option<String>>;

fn worker() -> Option<&'static Worker> {
    static WORKER: OnceLock<Option<Worker>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            Worker::start("layout-conversion", || {
                let mut converter = Converter::default();
                for layout in installed_layouts() {
                    if let Some(table) = LayoutTable::build(layout) {
                        converter.tables.insert(layout, table);
                    }
                }
                move |request| converter.convert(request)
            })
            .ok()
        })
        .as_ref()
}

#[derive(Default)]
struct Converter {
    tables: HashMap<LayoutId, LayoutTable>,
}

impl Converter {
    fn convert(&mut self, request: ConversionRequest) -> Option<String> {
        self.tables
            .retain(|layout, _| request.available.contains(layout));
        if let std::collections::hash_map::Entry::Vacant(entry) = self.tables.entry(request.source)
        {
            let table = LayoutTable::build(request.source)?;
            entry.insert(table);
        }
        let source = self.tables.get(&request.source)?;
        let input = source.encode(&request.text)?;
        translate_sequence(&input, request.target)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct KeyStroke {
    scan: u32,
    modifiers: u8,
}

impl KeyStroke {
    fn from_vk(vk: u32, modifiers: u8, layout: LayoutId) -> Option<Self> {
        let scan = unsafe { MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC_EX, Some(layout.hkl())) };
        (scan != 0).then_some(Self { scan, modifiers })
    }

    fn priority(self) -> (bool, bool, bool, u32) {
        (
            self.modifiers & CAPS_LOCK != 0,
            self.modifiers & ALT_GR != 0,
            self.modifiers & SHIFT != 0,
            self.scan,
        )
    }
}

enum InputUnit {
    Key(KeyStroke),
    Literal(char),
}

struct Mapping {
    text: String,
    keys: Vec<KeyStroke>,
}

struct LayoutTable {
    by_first_char: HashMap<char, Vec<Mapping>>,
}

impl LayoutTable {
    fn build(layout: LayoutId) -> Option<Self> {
        if !clear_dead_keys(layout) {
            return None;
        }
        let mut mappings: HashMap<String, Vec<KeyStroke>> = HashMap::new();
        let mut keys = Vec::new();
        let mut dead_keys = Vec::new();

        for modifiers in KEY_STATES {
            for vk in 1..=254 {
                // VkKeyScanExW also prefers the main keyboard. A numpad digit must
                // not win over its number-row key with a different shifted symbol.
                if (0x60..=0x6f).contains(&vk) {
                    continue;
                }
                let Some(key) = KeyStroke::from_vk(vk, modifiers, layout) else {
                    continue;
                };
                match translate_key(layout, key, NO_STATE_CHANGE)? {
                    Translation::Text(text) => {
                        if is_printable(&text) {
                            insert_mapping(&mut mappings, text, vec![key]);
                            if !keys.contains(&key) {
                                keys.push(key);
                            }
                        }
                    }
                    Translation::Dead(text) => {
                        insert_mapping(&mut mappings, text, vec![key]);
                        if !dead_keys.contains(&key) {
                            dead_keys.push(key);
                        }
                    }
                    Translation::Empty => {}
                }
            }
        }

        // Discover the driver's own compositions instead of assuming Unicode
        // normalization reproduces its dead-key rules.
        for &dead in &dead_keys {
            for &key in &keys {
                if !clear_dead_keys(layout) {
                    return None;
                }
                if !matches!(translate_key(layout, dead, 0)?, Translation::Dead(_)) {
                    return None;
                }
                if let Translation::Text(text) = translate_key(layout, key, 0)? {
                    insert_mapping(&mut mappings, text, vec![dead, key]);
                }
            }
        }
        if !clear_dead_keys(layout) || mappings.is_empty() {
            return None;
        }

        // Respect Windows' canonical key for an individual character when one
        // exists. This resolves duplicates such as the two backslash positions.
        for (text, keys) in &mut mappings {
            let mut chars = text.chars();
            let Some(ch) = chars.next() else {
                continue;
            };
            if chars.next().is_some() || ch.len_utf16() != 1 {
                continue;
            }
            let encoded = unsafe { VkKeyScanExW(ch as u16, layout.hkl()) };
            if encoded == -1 {
                continue;
            }
            let encoded = encoded as u16;
            let modifiers = (encoded >> 8) as u8;
            if modifiers & !(SHIFT | ALT_GR) != 0 {
                continue;
            }
            let Some(key) = KeyStroke::from_vk(u32::from(encoded & 0xff), modifiers, layout) else {
                continue;
            };
            if matches!(translate_key(layout, key, NO_STATE_CHANGE), Some(Translation::Text(ref candidate) | Translation::Dead(ref candidate)) if candidate == text)
            {
                *keys = vec![key];
            }
        }

        let mut by_first_char: HashMap<char, Vec<Mapping>> = HashMap::new();
        for (text, keys) in mappings {
            if let Some(first) = text.chars().next() {
                by_first_char
                    .entry(first)
                    .or_default()
                    .push(Mapping { text, keys });
            }
        }
        for entries in by_first_char.values_mut() {
            entries.sort_by(|left, right| {
                right
                    .text
                    .len()
                    .cmp(&left.text.len())
                    .then_with(|| left.text.cmp(&right.text))
            });
        }
        Some(Self { by_first_char })
    }

    fn encode(&self, text: &str) -> Option<Vec<InputUnit>> {
        let mut input = Vec::with_capacity(text.chars().count());
        let mut remaining = text;
        while let Some(first) = remaining.chars().next() {
            let mapping = self.by_first_char.get(&first).and_then(|entries| {
                entries
                    .iter()
                    .find(|mapping| remaining.starts_with(&mapping.text))
            });
            if let Some(mapping) = mapping {
                input.extend(mapping.keys.iter().copied().map(InputUnit::Key));
                remaining = &remaining[mapping.text.len()..];
            } else {
                if first.is_alphabetic() {
                    return None;
                }
                input.push(InputUnit::Literal(first));
                remaining = &remaining[first.len_utf8()..];
            }
        }
        Some(input)
    }
}

fn insert_mapping(
    mappings: &mut HashMap<String, Vec<KeyStroke>>,
    text: String,
    keys: Vec<KeyStroke>,
) {
    if !is_printable(&text) {
        return;
    }
    match mappings.entry(text) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(keys);
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            let old = entry.get();
            if keys.len() < old.len()
                || (keys.len() == old.len()
                    && keys
                        .iter()
                        .map(|key| key.priority())
                        .cmp(old.iter().map(|key| key.priority()))
                        == std::cmp::Ordering::Less)
            {
                entry.insert(keys);
            }
        }
    }
}

fn is_printable(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(char::is_control)
}

enum Translation {
    Text(String),
    Dead(String),
    Empty,
}

fn translate_key(layout: LayoutId, key: KeyStroke, flags: u32) -> Option<Translation> {
    let vk = unsafe { MapVirtualKeyExW(key.scan, MAPVK_VSC_TO_VK_EX, Some(layout.hkl())) };
    if vk == 0 || vk > 255 {
        return Some(Translation::Empty);
    }
    let mut state = [0u8; 256];
    state[vk as usize] = 0x80;
    if key.modifiers & SHIFT != 0 {
        state[usize::from(VK_SHIFT.0)] = 0x80;
        state[usize::from(VK_LSHIFT.0)] = 0x80;
    }
    if key.modifiers & CONTROL != 0 {
        state[usize::from(VK_CONTROL.0)] = 0x80;
        state[usize::from(VK_LCONTROL.0)] = 0x80;
    }
    if key.modifiers & ALT != 0 {
        state[usize::from(VK_MENU.0)] = 0x80;
        state[usize::from(VK_RMENU.0)] = 0x80;
    }
    state[usize::from(VK_CAPITAL.0)] = u8::from(key.modifiers & CAPS_LOCK != 0);
    let mut output = [0u16; UTF16_CAPACITY];
    let count =
        unsafe { ToUnicodeEx(vk, key.scan, &state, &mut output, flags, Some(layout.hkl())) };
    if count == 0 {
        return Some(Translation::Empty);
    }
    let length = usize::try_from(count.unsigned_abs()).ok()?;
    let text = String::from_utf16(output.get(..length)?).ok()?;
    Some(if count < 0 {
        Translation::Dead(text)
    } else {
        Translation::Text(text)
    })
}

fn clear_dead_keys(layout: LayoutId) -> bool {
    let Some(space) = KeyStroke::from_vk(u32::from(VK_SPACE.0), 0, layout) else {
        return false;
    };
    for _ in 0..8 {
        match translate_key(layout, space, 0) {
            Some(Translation::Dead(_)) => {}
            Some(_) => return true,
            None => return false,
        }
    }
    false
}

fn translate_sequence(input: &[InputUnit], target: LayoutId) -> Option<String> {
    if !clear_dead_keys(target) {
        return None;
    }
    let result = translate_sequence_inner(input, target);
    // A rejected conversion must not leak pending composition into the next one.
    let clean = clear_dead_keys(target);
    clean.then_some(result).flatten()
}

fn translate_sequence_inner(input: &[InputUnit], target: LayoutId) -> Option<String> {
    let mut output = String::new();
    let mut pending = false;
    for unit in input {
        match unit {
            InputUnit::Key(key) => match translate_key(target, *key, 0)? {
                Translation::Text(text) if is_printable(&text) => {
                    output.push_str(&text);
                    pending = false;
                }
                Translation::Dead(_) => pending = true,
                Translation::Text(_) | Translation::Empty => return None,
            },
            InputUnit::Literal(ch) => {
                if pending {
                    output.push_str(&finish_dead_key(target)?);
                    pending = false;
                }
                output.push(*ch);
            }
        }
    }
    if pending {
        output.push_str(&finish_dead_key(target)?);
    }
    Some(output)
}

fn finish_dead_key(layout: LayoutId) -> Option<String> {
    let space = KeyStroke::from_vk(u32::from(VK_SPACE.0), 0, layout)?;
    match translate_key(layout, space, 0)? {
        Translation::Text(text) if is_printable(&text) => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use windows::{
        Win32::UI::{
            Input::KeyboardAndMouse::{
                ActivateKeyboardLayout, KLF_ACTIVATE, KLF_NOTELLSHELL, LoadKeyboardLayoutW,
                SetFocus, UnloadKeyboardLayout,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, SetForegroundWindow, WINDOW_EX_STYLE,
                WS_OVERLAPPEDWINDOW, WS_VISIBLE,
            },
        },
        core::{PCWSTR, w},
    };

    use super::*;

    fn load_layout(klid: &str) -> LayoutId {
        let name: Vec<u16> = klid.encode_utf16().chain(Some(0)).collect();
        let hkl = unsafe { LoadKeyboardLayoutW(PCWSTR(name.as_ptr()), KLF_NOTELLSHELL) }
            .expect("the native test layout must be installed on the Windows runner");
        LayoutId(hkl.0 as usize)
    }

    fn convert_native(text: &str, source: &str, target: &str) -> Option<String> {
        let text = text.to_owned();
        let source = source.to_owned();
        let target = target.to_owned();
        // Each test uses a private translation thread and never touches the
        // production worker or another test's pending composition.
        std::thread::spawn(move || {
            let source = load_layout(&source);
            let target = load_layout(&target);
            let table = LayoutTable::build(source)?;
            translate_sequence(&table.encode(&text)?, target)
        })
        .join()
        .expect("native conversion thread must not panic")
    }

    struct TestDesktop {
        window: HWND,
        previous_window: HWND,
        previous_layout: HKL,
        added_layout: Option<LayoutId>,
    }

    impl TestDesktop {
        fn open() -> Self {
            let previous_window = unsafe { GetForegroundWindow() };
            let previous_layout = unsafe { GetKeyboardLayout(0) };
            let window = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Rust Switcher isolated layout catalog test"),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    0,
                    0,
                    360,
                    100,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .expect("the isolated layout test must create its own foreground window");
            let desktop = Self {
                window,
                previous_window,
                previous_layout,
                added_layout: None,
            };
            unsafe {
                let _ = SetForegroundWindow(window);
                let _ = SetFocus(Some(window));
            }
            assert_eq!(
                unsafe { GetForegroundWindow() },
                window,
                "LoadKeyboardLayout requires the test process to own keyboard focus"
            );
            desktop
        }
    }

    impl Drop for TestDesktop {
        fn drop(&mut self) {
            unsafe {
                let _ = ActivateKeyboardLayout(self.previous_layout, Default::default());
                if let Some(layout) = self.added_layout {
                    let _ = UnloadKeyboardLayout(layout.hkl());
                }
                let _ = DestroyWindow(self.window);
                if !self.previous_window.0.is_null() {
                    let _ = SetForegroundWindow(self.previous_window);
                }
            }
        }
    }

    #[test]
    #[ignore = "requires initialize-layouts.ps1 and a disposable interactive Windows desktop"]
    fn native_public_layout_catalog_refreshes_after_add_and_remove() {
        for (name, value) in [
            ("RUST_SWITCHER_HOSTED_E2E", "1"),
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
            ("RUNNER_OS", "Windows"),
        ] {
            assert_eq!(
                std::env::var(name).as_deref(),
                Ok(value),
                "catalog mutation is restricted to the disposable hosted Windows job"
            );
        }

        let _guard = test_guard();
        let mut desktop = TestDesktop::open();
        let english = load_layout("00000409");
        let russian = load_layout("00000419");
        let before = installed_layouts();
        assert!(before.contains(&english) && before.contains(&russian));
        assert!(
            !before.iter().any(|layout| layout.0 & 0xffff == 0x041d),
            "the isolated fixture must start without a Swedish layout"
        );

        // This is deliberately the first request in a separate test process:
        // it exercises the real catalog, cold worker and complete conversion.
        assert_eq!(
            convert_text(".ghbdtn", english, russian),
            Some("юпривет".to_owned())
        );

        let swedish = LayoutId(
            unsafe { LoadKeyboardLayoutW(w!("0000041D"), KLF_ACTIVATE) }
                .expect("the native Swedish layout must be available")
                .0 as usize,
        );
        desktop.added_layout = Some(swedish);
        assert_eq!(swedish.0 & 0xffff, 0x041d);
        assert!(installed_layouts().contains(&swedish));
        assert_eq!(
            convert_text("åäö", swedish, english),
            Some("[';".to_owned())
        );

        unsafe { ActivateKeyboardLayout(english.hkl(), Default::default()) }
            .expect("English must be active before unloading the temporary layout");
        unsafe { UnloadKeyboardLayout(swedish.hkl()) }
            .expect("the temporary Swedish layout must unload from the live catalog");
        desktop.added_layout = None;
        assert!(!installed_layouts().contains(&swedish));
        assert_eq!(convert_text("åäö", swedish, english), None);
        assert_eq!(
            convert_text("ghbdtn", english, russian),
            Some("привет".to_owned())
        );
    }

    #[test]
    fn native_layout_preserves_leading_punctuation_and_case() {
        assert_eq!(
            convert_native(".Ghbdtn", "00000409", "00000419"),
            Some("юПривет".to_owned())
        );
        assert_eq!(
            convert_native(".Привет", "00000419", "00000409"),
            Some("/Ghbdtn".to_owned())
        );
    }

    #[test]
    fn native_layout_uses_physical_positions_for_qwertz() {
        assert_eq!(
            convert_native("zZyY", "00000407", "00000409"),
            Some("yYzZ".to_owned())
        );
    }

    #[test]
    fn native_layout_supports_ukrainian_without_a_language_allowlist() {
        assert_eq!(
            convert_native("привіт", "00000422", "00000409"),
            Some("ghbdsn".to_owned())
        );
    }

    #[test]
    fn native_layout_uses_french_number_row_and_azerty_positions() {
        assert_eq!(
            convert_native("azé&", "0000040c", "00000409"),
            Some("qw21".to_owned())
        );
    }

    #[test]
    fn native_layout_recovers_composed_dead_key_as_a_key_sequence() {
        assert_eq!(
            convert_native("ê", "00000407", "00000409"),
            Some("`e".to_owned())
        );
        assert_eq!(
            convert_native("`e", "00000409", "00000407"),
            Some("ê".to_owned())
        );
    }

    #[test]
    fn native_layout_keeps_english_keyboard_variants_distinct() {
        assert_eq!(
            convert_native("ê", "00020409", "00000409"),
            Some("^e".to_owned())
        );
    }

    #[test]
    fn native_layout_flushes_a_trailing_dead_key_as_visible_text() {
        assert_eq!(
            convert_native("`", "00000409", "00000407"),
            Some("^".to_owned())
        );
    }

    #[test]
    fn native_layout_preserves_unicode_symbols_and_line_breaks() {
        assert_eq!(
            convert_native("🙂 ghbdtn\n", "00000409", "00000419"),
            Some("🙂 привет\n".to_owned())
        );
    }

    #[test]
    fn native_layout_rejects_unsupported_source_letters() {
        assert_eq!(convert_native("helloЖ", "00000409", "00000407"), None);
    }

    #[test]
    fn native_layout_does_not_turn_altgr_into_control_characters() {
        assert_eq!(convert_native("ą", "00000415", "00000409"), None);
    }
}
