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

    const AMBIGUOUS: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
wide = array, U08, 0, [4], "", 1, 0, 0, 255, 0
straddle = array, U08, 2, [4], "", 1, 0, 0, 255, 0
"#;

    #[test]
    fn an_ambiguous_page_keeps_both_peers_when_imported() {
        let definition = parse_str(AMBIGUOUS, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));

        // A file builds its own layout allocation, as a real import does.
        let file_definition = parse_str(AMBIGUOUS, &SettingsContext::new()).expect("parses");
        let file_layout = Arc::new(TuneLayout::build(&file_definition, &|_| None));
        let mut imported = Tune::new(file_layout);
        for (index, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            imported
                .set_image_raw("wide", index, value)
                .expect("wide takes a value");
        }
        for (index, value) in [5.0, 6.0, 7.0, 8.0].into_iter().enumerate() {
            imported
                .set_image_raw("straddle", index, value)
                .expect("straddle takes a value");
        }

        images
            .import_applied(&imported, &["wide".to_owned(), "straddle".to_owned()])
            .expect("import");

        assert_eq!(
            images.working().raw_at("wide", 3).unwrap(),
            4.0,
            "the file's own tail survives"
        );
        assert_eq!(images.working().raw_at("straddle", 0).unwrap(), 5.0);
    }

    const AMBIGUOUS_VIEW: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
wide = array, U08, 0, [4], "", 1, 0, 0, 255, 0
wide0 = bits, U08, 0, [0:7]
straddle = array, U08, 2, [4], "", 1, 0, 0, 255, 0
"#;

    #[test]
    fn a_file_that_omits_an_ambiguous_page_leaves_its_views_alone() {
        let definition = parse_str(AMBIGUOUS_VIEW, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));
        images
            .working_mut()
            .set_image_raw("wide0", 0, 7.0)
            .expect("the view takes a value");

        let file_definition = parse_str(AMBIGUOUS_VIEW, &SettingsContext::new()).expect("parses");
        let file_layout = Arc::new(TuneLayout::build(&file_definition, &|_| None));
        let mut imported = Tune::new(file_layout);
        imported
            .set_image_raw("wide", 0, 1.0)
            .expect("the owner takes a value");

        images.import_applied(&imported, &[]).expect("import");

        assert_eq!(
            images.working().raw_at("wide0", 0).unwrap(),
            7.0,
            "a file that says nothing about the page must not move its views"
        );
    }

    #[test]
    fn a_view_carried_alone_keeps_the_files_value_on_an_ambiguous_page() {
        let definition = parse_str(AMBIGUOUS_VIEW, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));

        let file_definition = parse_str(AMBIGUOUS_VIEW, &SettingsContext::new()).expect("parses");
        let file_layout = Arc::new(TuneLayout::build(&file_definition, &|_| None));
        let mut imported = Tune::new(file_layout);
        imported
            .set_image_raw("wide0", 0, 9.0)
            .expect("the view takes a value");

        images
            .import_applied(&imported, &["wide0".to_owned()])
            .expect("import");

        assert_eq!(images.working().raw_at("wide0", 0).unwrap(), 9.0);
    }

    #[test]
    fn an_ambiguous_page_raises_no_conflict() {
        let definition = parse_str(AMBIGUOUS, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));
        let mut ram = Tune::new(Arc::clone(&layout));
        ram.set_image_raw("wide", 0, 9.0)
            .expect("the ECU holds a different value");

        let conflicts = images.reconcile_read(ram).expect("a read reconciles");

        assert!(
            conflicts.is_empty(),
            "a page no write can carry must not ask for a decision: {conflicts:?}"
        );
    }

    const PEERS: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 8
