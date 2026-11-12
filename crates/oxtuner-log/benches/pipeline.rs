//! Logging benchmarks: one event through the pipeline, and the Diagnostics
//! snapshot the view takes each time it refreshes.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-log
//! ```
//!
//! The pipeline is installed once per process, exactly as the binary installs
//! it, with the file layer off so the disk is not part of the measurement.
//! Events are emitted under the `oxtuner_bench` target because the default
//! filter only enables the workspace prefix.
//!
//! If `RUST_LOG` is set to something that excludes `oxtuner_=info`, the probe
//! fails with a message instead of silently benchmarking an empty ring.

use std::hint::black_box;
use std::sync::{Mutex, OnceLock};

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_log::{FilePolicy, LogConfig, LogHandle, LogStore, init};

/// The installed pipeline, kept alive for the whole bench process.
fn store() -> LogStore {
    static HANDLE: OnceLock<Mutex<Option<LogHandle>>> = OnceLock::new();

    let slot = HANDLE.get_or_init(|| Mutex::new(None));
    let mut slot = slot
        .lock()
        .expect("a previous panic poisoned the bench setup lock");

    let handle = slot.get_or_insert_with(|| {
        let directory =
            std::env::temp_dir().join(format!("oxtuner-bench-log-{}", std::process::id()));

        init(&LogConfig::new(directory).with_file(FilePolicy::Off))
            .expect("installs the logging pipeline")
    });

    let store = handle.store();

    tracing::info!(target: "oxtuner_bench", probe = true, "bench probe");
    assert!(
        store.revision() > 0,
        "the bench events were filtered out; unset RUST_LOG or include `oxtuner_=info`"
    );

    store
}

/// One event from `tracing::info!` to a record in the ring: field capture, two
/// RFC 3339 timestamps, the mutex, and the push.
fn bench_event_capture(criterion: &mut Criterion) {
    let _store = store();

    criterion.bench_function("log_event_capture", |bencher| {
        bencher.iter(|| {
            tracing::info!(
                target: "oxtuner_bench",
                request = black_box(42u64),
                rpm = black_box(3000.0f64),
                "a request finished"
            );
        });
    });
}

/// The Diagnostics view clones every record it shows. The ring holds 5 000 by
/// default, so this is the cost of one refresh at the default capacity.
fn bench_snapshot(criterion: &mut Criterion) {
    let store = store();

    while store.len() < 5_000 {
        tracing::info!(target: "oxtuner_bench", "filling the ring");
    }

    criterion.bench_function("log_store_snapshot_5000", |bencher| {
        bencher.iter(|| black_box(store.snapshot()));
    });
}

criterion_group!(benches, bench_event_capture, bench_snapshot);
criterion_main!(benches);
