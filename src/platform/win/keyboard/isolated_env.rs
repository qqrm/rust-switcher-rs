use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT};

use crate::{
    app::{AppState, HotkeySequenceValues, HotkeySlot},
    config::{HotkeyChord, HotkeySequence, MODVK_LALT, MODVK_LSHIFT, MODVK_RCTRL},
    platform::win::keyboard::sequence::{
        DEFERRED_SEQUENCE_DISAMBIGUATION_MS, SequenceMatch,
        take_deferred_sequence_disambiguated_by_chord, try_match_any_sequence_slot,
    },
};

#[derive(Debug, Default)]
pub(crate) struct IsolatedHotkeyEnv {
    state: AppState,
    fired: Vec<HotkeySlot>,
}

impl IsolatedHotkeyEnv {
    pub(crate) fn with_default_overlap_sequences() -> Self {
        Self {
            state: AppState {
                active_hotkey_sequences: HotkeySequenceValues {
                    last_word: Some(modifier_sequence(MOD_SHIFT.0, MODVK_LSHIFT, 2)),
                    last_sequence: Some(modifier_sequence(MOD_ALT.0, MODVK_LALT, 2)),
                    pause: Some(modifier_sequence(MOD_CONTROL.0, MODVK_RCTRL, 2)),
                    selection: Some(modifier_sequence(MOD_SHIFT.0, MODVK_LSHIFT, 2)),
                    smart_last_word: Some(modifier_sequence(MOD_SHIFT.0, MODVK_LSHIFT, 3)),
                    smart_last_sequence: Some(modifier_sequence(MOD_ALT.0, MODVK_LALT, 3)),
                    smart_selection: Some(modifier_sequence(MOD_SHIFT.0, MODVK_LSHIFT, 3)),
                    ..Default::default()
                },
                ..Default::default()
            },
            fired: Vec::new(),
        }
    }

    pub(crate) fn tap_left_shift(&mut self, now_ms: u64) -> SequenceMatch {
        self.tap(modifier_chord(MOD_SHIFT.0, MODVK_LSHIFT), now_ms)
    }

    pub(crate) fn tap_left_alt(&mut self, now_ms: u64) -> SequenceMatch {
        self.tap(modifier_chord(MOD_ALT.0, MODVK_LALT), now_ms)
    }

    pub(crate) fn tap_right_ctrl(&mut self, now_ms: u64) -> SequenceMatch {
        self.tap(modifier_chord(MOD_CONTROL.0, MODVK_RCTRL), now_ms)
    }

    pub(crate) fn fire_deferred_timeout(&mut self) -> Option<HotkeySlot> {
        let slot = self
            .state
            .deferred_sequence_hotkey
            .take()
            .map(|deferred| deferred.slot)?;
        self.state.hotkey_sequence_progress = Default::default();
        self.fired.push(slot);
        Some(slot)
    }

    pub(crate) fn fired(&self) -> &[HotkeySlot] {
        &self.fired
    }

    fn tap(&mut self, chord: HotkeyChord, now_ms: u64) -> SequenceMatch {
        if let Some(deferred) =
            take_deferred_sequence_disambiguated_by_chord(&mut self.state, chord, now_ms)
        {
            self.state.hotkey_sequence_progress = Default::default();
            self.fired.push(deferred.slot);
        }

        let result = try_match_any_sequence_slot(&mut self.state, chord, now_ms);
        if let SequenceMatch::Triggered(slot) = result {
            self.state.deferred_sequence_hotkey = None;
            self.fired.push(slot);
        }
        result
    }
}

fn modifier_chord(mods: u32, mods_vks: u32) -> HotkeyChord {
    HotkeyChord {
        mods,
        mods_vks,
        vk: None,
    }
}

