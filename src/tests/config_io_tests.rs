use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT};

use super::env_lock::lock_env;
use crate::{
    app::{HotkeySequenceValues, HotkeySlot},
    config::{
        self, Config, HotkeyChord, HotkeySequence, MODVK_LALT, MODVK_LSHIFT, MODVK_RCTRL,
    },
};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("rust-switcher-tests-{prefix}-{ts}"))
}

fn seq_ctrl_a() -> HotkeySequence {
    HotkeySequence {
        first: HotkeyChord {
            mods: MOD_CONTROL.0,
            mods_vks: 0,
            vk: Some(u32::from(b'A')),
        },
        second: None,
        third: None,
        max_gap_ms: 1000,
    }
}

struct AppDataOverride {
    _guard: std::sync::MutexGuard<'static, ()>,
    old: Option<std::ffi::OsString>,
    dir: PathBuf,
}

impl AppDataOverride {
    fn new(prefix: &str) -> Self {
        let guard = lock_env();

        let old = std::env::var_os("APPDATA");
        let dir = unique_temp_dir(prefix);
        fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("APPDATA", &dir) };

        Self {
            _guard: guard,
            old,
            dir,
        }
    }
}

impl Drop for AppDataOverride {
    fn drop(&mut self) {
        match self.old.take() {
            Some(v) => unsafe { std::env::set_var("APPDATA", v) },
            None => unsafe { std::env::remove_var("APPDATA") },
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn config_save_and_load_roundtrip_via_appdata() {
    let _env = AppDataOverride::new("appdata");

    let cfg = Config {
        hotkey_pause_sequence: Some(seq_ctrl_a()),
        ..Default::default()
    };

    config::save(&cfg).unwrap();
    let loaded = config::load().unwrap();

    assert_eq!(loaded.hotkey_pause_sequence, cfg.hotkey_pause_sequence);
}

#[test]
fn config_loads_missing_autoconvert_feature_flag_as_enabled() {
    let _env = AppDataOverride::new("appdata-legacy-autoconvert");
    let path = config::config_path().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "delay_ms = 100\n").unwrap();

    let loaded = config::load().unwrap();

    assert!(loaded.autoconvert_feature_enabled);
}

#[test]
fn browser_acceptance_config_enables_every_exercised_runtime_shortcut() {
    let _env = AppDataOverride::new("browser-acceptance-config");
    let path = config::config_path().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, include_str!("../../scripts/e2e/browser-config.toml")).unwrap();

    let loaded = config::load().unwrap();
    loaded.validate_hotkey_sequences().unwrap();
    assert!(loaded.smarter_hotkeys_enabled);
    assert!(loaded.autoconvert_feature_enabled);
    let runtime = HotkeySequenceValues::from_config(&loaded);
    let repeated = |mods, mods_vks, count| {
        let chord = HotkeyChord { mods, mods_vks, vk: None };
        HotkeySequence {
            first: chord,
            second: Some(chord),
            third: (count == 3).then_some(chord),
            max_gap_ms: 1000,
        }
    };
    for (slot, sequence) in [
        (HotkeySlot::LastWord, repeated(MOD_SHIFT.0, MODVK_LSHIFT, 2)),
        (HotkeySlot::LastSequence, repeated(MOD_ALT.0, MODVK_LALT, 2)),
        (HotkeySlot::Selection, repeated(MOD_SHIFT.0, MODVK_LSHIFT, 2)),
        (HotkeySlot::SmartLastWord, repeated(MOD_SHIFT.0, MODVK_LSHIFT, 3)),
        (HotkeySlot::SmartLastSequence, repeated(MOD_ALT.0, MODVK_LALT, 3)),
        (HotkeySlot::SmartSelection, repeated(MOD_SHIFT.0, MODVK_LSHIFT, 3)),
        (HotkeySlot::Pause, repeated(MOD_CONTROL.0, MODVK_RCTRL, 2)),
        (HotkeySlot::SwitchLayout, HotkeySequence {
            first: HotkeyChord { mods: 0, mods_vks: 0, vk: Some(20) },
            second: None,
            third: None,
            max_gap_ms: 1000,
        }),
    ] {
        assert_eq!(runtime.get(slot), Some(sequence), "browser binding: {slot:?}");
    }
}

#[test]
fn config_save_rejects_invalid_sequences() {
    let _env = AppDataOverride::new("appdata-invalid");

    let cfg = Config {
        hotkey_convert_last_word_sequence: Some(seq_ctrl_a()),
        hotkey_pause_sequence: Some(seq_ctrl_a()),
        ..Default::default()
    };

    let err = config::save(&cfg).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("unique hotkey sequence"));
}

