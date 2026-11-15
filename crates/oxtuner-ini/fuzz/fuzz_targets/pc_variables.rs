//! Fuzz a `[PcVariables]` entry.
//!
//! The entry format is the `[Constants]` one without an offset, and a file may
//! still write an offset. The shape split and the attribute split therefore
//! both run on user text, and the parser has to keep the positions straight for
//! `scalar`, `array`, `bits` and `string`. Keeping the input on that line makes
//! the target exercise all four instead of waiting for the full pipeline to
//! generate one.
//!
//! Contract: arbitrary entry text parses or is rejected, never panics, and the
//! same text always produces the same model. A line the parser warns about, or
//! one that is only a comment, is dropped by design; the target must not claim
//! it is stored.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtuner_ini::{SettingsContext, parse_str};

fuzz_target!(|data: &str| {
    // One line: a newline would start the next INI line instead of extending
    // the entry.
    let rest = data.replace(['\n', '\r'], " ");
    let text = format!("[MegaTune]\nsignature = \"fuzz\"\n[PcVariables]\nv = {rest}\n");

    let Ok(first) = parse_str(&text, &SettingsContext::new()) else {
        return;
    };
    let Ok(second) = parse_str(&text, &SettingsContext::new()) else {
        panic!("the same text parsed twice must give the same answer");
    };

    assert_eq!(
        first.summary(),
        second.summary(),
        "a PC variable must not depend on hidden state"
    );

    if let Some(variable) = first.pc_variables.get("v") {
        assert_eq!(
            variable.name, "v",
            "a stored PC variable keeps the name it declared"
        );
    }
});
