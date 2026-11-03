//! Datalog benchmarks: what the poller pays to hand a frame to the writer.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-datalog
//! ```
//!
//! `try_log` never blocks. A tight loop fills the bounded channel quickly, so
//! most iterations take the saturated path: copy the values, fail the send,
//! count the drop. That is the number that matters for the poller — the writer
//! thread may be as slow as the disk and the realtime loop must not wait for
//! it. The write itself is a separate thread and is not what this measures.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_datalog::{LogColumn, LogStatus, Logger};

fn columns(count: usize) -> Vec<LogColumn> {
    (0..count)
        .map(|index| LogColumn::new(format!("channel_{index}"), "units"))
        .collect()
}

fn bench_try_log(criterion: &mut Criterion) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("bench.csv");
    let status = LogStatus::new();
    let logger = Logger::start(&path, columns(8), status).expect("starts");
    let values = vec![3000.0; 8];
    let mut time = 0.0;

    criterion.bench_function("datalog_try_log_saturated_8_columns", |bencher| {
        bencher.iter(|| {
            time += 0.01;

            black_box(logger.try_log(black_box(time), black_box(&values)))
        });
    });

    logger.stop().expect("stops");
}

criterion_group!(benches, bench_try_log);
criterion_main!(benches);
