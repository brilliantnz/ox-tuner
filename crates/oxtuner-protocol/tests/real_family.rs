//! Picks a behavior for the real reference definitions when they are available.
//!
//! The INIs are third-party files that ox-tuner does not bundle, so they are
//! never checked in. The test looks for them in the git-ignored study folder
//! (`docs/tmp/local/ini/`) or at the path in the matching environment variable.
//! When a file is missing the test reports the skip and moves on; set
//! `OXTUNER_INI_REQUIRED=1` to turn a missing fixture into a hard failure.
//!
//! These files are what prove the family order: FOME reports a signature that
//! starts with `rusEFI`, so it only lands on the FOME behavior because it is
//! tried first.

#![expect(
    clippy::print_stdout,
    reason = "the integration test prints a readable report for the developer running it"
)]

use std::path::PathBuf;

use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_protocol::{BehaviorKind, Match, ReplyCode};

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
        Definition::parse_bytes(&bytes, &SettingsContext::new()).expect("the reference INI parses"),
    )
}

#[test]
fn a_real_speeduino_definition_selects_the_speeduino_behavior() {
    let Some(definition) = load(
        "OXTUNER_INI_SPEEDUINO",
        "../../docs/tmp/local/ini/speeduino-202501.7.ini",
    ) else {
        println!("skipped: no local Speeduino INI");
        return;
    };

    let behavior = BehaviorKind::select(&definition).expect("a family claims it");

    assert!(matches!(behavior, BehaviorKind::Speeduino(_)));
    assert_eq!(behavior.as_dyn().id(), "speeduino");
    assert_eq!(behavior.as_dyn().name(), "Speeduino");
    assert_eq!(behavior.as_dyn().matches(&definition), Match::Exact);
    assert_eq!(behavior.as_dyn().reply_code(), ReplyCode::Leading);
}

#[test]
fn a_real_rusefi_definition_selects_the_rusefi_behavior() {
    let Some(definition) = load("OXTUNER_INI_RUSEFI", "../../docs/tmp/local/ini/rusefi.ini") else {
        println!("skipped: no local rusEFI INI");
        return;
    };

    let behavior = BehaviorKind::select(&definition).expect("a family claims it");

    assert!(matches!(behavior, BehaviorKind::Rusefi(_)));
    assert_eq!(behavior.as_dyn().id(), "rusefi");
    assert_eq!(behavior.as_dyn().name(), "rusEFI");
    assert_eq!(behavior.as_dyn().reply_code(), ReplyCode::Absent);
}

#[test]
fn a_real_fome_definition_selects_the_fome_behavior_before_rusefi() {
    let Some(definition) = load(
        "OXTUNER_INI_FOME",
        "../../docs/tmp/local/ini/fome_proteus_h7.ini",
    ) else {
        println!("skipped: no local FOME INI");
        return;
    };

    let behavior = BehaviorKind::select(&definition).expect("a family claims it");

    assert!(matches!(behavior, BehaviorKind::Fome(_)));
    assert_eq!(behavior.as_dyn().id(), "fome");
    assert_eq!(behavior.as_dyn().name(), "FOME");
    assert_eq!(behavior.as_dyn().reply_code(), ReplyCode::Absent);
}
