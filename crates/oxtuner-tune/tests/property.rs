//! Property tests: any sequence of edits must leave the derived states
//! consistent with the images they were derived from (ADR-0010).
//!
//! The exhaustive test in `images.rs` covers one cell; this covers arbitrary
//! edits across scalars and a table, including edits applied to the ECU images
//! out of order.

use std::sync::Arc;

use oxtuner_ini::{SettingsContext, parse_str};
use oxtuner_tune::{CellState, Layout, Tune, TuneDiff, TuneImages};
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

fn layout() -> Arc<Layout> {
    let definition = parse_str(DEFINITION, &SettingsContext::new()).expect("parses");
    Arc::new(Layout::build(&definition, &|_| None))
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
    if working == ram {
        if working == flash {
            CellState::Synced
        } else {
            CellState::InRam
        }
    } else if working == baseline {
        CellState::Conflict
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
