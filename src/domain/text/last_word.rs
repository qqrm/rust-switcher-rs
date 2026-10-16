use std::{
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use windows::Win32::UI::{
    Input::KeyboardAndMouse::VIRTUAL_KEY, WindowsAndMessaging::GetForegroundWindow,
};

use super::{
    convert::expected_direction_for_foreground_window,
    mapping::{ConversionDirection, conversion_direction_for_text, convert_ru_en_with_direction},
    smart,
    switch_keyboard_layout, wait_shift_released,
};
use crate::{
    app::AppState,
    conversion::input::{send_text_unicode, tap_key},
    input_journal::{InputRun, LayoutTag, RunKind, RunOrigin},
};
const VK_BACKSPACE_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0x08);
const VK_LEFT_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0x25);
const VK_RIGHT_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0x27);
const MIN_WORD_LEN: usize = 4;
const MIN_CONVERTED_CONFIDENCE: f64 = 0.70;
const MIN_CONFIDENCE_GAIN: f64 = 0.25;
static AUTOCONVERT_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[derive(Copy, Clone, Debug)]
enum DirectionMode {
    LayoutFirst,
    TextFirst,
}

fn direction_for_layout(layout: &LayoutTag) -> Option<ConversionDirection> {
    match layout {
        LayoutTag::Ru => Some(ConversionDirection::RuToEn),
        LayoutTag::En => Some(ConversionDirection::EnToRu),
        LayoutTag::Other(_) | LayoutTag::Unknown => None,
    }
}

fn convert_with_mode(text: &str, layout: &LayoutTag, mode: DirectionMode) -> String {
    let direction = match mode {
        DirectionMode::LayoutFirst => direction_for_layout(layout)
            .or_else(|| conversion_direction_for_text(text))
            .or_else(expected_direction_for_foreground_window),
        DirectionMode::TextFirst => conversion_direction_for_text(text)
            .or_else(|| direction_for_layout(layout))
            .or_else(expected_direction_for_foreground_window),
    }
    .unwrap_or(ConversionDirection::RuToEn);
    convert_ru_en_with_direction(text, direction)
}

fn convert_with_layout_fallback(text: &str, layout: &LayoutTag) -> String {
    convert_with_mode(text, layout, DirectionMode::LayoutFirst)
}

fn convert_sequence_runs(runs: &[InputRun], mode: DirectionMode) -> String {
    let mut out = String::new();

    for run in runs {
        match run.kind {
            RunKind::Text => out.push_str(&convert_with_mode(&run.text, &run.layout, mode)),
            RunKind::Whitespace => out.push_str(&run.text),
        }
    }

    out
}
pub fn convert_last_word(state: &mut AppState) {
    let _ = convert_last_word_if_any(state);
}

/// Converts the last word when a current-line word is available.
///
/// Returns `true` when a conversion was attempted or deliberately rejected.
/// Returns `false` only when no word is available at the caret, allowing a
/// shared layout-switch hotkey to fall back to switching the input layout.
pub fn convert_last_word_if_any(state: &mut AppState) -> bool {
    convert_last_word_impl(state, true, DirectionMode::LayoutFirst)
}

pub fn convert_last_sequence(state: &mut AppState) {
    convert_last_sequence_impl(state, true, DirectionMode::LayoutFirst);
}

pub fn smart_convert_last_word(state: &mut AppState) {
    let _ = convert_last_word_impl(state, true, DirectionMode::TextFirst);
}

pub fn smart_convert_last_sequence(state: &mut AppState) {
    convert_last_sequence_impl(state, true, DirectionMode::TextFirst);
}

pub fn autoconvert_last_word(state: &mut AppState) {
    if !foreground_window_alive() {
        tracing::warn!("foreground window is null");
        return;
    }
    let _guard = match AutoconvertGuard::try_acquire() {
        Ok(g) => g,
        Err(reason) => {
            tracing::trace!(reason = %reason.as_str(), "autoconvert skip: reentry");
            return;
        }
    };
    sleep_before_convert(state);
    let Some(payload) = take_last_word_payload() else {
        tracing::trace!("journal: no last word");
        return;
    };
    let mut restore = JournalRestore::new(&payload);
    let converted = match autoconvert_candidate(&payload) {
        Ok(v) => v,
        Err(reason) => {
            tracing::trace!(reason = %reason.as_str(), "autoconvert skip: candidate");
            return;
        }
    };
    let detector = language_detector();
    if let Err(reason) = should_autoconvert_word(detector, &payload.run.text, &converted) {
        tracing::trace!(reason = %reason.as_str(), "autoconvert skip: decision");
        return;
    }
    tracing::trace!(word = %payload.run.text, converted = %converted, "autoconvert decision");
    if let Err(err) = apply_last_word_replacement(&payload, &converted) {
        tracing::warn!(error = %err.as_str(), "autoconvert apply failed");
        return;
    }
    update_journal(&payload, &converted);
    crate::input_journal::mark_last_token_autoconverted();
    restore.commit();
    match switch_keyboard_layout() {
        Ok(()) => tracing::trace!("layout switched (autoconvert)"),
        Err(e) => tracing::warn!(error = ?e, "layout switch failed (autoconvert)"),
    }
}
#[must_use = "guard must be kept alive to prevent reentry"]
struct AutoconvertGuard;
impl AutoconvertGuard {
    fn try_acquire() -> Result<Self, SkipReason> {
        if AUTOCONVERT_IN_PROGRESS.swap(true, Ordering::AcqRel) {
            return Err(SkipReason::Reentry);
        }
        Ok(Self)
    }
}
impl Drop for AutoconvertGuard {
    fn drop(&mut self) {
        AUTOCONVERT_IN_PROGRESS.store(false, Ordering::Release);
    }
}
#[must_use = "restore guard must be kept alive until commit"]
struct JournalRestore<'a> {
    payload: &'a LastRunPayload,
    committed: bool,
}
impl<'a> JournalRestore<'a> {
    fn new(payload: &'a LastRunPayload) -> Self {
        Self {
            payload,
            committed: false,
        }
    }
    fn commit(&mut self) {
        self.committed = true;
    }
}
impl Drop for JournalRestore<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        restore_journal_original(self.payload);
        tracing::trace!("autoconvert: journal restored");
    }
}

