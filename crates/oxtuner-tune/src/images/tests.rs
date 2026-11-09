use super::*;

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::float_cmp,
        reason = "unit tests may unwrap and compare model values exactly"
    )]
    use std::sync::OnceLock;

    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;

    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"
queryCommand = "Q"

[Constants]
nPages = 1
pageSize = 288
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 255, 0
b = scalar, U08, 1, "", 1, 0, 0, 255, 0

[PcVariables]
pc = scalar, U08, 1, "", 1, 0, 0, 255, 0
"#;

    fn tune() -> Tune {
        // Every tune in these tests must share one `Arc<TuneLayout>`: the images
        // reject anything else, exactly as production does when the ECU image
        // is read through the same definition as the working image.
        static LAYOUT: OnceLock<Arc<TuneLayout>> = OnceLock::new();
        let layout = LAYOUT.get_or_init(|| {
            let definition =
                parse_str(DEFINITION, &SettingsContext::new()).expect("definition parses");
            Arc::new(TuneLayout::build(&definition, &|_| None))
        });
        Tune::new(Arc::clone(layout))
    }

    fn images() -> TuneImages {
        TuneImages::from_working(tune())
    }

    #[test]
    fn without_an_ecu_read_everything_is_unknown() {
        let images = images();
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Unknown);
        assert_eq!(
            images.cell_state("pc", 0, 0).unwrap(),
            CellState::NotApplicable
        );
        assert_eq!(images.overall_state(), CellState::Unknown);
    }

    #[test]
    fn a_confirmed_equal_ram_and_flash_is_synced() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Synced);
        assert_eq!(images.status().state, CellState::Synced);
    }

    #[test]
    fn a_local_edit_is_local_dirty_until_written_to_ram() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();
        images.working_mut().set_user("a", 42.0).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::LocalDirty);
        assert_eq!(images.status().unsaved.cells, 1);
        assert_eq!(images.status().pending_write.cells, 1);
    }

    #[test]
    fn a_verified_write_is_in_ram_and_burn_clears_it() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();
        images.working_mut().set_user("a", 42.0).unwrap();

        let mut ram = tune();
        ram.set_user("a", 42.0).unwrap();
        images.set_ecu_ram(ram.clone()).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::InRam);
        assert_eq!(images.status().pending_write.cells, 0);
        assert_eq!(images.status().pending_burn.cells, 1);

        images.set_ecu_flash(ram).unwrap();
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Synced);
    }

    #[test]
    fn the_truth_table_is_exhaustive_over_image_equalities() {
        // One cell, four images, each either 0 or 1. `ecu_unknown` is covered
        // by the "no RAM read" case, and both overlays are tested separately.
        for working in [0.0, 1.0] {
            for baseline in [0.0, 1.0] {
                for ram in [0.0, 1.0] {
                    for flash in [0.0, 1.0] {
                        let mut base = tune();
                        base.set_raw_at("a", 0, baseline).unwrap();

                        let mut work = tune();
                        work.set_raw_at("a", 0, working).unwrap();

                        let mut ecu = tune();
                        ecu.set_raw_at("a", 0, ram).unwrap();

                        let mut burned = tune();
                        burned.set_raw_at("a", 0, flash).unwrap();

                        let images = TuneImages::new(base, work)
                            .unwrap()
                            .with_ecu_ram(ecu)
                            .unwrap()
                            .with_ecu_flash(burned)
                            .unwrap();

                        let expected = if working == ram {
                            if working == flash {
                                CellState::Synced
                            } else {
                                CellState::InRam
                            }
                        } else if working == baseline {
                            CellState::Conflict
                        } else {
                            CellState::LocalDirty
                        };

                        assert_eq!(
                            images.cell_state("a", 0, 0).unwrap(),
                            expected,
                            "w={working} b={baseline} r={ram} f={flash}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_flash_turns_a_match_into_in_ram_not_synced() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::InRam);
    }

    #[test]
    fn a_failed_write_is_sticky_and_more_severe_than_unknown() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.mark_write_failed("a", 0, 0).unwrap();
        images.mark_unknown();

        assert_eq!(
            images.cell_state("a", 0, 0).unwrap(),
            CellState::WriteFailed
        );
        assert_eq!(images.overall_state(), CellState::WriteFailed);
    }

    #[test]
    fn ecu_drift_without_a_local_edit_is_a_conflict() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();

        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        let conflicts = images.reconnect(ram).unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].name, "a");
        assert_eq!(conflicts[0].working, CellValue::Number(0.0));
        assert_eq!(conflicts[0].ecu, CellValue::Number(9.0));
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Conflict);

        images.clear_conflict("a", 0, 0).unwrap();
        assert_eq!(
            images.cell_state("a", 0, 0).unwrap(),
            CellState::Conflict,
            "the underlying images still differ; only an overlay was cleared"
        );
    }

    #[test]
    fn reverting_one_variable_leaves_the_others_edited() {
        let mut images = images();
        images.working_mut().set_user("a", 3.0).unwrap();
        images.working_mut().set_user("b", 4.0).unwrap();

        let reverted = images.revert_variables(&["a".to_owned()]);

        assert_eq!(reverted, 1);
        assert_eq!(images.working().scalar_raw("a").unwrap(), 0.0);
        assert_ne!(
            images.working().scalar_raw("b").unwrap(),
            0.0,
            "the variable the caller did not name keeps its edit"
        );
    }

    #[test]
    fn reverting_one_variable_clears_its_conflict_mark() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        images.reconnect(ram).unwrap();
        images.working_mut().set_user("a", 5.0).unwrap();

        images.revert_variables(&["a".to_owned()]);

        assert!(
            images.conflicts().is_empty(),
            "the reverted variable carries no conflict mark"
        );
        // The cell still reads Conflict: the ECU's value really does differ
        // from the file. That is the value-based state, not a stale overlay.
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Conflict);
    }

    #[test]
    fn reverting_restores_the_baseline_and_clears_conflicts() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.working_mut().set_user("a", 3.0).unwrap();
        images.working_mut().set_user("b", 4.0).unwrap();

        images.revert_to_baseline();

        assert_eq!(images.working().scalar_raw("a").unwrap(), 0.0);
        assert_eq!(images.status().unsaved.cells, 0);
    }

    #[test]
    fn committing_the_baseline_marks_the_tune_saved() {
        let mut images = images();
        images.working_mut().set_user("a", 1.0).unwrap();
        assert_eq!(images.status().unsaved.cells, 1);

        images.commit_baseline();

        assert_eq!(images.status().unsaved.cells, 0);
    }

    #[test]
    fn the_state_table_matches_cell_lookups() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();
        images.working_mut().set_user("a", 7.0).unwrap();

        let table = StateTable::build(&images);

        assert_eq!(table.cell("a", 0, 0).unwrap(), CellState::LocalDirty);
        assert_eq!(table.cell("b", 0, 0).unwrap(), CellState::Synced);
        assert_eq!(table.cell("pc", 0, 0).unwrap(), CellState::NotApplicable);
        assert_eq!(table.count(CellState::LocalDirty), 1);
        assert_eq!(table.overall(), CellState::LocalDirty);
        assert_eq!(
            table.page(IniPage::FIRST).unwrap().state,
            CellState::LocalDirty
        );
        assert_eq!(table.page(IniPage::FIRST).unwrap().changed, 1);
        assert_eq!(table.page(IniPage::new(9).unwrap()), None);
    }

    const SCALED_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
