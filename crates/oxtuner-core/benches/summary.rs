//! Core summary and history benchmarks: what the UI reads every frame.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-core
//! ```
//!
//! These bench the UI-facing summaries and the chart-history helpers through
//! public API only, never the threaded command loop. The fixtures are
//! synthetic.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_core::{
    AxisView, ChannelInfo, ChartHistory, DefinitionView, RealtimeDecoder, Sample, SettingsContext,
    TableView, decimate,
};

const INI: &str = "\
[MegaTune]
signature = \"bench 1\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 288
page = 1
reqFuel = scalar, U16, 0, \"ms\", 0.1, 0, 0, 25.5, 1
rpmBins = array, U08, 2, [16], \"RPM\", 100, 0, 0, 25500, 0
loadBins = array, U08, 18, [16], \"kPa\", 1, 0, 0, 255, 0
veTable = array, U08, 34, [16x16], \"%\", 1, 0, 0, 255, 0

[OutputChannels]
ochBlockSize = 4
rpm = scalar, U16, 0, \"RPM\", 1.0, 0.0
map = scalar, U16, 2, \"kPa\", 0.1, 0.0

[TableEditor]
table = veTable, veMap, \"VE Table\"
xBins = rpmBins, rpm
yBins = loadBins, map
zBins = veTable

[Menu]
menu = \"Fuel\"
subMenu = veTable, \"VE Table\"
";

fn definition() -> oxtuner_ini::Definition {
    oxtuner_ini::parse_str(INI, &SettingsContext::new()).expect("parses")
}

/// A 16x16 table snapshot, the shape the grid renders.
fn table() -> TableView {
    let axis = |name: &str| AxisView {
        name: name.to_owned(),
        label: name.to_owned(),
        units: String::new(),
        channel: None,
        digits: 0,
        minimum: None,
        maximum: None,
        power_cycle: false,
        values: vec![0.0; 16],
    };

    TableView {
        id: "veTable".to_owned(),
        name: "veTable".to_owned(),
        title: "VE Table".to_owned(),
        cols: 16,
        rows: 16,
        digits: 0,
        units: "%".to_owned(),
        cells: vec![50.0; 256],
        states: vec![oxtuner_core::CellState::Unknown; 256],
        changed: vec![false; 256],
        x_axis: axis("rpmBins"),
        y_axis: axis("loadBins"),
        minimum: 0.0,
        maximum: 100.0,
        cell_minimum: None,
        cell_maximum: None,
        writable: true,
        power_cycle: false,
        curve: None,
    }
}

fn bench_definition_summary(criterion: &mut Criterion) {
    let definition = definition();

    criterion.bench_function("core_definition_summary_synthetic", |bencher| {
        bencher.iter(|| {
            black_box(DefinitionView::new(
                black_box("mainController.ini"),
                black_box(&definition),
            ))
        });
    });
}

/// The render path clones the summary every frame (`layout/mod.rs`); this is
/// the cost of one clone.
fn bench_table_clone(criterion: &mut Criterion) {
    let snapshot = table();

    criterion.bench_function("core_table_summary_clone_16x16", |bencher| {
        bencher.iter(|| black_box(black_box(&snapshot).clone()));
    });
}

fn bench_realtime_decode(criterion: &mut Criterion) {
    let definition = definition();
    let decoder = RealtimeDecoder::new(&definition);
    let block = vec![0u8; decoder.block_size()];
    assert_eq!(
        decoder.decode(&block, 1).values.len(),
        2,
        "the decode fixture must decode, not take the wrong-size path"
    );

    criterion.bench_function("core_realtime_decode", |bencher| {
        bencher.iter(|| black_box(decoder.decode(black_box(&block), black_box(1))));
    });
}

/// A definition whose decoded frame includes a computed channel.
const COMPUTED_INI: &str = "\
[MegaTune]
signature = \"bench computed\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"RPM\", 1.0, 0.0
map = scalar, U16, 2, \"kPa\", 0.1, 0.0
halfRpm = { rpm / 2 }, \"RPM\"
";

/// The same decode with a computed channel, so the map it builds is read too.
fn bench_realtime_decode_computed(criterion: &mut Criterion) {
    let definition = oxtuner_ini::parse_str(COMPUTED_INI, &SettingsContext::new())
        .expect("the computed fixture parses");
    let decoder = RealtimeDecoder::new(&definition);
    let block = vec![0u8; decoder.block_size()];
    assert_eq!(
        decoder.decode(&block, 1).values.len(),
        3,
        "the computed fixture must decode, not take the wrong-size path"
    );

    criterion.bench_function("core_realtime_decode_computed", |bencher| {
        bencher.iter(|| black_box(decoder.decode(black_box(&block), black_box(1))));
    });
}

/// A definition with `[FrontPage]` indicators, for the per-frame evaluation.
const INDICATOR_INI: &str = "\
[MegaTune]
signature = \"bench 1\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1

[FrontPage]
indicator = { rpm > 1000 }, \"Low\", \"High\", white, black, red, black
indicator = { map > 50 }, \"Light\", \"Loaded\", white, black, green, black

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"RPM\", 1.0, 0.0
map = scalar, U16, 2, \"kPa\", 0.1, 0.0
";

/// Evaluates the front-page indicators for one frame, as the poller does.
fn bench_realtime_indicators(criterion: &mut Criterion) {
    let definition = oxtuner_ini::parse_str(INDICATOR_INI, &SettingsContext::new())
        .expect("the indicator fixture parses");
    let decoder = RealtimeDecoder::new(&definition);
    let block = vec![0u8; decoder.block_size().max(1)];
    let frame = decoder.decode(&block, 1);

    criterion.bench_function("core_realtime_indicators", |bencher| {
        bencher.iter(|| {
            black_box(decoder.indicators(black_box(&frame), None));
        });
    });
}

fn bench_history_push(criterion: &mut Criterion) {
    let history = ChartHistory::new(6000);
    history.configure(vec![
        ChannelInfo::new("rpm", "RPM"),
        ChannelInfo::new("map", "kPa"),
    ]);

    let mut time = 0.0;

    criterion.bench_function("core_history_push_frame", |bencher| {
        bencher.iter(|| {
            time += 0.01;

            history.push_frame(
                black_box(time),
                black_box(&[("rpm", 3000.0), ("map", 90.0)]),
            );
            black_box(());
        });
    });
}

fn bench_decimate(criterion: &mut Criterion) {
    let points: Vec<Sample> = (0..6000)
        .map(|index| Sample {
            t: f64::from(index) * 0.01,
            v: f64::from(index).sin() * 1000.0 + 3000.0,
        })
        .collect();

    criterion.bench_function("core_decimate_6000_to_300", |bencher| {
        bencher.iter(|| black_box(decimate(black_box(&points), black_box(300))));
    });
}

criterion_group!(
    benches,
    bench_definition_summary,
    bench_table_clone,
    bench_realtime_decode,
    bench_realtime_decode_computed,
    bench_realtime_indicators,
    bench_history_push,
    bench_decimate
);
criterion_main!(benches);
