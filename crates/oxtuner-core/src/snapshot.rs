//! Snapshots before risky operations (ADR-0011 S4).
//!
//! A snapshot is a complete `.msq` rendered from the **working** image, not a
//! copy of the project file: unsaved edits are part of what the user is about to
//! write, burn, revert or replace, so they must be recoverable. Files live in
//! `<project>/.oxtuner/snapshots/` and are pruned to a bounded count.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use oxtuner_project::Project;

/// How many snapshots one project keeps.
pub(crate) const MAX_SNAPSHOTS: usize = 50;

/// The next unused sequence for a snapshot write in this process.
///
/// It backs both the final name (write order) and the temporary suffix
/// (uniqueness); fetching once keeps the two in step.
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Why a snapshot could not be written.
#[derive(Debug)]
pub(crate) enum SnapshotError {
    /// The snapshot directory could not be created.
    Directory {
        /// The directory that failed.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
    /// The working image could not be rendered as a `.msq`.
    Render(oxtuner_project::ProjectError),
    /// The snapshot file could not be written.
    Write {
        /// The snapshot path that was being written.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
    /// The temporary file could not be moved into place.
    Replace {
        /// The snapshot path the rename targeted.
        path: PathBuf,
        /// The underlying I/O failure.
        source: io::Error,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directory { path, source } => {
                write!(
                    formatter,
                    "the snapshot directory {} could not be created: {source}",
                    path.display()
                )
            }
            Self::Render(error) => {
                write!(
                    formatter,
                    "the tune could not be rendered for a snapshot: {error}"
                )
            }
            Self::Write { path, source } => {
                write!(
                    formatter,
                    "the snapshot {} could not be written: {source}",
                    path.display()
                )
            }
            Self::Replace { path, source } => {
                write!(
                    formatter,
                    "the snapshot {} could not be moved into place: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Directory { source, .. }
            | Self::Write { source, .. }
            | Self::Replace { source, .. } => Some(source),
            Self::Render(error) => Some(error),
        }
    }
}

/// The snapshot directory inside a project's sidecar.
pub(crate) fn directory(root: &Path) -> PathBuf {
    root.join(oxtuner_project::SIDECAR_DIR).join("snapshots")
}

/// The final name of one snapshot.
///
/// The sequence is zero-padded to the full `u64` width so files written in the
/// same millisecond still sort in write order.
fn snapshot_path(directory: &Path, label: &str, stamp: u128, sequence: u64) -> PathBuf {
    directory.join(format!("snapshot-{stamp}-{label}-{sequence:020}.msq"))
}

/// Writes the working image as a snapshot and prunes the oldest files.
///
/// `label` names the operation (`write`, `burn`, `import`, `restore`, `revert`);
/// the unix timestamp in the file name keeps the list sorted.
///
/// # Errors
///
/// Returns [`SnapshotError`] naming the directory, render, write or rename step
/// that failed.
pub(crate) fn write(project: &Project, label: &str) -> Result<PathBuf, SnapshotError> {
    debug_assert!(
        !label.contains(['/', '\\']) && !label.contains(".."),
        "snapshot label must be a plain name: {label}"
    );

    let directory = directory(project.root());
    fs::create_dir_all(&directory).map_err(|source| SnapshotError::Directory {
        path: directory.clone(),
        source,
    })?;

    let bytes = project.snapshot_msq().map_err(SnapshotError::Render)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    // The sequence keeps two writes in one millisecond from sharing the final
    // name, and the process id keeps their temporaries apart.
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = snapshot_path(&directory, label, stamp, sequence);

    // Written through a sibling temp file so a crash cannot leave half a
    // snapshot where a restore would find it.
    let temporary = path.with_extension(format!("msq.{}.{sequence}.tmp", std::process::id()));
    fs::write(&temporary, bytes).map_err(|source| {
        let _ = fs::remove_file(&temporary);
        SnapshotError::Write {
            path: path.clone(),
            source,
        }
    })?;
    fs::rename(&temporary, &path).map_err(|source| {
        let _ = fs::remove_file(&temporary);
        SnapshotError::Replace {
            path: path.clone(),
            source,
        }
    })?;

    prune(&directory);
    Ok(path)
}

/// Snapshots in `root`, oldest first. Unreadable directories are empty.
///
/// Order comes from the parsed timestamp and sequence, so different labels in
/// one millisecond stay in write order. A pre-sequence name has no sequence and
/// falls back to lexical order for the tie.
pub(crate) fn list(root: &Path) -> Vec<PathBuf> {
    sorted_snapshots(&directory(root))
}

/// Drops the oldest snapshots over [`MAX_SNAPSHOTS`].
///
/// An interrupted write leaves a `*.tmp` file behind, which this and [`list`]
/// ignore. It is not swept: a live writer's temporary looks the same.
fn prune(directory: &Path) {
    let mut paths = sorted_snapshots(directory);
    while paths.len() > MAX_SNAPSHOTS {
        if let Some(oldest) = paths.first().cloned() {
            if let Err(error) = fs::remove_file(&oldest) {
                tracing::warn!(%error, path = %oldest.display(), "could not prune a snapshot");
                break;
            }
            paths.remove(0);
        }
    }
}

/// The snapshot files in `directory`, oldest first.
fn sorted_snapshots(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("msq"))
        })
        .collect();
    paths.sort_by(|left, right| {
        snapshot_order(left)
            .cmp(&snapshot_order(right))
            .then_with(|| left.cmp(right))
    });
    paths
}

