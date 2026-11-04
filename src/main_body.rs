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
    if std::env::args().any(|arg| arg == "--quit-for-update") {
        let _ = platform::win::request_running_instance_exit();
        return Ok(());
    }

    utils::tracing::init_tracing();
    utils::helpers::init_app_user_model_id()?;

    let Some(_guard) = utils::helpers::single_instance_guard()? else {
        let _ = platform::win::activate_running_instance();
        return Ok(());
    };

    let autostart_hidden = std::env::args().any(|arg| arg == platform::win::AUTOSTART_ARG);

    let cfg_hidden = config::load()
        .ok()
        .map(|c| c.start_minimized)
        .unwrap_or(false);

    let start_hidden = autostart_hidden || cfg_hidden;

    platform::win::run(start_hidden)
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "rust-switcher is a Windows-only binary. Linux CI should run `cargo test --lib --tests`."
    );
}

#[cfg(all(test, windows))]
mod tests;
