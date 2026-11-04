# Rust Switcher

![Screenshot](assets/screenshots/overview.png)

Rust Switcher is a Windows 11 utility that converts text between your installed keyboard layouts.

It was created as an open-source alternative to Punto Switcher and Caramba
Switcher with a clear privacy requirement: keyboard input stays on your device.
**No telemetry, no usage analytics, and no input uploads.** Text conversion and
language scoring happen on your computer. English and Russian models are bundled;
models for additional installed languages are downloaded once and cached locally.
These downloads contain no typed text, selections, or input history.

## Features

- Convert selected text using keyboard layouts configured in Windows
- Convert the last typed sequence via a hotkey ("smart" conversion also handles selection)
- Auto-convert the last word while typing (can be disabled in Settings; runtime toggle starts paused)
- Automatically discover added and removed Windows layouts, including variants of the same language
- Load language models on demand, with local scoring and verified offline caches
- Tray icon and quick actions menu
- Light and dark UI themes
- Settings are saved to a config file
- Autostart

## Requirements

- Windows 11
- Rust nightly (see `rust-toolchain.toml`)
- MSVC toolchain (Visual Studio 2022 Build Tools)

## Install

### Via cargo

```powershell
cargo install rust-switcher
```

### From GitHub Releases

Download `rust-switcher.exe` from Releases.

## Configuration

The config file is stored at:

* `%APPDATA%\RustSwitcher\config.json`

Default hotkey sequences:

* Convert last word: double tap Left Shift
* Convert last sequence: double tap Left Alt
* Convert selection: double tap Left Shift
* Autoconvert toggle: double tap Right Ctrl
* Switch layout: CapsLock

To remove a binding, focus its hotkey field and press Backspace or Delete.

## Keyboard languages

Add or remove keyboard layouts in Windows. Rust Switcher uses that list directly;
there is no separate language list to maintain in the application. Changes are
picked up when the next conversion is requested, without restarting Rust Switcher.

Conversions use the physical key positions and the corresponding Windows layout
drivers. This preserves distinctions such as QWERTY, QWERTZ, AZERTY, and variants
sharing a language. Shift, Caps Lock, AltGr, and ordinary dead-key combinations
use the character and accent tables supplied by the installed keyboard driver.
Composition is tracked inside Rust Switcher. Legacy IME registrations and
unsupported keyboard-table formats are rejected; a transformation that cannot
be completed is left unchanged. Modern TSF composition and custom text services
are unsupported: a foreground keyboard-layout handle alone does not reliably
identify their composition state.
Custom keyboard drivers need a locally verifiable embedded or Windows catalog
signature to be loaded.

With several layouts installed, local character n-gram models rank possible
results. The model catalog covers 75 languages; only models relevant to installed
layouts are requested. English and Russian work immediately offline. Additional
models are loaded in the background from pinned Lingua model resources on GitHub,
verified against their expected size and SHA-256, and stored under
`%LOCALAPPDATA%\RustSwitcher\models\<model-hash>\<language>.fst`.

An explicit conversion has a deterministic fallback to the next usable Windows
layout when language evidence is ambiguous or unavailable, including punctuation.
Smart conversion and AutoConvert leave text unchanged when evidence is insufficient.
A network failure therefore does not block keyboard input or require a sign-in;
cached models remain usable offline.

The recent-input journal holds up to 4,096 characters. A conversion that would
include a prefix already dropped from the journal is rejected as a whole. Selecting
text uses a separate path with a 512-character, single-line limit.

## Privacy

**Your input stays on your device.** Rust Switcher processes keyboard input
and selected text locally and does not collect telemetry or usage analytics.
It does not upload typed text, selections, input history, or preferences to
the developer or third parties.

Recent input is held temporarily in memory to support conversion; preferences
and downloaded models are saved locally. Model downloads reveal the requested
model and ordinary connection metadata to the hosting service, but include no
input text. The [privacy policy](PRIVACY.md) explains processing, retention,
model downloads, clipboard behavior, and development logging.

Pausing or disabling AutoConvert leaves keyboard tracking and manual hotkeys
running. To stop all processing, right-click the tray icon and choose **Exit**;
closing the settings window only hides it.

## Development

This project includes a ready-to-use `bacon.toml` for a fast development loop.

### Bacon hotkeys (from `bacon.toml`)

* `d` dev-long
* `r` release-long
* `t` test-long
* `p` dushnota

What these jobs do:

* dev-long: fmt check, clippy, build (with debug tracing), run the app
* release-long: fmt check, clippy, release build, run the app
* test-long: fmt check, clippy, run tests
* dushnota: strict clippy

## Logging (development only)

```powershell
$env:RUST_LOG="trace"
cargo +nightly run -F debug-tracing
```

## License

MIT
