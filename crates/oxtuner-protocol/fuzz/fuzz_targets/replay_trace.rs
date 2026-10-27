#![no_main]

//! Trace parsing is the other untrusted-text entry point: a corrupted or
//! hand-edited recording must be a clean error, never a panic.

use libfuzzer_sys::fuzz_target;
use oxtuner_transport::Trace;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    if let Ok(trace) = Trace::parse(&text) {
        // A parsed trace re-serialises and parses again to the same frames.
        let again = Trace::parse(&trace.to_text()).expect("our own output parses");
        assert_eq!(trace.frames(), again.frames());
    }
});
