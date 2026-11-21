#[cfg(windows)]
pub mod convert;
#[cfg(windows)]
pub mod last_word;
pub mod mapping;
#[cfg(windows)]
pub(crate) mod multilang;
#[cfg(windows)]
mod ranking;
#[cfg(windows)]
mod selection_probe;
#[cfg(windows)]
mod smart;

#[cfg(windows)]
pub use convert::{switch_keyboard_layout, wait_shift_released};
