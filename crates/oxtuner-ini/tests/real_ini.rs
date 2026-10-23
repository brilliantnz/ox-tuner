//! Parses the real reference definitions **when they are available locally**.
//!
//! The INIs are third-party files that ox-tuner does not bundle (ADR-0006
//! decision 7), so they are never checked in. The test looks for them in the
//! git-ignored study folder (`docs/tmp/local/ini/`) or at the path in
//! `OXTUNER_INI_SPEEDUINO` / `OXTUNER_INI_RUSEFI`. When neither exists the test
//! reports that it was skipped — CI stays green without pretending. Set
//! `OXTUNER_INI_REQUIRED=1` (e.g. in a pre-release integration job) to turn a
//! missing fixture into a hard failure; pinned fixture versions are recorded in
//! `docs/development/ini/real-ini-fixtures.md`.

use std::path::PathBuf;

use oxtuner_ini::{SettingsContext, parse_bytes};

fn load(variable: &str, fallback: &str) -> Option<(String, Vec<u8>)> {
    let path = std::env::var(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(fallback));

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        // Integration mode: a missing fixture is a failure, not a skip, so a
        // compatibility run can never silently pass by parsing nothing.
        Err(_) if std::env::var_os("OXTUNER_INI_REQUIRED").is_some_and(|v| v == "1") => panic!(
            "OXTUNER_INI_REQUIRED=1 but `{}` is unreadable — provide the reference INIs \
             (see docs/development/ini/real-ini-fixtures.md) or unset the variable",
            path.display()
        ),
        Err(_) => return None,
    };
    Some((path.display().to_string(), bytes))
}

fn report(name: &str, definition: &oxtuner_ini::Definition) {
    println!("\n== {name} ==");
    println!("signature      : {}", definition.signature);
    println!("spec version   : {:?}", definition.spec_version);
    println!("endianness     : {}", definition.endianness);
    println!("pages          : {}", definition.pages.len());
    println!("constants      : {}", definition.constants.len());
    println!("pc variables   : {}", definition.pc_variables.len());
    println!("channels       : {}", definition.channels.len());
    println!("tables         : {}", definition.tables.len());
    println!("curves         : {}", definition.curves.len());
    println!("gauges         : {}", definition.gauges.len());
    println!("dialogs        : {}", definition.dialogs.len());
    println!("menus          : {}", definition.menus.len());
    println!("datalog fields : {}", definition.datalog_entries.len());
    println!("commands       : {}", definition.controller_commands.len());
    println!("unsupported    : {}", definition.unsupported.len());
    println!("coverage       : {:.2}%", definition.coverage.percent());
    for section in &definition.coverage.sections {
        println!(
            "  {:<24} {:>5}/{:<5} {:>6.2}%",
            section.name,
            section.understood,
            section.entries,
            section.percent()
        );
    }

    let mut errors = 0;
    let mut warnings = 0;
    for diagnostic in &definition.diagnostics {
        match diagnostic.severity {
            oxtuner_ini::Severity::Error => errors += 1,
            oxtuner_ini::Severity::Warning => warnings += 1,
            oxtuner_ini::Severity::Note => {}
        }
        if diagnostic.severity != oxtuner_ini::Severity::Note {
            println!("  {diagnostic}");
        }
    }
    println!("errors: {errors}, warnings: {warnings}");
}

#[test]
fn the_reference_speeduino_ini_parses() {
    let Some((path, bytes)) = load(
        "OXTUNER_INI_SPEEDUINO",
        "../../docs/tmp/local/ini/speeduino.ini",
    ) else {
        println!("skipped: no local Speeduino INI");
        return;
    };

    let definition = parse_bytes(&bytes, &SettingsContext::new())
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    report(&path, &definition);

    assert!(definition.signature.starts_with("speeduino"));
    assert!(
        definition.constants.len() > 500,
        "constants: {}",
        definition.constants.len()
    );
    assert!(
        definition.channels.len() > 100,
        "channels: {}",
        definition.channels.len()
    );
    assert!(
        definition.tables.len() >= 10,
        "tables: {}",
        definition.tables.len()
    );
    assert_eq!(definition.pages.len(), 15);

    // The settings-before-commands rule (ADR-0006): `COMMS_COMPAT` selects the
    // `B` burn command, `mcu_stm32` the smaller blocking factor.
    let default_burn = definition
        .page(1)
        .and_then(|page| page.burn_command.as_ref());
    assert_eq!(
        default_burn.map(|command| command.raw.as_str()),
        Some("b%2i")
    );
    assert_eq!(definition.protocol.blocking_factor, Some(251));

    let compatible = parse_bytes(
        &bytes,
        &SettingsContext::new().with_overrides([("COMMS_COMPAT", true), ("mcu_stm32", true)]),
    )
    .expect("parses with compatibility settings");

    assert_eq!(
        compatible
            .page(1)
            .and_then(|page| page.burn_command.as_ref())
            .map(|command| command.raw.as_str()),
        Some("B%2i"),
        "COMMS_COMPAT must rewrite burnCommand"
    );
    assert_eq!(
        compatible.protocol.blocking_factor,
        Some(121),
        "mcu_stm32 must rewrite blockingFactor"
    );

    for section in ["Constants", "OutputChannels", "TableEditor"] {
        let coverage = definition.coverage.section(section).expect(section);
        assert!(
            coverage.percent() >= 95.0,
            "{section} coverage is {:.2}%",
            coverage.percent()
        );
    }
}