#[must_use = "restore guard must be kept alive until commit"]
struct JournalRestoreSequence<'a> {
    payload: &'a LastSequencePayload,
    committed: bool,
}
impl<'a> JournalRestoreSequence<'a> {
    fn new(payload: &'a LastSequencePayload) -> Self {
        Self {
            payload,
            committed: false,
        }
    }
    fn commit(&mut self) {
        self.committed = true;
    }
}
impl Drop for JournalRestoreSequence<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        restore_journal_original_sequence(self.payload);
    }
}
#[derive(Copy, Clone, Debug)]
pub(crate) enum SkipReason {
    Reentry,
    SuffixHasNewline,
    NotAWord,
    NoChangeAfterConvert,
    TooShort,
    ScriptCheckFailed,
    AlreadyCorrect,
    ConvertedConfidenceLow,
    NotBetterEnough,
}
impl SkipReason {
    fn as_str(self) -> &'static str {
        match self {
            SkipReason::Reentry => "reentry",
            SkipReason::SuffixHasNewline => "suffix_has_newline",
            SkipReason::NotAWord => "not_a_word",
            SkipReason::NoChangeAfterConvert => "no_change_after_convert",
            SkipReason::TooShort => "too_short",
            SkipReason::ScriptCheckFailed => "script_check_failed",
            SkipReason::AlreadyCorrect => "already_correct",
            SkipReason::ConvertedConfidenceLow => "converted_confidence_low",
            SkipReason::NotBetterEnough => "not_better_enough",
        }
    }
}
fn has_ascii_vowel(s: &str) -> bool {
    s.chars().any(|ch| {
        let c = ch.to_ascii_lowercase();
        matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
    })
}
fn has_cyrillic_vowel(s: &str) -> bool {
    s.chars().any(|ch| {
        let c = ch.to_lowercase().next().unwrap_or(ch);
        matches!(c, 'а' | 'е' | 'ё' | 'и' | 'о' | 'у' | 'ы' | 'э' | 'ю' | 'я')
    })
}
fn is_plausible_english_like_token(s: &str) -> bool {
    if !looks_like_ascii_word(s) {
        return false;
    }
    let has_vowel = has_ascii_vowel(s);
    // 'y' intentionally treated as consonant here to reduce false positives.
    let mut consonant_run = 0usize;
    let mut max_consonant_run = 0usize;
    let mut rare = 0usize;
    for ch in s.chars() {
        if ch == '\'' {
            continue;
        }
        let c = ch.to_ascii_lowercase();
        let is_vowel = matches!(c, 'a' | 'e' | 'i' | 'o' | 'u');
        if is_vowel {
            consonant_run = 0;
        } else {
            consonant_run += 1;
            max_consonant_run = max_consonant_run.max(consonant_run);
            if matches!(c, 'j' | 'q' | 'x' | 'z') {
                rare += 1;
            }
        }
    }
    has_vowel && max_consonant_run <= 4 && rare <= 1
}
fn is_plausible_russian_like_token(s: &str) -> bool {
    if !looks_like_cyrillic_word(s) {
        return false;
    }
    if !has_cyrillic_vowel(s) {
        return false;
    }
    let mut consonant_run = 0usize;
    let mut max_consonant_run = 0usize;
    for ch in s.chars() {
        if ch == '\'' || ch == '-' {
            continue;
        }
        if !ch.is_alphabetic() {
            continue;
        }
        let c = ch.to_lowercase().next().unwrap_or(ch);
        let is_vowel = matches!(c, 'а' | 'е' | 'ё' | 'и' | 'о' | 'у' | 'ы' | 'э' | 'ю' | 'я');
        if is_vowel {
            consonant_run = 0;
        } else {
            consonant_run += 1;
            max_consonant_run = max_consonant_run.max(consonant_run);
        }
    }
    max_consonant_run <= 4
}
#[derive(Copy, Clone, Debug)]
enum ApplyError {
    KeyInjectionFailed,
}
impl ApplyError {
    fn as_str(self) -> &'static str {
        match self {
            ApplyError::KeyInjectionFailed => "key_injection_failed",
        }
    }
}
fn autoconvert_candidate(p: &LastRunPayload) -> Result<String, SkipReason> {
    ensure_no_newline(p)?;
    ensure_has_letters(&p.run.text)?;
    let converted = convert_with_layout_fallback(&p.run.text, &p.run.layout);
    ensure_changed(&p.run.text, &converted)?;
    Ok(converted)
}
fn language_detector() -> &'static lingua::LanguageDetector {
    use lingua::{Language, LanguageDetector, LanguageDetectorBuilder};
    static DETECTOR: OnceLock<LanguageDetector> = OnceLock::new();
    DETECTOR.get_or_init(|| {
        LanguageDetectorBuilder::from_languages(&[Language::English, Language::Russian])
            .with_minimum_relative_distance(0.20)
            .build()
    })
}
fn looks_like_ascii_word(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }

    let is_ascii_letter = |b: u8| b.is_ascii_alphabetic();
    let has_letter = bytes.iter().copied().any(is_ascii_letter);
    if !has_letter {
        return false;
    }

    bytes.iter().copied().enumerate().all(|(i, b)| {
        if is_ascii_letter(b) || b == b'\'' {
            return true;
        }

        // Allow dot or comma only when it is between ASCII letters.
        (b == b'.' || b == b',')
            && i > 0
            && i + 1 < bytes.len()
            && is_ascii_letter(bytes[i - 1])
            && is_ascii_letter(bytes[i + 1])
    })
}
fn trailing_convertible_punct_count(s: &str) -> usize {
    s.chars()
        .rev()
        .take_while(|ch| matches!(ch, '?' | '/' | ',' | '.'))
        .count()
}
fn trim_tail_chars(s: &str, n: usize) -> &str {
    if n == 0 {
        return s;
    }

    // `n` is usually tiny (trailing punctuation), so scan from the end for the cut boundary.
    let Some((cut, _)) = s.char_indices().rev().nth(n.saturating_sub(1)) else {
        return "";
    };
    &s[..cut]
}
fn should_autoconvert_word(
    detector: &lingua::LanguageDetector,
    word: &str,
    converted: &str,
) -> Result<(), SkipReason> {
    use lingua::Language;
    const MIN_CONVERTED_EN_CONF_FOR_OVERRIDE: f64 = 0.80;
    let trailing_punct = trailing_convertible_punct_count(word);
    let word_analysis = trim_tail_chars(word, trailing_punct);
    let conv_analysis = trim_tail_chars(converted, trailing_punct);
    if word_analysis.is_empty() || conv_analysis.is_empty() {
        return Err(SkipReason::ScriptCheckFailed);
    }
    if word_analysis.chars().count() < MIN_WORD_LEN {
        return Err(SkipReason::TooShort);
    }
    let w_is_ascii = looks_like_ascii_word(word_analysis);
    let w_is_cyr = looks_like_cyrillic_word(word_analysis);
    let c_is_ascii = looks_like_ascii_word(conv_analysis);
    let c_is_cyr = looks_like_cyrillic_word(conv_analysis);
    if !(w_is_ascii || w_is_cyr) || !(c_is_ascii || c_is_cyr) {
        return Err(SkipReason::ScriptCheckFailed);
    }
    let w_ru = confidence(detector, word_analysis, Language::Russian);
    let w_en = confidence(detector, word_analysis, Language::English);
    let c_ru = confidence(detector, conv_analysis, Language::Russian);
    let c_en = confidence(detector, conv_analysis, Language::English);
    // Keep the English guard: do not convert real English words to Russian.
    if w_is_ascii && is_plausible_english_like_token(word_analysis) {
        return Err(SkipReason::AlreadyCorrect);
    }
    // Russian guard is conditional: if conversion yields a strong English candidate, do not short circuit.
    if w_is_cyr && is_plausible_russian_like_token(word_analysis) {
        let converted_looks_english = is_plausible_english_like_token(conv_analysis)
            && c_en >= MIN_CONVERTED_EN_CONF_FOR_OVERRIDE;
        if !converted_looks_english {
            return Err(SkipReason::AlreadyCorrect);
        }
    }
    let w_best = w_ru.max(w_en);
    let c_best = c_ru.max(c_en);
    let target = if w_is_ascii {
        Language::Russian
    } else {
        Language::English
    };
    let (w_in_target, c_in_target) = if matches!(target, Language::Russian) {
        (w_ru, c_ru)
    } else {
        (w_en, c_en)
    };
    if c_best < MIN_CONVERTED_CONFIDENCE {
        return Err(SkipReason::ConvertedConfidenceLow);
    }
    let min_abs = if w_best < 0.30 {
        0.55
    } else {
        MIN_CONVERTED_CONFIDENCE
    };
    if c_in_target < min_abs {
        return Err(SkipReason::ConvertedConfidenceLow);
    }
    if c_in_target - w_in_target < MIN_CONFIDENCE_GAIN {
        return Err(SkipReason::NotBetterEnough);
    }
    Ok(())
}
fn confidence(detector: &lingua::LanguageDetector, text: &str, lang: lingua::Language) -> f64 {
    detector
        .compute_language_confidence_values(text)
        .iter()
        .find(|(l, _)| *l == lang)
        .map_or(0.0, |(_, v)| *v)
}
fn ensure_no_newline(p: &LastRunPayload) -> Result<(), SkipReason> {
    if p.suffix_has_newline {
        return Err(SkipReason::SuffixHasNewline);
    }
    Ok(())
}
fn ensure_has_letters(word: &str) -> Result<(), SkipReason> {
    if word.chars().any(char::is_alphabetic) {
        return Ok(());
    }
    Err(SkipReason::NotAWord)
}
fn ensure_changed(word: &str, converted: &str) -> Result<(), SkipReason> {
    if word != converted {
        return Ok(());
    }
    Err(SkipReason::NoChangeAfterConvert)
}
fn looks_like_cyrillic_word(s: &str) -> bool {
    let mut has_alpha = false;
    for ch in s.chars() {
        if ch.is_alphabetic() {
            if !is_cyrillic(ch) {
                return false;
            }
            has_alpha = true;
            continue;
        }
        if ch == '\'' || ch == '-' {
            continue;
        }
        return false;
    }
    has_alpha
}
fn is_cyrillic(ch: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&ch) || ('\u{0500}'..='\u{052F}').contains(&ch)
}
fn apply_last_word_replacement(p: &LastRunPayload, converted: &str) -> Result<(), ApplyError> {
    if apply_last_word_conversion(p, converted) {
        Ok(())
    } else {
        Err(ApplyError::KeyInjectionFailed)
    }
}

