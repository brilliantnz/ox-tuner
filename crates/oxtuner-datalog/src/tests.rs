//! Session-log tests: file shape, bounded accounting and clean flush.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{LogColumn, LogStatus, Logger};

/// A fresh directory under the system temporary directory.
fn temp_dir(test: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let directory = std::env::temp_dir().join(format!(
        "oxtuner-datalog-{test}-{}-{stamp}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("create temp dir");
    directory
}

fn columns() -> Vec<LogColumn> {
    vec![
        LogColumn::new("rpm", "rpm"),
        LogColumn::new("map", "kPa"),
        LogColumn::new("afr", ""),
    ]
}

#[test]
fn a_session_writes_the_header_and_every_frame_in_order() {
    let directory = temp_dir("session");
    let path = directory.join("session.csv");
    let status = LogStatus::new();

    let logger = Logger::start(&path, columns(), status.clone()).expect("starts");
    assert!(status.is_running());
    assert_eq!(status.path().as_deref(), Some(path.as_path()));

    for step in 0..200 {
        assert!(logger.try_log(
            f64::from(step) * 0.05,
            &[1000.0 + f64::from(step), 50.0, 14.7]
        ));
    }
    let summary = logger.stop().expect("stops cleanly");

    assert_eq!(summary.frames, 200);
    assert_eq!(summary.dropped, 0);
    assert!(summary.bytes > 0);
    assert!(!status.is_running());

    let text = fs::read_to_string(&path).expect("read log");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 201, "header plus one row per frame");
    assert_eq!(lines[0], "Time,rpm,map,afr");
    assert_eq!(lines[1], "0.0000,1000.0000,50.0000,14.7000");
    assert_eq!(lines[200], "9.9500,1199.0000,50.0000,14.7000");

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_stopped_log_is_flushed_without_a_grace_period() {
    let directory = temp_dir("flush");
    let path = directory.join("flush.csv");
    let status = LogStatus::new();
    let logger = Logger::start(&path, columns(), status).expect("starts");

    for step in 0..3 {
        logger.try_log(f64::from(step), &[1.0, 2.0, 3.0]);
    }
    logger.stop().expect("stops");

    let text = fs::read_to_string(&path).expect("read log");
    assert_eq!(text.lines().count(), 4);
    assert!(text.ends_with('\n'));

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn an_unwritable_path_is_an_error_not_an_empty_log() {
    let directory = temp_dir("unwritable");
    let path = directory.join("missing").join("session.csv");
    fs::write(directory.join("missing"), b"not a directory").expect("blocking file");

    let error = Logger::start(&path, columns(), LogStatus::new()).expect_err("must refuse");

    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::NotADirectory | std::io::ErrorKind::AlreadyExists
        ),
        "unexpected error kind: {:?}",
        error.kind()
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn the_queue_accounts_for_every_frame_it_is_given() {
    let directory = temp_dir("accounting");
    let path: PathBuf = directory.join("accounting.csv");
    let status = LogStatus::new();
    let logger = Logger::start(&path, columns(), status).expect("starts");

    let attempts = 10_000u32;
    for step in 0..attempts {
        logger.try_log(f64::from(step), &[f64::from(step), 0.0, 0.0]);
    }
    let summary = logger.stop().expect("stops");

    assert_eq!(
        summary.frames + summary.dropped,
        u64::from(attempts),
        "every queued or refused frame is counted exactly once"
    );

    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn a_log_directory_is_created_on_demand() {
    let directory = temp_dir("create-dir");
    let path = directory.join("nested").join("logs").join("session.csv");

    let logger = Logger::start(&path, columns(), LogStatus::new()).expect("starts");
    logger.stop().expect("stops");

    assert!(Path::new(&path).is_file());

    let _ = fs::remove_dir_all(&directory);
}
