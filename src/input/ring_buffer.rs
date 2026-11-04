#[cfg(windows)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::MutexGuard;
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

#[cfg(windows)]
#[path = "layout_decode.rs"]
mod layout_decode;

#[cfg(all(test, windows))]
use windows::Win32::UI::Input::KeyboardAndMouse::ToUnicodeEx;
#[cfg(windows)]
use windows::Win32::{
    System::SystemInformation::GetTickCount64,
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, GetKeyState, GetKeyboardLayout, GetKeyboardState, HKL, VIRTUAL_KEY,
            VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
            VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_MENU, VK_NEXT, VK_PRIOR,
            VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_SHIFT, VK_TAB, VK_UP,
        },
        WindowsAndMessaging::{
            GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
            KBDLLHOOKSTRUCT, LLKHF_INJECTED,
        },
    },
};

static JOURNAL: OnceLock<Mutex<InputJournal>> = OnceLock::new();
const DEFAULT_CAPACITY_CHARS: usize = 4096;
#[cfg(windows)]
static JOURNAL_CACHE: OnceLock<Mutex<HashMap<FocusCacheKey, CachedJournal>>> = OnceLock::new();

#[cfg(test)]
static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(windows)]
const FOREGROUND_CACHE_TTL_MS: u64 = 2 * 60 * 1000;

#[cfg(windows)]
fn allow_injected_input_for_e2e() -> bool {
    cfg!(debug_assertions) && std::env::var_os("RUST_SWITCHER_E2E_ALLOW_INJECTED").is_some()
}

fn journal() -> &'static Mutex<InputJournal> {
    JOURNAL.get_or_init(|| Mutex::new(InputJournal::new(DEFAULT_CAPACITY_CHARS)))
}

#[cfg(test)]
pub fn test_guard() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn with_journal_mut<R>(f: impl FnOnce(&mut InputJournal) -> R) -> R {
    let mut guard = match journal().lock() {
        Ok(g) => g,
        Err(poison) => {
            #[cfg(debug_assertions)]
            tracing::warn!("input journal mutex was poisoned; continuing with inner value");
            poison.into_inner()
        }
    };
    f(&mut guard)
}

#[cfg(windows)]
fn journal_cache() -> &'static Mutex<HashMap<FocusCacheKey, CachedJournal>> {
    JOURNAL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(windows)]
fn with_journal_cache_mut<R>(f: impl FnOnce(&mut HashMap<FocusCacheKey, CachedJournal>) -> R) -> R {
    let mut guard = match journal_cache().lock() {
        Ok(g) => g,
        Err(poison) => {
            #[cfg(debug_assertions)]
            tracing::warn!("input journal cache mutex was poisoned; continuing with inner value");
            poison.into_inner()
        }
    };
    f(&mut guard)
}

#[cfg(windows)]
fn prune_stale_foreground_cache(cache: &mut HashMap<FocusCacheKey, CachedJournal>, now_ms: u64) {
    cache.retain(|_, cached| now_ms.saturating_sub(cached.updated_ms) <= FOREGROUND_CACHE_TTL_MS);
}