page = 1
first = scalar, U08, 0, "", 1, 0, 0, 255, 0
second = scalar, U08, 0, "", 1, 0, 0, 255, 0
"#;

    fn peer_images() -> (TuneImages, Tune) {
        let definition = parse_str(PEERS, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));

        // A file builds its own layout allocation, as a real import does.
        let file_definition = parse_str(PEERS, &SettingsContext::new()).expect("parses");
        let file_layout = Arc::new(TuneLayout::build(&file_definition, &|_| None));

        (images, Tune::new(file_layout))
    }

    #[test]
    fn a_file_carrying_two_values_for_one_byte_is_refused() {
        let (mut images, mut imported) = peer_images();
        imported
            .set_image_raw("first", 0, 100.0)
            .expect("the first takes a value");
        imported
            .set_image_raw("second", 0, 200.0)
            .expect("the second takes a value");

        let error = images
            .import_applied(&imported, &["first".to_owned(), "second".to_owned()])
            .expect_err("one byte cannot carry both values");

        assert!(
            matches!(error, TuneError::ConflictingValues { .. }),
            "the refusal names the pair: {error}"
        );
    }

    #[test]
    fn a_refused_file_leaves_the_working_image_alone() {
        let (mut images, mut imported) = peer_images();
        images
            .working_mut()
            .set_image_raw("first", 0, 7.0)
            .expect("the image holds a value");
        imported
            .set_image_raw("first", 0, 100.0)
            .expect("the first takes a value");
        imported
            .set_image_raw("second", 0, 200.0)
            .expect("the second takes a value");

        let _ = images
            .import_applied(&imported, &["first".to_owned(), "second".to_owned()])
            .expect_err("the import is refused");

        assert_eq!(
            images
                .working()
                .raw_at("first", 0)
                .expect("the image reads"),
            7.0,
            "a refused import must not move a single cell"
        );
    }

    #[test]
    fn a_file_whose_peers_agree_merges() {
        let (mut images, mut imported) = peer_images();
        imported
            .set_image_raw("first", 0, 100.0)
            .expect("the first takes a value");
        imported
            .set_image_raw("second", 0, 100.0)
            .expect("the second takes a value");

        images
            .import_applied(&imported, &["first".to_owned(), "second".to_owned()])
            .expect("the two values agree");

        assert_eq!(
            images
                .working()
                .raw_at("first", 0)
                .expect("the image reads"),
            100.0
        );
        assert_eq!(
            images
                .working()
                .raw_at("second", 0)
                .expect("the image reads"),
            100.0
        );
    }

    /// A definition with the peers on one page and an unrelated constant on another.
    const TWO_PAGES: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 2
pageSize = 8, 8
page = 1
first = scalar, U08, 0, "", 1, 0, 0, 255, 0
second = scalar, U08, 0, "", 1, 0, 0, 255, 0
page = 2
other = scalar, U08, 0, "", 1, 0, 0, 255, 0
"#;

    #[test]
    fn a_file_that_says_nothing_about_a_page_leaves_its_peers_alone() {
        // A disagreement on a page the file never names is not the file's
        // answer to give, so the import goes ahead and that page keeps what it
        // had.
        let definition = parse_str(TWO_PAGES, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));
        images
            .working_mut()
            .set_image_raw("first", 0, 100.0)
            .expect("takes a value");
        images
            .working_mut()
            .set_image_raw("second", 0, 200.0)
            .expect("takes a value");

        let file_definition = parse_str(TWO_PAGES, &SettingsContext::new()).expect("parses");
        let file_layout = Arc::new(TuneLayout::build(&file_definition, &|_| None));
        let mut imported = Tune::new(file_layout);
        imported
            .set_image_raw("other", 0, 5.0)
            .expect("takes a value");

        images
            .import_applied(&imported, &["other".to_owned()])
            .expect("the file names another page");

        assert_eq!(
            images
                .working()
                .raw_at("other", 0)
                .expect("the image reads"),
            5.0
        );
        assert_eq!(
            images
                .working()
                .raw_at("first", 0)
                .expect("the image reads"),
            100.0,
            "a page the file says nothing about keeps its values"
        );
    }

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
        assert!(images.status().flash_known);

        images.set_ecu_flash(ram).unwrap();
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Synced);

        images.mark_flash_unknown();
        assert!(!images.status().flash_known);
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::InRam);
    }

    #[test]
    fn the_truth_table_is_exhaustive_over_image_equalities() {
        // One cell, three live images, each either 0 or 1. The baseline does
        // not enter the derived state, and both overlays are tested separately.
        for working in [0.0, 1.0] {
            for ram in [0.0, 1.0] {
                for flash in [0.0, 1.0] {
                    let mut work = tune();
                    work.set_raw_at("a", 0, working).unwrap();

                    let mut ecu = tune();
                    ecu.set_raw_at("a", 0, ram).unwrap();

                    let mut burned = tune();
                    burned.set_raw_at("a", 0, flash).unwrap();

                    let images = TuneImages::new(tune(), work)
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
                    } else {
                        // A real difference from RAM is an unsent change; only
                        // a read that found no decision marks a conflict, and
                        // these images carry no overlays.
                        CellState::LocalDirty
                    };
                    assert_eq!(
                        images.cell_state("a", 0, 0).unwrap(),
                        expected,
                        "w={working} r={ram} f={flash}"
                    );
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
        let conflicts = images.reconcile_read(ram).unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].name, "a");
        assert_eq!(conflicts[0].working, CellValue::Number(0.0));
        assert_eq!(conflicts[0].ecu, CellValue::Number(9.0));
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Conflict);
        assert_eq!(
            images.status().conflicts,
            images.status().pending_write.cells,
            "every reported conflict is a cell the write could send"
        );

        images.clear_conflict("a", 0, 0).unwrap();
        assert_eq!(
            images.cell_state("a", 0, 0).unwrap(),
            CellState::LocalDirty,
            "without a conflict mark the difference is an unsent change"
        );
    }

    #[test]
    fn a_page_scoped_write_keeps_a_decision_it_did_not_write() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        ram.set_user("b", 9.0).unwrap();
        images.reconcile_read(ram.clone()).unwrap();
        images.keep_working();

        images.clear_resolutions_for(&["a".to_owned()]);

        let conflicts = images.reconcile_read(ram).unwrap();
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert_eq!(conflicts[0].name, "a");
    }

    #[test]
    fn a_float_that_stores_the_same_bytes_is_not_a_conflict() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
