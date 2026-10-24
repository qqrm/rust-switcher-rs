# Rust Switcher privacy policy

Last updated: October 24, 2026. This policy describes Rust Switcher 1.0.20,
published by [QQRM](https://github.com/qqrm), including its portable Windows
release. It covers the application itself; GitHub, package managers, Windows,
and applications in which you type have their own data handling practices.

## Local processing; no telemetry or data uploads

**Your keyboard input stays on your device.** Rust Switcher works offline and
does not collect telemetry or usage analytics. The application does not send
typed text, selections, input history, preferences, or other user data to
QQRM or third parties. There is no server-side collection of your input by
the application.

The keyboard hook and temporary input journal exist to convert text on your
computer. The local memory and settings described below do not upload data
or create a remote record of your activity.

## Text and other information the application accesses

Rust Switcher processes text locally to recognize hotkeys, track recently typed
text, and convert Russian and English keyboard layouts. While the release
application is running, a global keyboard hook receives keyboard events from
other applications. It uses key codes, modifier state, keyboard layout, and
characters derived from those events. Window, process, thread, focus, mouse,
and caret information help track the editing context and target conversions.

Selection conversion reads text from the focused control using Windows UI
Automation or native Edit/RichEdit control messages. A conversion accepts at
most 512 characters and rejects multiline selections. The native control path
can temporarily read the control's entire text, up to 1,048,576 UTF-16 code
units, to extract the selected part. The 512-character conversion limit is
therefore not a limit on all text accessed during selection detection.

Converted text is sent back to the focused application using simulated
keyboard input. That application may store or transmit it according to its
own settings and policies. Rust Switcher does not guarantee that sensitive
fields, including password fields, are excluded from keyboard processing.

## Memory and retention

- The active input journal keeps up to 100 Unicode characters, together with
  layout and editing context. Older text is dropped as the journal reaches
  its limit. The active journal has no two-minute expiry timer.
- When the editing focus changes, the application may cache a copy of that
  context's journal so recent text can be restored when you return. Each
  cached journal has the same 100-character limit; multiple contexts can
  retain more than 100 characters in total.
- Cached entries expire after two minutes from caching. Expiry is checked
  when the application processes a relevant focus change, not by a background
  deletion timer. Expired entries may therefore remain in memory until that
  check or process exit, but are removed before cache restoration.
- Selection buffers and selection metadata are separate from the journal.
  UI Automation selection references can remain until a later probe replaces
  them or the process exits.

The application does not save these journals or selections to a history file
or its settings file. Exiting ends their live process storage. Rust Switcher
does not securely overwrite freed memory or control copies made by Windows,
such as paging, crash dumps, or backups.

## Clipboard

Rust Switcher 1.0.20 does not read, write, clear, or restore the Windows
clipboard. Selection conversion uses the Windows text interfaces described
above and simulated input; it has no clipboard fallback and does not send
Ctrl+C or Ctrl+V to perform conversion.

## Saved settings and autostart

Preferences such as hotkeys, conversion options, timing, theme, and startup
window behavior are stored in a plain text settings file at
`%APPDATA%\RustSwitcher\config.json`. Development builds use
`%APPDATA%\RustSwitcherDebug\config.json`. These files contain preferences,
not a record of typed text. Saving uses a temporary file in the same folder.

Enabling **Start on startup** stores the executable path and `--autostart`
argument in the current user's Windows registry, in the `RustSwitcher` value
under `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`.
Older installations may have a `RustSwitcher Autostart.lnk` shortcut in the
user's Startup folder; the application migrates or removes that shortcut.
Autostart is separate from the settings file.

To remove saved preferences and stop future automatic launches:

1. Clear **Start on startup** in the application.
2. Right-click the tray icon and choose **Exit**.
3. Delete `%APPDATA%\RustSwitcher\config.json` (or the dedicated
   `%APPDATA%\RustSwitcher` folder). If you used development builds, remove
   their separate settings folder too.

Launching again recreates default preferences. Deleting only the executable
or settings file does not remove the autostart registration. Logs you chose
to capture during development must be deleted from wherever you saved them.

## Pause, disable, and exit controls

The runtime AutoConvert toggle starts paused. You can toggle it through
**AutoConvert** in the tray menu, by double-clicking the tray icon, or with
the configured hotkey (double Right Ctrl by default), when the feature is
enabled. To disable automatic conversion across launches, clear
**Enable AutoConvert** in Settings and apply the change. To remove an
individual hotkey binding, focus its field, press Backspace or Delete, and
apply the change.

Pausing or disabling AutoConvert stops automatic conversion; it does not
uninstall the keyboard hook, stop the input journal, clear every cache, or
disable other configured hotkeys. To stop all processing by Rust Switcher,
choose **Exit** from the tray menu. Closing or minimizing the settings window
hides it and leaves the application running. Exit before entering sensitive
text if you do not want Rust Switcher to process that input.

## Development logging

The published release does not enable tracing output. Debug builds with the
`debug-tracing` feature can write diagnostics to standard error, controlled
by `RUST_LOG`. These diagnostics may include original and converted text,
keyboard layouts, and window or caret information. A terminal, development
tool, or redirection configured by the developer may retain that output.
Avoid sensitive input during such sessions and remove captured logs when
they are no longer needed.

## Transmission and security

The application performs conversion on the device. It has no telemetry,
analytics, automatic crash uploads, or network transmission of typed text,
selections, journals, or preferences to QQRM or other recipients. Downloading
or updating through GitHub or a package manager involves those services
separately. Information you choose to submit in a support report is also
separate from application processing.

Text in memory and saved preferences are not encrypted by Rust Switcher.
They rely on Windows account and process protections. The application checks
the focused editing context before replacing text and bounds journal size
and selection operations, but these measures do not provide secure erasure
or protection against software with access to your account or process.

## Questions and policy updates

For questions about this policy, use the
[project issue tracker](https://github.com/qqrm/rust-switcher-rs/issues).
Issues are public: do not include passwords, private text, or unreviewed
diagnostic logs. Changes to this policy will be published in this file with
an updated date and remain visible in the repository history.
