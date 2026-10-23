//! Fuzz the whole parse pipeline from text.
//!
//! Contract: for **any** UTF-8 input the function returns a definition or a
//! structured error. It must never panic — a panic in a parser is the one bug a
//! user cannot work around, because it takes the whole definition down.
//!
//! Seeds: `cargo fuzz run parse_ini corpus/parse_ini`
//! A local corpus can be seeded from the reference INIs, which are never
//! committed:
//!
//! ```sh
//! mkdir -p corpus/parse_ini
//! cp ../../../docs/tmp/local/ini/*.ini corpus/parse_ini/
//! ```

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtuner_ini::{SettingsContext, parse_str};

fuzz_target!(|data: &str| {
    let context = SettingsContext::new();
    let _ = parse_str(data, &context);

    // Parsing the same text twice must agree; a parser with hidden state would
    // produce a different model the second time in a real application.
    if let Ok(first) = parse_str(data, &context)
        && let Ok(second) = parse_str(data, &context)
    {
        assert_eq!(first.summary(), second.summary());
    }
});
