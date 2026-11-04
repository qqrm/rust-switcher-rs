//! Independent Windows oracle for the installed keyboard-table interpreter.
//!
//! Stateful ToUnicodeEx calls are confined to this explicitly opted-in test
//! process on the disposable hosted desktop. Production decoding must never
//! call that API. Expected characters and composition transitions come from
//! Windows itself, not from a second copy of the parser's rules.

#![cfg(windows)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetKeyboardLayout, GetKeyboardLayoutList, GetKeyboardLayoutNameW, HKL,
    MAPVK_VSC_TO_VK_EX, MapVirtualKeyExW, ToUnicodeEx, VK_CAPITAL, VK_CONTROL, VK_LCONTROL,
    VK_LSHIFT, VK_MENU, VK_RMENU, VK_SHIFT, VK_SPACE,
};

use crate::layout_tables::{Composition, KeyboardTables, Translation};

const SHIFT: u8 = 1;
const CAPS: u8 = 2;
const ALT_GR: u8 = 4;
const EXPECTED_KLIDS: [&str; 7] = [
    "00000409", // English US
    "00020409", // English US International, a distinct physical layout
    "00000419", // Russian
    "00000422", // Ukrainian
    "00000407", // German
    "0000040C", // French
    "00000415", // Polish (Programmers)
];

#[derive(Clone, Copy, Debug)]
struct Stroke {
    scan: u32,
    vk: u32,
    modifiers: u8,
}

impl Stroke {
    fn new(hkl: HKL, scan: u32, modifiers: u8) -> Self {
        let vk = unsafe { MapVirtualKeyExW(scan, MAPVK_VSC_TO_VK_EX, Some(hkl)) };
        assert!(
            (1..=255).contains(&vk),
            "the hosted layout must map printable scan 0x{scan:02x}; got VK 0x{vk:x}"
        );
        Self {
            scan,
            vk,
            modifiers,
        }
    }

    fn state(self) -> [u8; 256] {
        let mut state = [0; 256];
        if self.modifiers & SHIFT != 0 {
            state[usize::from(VK_SHIFT.0)] = 0x80;
            state[usize::from(VK_LSHIFT.0)] = 0x80;
        }
        if self.modifiers & CAPS != 0 {
            state[usize::from(VK_CAPITAL.0)] = 1;
        }
        if self.modifiers & ALT_GR != 0 {
            for key in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
                state[usize::from(key.0)] = 0x80;
            }
        }
        state
    }
}

struct Fixture {
    klid: &'static str,
    hkl: HKL,
}

struct RestoreLayout(HKL);

impl Drop for RestoreLayout {
    fn drop(&mut self) {
        let _ = unsafe { ActivateKeyboardLayout(self.0, Default::default()) };
    }
}

fn fixtures() -> Vec<Fixture> {
    for (name, value) in [
        ("RUST_SWITCHER_HOSTED_E2E", "1"),
        ("GITHUB_ACTIONS", "true"),
        ("RUNNER_ENVIRONMENT", "github-hosted"),
        ("RUNNER_OS", "Windows"),
    ] {
        assert_eq!(
            std::env::var(name).as_deref(),
            Ok(value),
            "stateful keyboard oracles require the disposable hosted Windows fixture"
        );
    }

    let count = unsafe { GetKeyboardLayoutList(None) };
    assert_eq!(
        usize::try_from(count).ok(),
        Some(EXPECTED_KLIDS.len()),
        "initialize-layouts.ps1 -Mode All must install exactly seven test layouts"
    );
    let mut handles = vec![HKL::default(); EXPECTED_KLIDS.len()];
    let copied = unsafe { GetKeyboardLayoutList(Some(&mut handles)) };
    assert_eq!(
        copied, count,
        "the installed catalog changed during the test"
    );

    // Resolve names using Windows, independently of the production registry
    // resolver. This also verifies that US International was not substituted
    // with US English. Restore the test thread's layout even after a panic.
    let previous = unsafe { GetKeyboardLayout(0) };
    assert!(!previous.0.is_null());
    let _restore = RestoreLayout(previous);
    let mut by_name = BTreeMap::new();
    for hkl in handles {
        unsafe { ActivateKeyboardLayout(hkl, Default::default()) }
            .expect("every hosted fixture layout must activate on the oracle thread");
        let mut name = [0u16; 9];
        unsafe { GetKeyboardLayoutNameW(&mut name) }
            .expect("Windows must report the exact active KLID");
        assert_eq!(name[8], 0, "KLID must be eight hexadecimal digits");
        let name = String::from_utf16(&name[..8])
            .expect("Windows KLID must be valid UTF-16")
            .to_uppercase();
        assert!(
            by_name.insert(name, hkl).is_none(),
            "duplicate physical KLID"
        );
    }
    let fixtures = EXPECTED_KLIDS
        .iter()
        .map(|&klid| Fixture {
            klid,
            hkl: by_name
                .remove(klid)
                .unwrap_or_else(|| panic!("required physical layout {klid} is missing")),
        })
        .collect();
    assert!(
        by_name.is_empty(),
        "unexpected physical layouts in hosted fixture"
    );
    fixtures
}

fn printable_strokes(hkl: HKL) -> Vec<Stroke> {
    // The main ISO keyboard's printable positions, including leading
    // punctuation, the OEM102 key and space. Controls and the numpad's
    // NumLock-dependent VK synthesis are outside this character-table test.
    let scans: Vec<_> = (0x02..=0x0d)
        .chain(0x10..=0x1b)
        .chain(0x1e..=0x29)
        .chain(0x2b..=0x35)
        .chain([0x39, 0x56])
        .collect();
    assert_eq!(scans.len(), 49);
    scans
        .into_iter()
        .flat_map(|scan| (0..8).map(move |modifiers| Stroke::new(hkl, scan, modifiers)))
        .collect()
}

