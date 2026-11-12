//! The append-only write journal (ADR-0011).
//!
//! Every write, burn, revert, import and snapshot restore appends a `begin`
//! entry before it touches anything and a `result` entry when it finishes. The
//! file is JSON lines under the project's sidecar, so it can be inspected with
//! `tail` and read back by the app. It rotates at a fixed size: bounded disk use
//! without ever rewriting history inside a file.

use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write as _},
    path::{Path, PathBuf},
};

use crate::contract::JournalEntry;

/// Rotate when the file passes this size.
pub(crate) const MAX_JOURNAL_BYTES: u64 = 512 * 1024;

/// How many entries the app reads back in one dialog.
pub(crate) const JOURNAL_READ_LIMIT: usize = 200;

/// The journal path for a project folder.
pub(crate) fn project_path(root: &Path) -> PathBuf {
    root.join(oxtuner_project::SIDECAR_DIR)
        .join("write-journal.log")
}

/// A journal for one session, or for a standalone definition.
#[derive(Debug, Clone)]
pub(crate) struct Journal {
    path: PathBuf,
}

impl Journal {
    /// Wraps a path. The parent directory is created on first append.
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file this journal writes.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one JSON line, rotating a full file first.
    pub(crate) fn append(&self, entry: &JournalEntry) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.rotate_if_full()?;

        let encoded = serde_json::to_string(entry)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(encoded.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()
    }

    /// The newest `limit` readable entries, oldest first.
    pub(crate) fn read(&self, limit: usize) -> io::Result<Vec<JournalEntry>> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let reader = BufReader::new(file);
        let mut entries: Vec<JournalEntry> = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<JournalEntry>(&line) {
                Ok(entry) => entries.push(entry),
                // A truncated last line from a crash is expected; it must not
                // make the rest of the journal unreadable.
                Err(error) => tracing::debug!(%error, "skipping an unreadable journal line"),
            }
        }
        if entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
        Ok(entries)
    }

    fn rotate_if_full(&self) -> io::Result<()> {
        let Ok(metadata) = fs::metadata(&self.path) else {
            return Ok(());
        };
        if metadata.len() < MAX_JOURNAL_BYTES {
            return Ok(());
        }
        let rotated = self.path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        fs::rename(&self.path, rotated)
    }
}

/// The current UTC time in RFC 3339, or a unix fallback if the clock is before
/// the epoch.
pub(crate) fn timestamp() -> String {
    let now = std::time::SystemTime::now();
    time::OffsetDateTime::from(now)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| {
            now.duration_since(std::time::UNIX_EPOCH).map_or_else(
                |_| "unix:unknown".to_owned(),
                |elapsed| format!("unix:{}", elapsed.as_secs()),
            )
        })
}

/// `1, 2, 5` for a page list, or `all` for an unrestricted operation.
pub(crate) fn pages_label(pages: &[u8]) -> String {
    if pages.is_empty() {
        "all".to_owned()
    } else {
        pages
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn entry(phase: &str) -> JournalEntry {
        JournalEntry {
            at: "2026-10-28T12:00:00Z".to_owned(),
            operation: "write".to_owned(),
            phase: phase.to_owned(),
            port: "simulator".to_owned(),
            signature: "speeduino 202501".to_owned(),
            pages: "1, 2".to_owned(),
            cells: 3,
            variables: 2,
            result: "pending".to_owned(),
            detail: String::new(),
        }
    }

    fn temp_dir() -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!(
            "oxtuner-journal-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("temp dir");
        directory
    }

    #[test]
    fn entries_round_trip_in_order() {
        let directory = temp_dir();
        let journal = Journal::new(directory.join("write-journal.log"));

        journal.append(&entry("begin")).expect("append");
        journal.append(&entry("result")).expect("append");

        let read = journal.read(10).expect("read");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].phase, "begin");
        assert_eq!(read[1].phase, "result");

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_full_journal_rotates_instead_of_growing() {
        let directory = temp_dir();
        let path = directory.join("write-journal.log");
        let journal = Journal::new(&path);

        let full = usize::try_from(MAX_JOURNAL_BYTES).expect("the limit fits a usize");
        fs::write(&path, vec![b'x'; full]).expect("fill the journal");
        journal.append(&entry("begin")).expect("append");

        let rotated = path.with_extension("log.1");
        assert!(rotated.exists(), "a full journal is kept as .log.1");
        assert_eq!(
            fs::metadata(&rotated).expect("metadata").len(),
            MAX_JOURNAL_BYTES
        );

        let read = journal.read(10).expect("read");
        assert_eq!(read.len(), 1, "the live journal holds only the new entry");

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_truncated_last_line_does_not_hide_earlier_entries() {
        let directory = temp_dir();
        let path = directory.join("write-journal.log");
        let journal = Journal::new(&path);
        journal.append(&entry("begin")).expect("append");
        let mut file = OpenOptions::new().append(true).open(&path).expect("open");
        file.write_all(b"{\"at\":").expect("truncated write");

        let read = journal.read(10).expect("read");

        assert_eq!(read.len(), 1);

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn reading_a_missing_journal_is_empty_not_an_error() {
        let directory = temp_dir();
        let journal = Journal::new(directory.join("none.log"));

        assert!(
            journal.read(10).expect("read").is_empty(),
            "a missing journal must read as empty"
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn page_labels_read_like_a_person_wrote_them() {
        assert_eq!(pages_label(&[]), "all");
        assert_eq!(pages_label(&[1, 2, 3]), "1, 2, 3");
    }

    #[test]
    fn the_timestamp_is_rfc3339_or_a_labelled_fallback() {
        let timestamp = timestamp();
        assert!(timestamp.contains('T') || timestamp.starts_with("unix:"));
    }
}
