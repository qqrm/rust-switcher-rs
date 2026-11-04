//! Immutable Windows keyboard tables with Rust-owned dead-key composition.
//!
//! ABI definitions come from Microsoft's WDK `um/kbd.h`:
//! https://github.com/microsoft/wdkmetadata/blob/fbb3a13785073a707c6171bffbd5522438031732/generation/WDK/IdlHeaders/um/kbd.h
//! The loader checks image ownership and readable pages before copying bytes.
//! This module never dereferences a driver pointer or calls `ToUnicodeEx`.

use std::{
    collections::HashMap,
    mem::{offset_of, size_of},
};

use crate::layout_library::KeyboardLibrary;

const WCH_NONE: u16 = 0xf000;
const WCH_DEAD: u16 = 0xf001;
const WCH_LGTR: u16 = 0xf002;
const CAPLOK: u8 = 0x01;
const CAPLOKALTGR: u8 = 0x04;
const INVALID_SHIFT: u8 = 0x0f;
const SHIFT: u8 = 1;
const CONTROL: u8 = 2;
const ALT: u8 = 4;
const MAX_TABLES: usize = 32;
const MAX_ROWS: usize = 512;
const MAX_DEAD_KEYS: usize = 4096;
const MAX_LIGATURES: usize = 512;
const MAX_READ_BYTES: usize = 1024 * 1024;

// These integer-only structures define native ABI offsets. They are never
// instantiated from untrusted bytes, and no pointer in them is dereferenced.
#[repr(C)]
struct RawTables {
    modifiers: usize,
    characters: usize,
    dead_keys: usize,
    key_names: usize,
    extended_key_names: usize,
    dead_key_names: usize,
    scan_to_vk: usize,
    scan_count: u8,
    extended_scan_to_vk: usize,
    alternate_scan_to_vk: usize,
    locale_flags: u32,
    ligature_length: u8,
    ligature_stride: u8,
    ligatures: usize,
}

#[repr(C)]
struct RawCharacterTable {
    rows: usize,
    columns: u8,
    stride: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum KeyOutput {
    None,
    Text(Vec<u16>),
    Dead(u16),
}

#[derive(Clone, Debug)]
struct KeyRow {
    attributes: u8,
    columns: Vec<KeyOutput>,
}

#[derive(Clone, Copy, Debug)]
struct DeadTransition {
    composed: u16,
    remains_dead: bool,
}

/// The tables own every byte they use; the keyboard DLL may be unloaded after
/// parsing. Unknown ABI versions and unsupported special processing fail closed.
pub(crate) struct KeyboardTables {
    modifiers: Vec<(u8, u8)>,
    modification_numbers: Vec<u8>,
    rows: Vec<Option<KeyRow>>,
    dead_keys: HashMap<(u16, u16), DeadTransition>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Composition {
    accent: Option<u16>,
}

impl Composition {
    pub(crate) fn is_pending(&self) -> bool {
        self.accent.is_some()
    }