#[tracing::instrument(level = "trace", skip(state))]
fn convert_last_word_impl(state: &mut AppState, switch_layout: bool, mode: DirectionMode) -> bool {
    if !foreground_window_alive() {
        tracing::warn!("foreground window is null");
        return true;
    }
    if switch_layout && !wait_shift_released(150) {
        tracing::info!("wait_shift_released returned false");
        return true;
    }
    sleep_before_convert(state);
    let Some(payload) = take_last_word_payload() else {
        tracing::info!("journal: no last word");
        return false;
    };
    let mut restore = JournalRestore::new(&payload);
    if payload.suffix_has_newline {
        tracing::trace!("newline present, skipping convert_last_word");
        return false;
    }
    if matches!(mode, DirectionMode::TextFirst) && smart::text_looks_correct(&payload.run.text) {
        tracing::trace!(text = %payload.run.text, "smart convert skipped: text already looks correct");
        return true;
    }
    let converted = convert_with_mode(&payload.run.text, &payload.run.layout, mode);
    tracing::trace!(%converted, "converted");
    if apply_last_word_conversion(&payload, &converted) {
        update_journal(&payload, &converted);
        restore.commit();
        if switch_layout {
            match switch_keyboard_layout() {
                Ok(()) => tracing::trace!("layout switched"),
                Err(e) => tracing::warn!(error = ?e, "layout switch failed"),
            }
        }
    } else {
        tracing::warn!("convert apply failed");
    }

    true
}

