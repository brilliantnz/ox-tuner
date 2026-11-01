use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use time::{
    Duration, OffsetDateTime, PrimitiveDateTime, format_description::FormatItem,
    macros::format_description,
};

/// A single run may grow to this before it rolls to the next part.
const MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Run groups kept on disk.
const RETENTION_RUNS: usize = 20;
/// A run group older than this is removed even when fewer than 20 exist.
const RETENTION_DAYS: i64 = 14;
const PREFIX: &str = "oxtuner-";
const INDEX_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]-[minute]-[second]");

/// The open file group for this run.
///
/// Writes are appended as they arrive from the non-blocking writer. When the
/// current part reaches the size limit, the next part is opened and the group
/// continues under the same base name.
pub(crate) struct RunFile {
    base: PathBuf,
    file: File,
    written: u64,
    part: u32,
    max_bytes: u64,
}

impl RunFile {
    /// Opens a new run file, choosing a suffix when this second already has
    /// one.
    pub(crate) fn create(directory: &Path, now: OffsetDateTime) -> io::Result<Self> {
        Self::create_with_limit(directory, now, MAX_BYTES)
    }

    fn create_with_limit(
        directory: &Path,
        now: OffsetDateTime,
        max_bytes: u64,
    ) -> io::Result<Self> {
        let stamp = now
            .format(INDEX_FORMAT)
            .map_err(|error| io::Error::other(error.to_string()))?;

        for suffix in 0..100 {
            let name = if suffix == 0 {
                format!("{PREFIX}{stamp}.log")
            } else {
                format!("{PREFIX}{stamp}-{}.log", suffix + 1)
            };
            let base = directory.join(name);

            match OpenOptions::new().create_new(true).append(true).open(&base) {
                Ok(file) => {
                    return Ok(Self {
                        base,
                        file,
                        written: 0,
                        part: 0,
                        max_bytes,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not choose a unique log file name",
        ))
    }

    /// The first part's path, which names the group.
    pub(crate) fn path(&self) -> &Path {
        &self.base
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.part += 1;
        let path = PathBuf::from(format!("{}.{}", self.base.display(), self.part));

        self.file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(path)?;
        self.written = 0;

        Ok(())
    }
}

impl Write for RunFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buffer)?;
        self.written = self.written.saturating_add(written as u64);

        if self.written >= self.max_bytes {
            self.rotate()?;
        }

        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Removes old run groups from the log directory.
///
/// A group is a base file and all its numbered parts. A group goes when it is
/// older than [`RETENTION_DAYS`] or falls outside the newest
/// [`RETENTION_RUNS`], whichever applies first. Files that do not match the
/// run-name pattern are left alone.
pub(crate) fn prune(directory: &Path, now: OffsetDateTime) -> io::Result<Vec<PathBuf>> {
    prune_with(
        directory,
        now,
        RETENTION_RUNS,
        Duration::days(RETENTION_DAYS),
    )
}

fn prune_with(
    directory: &Path,
    now: OffsetDateTime,
    keep: usize,
    max_age: Duration,
) -> io::Result<Vec<PathBuf>> {
    let mut groups: BTreeMap<String, (OffsetDateTime, Vec<PathBuf>)> = BTreeMap::new();

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some((base, _)) = name.split_once(".log") else {
            continue;
        };
        let Some(stamp) = parse_stamp(base) else {
            continue;
        };

        groups
            .entry(base.to_owned())
            .or_insert_with(|| (stamp, Vec::new()))
            .1
            .push(entry.path());
    }

    let mut ordered: Vec<_> = groups.into_values().collect();
    ordered.sort_by_key(|left| std::cmp::Reverse(left.0));

    let mut removed = Vec::new();
    for (index, (stamp, files)) in ordered.into_iter().enumerate() {
        let too_old = now - stamp > max_age;
        if index < keep && !too_old {
            continue;
        }

        for file in files {
            if fs::remove_file(&file).is_ok() {
                removed.push(file);
            }
        }
    }

    Ok(removed)
}

/// Reads the `YYYY-MM-DDTHH-MM-SS` prefix of a run base name.
fn parse_stamp(base: &str) -> Option<OffsetDateTime> {
    let rest = base.strip_prefix(PREFIX)?;
    let stamp = rest.get(..19)?;

    PrimitiveDateTime::parse(stamp, INDEX_FORMAT)
        .ok()
        .map(PrimitiveDateTime::assume_utc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn write(path: &Path, text: &str) {
        fs::write(path, text).expect("write a fixture file");
    }

    #[test]
    fn rollover_creates_a_numbered_part() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut run =
            RunFile::create_with_limit(directory.path(), datetime!(2026-11-01 12:00 UTC), 8)
                .expect("create the run file");

        run.write_all(b"123456789").expect("write the first part");
        run.write_all(b"abc").expect("write the second part");
        run.flush().expect("flush");

        let base = run.path().to_path_buf();
        let part = PathBuf::from(format!("{}.1", base.display()));

        assert_eq!(fs::read(&base).expect("read the base part"), b"123456789");
        assert_eq!(fs::read(&part).expect("read the numbered part"), b"abc");
    }

    #[test]
    fn two_runs_in_the_same_second_get_distinct_names() {
        let directory = tempfile::tempdir().expect("temp dir");
        let now = datetime!(2026-11-01 12:00 UTC);

        let first = RunFile::create(directory.path(), now).expect("first run");
        let second = RunFile::create(directory.path(), now).expect("second run");

        assert_ne!(first.path(), second.path());
        assert!(first.path().exists());
        assert!(second.path().exists());
    }

    #[test]
    fn retention_keeps_twenty_runs_and_removes_old_parts_together() {
        let directory = tempfile::tempdir().expect("temp dir");
        let now = datetime!(2026-11-01 12:00 UTC);

        for index in 0..25 {
            let stamp = now - Duration::minutes(index);
            let name = stamp.format(INDEX_FORMAT).expect("format a fixture stamp");
            write(&directory.path().join(format!("{PREFIX}{name}.log")), "run");
            if index == 24 {
                write(
                    &directory.path().join(format!("{PREFIX}{name}.log.1")),
                    "part",
                );
            }
        }

        let old = now - Duration::days(20);
        let old_name = old.format(INDEX_FORMAT).expect("format the old stamp");
        write(
            &directory.path().join(format!("{PREFIX}{old_name}.log")),
            "old",
        );
        write(&directory.path().join("notes.txt"), "keep me");

        let removed =
            prune_with(directory.path(), now, 20, Duration::days(14)).expect("prune the directory");

        assert_eq!(removed.len(), 7, "five runs, one part and the old run");
        assert!(directory.path().join("notes.txt").exists());
    }
}
