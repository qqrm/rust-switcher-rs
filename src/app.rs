//! Defines the application state and associated constants.
//!
//! The `AppState` structure holds handles to all of the controls in
//! the settings window along with the font used for drawing text.
//! Constants representing control identifiers are defined here so
//! that they can be shared between modules.

use std::collections::VecDeque;

use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{HBRUSH, HFONT},
    UI::WindowsAndMessaging::HMENU,
};

use crate::{config, input::hotkeys::HotkeyAction};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RuntimeCommand {
    Hotkey(HotkeyAction),
    AutoconvertLastWord,
}

#[derive(Debug, Clone)]
pub struct UiError {
    pub title: String,
    pub user_text: String,
    pub _debug_text: String,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum HotkeySlot {
    LastWord,
    LastSequence,
    Pause,
    Selection,
    SwitchLayout,
    SmartLastWord,
    SmartLastSequence,
    SmartSelection,
}

#[derive(Debug, Default, Clone)]
pub struct HotkeyValues {
    pub last_word: Option<config::Hotkey>,
    pub last_sequence: Option<config::Hotkey>,
    pub pause: Option<config::Hotkey>,
    pub selection: Option<config::Hotkey>,
    pub switch_layout: Option<config::Hotkey>,
}

impl HotkeyValues {
    pub fn from_config(cfg: &config::Config) -> Self {
        Self {
            last_word: cfg.hotkey_convert_last_word,
            last_sequence: cfg.hotkey_convert_last_sequence,
            pause: cfg.hotkey_pause,
            selection: cfg.hotkey_convert_selection,
            switch_layout: cfg.hotkey_switch_layout,
        }
    }

    #[allow(dead_code)]
    pub fn get(&self, slot: HotkeySlot) -> Option<config::Hotkey> {
        match slot {
            HotkeySlot::LastWord => self.last_word,
            HotkeySlot::LastSequence => self.last_sequence,
            HotkeySlot::Pause => self.pause,
            HotkeySlot::Selection => self.selection,
            HotkeySlot::SwitchLayout => self.switch_layout,
            HotkeySlot::SmartLastWord
            | HotkeySlot::SmartLastSequence
            | HotkeySlot::SmartSelection => None,
        }
    }

