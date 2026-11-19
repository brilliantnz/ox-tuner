//! Fuzz the newer named sections.
//!
//! The full-pipeline targets rarely mutate into a valid `loggerDef` block, so
//! this one keeps every input on a `recordField` and a config-section entry.
//! The bit offsets, symbolic conditions and typed field split all run on the
//! raw text.
//!
//! Contract: arbitrary field text parses or is rejected, never panics, and the
//! same text always produces the same definition.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxtuner_ini::{SettingsContext, parse_str};

fuzz_target!(|data: &str| {
    let field = data.replace(['\n', '\r'], " ");
    let text = format!(
        "[MegaTune]\nsignature = \"fuzz\"\n[LoggerDefinition]\n\
         loggerDef = l, \"L\", composite\nrecordDef = 0, 0, 8\n\
         recordField = {field}\ncalcField = {field}\n\
         [PortEditor]\nentry = {field}\n"
    );

    let settings = SettingsContext::new();
    let first = parse_str(&text, &settings).map(|definition| definition.summary());
    let second = parse_str(&text, &settings).map(|definition| definition.summary());
    assert_eq!(first, second, "the same text parsed twice must give the same answer");
});
