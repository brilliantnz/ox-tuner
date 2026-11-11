//! Crash-safe file replacement.
//!
//! A write goes to a sibling temporary file that is flushed to disk before it
//! replaces the target, so a crash or power loss leaves either the old file or
//! the new one, never a half-written tune. The parent directory is flushed
//! afterwards so the rename itself is durable too.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Replaces `path` with `bytes`.
///
/// The target keeps its permissions when it already exists. A failed write or
/// rename removes the temporary file and leaves the target untouched.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path);
    let result = replace(&temporary, path, bytes);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn replace(temporary: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Replacing a file must not change what the user can do with it.
    let permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());

    let mut file = fs::File::create(temporary)?;
    file.write_all(bytes)?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)?;
    }
    file.sync_all()?;
    drop(file);

    fs::rename(temporary, path)?;
    sync_parent(path);
    Ok(())
}

/// Replaces `path` with `bytes` without flushing to disk.
///
/// For derived data — the definition cache — where a crash losing the new file
/// costs one parse and nothing else. The rename is still atomic, so a reader
/// never sees a half-written file, but the caller does not wait for the disk.
pub(crate) fn write_derived(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = derived_temporary_path(path);
    let result = fs::write(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// A sibling temporary name a second process cannot take.
fn derived_temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", std::process::id()));
    PathBuf::from(name)
}

/// The next unused suffix for a tune temporary in this process.
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// A crash can leave one temporary behind. It is deliberately not swept here: a
// concurrent saver's live temporary looks the same as a stale one.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".oxtuner-tmp.{}.{sequence}", std::process::id()));
    PathBuf::from(name)
}

/// Flushes the directory entry so the rename survives a crash.
///
/// Not every platform can open a directory as a file. The data file is already
/// flushed by then, so a failure here only weakens durability of the rename
/// itself and is not reported.
fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "oxtuner-atomic-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_write_replaces_the_file_and_leaves_no_temporary_behind() {
        let root = temp_dir("replace");
        let path = root.join("CurrentTune.msq");
        fs::write(&path, b"old").unwrap();

        write(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("oxtuner-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_temporary_name_is_unique_per_write_and_per_process() {
        let path = Path::new("/projects/roadster/CurrentTune.msq");

        let first = temporary_path(path);
        let second = temporary_path(path);

        assert_ne!(first, second, "two savers must not share one temporary");
        for name in [&first, &second] {
            assert!(
                name.to_string_lossy()
                    .contains(&format!(".oxtuner-tmp.{}.", std::process::id())),
                "{name:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_replacement_keeps_the_permissions_it_found() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("permissions");
        let path = root.join("CurrentTune.msq");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        write(&path, b"new").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_derived_write_replaces_the_file_and_cleans_up_its_temporary() {
        let root = temp_dir("derived");
        let path = root.join("definition.defcache");
        fs::write(&path, b"old").unwrap();

        write_derived(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        let names: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["definition.defcache"], "{names:?}");

        fs::remove_dir_all(&root).unwrap();
    }
}
