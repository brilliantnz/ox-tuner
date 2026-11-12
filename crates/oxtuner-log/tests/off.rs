//! `FilePolicy::Off` in its own test binary: the global subscriber is
//! installed once per process.

use oxtuner_log::{FilePolicy, LogConfig, Profile};

#[test]
fn a_fileless_build_leaves_the_directory_alone() {
    let root = tempfile::tempdir().expect("temp dir");
    let logs = root.path().join("logs");
    let config = LogConfig::new(&logs)
        .with_profile(Profile::Debug)
        .with_file(FilePolicy::Off);

    let handle = oxtuner_log::init(&config).expect("install logging");

    assert!(
        !logs.exists(),
        "no run file was asked for, so no directory is created"
    );
    assert_eq!(handle.path(), None);

    drop(handle);
}
