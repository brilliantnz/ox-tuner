//! Atomic JSON files for settings and session state.
//!
//! Both files are small, human-readable and user-editable. A write goes to a
//! sibling temp file which is flushed and then renamed over the target, so a
//! crash mid-write leaves the previous file intact instead of a truncated one.
//!
//! A file that cannot be parsed is moved aside rather than overwritten: the
//! user can still inspect or repair what the app could not read.

use std::{
    fs::{self, File},
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Serialize, de::DeserializeOwned};

/// Reads and parses a JSON file, or returns `None` when it does not exist.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Writes JSON atomically: create parent directories, write a temp file, flush
/// it, then rename it over the target.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let temp = temp_path(path);
    {
        let mut file = File::create(&temp)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }

    // `rename` replaces an existing file atomically on Unix. Windows refuses
    // to replace through `std::fs::rename`, so remove the target first; the
    // previous version is still recoverable from the temp file if that removal
    // succeeds and the rename then fails.
    #[cfg(windows)]
    if path.exists() {
        let _ = fs::remove_file(path);
    }

    fs::rename(&temp, path)
}

/// Moves an unreadable file out of the way and returns its new path.
pub(crate) fn quarantine(path: &Path) -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let backup = path.with_extension(format!("invalid-{stamp}.json"));
    fs::rename(path, &backup)?;
    Ok(backup)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde::{Deserialize, Serialize};

    use super::*;

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("oxtuner-{name}-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        name: String,
        count: u32,
    }

    #[test]
    fn missing_file_reads_as_none() {
        let dir = TempDir::new("missing");
        let path = dir.join("settings.json");

        assert_eq!(read_json::<Sample>(&path).expect("read"), None);
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = TempDir::new("round-trip");
        let path = dir.join("nested/settings.json");
        let sample = Sample {
            name: "ox-tuner".into(),
            count: 3,
        };

        write_json(&path, &sample).expect("write");
        let loaded = read_json::<Sample>(&path).expect("read").expect("present");

        assert_eq!(loaded, sample);
        assert!(
            !dir.join("nested/settings.json.tmp").exists(),
            "the temp file must be renamed away"
        );
    }

    #[test]
    fn a_second_write_replaces_the_first() {
        let dir = TempDir::new("replace");
        let path = dir.join("settings.json");

        write_json(
            &path,
            &Sample {
                name: "first".into(),
                count: 1,
            },
        )
        .expect("first write");
        write_json(
            &path,
            &Sample {
                name: "second".into(),
                count: 2,
            },
        )
        .expect("second write");

        let loaded = read_json::<Sample>(&path).expect("read").expect("present");
        assert_eq!(loaded.name, "second");
    }

    #[test]
    fn unreadable_files_are_moved_aside() {
        let dir = TempDir::new("quarantine");
        let path = dir.join("settings.json");
        fs::write(&path, b"{ this is not json").expect("write broken file");

        let backup = quarantine(&path).expect("quarantine");

        assert!(!path.exists());
        assert!(backup.exists());
        assert!(backup.to_string_lossy().contains("invalid-"));
    }
}