fn modifier_sequence(mods: u32, mods_vks: u32, len: usize) -> HotkeySequence {
    let chord = modifier_chord(mods, mods_vks);
    HotkeySequence {
        first: chord,
        second: (len >= 2).then_some(chord),
        third: (len >= 3).then_some(chord),
        max_gap_ms: 1000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_env_triple_shift_cancels_deferred_double_shift() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_shift(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_shift(200),
            SequenceMatch::Pending(HotkeySlot::LastWord, DEFERRED_SEQUENCE_DISAMBIGUATION_MS)
        );
        assert_eq!(env.fired(), &[]);
        assert_eq!(
            env.tap_left_shift(300),
            SequenceMatch::Triggered(HotkeySlot::SmartLastWord)
        );

        assert_eq!(env.fired(), &[HotkeySlot::SmartLastWord]);
        assert_eq!(env.fire_deferred_timeout(), None);
    }

    #[test]
    fn isolated_env_double_shift_fires_after_timeout_without_third_shift() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_shift(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_shift(200),
            SequenceMatch::Pending(HotkeySlot::LastWord, DEFERRED_SEQUENCE_DISAMBIGUATION_MS)
        );

        assert_eq!(env.fire_deferred_timeout(), Some(HotkeySlot::LastWord));
        assert_eq!(env.fired(), &[HotkeySlot::LastWord]);
    }

    #[test]
    fn isolated_env_triple_alt_cancels_deferred_double_alt() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_alt(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_alt(200),
            SequenceMatch::Pending(
                HotkeySlot::LastSequence,
                DEFERRED_SEQUENCE_DISAMBIGUATION_MS
            )
        );
        assert_eq!(
            env.tap_left_alt(300),
            SequenceMatch::Triggered(HotkeySlot::SmartLastSequence)
        );

        assert_eq!(env.fired(), &[HotkeySlot::SmartLastSequence]);
        assert_eq!(env.fire_deferred_timeout(), None);
    }

    #[test]
    fn isolated_env_ctrl_pause_has_no_smart_prefix_delay() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_right_ctrl(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_right_ctrl(200),
            SequenceMatch::Triggered(HotkeySlot::Pause)
        );

        assert_eq!(env.fired(), &[HotkeySlot::Pause]);
        assert_eq!(env.fire_deferred_timeout(), None);
    }

    #[test]
    fn isolated_env_shift_alt_ctrl_slots_do_not_cross_trigger() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_shift(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_shift(200),
            SequenceMatch::Pending(HotkeySlot::LastWord, DEFERRED_SEQUENCE_DISAMBIGUATION_MS)
        );
        assert_eq!(
            env.tap_left_shift(300),
            SequenceMatch::Triggered(HotkeySlot::SmartLastWord)
        );

        assert_eq!(env.tap_left_alt(1_500), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_alt(1_600),
            SequenceMatch::Pending(
                HotkeySlot::LastSequence,
                DEFERRED_SEQUENCE_DISAMBIGUATION_MS
            )
        );
        assert_eq!(
            env.tap_left_alt(1_700),
            SequenceMatch::Triggered(HotkeySlot::SmartLastSequence)
        );

        assert_eq!(env.tap_right_ctrl(3_000), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_right_ctrl(3_100),
            SequenceMatch::Triggered(HotkeySlot::Pause)
        );

        assert_eq!(
            env.fired(),
            &[
                HotkeySlot::SmartLastWord,
                HotkeySlot::SmartLastSequence,
                HotkeySlot::Pause
            ]
        );
    }

    #[test]
    fn isolated_env_shift_shift_alt_alt_flushes_word_before_sequence() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_shift(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_shift(200),
            SequenceMatch::Pending(HotkeySlot::LastWord, DEFERRED_SEQUENCE_DISAMBIGUATION_MS)
        );
        assert_eq!(env.fired(), &[]);

        assert_eq!(env.tap_left_alt(300), SequenceMatch::Consumed);
        assert_eq!(env.fired(), &[HotkeySlot::LastWord]);
        assert_eq!(
            env.tap_left_alt(400),
            SequenceMatch::Pending(
                HotkeySlot::LastSequence,
                DEFERRED_SEQUENCE_DISAMBIGUATION_MS
            )
        );
        assert_eq!(env.fire_deferred_timeout(), Some(HotkeySlot::LastSequence));

        assert_eq!(
            env.fired(),
            &[HotkeySlot::LastWord, HotkeySlot::LastSequence]
        );
    }

    #[test]
    fn isolated_env_shift_shift_shift_does_not_flush_double_shift() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_shift(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_shift(200),
            SequenceMatch::Pending(HotkeySlot::LastWord, DEFERRED_SEQUENCE_DISAMBIGUATION_MS)
        );
        assert_eq!(
            env.tap_left_shift(300),
            SequenceMatch::Triggered(HotkeySlot::SmartLastWord)
        );

        assert_eq!(env.fired(), &[HotkeySlot::SmartLastWord]);
        assert_eq!(env.fire_deferred_timeout(), None);
    }

    #[test]
    fn isolated_env_alt_alt_shift_shift_flushes_sequence_before_word() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_alt(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_alt(200),
            SequenceMatch::Pending(
                HotkeySlot::LastSequence,
                DEFERRED_SEQUENCE_DISAMBIGUATION_MS
            )
        );
        assert_eq!(env.tap_left_shift(300), SequenceMatch::Consumed);
        assert_eq!(env.fired(), &[HotkeySlot::LastSequence]);
        assert_eq!(
            env.tap_left_shift(400),
            SequenceMatch::Pending(HotkeySlot::LastWord, DEFERRED_SEQUENCE_DISAMBIGUATION_MS)
        );
        assert_eq!(env.fire_deferred_timeout(), Some(HotkeySlot::LastWord));

        assert_eq!(
            env.fired(),
            &[HotkeySlot::LastSequence, HotkeySlot::LastWord]
        );
    }

    #[test]
    fn isolated_env_shift_shift_ctrl_ctrl_flushes_word_before_pause() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_shift(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_shift(200),
            SequenceMatch::Pending(HotkeySlot::LastWord, DEFERRED_SEQUENCE_DISAMBIGUATION_MS)
        );
        assert_eq!(env.tap_right_ctrl(300), SequenceMatch::Consumed);
        assert_eq!(env.fired(), &[HotkeySlot::LastWord]);
        assert_eq!(
            env.tap_right_ctrl(400),
            SequenceMatch::Triggered(HotkeySlot::Pause)
        );

        assert_eq!(env.fired(), &[HotkeySlot::LastWord, HotkeySlot::Pause]);
    }

    #[test]
    fn isolated_env_shared_shift_slots_do_not_leave_stale_selection_progress() {
        let mut env = IsolatedHotkeyEnv::with_default_overlap_sequences();

        assert_eq!(env.tap_left_shift(100), SequenceMatch::Consumed);
        assert_eq!(
            env.tap_left_shift(200),
            SequenceMatch::Pending(HotkeySlot::LastWord, DEFERRED_SEQUENCE_DISAMBIGUATION_MS)
        );
        assert_eq!(env.fire_deferred_timeout(), Some(HotkeySlot::LastWord));
        assert_eq!(env.tap_left_shift(1_500), SequenceMatch::Consumed);

        assert_eq!(env.fired(), &[HotkeySlot::LastWord]);
    }
}