/// The `(timestamp, sequence)` a snapshot name carries.
///
/// A pre-sequence name has no numeric sequence, so it sorts as zero and falls
/// back to the lexical tiebreak.
fn snapshot_order(path: &Path) -> (u128, u64) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return (0, 0);
    };
    let stamp = name
        .strip_prefix("snapshot-")
        .and_then(|rest| rest.split('-').next())
        .and_then(|stamp| stamp.parse().ok())
        .unwrap_or(0);
    let sequence = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .and_then(|(_, sequence)| sequence.parse().ok())
        .unwrap_or(0);
    (stamp, sequence)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_dir() -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!(
            "oxtuner-snapshot-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("temp dir");
        directory
    }

    #[test]
    fn pruning_keeps_the_newest_snapshots_and_drops_the_rest() {
        let directory = temp_dir();
        let stamp = 1_700_000_000_000;
        let total = MAX_SNAPSHOTS + 3;

        for sequence in 0..total {
            let sequence = u64::try_from(sequence).expect("a small count");
            let path = snapshot_path(&directory, "write", stamp, sequence);
            fs::write(&path, b"snapshot").expect("write a snapshot");
        }

        prune(&directory);

        let remaining = sorted_snapshots(&directory);
        assert_eq!(remaining.len(), MAX_SNAPSHOTS);
        assert_eq!(
            remaining.first(),
            Some(&snapshot_path(&directory, "write", stamp, 3)),
            "the oldest three are pruned"
        );
        let newest = u64::try_from(total - 1).expect("a small count");
        assert_eq!(
            remaining.last(),
            Some(&snapshot_path(&directory, "write", stamp, newest)),
            "the newest snapshot survives"
        );
    }

    #[test]
    fn the_snapshot_directory_is_the_project_sidecar() {
        let root = Path::new("/projects/roadster");

        assert_eq!(
            directory(root),
            Path::new("/projects/roadster/.oxtuner/snapshots")
        );
    }

    #[test]
    fn listing_ignores_temporary_files() {
        let root = temp_dir();
        let snapshots = directory(&root);
        fs::create_dir_all(&snapshots).expect("directory");
        fs::write(snapshots.join("snapshot-1-write.msq"), b"x").expect("write");
        fs::write(snapshots.join("snapshot-2-write.msq.tmp"), b"x").expect("write");

        let listed = list(&root);

        assert_eq!(listed.len(), 1);
        assert!(listed[0].ends_with("snapshot-1-write.msq"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn two_snapshots_in_one_millisecond_get_distinct_names() {
        let directory = Path::new("/projects/roadster/.oxtuner/snapshots");
        let stamp = 1_700_000_000_000;

        let first = snapshot_path(directory, "write", stamp, 0);
        let second = snapshot_path(directory, "write", stamp, 1);

        assert_ne!(first, second);
    }

    #[test]
    fn a_sequence_keeps_snapshot_names_in_write_order() {
        let root = temp_dir();
        let snapshots = directory(&root);
        fs::create_dir_all(&snapshots).expect("directory");
        let first = snapshot_path(&snapshots, "write", 1_700_000_000_000, 2);
        let second = snapshot_path(&snapshots, "write", 1_700_000_000_000, 10);
        fs::write(&first, b"x").expect("write");
        fs::write(&second, b"x").expect("write");

        let listed = list(&root);

        assert_eq!(
            listed,
            [first, second],
            "padded sequences sort in write order"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_newer_sequence_sorts_before_a_later_label() {
        let root = temp_dir();
        let snapshots = directory(&root);
        fs::create_dir_all(&snapshots).expect("directory");
        let older = snapshot_path(&snapshots, "write", 1_700_000_000_000, 5);
        let newer = snapshot_path(&snapshots, "burn", 1_700_000_000_000, 6);
        fs::write(&older, b"x").expect("write");
        fs::write(&newer, b"x").expect("write");

        let listed = list(&root);

        assert_eq!(
            listed,
            [older, newer],
            "the sequence decides the order when the timestamp ties"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_snapshot_write_error_keeps_its_cause_and_names_the_path() {
        let path = PathBuf::from("/projects/roadster/.oxtuner/snapshots/snapshot-1.msq");
        let write = SnapshotError::Write {
            path: path.clone(),
            source: io::Error::other("disk full"),
        };
        let directory = SnapshotError::Directory {
            path: path
                .parent()
                .expect("the snapshot path has a directory")
                .to_path_buf(),
            source: io::Error::other("permission denied"),
        };

        for error in [write, directory] {
            assert!(error.to_string().contains("snapshots"), "{error}");
            assert!(
                std::error::Error::source(&error).is_some(),
                "the cause is lost: {error}"
            );
        }
        assert!(
            SnapshotError::Write {
                path,
                source: io::Error::other("disk full"),
            }
            .to_string()
            .contains("snapshot-1.msq"),
            "the message names the snapshot, not only the temporary file"
        );
    }
}
