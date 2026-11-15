//! Fuzz a `bits` option list.
//!
//! The full-pipeline targets only reach this code when a mutation happens to
//! produce a valid `bits` line, which is rare. This target keeps every input
//! on the option list itself, so the label splitter (commas, the quoted-period
//! separator, explicit `3="x"` values) and the shared-table cache get coverage
//! instead of luck.
//!
//! Contract: arbitrary option text parses or is rejected, never panics, and the
//! same text always produces the same option table.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtuner_ini::{SettingsContext, parse_str};

fuzz_target!(|data: &str| {
    // Newlines would end the entry line, so the input is flattened into one
    // field list. Everything else — quotes, `=`, periods, braces — is kept.
    let options = data.replace(['\n', '\r'], " ");
    let text = format!(
        "[MegaTune]\nsignature = \"fuzz\"\n[Constants]\nnPages = 1\npageSize = 64\npage = 1\n\
         field = bits, U08, 0, [0:7], {options}\n"
    );

    let Ok(first) = parse_str(&text, &SettingsContext::new()) else {
        return;
    };
    let Ok(second) = parse_str(&text, &SettingsContext::new()) else {
        panic!("the same text parsed twice must give the same answer");
    };

    assert_eq!(
        first.summary(),
        second.summary(),
        "the option list must not depend on hidden state"
    );
});
