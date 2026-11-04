//! The install path, exercised in its own test binary so the global
//! subscriber is installed once and does not leak into another test.

use std::fs;

use oxtuner_log::{FilePolicy, LogConfig, Profile};

#[test]
fn install_writes_a_run_file_and_refuses_a_second_subscriber() {
    let directory = tempfile::tempdir().expect("temp dir");
    let config = LogConfig::new(directory.path())
        .with_profile(Profile::Debug)
        .with_file(FilePolicy::On)
        .with_ring_capacity(8);

    let handle = oxtuner_log::init(&config).expect("install logging");
    let store = handle.store();
    let path = handle.path().expect("a file was requested").to_path_buf();
    assert_eq!(handle.file_dropped(), 0);

    tracing::info!(target: "oxtuner_log::tests", op = "op-1", pages = 2, "wrote two pages");

    let snapshot = store.snapshot();
    let record = snapshot
        .records
        .iter()
        .find(|record| record.message.as_ref() == "wrote two pages")
        .expect("the test record is in the ring");
    assert_eq!(record.op.as_deref(), Some("op-1"));

    let second = oxtuner_log::init(
        &LogConfig::new(directory.path())
            .with_profile(Profile::Debug)
            .with_file(FilePolicy::Off),
    );
    assert!(second.is_err());

    // Dropping the handle drops the worker guard, which flushes and joins.
    drop(handle);

    let text = fs::read_to_string(&path).expect("read the run file");
    assert!(text.contains("wrote two pages"), "{text}");
}
