//! Fixture tests for the two supported dialects.
//!
//! Each fixture is a synthetic file that reproduces the *shapes* of the real
//! dialect (see the header of each fixture). The full model is compared against
//! a golden snapshot, so a change in what the parser understands produces a
//! reviewable diff instead of a silently different count.
//!
//! Regenerate the snapshots after an intended change:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test -p oxtuner-ini --test fixtures
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use oxtuner_ini::{CommandArguments, SettingsContext, parse_str};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"))
}

fn check_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().expect("golden dir")).expect("create golden dir");
        fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error}\nrun `UPDATE_GOLDEN=1 cargo test -p oxtuner-ini --test fixtures`",
            path.display()
        )
    });
    // Compare with LF only on Windows git may check the golden files out
    // with CRLF, while `summary()` always generates `\n`.
    assert_eq!(
        expected.replace("\r\n", "\n"),
        actual.replace("\r\n", "\n"),
        "the parsed model changed"
    );
}

#[test]
fn the_speeduino_dialect_parses_into_the_expected_model() {
    let definition = parse_str(&fixture("speeduino-like.ini"), &SettingsContext::new())
        .expect("the fixture parses");

    check_golden("speeduino-like", &definition.summary());

    assert_eq!(definition.signature, "speeduino 202501");
    assert_eq!(definition.signature_section, "MegaTune");
    assert_eq!(definition.spec_version, Some(3.78));
    assert_eq!(definition.endianness, oxtuner_ini::Endianness::Little);
    assert_eq!(definition.pages.len(), 2);

    // Settings-before-commands: the default branches win.
    assert_eq!(
        definition
            .page(1)
            .and_then(|page| page.burn_command.as_ref())
            .map(|c| c.raw.as_str()),
        Some("b%2i")
    );
    assert_eq!(definition.comms.blocking_factor, Some(251));

    let constant = definition.constant("idleAdvRPM").expect("idleAdvRPM");
    assert_eq!(constant.page, 1);
    assert_eq!(constant.address, Some(93));
    assert_eq!(constant.units.literal(), Some("rpm"));

    // A `bits` field expanded from a nested `#define`. Sixteen labels for a
    // five-bit field is legal: the remaining values count up as numbers.
    let delay = definition.constant("idleAdvDelay").expect("idleAdvDelay");
    assert_eq!(delay.options.len(), 16);

    // `lastOffset` overlays the previous constant instead of advancing.
    let afr = definition.constant("afrTable").expect("afrTable");
    assert_eq!(afr.address, Some(32));

    // A trailing storage keyword does not become a numeric attribute.
    let outputs = definition.constant("boardFuelOutputs").expect("outputs");
    assert!(outputs.flags.no_msq_save);
    assert_eq!(
        outputs
            .translate
            .as_ref()
            .and_then(oxtuner_ini::Number::literal),
        Some(0.0)
    );

    // A `string` without an offset is a length, not an address.
    let alias = definition.constant("AUXin00Alias").expect("alias");
    assert_eq!(alias.byte_size(), Some(20));

    // Expressions stay symbolic.
    let lambda = definition.constant("ego_min_lambda").expect("lambda");
    assert!(
        lambda
            .scale
            .as_ref()
            .is_some_and(oxtuner_ini::Number::is_dynamic)
    );

    assert_eq!(definition.channels.len(), 7);
    let squirting = definition.channel("nSquirts").expect("nSquirts");
    assert!(
        definition
            .channel("pulseLimit")
            .is_some_and(oxtuner_ini::Channel::is_computed)
    );
    assert!(matches!(
        squirting.kind,
        oxtuner_ini::ChannelKind::Measured { bits: Some(_), .. }
    ));

    let table = definition.table("veTable1Tbl").expect("table");
    assert_eq!(table.page, Some(2));
    assert_eq!(table.z_bins.as_deref(), Some("veTable"));
    assert_eq!(
        table.grid_orient.as_ref().map(|angles| angles.len()),
        Some(3)
    );

    assert_eq!(definition.curves.len(), 1);
    assert_eq!(definition.gauges.len(), 1);
    assert_eq!(definition.front_page.slots().len(), 2);
    assert_eq!(definition.front_page.indicators.len(), 1);
    assert_eq!(definition.dialogs.len(), 1);
    assert_eq!(definition.menus.len(), 1);
    assert_eq!(definition.menus[0].items.len(), 3);
    assert_eq!(definition.controller_commands.len(), 2);
    assert!(definition.extensions.is_read_only("engineMake"));

    // An unknown section is data, not a failure, and renders back.
    assert_eq!(definition.unsupported.len(), 1);
    assert_eq!(
        definition.unsupported[0].render(),
        "[EventTriggers]\n  trigger = something\n"
    );

    assert_eq!(definition.errors().count(), 0);
    for section in ["Constants", "OutputChannels", "TableEditor"] {
        let coverage = definition.coverage.section(section).expect(section);
        assert_eq!(coverage.percent(), 100.0, "{section}");
    }
}