note = string, ASCII, 8
"#;

    fn scaled_images() -> TuneImages {
        let definition =
            parse_str(SCALED_DEFINITION, &SettingsContext::new()).expect("definition parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        TuneImages::from_working(Tune::new(layout))
    }

    #[test]
    fn conflict_values_are_reported_in_user_units() {
        let mut images = scaled_images();
        let mut ram = Tune::new(Arc::clone(images.layout()));
        ram.set_user("reqFuel", 12.3).unwrap();

        let conflicts = images.reconnect(ram).unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].name, "reqFuel");
        assert_eq!(conflicts[0].working, CellValue::Number(0.0));
        let CellValue::Number(ecu) = conflicts[0].ecu else {
            panic!("the ECU value should be a user-unit number");
        };
        assert!((ecu - 12.3).abs() < 1e-9, "got {ecu}");
    }

    #[test]
    fn a_text_value_is_one_cell_not_its_reserved_length() {
        let mut images = scaled_images();
        let ram = Tune::new(Arc::clone(images.layout()));
        images.set_ecu_ram(ram.clone()).unwrap();
        images.set_ecu_flash(ram).unwrap();

        assert_eq!(images.cell_state("note", 0, 0).unwrap(), CellState::Synced);
        assert_eq!(images.page_state(IniPage::FIRST), Some(CellState::Synced));

        let table = StateTable::build(&images);
        assert_eq!(table.page(IniPage::FIRST).unwrap().cells, 2);
        assert_eq!(table.count(CellState::Synced), 2);

        images.working_mut().set_text("note", "ABC").unwrap();
        let diff = TuneDiff::between(images.baseline(), images.working()).unwrap();
        assert_eq!(diff.cells, 1, "an edited string is one changed cell");
    }

    #[test]
    fn images_from_different_definitions_are_rejected() {
        let other = Tune::from_definition(
            &parse_str(
                "[MegaTune]\nsignature = \"other\"\n[Constants]\nnPages = 1\npageSize = 1\npage = 1\nx = scalar, U08, 0, \"\", 1, 0, 0, 1, 0\n",
                &SettingsContext::new(),
            )
            .expect("parses"),
        );

        assert!(matches!(
            TuneImages::new(tune(), other),
            Err(TuneError::LayoutMismatch)
        ));
    }
}