#[test]
fn default_hotkey_sequences_keep_last_word_on_double_left_shift() {
    let cfg = Config::default();
    let seq = cfg
        .hotkey_convert_last_word_sequence
        .expect("default last-word hotkey");

    assert_eq!(seq.first.mods, MOD_SHIFT.0);
    assert_eq!(seq.first.mods_vks, MODVK_LSHIFT);
    assert_eq!(seq.first.vk, None);
    assert_eq!(seq.second, Some(seq.first));
    assert_eq!(seq.third, None);
}

#[test]
fn default_hotkey_sequences_use_double_left_alt_for_last_sequence() {
    let cfg = Config::default();
    let seq = cfg
        .hotkey_convert_last_sequence_sequence
        .expect("default last-sequence hotkey");

    assert_eq!(seq.first.mods, MOD_ALT.0);
    assert_eq!(seq.first.mods_vks, MODVK_LALT);
    assert_eq!(seq.first.vk, None);
    assert_eq!(seq.second, Some(seq.first));
    assert_eq!(seq.third, None);
}

#[test]
fn default_hotkey_sequences_keep_switch_layout_on_capslock() {
    let cfg = Config::default();
    let seq = cfg
        .hotkey_switch_layout_sequence
        .expect("default switch-layout hotkey");

    assert_eq!(seq.first.mods, 0);
    assert_eq!(seq.first.mods_vks, 0);
    assert_eq!(seq.first.vk, Some(20));
    assert_eq!(seq.second, None);
    assert_eq!(seq.third, None);
}

#[test]
fn default_hotkey_sequences_use_double_right_ctrl_for_pause() {
    let cfg = Config::default();
    assert!(cfg.autoconvert_feature_enabled);
    let seq = cfg.hotkey_pause_sequence.expect("default pause hotkey");

    assert_eq!(seq.first.mods, MOD_CONTROL.0);
    assert_eq!(seq.first.mods_vks, MODVK_RCTRL);
    assert_eq!(seq.first.vk, None);
    assert_eq!(seq.second, Some(seq.first));
    assert_eq!(seq.third, None);
}

#[test]
fn default_hotkey_sequences_keep_convert_selection_on_double_left_shift() {
    let cfg = Config::default();
    let seq = cfg
        .hotkey_convert_selection_sequence
        .expect("default selection hotkey");

    assert_eq!(seq.first.mods, MOD_SHIFT.0);
    assert_eq!(seq.first.mods_vks, MODVK_LSHIFT);
    assert_eq!(seq.first.vk, None);
    assert_eq!(seq.second, Some(seq.first));
    assert_eq!(seq.third, None);
}

#[test]
fn default_smart_hotkeys_are_disabled_but_keep_triple_tap_defaults() {
    let cfg = Config::default();
    assert!(!cfg.smarter_hotkeys_enabled);

    let word = cfg
        .smart_hotkey_convert_last_word_sequence
        .expect("default smart last-word hotkey");
    assert_eq!(word.first.mods, MOD_SHIFT.0);
    assert_eq!(word.first.mods_vks, MODVK_LSHIFT);
    assert_eq!(word.second, Some(word.first));
    assert_eq!(word.third, Some(word.first));

    let sequence = cfg
        .smart_hotkey_convert_last_sequence_sequence
        .expect("default smart last-sequence hotkey");
    assert_eq!(sequence.first.mods, MOD_ALT.0);
    assert_eq!(sequence.first.mods_vks, MODVK_LALT);
    assert_eq!(sequence.second, Some(sequence.first));
    assert_eq!(sequence.third, Some(sequence.first));

    let selection = cfg
        .smart_hotkey_convert_selection_sequence
        .expect("default smart selection hotkey");
    assert_eq!(selection.first.mods, MOD_SHIFT.0);
    assert_eq!(selection.first.mods_vks, MODVK_LSHIFT);
    assert_eq!(selection.second, Some(selection.first));
    assert_eq!(selection.third, Some(selection.first));
}
