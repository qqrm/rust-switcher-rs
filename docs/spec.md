# Rust Switcher - Specification (implementation aligned)

This document describes the current behavior and architecture of the repository implementation.
It is intended as onboarding documentation and as a reference for expected runtime behavior.

## Scope

- Supported OS: Windows (tested on Windows 11)
- Primary UI: native Win32 window + tray icon
- Linux: out of scope for runtime support (some parts may compile, but the app is not a supported Linux product)

## Core user goals

- Convert text typed in the wrong keyboard layout (RU <-> EN) quickly and reliably.
- Work without relying on clipboard paste for the main path.
- Never send `Ctrl+C` to the foreground application (to avoid SIGINT / app-specific semantics).
- Provide global hotkeys and a lightweight UI for configuration.

## Terminology

- Convert: map characters between keyboard layouts (RU <-> EN) using the built in mapping table.
- Selection: currently selected text in the active application.
- Last word: the last token captured by the keyboard hook input journal.
- Autoconvert: automatic conversion triggered by typed delimiter characters (for example Space).
- Autoconvert enabled: runtime flag controlling whether Autoconvert is active.
  - Important: this flag is NOT persisted in config.
  - Default on app start: disabled.

## High level architecture

- UI boundary (Win32):
  - Window procedure, message loop, controls
  - Apply and Cancel logic
  - Tray icon integration and tray menu actions
- Input boundary:
  - Low level keyboard hook (WH_KEYBOARD_LL)
  - Input journal and ring buffer for tokenization and last word extraction
  - Hotkey sequences matching
- Domain logic:
  - Text conversion, replacement of selection, insertion via SendInput
- Platform integration:
  - Global hotkeys are implemented by a hybrid model:
    - RegisterHotKey for legacy single-chord bindings
    - WH_KEYBOARD_LL + sequence matcher for hotkey sequences
  - Keyboard layout switching
  - Per-user Registry Run autostart entry
  - Notifications (tray balloon, MessageBox fallback)
- Persistence:
  - Config stored under %APPDATA%\RustSwitcher\config.json via confy

## Configuration

Config fields (see src/config.rs):

- delay_ms: u32 (legacy config field; runtime uses the fixed 100 ms replacement delay)
- start_minimized: bool
- theme_dark: bool
- autoconvert_feature_enabled: bool (defaults to true; enables automatic conversion and its pause hotkey)

Hotkeys (legacy single chord, optional):
- hotkey_convert_last_word
- hotkey_convert_selection
- hotkey_switch_layout
- hotkey_pause

Hotkey sequences (preferred, optional):
- hotkey_convert_last_word_sequence
- hotkey_pause_sequence
- hotkey_convert_selection_sequence
- hotkey_switch_layout_sequence

Notes:
- Autoconvert active/paused state is runtime only and is not stored in config. The feature itself can be disabled persistently with `autoconvert_feature_enabled`.
- Hotkey fields are shown in read-only edits, but user interaction updates pending sequence values that are applied on Apply. Press Backspace or Delete while a field is focused to clear that binding.
- Hotkey sequences are validated on save.

Default bindings (current defaults in code):
- Convert last word: double tap Left Shift within 1000 ms
- Convert last sequence: double tap Left Alt within 1000 ms
- Convert selection: double tap Left Shift within 1000 ms
- Autoconvert toggle: double tap Right Ctrl within 1000 ms
- When a double-tap sequence is also a prefix of an enabled triple-tap sequence,
  the double-tap action is deferred for at most 325 ms after the second tap.
- Switch keyboard layout: CapsLock

## Actions and behavior

### Convert last word

Behavior:
- Converts the last token captured by the input journal.
- Sleeps for the fixed 100 ms replacement delay before replacement.
- Replaces only the final run, preserving trailing suffix text.

### Convert selection

Algorithm (src/domain/text/convert.rs and selection probes):
- Probe selection text from the focused control using UI Automation and/or Win32 (Edit/RichEdit) APIs.
  - No synthetic input is sent.
  - Clipboard is not touched.
- Sleep for the fixed 100 ms replacement delay before conversion and replacement.
- Convert the probed text via mapping.
- Replace selection by:
  - Send Delete to remove the selection
  - Inject Unicode text via SendInput
  - Attempt to reselect the inserted text within a retry budget