    pub(crate) fn spacing_accent(&self) -> Option<String> {
        String::from_utf16(&[self.accent?]).ok()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Translation {
    Text(String),
    PendingDeadKey,
    NoText,
}

impl KeyboardTables {
    pub(crate) fn load(hkl: usize) -> Option<Self> {
        // WOW64 keyboard tables can use __ptr64 in a 32-bit process. Until that
        // ABI is explicitly supported, never interpret it as native pointers.
        if size_of::<usize>() != 8 {
            return None;
        }
        let library = KeyboardLibrary::load(hkl)?;
        Self::parse(library.descriptor() as usize, |address, length| {
            library.read_bytes(address, length)
        })
    }

    fn parse(descriptor: usize, read: impl FnMut(usize, usize) -> Option<Vec<u8>>) -> Option<Self> {
        let mut reader = Reader {
            read,
            remaining: MAX_READ_BYTES,
        };
        let raw = reader.bytes(descriptor, size_of::<RawTables>())?;
        let flags = read_u32(&raw, offset_of!(RawTables, locale_flags))?;
        // WDK version 1 introduces the ligature fields. ALTGR changes input
        // modifier synthesis, which the caller supplies explicitly. SHIFTLOCK,
        // LRM/RLM and unknown locale processing need additional stateful APIs.
        if flags >> 16 != 1 || flags as u16 & !0x0001 != 0 {
            return None;
        }
        let modifiers_address = read_pointer(&raw, offset_of!(RawTables, modifiers))?;
        let modifier_header = reader.bytes(modifiers_address, size_of::<usize>() + 2)?;
        let modifier_bits_address = read_pointer(&modifier_header, 0)?;
        let max_bits = usize::from(read_u16(&modifier_header, size_of::<usize>())?);
        if max_bits > 255 {
            return None;
        }
        // MODIFIERS.ModNumber is a flexible array immediately after the WORD,
        // not after the padded sizeof(MODIFIERS).
        let modification_numbers = reader.bytes(
            modifiers_address.checked_add(size_of::<usize>() + 2)?,
            max_bits + 1,
        )?;
        if modification_numbers
            .iter()
            .any(|&n| n > 9 && n != INVALID_SHIFT)
        {
            return None;
        }
        let mut modifiers = Vec::new();
        let mut modifiers_terminated = false;
        for index in 0..32 {
            let bits = reader.entry(modifier_bits_address, index, 2)?;
            let vk = bits[0];
            if vk == 0 {
                modifiers_terminated = true;
                break;
            }
            if bits[1] == 0
                || bits[1] & !(SHIFT | CONTROL | ALT) != 0
                || modifiers.iter().any(|&(previous, _)| previous == vk)
            {
                return None;
            }
            modifiers.push((vk, bits[1]));
        }
        if !modifiers_terminated || modifiers.is_empty() {
            return None;
        }

        let ligatures = parse_ligatures(&raw, &mut reader)?;
        let rows = parse_character_tables(
            read_pointer(&raw, offset_of!(RawTables, characters))?,
            &ligatures,
            &mut reader,
        )?;
        let dead_keys = parse_dead_keys(
            read_pointer(&raw, offset_of!(RawTables, dead_keys))?,
            &mut reader,
        )?;
        Some(Self {
            modifiers,
            modification_numbers,
            rows,
            dead_keys,
        })
    }

    /// Decode one normalized key-down event. `scan` is retained in this API to
    /// match physical capture; ordinary character tables are indexed by VK.
    /// Alt+numpad entry and special NLS modes are deliberately unsupported.
    pub(crate) fn translate(
        &self,
        vk: u32,
        _scan: u32,
        state: &[u8; 256],
        composition: &mut Composition,
    ) -> Option<Translation> {
        let vk = usize::try_from(vk).ok()?;
        let row = self.rows.get(vk)?;
        let Some(row) = row else {
            return Some(Translation::NoText);
        };
        let bits = self.modifiers.iter().fold(0u8, |bits, &(key, mask)| {
            if state[usize::from(key)] & 0x80 != 0 {
                bits | mask
            } else {
                bits
            }
        });
        if bits & (CONTROL | ALT) == ALT {
            return None;
        }
        let mut bits = bits;
        if state[0x14] & 1 != 0 {
            let affected = (bits & !SHIFT == 0 && row.attributes & CAPLOK != 0)
                || (bits & !SHIFT == (CONTROL | ALT) && row.attributes & CAPLOKALTGR != 0);
            if affected {
                bits ^= SHIFT;
            }
        }
        let Some(&column) = self.modification_numbers.get(usize::from(bits)) else {
            return Some(Translation::NoText);
        };
        if column == INVALID_SHIFT {
            return Some(Translation::NoText);
        }
        let Some(output) = row.columns.get(usize::from(column)) else {
            return Some(Translation::NoText);
        };
        self.compose(output, composition)
    }

    fn compose(&self, output: &KeyOutput, composition: &mut Composition) -> Option<Translation> {
        let (units, incoming_dead) = match output {
            KeyOutput::None => return Some(Translation::NoText),
            KeyOutput::Dead(accent) => (vec![*accent], true),
            KeyOutput::Text(units) => (units.clone(), false),
        };
        let first = *units.first()?;
        let Some(accent) = composition.accent else {
            if incoming_dead {
                composition.accent = Some(first);
                return Some(Translation::PendingDeadKey);
            }
            return Some(Translation::Text(String::from_utf16(&units).ok()?));
        };

        // A dead key plus a ligature is not specified by the basic WDK table
        // contract. Refuse that sequence instead of inventing normalization.
        if units.len() != 1 {
            return None;
        }
        if let Some(transition) = self.dead_keys.get(&(first, accent)) {
            if transition.remains_dead {
                composition.accent = Some(transition.composed);
                return Some(Translation::PendingDeadKey);
            }
            let text = String::from_utf16(&[transition.composed]).ok()?;
            composition.accent = None;
            return Some(Translation::Text(text));
        }
        let mut text = composition.spacing_accent()?;
        text.push_str(&String::from_utf16(&[first]).ok()?);
        composition.accent = None;
        Some(Translation::Text(text))
    }
}

struct Reader<F> {
    read: F,
    remaining: usize,
}

impl<F: FnMut(usize, usize) -> Option<Vec<u8>>> Reader<F> {
    fn bytes(&mut self, address: usize, length: usize) -> Option<Vec<u8>> {
        if address == 0 || length == 0 {
            return None;
        }
        address.checked_add(length)?;
        self.remaining = self.remaining.checked_sub(length)?;
        let bytes = (self.read)(address, length)?;
        (bytes.len() == length).then_some(bytes)
    }

    fn entry(&mut self, base: usize, index: usize, stride: usize) -> Option<Vec<u8>> {
        self.bytes(base.checked_add(index.checked_mul(stride)?)?, stride)
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_pointer(bytes: &[u8], offset: usize) -> Option<usize> {
    Some(usize::from_le_bytes(
        bytes
            .get(offset..offset.checked_add(size_of::<usize>())?)?
            .try_into()
            .ok()?,
    ))
}

type Ligatures = HashMap<(u8, u16), Vec<u16>>;

fn parse_ligatures<F: FnMut(usize, usize) -> Option<Vec<u8>>>(
    raw: &[u8],
    reader: &mut Reader<F>,
) -> Option<Ligatures> {
    let length = usize::from(*raw.get(offset_of!(RawTables, ligature_length))?);
    let stride = usize::from(*raw.get(offset_of!(RawTables, ligature_stride))?);
    let address = read_pointer(raw, offset_of!(RawTables, ligatures))?;
    let mut entries = HashMap::new();
    if length == 0 && stride == 0 && address == 0 {
        return Some(entries);
    }
    if !(1..=16).contains(&length) || stride != 4 + 2 * length || address == 0 {
        return None;
    }
    for index in 0..MAX_LIGATURES {
        let row = reader.entry(address, index, stride)?;
        if row[0] == 0 {
            return Some(entries);
        }
        let modification = read_u16(&row, 2)?;
        if modification > 9 {
            return None;
        }
        let mut text = Vec::with_capacity(length);
        let mut ended = false;
        for column in 0..length {
            let unit = read_u16(&row, 4 + column * 2)?;
            if unit == WCH_NONE {
                ended = true;
            } else {
                if ended || matches!(unit, WCH_DEAD | WCH_LGTR) {
                    return None;
                }
                text.push(unit);
            }
        }
        if text.is_empty()
            || String::from_utf16(&text).is_err()
            || entries.insert((row[0], modification), text).is_some()
        {
            return None;
        }
    }
    None
}

fn parse_character_tables<F: FnMut(usize, usize) -> Option<Vec<u8>>>(
    address: usize,
    ligatures: &Ligatures,
    reader: &mut Reader<F>,
) -> Option<Vec<Option<KeyRow>>> {
    let mut rows: Vec<Option<KeyRow>> = vec![None; 256];
    for table_index in 0..MAX_TABLES {
        let table = reader.entry(address, table_index, size_of::<RawCharacterTable>())?;
        let rows_address = read_pointer(&table, offset_of!(RawCharacterTable, rows))?;
        if rows_address == 0 {
            return rows.iter().any(Option::is_some).then_some(rows);
        }
        let columns = usize::from(table[offset_of!(RawCharacterTable, columns)]);
        let stride = usize::from(table[offset_of!(RawCharacterTable, stride)]);
        if !(1..=10).contains(&columns) || stride != 2 + 2 * columns {
            return None;
        }
        let mut row_index = 0;
        let mut terminated = false;
        while row_index < MAX_ROWS {
            let row = reader.entry(rows_address, row_index, stride)?;
            row_index += 1;
            let vk = row[0];
            if vk == 0 {
                terminated = true;
                break;
            }
            // SGCAPS supplementary rows, KANALOK and group-tap processing need
            // their own verified interpretation; never silently ignore them.
            if vk == 0xff
                || row[1] & !(CAPLOK | CAPLOKALTGR) != 0
                || rows[usize::from(vk)].is_some()
            {
                return None;
            }
            let has_dead =
                (0..columns).any(|column| read_u16(&row, 2 + column * 2) == Some(WCH_DEAD));
            let dead_row = if has_dead {
                if row_index >= MAX_ROWS {
                    return None;
                }
                let dead = reader.entry(rows_address, row_index, stride)?;
                row_index += 1;
                if dead[0] != 0xff || dead[1] != 0 {
                    return None;
                }
                Some(dead)
            } else {
                None
            };
            let mut outputs = Vec::with_capacity(columns);
            for column in 0..columns {
                let value = read_u16(&row, 2 + column * 2)?;
                outputs.push(match value {
                    WCH_NONE => KeyOutput::None,
                    WCH_DEAD => {
                        let accent = read_u16(dead_row.as_ref()?, 2 + column * 2)?;
                        if !is_scalar(accent) {
                            return None;
                        }
                        KeyOutput::Dead(accent)
                    }
                    WCH_LGTR => KeyOutput::Text(ligatures.get(&(vk, column as u16))?.clone()),
                    value => {
                        if !is_scalar(value) {
                            return None;
                        }
                        KeyOutput::Text(vec![value])
                    }
                });
            }
            rows[usize::from(vk)] = Some(KeyRow {
                attributes: row[1],
                columns: outputs,
            });
        }
        if !terminated {
            return None;
        }
    }
    None
}

fn is_scalar(unit: u16) -> bool {
    !matches!(unit, WCH_NONE | WCH_DEAD | WCH_LGTR | 0xd800..=0xdfff)
}

fn parse_dead_keys<F: FnMut(usize, usize) -> Option<Vec<u8>>>(
    address: usize,
    reader: &mut Reader<F>,
) -> Option<HashMap<(u16, u16), DeadTransition>> {
    let mut transitions = HashMap::new();
    if address == 0 {
        return Some(transitions);
    }
    for index in 0..MAX_DEAD_KEYS {
        let row = reader.entry(address, index, 8)?;
        let both = read_u32(&row, 0)?;
        if both == 0 {
            return Some(transitions);
        }
        let base = both as u16;
        let accent = (both >> 16) as u16;
        let composed = read_u16(&row, 4)?;
        let flags = read_u16(&row, 6)?;
        if flags & !1 != 0 || !is_scalar(base) || !is_scalar(accent) || !is_scalar(composed) {
            return None;
        }
        if transitions
            .insert(
                (base, accent),
                DeadTransition {
                    composed,
                    remains_dead: flags == 1,
                },
            )
            .is_some()
        {
            return None;
        }
    }
    None
}

#[cfg(all(test, windows))]
#[path = "layout_tables_native_tests.rs"]
mod native_tests;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    const BASE: usize = 0x1000;

    struct Fixture {
        image: Vec<u8>,
        descriptor: usize,
        character_table: usize,
        rows: usize,
    }

    impl Fixture {
        fn new() -> Self {
            let mut image = Vec::new();
            let modifier_bits = append(&mut image, &[0x10, SHIFT, 0x11, CONTROL, 0x12, ALT, 0, 0]);
            let mut modifiers = vec![0; size_of::<usize>() + 2];
            put_pointer(&mut modifiers, 0, modifier_bits);
            put_u16(&mut modifiers, size_of::<usize>(), 7);
            modifiers.extend_from_slice(&[
                0,
                1,
                INVALID_SHIFT,
                INVALID_SHIFT,
                INVALID_SHIFT,
                INVALID_SHIFT,
                2,
                3,
            ]);
            let modifiers = append(&mut image, &modifiers);

            let mut characters = Vec::new();
            for (vk, attributes, units) in [
                (b'E', CAPLOK, [b'e' as u16, b'E' as u16, 0x20ac, WCH_NONE]),
                (
                    b'A',
                    CAPLOK | CAPLOKALTGR,
                    [b'a' as u16, b'A' as u16, 0x0105, 0x0104],
                ),
                (0xc0, 0, [WCH_DEAD, WCH_DEAD, WCH_NONE, WCH_NONE]),
                (0xff, 0, [b'^' as u16, b'~' as u16, WCH_NONE, WCH_NONE]),
                (b'B', 0, [WCH_LGTR, WCH_NONE, WCH_NONE, WCH_NONE]),
                (0x20, 0, [b' ' as u16, b' ' as u16, WCH_NONE, WCH_NONE]),
                (0, 0, [0; 4]),
            ] {
                characters.extend_from_slice(&[vk, attributes]);
                for unit in units {
                    characters.extend_from_slice(&unit.to_le_bytes());
                }
            }
            let rows = append(&mut image, &characters);
            let mut table = vec![0; size_of::<RawCharacterTable>() * 2];
            put_pointer(&mut table, offset_of!(RawCharacterTable, rows), rows);
            table[offset_of!(RawCharacterTable, columns)] = 4;
            table[offset_of!(RawCharacterTable, stride)] = 10;
            let character_table = append(&mut image, &table);

            let mut dead = Vec::new();
            for (base, accent, composed, flags) in [
                // Deliberately differs from Unicode normalization: the driver
                // defines this composition, not a guessed character algorithm.
                (b'e' as u16, b'^' as u16, b'x' as u16, 0u16),
                (b'~' as u16, b'^' as u16, 0x00a8, 1),
                (b'e' as u16, 0x00a8, 0x00eb, 0),
                (b' ' as u16, b'^' as u16, b'^' as u16, 0),
                (0, 0, 0, 0),
            ] {
                dead.extend_from_slice(&(u32::from(base) | u32::from(accent) << 16).to_le_bytes());
                dead.extend_from_slice(&composed.to_le_bytes());
                dead.extend_from_slice(&flags.to_le_bytes());
            }
            let dead = append(&mut image, &dead);
            let ligatures = append(
                &mut image,
                &[
                    b'B', 0, 0, 0, 0x3d, 0xd8, 0x42, 0xde, 0, 0, 0, 0, 0, 0, 0, 0,
                ],
            );
            let mut raw = vec![0; size_of::<RawTables>()];
            put_pointer(&mut raw, offset_of!(RawTables, modifiers), modifiers);
            put_pointer(&mut raw, offset_of!(RawTables, characters), character_table);
            put_pointer(&mut raw, offset_of!(RawTables, dead_keys), dead);
            raw[offset_of!(RawTables, locale_flags)..offset_of!(RawTables, locale_flags) + 4]
                .copy_from_slice(&0x0001_0001u32.to_le_bytes());
            raw[offset_of!(RawTables, ligature_length)] = 2;
            raw[offset_of!(RawTables, ligature_stride)] = 8;
            put_pointer(&mut raw, offset_of!(RawTables, ligatures), ligatures);
            let descriptor = append(&mut image, &raw);
            Self {
                image,
                descriptor,
                character_table,
                rows,
            }
        }

        fn parse(&self) -> Option<KeyboardTables> {
            KeyboardTables::parse(self.descriptor, |address, length| {
                let offset = address.checked_sub(BASE)?;
                Some(
                    self.image
                        .get(offset..offset.checked_add(length)?)?
                        .to_vec(),
                )
            })
        }
    }

    fn append(image: &mut Vec<u8>, bytes: &[u8]) -> usize {
        let address = BASE + image.len();
        image.extend_from_slice(bytes);
        address
    }

    fn put_pointer(bytes: &mut [u8], offset: usize, value: usize) {
        bytes[offset..offset + size_of::<usize>()].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn state(shift: bool, caps: bool, altgr: bool) -> [u8; 256] {
        let mut state = [0; 256];
        state[0x10] = if shift { 0x80 } else { 0 };
        state[0x14] = u8::from(caps);
        if altgr {
            state[0x11] = 0x80;
            state[0x12] = 0x80;
        }
        state
    }

    #[test]
    fn native_table_abi_offsets_match_the_x64_wdk_header() {
        assert_eq!(size_of::<usize>(), 8);
        assert_eq!(offset_of!(RawTables, locale_flags), 80);
        assert_eq!(offset_of!(RawTables, ligatures), 88);
        assert_eq!(size_of::<RawTables>(), 96); // prefix before optional type/subtype
        assert_eq!(size_of::<RawCharacterTable>(), 16);
    }

    #[test]
    fn owned_tables_preserve_modifier_columns_and_utf16_ligatures() {
        let table = Fixture::new().parse().unwrap();
        for (shift, caps, altgr, expected) in [
            (false, false, false, "a"),
            (true, false, false, "A"),
            (false, true, false, "A"),
            (true, true, false, "a"),
            (false, false, true, "ą"),
            (true, false, true, "Ą"),
            (false, true, true, "Ą"),
            (true, true, true, "ą"),
        ] {
            assert_eq!(
                table.translate(
                    u32::from(b'A'),
                    0,
                    &state(shift, caps, altgr),
                    &mut Composition::default()
                ),
                Some(Translation::Text(expected.to_owned()))
            );
        }
        assert_eq!(
            table.translate(u32::from(b'B'), 0, &[0; 256], &mut Composition::default()),
            Some(Translation::Text("🙂".to_owned()))
        );
    }

    #[test]
    fn composition_uses_driver_transitions_and_independent_owned_state() {
        let table = Fixture::new().parse().unwrap();
        let mut first = Composition::default();
        let mut second = Composition::default();
        assert_eq!(
            table.translate(0xc0, 0, &[0; 256], &mut first),
            Some(Translation::PendingDeadKey)
        );
        assert_eq!(
            table.translate(u32::from(b'E'), 0, &[0; 256], &mut second),
            Some(Translation::Text("e".to_owned()))
        );
        assert_eq!(
            table.translate(u32::from(b'E'), 0, &[0; 256], &mut first),
            Some(Translation::Text("x".to_owned()))
        );
        assert!(!first.is_pending());
        assert_eq!(
            table.translate(0xc0, 0, &[0; 256], &mut first),
            Some(Translation::PendingDeadKey)
        );
        assert_eq!(
            table.translate(0xc0, 0, &state(true, false, false), &mut first),
            Some(Translation::PendingDeadKey)
        );
        assert_eq!(first.spacing_accent().as_deref(), Some("¨"));
        assert_eq!(
            table.translate(u32::from(b'E'), 0, &[0; 256], &mut first),
            Some(Translation::Text("ë".to_owned()))
        );
    }

    #[test]
    fn pending_accent_survives_non_character_keys_and_commits_on_space() {
        let table = Fixture::new().parse().unwrap();
        let mut composition = Composition::default();
        assert_eq!(
            table.translate(0xc0, 0, &[0; 256], &mut composition),
            Some(Translation::PendingDeadKey)
        );
        assert_eq!(
            table.translate(0x10, 0, &[0; 256], &mut composition),
            Some(Translation::NoText)
        );
        assert!(composition.is_pending());
        assert_eq!(
            table.translate(0x20, 0, &[0; 256], &mut composition),
            Some(Translation::Text("^".to_owned()))
        );
        assert!(!composition.is_pending());
    }

    #[test]
    fn parser_rejects_unknown_versions_attributes_and_truncated_tables() {
        let mut version = Fixture::new();
        version.image[version.descriptor - BASE + offset_of!(RawTables, locale_flags) + 2] = 2;
        assert!(version.parse().is_none());
        let mut attributes = Fixture::new();
        attributes.image[attributes.rows - BASE + 1] = 0x80;
        assert!(attributes.parse().is_none());
        let mut stride = Fixture::new();
        stride.image[stride.character_table - BASE + offset_of!(RawCharacterTable, stride)] = 2;
        assert!(stride.parse().is_none());
        let mut truncated = Fixture::new();
        truncated.image.pop();
        assert!(truncated.parse().is_none());
    }

    #[test]
    fn reader_rejects_address_overflow_and_bounded_budget_before_copying() {
        let mut reader = Reader {
            read: |_, _| panic!("invalid range must not reach the memory reader"),
            remaining: 4,
        };
        assert!(reader.bytes(usize::MAX - 1, 4).is_none());
        assert!(reader.bytes(BASE, 5).is_none());
        assert!(reader.entry(BASE, usize::MAX, 8).is_none());
    }
}
