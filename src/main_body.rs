#[cfg(windows)]
mod app;
#[cfg(windows)]
mod app_identity;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod conversion;
#[cfg(windows)]
mod domain;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod input_journal;
#[cfg(windows)]
#[path = "platform/win/layout_library.rs"]
mod layout_library;
#[cfg(windows)]
#[path = "input/layout_tables.rs"]
mod layout_tables;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod utils;

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--quit-for-update") {
        let expected_pid = if let Some(index) = args.iter().position(|arg| arg == "--parent-pid") {
            let pid = args
                .get(index + 1)
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|pid| *pid != 0)
                .ok_or_else(|| {
                    windows::core::Error::new(
                        windows::core::HRESULT(0x8007_0057_u32.cast_signed()),
                        "Invalid updater parent process ID",
                    )
                })?;
            Some(pid)
        } else {
            None
        };
        if !platform::win::request_running_instance_exit(expected_pid)? && expected_pid.is_some() {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x8007_0057_u32.cast_signed()),
                "The updater's original process is no longer running",
            ));
        }
        return Ok(());
    }

    utils::tracing::init_tracing();
    utils::helpers::init_app_user_model_id()?;

    let replace_existing = !args.iter().any(|arg| arg == "--ensure-running");
    let Some(mut guard) = utils::instance::SingleInstanceGuard::acquire(replace_existing)? else {
        return Ok(());
    };

    let autostart_hidden = std::env::args().any(|arg| arg == platform::win::AUTOSTART_ARG);

    let cfg_hidden = config::load()
        .ok()
        .map(|c| c.start_minimized)
        .unwrap_or(false);

    let start_hidden = autostart_hidden || cfg_hidden;

    platform::win::run(start_hidden, &mut guard)
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "rust-switcher is a Windows-only binary. Linux CI should run `cargo test --lib --tests`."
    );
}

#[cfg(all(test, windows))]
mod tests;