    pub fn set(&mut self, slot: HotkeySlot, hk: Option<config::Hotkey>) {
        match slot {
            HotkeySlot::LastWord => self.last_word = hk,
            HotkeySlot::LastSequence => self.last_sequence = hk,
            HotkeySlot::Pause => self.pause = hk,
            HotkeySlot::Selection => self.selection = hk,
            HotkeySlot::SwitchLayout => self.switch_layout = hk,
            HotkeySlot::SmartLastWord
            | HotkeySlot::SmartLastSequence
            | HotkeySlot::SmartSelection => {}
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct HotkeySequenceValues {
    pub last_word: Option<config::HotkeySequence>,
    pub last_sequence: Option<config::HotkeySequence>,
    pub pause: Option<config::HotkeySequence>,
    pub selection: Option<config::HotkeySequence>,
    pub switch_layout: Option<config::HotkeySequence>,
    pub smart_last_word: Option<config::HotkeySequence>,
    pub smart_last_sequence: Option<config::HotkeySequence>,
    pub smart_selection: Option<config::HotkeySequence>,
}

impl HotkeySequenceValues {
    pub fn from_config(cfg: &config::Config) -> Self {
        let mut values = Self::from_config_all(cfg);
        if !cfg.autoconvert_feature_enabled {
            values.pause = None;
        }
        if !cfg.smarter_hotkeys_enabled {
            values.smart_last_word = None;
            values.smart_last_sequence = None;
            values.smart_selection = None;
        }
        values
    }

    pub fn from_config_all(cfg: &config::Config) -> Self {
        Self {
            last_word: cfg.hotkey_convert_last_word_sequence,
            last_sequence: cfg.hotkey_convert_last_sequence_sequence,
            pause: cfg.hotkey_pause_sequence,
            selection: cfg.hotkey_convert_selection_sequence,
            switch_layout: cfg.hotkey_switch_layout_sequence,
            smart_last_word: cfg.smart_hotkey_convert_last_word_sequence,
            smart_last_sequence: cfg.smart_hotkey_convert_last_sequence_sequence,
            smart_selection: cfg.smart_hotkey_convert_selection_sequence,
        }
    }

    pub fn get(&self, slot: HotkeySlot) -> Option<config::HotkeySequence> {
        match slot {
            HotkeySlot::LastWord => self.last_word,
            HotkeySlot::LastSequence => self.last_sequence,
            HotkeySlot::Pause => self.pause,
            HotkeySlot::Selection => self.selection,
            HotkeySlot::SwitchLayout => self.switch_layout,
            HotkeySlot::SmartLastWord => self.smart_last_word,
            HotkeySlot::SmartLastSequence => self.smart_last_sequence,
            HotkeySlot::SmartSelection => self.smart_selection,
        }
    }

    pub fn set(&mut self, slot: HotkeySlot, seq: Option<config::HotkeySequence>) {
        match slot {
            HotkeySlot::LastWord => self.last_word = seq,
            HotkeySlot::LastSequence => self.last_sequence = seq,
            HotkeySlot::Pause => self.pause = seq,
            HotkeySlot::Selection => self.selection = seq,
            HotkeySlot::SwitchLayout => self.switch_layout = seq,
            HotkeySlot::SmartLastWord => self.smart_last_word = seq,
            HotkeySlot::SmartLastSequence => self.smart_last_sequence = seq,
            HotkeySlot::SmartSelection => self.smart_selection = seq,
        }
    }
}

#[derive(Debug, Default)]
pub struct HotkeyCaptureUi {
    pub active: bool,
    pub slot: Option<HotkeySlot>,

    pub pending_mods_vks: u32,
    pub pending_mods: u32,
    pub pending_mods_valid: bool,
    pub saw_non_mod: bool,

    // Last successful chord capture time in milliseconds since boot (GetTickCount64).
    // Used to reset sequence after a long pause.
    pub last_input_tick_ms: u64,
}

impl HotkeyCaptureUi {
    pub fn start(&mut self, slot: HotkeySlot) {
        self.active = true;
        self.slot = Some(slot);
        self.pending_mods_vks = 0;
        self.pending_mods = 0;
        self.pending_mods_valid = false;
        self.saw_non_mod = false;
        self.last_input_tick_ms = 0;
    }

    pub fn stop(&mut self) {
        self.active = false;
        self.slot = None;
        self.pending_mods_vks = 0;
        self.pending_mods = 0;
        self.pending_mods_valid = false;
        self.saw_non_mod = false;
        self.last_input_tick_ms = 0;
    }
}

#[derive(Debug, Default)]
pub struct RuntimeChordCapture {
    pub pending_mods_vks: u32,
    pub pending_mods: u32,
    pub pending_mods_valid: bool,
    pub saw_non_mod: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SequenceProgress {
    pub waiting_second: bool,
    pub first_tick_ms: u64,
    pub matched_chords: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct DeferredSequenceHotkey {
    pub slot: HotkeySlot,
}

#[derive(Debug, Default)]
pub struct HotkeySequenceProgress {
    pub last_word: SequenceProgress,
    pub last_sequence: SequenceProgress,
    pub pause: SequenceProgress,
    pub selection: SequenceProgress,
    pub switch_layout: SequenceProgress,
    pub smart_last_word: SequenceProgress,
    pub smart_last_sequence: SequenceProgress,
    pub smart_selection: SequenceProgress,
}

/// Per-window state used throughout the application.
///
/// Stored in window user data. Contains handles of child controls and UI resources.
#[derive(Debug, Default)]
pub struct AppState {
    /// Font used for all controls. Assigned after window creation.
    pub font: HFONT,

    pub checkboxes: Checkboxes,
    pub edits: Edits,
    pub hotkeys: HotkeyEdits,
    pub buttons: Buttons,

    pub autoconvert_enabled: bool,
    /// Whether the AutoConvert feature and its pause hotkey are available.
    pub autoconvert_feature_enabled: bool,
    pub errors: VecDeque<UiError>,

    /// Temporary hotkeys currently shown in UI. Committed on Apply.
    pub hotkey_values: HotkeyValues,
    pub hotkey_sequence_values: HotkeySequenceValues,

    /// Which hotkey edit is currently capturing input.
    pub hotkey_capture: HotkeyCaptureUi,

    /// Active (already applied) sequences used by the runtime hotkey recognizer.
    /// This must NOT be tied to temporary edits in the UI.
    pub active_hotkey_sequences: HotkeySequenceValues,

    /// The already applied pause hotkeys. These are kept separately from the
    /// editable values so toggling AutoConvert in the UI never activates an
    /// un-applied hotkey change.
    pub active_pause_hotkey: Option<config::Hotkey>,
    pub active_pause_hotkey_sequence: Option<config::HotkeySequence>,

    /// Runtime state for modifier-only chord detection.
    pub runtime_chord_capture: RuntimeChordCapture,

    /// Runtime state for chord sequence progress.
    pub hotkey_sequence_progress: HotkeySequenceProgress,
    pub deferred_sequence_hotkey: Option<DeferredSequenceHotkey>,

    /// Serialized runtime commands triggered by hotkeys/autoconvert.
    pub pending_runtime_commands: VecDeque<RuntimeCommand>,
    pub runtime_command_running: bool,

    pub active_switch_layout_sequence: Option<config::HotkeySequence>,
    pub switch_layout_waiting_second: bool,
    pub switch_layout_first_tick_ms: u64,

    pub current_theme_dark: bool,
    // Cached theme brushes (must be deleted on window destroy)
    pub dark_brush_window_bg: HBRUSH,
    pub dark_brush_control_bg: HBRUSH,
    pub dark_brush_edit_bg: HBRUSH,
}

impl AppState {
    /// Applies the AutoConvert availability gate immediately, without saving
    /// the Settings form. `Cancel` restores the persisted configuration.
    ///
    /// Returns whether a deferred pause sequence was cancelled and its timer
    /// therefore needs to be stopped by the window layer.
    pub fn set_autoconvert_feature_enabled(&mut self, enabled: bool) -> bool {
        let cancelled_deferred_pause = self
            .deferred_sequence_hotkey
            .is_some_and(|deferred| deferred.slot == HotkeySlot::Pause);

        self.autoconvert_feature_enabled = enabled;
        self.autoconvert_enabled = false;
        self.active_hotkey_sequences.pause = if enabled {
            self.active_pause_hotkey_sequence
        } else {
            None
        };
        self.hotkey_sequence_progress.pause = SequenceProgress::default();

        if cancelled_deferred_pause {
            self.deferred_sequence_hotkey = None;
        }

        self.pending_runtime_commands.retain(|command| {
            !matches!(
                command,
                RuntimeCommand::AutoconvertLastWord
                    | RuntimeCommand::Hotkey(HotkeyAction::PauseToggle)
            )
        });

        cancelled_deferred_pause
    }
}

#[derive(Debug, Default)]
pub struct Checkboxes {
    pub autostart: HWND,
    pub start_minimized: HWND,
    pub theme_dark: HWND,
    pub smarter_hotkeys: HWND,
    pub autoconvert_feature: HWND,
}

#[derive(Debug, Default)]
pub struct Edits {
    pub playground_label: HWND,
    pub playground: HWND,
}

#[derive(Debug, Default)]
pub struct HotkeyEdits {
    pub last_word: HWND,
    pub last_sequence: HWND,
    pub pause: HWND,
    pub selection: HWND,
    pub switch_layout: HWND,
    pub smart_last_word: HWND,
    pub smart_last_sequence: HWND,
    pub smart_selection: HWND,
}

#[derive(Debug, Default)]
pub struct Buttons {
    pub apply: HWND,
    pub cancel: HWND,
    pub exit: HWND,
}

/// Control identifiers used in `WM_COMMAND` and as `HMENU` in `CreateWindowExW`.
#[repr(i32)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ControlId {
    Autostart = 1001,
    Tray = 1002,
    StartMinimized = 1004,
    DarkTheme = 1005,
    SmarterHotkeys = 1006,
    AutoconvertFeature = 1007,

    HotkeyLastWord = 1201,
    HotkeyLastSequence = 1202,
    HotkeyPause = 1203,
    HotkeySelection = 1204,
    HotkeySwitchLayout = 1205,
    HotkeySmartLastWord = 1206,
    HotkeySmartLastSequence = 1207,
    HotkeySmartSelection = 1208,

    Apply = 1101,
    Cancel = 1102,
    Exit = 1103,
}

impl ControlId {
    #[inline]
    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            1001 => Some(Self::Autostart),
            1002 => Some(Self::Tray),
            1004 => Some(Self::StartMinimized),
            1005 => Some(Self::DarkTheme),
            1006 => Some(Self::SmarterHotkeys),
            1007 => Some(Self::AutoconvertFeature),

            1201 => Some(Self::HotkeyLastWord),
            1202 => Some(Self::HotkeyLastSequence),
            1203 => Some(Self::HotkeyPause),
            1204 => Some(Self::HotkeySelection),
            1205 => Some(Self::HotkeySwitchLayout),
            1206 => Some(Self::HotkeySmartLastWord),
            1207 => Some(Self::HotkeySmartLastSequence),
            1208 => Some(Self::HotkeySmartSelection),

            1101 => Some(Self::Apply),
            1102 => Some(Self::Cancel),
            1103 => Some(Self::Exit),

            _ => None,
        }
    }

    #[inline]
    pub fn hmenu(self) -> windows::Win32::UI::WindowsAndMessaging::HMENU {
        use std::ffi::c_void;

        let id = self as u16;
        HMENU(usize::from(id) as *mut c_void)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, input::hotkeys::HotkeyAction};

    #[test]
    fn disabled_autoconvert_feature_removes_pause_from_runtime_sequences() {
        let cfg = Config {
            autoconvert_feature_enabled: false,
            ..Default::default()
        };

        assert!(HotkeySequenceValues::from_config(&cfg).pause.is_none());
        assert!(HotkeySequenceValues::from_config_all(&cfg).pause.is_some());
    }

    #[test]
    fn disabling_autoconvert_removes_every_pending_pause_entry_point() {
        let cfg = Config::default();
        let pause_sequence = cfg.hotkey_pause_sequence.expect("default pause sequence");
        let last_word_sequence = cfg
            .hotkey_convert_last_word_sequence
            .expect("default last-word sequence");
        let mut state = AppState {
            autoconvert_enabled: true,
            autoconvert_feature_enabled: true,
            active_hotkey_sequences: HotkeySequenceValues {
                last_word: Some(last_word_sequence),
                pause: Some(pause_sequence),
                ..Default::default()
            },
            active_pause_hotkey: cfg.hotkey_pause,
            active_pause_hotkey_sequence: Some(pause_sequence),
            hotkey_sequence_progress: HotkeySequenceProgress {
                pause: SequenceProgress {
                    waiting_second: true,
                    first_tick_ms: 42,
                    matched_chords: 1,
                },
                ..Default::default()
            },
            deferred_sequence_hotkey: Some(DeferredSequenceHotkey {
                slot: HotkeySlot::Pause,
            }),
            pending_runtime_commands: [
                RuntimeCommand::AutoconvertLastWord,
                RuntimeCommand::Hotkey(HotkeyAction::PauseToggle),
                RuntimeCommand::Hotkey(HotkeyAction::ConvertLastWord),
            ]
            .into(),
            ..Default::default()
        };

        assert!(state.set_autoconvert_feature_enabled(false));
        assert!(!state.autoconvert_feature_enabled);
        assert!(!state.autoconvert_enabled);
        assert!(state.active_hotkey_sequences.pause.is_none());
        assert_eq!(
            state.active_hotkey_sequences.last_word,
            Some(last_word_sequence),
            "disabling AutoConvert must not disable unrelated hotkeys"
        );
        assert_eq!(state.hotkey_sequence_progress.pause.matched_chords, 0);
        assert!(state.deferred_sequence_hotkey.is_none());
        assert_eq!(
            state
                .pending_runtime_commands
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![RuntimeCommand::Hotkey(HotkeyAction::ConvertLastWord)]
        );

        assert!(!state.set_autoconvert_feature_enabled(true));
        assert!(state.autoconvert_feature_enabled);
        assert!(!state.autoconvert_enabled);
        assert_eq!(state.active_hotkey_sequences.pause, Some(pause_sequence));
    }
}
