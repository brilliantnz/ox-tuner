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
label = string, ASCII, 8

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

    const ALIASED: &str = r#"
[MegaTune]
signature = "speeduino 202501"
queryCommand = "Q"

[Constants]
nPages = 1
pageSize = 8
page = 1
data = array, U08, 0, [4], "", 1, 0, 0, 255, 0
data0 = bits, U08, 0, [0:7]
data1 = bits, U08, 1, [0:7]
data2 = bits, U08, 2, [0:7]
data3 = bits, U08, 3, [0:7]
"#;

    fn aliased_images() -> TuneImages {
        let definition = parse_str(ALIASED, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));

        TuneImages::from_working(Tune::new(layout))
    }

    fn aliased_tune(images: &TuneImages) -> Tune {
        Tune::new(Arc::clone(images.working().layout()))
    }

    #[test]
    fn a_file_without_its_aliases_keeps_them_with_the_page() {
        let mut images = aliased_images();
        let mut imported = aliased_tune(&images);
        for index in 0..4 {
            imported
                .set_image_raw("data", index, 255.0)
                .expect("the array takes a value");
        }

        // What TunerStudio writes: the array, none of its derived aliases.
        images
            .import_applied(&imported, &["data".to_owned()])
            .expect("import");

        for index in 0..4 {
            assert_eq!(images.working().raw_at("data", index).unwrap(), 255.0);
            let alias = format!("data{index}");
            assert_eq!(
                images.working().scalar_raw(&alias).unwrap(),
                255.0,
                "a derived alias follows the byte the file wrote"
            );
        }
    }

    #[test]
    fn a_file_that_writes_an_alias_keeps_the_owner_in_charge_of_the_byte() {
        let mut images = aliased_images();
        let mut imported = aliased_tune(&images);
        for index in 0..4 {
            imported
                .set_image_raw("data", index, 255.0)
                .expect("the array takes a value");
            imported
                .set_image_raw(&format!("data{index}"), 0, 0.0)
                .expect("the alias takes a value");
        }

        images
            .import_applied(
                &imported,
                &[
                    "data".to_owned(),
                    "data0".to_owned(),
                    "data1".to_owned(),
                    "data2".to_owned(),
                    "data3".to_owned(),
                ],
            )
            .expect("import");

        for index in 0..4 {
            assert_eq!(
                images.working().raw_at("data", index).unwrap(),
                255.0,
                "the owner writes the byte even when the file also carried the view"
            );
            let alias = format!("data{index}");
            assert_eq!(images.working().scalar_raw(&alias).unwrap(), 255.0);
        }
    }

    #[test]
    fn a_file_that_writes_only_a_view_moves_its_owner() {
        let mut images = aliased_images();
        images
            .working_mut()
            .set_image_raw("data", 1, 7.0)
            .expect("a byte to keep");

        let mut imported = aliased_tune(&images);
        imported
            .set_image_raw("data0", 0, 5.0)
            .expect("the view takes a value");

        images
            .import_applied(&imported, &["data0".to_owned()])
            .expect("import");

        assert_eq!(
            images.working().raw_at("data", 0).unwrap(),
            5.0,
            "the owner follows the byte the view wrote"
        );
        assert_eq!(images.working().scalar_raw("data0").unwrap(), 5.0);
        assert_eq!(
            images.working().raw_at("data", 1).unwrap(),
            7.0,
            "a byte the file did not write keeps its value"
        );
    }

    #[test]
    fn a_view_shows_the_state_of_the_element_it_covers() {
        let mut images = aliased_images();
        images
            .set_ecu_ram(aliased_tune(&images))
            .expect("same layout");
        images
            .set_ecu_flash(aliased_tune(&images))
            .expect("same layout");

        images
            .working_mut()
            .set_image_raw("data", 1, 9.0)
            .expect("the array takes a value");

        assert_eq!(
            images.cell_state("data0", 0, 0).unwrap(),
            CellState::Synced,
            "byte 0 did not change"
        );
        assert_eq!(
            images.cell_state("data1", 0, 0).unwrap(),
            CellState::LocalDirty,
            "byte 1 is the view's element"
        );
    }

    const OVERLAPPING: &str = r#"
