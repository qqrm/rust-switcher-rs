#![allow(
    clippy::multiple_crate_versions,
    reason = "The binary integrates Win32 and NLP crates that currently pull parallel transitive major versions."
)]

// Library target is intentionally minimal and cross-platform.
//
// The Windows application lives in `src/main.rs` and declares its own module tree.
// If we compile the Windows app modules here, `cargo check` / `cargo clippy --all-targets`
// will build the library target first and hit `dead_code` cascades under `-D warnings`.
//
// The only code that belongs in the shared library right now is `rust-switcher-core`.

pub use rust_switcher_core as core;

#[cfg(windows)]
#[path = "platform/win/layout_library.rs"]
mod layout_library;
#[cfg(windows)]
#[path = "input/layout_tables.rs"]
mod layout_tables;

// Compatibility shim for existing unit tests that still refer to
// `crate::domain::text::mapping::*`.
pub mod domain {
    pub mod text {
        pub mod mapping {
            pub use rust_switcher_core::text::mapping::*;
        }
    }
}

#[path = "input/ring_buffer.rs"]
pub mod ring_buffer;

// Compatibility shim for unit tests that still refer to
// `crate::input::ring_buffer::*`.
pub mod input {
    pub use super::ring_buffer;
}

#[cfg(test)]
mod core_tests;

// Exercise the real model and cache fixtures in the cross-platform test jobs.
#[cfg(test)]
#[path = "domain/text/ranking.rs"]
mod ranking;

// The asynchronous capture transport is platform-independent; exercise stalled
// providers and queue expiry in the Linux test jobs too.
#[cfg(test)]
#[path = "utils/bounded_worker.rs"]
mod bounded_worker;
