//! Fuzz the byte entry point, including invalid UTF-8.
//!
//! Real definitions are not reliably UTF-8 (Prototype P3, finding 1), so the
//! decoder is part of the attack surface: arbitrary bytes must decode and parse
//! without panicking.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtuner_ini::{SettingsContext, parse_bytes};

fuzz_target!(|data: &[u8]| {
    let _ = parse_bytes(data, &SettingsContext::new());
});