[MegaTune]
signature = "speeduino 202501"
queryCommand = "Q"

[Constants]
nPages = 1
pageSize = 16
page = 1
note = string, ASCII, 8
data = array, U08, 0, [4], "", 1, 0, 0, 255, 0
data0 = bits, U08, 0, [0:7]
inside = scalar, U08, 4, "", 1, 0, 0, 255, 0
outside = scalar, U08, 12, "", 1, 0, 0, 255, 0
"#;

    #[test]
    fn a_page_a_text_field_overlaps_keeps_what_the_file_carried() {
        let definition = parse_str(OVERLAPPING, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));
        images
            .working_mut()
            .set_image_raw("outside", 0, 4.0)
            .expect("a value to keep");

        // The file's tune is built from its own layout, as a project load
        // builds it; an alias test that reuses the image's `Arc` would hide the
        // allocation mismatch a real import hits.
        let file_definition = parse_str(OVERLAPPING, &SettingsContext::new()).expect("parses");
        let file_layout = Arc::new(TuneLayout::build(&file_definition, &|_| None));
        let mut imported = Tune::new(file_layout);
        imported
            .set_image_raw("inside", 0, 7.0)
            .expect("a value to apply");

        images
            .import_applied(&imported, &["inside".to_owned()])
            .expect("import");

        assert_eq!(
            images.working().raw_at("inside", 0).unwrap(),
            7.0,
            "the value the file carried is applied"
        );
        assert_eq!(
            images.working().raw_at("outside", 0).unwrap(),
            4.0,
            "a value the file did not carry keeps its value"
        );
    }

    #[test]
    fn a_view_without_its_owner_moves_the_owners_bytes_on_an_overlap_page() {
        let definition = parse_str(OVERLAPPING, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));
        images
            .working_mut()
            .set_image_raw("data", 1, 7.0)
            .expect("a byte to keep");

        let mut imported = separate_file_tune();
        imported
            .set_image_raw("data0", 0, 5.0)
            .expect("the view takes a value");

        images
            .import_applied(&imported, &["data0".to_owned()])
            .expect("import");

        assert_eq!(
            images.working().raw_at("data", 0).unwrap(),
            5.0,
            "the owner takes the byte the view wrote"
        );
        assert_eq!(
            images.working().raw_at("data", 1).unwrap(),
            7.0,
            "the rest of the owner keeps its value"
        );
        assert_eq!(images.working().scalar_raw("data0").unwrap(), 5.0);
    }

    #[test]
    fn a_view_the_file_carries_beside_its_owner_is_adopted() {
        let definition = parse_str(OVERLAPPING, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(layout));

        let mut imported = separate_file_tune();
        for (index, value) in [9.0, 8.0, 7.0, 6.0].into_iter().enumerate() {
            imported
                .set_image_raw("data", index, value)
                .expect("the array takes a value");
        }
        imported
            .set_image_raw("data0", 0, 9.0)
            .expect("the view takes a value");

        images
            .import_applied(&imported, &["data".to_owned(), "data0".to_owned()])
            .expect("import");

        assert_eq!(
            images.working().raw_at("data", 0).unwrap(),
            9.0,
            "the owner keeps the file's value"
        );
        assert_eq!(
            images.working().scalar_raw("data0").unwrap(),
            9.0,
            "the view follows the value the file carried"
        );
    }

    /// A tune built the way a project load builds one: its own layout
    /// allocation, not the image's.
    fn separate_file_tune() -> Tune {
        let definition = parse_str(OVERLAPPING, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        Tune::new(layout)
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
    fn reverting_a_text_variable_restores_the_baseline_text() {
        let mut images = images();
        images.working_mut().set_text("label", "edited").unwrap();

        let reverted = images.revert_variables(&["label".to_owned()]);

        assert_eq!(reverted, 1);
        assert!(
            images.working().text("label").unwrap().is_empty(),
            "the text went back to the saved tune"
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
