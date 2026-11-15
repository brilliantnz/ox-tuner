//! Firmware images: data and URLs only.
//!
//! The application has no flasher yet, so this module ships no execution: no
//! processes, no port handling, no writes. It records what a firmware image
//! is, which board it targets, and where its bytes come from, so the download
//! cache and a future flash worker share one vocabulary.

pub mod source;
pub mod types;

pub use source::FirmwareImageSource;
pub use types::{Board, FirmwareImage, FlashPlan, FlashTool};