f = scalar, F32, 0, \"\", 100.0, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));
        images
            .working_mut()
            .set_user("f", 5.0)
            .expect("the raw value is 0.05");

        // What the page decodes back: the raw narrowed to f32 and widened.
        let mut ram = Tune::new(layout);
        ram.set_image_raw("f", 0, f64::from(0.05_f32))
            .expect("the ECU value");

        let conflicts = images.reconcile_read(ram).unwrap();

        assert_eq!(conflicts.len(), 0, "the stored bytes are equal");
        assert_eq!(images.status().conflicts, 0);
        assert_eq!(images.status().pending_write.cells, 0);
    }

    #[test]
    fn a_conflict_edited_back_to_the_ecu_value_is_resolved() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        images.reconcile_read(ram).unwrap();
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Conflict);

        images.working_mut().set_user("a", 9.0).unwrap();

        assert_eq!(images.status().conflicts, 0);
        assert_eq!(images.status().pending_write.cells, 0);
        assert_eq!(images.conflicts().len(), 0);
        assert_ne!(images.cell_state("a", 0, 0).unwrap(), CellState::Conflict);
    }

    #[test]
    fn storing_a_ram_image_keeps_a_failed_write_mark() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.mark_write_failed("a", 0, 0).unwrap();

        // The write's base read covers one page and must not clear a mark the
        // write path did not observe.
        images.set_ecu_ram(tune()).unwrap();

        assert_eq!(
            images.cell_state("a", 0, 0).unwrap(),
            CellState::WriteFailed
        );
    }

    #[test]
    fn a_verified_write_clears_only_the_failures_it_wrote() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.mark_write_failed("a", 0, 0).unwrap();
        images.mark_write_failed("b", 0, 0).unwrap();

        images.clear_write_failures_for(&["a".to_owned()]);

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::InRam);
        assert_eq!(
            images.cell_state("b", 0, 0).unwrap(),
            CellState::WriteFailed,
            "the page this write did not cover still failed"
        );
    }

    #[test]
    fn a_complete_read_clears_a_failed_write() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.mark_write_failed("a", 0, 0).unwrap();
        assert_eq!(
            images.cell_state("a", 0, 0).unwrap(),
            CellState::WriteFailed
        );

        images.reconcile_read(tune()).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::InRam);
    }

    #[test]
    fn importing_another_tune_clears_the_decision_about_the_old_values() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        images.reconcile_read(ram.clone()).unwrap();
        images.keep_working();

        let mut imported = Tune::new(Arc::clone(images.layout()));
        imported.set_user("a", 4.0).unwrap();
        images
            .import_applied(&imported, &["a".to_owned()])
            .expect("import");

        let conflicts = images.reconcile_read(ram).unwrap();
        assert_eq!(
            conflicts.len(),
            1,
            "the imported value is a new difference and must ask"
        );
    }

    #[test]
    fn a_value_the_codec_refuses_is_not_a_conflict_or_pending() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
wide = bits, U16, 0, [40:47]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));

        let mut ram = Tune::new(layout);
        ram.set_image_raw("wide", 0, 3.0).expect("the ECU value");

        let conflicts = images.reconcile_read(ram).unwrap();

        assert_eq!(conflicts.len(), 0, "the write plan cannot send it");
        assert_eq!(images.status().conflicts, 0);
        assert_eq!(images.status().pending_write.cells, 0);
    }

    #[test]
    fn a_no_msq_save_value_is_never_reported() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 16
page = 1
saveMe = scalar, U08, 0, \"\", 1, 0, 0, 255, 0
runtime = array, U08, 4, [4], \"\", 1, 0, 0, 255, 0, noMsqSave
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));

        let mut ram = Tune::new(layout);
        ram.set_image_raw("runtime", 0, 9.0).expect("the ECU value");

        let conflicts = images.reconcile_read(ram).unwrap();

        assert_eq!(conflicts.len(), 0, "the file cannot carry the value");
        assert_eq!(
            images.cell_state("runtime", 0, 0).unwrap(),
            CellState::NotApplicable
        );
        assert_eq!(images.status().conflicts, 0);
        assert_eq!(images.status().pending_write.cells, 0);
    }

    #[test]
    fn a_controller_owned_value_is_not_a_conflict() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
