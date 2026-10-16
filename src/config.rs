mod config_validator;
pub mod constants;

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT};

const CONFIG_FILE: &str = "config.json";
pub const CONVERSION_DELAY_MS: u32 = 100;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Hotkey {
    pub vk: u32,
    pub mods: u32,
}

pub const MODVK_LCTRL: u32 = 1 << 0;
pub const MODVK_RCTRL: u32 = 1 << 1;
pub const MODVK_LSHIFT: u32 = 1 << 2;
pub const MODVK_RSHIFT: u32 = 1 << 3;
pub const MODVK_LALT: u32 = 1 << 4;
pub const MODVK_RALT: u32 = 1 << 5;
pub const MODVK_LWIN: u32 = 1 << 6;
pub const MODVK_RWIN: u32 = 1 << 7;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotkeyChord {
    pub mods: u32,

    #[serde(default)]
    pub mods_vks: u32,

    pub vk: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotkeySequence {
    pub first: HotkeyChord,
    pub second: Option<HotkeyChord>,
    #[serde(default)]
    pub third: Option<HotkeyChord>,
    pub max_gap_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub delay_ms: u32,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default)]
    pub theme_dark: bool,

    pub hotkey_convert_last_word: Option<Hotkey>,
    #[serde(default)]
    pub hotkey_convert_last_sequence: Option<Hotkey>,
    pub hotkey_convert_selection: Option<Hotkey>,
    pub hotkey_switch_layout: Option<Hotkey>,
    pub hotkey_pause: Option<Hotkey>,

    #[serde(default)]
    pub hotkey_convert_last_word_sequence: Option<HotkeySequence>,
    #[serde(default = "default_hotkey_convert_last_sequence_sequence")]
    pub hotkey_convert_last_sequence_sequence: Option<HotkeySequence>,
    #[serde(default)]
    pub hotkey_pause_sequence: Option<HotkeySequence>,
    #[serde(default)]
    pub hotkey_convert_selection_sequence: Option<HotkeySequence>,
    #[serde(default)]
    pub hotkey_switch_layout_sequence: Option<HotkeySequence>,

    #[serde(default)]
    pub smarter_hotkeys_enabled: bool,
    #[serde(default = "default_autoconvert_feature_enabled")]
    pub autoconvert_feature_enabled: bool,
    #[serde(default = "default_smart_hotkey_convert_last_word_sequence")]
    pub smart_hotkey_convert_last_word_sequence: Option<HotkeySequence>,
    #[serde(default = "default_smart_hotkey_convert_last_sequence_sequence")]
    pub smart_hotkey_convert_last_sequence_sequence: Option<HotkeySequence>,
    #[serde(default = "default_smart_hotkey_convert_selection_sequence")]
    pub smart_hotkey_convert_selection_sequence: Option<HotkeySequence>,
}

fn modifier_only_chord(mods: u32, mods_vks: u32) -> HotkeyChord {
    HotkeyChord {
        mods,
        mods_vks,
        vk: None,
    }
}

fn double_modifier_sequence(mods: u32, mods_vks: u32) -> HotkeySequence {
    HotkeySequence {
        first: modifier_only_chord(mods, mods_vks),
        second: Some(modifier_only_chord(mods, mods_vks)),
        third: None,
        max_gap_ms: 1000,
    }
}

fn triple_modifier_sequence(mods: u32, mods_vks: u32) -> HotkeySequence {
    HotkeySequence {
        first: modifier_only_chord(mods, mods_vks),
        second: Some(modifier_only_chord(mods, mods_vks)),
        third: Some(modifier_only_chord(mods, mods_vks)),
        max_gap_ms: 1000,
    }
}

fn default_hotkey_convert_last_sequence_sequence() -> Option<HotkeySequence> {
    Some(double_modifier_sequence(MOD_ALT.0, MODVK_LALT))
}

const fn default_autoconvert_feature_enabled() -> bool {
    true
}

fn default_smart_hotkey_convert_last_word_sequence() -> Option<HotkeySequence> {
    Some(triple_modifier_sequence(MOD_SHIFT.0, MODVK_LSHIFT))
}

fn default_smart_hotkey_convert_last_sequence_sequence() -> Option<HotkeySequence> {
    Some(triple_modifier_sequence(MOD_ALT.0, MODVK_LALT))
}

fn default_smart_hotkey_convert_selection_sequence() -> Option<HotkeySequence> {
    Some(triple_modifier_sequence(MOD_SHIFT.0, MODVK_LSHIFT))
}

