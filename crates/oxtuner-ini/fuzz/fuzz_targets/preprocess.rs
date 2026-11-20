//! Fuzz the preprocessor alone.
//!
//! The preprocessor owns the only loops in the crate (`#include` inlining,
//! `$define` expansion), so it is the place where a hostile input could hang
//! rather than panic. Any input must terminate.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtuner_ini::{Include, IncludeRequest, SettingsContext, preprocess};

fuzz_target!(|data: &str| {
    let settings = SettingsContext::new();
    let _ = preprocess(data, &settings, None);

    // A resolver that answers every request with the fuzz input itself, plus a
    // fixed two-file cycle. Both duplicate paths and cycles must stay bounded.
    let resolver = |request: IncludeRequest<'_>| {
        let text = match request.path {
            "a.inc" => "#include b.inc\n",
            "b.inc" => "#include a.inc\n",
            _ => data,
        };
        Some(Include {
            path: request.path.to_owned(),
            text: text.to_owned(),
        })
    };
    let _ = preprocess(data, &settings, Some(&resolver));
});
