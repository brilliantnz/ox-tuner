//! Fuzz the preprocessor alone.
//!
//! The preprocessor owns the only loops in the crate (`#include` inlining,
//! `$define` expansion), so it is the place where a hostile input could hang
//! rather than panic. Any input must terminate.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtuner_ini::{SettingsContext, preprocess};

fuzz_target!(|data: &str| {
    let _ = preprocess(data, &SettingsContext::new(), None);
});
