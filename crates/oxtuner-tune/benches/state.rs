//! Budget benchmark: a 15-page, 818-constant tune (the reference project's
//! shape) and the operations the UI runs per frame.
//!
//! Numbers live in `docs/architecture/performance/budget.md`. Run with:
//!
//! ```text
//! cargo bench -p oxtuner-tune
//! ```

use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_ini::{Definition, SettingsContext, parse_str};
use oxtuner_tune::{StateTable, Tune, TuneImages, TuneLayout};

/// The reference project's definition, in shape if not in content.
///
/// Fifteen pages; 818 constants in total, eight of them tables; `pageSize`
/// and byte offsets are close to a real Speeduino INI but are irrelevant to
/// the measured code paths (layout size, cell count, state derivation).
fn definition() -> Definition {
    const PAGES: usize = 15;
    const CONSTANTS: usize = 818;
    const TABLES: usize = 8;

    let scalars_per_page = (CONSTANTS - TABLES) / PAGES;
    let mut text = String::from(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 15
pageSize = 288, 288, 288, 288, 288, 288, 288, 288, 288, 288, 288, 288, 288, 288, 288
",
    );

    let mut written = 0usize;
    for page in 1..=PAGES {
        let _ = writeln!(text, "page = {page}");
        for index in 0..scalars_per_page {
            let _ = writeln!(
                text,
                "scalar_{page}_{index} = scalar, U08, {index}, \"\", 1, 0, 0, 255, 0"
            );
            written += 1;
        }
        if page <= TABLES {
            // The largest grid the editor draws; the other seven are 16×16.
            let shape = if page == 1 { "[32x32]" } else { "[16x16]" };
            let _ = writeln!(
                text,
                "table_{page} = array, U08, 256, {shape}, \"\", 1, 0, 0, 255, 0"
            );
            written += 1;
        }
    }
    assert_eq!(
        written, CONSTANTS,
        "the fixture must match the budget's shape"
    );

    parse_str(&text, &SettingsContext::new()).expect("the synthetic definition parses")
}

fn layout(definition: &Definition) -> Arc<TuneLayout> {
    Arc::new(TuneLayout::build(definition, &|_| None))
}

/// A tune that looks connected: RAM and flash equal, every tenth cell edited.
fn images(layout: Arc<TuneLayout>) -> TuneImages {
    let baseline = Tune::new(Arc::clone(&layout));
    let mut ram = baseline.snapshot();
    let mut working = baseline.snapshot();

    for (index, variable) in layout.variables().iter().enumerate() {
        if variable.owner != oxtuner_tune::Owner::Page(oxtuner_tune::IniPage::FIRST) {
            continue;
        }
        if index % 10 == 0 {
            ram.set_raw_at(&variable.name, 0, 7.0)
                .expect("the fixture variable exists");
            working
                .set_raw_at(&variable.name, 0, 9.0)
                .expect("the fixture variable exists");
        }
    }

    TuneImages::new(baseline, working)
        .expect("same layout")
        .with_ecu_ram(ram)
        .expect("same layout")
        .with_ecu_flash(Tune::new(layout))
        .expect("same layout")
}

fn budget(criterion: &mut Criterion) {
    let definition = definition();
    let layout = layout(&definition);
    let images = images(Arc::clone(&layout));
    let table = StateTable::build(&images);

    criterion.bench_function("layout_build_15_pages_818_constants", |bencher| {
        bencher.iter(|| black_box(TuneLayout::build(black_box(&definition), &|_| None)));
    });

    criterion.bench_function("layout_lookup_mixed_case", |bencher| {
        bencher.iter(|| black_box(layout.index(black_box("ScAlAr_8_40"))));
    });

    criterion.bench_function("state_table_build_15_pages", |bencher| {
        bencher.iter(|| black_box(StateTable::build(black_box(&images))));
    });

    criterion.bench_function("cell_lookup_32x32_table", |bencher| {
        bencher.iter(|| {
            black_box(
                table
                    .cell(black_box("table_1"), black_box(31), black_box(31))
                    .expect("the fixture table cell exists"),
            )
        });
    });

    criterion.bench_function("status_summary", |bencher| {
        bencher.iter(|| black_box(images.status()));
    });

    // One edit re-derives the constants whose bytes it moved. This is what a
    // table keystroke and a dialog change cost on top of the edit itself.
    criterion.bench_function("reconcile_shared_bytes_15_pages", |bencher| {
        let mut edited = images.working().snapshot();
        edited
            .set_raw_at("scalar_1_0", 0, 11.0)
            .expect("the fixture variable exists");
        bencher.iter(|| {
            edited
                .reconcile_shared_bytes(black_box("scalar_1_0"))
                .expect("the page reconciles");
        });
    });

    // The fixture above has no shared bytes, so it measures the page round trip
    // alone. This one is the shape the re-derivation exists for: two tables over
    // the same bytes and a word split between two bit fields.
    criterion.bench_function("reconcile_shared_bytes_peer_page", |bencher| {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 288
page = 1
peerA = array, U08, 0, [16x16], \"\", 1, 0, 0, 255, 0
peerB = array, U08, 0, [16x16], \"\", 1, 0, 0, 255, 0
word = scalar, U08, 256, \"bits\", 1, 0
low = bits, U08, 256, [0:3]
high = bits, U08, 256, [4:7]
",
            &SettingsContext::new(),
        )
        .expect("the synthetic definition parses");
        let mut edited = Tune::from_definition(&definition);
        edited
            .set_raw_at("peerA", 100, 11.0)
            .expect("the fixture variable exists");
        bencher.iter(|| {
            edited
                .reconcile_shared_bytes(black_box("peerA"))
                .expect("the page reconciles");
        });
    });
}

criterion_group!(benches, budget);
criterion_main!(benches);