/// Owns only the oracle's pending composition in this guarded test process.
/// A separate thread or desktop is deliberately not treated as an isolation
/// guarantee: earlier native regressions disproved that assumption.
struct NativeOracle {
    hkl: HKL,
}

impl NativeOracle {
    fn new(hkl: HKL) -> Self {
        let mut oracle = Self { hkl };
        oracle.reset();
        oracle
    }

    fn clear(&mut self) -> bool {
        let mut output = [0u16; 64];
        for _ in 0..16 {
            let count = unsafe {
                ToUnicodeEx(
                    u32::from(VK_SPACE.0),
                    0x39,
                    &[0; 256],
                    &mut output,
                    0,
                    Some(self.hkl),
                )
            };
            if count >= 0 {
                return true;
            }
        }
        false
    }

    fn reset(&mut self) {
        assert!(
            self.clear(),
            "the oracle's prior composition must be cleared"
        );
    }

    fn translate(&mut self, stroke: Stroke) -> Translation {
        let mut output = [0u16; 64];
        let count = unsafe {
            ToUnicodeEx(
                stroke.vk,
                stroke.scan,
                &stroke.state(),
                &mut output,
                0,
                Some(self.hkl),
            )
        };
        match count.cmp(&0) {
            std::cmp::Ordering::Less => Translation::PendingDeadKey,
            std::cmp::Ordering::Equal => Translation::NoText,
            std::cmp::Ordering::Greater => {
                let count = usize::try_from(count).expect("a positive result must fit usize");
                let units = output
                    .get(..count)
                    .expect("oracle output must fit its buffer");
                Translation::Text(
                    String::from_utf16(units)
                        .expect("native printable output must be valid UTF-16"),
                )
            }
        }
    }
}

impl Drop for NativeOracle {
    fn drop(&mut self) {
        let _ = self.clear();
    }
}

fn compare_sequence(
    fixture: &Fixture,
    tables: &KeyboardTables,
    oracle: &mut NativeOracle,
    strokes: &[Stroke],
) {
    oracle.reset();
    let mut composition = Composition::default();
    for (index, stroke) in strokes.iter().copied().enumerate() {
        // Query Windows first: the pure interpreter must preserve the oracle's
        // pending dead key until the next oracle call consumes it.
        let expected = oracle.translate(stroke);
        let actual = tables.translate(stroke.vk, stroke.scan, &stroke.state(), &mut composition);
        assert_eq!(
            actual,
            Some(expected),
            "KLID {} step {index} of {strokes:?}",
            fixture.klid,
        );
    }
    assert!(
        !composition.is_pending(),
        "KLID {} retained a dead key after the neutral e/space probes: {strokes:?}",
        fixture.klid,
    );
}

#[test]
#[ignore = "requires the All-seven-layout fixture on a disposable hosted Windows desktop"]
fn native_public_layout_table_oracle_matches_printable_keys_and_modifiers() {
    let _guard = crate::input::ring_buffer::test_guard();
    let mut compared = 0;
    for fixture in fixtures() {
        let tables = KeyboardTables::load(fixture.hkl.0 as usize)
            .unwrap_or_else(|| panic!("cannot parse installed layout {}", fixture.klid));
        let mut oracle = NativeOracle::new(fixture.hkl);
        let neutral_e = Stroke::new(fixture.hkl, 0x12, 0);
        let neutral_space = Stroke::new(fixture.hkl, 0x39, 0);
        for stroke in printable_strokes(fixture.hkl) {
            compare_sequence(
                &fixture,
                &tables,
                &mut oracle,
                &[stroke, neutral_e, neutral_space],
            );
            compared += 1;
        }
    }
    assert_eq!(compared, EXPECTED_KLIDS.len() * 49 * 8);
}

#[test]
#[ignore = "requires the All-seven-layout fixture on a disposable hosted Windows desktop"]
fn native_public_layout_table_oracle_matches_dead_key_sequences() {
    let _guard = crate::input::ring_buffer::test_guard();
    let mut sequences = 0;
    for fixture in fixtures() {
        let tables = KeyboardTables::load(fixture.hkl.0 as usize)
            .unwrap_or_else(|| panic!("cannot parse installed layout {}", fixture.klid));
        let mut oracle = NativeOracle::new(fixture.hkl);
        let strokes = printable_strokes(fixture.hkl);
        let mut dead_strokes = Vec::new();
        for &stroke in &strokes {
            oracle.reset();
            if oracle.translate(stroke) == Translation::PendingDeadKey {
                dead_strokes.push(stroke);
            }
        }
        if ["00020409", "00000407", "0000040C"].contains(&fixture.klid) {
            assert!(
                !dead_strokes.is_empty(),
                "the {} fixture must exercise real dead-key composition",
                fixture.klid,
            );
        }
        let neutral_e = Stroke::new(fixture.hkl, 0x12, 0);
        let neutral_space = Stroke::new(fixture.hkl, 0x39, 0);
        for dead in dead_strokes {
            for &following in &strokes {
                // Includes every printable base, uncomposable punctuation,
                // repeated and different dead keys, space, and all eight
                // Shift/Caps/AltGr states. The final probes expose any pending
                // state that survives a positive dead+dead return value.
                compare_sequence(
                    &fixture,
                    &tables,
                    &mut oracle,
                    &[dead, following, neutral_e, neutral_space],
                );
                sequences += 1;
            }
        }
    }
    assert!(
        sequences > 0,
        "the native composition comparison cannot be vacuous"
    );
}
