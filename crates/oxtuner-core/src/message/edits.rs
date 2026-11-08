//! Outcomes of tune mutations: writes, burns, saves and reverts.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What a successful [`super::Command::SaveTune`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSummary {
    /// The tune file that was rewritten.
    pub path: PathBuf,
    /// Number of values that changed and were written.
    pub cells: usize,
}

/// What a verified RAM write did.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteSummary {
    /// Pages written.
    pub pages: usize,
    /// Cells whose values changed and were sent.
    pub cells: usize,
    /// Variables those cells belong to.
    pub variables: usize,
    /// Whether the read-back matched the bytes written.
    pub verified: bool,
    /// The snapshot taken before the write, when one was needed.
    pub snapshot: Option<PathBuf>,
    /// The journal file the entries went to.
    pub journal: Option<PathBuf>,
}

/// What a verified burn did.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnSummary {
    /// Pages burned.
    pub pages: usize,
    /// Whether the read-back matched RAM after the burn.
    pub verified: bool,
    /// The snapshot taken before the burn.
    pub snapshot: Option<PathBuf>,
    /// The journal file the entries went to.
    pub journal: Option<PathBuf>,
}

/// What reverting to the file baseline did.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertSummary {
    /// Cells that were restored.
    pub cells: usize,
    /// The snapshot taken before the revert.
    pub snapshot: Option<PathBuf>,
}

/// What importing another tune did.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// The file that was imported.
    pub path: PathBuf,
    /// Cells whose working values changed.
    pub cells: usize,
    /// Variables those cells belong to.
    pub variables: usize,
    /// Whether the import was approved despite a signature difference (S9).
    pub consented: bool,
    /// The snapshot taken before the import.
    pub snapshot: Option<PathBuf>,
}

/// One line in the append-only write journal.
///
/// A write or burn writes a `begin` entry before touching the ECU and a `result`
/// entry when it finishes, so a crash leaves evidence of the attempt. Entries are
/// JSON lines in `.ox-tuner/write-journal.log`, capped by rotation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// UTC time, RFC 3339.
    pub at: String,
    /// `write`, `burn`, `revert`, `import` or `restore`.
    pub operation: String,
    /// `begin` or `result`.
    pub phase: String,
    /// The link the operation ran on (`simulator`, a device path).
    pub port: String,
    /// The ECU signature the session saw.
    pub signature: String,
    /// Page numbers, comma separated, or `all`.
    pub pages: String,
    /// Cells touched.
    pub cells: usize,
    /// Variables touched.
    pub variables: usize,
    /// `ok`, `failed`, `refused` or `pending`.
    pub result: String,
    /// The verification outcome or failure text.
    pub detail: String,
}