#[test]
fn the_reference_project_ini_parses() {
    // The second reference definition the spec names: the copy that lives
    // inside a real TunerStudio project (`projectCfg/mainController.ini`).
    // It is an older revision of the same firmware definition, so parsing it
    // as well proves the parser is not tuned to one snapshot.
    let Some((path, bytes)) = load(
        "OXTUNER_INI_PROJECT",
        "../../docs/tmp/local/tunerstudio/projects/Sonic_150/projectCfg/mainController.ini",
    ) else {
        println!("skipped: no local TunerStudio project INI");
        return;
    };

    let definition = parse_bytes(&bytes, &SettingsContext::new())
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    report(&path, &definition);

    assert!(definition.signature.starts_with("speeduino"));
    assert_eq!(definition.signature_section, "MegaTune");
    assert_eq!(definition.pages.len(), 15);
    assert!(definition.constants.len() > 500);
    assert!(definition.tables.len() >= 10);

    // A `bits` option list is a value → label table, so it may be wider than
    // a byte: `[0:15]` with all 65 536 CAN addresses spelled out. The values
    // must survive that width, and the twenty entries that reference the same
    // `$define` must share one parsed table instead of copying it.
    let can = definition
        .constant("caninput_source_can_address0")
        .expect("CAN address constant");
    assert_eq!(can.options.len(), 65_536);
    assert_eq!(can.options[0].label, "0x100");
    assert_eq!(can.options[256].label, "0x200");
    assert_eq!(can.options[256].value, 256);

    let other = definition
        .constant("caninput_source_can_address1")
        .expect("second CAN address constant");
    assert!(
        std::sync::Arc::ptr_eq(&can.options, &other.options),
        "entries sharing a `$define` must share the parsed option table"
    );

    for section in ["Constants", "OutputChannels", "TableEditor"] {
        let coverage = definition.coverage.section(section).expect(section);
        assert!(
            coverage.percent() >= 95.0,
            "{section} coverage is {:.2}%",
            coverage.percent()
        );
    }
}

#[test]
fn the_reference_rusefi_ini_parses() {
    let Some((path, bytes)) = load("OXTUNER_INI_RUSEFI", "../../docs/tmp/local/ini/rusefi.ini")
    else {
        println!("skipped: no local rusEFI INI");
        return;
    };

    let definition = parse_bytes(&bytes, &SettingsContext::new())
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    report(&path, &definition);

    // rusEFI keeps its signature in `[TunerStudio]`, not `[MegaTune]`.
    assert_eq!(definition.signature_section, "TunerStudio");
    assert!(definition.signature.starts_with("rusEFI"));
    assert!(
        definition.constants.len() > 500,
        "constants: {}",
        definition.constants.len()
    );
    assert!(
        definition.channels.len() > 300,
        "channels: {}",
        definition.channels.len()
    );
    assert_eq!(definition.pages.len(), 5);

    // Pages 2 and 3 declare an empty burn command: they cannot be burned.
    assert!(definition.page(1).is_some_and(|page| page.is_burnable()));
    assert!(definition.page(2).is_some_and(|page| !page.is_burnable()));

    // The `.ini` is not valid UTF-8 (a raw 0xB1 in a help string).
    assert!(
        definition
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "non-utf8"),
        "the lossy decode must be reported"
    );

    for section in ["Constants", "OutputChannels", "TableEditor"] {
        let coverage = definition.coverage.section(section).expect(section);
        assert!(
            coverage.percent() >= 95.0,
            "{section} coverage is {:.2}%",
            coverage.percent()
        );
    }
}
