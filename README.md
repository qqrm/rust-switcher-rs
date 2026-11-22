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
- Convert recently typed text in ordinary and password fields, with input-context checks that cancel stale conversions after navigation ([behavior and focus limitations](docs/security.md))
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

The standalone executable is `rust-switcher.exe`. The installer build adds
`rust-switcher-setup.exe` for a per-user installation when its release is published.

### Via WinGet or UniGetUI

```powershell
winget install --id QQRM.RustSwitcher --exact --source winget --scope user
```

The installer updates a running instance automatically and restarts it only if
it was running before the update. It preserves the existing autostart setting
and configuration. Existing per-user WinGet portable installations are migrated
in their current directory, keeping their command alias. A fresh silent install
does not launch the app or enable autostart. UniGetUI uses the same WinGet package.

This behavior reaches UniGetUI after the installer release is published and its
manifest is accepted in `microsoft/winget-pkgs`. The existing catalog release
1.0.29 is still a portable EXE; merging the source change alone does not update
that catalog entry.

### Updating

Right-click the tray icon and select **Check for updates**. When a newer stable
GitHub Release is available, Rust Switcher verifies the download's size and
GitHub-published SHA-256 digest. An installer-managed installation uses
`rust-switcher-setup.exe`, which also updates the installed version reported to
WinGet. A standalone or Cargo installation uses `rust-switcher.exe`; a detached
PowerShell helper replaces it and restarts the app, retaining the previous
executable for rollback until the replacement launches.

Updates require Windows PowerShell 5.1, HTTPS access to GitHub, and write access
to the installation directory. The update log is saved to
`%LOCALAPPDATA%\RustSwitcher\updater.log`. Debug builds cannot self-update.

External `cargo install rust-switcher --force` still cannot replace a running
executable. Exit Rust Switcher first when installing through Cargo.

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

Starting the executable again restarts Rust Switcher: the existing process exits
and a fresh process takes over. If the old UI does not respond within two seconds,
the launcher terminates that instance before starting. Saved settings are retained;
unsaved UI edits and the in-memory input journal are reset. Concurrent launches are
serialized, and debug and release applications retain separate identities.

The tray icon is re-registered when Windows recreates the taskbar, after resume,
and when the display configuration changes. Recovery retains the current icon and
tooltip and retries briefly if the shell is not ready yet.

## Development

This project includes a ready-to-use `bacon.toml` for a fast development loop.

### Windows acceptance checks

CI and release preparation exercise the actual autostart command and WinGet
upgrades on a disposable Windows runner. Candidate installers are served by an
isolated trusted REST source generated from the production manifests; WinGet
still performs package selection and hash verification. The source's HTTPS and
protocol tests and the native Inno process snapshot controls are required before
package-manager acceptance. See the
[E2E coverage and setup](scripts/e2e/README.md#autostart-and-package-manager-updates)
for prerequisites, temporary certificate handling and the limits of these checks.

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