owned = scalar, U08, 0, \"\", 1, 0, 0, 255, 0

[ConstantsExtensions]
controllerPriority = owned
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));

        let mut ram = Tune::new(layout);
        ram.set_image_raw("owned", 0, 9.0).expect("takes a value");

        let conflicts = images.reconcile_read(ram).unwrap();

        assert_eq!(conflicts.len(), 0, "the ECU owns the value");
        assert_eq!(images.conflicts().len(), 0);
        assert_eq!(
            images.working().raw_at("owned", 0).unwrap(),
            9.0,
            "the ECU's value replaces the offline copy"
        );
    }

    #[test]
    fn importing_one_variable_keeps_another_variables_decision() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        ram.set_user("b", 9.0).unwrap();
        images.reconcile_read(ram.clone()).unwrap();
        images.keep_working();

        let mut imported = Tune::new(Arc::clone(images.layout()));
        imported.set_user("a", 4.0).unwrap();
        images
            .import_applied(&imported, &["a".to_owned()])
            .expect("import");

        let conflicts = images.reconcile_read(ram).unwrap();
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert_eq!(conflicts[0].name, "a", "only the imported value asks again");
    }

    #[test]
    fn importing_a_variable_it_did_not_change_keeps_its_decision() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        ram.set_user("b", 9.0).unwrap();
        images.reconcile_read(ram.clone()).unwrap();
        images.keep_working();

        let mut imported = Tune::new(Arc::clone(images.layout()));
        imported.set_user("a", 4.0).unwrap();
        // The file names `b` but carries the value it already had.
        imported.set_user("b", 0.0).unwrap();
        images
            .import_applied(&imported, &["a".to_owned(), "b".to_owned()])
            .expect("import");

        let conflicts = images.reconcile_read(ram).unwrap();
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert_eq!(conflicts[0].name, "a");
    }

    #[test]
    fn a_value_with_no_presentation_is_never_sent_or_reported() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
scaled = scalar, U08, 0, \"\", {unknownExpression}, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut images = TuneImages::from_working(Tune::new(Arc::clone(&layout)));

        let mut ram = Tune::new(layout);
        ram.set_image_raw("scaled", 0, 9.0).expect("the ECU value");

        let conflicts = images.reconcile_read(ram).unwrap();

        assert_eq!(conflicts.len(), 0, "the value has no file form");
        assert_eq!(
            images.cell_state("scaled", 0, 0).unwrap(),
            CellState::NotApplicable
        );
        assert_eq!(images.status().pending_write.cells, 0);
    }

    #[test]
    fn keeping_the_working_values_turns_a_conflict_into_an_unsent_change() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();

        let conflicts = images.reconcile_read(ram.clone()).unwrap();
        assert_eq!(conflicts.len(), 1, "the first read reports the difference");

        images.keep_working();

        assert_eq!(images.conflicts().len(), 0, "the decision was recorded");
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::LocalDirty);
        assert_eq!(images.status().pending_write.cells, 1);

        // The ECU still holds its value, but the user already decided: a later
        // read must not ask again.
        let conflicts = images.reconcile_read(ram).unwrap();
        assert_eq!(conflicts.len(), 0, "a decided cell is not a new conflict");
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::LocalDirty);
    }

    #[test]
    fn a_read_that_finds_the_values_equal_clears_the_decision() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        images.reconcile_read(ram).unwrap();
        images.keep_working();

        images.reconcile_read(tune()).unwrap();

        assert_eq!(images.conflicts().len(), 0);
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::InRam);
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
        images.reconcile_read(ram).unwrap();
        images.working_mut().set_user("a", 5.0).unwrap();

        images.revert_variables(&["a".to_owned()]);

        assert!(
            images.conflicts().is_empty(),
            "the reverted variable carries no conflict mark"
        );
        // The ECU's value still differs from the file, so the cell is an
        // unsent change rather than a conflict the user has to decide.
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::LocalDirty);
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
    fn saving_without_edits_leaves_an_undecided_conflict() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        images.reconcile_read(ram).unwrap();

        images.commit_baseline();

        assert_eq!(
            images.cell_state("a", 0, 0).unwrap(),
            CellState::Conflict,
            "a save with nothing to write is not a decision"
        );
    }

    #[test]
    fn saving_keeps_the_unsent_values_as_the_chosen_tune() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.working_mut().set_user("a", 3.0).unwrap();

        images.commit_baseline();

        assert_eq!(images.status().unsaved.cells, 0);
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::LocalDirty);

        let conflicts = images.reconcile_read(tune()).unwrap();
        assert_eq!(conflicts.len(), 0, "a saved value is not a new conflict");
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::LocalDirty);
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

        let conflicts = images.reconcile_read(ram).unwrap();

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