#[tracing::instrument(level = "trace", skip(state))]
fn convert_last_sequence_impl(state: &mut AppState, switch_layout: bool, mode: DirectionMode) {
    if !foreground_window_alive() {
        tracing::warn!("foreground window is null");
        return;
    }
    if switch_layout && !wait_shift_released(150) {
        tracing::info!("wait_shift_released returned false");
        return;
    }
    sleep_before_convert(state);
    let Some(payload) = take_last_sequence_payload() else {
        tracing::info!("journal: no last sequence");
        return;
    };
    let mut restore = JournalRestoreSequence::new(&payload);
    if payload.suffix_has_newline || payload.seq_has_newline {
        tracing::trace!("newline present, skipping convert_last_sequence");
        return;
    }
    if matches!(mode, DirectionMode::TextFirst) && smart::text_looks_correct(&payload.seq_text) {
        tracing::trace!(text = %payload.seq_text, "smart convert skipped: text already looks correct");
        return;
    }
    let converted = convert_sequence_runs(&payload.runs, mode);
    tracing::trace!(%converted, "converted");
    if apply_last_sequence_conversion(&payload, &converted) {
        update_journal_sequence(&payload, &converted, mode);
        restore.commit();
        if switch_layout {
            match switch_keyboard_layout() {
                Ok(()) => tracing::trace!("layout switched"),
                Err(e) => tracing::warn!(error = ?e, "layout switch failed"),
            }
        }
    } else {
        tracing::warn!("convert apply failed");
    }
}

fn foreground_window_alive() -> bool {
    let fg = unsafe { GetForegroundWindow() };
    !fg.0.is_null()
}
fn sleep_before_convert(_state: &AppState) {
    let delay_ms = crate::config::CONVERSION_DELAY_MS;
    tracing::trace!(delay_ms, "sleep before convert");
    thread::sleep(Duration::from_millis(u64::from(delay_ms)));
}
struct LastRunPayload {
    run: InputRun,
    suffix_runs: Vec<InputRun>,
    suffix_text: String,
    run_len: usize,
    suffix_len: usize,
    suffix_spaces_only: bool,
    suffix_has_newline: bool,
}