#[cfg(any(test, windows))]
fn with_journal<R>(f: impl FnOnce(&InputJournal) -> R) -> R {
    let guard = match journal().lock() {
        Ok(g) => g,
        Err(poison) => {
            #[cfg(debug_assertions)]
            tracing::warn!("input journal mutex was poisoned; continuing with inner value");
            poison.into_inner()
        }
    };
    f(&guard)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LayoutTag {
    /// Complete Windows layout identity, including variants of the same language.
    Native(usize),
    Ru,
    En,
    Unknown,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RunOrigin {
    Physical,
    Programmatic,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RunKind {
    Text,
    Whitespace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputRun {
    pub text: String,
    pub layout: LayoutTag,
    pub origin: RunOrigin,
    pub kind: RunKind,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct CaretRectSignature {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FocusCacheKey {
    foreground_hwnd: isize,
    focus_hwnd: isize,
    caret_hwnd: isize,
    caret_rect: CaretRectSignature,
}

#[cfg(windows)]
#[derive(Clone, Debug)]
struct CachedJournal {
    journal: InputJournal,
    updated_ms: u64,
}

#[cfg(windows)]
fn same_focus_identity(a: FocusCacheKey, b: FocusCacheKey) -> bool {
    // A caret can be recreated after the first glyph without moving keyboard focus.
    // Treating its owner as a new editor discards that glyph from the live journal.
    // The complete caret signature still guards restoration after a focus switch.
    a.foreground_hwnd == b.foreground_hwnd && a.focus_hwnd == b.focus_hwnd
}

#[derive(Clone, Debug, Default)]
struct InputJournal {
    runs: VecDeque<InputRun>,
    cap_chars: usize,
    total_chars: usize,
    caret_from_end: usize,
    last_token_autoconverted: bool,
    capture_unavailable: bool,
    pending_dead_key: bool,
    // Shrinking the buffer later cannot make a discarded prefix known again.
    prefix_truncated: bool,
    #[cfg(windows)]
    last_fg_hwnd: isize,
    #[cfg(windows)]
    last_focus_key: Option<FocusCacheKey>,
}

impl InputJournal {
    const fn new(cap_chars: usize) -> Self {
        Self {
            runs: VecDeque::new(),
            cap_chars,
            total_chars: 0,
            caret_from_end: 0,
            last_token_autoconverted: false,
            capture_unavailable: false,
            pending_dead_key: false,
            prefix_truncated: false,
            #[cfg(windows)]
            last_fg_hwnd: 0,
            #[cfg(windows)]
            last_focus_key: None,
        }
    }

    #[cfg(any(test, windows))]
    fn clear(&mut self) {
        self.runs.clear();
        self.total_chars = 0;
        self.caret_from_end = 0;
        self.last_token_autoconverted = false;
        self.capture_unavailable = false;
        self.pending_dead_key = false;
        self.prefix_truncated = false;
    }

    fn append_segment_end(
        &mut self,
        text: &str,
        layout: LayoutTag,
        origin: RunOrigin,
        kind: RunKind,
    ) {
        if text.is_empty() {
            return;
        }

        if let Some(last) = self.runs.back_mut()
            && last.layout == layout
            && last.origin == origin
            && last.kind == kind
        {
            last.text.push_str(text);
            self.total_chars += text.chars().count();
            self.enforce_cap_chars();
            return;
        }

        self.total_chars += text.chars().count();
        self.runs.push_back(InputRun {
            text: text.to_string(),
            layout,
            origin,
            kind,
        });
        self.enforce_cap_chars();
    }

    fn insert_run_before_caret(&mut self, run: InputRun) {
        if run.text.is_empty() {
            return;
        }

        let caret_from_end = self.caret_from_end.min(self.total_chars);
        let suffix_runs = self.detach_suffix(caret_from_end);

        self.append_segment_end(&run.text, run.layout, run.origin, run.kind);
        self.restore_suffix_after_caret(suffix_runs);
    }

    fn restore_suffix_after_caret(&mut self, suffix_runs: Vec<InputRun>) {
        let suffix_len = suffix_runs.iter().map(|run| run.text.chars().count()).sum();

        for run in suffix_runs {
            self.append_segment_end(&run.text, run.layout, run.origin, run.kind);
        }

        self.caret_from_end = suffix_len;
    }

    #[cfg(any(test, windows))]
    fn push_text_internal(&mut self, text: &str, layout: LayoutTag, origin: RunOrigin) {
        if text.is_empty() || self.capture_unavailable {
            return;
        }

        // Segment the input without intermediate allocations by slicing `text` at kind boundaries.
        // This keeps the journal cheap for frequent short inputs (single key presses) and also
        // efficient for programmatic multi-character inserts.
        let mut start = 0usize;
        let mut current_kind: Option<RunKind> = None;

        for (i, ch) in text.char_indices() {
            let kind = if ch.is_whitespace() {
                RunKind::Whitespace
            } else {
                RunKind::Text
            };

            match current_kind {
                None => {
                    start = i;
                    current_kind = Some(kind);
                }
                Some(k) if k == kind => {}
                Some(k) => {
                    self.insert_run_before_caret(InputRun {
                        text: text[start..i].to_string(),
                        layout,
                        origin,
                        kind: k,
                    });
                    start = i;
                    current_kind = Some(kind);
                }
            }
        }

        if let Some(kind) = current_kind {
            self.insert_run_before_caret(InputRun {
                text: text[start..].to_string(),
                layout,
                origin,
                kind,
            });
        }
    }

    fn push_run(&mut self, run: InputRun) {
        self.insert_run_before_caret(run);
    }

    fn push_runs(&mut self, runs: impl IntoIterator<Item = InputRun>) {
        for run in runs {
            self.push_run(run);
        }
    }

    fn enforce_cap_chars(&mut self) {
        while self.total_chars > self.cap_chars {
            let mut remove_front_run = false;

            if let Some(front) = self.runs.front_mut() {
                self.prefix_truncated = true;
                if let Some((idx, _)) = front.text.char_indices().nth(1) {
                    front.text.drain(..idx);
                } else {
                    front.text.clear();
                    remove_front_run = true;
                }
                self.total_chars = self.total_chars.saturating_sub(1);

                if front.text.is_empty() {
                    remove_front_run = true;
                }
            } else {
                self.total_chars = 0;
                break;
            }

            if remove_front_run {
                let _ = self.runs.pop_front();
            }
        }

        self.caret_from_end = self.caret_from_end.min(self.total_chars);
    }

    #[cfg(any(test, windows))]
    fn backspace(&mut self) {
        let caret_from_end = self.caret_from_end.min(self.total_chars);
        let suffix_runs = self.detach_suffix(caret_from_end);
        let mut pop_last = false;

        if let Some(last) = self.runs.back_mut()
            && let Some((idx, _)) = last.text.char_indices().last()
        {
            last.text.drain(idx..);
            self.total_chars = self.total_chars.saturating_sub(1);
            if last.text.is_empty() {
                pop_last = true;
            }
        }

        if pop_last {
            let _ = self.runs.pop_back();
        }

        self.restore_suffix_after_caret(suffix_runs);
    }

    #[cfg(any(test, windows))]
    fn move_caret_left(&mut self) {
        self.caret_from_end = self.caret_from_end.saturating_add(1).min(self.total_chars);
    }

    #[cfg(any(test, windows))]
    fn move_caret_right(&mut self) {
        self.caret_from_end = self.caret_from_end.saturating_sub(1);
    }

    #[cfg(windows)]
    fn move_caret_end(&mut self) {
        self.caret_from_end = 0;
    }

    fn detach_suffix(&mut self, count: usize) -> Vec<InputRun> {
        let mut remaining = count.min(self.total_chars);
        let mut suffix_rev = Vec::new();

        while remaining > 0 {
            let Some(mut run) = self.runs.pop_back() else {
                self.total_chars = 0;
                break;
            };

            let run_len = run.text.chars().count();
            if run_len <= remaining {
                self.total_chars = self.total_chars.saturating_sub(run_len);
                remaining -= run_len;
                suffix_rev.push(run);
                continue;
            }

            let split_chars = run_len - remaining;
            let Some((split_idx, _)) = run.text.char_indices().nth(split_chars) else {
                self.runs.push_back(run);
                break;
            };
            let suffix_text = run.text.split_off(split_idx);
            let suffix_run = InputRun {
                text: suffix_text,
                layout: run.layout,
                origin: run.origin,
                kind: run.kind,
            };
            self.runs.push_back(run);
            self.total_chars = self.total_chars.saturating_sub(remaining);
            suffix_rev.push(suffix_run);
            remaining = 0;
        }

        suffix_rev.reverse();
        suffix_rev
    }

    #[cfg(windows)]
    fn invalidate_if_foreground_changed(&mut self) {
        let fg = unsafe { GetForegroundWindow() };
        let raw = fg.0 as isize;
        let key = current_focus_cache_key(fg);
        self.switch_foreground(raw, key, unsafe { GetTickCount64() });
    }

    #[cfg(windows)]
    fn accept_decoded_focus(&mut self, captured: FocusCacheKey) -> bool {
        if self
            .last_focus_key
            .is_some_and(|current| same_focus_identity(captured, current))
        {
            return true;
        }
        // A worker result belongs to the field in which its key was captured.
        // Do not insert it into a new field after a focus switch during decoding.
        self.clear();
        self.capture_unavailable = true;
        layout_decode::reset();
        false
    }

    #[cfg(windows)]
    fn switch_foreground(&mut self, raw: isize, key: Option<FocusCacheKey>, now_ms: u64) {
        if raw == self.last_fg_hwnd && key == self.last_focus_key {
            return;
        }
        let unknown_window_input = self.last_fg_hwnd == 0 && self.capture_unavailable;

        if raw == self.last_fg_hwnd
            && match (self.last_focus_key, key) {
                (Some(prev), Some(next)) => same_focus_identity(prev, next),
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => true,
            }
        {
            self.last_focus_key = key;
            return;
        }

        // Decoder composition is discarded when keyboard focus changes. Never
        // restore a cached journal as if its abandoned dead key could resume.
        if self.pending_dead_key {
            self.clear();
            self.capture_unavailable = true;
            layout_decode::reset();
        }

        with_journal_cache_mut(|cache| {
            prune_stale_foreground_cache(cache, now_ms);

            if self.last_fg_hwnd != 0
                && let Some(last_key) = self.last_focus_key
            {
                cache.insert(
                    last_key,
                    CachedJournal {
                        journal: self.clone(),
                        updated_ms: now_ms,
                    },
                );
            }

            if raw == 0 {
                self.clear();
                self.last_fg_hwnd = 0;
                self.last_focus_key = None;
                return;
            }

            if let Some(key) = key
                && let Some(cached) = cache.remove(&key)
            {
                *self = cached.journal;
                if unknown_window_input {
                    self.clear();
                    self.capture_unavailable = true;
                }
                self.last_fg_hwnd = raw;
                self.last_focus_key = Some(key);
                self.enforce_cap_chars();
                return;
            }

            self.clear();
            self.capture_unavailable = unknown_window_input;
            self.last_fg_hwnd = raw;
            self.last_focus_key = key;
        });
    }

    #[cfg(any(test, windows))]
    fn last_char(&self) -> Option<char> {
        self.chars_before_caret_rev().next()
    }

    #[cfg(any(test, windows))]
    fn prev_char_before_last(&self) -> Option<char> {
        self.chars_before_caret_rev().nth(1)
    }

    #[cfg(any(test, windows))]
    fn chars_before_caret_rev(&self) -> impl Iterator<Item = char> + '_ {
        let prefix_len = self.total_chars.saturating_sub(self.caret_from_end);
        self.runs
            .iter()
            .flat_map(|run| run.text.chars())
            .take(prefix_len)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
    }

    fn take_last_layout_run_with_suffix(&mut self) -> Option<(InputRun, Vec<InputRun>)> {
        let caret_from_end = self.caret_from_end.min(self.total_chars);
        let suffix_after_caret = self.detach_suffix(caret_from_end);
        self.caret_from_end = 0;

        let mut suffix_runs = self.pop_suffix_whitespace();

        let result = if self.runs.back().is_none_or(|run| run.kind != RunKind::Text)
            || (self.prefix_truncated && self.runs.len() == 1)
        {
            self.restore_suffix(&mut suffix_runs);
            None
        } else if let Some(run) = self.runs.pop_back() {
            self.total_chars = self.total_chars.saturating_sub(run.text.chars().count());
            suffix_runs.reverse();
            Some((run, suffix_runs))
        } else {
            self.restore_suffix(&mut suffix_runs);
            None
        };

        self.restore_suffix_after_caret(suffix_after_caret);
        result
    }

    #[cfg(test)]
    fn take_last_layout_sequence_with_suffix(&mut self) -> Option<(Vec<InputRun>, Vec<InputRun>)> {
        let caret_from_end = self.caret_from_end.min(self.total_chars);
        let suffix_after_caret = self.detach_suffix(caret_from_end);
        self.caret_from_end = 0;

        let mut suffix_runs = self.pop_suffix_whitespace();

        let result = if self.runs.back().is_none_or(|run| run.kind != RunKind::Text) {
            self.restore_suffix(&mut suffix_runs);
            None
        } else {
            let Some(last) = self.runs.back() else {
                self.restore_suffix(&mut suffix_runs);
                self.restore_suffix_after_caret(suffix_after_caret);
                return None;
            };
            let target_layout = last.layout;
            let target_origin = last.origin;
            let mut seq_rev: Vec<InputRun> = Vec::new();
            while let Some(run) = self.runs.back() {
                if run.layout != target_layout || run.origin != target_origin {
                    break;
                }
                let Some(run) = self.runs.pop_back() else {
                    break;
                };
                self.total_chars = self.total_chars.saturating_sub(run.text.chars().count());
                seq_rev.push(run);
            }

            if seq_rev.is_empty() || (self.prefix_truncated && self.runs.is_empty()) {
                self.restore_suffix(&mut seq_rev);
                self.restore_suffix(&mut suffix_runs);
                None
            } else {
                seq_rev.reverse();
                suffix_runs.reverse();
                Some((seq_rev, suffix_runs))
            }
        };

        self.restore_suffix_after_caret(suffix_after_caret);
        result
    }

    #[cfg(test)]
    fn take_last_programmatic_sequence_with_suffix(
        &mut self,
    ) -> Option<(Vec<InputRun>, Vec<InputRun>)> {
        let caret_from_end = self.caret_from_end.min(self.total_chars);
        let suffix_after_caret = self.detach_suffix(caret_from_end);
        self.caret_from_end = 0;

        let mut suffix_runs = self.pop_suffix_whitespace();

        let result = if self
            .runs
            .back()
            .is_none_or(|run| run.origin != RunOrigin::Programmatic)
        {
            self.restore_suffix(&mut suffix_runs);
            None
        } else {
            let mut seq_rev: Vec<InputRun> = Vec::new();
            while let Some(run) = self.runs.back() {
                if run.origin != RunOrigin::Programmatic {
                    break;
                }
                let Some(run) = self.runs.pop_back() else {
                    break;
                };
                self.total_chars = self.total_chars.saturating_sub(run.text.chars().count());
                seq_rev.push(run);
            }

            if seq_rev.is_empty() || (self.prefix_truncated && self.runs.is_empty()) {
                self.restore_suffix(&mut seq_rev);
                self.restore_suffix(&mut suffix_runs);
                None
            } else {
                seq_rev.reverse();
                suffix_runs.reverse();
                Some((seq_rev, suffix_runs))
            }
        };

        self.restore_suffix_after_caret(suffix_after_caret);
        result
    }

    fn take_last_sequence_with_suffix(&mut self) -> Option<(Vec<InputRun>, Vec<InputRun>)> {
        let caret_from_end = self.caret_from_end.min(self.total_chars);
        let suffix_after_caret = self.detach_suffix(caret_from_end);
        self.caret_from_end = 0;

        let result = self.take_last_sequence_at_end();

        // The conversion deletes only text before the caret. Keep the untouched
        // tail in place on both success and rejection, including truncated input.
        self.restore_suffix_after_caret(suffix_after_caret);
        result
    }

    fn take_last_sequence_at_end(&mut self) -> Option<(Vec<InputRun>, Vec<InputRun>)> {
        let mut suffix_runs = self.pop_suffix_whitespace();

        if self.runs.back().is_none_or(|run| run.kind != RunKind::Text) {
            self.restore_suffix(&mut suffix_runs);
            return None;
        }

        let mut seq_rev = Vec::new();
        let last = self.runs.back()?;

        if last.origin == RunOrigin::Programmatic {
            while self
                .runs
                .back()
                .is_some_and(|run| run.origin == RunOrigin::Programmatic)
            {
                let run = self.runs.pop_back()?;
                self.total_chars = self.total_chars.saturating_sub(run.text.chars().count());
                seq_rev.push(run);
            }

            let prefix_layout = self
                .runs
                .iter()
                .rev()
                .find(|run| run.origin == RunOrigin::Physical && run.kind == RunKind::Text)
                .map(|run| run.layout);

            if let Some(layout) = prefix_layout {
                while self
                    .runs
                    .back()
                    .is_some_and(|run| run.origin == RunOrigin::Physical && run.layout == layout)
                {
                    let run = self.runs.pop_back()?;
                    self.total_chars = self.total_chars.saturating_sub(run.text.chars().count());
                    seq_rev.push(run);
                }
            }
        } else {
            let target_layout = last.layout;
            let target_origin = last.origin;
            while let Some(run) = self.runs.back() {
                if run.layout != target_layout || run.origin != target_origin {
                    break;
                }
                let run = self.runs.pop_back()?;
                self.total_chars = self.total_chars.saturating_sub(run.text.chars().count());
                seq_rev.push(run);
            }
        }

        if seq_rev.is_empty() || (self.prefix_truncated && self.runs.is_empty()) {
            // A retained tail is not the full typed sequence. Leave both the
            // document and journal intact instead of converting only its suffix.
            self.restore_suffix(&mut seq_rev);
            self.restore_suffix(&mut suffix_runs);
            return None;
        }

        seq_rev.reverse();
        suffix_runs.reverse();
        Some((seq_rev, suffix_runs))
    }

    fn pop_suffix_whitespace(&mut self) -> Vec<InputRun> {
        let mut suffix_runs: Vec<InputRun> = Vec::new();
        while self
            .runs
            .back()
            .is_some_and(|run| run.kind == RunKind::Whitespace)
        {
            let Some(run) = self.runs.pop_back() else {
                break;
            };
            self.total_chars = self.total_chars.saturating_sub(run.text.chars().count());
            suffix_runs.push(run);
        }
        suffix_runs
    }

    fn restore_suffix(&mut self, suffix_runs: &mut Vec<InputRun>) {
        // `suffix_runs` is expected to be in reverse order (from repeated `pop_back`).
        while let Some(run) = suffix_runs.pop() {
            self.total_chars += run.text.chars().count();
            self.runs.push_back(run);
        }
    }
}

#[cfg(windows)]
fn current_focus_cache_key(foreground: windows::Win32::Foundation::HWND) -> Option<FocusCacheKey> {
    if foreground.0.is_null() {
        return None;
    }

    let tid = unsafe { GetWindowThreadProcessId(foreground, None) };
    if tid == 0 {
        return None;
    }

    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };

    if unsafe { GetGUIThreadInfo(tid, &mut info) }.is_err() || info.hwndFocus.0.is_null() {
        return None;
    }

    Some(FocusCacheKey {
        foreground_hwnd: foreground.0 as isize,
        focus_hwnd: info.hwndFocus.0 as isize,
        caret_hwnd: info.hwndCaret.0 as isize,
        caret_rect: CaretRectSignature {
            left: info.rcCaret.left,
            top: info.rcCaret.top,
            right: info.rcCaret.right,
            bottom: info.rcCaret.bottom,
        },
    })
}

#[cfg(windows)]
#[derive(Debug)]
struct DecodedText {
    text: String,
    layout: LayoutTag,
    focus: FocusCacheKey,
}

#[cfg(windows)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct KeyboardStateOverrides {
    shift_down: bool,
    left_shift_down: bool,
    right_shift_down: bool,
    caps_lock_on: bool,
}

#[cfg(windows)]
pub fn layout_tag_from_hkl(hkl: HKL) -> LayoutTag {
    let hkl_raw = hkl.0 as usize;

    if hkl_raw == 0 {
        return LayoutTag::Unknown;
    }

    LayoutTag::Native(hkl_raw)
}

#[cfg(windows)]
pub fn warm_decoder() {
    let _ = layout_decode::warm();
}

#[cfg(windows)]
fn current_foreground_layout_tag() -> LayoutTag {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        return LayoutTag::Unknown;
    }

    let tid = unsafe { GetWindowThreadProcessId(fg, None) };
    let hkl = unsafe { GetKeyboardLayout(tid) };
    layout_tag_from_hkl(hkl)
}

pub fn mark_last_token_autoconverted() {
    with_journal_mut(|j| j.last_token_autoconverted = true);
}

#[cfg(any(test, windows))]
#[must_use]
pub fn last_token_autoconverted() -> bool {
    with_journal(|j| j.last_token_autoconverted)
}

#[cfg(windows)]
fn mods_ctrl_or_alt_down() -> bool {
    // Keep this module independent from `crate::platform` so it can be built from the minimal lib target.
    // VK_CONTROL = 0x11, VK_MENU (Alt) = 0x12.
    let ctrl = unsafe { GetAsyncKeyState(0x11) }.cast_unsigned();
    let alt = unsafe { GetAsyncKeyState(0x12) }.cast_unsigned();
    (ctrl & 0x8000) != 0 || (alt & 0x8000) != 0
}

#[cfg(windows)]
fn is_modifier_vk(vk: VIRTUAL_KEY) -> bool {
    matches!(vk.0, 0x10..=0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

#[cfg(windows)]
fn altgr_down() -> bool {
    key_is_down(VK_RMENU) && !key_is_down(VK_LMENU) && !key_is_down(VK_RCONTROL)
}

#[cfg(windows)]
fn should_clear_for_ctrl_alt_combo(vk: VIRTUAL_KEY, ctrl_or_alt_down: bool) -> bool {
    ctrl_or_alt_down && !is_modifier_vk(vk)
}

#[cfg(windows)]
fn key_is_down(vk: VIRTUAL_KEY) -> bool {
    let value = unsafe { GetAsyncKeyState(i32::from(vk.0)) }.cast_unsigned();
    (value & 0x8000) != 0
}

#[cfg(windows)]
fn key_is_toggled(vk: VIRTUAL_KEY) -> bool {
    let value = unsafe { GetKeyState(i32::from(vk.0)) }.cast_unsigned();
    (value & 0x0001) != 0
}

#[cfg(windows)]
fn set_key_down_state(state: &mut [u8; 256], vk: VIRTUAL_KEY, is_down: bool) {
    let idx = usize::from(vk.0);
    if idx >= state.len() {
        return;
    }

    if is_down {
        state[idx] |= 0x80;
    } else {
        state[idx] &= !0x80;
    }
}

#[cfg(windows)]
fn set_key_toggle_state(state: &mut [u8; 256], vk: VIRTUAL_KEY, is_toggled: bool) {
    let idx = usize::from(vk.0);
    if idx >= state.len() {
        return;
    }

    if is_toggled {
        state[idx] |= 0x01;
    } else {
        state[idx] &= !0x01;
    }
}

#[cfg(windows)]
fn current_keyboard_state_overrides() -> KeyboardStateOverrides {
    KeyboardStateOverrides {
        shift_down: key_is_down(VK_SHIFT),
        left_shift_down: key_is_down(VK_LSHIFT),
        right_shift_down: key_is_down(VK_RSHIFT),
        caps_lock_on: key_is_toggled(VK_CAPITAL),
    }
}

#[cfg(windows)]
fn apply_keyboard_state_overrides(state: &mut [u8; 256], overrides: KeyboardStateOverrides) {
    // LL-hook decoding runs before the target thread's keyboard state is fully reflected in our
    // thread-local `GetKeyboardState` snapshot. Patch in the physical modifier/toggle state that
    // affects character case before interpreting the layout's character tables.
    set_key_down_state(state, VK_SHIFT, overrides.shift_down);
    set_key_down_state(state, VK_LSHIFT, overrides.left_shift_down);
    set_key_down_state(state, VK_RSHIFT, overrides.right_shift_down);
    set_key_toggle_state(state, VK_CAPITAL, overrides.caps_lock_on);

    // record_keydown filters physically held Ctrl/Alt shortcuts before decoding printable input.
    // Our thread's snapshot can still contain an old shortcut from a previously focused window.
    // Leaving those bits set can select a control character or no character at all.
    for vk in [
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
    ] {
        set_key_down_state(state, vk, false);
    }
}

#[cfg(windows)]
fn decode_typed_text(kb: &KBDLLHOOKSTRUCT, vk: VIRTUAL_KEY) -> Option<DecodedText> {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null() {
        mark_capture_unavailable();
        return None;
    }

    let tid = unsafe { GetWindowThreadProcessId(fg, None) };
    let hkl = unsafe { GetKeyboardLayout(tid) };
    let layout = layout_tag_from_hkl(hkl);

    let Some(focus) = current_focus_cache_key(fg) else {
        mark_capture_unavailable();
        return None;
    };

    let mut state = [0u8; 256];
    if unsafe { GetKeyboardState(&mut state) }.is_err() {
        mark_capture_unavailable();
        return None;
    }

    apply_keyboard_state_overrides(&mut state, current_keyboard_state_overrides());
    let ctrl_or_alt_held = mods_ctrl_or_alt_down();
    let altgr_held = altgr_down();
    // AltGr is printable input on many layouts. Restore only its *physical*
    // state after clearing stale Ctrl/Alt bits; ordinary shortcuts stay filtered.
    if altgr_held {
        for modifier in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
            set_key_down_state(&mut state, modifier, true);
        }
    }
    let decoded = layout_decode::decode(
        u32::from(vk.0),
        kb.scanCode,
        state,
        hkl.0 as usize,
        (focus.foreground_hwnd as usize, focus.focus_hwnd as usize),
    );
    if !with_journal_mut(|journal| {
        journal.invalidate_if_foreground_changed();
        journal.accept_decoded_focus(focus)
    }) {
        return None;
    }
    capture_decode_result(decoded, layout, focus, ctrl_or_alt_held, altgr_held)
}

#[cfg(windows)]
fn capture_decode_result(
    decoded: layout_decode::DecodeResult,
    layout: LayoutTag,
    focus: FocusCacheKey,
    ctrl_or_alt_held: bool,
    altgr_held: bool,
) -> Option<DecodedText> {
    match decoded {
        layout_decode::DecodeResult::Text(text) => {
            if text.chars().any(char::is_control) {
                invalidate();
                return None;
            }
            with_journal_mut(|journal| journal.pending_dead_key = false);
            Some(DecodedText {
                text,
                layout,
                focus,
            })
        }
        layout_decode::DecodeResult::PendingDeadKey => {
            with_journal_mut(|journal| journal.pending_dead_key = true);
            None
        }
        layout_decode::DecodeResult::NoText => {
            // An unmapped AltGr combination does not consume Windows' pending
            // dead key. Preserve both that composition and the captured prefix;
            // resetting here would make the next glyph differ from the editor.
            if ctrl_or_alt_held && !altgr_held {
                invalidate();
            }
            None
        }
        layout_decode::DecodeResult::Unavailable => {
            // Do not retain a suffix after an unknown first character: that would
            // make a later conversion look successful while leaving its prefix.
            mark_capture_unavailable();
            None
        }
    }
}

#[cfg(windows)]
fn mark_capture_unavailable() {
    layout_decode::reset();
    with_journal_mut(|journal| {
        journal.clear();
        journal.capture_unavailable = true;
    });
}

#[cfg(windows)]
pub fn record_keydown(kb: &KBDLLHOOKSTRUCT, vk: u32) -> Option<String> {
    if kb.flags.contains(LLKHF_INJECTED) && !allow_injected_input_for_e2e() {
        return None;
    }

    let vk_u16 = u16::try_from(vk).ok()?;
    let vk = VIRTUAL_KEY(vk_u16);
    if is_modifier_vk(vk) {
        return None;
    }
    with_journal_mut(InputJournal::invalidate_if_foreground_changed);

    enum JournalAction {
        Clear,
        Backspace,
        MoveCaretLeft,
        MoveCaretRight,
        MoveCaretEnd,
        PushText {
            text: String,
            layout: LayoutTag,
            origin: RunOrigin,
        },
    }

    let mut action: Option<JournalAction> = None;
    let mut output: Option<String> = None;
    let mut decoded_focus = None;

    match vk {
        VK_ESCAPE | VK_DELETE | VK_INSERT | VK_UP | VK_DOWN | VK_HOME | VK_PRIOR | VK_NEXT
        | VK_TAB => {
            action = Some(JournalAction::Clear);
        }
        VK_LEFT => action = Some(JournalAction::MoveCaretLeft),
        VK_RIGHT => action = Some(JournalAction::MoveCaretRight),
        VK_END => action = Some(JournalAction::MoveCaretEnd),
        VK_BACK => action = Some(JournalAction::Backspace),
        VK_RETURN => {
            let layout = current_foreground_layout_tag();
            output = Some("\n".to_string());
            action = Some(JournalAction::PushText {
                text: "\n".to_string(),
                layout,
                origin: RunOrigin::Physical,
            });
        }
        _ => {}
    }

    if should_clear_for_ctrl_alt_combo(vk, mods_ctrl_or_alt_down() && !altgr_down()) {
        action = Some(JournalAction::Clear);
    }
    if matches!(vk, VK_LEFT | VK_RIGHT | VK_END) && key_is_down(VK_SHIFT) {
        // Shift navigation extends a selection; it does not move a collapsed
        // caret. Its next edit can replace many glyphs, which the journal does
        // not track. Start fresh instead of authorizing deletion of stale text.
        action = Some(JournalAction::Clear);
    }

    if action.is_none() {
        let decoded = decode_typed_text(kb, vk)?;
        decoded_focus = Some(decoded.focus);
        output = Some(decoded.text.clone());
        action = Some(JournalAction::PushText {
            text: decoded.text,
            layout: decoded.layout,
            origin: RunOrigin::Physical,
        });
    }

    with_journal_mut(|j| {
        j.invalidate_if_foreground_changed();
        if let Some(focus) = decoded_focus
            && !j.accept_decoded_focus(focus)
        {
            output = None;
            return;
        }
        if let Some(action) = action {
            if j.pending_dead_key && matches!(vk, VK_LEFT | VK_RIGHT | VK_END) {
                j.clear();
                j.capture_unavailable = true;
                layout_decode::reset();
                return;
            }
            match action {
                JournalAction::Clear => {
                    j.clear();
                    layout_decode::reset();
                }
                JournalAction::Backspace => {
                    if j.pending_dead_key {
                        j.pending_dead_key = false;
                        layout_decode::reset();
                    } else {
                        j.backspace();
                    }
                }
                JournalAction::MoveCaretLeft => j.move_caret_left(),
                JournalAction::MoveCaretRight => j.move_caret_right(),
                JournalAction::MoveCaretEnd => j.move_caret_end(),
                JournalAction::PushText {
                    text,
                    layout,
                    origin,
                } => {
                    if text.contains('\n') {
                        j.clear();
                        layout_decode::reset();
                    }
                    if text.chars().any(char::is_alphanumeric) {
                        j.last_token_autoconverted = false;
                    }
                    j.push_text_internal(&text, layout, origin);
                }
            }
        }
    });

    output
}

#[must_use]
pub fn take_last_layout_run_with_suffix() -> Option<(InputRun, Vec<InputRun>)> {
    with_journal_mut(|j| {
        #[cfg(windows)]
        if j.last_fg_hwnd != 0 {
            j.invalidate_if_foreground_changed();
        }
        if j.pending_dead_key || j.capture_unavailable {
            return None;
        }
        j.take_last_layout_run_with_suffix()
    })
}

#[cfg(test)]
#[must_use]
pub fn take_last_layout_sequence_with_suffix() -> Option<(Vec<InputRun>, Vec<InputRun>)> {
    with_journal_mut(|j| j.take_last_layout_sequence_with_suffix())
}

#[cfg(test)]
#[must_use]
pub fn take_last_programmatic_sequence_with_suffix() -> Option<(Vec<InputRun>, Vec<InputRun>)> {
    with_journal_mut(|j| j.take_last_programmatic_sequence_with_suffix())
}

#[must_use]
pub fn take_last_sequence_with_suffix() -> Option<(Vec<InputRun>, Vec<InputRun>)> {
    with_journal_mut(|j| {
        #[cfg(windows)]
        if j.last_fg_hwnd != 0 {
            j.invalidate_if_foreground_changed();
        }
        if j.pending_dead_key || j.capture_unavailable {
            return None;
        }
        j.take_last_sequence_with_suffix()
    })
}

#[cfg(test)]
pub fn push_text(s: &str) {
    with_journal_mut(|j| j.push_text_internal(s, LayoutTag::Unknown, RunOrigin::Programmatic));
}

pub fn push_run(run: InputRun) {
    with_journal_mut(|j| j.push_run(run));
}

pub fn push_runs(runs: impl IntoIterator<Item = InputRun>) {
    with_journal_mut(|j| j.push_runs(runs));
}

#[cfg(test)]
pub fn test_backspace() {
    with_journal_mut(|j| j.backspace());
}

#[cfg(test)]
pub fn test_move_caret_left(count: usize) {
    with_journal_mut(|j| {
        for _ in 0..count {
            j.move_caret_left();
        }
    });
}

#[cfg(test)]
pub fn test_move_caret_right(count: usize) {
    with_journal_mut(|j| {
        for _ in 0..count {
            j.move_caret_right();
        }
    });
}

#[cfg(test)]
pub fn runs_snapshot() -> Vec<InputRun> {
    with_journal(|j| j.runs.iter().cloned().collect())
}

#[cfg(any(test, windows))]
pub fn invalidate() {
    #[cfg(windows)]
    layout_decode::reset();
    with_journal_mut(|j| {
        j.clear();
        #[cfg(windows)]
        {
            j.last_fg_hwnd = 0;
            j.last_focus_key = None;
        }
    });
    #[cfg(all(test, windows))]
    clear_foreground_cache_for_test();
}

#[cfg(all(test, windows))]
pub fn clear_foreground_cache_for_test() {
    with_journal_cache_mut(HashMap::clear);
}

#[cfg(all(test, windows))]
pub fn test_switch_foreground(raw: isize, now_ms: u64) {
    with_journal_mut(|j| j.switch_foreground(raw, focus_key_for_test(raw, 0), now_ms));
}

#[cfg(all(test, windows))]
fn focus_key_for_test(raw: isize, caret_left: i32) -> Option<FocusCacheKey> {
    (raw != 0).then_some(FocusCacheKey {
        foreground_hwnd: raw,
        focus_hwnd: raw + 10_000,
        caret_hwnd: raw + 20_000,
        caret_rect: CaretRectSignature {
            left: caret_left,
            top: 10,
            right: caret_left + 1,
            bottom: 30,
        },
    })
}

#[cfg(all(test, windows))]
fn test_switch_foreground_at(raw: isize, caret_left: i32, now_ms: u64) {
    with_journal_mut(|j| j.switch_foreground(raw, focus_key_for_test(raw, caret_left), now_ms));
}

#[cfg(all(test, windows))]
fn test_switch_foreground_control(
    raw: isize,
    focus_hwnd: isize,
    caret_hwnd: isize,
    caret_left: i32,
    now_ms: u64,
) {
    let key = (raw != 0).then_some(FocusCacheKey {
        foreground_hwnd: raw,
        focus_hwnd,
        caret_hwnd,
        caret_rect: CaretRectSignature {
            left: caret_left,
            top: 10,
            right: caret_left + 1,
            bottom: 30,
        },
    });
    with_journal_mut(|j| j.switch_foreground(raw, key, now_ms));
}

#[cfg(all(test, windows))]
fn test_switch_foreground_without_signature(raw: isize, now_ms: u64) {
    with_journal_mut(|j| j.switch_foreground(raw, None, now_ms));
}

#[cfg(all(test, windows))]
#[must_use]
pub fn raw_foreground_for_test() -> isize {
    with_journal(|j| j.last_fg_hwnd)
}

#[cfg(any(test, windows))]
#[must_use]
pub fn last_char_triggers_autoconvert() -> bool {
    with_journal(|j| {
        let Some(last) = j.last_char() else {
            return false;
        };

        if matches!(last, '.' | ',' | '!' | '?' | ';' | ':') {
            return j
                .prev_char_before_last()
                .is_some_and(|prev| !prev.is_whitespace());
        }

        if last.is_whitespace() {
            return j
                .prev_char_before_last()
                .is_some_and(|prev| !prev.is_whitespace());
        }

        false
    })
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    fn captured_text(text: &str, origin: RunOrigin, capacity: usize) -> InputJournal {
        let mut journal = InputJournal::new(capacity);
        for ch in text.chars() {
            journal.push_text_internal(&ch.to_string(), LayoutTag::En, origin);
        }
        journal
    }

    #[test]
    fn native_input_journal_capacity_keeps_the_first_glyph_through_4096_characters() {
        for length in [99, 100, 101, DEFAULT_CAPACITY_CHARS] {
            let text = format!(".{}", "a".repeat(length - 1));
            let mut journal = captured_text(&text, RunOrigin::Physical, DEFAULT_CAPACITY_CHARS);
            let (runs, suffix) = journal
                .take_last_sequence_with_suffix()
                .expect("the complete sequence must remain convertible");
            assert!(suffix.is_empty());
            assert_eq!(
                runs.iter().map(|run| run.text.as_str()).collect::<String>(),
                text,
                "journal input length {length}"
            );
        }
    }

    #[test]
    fn native_input_journal_overflow_refuses_partial_word_and_sequence_without_mutation() {
        for origin in [RunOrigin::Physical, RunOrigin::Programmatic] {
            for trailing in ["", " "] {
                let text = format!(".{}{trailing}", "a".repeat(DEFAULT_CAPACITY_CHARS));
                let mut journal = captured_text(&text, origin, DEFAULT_CAPACITY_CHARS);
                let before = journal.runs.clone();
                assert_eq!(journal.total_chars, DEFAULT_CAPACITY_CHARS);

                assert!(journal.take_last_layout_run_with_suffix().is_none());
                assert_eq!(journal.runs, before);
                assert!(journal.take_last_sequence_with_suffix().is_none());
                assert_eq!(journal.runs, before);
                assert!(journal.take_last_layout_sequence_with_suffix().is_none());
                assert_eq!(journal.runs, before);
                assert!(
                    journal
                        .take_last_programmatic_sequence_with_suffix()
                        .is_none()
                );
                assert_eq!(journal.runs, before);
                assert_eq!(journal.total_chars, DEFAULT_CAPACITY_CHARS);
            }
        }
    }

    #[test]
    fn native_input_journal_overflow_keeps_complete_last_word_after_a_retained_boundary() {
        let mut journal = captured_text("overlongoldprefix ghbdtn ", RunOrigin::Physical, 10);
        let before = journal.runs.clone();
        assert!(journal.take_last_sequence_with_suffix().is_none());
        assert_eq!(journal.runs, before);

        let (word, suffix) = journal
            .take_last_layout_run_with_suffix()
            .expect("the final word has a known whitespace boundary");
        assert_eq!(word.text, "ghbdtn");
        assert_eq!(
            suffix
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            " "
        );
        journal.push_run(word);
        journal.push_runs(suffix);
        assert_eq!(journal.runs, before);
        assert!(journal.take_last_sequence_with_suffix().is_none());
        assert_eq!(journal.runs, before);
    }

    #[test]
    fn native_input_journal_overflow_rejection_preserves_the_caret() {
        let mut journal = captured_text(".abcdefgh ", RunOrigin::Programmatic, 8);
        journal.move_caret_left();
        journal.move_caret_left();
        journal.move_caret_left();
        let before = journal.runs.clone();

        assert!(journal.take_last_layout_run_with_suffix().is_none());
        assert_eq!(journal.runs, before);
        assert_eq!(journal.caret_from_end, 3);
        assert!(journal.take_last_layout_sequence_with_suffix().is_none());
        assert_eq!(journal.runs, before);
        assert_eq!(journal.caret_from_end, 3);
        assert!(
            journal
                .take_last_programmatic_sequence_with_suffix()
                .is_none()
        );
        assert_eq!(journal.runs, before);
        assert_eq!(journal.caret_from_end, 3);
        assert!(journal.take_last_sequence_with_suffix().is_none());
        assert_eq!(journal.runs, before);
        assert_eq!(journal.caret_from_end, 3);
    }

    #[test]
    fn native_input_journal_overflow_recovers_after_clear() {
        let mut journal = captured_text(".abcdefgh", RunOrigin::Physical, 8);
        assert!(journal.take_last_sequence_with_suffix().is_none());
        journal.clear();
        journal.push_text_internal(".ghbdtn", LayoutTag::En, RunOrigin::Physical);
        let (runs, suffix) = journal
            .take_last_sequence_with_suffix()
            .expect("a new input session must recover after overflow");
        assert!(suffix.is_empty());
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            ".ghbdtn"
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use windows::{
        Win32::UI::Input::KeyboardAndMouse::{
            LoadKeyboardLayoutW, MAPVK_VK_TO_VSC, MAPVK_VSC_TO_VK_EX, MapVirtualKeyExW, VK_CONTROL,
            VK_LCONTROL, VK_LMENU, VK_MENU, VK_OEM_2, VK_OEM_PERIOD, VK_RCONTROL, VK_RMENU,
        },
        core::{PCWSTR, w},
    };

    use super::*;

    fn released_keyboard_state_overrides() -> KeyboardStateOverrides {
        KeyboardStateOverrides {
            shift_down: false,
            left_shift_down: false,
            right_shift_down: false,
            caps_lock_on: false,
        }
    }

    fn load_native_test_layout(name: PCWSTR) -> HKL {
        let _guard = test_guard();
        // No activation flag: decoding uses this explicit layout without requiring foreground focus.
        let layout = unsafe { LoadKeyboardLayoutW(name, Default::default()) }
            .expect("the native test keyboard layout must load");
        // Match startup preparation before the hook is installed. No keystroke
        // is consumed here: the following assertion still checks the first one.
        assert!(
            layout_decode::prepare_layout(layout.0 as usize),
            "the native test layout must be prepared before input capture"
        );
        layout
    }

    fn native_text_after_overrides(
        layout: HKL,
        vk: VIRTUAL_KEY,
        stale_keys: &[VIRTUAL_KEY],
        overrides: KeyboardStateOverrides,
    ) -> String {
        let _guard = test_guard();
        let mut state = [0u8; 256];
        for key in stale_keys {
            state[usize::from(key.0)] = 0x80;
        }
        apply_keyboard_state_overrides(&mut state, overrides);

        let scan = unsafe { MapVirtualKeyExW(u32::from(vk.0), MAPVK_VK_TO_VSC, Some(layout)) };
        assert_ne!(scan, 0, "the test key must have a native scan code");
        let mut buf = [0u16; 8];
        let count =
            unsafe { ToUnicodeEx(u32::from(vk.0), scan, &state, &mut buf, 0, Some(layout)) };
        let count = usize::try_from(count).expect("the test keys must not be dead keys");
        let native_text =
            String::from_utf16(&buf[..count]).expect("native decoding must produce valid UTF-16");
        assert_eq!(
            layout_decode::decode_probe(u32::from(vk.0), scan, state, layout.0 as usize, (1, 1)),
            layout_decode::DecodeResult::Text(native_text.clone()),
            "the production decoder must agree with the native first-character oracle"
        );
        native_text
    }

    fn assert_native_character_ignores_stale_shortcuts(
        layout: HKL,
        vk: VIRTUAL_KEY,
        expected: &str,
    ) {
        assert_eq!(
            native_text_after_overrides(layout, vk, &[], released_keyboard_state_overrides()),
            expected,
            "the explicit layout must decode the clean baseline"
        );

        let stale_snapshots: [(&str, &[VIRTUAL_KEY]); 6] = [
            ("left Control", &[VK_CONTROL, VK_LCONTROL]),
            ("right Control", &[VK_CONTROL, VK_RCONTROL]),
            ("left Alt", &[VK_MENU, VK_LMENU]),
            ("right Alt", &[VK_MENU, VK_RMENU]),
            ("Control+Alt", &[VK_CONTROL, VK_LCONTROL, VK_MENU, VK_LMENU]),
            ("AltGr", &[VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU]),
        ];
        let mut mismatches = Vec::new();
        for (name, stale_keys) in stale_snapshots {
            let actual = native_text_after_overrides(
                layout,
                vk,
                stale_keys,
                released_keyboard_state_overrides(),
            );
            if actual != expected {
                mismatches.push(format!("{name}: got {actual:?}, expected {expected:?}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "released physical modifiers must override stale thread state: {mismatches:?}"
        );
    }

    #[test]
    fn native_input_unavailable_first_key_never_exposes_known_suffix() {
        let _guard = test_guard();
        for suffix in ["hbdtn", "ghbdtn", "."] {
            invalidate();
            mark_capture_unavailable();

            // Later successful decoding must not turn an unknown first glyph
            // into a seemingly complete, convertible suffix.
            with_journal_mut(|journal| {
                journal.push_text_internal(suffix, LayoutTag::En, RunOrigin::Physical);
            });
            assert!(runs_snapshot().is_empty());
            assert!(take_last_layout_run_with_suffix().is_none());
            assert!(take_last_sequence_with_suffix().is_none());

            // Restoring an older snapshot cannot bypass the conversion guard.
            push_run(test_run(suffix, LayoutTag::En));
            assert!(take_last_layout_run_with_suffix().is_none());
            assert!(take_last_sequence_with_suffix().is_none());
        }
        invalidate();
    }

    #[test]
    fn native_input_unavailable_capture_recovers_after_explicit_invalidation() {
        let _guard = test_guard();
        invalidate();
        mark_capture_unavailable();
        with_journal_mut(|journal| {
            journal.push_text_internal("hbdtn", LayoutTag::En, RunOrigin::Physical);
        });
        assert!(take_last_sequence_with_suffix().is_none());

        invalidate();
        with_journal_mut(|journal| {
            journal.push_text_internal(".ghbdtn", LayoutTag::En, RunOrigin::Physical);
        });
        let (runs, suffix) = take_last_sequence_with_suffix()
            .expect("a new capture session should contain the complete new text");
        assert!(suffix.is_empty());
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            ".ghbdtn"
        );
        invalidate();
    }

    #[test]
    fn native_input_unmapped_altgr_keeps_dead_key_and_complete_prefix() {
        let layout = load_native_test_layout(w!("00000407"));
        let _guard = test_guard();
        let focus = focus_key_for_test(1001, 0).expect("the fixture has a focused target");
        let tag = layout_tag_from_hkl(layout);
        let dead_vk = unsafe { MapVirtualKeyExW(0x29, MAPVK_VSC_TO_VK_EX, Some(layout)) };
        assert_ne!(dead_vk, 0);
        let decode = |vk, state| {
            let scan = unsafe { MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC, Some(layout)) };
            assert_ne!(scan, 0);
            layout_decode::decode(
                vk,
                scan,
                state,
                layout.0 as usize,
                (focus.foreground_hwnd as usize, focus.focus_hwnd as usize),
            )
        };

        for pending_accent in [false, true] {
            invalidate();
            with_journal_mut(|journal| {
                journal.push_text_internal("abc", tag, RunOrigin::Physical);
            });
            if pending_accent {
                let decoded = decode(dead_vk, [0; 256]);
                assert_eq!(decoded, layout_decode::DecodeResult::PendingDeadKey);
                assert!(capture_decode_result(decoded, tag, focus, false, false).is_none());
            }

            let mut altgr_state = [0; 256];
            for key in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
                set_key_down_state(&mut altgr_state, key, true);
            }
            // German X has no AltGr character. The native table oracle also
            // covers this no-output stroke between the dead key and E.
            let decoded = decode(u32::from(b'X'), altgr_state);
            assert_eq!(decoded, layout_decode::DecodeResult::NoText);
            assert!(capture_decode_result(decoded, tag, focus, true, true).is_none());
            with_journal(|journal| {
                assert_eq!(journal.pending_dead_key, pending_accent);
                assert!(!journal.capture_unavailable);
                assert_eq!(journal.total_chars, 3);
            });

            let decoded = decode(u32::from(b'E'), [0; 256]);
            let captured = capture_decode_result(decoded, tag, focus, false, false)
                .expect("the following E must remain capturable");
            assert_eq!(captured.text, if pending_accent { "ê" } else { "e" });
            with_journal_mut(|journal| {
                journal.push_text_internal(&captured.text, captured.layout, RunOrigin::Physical);
            });
            let (runs, suffix) = take_last_sequence_with_suffix()
                .expect("Double Alt must retain the whole prefix and following glyph");
            assert!(suffix.is_empty());
            assert_eq!(
                runs.iter().map(|run| run.text.as_str()).collect::<String>(),
                if pending_accent { "abcê" } else { "abce" }
            );
        }
        invalidate();
    }

    #[test]
    fn native_input_no_text_ctrl_alt_shortcut_still_invalidates_journal() {
        let _guard = test_guard();
        invalidate();
        let focus = focus_key_for_test(1001, 0).expect("the fixture has a focused target");
        with_journal_mut(|journal| {
            journal.push_text_internal("abc", LayoutTag::En, RunOrigin::Physical);
            journal.pending_dead_key = true;
        });

        assert!(
            capture_decode_result(
                layout_decode::DecodeResult::NoText,
                LayoutTag::En,
                focus,
                true,
                false,
            )
            .is_none()
        );
        assert!(runs_snapshot().is_empty());
        assert!(!with_journal(|journal| journal.pending_dead_key));
        assert!(take_last_sequence_with_suffix().is_none());
        invalidate();
    }

    #[test]
    fn native_input_first_english_letter_survives_stale_shortcut_state() {
        let layout = load_native_test_layout(w!("00000409"));
        assert_native_character_ignores_stale_shortcuts(layout, VIRTUAL_KEY(u16::from(b'G')), "g");
    }

    #[test]
    fn native_input_first_russian_letter_survives_stale_shortcut_state() {
        let layout = load_native_test_layout(w!("00000419"));
        assert_native_character_ignores_stale_shortcuts(layout, VIRTUAL_KEY(u16::from(b'G')), "п");
    }

    #[test]
    fn native_input_first_english_dot_survives_stale_shortcut_state() {
        let layout = load_native_test_layout(w!("00000409"));
        assert_native_character_ignores_stale_shortcuts(layout, VK_OEM_PERIOD, ".");
    }

    #[test]
    fn native_input_first_russian_dot_survives_stale_shortcut_state() {
        let layout = load_native_test_layout(w!("00000419"));
        assert_native_character_ignores_stale_shortcuts(layout, VK_OEM_2, ".");
    }

    #[test]
    fn native_input_first_stale_snapshot_does_not_shorten_the_journal_sequence() {
        let sequences: [(PCWSTR, LayoutTag, &[u16], &str); 4] = [
            (
                w!("00000409"),
                LayoutTag::En,
                &[0x47, 0x48, 0x42, 0x44, 0x54, 0x4E],
                "ghbdtn",
            ),
            (
                w!("00000409"),
                LayoutTag::En,
                &[VK_OEM_PERIOD.0, 0x47, 0x48, 0x42, 0x44, 0x54, 0x4E],
                ".ghbdtn",
            ),
            (
                w!("00000419"),
                LayoutTag::Ru,
                &[0x47, 0x48, 0x42, 0x44, 0x54, 0x4E],
                "привет",
            ),
            (
                w!("00000419"),
                LayoutTag::Ru,
                &[VK_OEM_2.0, 0x47, 0x48, 0x42, 0x44, 0x54, 0x4E],
                ".привет",
            ),
        ];
        let mut mismatches = Vec::new();
        for (name, layout_tag, keys, expected) in sequences {
            let layout = load_native_test_layout(name);
            let mut journal = InputJournal::new(100);
            for (index, &vk) in keys.iter().enumerate() {
                let stale_keys: &[VIRTUAL_KEY] = if index == 0 {
                    &[VK_CONTROL, VK_LCONTROL]
                } else {
                    &[]
                };
                let decoded = native_text_after_overrides(
                    layout,
                    VIRTUAL_KEY(vk),
                    stale_keys,
                    released_keyboard_state_overrides(),
                );
                // Match decode_typed_text's control-character rejection before journaling.
                if !decoded.chars().any(char::is_control) {
                    journal.push_text_internal(&decoded, layout_tag, RunOrigin::Physical);
                }
            }
            let (runs, suffix) = journal
                .take_last_sequence_with_suffix()
                .expect("the native key sequence must reach the journal");
            assert!(suffix.is_empty());
            let actual = runs.iter().map(|run| run.text.as_str()).collect::<String>();
            if actual != expected {
                mismatches.push(format!("got {actual:?}, expected {expected:?}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "a stale snapshot on the first key must not omit that key: {mismatches:?}"
        );
    }

    #[test]
    fn native_input_letter_case_follows_physical_shift_and_caps_lock() {
        for (name, lowercase, uppercase) in [(w!("00000409"), "g", "G"), (w!("00000419"), "п", "П")]
        {
            let layout = load_native_test_layout(name);
            for (shift, caps_lock, expected) in [
                (false, false, lowercase),
                (true, false, uppercase),
                (false, true, uppercase),
                (true, true, lowercase),
            ] {
                let overrides = KeyboardStateOverrides {
                    shift_down: shift,
                    left_shift_down: shift,
                    right_shift_down: false,
                    caps_lock_on: caps_lock,
                };
                assert_eq!(
                    native_text_after_overrides(
                        layout,
                        VIRTUAL_KEY(u16::from(b'G')),
                        &[],
                        overrides,
                    ),
                    expected,
                    "Shift={shift}, CapsLock={caps_lock}"
                );
            }
        }
    }

    #[test]
    fn native_input_journal_short_sequence_keeps_leading_dot_and_single_dot() {
        for (text, layout) in [
            (".", LayoutTag::En),
            (".", LayoutTag::Ru),
            (".ghbdtn", LayoutTag::En),
            (".руддщ", LayoutTag::Ru),
        ] {
            let mut journal = InputJournal::new(100);
            for ch in text.chars() {
                journal.push_text_internal(&ch.to_string(), layout, RunOrigin::Physical);
            }
            let (runs, suffix) = journal
                .take_last_sequence_with_suffix()
                .expect("leading punctuation must remain in a sequence");
            assert!(suffix.is_empty());
            assert_eq!(
                runs.iter().map(|run| run.text.as_str()).collect::<String>(),
                text
            );
        }
    }

    fn test_run(text: &str, layout: LayoutTag) -> InputRun {
        InputRun {
            text: text.to_string(),
            layout,
            origin: RunOrigin::Physical,
            kind: RunKind::Text,
        }
    }

    fn test_space() -> InputRun {
        InputRun {
            text: " ".to_string(),
            layout: LayoutTag::En,
            origin: RunOrigin::Physical,
            kind: RunKind::Whitespace,
        }
    }

    fn test_programmatic_text(text: &str, layout: LayoutTag) -> InputRun {
        InputRun {
            text: text.to_string(),
            layout,
            origin: RunOrigin::Programmatic,
            kind: RunKind::Text,
        }
    }

    fn test_programmatic_space(layout: LayoutTag) -> InputRun {
        InputRun {
            text: " ".to_string(),
            layout,
            origin: RunOrigin::Programmatic,
            kind: RunKind::Whitespace,
        }
    }

    fn take_last_word_after_focus_refresh(
        raw: isize,
        caret_left: i32,
        now_ms: u64,
    ) -> Option<InputRun> {
        with_journal_mut(|j| {
            j.switch_foreground(raw, focus_key_for_test(raw, caret_left), now_ms);
            j.take_last_layout_run_with_suffix().map(|(run, suffix)| {
                j.push_runs(suffix);
                run
            })
        })
    }

    fn take_last_sequence_after_focus_refresh(
        raw: isize,
        caret_left: i32,
        now_ms: u64,
    ) -> Option<Vec<InputRun>> {
        with_journal_mut(|j| {
            j.switch_foreground(raw, focus_key_for_test(raw, caret_left), now_ms);
            j.take_last_sequence_with_suffix().map(|(runs, suffix)| {
                j.push_runs(suffix);
                runs
            })
        })
    }

    #[test]
    fn foreground_switch_restores_recent_window_journal() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();

        test_switch_foreground(1001, 1_000);
        push_run(test_run("first", LayoutTag::En));

        test_switch_foreground(2002, 1_100);
        assert!(runs_snapshot().is_empty());
        push_run(test_run("second", LayoutTag::Ru));

        test_switch_foreground(1001, 1_200);
        assert_eq!(raw_foreground_for_test(), 1001);
        assert_eq!(
            runs_snapshot()
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "first"
        );

        test_switch_foreground(2002, 1_300);
        assert_eq!(raw_foreground_for_test(), 2002);
        assert_eq!(
            runs_snapshot()
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "second"
        );
    }

    #[test]
    fn foreground_switch_drops_stale_window_journal_after_ttl() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();

        test_switch_foreground(1001, 1_000);
        push_run(test_run("stale", LayoutTag::En));

        test_switch_foreground(2002, 2_000);
        test_switch_foreground(1001, 2_000 + FOREGROUND_CACHE_TTL_MS + 1);

        assert_eq!(raw_foreground_for_test(), 1001);
        assert!(runs_snapshot().is_empty());
    }

    #[test]
    fn foreground_switch_requires_matching_caret_signature_to_restore() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();

        test_switch_foreground_at(1001, 10, 1_000);
        push_run(test_run("first", LayoutTag::En));

        test_switch_foreground(2002, 1_100);
        assert!(runs_snapshot().is_empty());

        test_switch_foreground_at(1001, 50, 1_200);
        assert_eq!(raw_foreground_for_test(), 1001);
        assert!(runs_snapshot().is_empty());
    }

    #[test]
    fn foreground_switch_same_control_caret_movement_keeps_current_session() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();

        test_switch_foreground_at(1001, 10, 1_000);
        push_run(test_run("first", LayoutTag::En));

        test_switch_foreground_at(1001, 50, 1_100);
        assert_eq!(
            runs_snapshot()
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "first"
        );

        test_switch_foreground(2002, 1_200);
        test_switch_foreground_at(1001, 50, 1_300);
        assert_eq!(
            runs_snapshot()
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "first"
        );

        test_switch_foreground(2002, 1_400);
        test_switch_foreground_at(1001, 10, 1_500);
        assert!(runs_snapshot().is_empty());
    }

    #[test]
    fn foreground_switch_restores_distinct_controls_in_same_window() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();

        test_switch_foreground_control(1001, 11, 21, 10, 1_000);
        push_run(test_run("first", LayoutTag::En));

        test_switch_foreground_control(1001, 12, 22, 10, 1_100);
        assert!(runs_snapshot().is_empty());
        push_run(test_run("second", LayoutTag::Ru));

        test_switch_foreground_control(1001, 11, 21, 10, 1_200);
        assert_eq!(
            runs_snapshot()
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "first"
        );

        test_switch_foreground_control(1001, 12, 22, 10, 1_300);
        assert_eq!(
            runs_snapshot()
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "second"
        );
    }

    #[test]
    fn foreground_switch_same_control_caret_recreation_keeps_first_glyph() {
        let _guard = test_guard();
        for (first, rest) in [("g", "hbdtn"), (".", "ghbdtn"), (".", "")] {
            invalidate();
            test_switch_foreground_control(1001, 11, 21, 10, 1_000);
            push_run(test_run(first, LayoutTag::En));

            // The application processes the first glyph and replaces the caret owner.
            // No focus transition or mouse/navigation input occurred in between.
            test_switch_foreground_control(1001, 11, 22, 20, 1_001);
            push_run(test_run(rest, LayoutTag::En));
            let text = with_journal_mut(|journal| {
                let (runs, _) = journal
                    .take_last_sequence_with_suffix()
                    .expect("first glyph must remain available to Double Alt");
                runs.iter().map(|run| run.text.as_str()).collect::<String>()
            });
            assert_eq!(text, format!("{first}{rest}"));
        }
    }

    #[test]
    fn foreground_switch_new_control_with_same_caret_owner_starts_empty() {
        let _guard = test_guard();
        invalidate();
        test_switch_foreground_control(1001, 11, 21, 10, 1_000);
        push_run(test_run("source-must-stay", LayoutTag::En));

        test_switch_foreground_control(1001, 12, 21, 10, 1_001);
        assert!(runs_snapshot().is_empty());
        push_run(test_run("destination", LayoutTag::En));

        test_switch_foreground_control(1001, 11, 21, 10, 1_002);
        let text = runs_snapshot()
            .iter()
            .map(|run| run.text.as_str())
            .collect::<String>();
        assert_eq!(text, "source-must-stay");
    }

    #[test]
    fn native_input_tab_navigation_discards_previous_field() {
        let _guard = test_guard();
        invalidate();
        push_run(test_run("source-must-stay", LayoutTag::En));

        // Browser DOM fields can share a focused native HWND. Tab is therefore a
        // journal boundary even when a later GetGUIThreadInfo snapshot is equal.
        let key = KBDLLHOOKSTRUCT {
            vkCode: u32::from(VK_TAB.0),
            scanCode: 0x0f,
            ..Default::default()
        };
        assert_eq!(record_keydown(&key, u32::from(VK_TAB.0)), None);
        assert!(runs_snapshot().is_empty());

        push_run(test_run(".ghbdtn", LayoutTag::En));
        let text = with_journal_mut(|journal| {
            let (runs, _) = journal
                .take_last_sequence_with_suffix()
                .expect("new field text should be available");
            runs.iter().map(|run| run.text.as_str()).collect::<String>()
        });
        assert_eq!(text, ".ghbdtn");
    }

    #[test]
    fn native_input_unknown_first_target_never_exposes_a_later_suffix() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();
        test_switch_foreground_without_signature(0, 1_000);
        mark_capture_unavailable();
        test_switch_foreground_control(1001, 11, 21, 10, 1_001);
        with_journal_mut(|journal| {
            assert!(journal.capture_unavailable);
            journal.push_text_internal("hbdtn", LayoutTag::En, RunOrigin::Physical);
            assert!(journal.runs.is_empty());
        });
    }

    #[test]
    fn native_input_worker_result_keeps_caret_changes_but_rejects_another_field() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();
        test_switch_foreground_control(1001, 11, 21, 10, 1_000);
        let captured = with_journal(|journal| journal.last_focus_key.unwrap());
        push_run(test_run("g", LayoutTag::En));
        test_switch_foreground_control(1001, 11, 22, 20, 1_001);
        assert!(with_journal_mut(
            |journal| journal.accept_decoded_focus(captured)
        ));
        assert_eq!(runs_snapshot()[0].text, "g");

        test_switch_foreground_control(1001, 12, 22, 20, 1_002);
        assert!(!with_journal_mut(
            |journal| journal.accept_decoded_focus(captured)
        ));
        with_journal_mut(|journal| {
            assert!(journal.capture_unavailable);
            journal.push_text_internal("hbdtn", LayoutTag::En, RunOrigin::Physical);
            assert!(journal.runs.is_empty());
        });
    }

    #[test]
    fn native_input_focus_cache_does_not_resume_abandoned_dead_key() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();
        test_switch_foreground_control(1001, 11, 21, 10, 1_000);
        push_run(test_run("prefix", LayoutTag::En));
        with_journal_mut(|journal| journal.pending_dead_key = true);
        test_switch_foreground_control(2002, 12, 22, 10, 1_001);
        push_run(test_run("other", LayoutTag::En));
        test_switch_foreground_control(1001, 11, 21, 10, 1_002);
        with_journal_mut(|journal| {
            assert!(!journal.pending_dead_key);
            assert!(journal.capture_unavailable);
            journal.push_text_internal("e", LayoutTag::En, RunOrigin::Physical);
            assert!(journal.runs.is_empty());
        });
    }

    #[test]
    fn foreground_switch_without_signature_keeps_current_session_but_does_not_restore() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();

        test_switch_foreground_without_signature(1001, 1_000);
        push_run(test_run("unsupported", LayoutTag::En));

        test_switch_foreground_without_signature(1001, 1_100);
        assert_eq!(
            runs_snapshot()
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "unsupported"
        );

        test_switch_foreground(2002, 1_200);
        assert!(runs_snapshot().is_empty());

        test_switch_foreground_without_signature(1001, 1_300);
        assert_eq!(raw_foreground_for_test(), 1001);
        assert!(runs_snapshot().is_empty());
    }

    #[test]
    fn command_style_last_word_replacement_survives_same_control_caret_signature_change() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();

        test_switch_foreground_at(1001, 10, 1_000);
        push_runs([
            test_run("ghbdtn", LayoutTag::En),
            test_space(),
            test_run("rfr", LayoutTag::En),
            test_space(),
            test_run("ltkf", LayoutTag::En),
        ]);

        let word = take_last_word_after_focus_refresh(1001, 50, 1_100)
            .expect("last word should survive caret movement");
        assert_eq!(word.text, "ltkf");

        push_run(test_programmatic_text("дела", LayoutTag::Ru));

        let sequence = take_last_sequence_after_focus_refresh(1001, 70, 1_200)
            .expect("sequence should remain available after replacement");
        assert_eq!(
            sequence
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "ghbdtn rfr дела"
        );
    }

    #[test]
    fn command_style_sequence_replacement_keeps_word_and_sequence_commands_available() {
        let _guard = test_guard();
        invalidate();
        clear_foreground_cache_for_test();

        test_switch_foreground_at(1001, 10, 1_000);
        push_runs([
            test_run("ghbdtn", LayoutTag::En),
            test_space(),
            test_run("rfr", LayoutTag::En),
            test_space(),
            test_run("ltkf", LayoutTag::En),
        ]);

        let sequence = take_last_sequence_after_focus_refresh(1001, 50, 1_100)
            .expect("sequence should survive caret movement");
        assert_eq!(
            sequence
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "ghbdtn rfr ltkf"
        );

        push_runs([
            test_programmatic_text("привет", LayoutTag::Ru),
            test_programmatic_space(LayoutTag::Ru),
            test_programmatic_text("как", LayoutTag::Ru),
            test_programmatic_space(LayoutTag::Ru),
            test_programmatic_text("дела", LayoutTag::Ru),
        ]);

        let word = take_last_word_after_focus_refresh(1001, 80, 1_200)
            .expect("last word should remain available after sequence replacement");
        assert_eq!(word.text, "дела");
        push_run(word);

        let sequence = take_last_sequence_after_focus_refresh(1001, 90, 1_300)
            .expect("sequence should remain available after sequence replacement");
        assert_eq!(
            sequence
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "привет как дела"
        );
    }

    #[test]
    fn ctrl_or_alt_combo_preserves_modifier_only_layout_switch_chords() {
        assert!(!should_clear_for_ctrl_alt_combo(VK_LSHIFT, true));
        assert!(!should_clear_for_ctrl_alt_combo(VK_RSHIFT, true));
        assert!(!should_clear_for_ctrl_alt_combo(VIRTUAL_KEY(0xA4), true));
        assert!(!should_clear_for_ctrl_alt_combo(VIRTUAL_KEY(0xA5), true));
    }

    #[test]
    fn ctrl_or_alt_combo_still_clears_non_modifier_keys() {
        assert!(should_clear_for_ctrl_alt_combo(
            VIRTUAL_KEY(u16::from(b'A')),
            true
        ));
        assert!(should_clear_for_ctrl_alt_combo(VK_TAB, true));
        assert!(!should_clear_for_ctrl_alt_combo(
            VIRTUAL_KEY(u16::from(b'A')),
            false
        ));
        for vk in [VIRTUAL_KEY(u16::from(b'G')), VK_OEM_PERIOD, VK_OEM_2] {
            assert!(should_clear_for_ctrl_alt_combo(vk, true));
            assert!(!should_clear_for_ctrl_alt_combo(vk, false));
        }
    }

    #[test]
    fn keyboard_state_overrides_apply_caps_lock_toggle_without_touching_high_bit() {
        let mut state = [0u8; 256];
        apply_keyboard_state_overrides(
            &mut state,
            KeyboardStateOverrides {
                shift_down: false,
                left_shift_down: false,
                right_shift_down: false,
                caps_lock_on: true,
            },
        );

        let caps = state[usize::from(VK_CAPITAL.0)];
        assert_eq!(caps & 0x01, 0x01);
        assert_eq!(caps & 0x80, 0x00);
    }

    #[test]
    fn keyboard_state_overrides_preserve_shift_and_caps_lock_combination() {
        let mut state = [0u8; 256];
        apply_keyboard_state_overrides(
            &mut state,
            KeyboardStateOverrides {
                shift_down: true,
                left_shift_down: true,
                right_shift_down: false,
                caps_lock_on: true,
            },
        );

        assert_eq!(state[usize::from(VK_SHIFT.0)] & 0x80, 0x80);
        assert_eq!(state[usize::from(VK_LSHIFT.0)] & 0x80, 0x80);
        assert_eq!(state[usize::from(VK_RSHIFT.0)] & 0x80, 0x00);
        assert_eq!(state[usize::from(VK_CAPITAL.0)] & 0x01, 0x01);
    }
}