impl Default for Config {
    fn default() -> Self {
        Self {
            delay_ms: CONVERSION_DELAY_MS,
            start_minimized: false,
            theme_dark: false,

            hotkey_switch_layout: None,
            hotkey_pause: None,
            hotkey_convert_last_word: None,
            hotkey_convert_last_sequence: None,
            hotkey_convert_selection: None,

            hotkey_convert_last_word_sequence: Some(double_modifier_sequence(
                MOD_SHIFT.0,
                MODVK_LSHIFT,
            )),
            hotkey_convert_last_sequence_sequence: default_hotkey_convert_last_sequence_sequence(),

            hotkey_pause_sequence: Some(double_modifier_sequence(MOD_CONTROL.0, MODVK_RCTRL)),
            hotkey_convert_selection_sequence: Some(double_modifier_sequence(
                MOD_SHIFT.0,
                MODVK_LSHIFT,
            )),
            hotkey_switch_layout_sequence: Some(HotkeySequence {
                first: HotkeyChord {
                    mods: 0,
                    mods_vks: 0,
                    vk: Some(20),
                },
                second: None,
                third: None,
                max_gap_ms: 1000,
            }),

            smarter_hotkeys_enabled: false,
            autoconvert_feature_enabled: default_autoconvert_feature_enabled(),
            smart_hotkey_convert_last_word_sequence:
                default_smart_hotkey_convert_last_word_sequence(),
            smart_hotkey_convert_last_sequence_sequence:
                default_smart_hotkey_convert_last_sequence_sequence(),
            smart_hotkey_convert_selection_sequence:
                default_smart_hotkey_convert_selection_sequence(),
        }
    }
}

pub fn config_path() -> io::Result<PathBuf> {
    let appdata = std::env::var_os("APPDATA")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA is not set"))?;

    Ok(PathBuf::from(appdata)
        .join(crate::app_identity::APP_DIR)
        .join(CONFIG_FILE))
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)
}

fn decode_config(path: &Path) -> io::Result<Config> {
    let text = fs::read_to_string(path)?;
    toml::from_str(&text).map_err(io::Error::other)
}

/// Commit a complete file from the same directory. Never truncate the destination.
fn write_atomically(
    path: &Path,
    overwrite: bool,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    ensure_parent_dir(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config has no parent"))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".rust-switcher-config-")
        .tempfile_in(parent)?;
    write(staged.as_file_mut())?;
    staged.as_file().sync_all()?;
    let result = if overwrite {
        staged.persist(path)
    } else {
        staged.persist_noclobber(path)
    };
    result.map(|_| ()).map_err(|error| error.error)
}

fn encode_and_store(path: &Path, cfg: &Config, overwrite: bool) -> io::Result<()> {
    cfg.validate_hotkey_sequences()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    // confy 2.0 used toml 0.9 despite the historical config.json filename.
    let text = toml::to_string_pretty(cfg).map_err(io::Error::other)?;
    write_atomically(path, overwrite, |file| file.write_all(text.as_bytes()))
}

fn load_path(path: &Path) -> io::Result<Config> {
    match decode_config(path) {
        Ok(cfg) => Ok(cfg),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let cfg = Config::default();
            match encode_and_store(path, &cfg, false) {
                Ok(()) => Ok(cfg),
                // Another instance won first creation. Read its complete settings.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => decode_config(path),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

pub fn load() -> io::Result<Config> {
    load_path(&config_path()?)
}

pub fn save(cfg: &Config) -> io::Result<()> {
    encode_and_store(&config_path()?, cfg, true)
}

#[cfg(test)]
mod atomic_tests {
    use std::os::windows::fs::OpenOptionsExt;

    use super::*;

    #[test]
    fn failed_staged_write_preserves_previous_bytes_and_removes_temporary_file() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.json");
        fs::write(&path, b"previous complete settings")?;
        let result = write_atomically(&path, true, |file| {
            file.write_all(b"partial replacement")?;
            Err(io::Error::other("injected write failure"))
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path)?, b"previous complete settings");
        assert_eq!(fs::read_dir(dir.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn denied_atomic_replace_preserves_previous_file() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.json");
        fs::write(&path, b"previous complete settings")?;
        // Deny FILE_SHARE_DELETE while permitting reads and writes.
        let held = fs::OpenOptions::new().read(true).share_mode(3).open(&path)?;
        let result = write_atomically(&path, true, |file| file.write_all(b"replacement"));
        assert!(result.is_err());
        assert_eq!(fs::read(&path)?, b"previous complete settings");
        drop(held);
        assert_eq!(fs::read_dir(dir.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn default_creation_is_complete_and_does_not_replace_a_racing_writer() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nested/config.json");
        let initial = load_path(&path)?;
        assert_eq!(initial.delay_ms, CONVERSION_DELAY_MS);
        let chosen = Config {
            theme_dark: true,
            ..initial
        };
        encode_and_store(&path, &chosen, true)?;
        let before = fs::read(&path)?;
        let result = encode_and_store(&path, &Config::default(), false);
        assert_eq!(
            result.err().map(|error| error.kind()),
            Some(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(fs::read(&path)?, before);
        assert!(load_path(&path)?.theme_dark);
        Ok(())
    }

    #[test]
    fn malformed_existing_config_is_not_replaced_with_defaults() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.json");
        fs::write(&path, b"invalid = [")?;
        assert!(load_path(&path).is_err());
        assert_eq!(fs::read(&path)?, b"invalid = [");
        Ok(())
    }

    #[test]
    fn legacy_toml_and_successful_atomic_update_remain_compatible() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.json");
        fs::write(&path, "delay_ms = 100\n")?;
        let mut cfg = load_path(&path)?;
        assert!(cfg.autoconvert_feature_enabled);
        cfg.theme_dark = true;
        encode_and_store(&path, &cfg, true)?;
        assert!(load_path(&path)?.theme_dark);
        assert!(fs::read_to_string(&path)?.contains("theme_dark = true"));
        assert_eq!(fs::read_dir(dir.path())?.count(), 1);
        Ok(())
    }
}
