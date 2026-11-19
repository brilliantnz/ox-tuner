//! Builds the tune layout from the real reference definitions.
//!
//! The INIs are third-party files that ox-tuner does not bundle, so they are
//! never checked in. The test looks for them in the git-ignored study folder
//! (`docs/tmp/local/ini/`) or at the path in the matching environment variable.
//! When a file is missing the test reports the skip and moves on; set
//! `OXTUNER_INI_REQUIRED=1` to turn a missing fixture into a hard failure.
//!
//! The reference definitions are what a user actually loads, so they must build
//! a layout with no ambiguous byte overlap: such a definition would load and
//! read fine, then refuse every write to the page. A failure on a newer official
//! INI means the detector needs refining rather than the assertion weakening.

#![expect(
    clippy::print_stdout,
    reason = "the integration test prints a readable report for the developer running it"
)]

use std::path::PathBuf;

use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_tune::{LayoutIssueKind, TuneLayout};

fn load(variable: &str, fallback: &str) -> Option<Definition> {
    let path = std::env::var(variable).map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fallback),
        PathBuf::from,
    );

    let Ok(bytes) = std::fs::read(&path) else {
        let required = std::env::var_os("OXTUNER_INI_REQUIRED").is_some_and(|value| value == "1");
        assert!(
            !required,
            "OXTUNER_INI_REQUIRED=1 but `{}` is unreadable: provide the reference INIs \
             or unset the variable",
            path.display()
        );
        return None;
    };

    Some(
        Definition::parse_bytes(&bytes, &SettingsContext::new())
            .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    )
}

/// Builds the layout and asserts it placed the definition and grouped the
/// shared words the reference INIs really contain.
fn assert_writable_layout(definition: &Definition) {
    let layout = TuneLayout::build(definition, &|_| None);
    assert!(
        layout.variables().len() > 500,
        "the definition must place its constants, not skip them"
    );

    let overlaps: Vec<String> = layout
        .issues()
        .iter()
        .filter(|issue| issue.kind == LayoutIssueKind::AmbiguousOverlap)
        .map(ToString::to_string)
        .collect();

    assert!(
        overlaps.is_empty(),
        "the definition must build a writable layout: {overlaps:?}"
    );
}

#[test]
fn a_real_speeduino_definition_has_no_ambiguous_overlaps() {
    let Some(definition) = load(
        "OXTUNER_INI_SPEEDUINO",
        "../../docs/tmp/local/ini/speeduino-202501.7.ini",
    ) else {
        println!("skipped: no local Speeduino INI");
        return;
    };

    assert_writable_layout(&definition);
}

#[test]
fn a_real_rusefi_definition_has_no_ambiguous_overlaps() {
    let Some(definition) = load("OXTUNER_INI_RUSEFI", "../../docs/tmp/local/ini/rusefi.ini") else {
        println!("skipped: no local rusEFI INI");
        return;
    };

    assert_writable_layout(&definition);
}
