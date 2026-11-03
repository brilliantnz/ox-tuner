//! UI model benchmarks: table grid lookups without a window.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-ui
//! ```
//!
//! These bench the pure grid model (`TableModel`), not GPUI rendering. Window
//! rendering stays covered by the `oxtuner-app` test-with-budget harness.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_core::{AxisSummary, CellState, TableSummary};
use oxtuner_ui::table::{CellPos, TableModel};

/// A 32x32 table snapshot, the largest grid the editor draws.
fn table() -> TableSummary {
    let axis = |name: &str| AxisSummary {
        name: name.to_owned(),
        label: name.to_owned(),
        units: String::new(),
        channel: Some("rpm".to_owned()),
        digits: 0,
        minimum: None,
        maximum: None,
        values: (0..32).map(|index| index as f64 * 500.0).collect(),
    };

    TableSummary {
        id: "veTable".to_owned(),
        name: "veTable".to_owned(),
        title: "VE Table".to_owned(),
        cols: 32,
        rows: 32,
        digits: 0,
        units: "%".to_owned(),
        cells: vec![50.0; 1024],
        states: vec![CellState::Unknown; 1024],
        changed: vec![false; 1024],
        x_axis: axis("rpmBins"),
        y_axis: axis("loadBins"),
        minimum: 0.0,
        maximum: 100.0,
        cell_minimum: None,
        cell_maximum: None,
        writable: true,
        curve: None,
    }
}

fn model() -> TableModel {
    let mut model = TableModel::new();
    model.set_table(table());

    model
}

fn bench_set_table(criterion: &mut Criterion) {
    let snapshot = table();

    criterion.bench_function("ui_table_set_32x32", |bencher| {
        bencher.iter_with_setup(TableModel::new, |mut model| {
            model.set_table(black_box(snapshot.clone()));
            black_box(());
        });
    });
}

fn bench_cell_value(criterion: &mut Criterion) {
    let model = model();
    let cell = CellPos {
        row: 17,
        column: 23,
    };

    criterion.bench_function("ui_table_cell_value", |bencher| {
        bencher.iter(|| black_box(model.value(black_box(cell))));
    });
}

fn bench_realtime_cell(criterion: &mut Criterion) {
    let model = model();

    criterion.bench_function("ui_table_realtime_cell", |bencher| {
        bencher.iter(|| black_box(model.realtime_cell(black_box(3500.0), black_box(80.0))));
    });
}

criterion_group!(
    benches,
    bench_set_table,
    bench_cell_value,
    bench_realtime_cell
);
criterion_main!(benches);