This intentionally avoids paste via Ctrl+V to reduce interference with application specific paste behavior.

### Convert last sequence

Algorithm (src/domain/text/last_word.rs):
- Uses the input journal tokenization to determine the last sequence.
- Sleep for the fixed 100 ms replacement delay before conversion and replacement.
- Applies an input based replacement strategy (backspace and Unicode injection via SendInput).
- Clipboard is not used as the primary mechanism.

### Switch keyboard layout

Switches keyboard layout (Windows) for the current thread using the platform API.

`Switch keyboard layout` may use the same sequence as `Convert last word` and
`Convert selection`. For a shared sequence, Rust Switcher handles the actions
in this order: selected text, then a word on the current line, then a plain
layout switch when there is no text to convert.

### Autoconvert

- The low level keyboard hook maintains a ring buffer of recent tokens.
- When a trigger delimiter is typed, the hook posts a window message (WM_APP_AUTOCONVERT).
- The UI thread handles WM_APP_AUTOCONVERT and calls autoconvert_last_word only when Autoconvert enabled is true.
- A guard prevents double conversion of the same token.

### Autoconvert toggle

- The toggle hotkey flips runtime Autoconvert enabled.
- Disabling AutoConvert makes its pause hotkey unavailable and prevents automatic conversions until it is enabled again.
- Autoconvert enabled default is disabled on app start.
- Toggling shows an informational tray balloon.
- Tray double click triggers the same toggle.

## UI

Native Win32 UI with a single window and two group sections (custom painted frames):

### Settings group
- Autostart (checkbox)
- Start minimized (checkbox)
- Theme dark (checkbox)
- Smarter hotkeys (checkbox)
- Enable AutoConvert (checkbox; disables the Autoconvert pause field when off)

Every checkbox applies and saves its setting as soon as it is toggled. A failed
save restores the previous checkbox value and reports the error. Checkbox clicks
do not commit pending hotkey edits.

### Hotkeys group
- Read only displays for:
  - Convert last word
  - Convert last sequence
  - Convert selection (sequence)
  - Autoconvert pause
  - Switch layout (sequence)

Buttons:
- Apply: saves and activates only the hotkey edits
- Cancel: discards pending hotkey edits; immediately saved settings and runtime AutoConvert status remain unchanged
- Exit: closes the application

Note: there is currently no dedicated GitHub/"Report issue" button in the shipped UI.

Theme behavior:
- Theme changes and saves immediately when its checkbox is toggled.
- Theme can also be changed from tray menu.
- Tray theme switch persists theme_dark into config immediately (without overwriting other pending UI edits).

## Tray icon

- A tray icon is always added via Shell_NotifyIconW.
- Right click shows a context menu:
  - Toggle autoconvert
  - Show or Hide (toggles window visibility)
  - Change theme
  - Exit
- Left click is implemented:
  - Single click toggles window visibility (debounced with a timer to distinguish it from double click).
- Double click is implemented:
  - Toggles autoconvert.

## Autostart

- Implemented through the per-user `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  registry key.
- The `RustSwitcher` value starts the current executable with `--autostart`.
- A legacy Startup-folder shortcut is migrated on first launch of this version.

Persistence note:
- Autostart is NOT stored in config.json.
- The UI checkbox reflects the current Registry Run value.
- Toggling the checkbox creates or deletes the Registry value immediately.

## Notifications and errors

- Info notifications use tray balloon where possible.
- Error notifications are queued and drained on the UI thread via a single entry point.
- Fallback for tray failure is MessageBoxW.
- Notifications must not block hotkey critical paths.

## Logging

Current behavior:
- Tracing initialization exists for development only.
- When enabled, logs go to stderr.
- By default, release builds do not install any tracing subscriber.

How it is gated:
- Tracing initialization is guarded by debug assertions.
- The intended way to enable logs during development is to build and run with feature debug-tracing and use RUST_LOG.

## Known issues (current behavior)

- Autostart depends on the Registry value pointing to the current executable path,
  so relocating the executable breaks autostart until it is re-enabled in the UI.
