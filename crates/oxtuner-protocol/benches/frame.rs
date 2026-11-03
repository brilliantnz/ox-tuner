//! Protocol benchmarks: CRC, framing, and pattern matching.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-protocol
//! ```

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_protocol::{Frame, FrameDecoder, crc32};

fn bench_crc32(criterion: &mut Criterion) {
    let block: Vec<u8> = (0..288u32).map(|index| (index % 251) as u8).collect();

    criterion.bench_function("protocol_crc32_288_bytes", |bencher| {
        bencher.iter(|| black_box(crc32(black_box(&block))));
    });
}

fn bench_frame_encode(criterion: &mut Criterion) {
    let payload = vec![0x51u8; 64];

    criterion.bench_function("protocol_frame_encode_64_bytes", |bencher| {
        bencher.iter(|| black_box(Frame::encode(black_box(&payload)).expect("encodes")));
    });
}

fn bench_frame_decode(criterion: &mut Criterion) {
    let payload = vec![0x51u8; 64];
    let bytes = Frame::encode(&payload).expect("encodes");

    criterion.bench_function("protocol_frame_parse_64_bytes", |bencher| {
        bencher.iter(|| black_box(Frame::parse(black_box(&bytes)).expect("decodes")));
    });

    criterion.bench_function("protocol_frame_stream_64_bytes", |bencher| {
        bencher.iter_with_setup(FrameDecoder::new, |mut decoder| {
            black_box(decoder.push(black_box(&bytes)).expect("decodes"))
        });
    });
}

criterion_group!(benches, bench_crc32, bench_frame_encode, bench_frame_decode);
criterion_main!(benches);
