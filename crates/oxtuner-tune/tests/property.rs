//! Property tests: any sequence of edits must leave the derived states
//! consistent with the images they were derived from (ADR-0010).
//!
//! The exhaustive test in `images.rs` covers one cell; this covers arbitrary
//! edits across scalars and a table, including edits applied to the ECU images
//! out of order.

#![expect(
    clippy::unwrap_used,
    clippy::float_cmp,
    reason = "the property test unwraps fixture setup and compares model values exactly"
)]

use std::sync::Arc;

use oxtuner_ini::{SettingsContext, parse_str};
use oxtuner_tune::{CellState, Tune, TuneDiff, TuneImages, TuneLayout};
use proptest::prelude::*;

const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 3, 0
b = scalar, U08, 1, "", 1, 0, 0, 3, 0
table = array, U08, 2, [2x2], "", 1, 0, 0, 3, 0
"#;

/// Every addressable cell, so a generated edit always names a real one.
const CELLS: [(&str, usize); 6] = [
    ("a", 0),
    ("b", 0),
    ("table", 0),
    ("table", 1),
    ("table", 2),
    ("table", 3),
];

fn layout() -> Arc<TuneLayout> {
    let definition = parse_str(DEFINITION, &SettingsContext::new()).expect("parses");
    Arc::new(TuneLayout::build(&definition, &|_| None))
}

fn apply(tune: &mut Tune, edits: &[(usize, u8)]) {
    for (cell, value) in edits {
        let (name, index) = CELLS[*cell];
        tune.set_raw_at(name, index, f64::from(*value)).unwrap();
    }
}

/// The `(row, column)` of a flat cell index (`table` is 2 columns wide).
fn position(name: &str, index: usize) -> (usize, usize) {
    if name == "table" {
        (index / 2, index % 2)
    } else {
        (0, 0)
    }
}

fn expected_state(working: f64, baseline: f64, ram: f64, flash: f64) -> CellState {
    // The property builds no overlays, so a difference from RAM is always an
    // unsent change; a conflict needs a read that found no decision.
    let _ = baseline;
    if working == ram {
        if working == flash {
            CellState::Synced
        } else {
            CellState::InRam
        }
    } else {
        CellState::LocalDirty
    }
}

proptest! {
    #[test]
    fn derived_states_always_match_the_images(
        baseline_edits in proptest::collection::vec((0usize..CELLS.len(), 0u8..4), 0..8),
        working_edits in proptest::collection::vec((0usize..CELLS.len(), 0u8..4), 0..8),
        ram_edits in proptest::collection::vec((0usize..CELLS.len(), 0u8..4), 0..8),
        flash_edits in proptest::collection::vec((0usize..CELLS.len(), 0u8..4), 0..8),
    ) {
        let layout = layout();
        let mut baseline = Tune::new(Arc::clone(&layout));
        let mut working = Tune::new(Arc::clone(&layout));
        let mut ram = Tune::new(Arc::clone(&layout));
        let mut flash = Tune::new(Arc::clone(&layout));

        apply(&mut baseline, &baseline_edits);
        apply(&mut working, &working_edits);
        apply(&mut ram, &ram_edits);
        apply(&mut flash, &flash_edits);

        let images = TuneImages::new(baseline.clone(), working.clone())
            .unwrap()
            .with_ecu_ram(ram.clone())
            .unwrap()
            .with_ecu_flash(flash.clone())
            .unwrap();

        let unsaved = TuneDiff::between(&baseline, &working).unwrap();
        prop_assert_eq!(unsaved.cells, images.status().unsaved.cells);

        for (cell, (name, index)) in CELLS.iter().enumerate() {
            let value = |tune: &Tune| tune.raw_at(name, *index).unwrap();
            let expected = expected_state(
                value(&working),
                value(&baseline),
                value(&ram),
                value(&flash),
            );
            let (row, column) = position(name, *index);
            prop_assert_eq!(
                images.cell_state(name, row, column).unwrap(),
                expected,
                "cell {} ({}, {})",
                cell,
                name,
                index
            );
        }
    }

    #[test]
    fn a_page_with_any_change_is_never_reported_synced(
        edits in proptest::collection::vec((0usize..CELLS.len(), 0u8..4), 1..8),
    ) {
        let layout = layout();
        let baseline = Tune::new(Arc::clone(&layout));
        let mut working = baseline.snapshot();
        apply(&mut working, &edits);

        let mut ram = baseline.snapshot();
        apply(&mut ram, &edits);

        let images = TuneImages::new(baseline, working)
            .unwrap()
            .with_ecu_ram(ram)
            .unwrap()
            .with_ecu_flash(Tune::new(layout))
            .unwrap();

        if images.status().unsaved.is_empty() {
            // Identical assignments can leave the tune untouched; nothing to say.
            return Ok(());
        }

        // RAM matches the working image, flash does not: every changed cell is
        // in RAM, so the aggregate must be `InRam`, never `Synced`.
        prop_assert_eq!(images.overall_state(), CellState::InRam);
    }
}

proptest! {
    /// The cell comparison agrees with what an integer wire can carry.
    ///
    /// Both raw values go through the encoder's own rounding and clamping, so
    /// two values the wire cannot tell apart are one cell and two it can tell
    /// apart never collapse, whatever the definition's scale.
    #[test]
    fn an_integer_cell_compares_by_its_stored_byte(left in -1000.0f64..1000.0, right in -1000.0f64..1000.0) {
        let definition = parse_str(
            "[MegaTune]\nsignature = \"speeduino 202501\"\n\n[Constants]\nnPages = 1\npageSize = 1\npage = 1\nbin = scalar, U08, 0, \"\", 100.0, 0.0\n",
            &SettingsContext::new(),
        )
        .expect("the fixture parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let spec = layout.variable("bin").expect("the bin");

        let stored = |value: f64| value.round().clamp(0.0, 255.0);
        prop_assert_eq!(
            spec.same_wire_value(left, right),
            stored(left) == stored(right),
            "{} and {}", left, right
        );
    }

    /// A float cell compares its stored bits, so a user value and the value the
    /// page returns are one cell.
    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::as_conversions,
        reason = "the property is about the f64 to f32 narrowing the wire performs"
    )]
    fn a_float_cell_compares_by_its_stored_bits(left in -1e6f64..1e6, right in -1e6f64..1e6) {
        let definition = parse_str(
            "[MegaTune]\nsignature = \"speeduino 202501\"\n\n[Constants]\nnPages = 1\npageSize = 4\npage = 1\nf = scalar, F32, 0, \"\", 1.0, 0.0\n",
            &SettingsContext::new(),
        )
        .expect("the fixture parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let spec = layout.variable("f").expect("the scalar");

        prop_assert_eq!(
            spec.same_wire_value(left, right),
            (left as f32).to_bits() == (right as f32).to_bits(),
            "{} and {}", left, right
        );
    }
}
