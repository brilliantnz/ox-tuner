//! Transport benchmarks: trace parse and serialization.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-transport
//! ```
//!
//! The fixtures are synthetic. No hardware is touched.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_transport::{Direction, Trace, TraceFrame};

fn trace_text(frames: usize) -> String {
    let mut trace = Trace::new();
    trace.note("signature: bench 1");

    for index in 0..frames {
        let payload = vec![0x51, (index % 251) as u8, 0x00, 0x01];
        trace.push(TraceFrame::new(
            Direction::Tx,
            payload,
            Duration::from_millis(index as u64),
        ));
        trace.push(TraceFrame::new(
            Direction::Rx,
            vec![0xAA; 8],
            Duration::from_millis(index as u64 + 1),
        ));
    }

    trace.to_text()
}

fn bench_trace_parse(criterion: &mut Criterion) {
    let text = trace_text(200);

    criterion.bench_function("transport_trace_parse_400_frames", |bencher| {
        bencher.iter(|| black_box(Trace::parse(black_box(&text)).expect("parses")));
    });
}

fn bench_trace_serialize(criterion: &mut Criterion) {
    let text = trace_text(200);
    let trace = Trace::parse(&text).expect("parses");

    criterion.bench_function("transport_trace_to_text_400_frames", |bencher| {
        bencher.iter(|| black_box(black_box(&trace).to_text()));
    });
}

criterion_group!(benches, bench_trace_parse, bench_trace_serialize);
criterion_main!(benches);