#[test]
fn the_settings_that_rewrite_the_protocol_are_applied_before_commands() {
    let text = fixture("speeduino-like.ini");
    let definition = parse_str(
        &text,
        &SettingsContext::new().with_overrides([("COMMS_COMPAT", true), ("mcu_stm32", true)]),
    )
    .expect("parses");

    // `COMMS_COMPAT` rewrites the burn command…
    assert_eq!(
        definition
            .page(1)
            .and_then(|page| page.burn_command.as_ref())
            .map(|c| c.raw.as_str()),
        Some("B%2i")
    );
    // …and both `mcu_stm32` and `COMMS_COMPAT` rewrite the blocking factor.
    assert_eq!(definition.comms.blocking_factor, Some(121));

    let definition = parse_str(
        &text,
        &SettingsContext::new().with_overrides([("mcu_stm32", true)]),
    )
    .expect("parses");
    assert_eq!(definition.comms.blocking_factor, Some(121));
    assert_eq!(
        definition
            .page(1)
            .and_then(|page| page.burn_command.as_ref())
            .map(|c| c.raw.as_str()),
        Some("b%2i"),
        "mcu_stm32 alone must not change the burn command"
    );
}

#[test]
fn the_speeduino_commands_encode_to_the_documented_bytes() {
    let definition =
        parse_str(&fixture("speeduino-like.ini"), &SettingsContext::new()).expect("parses");

    let och_get = definition
        .comms
        .och_get_command
        .as_ref()
        .expect("ochGetCommand");
    let bytes = och_get
        .encode(
            &CommandArguments {
                offset: 0,
                count: 130,
                ..CommandArguments::default()
            },
            &|name| definition.command_variable(name),
        )
        .expect("encodes");

    // `r` `$tsCanId` `\x30` `%2o` `%2c` — offsets and counts are
    // little-endian on the wire (dialect default; `forceBigEndianProtocol`
    // flips them).
    assert_eq!(bytes, vec![b'r', 0x00, 0x30, 0x00, 0x00, 130, 0x00]);
}

