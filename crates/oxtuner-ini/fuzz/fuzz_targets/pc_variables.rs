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

    // The summary names the variable; this pins the parsed fields themselves,
    // so the shape, scale and flags are compared field for field.
    assert_eq!(
        first.pc_variables.get("v"),
        second.pc_variables.get("v"),
        "the same entry text must produce the same variable"
    );

    if let Some(variable) = first.pc_variables.get("v") {
        assert!(
            !variable.raw.is_empty() && variable.line >= 1,
            "a stored variable keeps the source text and line it was read from"
        );
    }
});
