//! Snapshots before risky operations (ADR-0011 S4).
//!
//! A snapshot is a complete `.msq` rendered from the **working** image, not a
//! copy of the project file: unsaved edits are part of what the user is about to
//! write, burn, revert or replace, so they must be recoverable. Files live in
//! `<project>/.ox-tuner/snapshots/` and are pruned to a bounded count.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use oxtuner_project::Project;

/// How many snapshots one project keeps.
pub(crate) const MAX_SNAPSHOTS: usize = 50;

/// The snapshot directory inside a project's sidecar.
pub(crate) fn directory(root: &Path) -> PathBuf {
    root.join(oxtuner_project::SIDECAR_DIR).join("snapshots")
}

/// Writes the working image as a snapshot and prunes the oldest files.
///
/// `label` names the operation (`write`, `burn`, `import`, `restore`, `revert`);
/// the unix timestamp in the file name keeps the list sorted.
pub(crate) fn write(project: &Project, label: &str) -> Result<PathBuf, String> {
    let directory = directory(project.root());
    fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "the snapshot directory {} could not be created: {error}",
            directory.display()
        )
    })?;

    let bytes = project
        .snapshot_msq()
        .map_err(|error| format!("the tune could not be rendered for a snapshot: {error}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let path = directory.join(format!("snapshot-{stamp}-{label}.msq"));

    // Written through a sibling temp file so a crash cannot leave a half a
    // snapshot where a restore would find it.
    let temporary = path.with_extension("msq.tmp");
    fs::write(&temporary, bytes).map_err(|error| {
        format!(
            "the snapshot {} could not be written: {error}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!(
            "the snapshot {} could not be moved into place: {error}",
            path.display()
        )
    })?;

    prune(&directory);
    Ok(path)
}

/// Snapshots in `root`, oldest first. Unreadable directories are empty.
pub(crate) fn list(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(directory(root))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("msq"))
        })
        .collect();
    paths.sort();
    paths
}

fn prune(directory: &Path) {
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
    paths.sort();
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
    fn the_snapshot_directory_is_the_project_sidecar() {
        let root = Path::new("/projects/roadster");

        assert_eq!(
            directory(root),
            Path::new("/projects/roadster/.ox-tuner/snapshots")
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
}