#[test]
fn the_rusefi_dialect_parses_into_the_expected_model() {
    let definition = parse_str(&fixture("rusefi-like.ini"), &SettingsContext::new())
        .expect("the fixture parses");

    check_golden("rusefi-like", &definition.summary());

    // The signature lives in `[TunerStudio]`, not `[MegaTune]`.
    assert_eq!(definition.signature_section, "TunerStudio");
    assert!(definition.signature.starts_with("rusEFI"));

    // A file-level entry before the first section is kept, not rejected.
    assert_eq!(
        definition
            .comms
            .raw
            .get("enable2ndByteCanID")
            .map(String::as_str),
        Some("false")
    );

    // An empty burn command makes the page unbootable-to-burn, not missing.
    assert!(
        definition
            .page(1)
            .is_some_and(oxtuner_ini::Page::is_burnable)
    );
    assert!(definition.page(2).is_some_and(|page| !page.is_burnable()));

    // `;+help` between the key and the `=` must not swallow the entry.
    let mode = definition.constant("injectionMode").expect("injectionMode");
    assert_eq!(mode.options.len(), 2);

    // A `string` with an offset keeps it.
    let make = definition.constant("engineMake").expect("engineMake");
    assert_eq!(make.address, Some(1184));
    assert_eq!(make.byte_size(), Some(32));

    // PC variables are constants without an offset.
    assert_eq!(definition.pc_variables.len(), 3);
    let gear = definition.pc_variables.get("gearCountArray").expect("gear");
    assert_eq!(gear.shape.element_count(), Some(10));
    assert_eq!(
        definition
            .pc_variables
            .get("liveBaro")
            .and_then(|variable| variable.channel_value_on_connect.as_deref()),
        Some("baro")
    );

    // Table axes: a trailing `readOnly` is a flag, not a channel name.
    let table = definition.table("veTable1Tbl").expect("table");
    let x_axis = table.x_bins.as_ref().expect("x axis");
    assert_eq!(x_axis.array, "gearCountArray");
    assert!(x_axis.read_only);
    assert_eq!(x_axis.channel.as_deref(), Some("RPMValue"));

    // A panel declared outside a dialog becomes its own container.
    let panel = definition
        .dialogs
        .get("fuel_computerIndicatorPanel")
        .expect("panel");
    assert_eq!(panel.layout.as_deref(), Some("indicatorpanel"));
    assert_eq!(panel.rows.len(), 2);

    // The grouped menu keeps the dual-condition item.
    let menu = definition.menus.first().expect("menu");
    assert_eq!(menu.items.len(), 3);
    assert_eq!(menu.items[0].conditions.len(), 2);

    assert_eq!(definition.key_actions.len(), 1);
    assert_eq!(definition.key_actions[0].dialog, "veTableDialog");

    assert_eq!(definition.errors().count(), 0);
    for section in ["Constants", "OutputChannels", "PcVariables", "TableEditor"] {
        let coverage = definition.coverage.section(section).expect(section);
        assert_eq!(coverage.percent(), 100.0, "{section}");
    }
}

#[test]
fn a_fixture_report_is_stable_under_reparse() {
    for name in ["speeduino-like.ini", "rusefi-like.ini"] {
        let text = fixture(name);
        let first = parse_str(&text, &SettingsContext::new()).expect("parses");
        let second = parse_str(&text, &SettingsContext::new()).expect("parses");

        assert_eq!(first.summary(), second.summary(), "{name}");
    }
}

#[test]
fn an_uninterpretable_offset_is_never_a_valid_address() {
    let text = "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 288
page = 1
broken = scalar, U08, xyz(, \"ms\", 0.1, 0, 0, 25.5, 1
";

    let definition = parse_str(text, &SettingsContext::new()).expect("the file still parses");
    let constant = definition.constant("broken").expect("the constant is kept");

    assert_eq!(constant.offset, oxtuner_ini::Offset::Unknown);
    assert_eq!(
        constant.address, None,
        "a failed offset must not become a fabricated ECU address"
    );

    assert!(
        definition
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "bad-offset"),
        "the failure must be reported, not guessed away"
    );
}

#[test]
fn a_wide_option_list_keeps_its_values_and_is_shared() {
    // Speeduino spells out all 65 536 CAN addresses and references the define
    // from twenty entries. Two things must hold: a `bits` value is not a byte
    // (the table is a value → label map for the whole `[low:high]` range), and
    // the entries that share a define share one parsed table.
    let text = "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 32
page = 1
#define addresses = 256=\"0x100\", 300=\"0x12C\"
first = bits, U16, 0, [0:15], $addresses
second = bits, U16, 2, [0:15], $addresses
";

    let definition = parse_str(text, &SettingsContext::new()).expect("the file parses");
    let first = definition.constant("first").expect("first");
    let second = definition.constant("second").expect("second");

    assert_eq!(first.options.len(), 2);
    assert_eq!(first.options[0].value, 256);
    assert_eq!(first.options[0].label, "0x100");
    assert_eq!(first.options[1].value, 300);
    assert!(
        std::sync::Arc::ptr_eq(&first.options, &second.options),
        "entries sharing a `$define` must share the parsed option table"
    );
}
