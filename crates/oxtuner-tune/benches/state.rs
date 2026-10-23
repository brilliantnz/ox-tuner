//! Budget benchmark: a 15-page, 818-constant tune (the reference project's
//! shape) and the operations the UI runs per frame.
//!
//! Numbers live in `docs/architecture/performance/budget.md`. Run with:
//!
//! ```text
//! cargo bench -p oxtuner-tune
//! ```

use std::sync::Arc;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_ini::{Definition, SettingsContext, parse_str};
use oxtuner_tune::{Layout, StateTable, Tune, TuneImages};

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
        text.push_str(&format!("page = {page}\n"));
        for index in 0..scalars_per_page {
            text.push_str(&format!(
                "scalar_{page}_{index} = scalar, U08, {index}, \"\", 1, 0, 0, 255, 0\n"
            ));
            written += 1;
        }
        if page <= TABLES {
            // The largest grid the editor draws; the other seven are 16×16.
            let shape = if page == 1 { "[32x32]" } else { "[16x16]" };
            text.push_str(&format!(
                "table_{page} = array, U08, 256, {shape}, \"\", 1, 0, 0, 255, 0\n"
            ));
            written += 1;
        }
    }
    assert_eq!(
        written, CONSTANTS,
        "the fixture must match the budget's shape"
    );

    parse_str(&text, &SettingsContext::new()).expect("the synthetic definition parses")
}

fn layout(definition: &Definition) -> Arc<Layout> {
    Arc::new(Layout::build(definition, &|_| None))
}

/// A tune that looks connected: RAM and flash equal, every tenth cell edited.
fn images(layout: Arc<Layout>) -> TuneImages {
    let baseline = Tune::new(Arc::clone(&layout));
    let mut ram = baseline.snapshot();
    let mut working = baseline.snapshot();

    for (index, variable) in layout.variables().iter().enumerate() {
        if variable.owner != oxtuner_tune::Owner::Page(oxtuner_tune::IniPage::FIRST) {
            continue;
        }
        if index % 10 == 0 {
            ram.set_raw_at(&variable.name, 0, 7.0).unwrap();
            working.set_raw_at(&variable.name, 0, 9.0).unwrap();
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
        bencher.iter(|| black_box(Layout::build(black_box(&definition), &|_| None)));
    });

    criterion.bench_function("state_table_build_15_pages", |bencher| {
        bencher.iter(|| black_box(StateTable::build(black_box(&images))));
    });

    criterion.bench_function("cell_lookup_32x32_table", |bencher| {
        bencher.iter(|| {
            black_box(
                table
                    .cell(black_box("table_1"), black_box(31), black_box(31))
                    .unwrap(),
            )
        });
    });

    criterion.bench_function("status_summary", |bencher| {
        bencher.iter(|| black_box(images.status()));
    });
}

criterion_group!(benches, budget);
criterion_main!(benches);