struct LastSequencePayload {
    prefix_runs: Vec<InputRun>,
    runs: Vec<InputRun>,
    #[cfg_attr(not(test), allow(dead_code))]
    layout: LayoutTag,
    suffix_runs: Vec<InputRun>,
    suffix_text: String,
    #[cfg_attr(not(test), allow(dead_code))]
    seq_text: String,
    seq_len: usize,
    suffix_len: usize,
    suffix_spaces_only: bool,
    suffix_has_newline: bool,
    seq_has_newline: bool,
}
fn suffix_text_and_meta(suffix_runs: &[InputRun]) -> (String, usize, bool, bool) {
    let text: String = suffix_runs.iter().map(|run| run.text.as_str()).collect();
    let len = text.chars().count();
    let spaces_only = !text.is_empty() && text.chars().all(|c| c == ' ' || c == '\t');
    let has_newline = text.contains('\n') || text.contains('\r');
    (text, len, spaces_only, has_newline)
}
fn take_last_word_payload() -> Option<LastRunPayload> {
    let (run, suffix_runs) = crate::input_journal::take_last_layout_run_with_suffix()?;
    if run.kind != RunKind::Text || run.text.is_empty() {
        return None;
    }
    let (suffix_text, suffix_len, suffix_spaces_only, suffix_has_newline) =
        suffix_text_and_meta(&suffix_runs);
    let run_len = run.text.chars().count();
    tracing::trace!(
        run_text = %run.text,
        run_layout = ?run.layout,
        run_origin = ?run.origin,
        suffix_text = %suffix_text,
        run_len,
        suffix_len,
        suffix_spaces_only,
        suffix_has_newline,
        "journal extracted run"
    );
    Some(LastRunPayload {
        run,
        suffix_runs,
        suffix_text,
        run_len,
        suffix_len,
        suffix_spaces_only,
        suffix_has_newline,
    })
}
fn join_runs_text(runs: &[InputRun]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

fn take_last_sequence_payload() -> Option<LastSequencePayload> {
    let (runs, suffix_runs) = crate::input_journal::take_last_sequence_with_suffix()?;
    let last = runs.last()?;
    if last.kind != RunKind::Text {
        return None;
    }
    let layout = last.layout;
    let seq_text = join_runs_text(&runs);
    if seq_text.is_empty() {
        return None;
    }
    let (suffix_text, suffix_len, suffix_spaces_only, suffix_has_newline) =
        suffix_text_and_meta(&suffix_runs);
    let seq_len = seq_text.chars().count();
    let seq_has_newline = seq_text.contains('\n') || seq_text.contains('\r');
    tracing::trace!(
        seq_text = %seq_text,
        seq_layout = ?layout,
        suffix_text = %suffix_text,
        seq_len,
        suffix_len,
        suffix_spaces_only,
        suffix_has_newline,
        seq_has_newline,
        "journal extracted sequence"
    );

    Some(LastSequencePayload {
        prefix_runs: Vec::new(),
        runs,
        layout,
        suffix_runs,
        suffix_text,
        seq_text,
        seq_len,
        suffix_len,
        suffix_spaces_only,
        suffix_has_newline,
        seq_has_newline,
    })
}

fn apply_conversion(
    core_len: usize,
    suffix_len: usize,
    suffix_spaces_only: bool,
    suffix_text: &str,
    converted: &str,
) -> bool {
    const MAX_TAPS: usize = 4096;

    let core_len = core_len.min(MAX_TAPS);
    let suffix_len = suffix_len.min(MAX_TAPS);

    if suffix_spaces_only {
        move_caret_left(suffix_len)
            && delete_with_backspace(core_len)
            && send_text_unicode(converted)
            && move_caret_right(suffix_len)
    } else {
        let delete_count = core_len.saturating_add(suffix_len).min(MAX_TAPS);
        delete_with_backspace(delete_count)
            && send_text_unicode(converted)
            && (suffix_text.is_empty() || send_text_unicode(suffix_text))
    }
}

fn apply_last_word_conversion(p: &LastRunPayload, converted: &str) -> bool {
    apply_conversion(
        p.run_len,
        p.suffix_len,
        p.suffix_spaces_only,
        &p.suffix_text,
        converted,
    )
}

fn apply_last_sequence_conversion(p: &LastSequencePayload, converted: &str) -> bool {
    apply_conversion(
        p.seq_len,
        p.suffix_len,
        p.suffix_spaces_only,
        &p.suffix_text,
        converted,
    )
}

fn flipped_layout(layout: LayoutTag) -> LayoutTag {
    match layout {
        LayoutTag::Ru => LayoutTag::En,
        LayoutTag::En => LayoutTag::Ru,
        other => other,
    }
}
/// Updates the input journal to match what was inserted.
fn restore_journal_original_sequence(p: &LastSequencePayload) {
    crate::input_journal::push_runs(p.prefix_runs.iter().cloned());
    crate::input_journal::push_runs(p.runs.iter().cloned());
    crate::input_journal::push_runs(p.suffix_runs.iter().cloned());
    tracing::trace!("journal restored (original sequence metadata)");
}

fn update_journal_sequence(p: &LastSequencePayload, converted: &str, mode: DirectionMode) {
    crate::input_journal::push_runs(p.prefix_runs.iter().cloned());
    crate::input_journal::push_runs(p.runs.iter().map(|run| {
        let text = match run.kind {
            RunKind::Whitespace => run.text.clone(),
            RunKind::Text => convert_with_mode(&run.text, &run.layout, mode),
        };

        let layout = if run.kind == RunKind::Text && text != run.text {
            flipped_layout(run.layout)
        } else {
            run.layout
        };

        InputRun {
            text,
            layout,
            origin: RunOrigin::Programmatic,
            kind: run.kind,
        }
    }));
    crate::input_journal::push_runs(p.suffix_runs.iter().cloned());
    tracing::trace!(%converted, "journal updated (sequence)");
}
fn restore_journal_original(p: &LastRunPayload) {
    crate::input_journal::push_run(p.run.clone());
    crate::input_journal::push_runs(p.suffix_runs.iter().cloned());
    tracing::trace!("journal restored (original metadata)");
}
fn update_journal(p: &LastRunPayload, converted: &str) {
    crate::input_journal::push_run(InputRun {
        text: converted.to_string(),
        layout: flipped_layout(p.run.layout),
        origin: RunOrigin::Programmatic,
        kind: RunKind::Text,
    });
    crate::input_journal::push_runs(p.suffix_runs.iter().cloned());
    tracing::trace!("journal updated");
}
fn delete_with_backspace(count: usize) -> bool {
    repeat_tap(VK_BACKSPACE_KEY, count, "backspace tap failed")
}
fn move_caret_left(count: usize) -> bool {
    repeat_tap(VK_LEFT_KEY, count, "left arrow tap failed")
}
fn move_caret_right(count: usize) -> bool {
    repeat_tap(VK_RIGHT_KEY, count, "right arrow tap failed")
}
fn repeat_tap(vk: VIRTUAL_KEY, count: usize, err_msg: &'static str) -> bool {
    match (0..count).try_for_each(|i| tap_key(vk).then_some(()).ok_or(i)) {
        Ok(()) => true,
        Err(i) => {
            tracing::error!(i, count, %err_msg);
            false
        }
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::sync::MutexGuard;

    use lingua::{Language, LanguageDetectorBuilder};

    use super::*;
    use crate::input::ring_buffer;

    fn test_lock() -> MutexGuard<'static, ()> {
        ring_buffer::test_guard()
    }
    fn detector_ru_en() -> lingua::LanguageDetector {
        LanguageDetectorBuilder::from_languages(&[Language::Russian, Language::English])
            .with_minimum_relative_distance(0.20)
            .build()
    }
    #[test]
    fn ru_layout_punctuation_run_converts_ru_to_en() {
        let _guard = test_lock();
        assert_eq!(convert_with_layout_fallback(",.", &LayoutTag::Ru), "?/");
    }
    #[test]
    fn en_layout_punctuation_run_converts_en_to_ru() {
        let _guard = test_lock();
        assert_eq!(convert_with_layout_fallback(",.", &LayoutTag::En), "бю");
    }
    #[test]
    fn known_layout_overrides_text_heuristic() {
        let _guard = test_lock();
        // Mixed punctuation has no letter heuristic signal, but known layout enforces direction.
        assert_eq!(convert_with_layout_fallback(".", &LayoutTag::Ru), "/");
        assert_eq!(convert_with_layout_fallback(".", &LayoutTag::En), "ю");
    }

    #[test]
    fn smart_direction_prefers_text_over_stale_layout_metadata() {
        let _guard = test_lock();
        assert_eq!(
            convert_with_mode("ghbdtn", &LayoutTag::Ru, DirectionMode::TextFirst),
            "привет"
        );
        assert_eq!(
            convert_with_mode("привет", &LayoutTag::En, DirectionMode::TextFirst),
            "ghbdtn"
        );
    }

    #[test]
    fn smart_sequence_converts_programmatic_tail_by_text_analysis() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "hfp".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "ldf".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "три".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Programmatic,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(payload.seq_text, "hfp ldf три");
        assert_eq!(
            convert_sequence_runs(&payload.runs, DirectionMode::TextFirst),
            "раз два nhb"
        );
    }

    #[test]
    fn update_and_restore_preserve_run_metadata() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "abc,".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: "  ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
        ]);
        let payload = take_last_word_payload().expect("payload expected");
        update_journal(&payload, "фисб");
        let (run, suffix) = ring_buffer::take_last_layout_run_with_suffix().expect("run expected");
        assert_eq!(run.layout, LayoutTag::Ru);
        assert_eq!(run.origin, RunOrigin::Programmatic);
        assert_eq!(run.kind, RunKind::Text);
        assert_eq!(run.text, "фисб");
        assert_eq!(suffix.len(), 1);
        assert_eq!(suffix[0].layout, LayoutTag::En);
        assert_eq!(suffix[0].origin, RunOrigin::Physical);
        assert_eq!(suffix[0].kind, RunKind::Whitespace);
        ring_buffer::push_run(payload.run.clone());
        ring_buffer::push_runs(payload.suffix_runs.clone());
        let (restored, restored_suffix) =
            ring_buffer::take_last_layout_run_with_suffix().expect("restored payload expected");
        assert_eq!(restored, payload.run);
        assert_eq!(restored_suffix, payload.suffix_runs);
    }
    #[test]
    fn convert_candidate_converts_entire_run_without_punct_peel() {
        let _guard = test_lock();
        let payload = LastRunPayload {
            run: InputRun {
                text: "ghbdtn,".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            suffix_runs: Vec::new(),
            suffix_text: String::new(),
            run_len: 7,
            suffix_len: 0,
            suffix_spaces_only: false,
            suffix_has_newline: false,
        };
        let converted = autoconvert_candidate(&payload).expect("candidate should convert");
        assert_eq!(converted, "приветб");
    }
    #[test]
    fn autoconvert_does_not_touch_correct_russian_word() {
        let _guard = test_lock();
        let detector = detector_ru_en();
        let word = "привет";
        let converted = convert_with_layout_fallback(word, &LayoutTag::Ru);
        assert_eq!(converted, "ghbdtn");
        let decision = should_autoconvert_word(&detector, word, &converted);
        assert!(
            decision.is_err(),
            "should not autoconvert correct Russian word"
        );
    }
    #[test]
    fn journal_restore_drop_restores_original_metadata() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "abc,".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: "  ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
        ]);
        let payload = take_last_word_payload().expect("payload expected");
        {
            let _restore = JournalRestore::new(&payload);
        }
        let (restored, restored_suffix) =
            ring_buffer::take_last_layout_run_with_suffix().expect("restored payload expected");
        assert_eq!(restored, payload.run);
        assert_eq!(restored_suffix, payload.suffix_runs);
    }
    #[test]
    fn autoconvert_decision_accepts_trailing_convertible_punctuation() {
        let _guard = test_lock();
        let detector = detector_ru_en();
        let word = "ghbdtn,";
        let converted = convert_with_layout_fallback(word, &LayoutTag::En);
        assert_eq!(converted, "приветб");
        assert!(should_autoconvert_word(&detector, word, &converted).is_ok());
    }

    #[test]
    fn last_word_payload_extracts_only_final_text_run() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: "  ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
        ]);

        let payload = take_last_word_payload().expect("payload expected");
        assert_eq!(payload.run.text, "rjynhjkm");
        assert_eq!(payload.suffix_text, "  ");
        assert_eq!(payload.run.layout, LayoutTag::En);
    }

    #[test]
    fn update_journal_for_last_word_keeps_prefix_runs() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_word_payload().expect("payload expected");
        update_journal(&payload, "школа");

        let runs = ring_buffer::runs_snapshot();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].text, "ghbdtn");
        assert_eq!(runs[0].layout, LayoutTag::En);
        assert_eq!(runs[1].text, " ");
        assert_eq!(runs[1].kind, RunKind::Whitespace);
        assert_eq!(runs[2].text, "школа");
        assert_eq!(runs[2].layout, LayoutTag::Ru);
        assert_eq!(runs[2].origin, RunOrigin::Programmatic);
    }

    #[test]
    fn last_sequence_payload_spans_whitespace_and_uses_single_layout() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: "  ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(payload.layout, LayoutTag::En);
        assert_eq!(payload.seq_text, "ghbdtn rjynhjkm");
        assert_eq!(payload.suffix_text, "  ");
        assert!(payload.prefix_runs.is_empty());
    }

    #[test]
    fn manual_sequence_can_toggle_back_via_programmatic_origin() {
        let _guard = test_lock();
        // Repro of the bug:
        // - First manual convert pushes Programmatic runs.
        // - Extractor must allow Programmatic-origin sequences, otherwise second convert cannot happen.
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        // First extraction (physical EN)
        let p1 = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(p1.layout, LayoutTag::En);
        assert_eq!(p1.seq_text, "ghbdtn rjynhjkm");

        // Simulate manual conversion journal update (programmatic RU)
        let c1 = convert_sequence_runs(&p1.runs, DirectionMode::LayoutFirst);
        update_journal_sequence(&p1, &c1, DirectionMode::LayoutFirst);

        // Second extraction must succeed (programmatic RU), enabling toggle-back.
        let p2 = take_last_sequence_payload()
            .expect("sequence payload after programmatic update expected");
        assert_eq!(p2.layout, LayoutTag::Ru);
        assert_eq!(p2.seq_text, "привет контроль");
        assert!(p2.suffix_text.is_empty());
        assert!(p2.prefix_runs.is_empty());
    }

    #[test]
    fn manual_sequence_toggles_roundtrip_twice() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let p1 = take_last_sequence_payload().expect("first sequence payload expected");
        let c1 = convert_sequence_runs(&p1.runs, DirectionMode::LayoutFirst);
        assert_ne!(c1, p1.seq_text);
        update_journal_sequence(&p1, &c1, DirectionMode::LayoutFirst);

        let p2 = take_last_sequence_payload().expect("second sequence payload expected");
        let c2 = convert_sequence_runs(&p2.runs, DirectionMode::LayoutFirst);
        update_journal_sequence(&p2, &c2, DirectionMode::LayoutFirst);

        let p3 = take_last_sequence_payload().expect("third sequence payload expected");
        assert_eq!(p3.layout, LayoutTag::En);
        assert_eq!(p3.seq_text, p1.seq_text);
    }

    #[test]
    fn last_sequence_payload_stops_on_layout_change_without_whitespace() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: "руддщ".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert!(payload.prefix_runs.is_empty());
        assert_eq!(payload.seq_text, "руддщ");
        assert_eq!(payload.layout, LayoutTag::Ru);
    }

    #[test]
    fn manual_sequence_toggles_roundtrip_for_capslock_style_uppercase_text() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_run(InputRun {
            text: "GHBDTN".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        });

        let p1 = take_last_sequence_payload().expect("first sequence payload expected");
        assert_eq!(p1.seq_text, "GHBDTN");
        let c1 = convert_with_layout_fallback(&p1.seq_text, &p1.layout);
        assert_eq!(c1, "ПРИВЕТ");
        update_journal_sequence(&p1, &c1, DirectionMode::LayoutFirst);

        let p2 = take_last_sequence_payload().expect("second sequence payload expected");
        assert_eq!(p2.layout, LayoutTag::Ru);
        assert_eq!(p2.seq_text, "ПРИВЕТ");
        let c2 = convert_with_layout_fallback(&p2.seq_text, &p2.layout);
        assert_eq!(c2, "GHBDTN");
        update_journal_sequence(&p2, &c2, DirectionMode::LayoutFirst);

        let p3 = take_last_sequence_payload().expect("third sequence payload expected");
        assert_eq!(p3.layout, LayoutTag::En);
        assert_eq!(p3.seq_text, "GHBDTN");
    }

    #[test]
    fn manual_sequence_toggles_roundtrip_for_shifted_capslock_style_lowercase_text() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_run(InputRun {
            text: "ghbdtn".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        });

        let p1 = take_last_sequence_payload().expect("first sequence payload expected");
        assert_eq!(p1.seq_text, "ghbdtn");
        let c1 = convert_with_layout_fallback(&p1.seq_text, &p1.layout);
        assert_eq!(c1, "привет");
        update_journal_sequence(&p1, &c1, DirectionMode::LayoutFirst);

        let p2 = take_last_sequence_payload().expect("second sequence payload expected");
        assert_eq!(p2.layout, LayoutTag::Ru);
        assert_eq!(p2.seq_text, "привет");
        let c2 = convert_with_layout_fallback(&p2.seq_text, &p2.layout);
        assert_eq!(c2, "ghbdtn");
        update_journal_sequence(&p2, &c2, DirectionMode::LayoutFirst);

        let p3 = take_last_sequence_payload().expect("third sequence payload expected");
        assert_eq!(p3.layout, LayoutTag::En);
        assert_eq!(p3.seq_text, "ghbdtn");
    }

    #[test]
    fn manual_sequence_preserves_suffix_after_programmatic_roundtrip() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: "  ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(payload.suffix_text, "  ");
        let converted = convert_sequence_runs(&payload.runs, DirectionMode::LayoutFirst);
        update_journal_sequence(&payload, &converted, DirectionMode::LayoutFirst);

        let next = take_last_sequence_payload().expect("sequence payload after update expected");
        assert_eq!(next.layout, LayoutTag::Ru);
        assert_eq!(next.seq_text, "привет контроль");
        assert_eq!(next.suffix_text, "  ");
    }

    #[test]
    fn exact_ghbdtn_smart_then_normal_double_shift_roundtrips_last_word() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_run(InputRun {
            text: "ghbdtn".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        });

        let smart_payload = take_last_word_payload().expect("smart word payload expected");
        assert!(!smart::text_looks_correct(&smart_payload.run.text));
        let smart_converted = convert_with_mode(
            &smart_payload.run.text,
            &smart_payload.run.layout,
            DirectionMode::TextFirst,
        );
        assert_eq!(smart_converted, "привет");
        update_journal(&smart_payload, &smart_converted);

        let normal_payload = take_last_word_payload().expect("normal word payload expected");
        assert_eq!(normal_payload.run.text, "привет");
        assert!(smart::text_looks_correct(&normal_payload.run.text));
        let normal_converted = convert_with_mode(
            &normal_payload.run.text,
            &normal_payload.run.layout,
            DirectionMode::LayoutFirst,
        );
        assert_eq!(normal_converted, "ghbdtn");
    }

    #[test]
    fn update_journal_sequence_preserves_whitespace_tokenization() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        let converted = convert_sequence_runs(&payload.runs, DirectionMode::LayoutFirst);
        update_journal_sequence(&payload, &converted, DirectionMode::LayoutFirst);

        let (runs, suffix) =
            ring_buffer::take_last_programmatic_sequence_with_suffix().expect("seq expected");
        assert!(suffix.is_empty());
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].text, "привет");
        assert_eq!(runs[0].kind, RunKind::Text);
        assert_eq!(runs[0].origin, RunOrigin::Programmatic);
        assert_eq!(runs[0].layout, LayoutTag::Ru);
        assert_eq!(runs[1].text, " ");
        assert_eq!(runs[1].kind, RunKind::Whitespace);
        assert_eq!(runs[2].text, "контроль");
        assert_eq!(runs[2].kind, RunKind::Text);
    }

    #[test]
    fn last_sequence_payload_includes_previous_english_word_in_same_sequence() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "hello".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert!(payload.prefix_runs.is_empty());
        assert_eq!(payload.seq_text, "hello ghbdtn");
        assert_eq!(payload.layout, LayoutTag::En);
    }

    #[test]
    fn last_sequence_payload_includes_previous_russian_word_in_same_sequence() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "привет".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "руддщ".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert!(payload.prefix_runs.is_empty());
        assert_eq!(payload.seq_text, "привет руддщ");
        assert_eq!(payload.layout, LayoutTag::Ru);
    }

    #[test]
    fn update_journal_sequence_converts_full_same_layout_sequence() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "hello".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        update_journal_sequence(&payload, "руддщ привет", DirectionMode::LayoutFirst);

        let runs = ring_buffer::runs_snapshot();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].text, "руддщ");
        assert_eq!(runs[0].layout, LayoutTag::Ru);
        assert_eq!(runs[0].origin, RunOrigin::Programmatic);
        assert_eq!(runs[1].text, " ");
        assert_eq!(runs[1].kind, RunKind::Whitespace);
        assert_eq!(runs[2].text, "привет");
        assert_eq!(runs[2].layout, LayoutTag::Ru);
        assert_eq!(runs[2].origin, RunOrigin::Programmatic);
    }

    #[test]
    fn last_sequence_payload_keeps_multiple_words_in_single_layout_sequence() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "руддщ".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "вапр".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "dfgdfgh".to_string(),
                layout: LayoutTag::Ru,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(payload.layout, LayoutTag::Ru);
        assert_eq!(payload.seq_text, "руддщ вапр dfgdfgh");
        assert!(payload.prefix_runs.is_empty());
    }

    #[test]
    fn mixed_script_sequence_roundtrip_current_behavior_is_explicit() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_run(InputRun {
            text: "fdghdrfghdfgh dfgh dfgh dfgh вапвапр".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        });

        let p1 = take_last_sequence_payload().expect("first sequence payload expected");
        assert_eq!(p1.seq_text, "fdghdrfghdfgh dfgh dfgh dfgh вапвапр");
        assert_eq!(p1.layout, LayoutTag::En);
        let c1 = convert_with_layout_fallback(&p1.seq_text, &p1.layout);
        assert_eq!(c1, "авпрвкапрвапр вапр вапр вапр вапвапр");
        update_journal_sequence(&p1, &c1, DirectionMode::LayoutFirst);

        let p2 = take_last_sequence_payload().expect("second sequence payload expected");
        assert_eq!(p2.seq_text, "авпрвкапрвапр вапр вапр вапр вапвапр");
        assert_eq!(p2.layout, LayoutTag::Ru);
        let c2 = convert_with_layout_fallback(&p2.seq_text, &p2.layout);
        assert_eq!(c2, "fdghdrfghdfgh dfgh dfgh dfgh dfgdfgh");
    }

    #[test]
    fn last_word_after_sequence_update_targets_only_final_programmatic_word() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let seq = take_last_sequence_payload().expect("sequence payload expected");
        let converted = convert_sequence_runs(&seq.runs, DirectionMode::LayoutFirst);
        update_journal_sequence(&seq, &converted, DirectionMode::LayoutFirst);

        let word = take_last_word_payload().expect("word payload expected");
        assert_eq!(word.run.text, "контроль");
        assert_eq!(word.run.layout, LayoutTag::Ru);
        assert_eq!(word.run.origin, RunOrigin::Programmatic);
    }

    #[test]
    fn last_sequence_after_last_word_update_preserves_full_phrase_buffer() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let word = take_last_word_payload().expect("word payload expected");
        let converted = convert_with_layout_fallback(&word.run.text, &word.run.layout);
        update_journal(&word, &converted);

        let seq = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(seq.seq_text, "ghbdtn контроль");
        assert_eq!(seq.layout, LayoutTag::Ru);
        assert_eq!(
            convert_sequence_runs(&seq.runs, DirectionMode::LayoutFirst),
            "привет rjynhjkm"
        );
        assert!(seq.prefix_runs.is_empty());
    }

    #[test]
    fn new_physical_tail_after_sequence_update_does_not_read_back_into_previous_sequence() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "rjynhjkm".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let seq = take_last_sequence_payload().expect("sequence payload expected");
        let converted = convert_sequence_runs(&seq.runs, DirectionMode::LayoutFirst);
        update_journal_sequence(&seq, &converted, DirectionMode::LayoutFirst);

        ring_buffer::push_run(InputRun {
            text: "asdf".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        });

        let tail = take_last_sequence_payload().expect("tail payload expected");
        assert_eq!(tail.seq_text, "asdf");
        assert_eq!(tail.layout, LayoutTag::En);
        assert!(tail.prefix_runs.is_empty());
    }

    #[test]
    fn exact_phrase_hfp_ldf_nhb_converts_as_full_last_sequence() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "hfp".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "ldf".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "nhb".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(payload.seq_text, "hfp ldf nhb");
        assert_eq!(
            convert_sequence_runs(&payload.runs, DirectionMode::LayoutFirst),
            "раз два три"
        );
    }

    #[test]
    fn exact_phrase_hfp_ldf_nhb_stays_full_sequence_after_last_word_update() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "hfp".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "ldf".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "nhb".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let word = take_last_word_payload().expect("word payload expected");
        let converted_word = convert_with_layout_fallback(&word.run.text, &word.run.layout);
        assert_eq!(converted_word, "три");
        update_journal(&word, &converted_word);

        let payload = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(payload.seq_text, "hfp ldf три");
        assert_eq!(
            convert_sequence_runs(&payload.runs, DirectionMode::LayoutFirst),
            "раз два nhb"
        );
    }

    #[test]
    fn shift_shift_then_alt_alt_updates_full_phrase_after_last_word_update() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "hfp".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "ldf".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: " ".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
            InputRun {
                text: "nhb".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
        ]);

        let word = take_last_word_payload().expect("word payload expected");
        let converted_word = convert_with_layout_fallback(&word.run.text, &word.run.layout);
        assert_eq!(converted_word, "три");
        update_journal(&word, &converted_word);

        let sequence = take_last_sequence_payload().expect("sequence payload expected");
        assert_eq!(sequence.seq_text, "hfp ldf три");
        let converted_sequence = convert_sequence_runs(&sequence.runs, DirectionMode::LayoutFirst);
        assert_eq!(converted_sequence, "раз два nhb");
        update_journal_sequence(&sequence, &converted_sequence, DirectionMode::LayoutFirst);

        let runs = ring_buffer::runs_snapshot();
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            "раз два nhb"
        );
        assert!(runs.iter().all(|run| run.origin == RunOrigin::Programmatic));
        assert_eq!(runs.last().map(|run| run.layout), Some(LayoutTag::En));
    }

    #[test]
    fn suffix_spaces_only_is_false_for_newline_suffix() {
        let _guard = test_lock();
        ring_buffer::invalidate();
        ring_buffer::push_runs([
            InputRun {
                text: "ghbdtn".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Text,
            },
            InputRun {
                text: "\n".to_string(),
                layout: LayoutTag::En,
                origin: RunOrigin::Physical,
                kind: RunKind::Whitespace,
            },
        ]);
        let payload = take_last_word_payload().expect("payload expected");
        assert!(payload.suffix_has_newline);
        assert!(!payload.suffix_spaces_only);
    }
    #[test]
    fn autoconvert_converts_mistyped_russian_layout_word() {
        let _guard = test_lock();
        let detector = detector_ru_en();
        let word = "ghbdtn";
        let converted = convert_with_layout_fallback(word, &LayoutTag::En);
        assert_eq!(converted, "привет");
        match should_autoconvert_word(&detector, word, &converted) {
            Ok(()) => {}
            Err(reason) => {
                panic!("should autoconvert mistyped Russian layout word, got Err({reason:?})");
            }
        }
    }
}
