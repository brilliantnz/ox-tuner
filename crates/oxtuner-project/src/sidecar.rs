//! The `.ox-tuner/` sidecar: state that is ours, never TunerStudio's.
//!
//! ADR-0007 allows exactly one place inside a project for ox-tuner's own
//! files: the `.ox-tuner/` directory. So far that is provenance — which INI a
//! tune was opened with (relative path, SHA-256 and firmware signature) — so a
//! later session can tell "the definition changed" from "the tune changed".
//!
//! Snapshots and UI state will live here too; they are deliberately separate
//! files so each can be added or removed without touching the others.
//!
//! The format is line-oriented UTF-8 with tab-separated `key<TAB>value` and
//! three escapes (`\\`, `\t`, `\n`) so paths and signatures cannot break the
//! structure. It is intentionally not JSON: the crate is dependency-free, and
//! a malformed sidecar must never take the project down with it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Directory name inside a project.
pub const SIDECAR_DIR: &str = ".ox-tuner";
/// Provenance file inside the sidecar directory.
pub const PROVENANCE_FILE: &str = "provenance";

const FORMAT: &str = "1";

/// What was opened, and which definition it was interpreted through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Definition path relative to the project root, with `/` separators.
    pub definition: String,
    /// SHA-256 of the definition file's bytes, as lowercase hex.
    pub definition_sha256: String,
    /// Firmware signature the definition declares.
    pub signature: String,
}

impl Provenance {
    /// Builds provenance for one open.
    pub fn new(definition: impl Into<String>, bytes: &[u8], signature: impl Into<String>) -> Self {
        Self {
            definition: definition.into(),
            definition_sha256: crate::hash::hex(&crate::hash::sha256(bytes)),
            signature: signature.into(),
        }
    }

    /// Writes the provenance file, creating `.ox-tuner/` when needed.
    pub fn write(&self, project_root: &Path) -> io::Result<PathBuf> {
        let directory = project_root.join(SIDECAR_DIR);
        fs::create_dir_all(&directory)?;
        let path = directory.join(PROVENANCE_FILE);
        let text = format!(
            "format\t{FORMAT}\ndefinition\t{}\nsha256\t{}\nsignature\t{}\n",
            escape(&self.definition),
            escape(&self.definition_sha256),
            escape(&self.signature),
        );
        fs::write(&path, text)?;
        Ok(path)
    }

    /// Reads the provenance file, or `None` when the sidecar has never been written.
    pub fn read(project_root: &Path) -> io::Result<Option<Self>> {
        let path = project_root.join(SIDECAR_DIR).join(PROVENANCE_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };

        let mut definition = None;
        let mut sha256 = None;
        let mut signature = None;
        let mut format_seen = false;
        for line in text.lines() {
            let (key, value) = line.split_once('\t').unwrap_or((line, ""));
            match key {
                "format" => format_seen = value == FORMAT,
                "definition" => definition = Some(unescape(value)),
                "sha256" => sha256 = Some(unescape(value)),
                "signature" => signature = Some(unescape(value)),
                _ => {}
            }
        }

        if !format_seen {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the provenance file has an unknown format",
            ));
        }
        match (definition, sha256, signature) {
            (Some(definition), Some(definition_sha256), Some(signature)) => Ok(Some(Self {
                definition,
                definition_sha256,
                signature,
            })),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the provenance file is incomplete",
            )),
        }
    }

    /// Whether `bytes` are the definition this provenance was written for.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        self.definition_sha256 == crate::hash::hex(&crate::hash::sha256(bytes))
    }
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(character),
        }
    }
    out
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "oxtuner-sidecar-{name}-{}-{}",
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
    fn provenance_round_trips_and_verifies_the_bytes() {
        let root = temp_project("roundtrip");
        let bytes = b"[MegaTune]\nsignature = \"speeduino 202501\"\n";
        let provenance =
            Provenance::new("projectCfg/mainController.ini", bytes, "speeduino 202501");

        provenance.write(&root).unwrap();
        let read = Provenance::read(&root).unwrap().expect("written");

        assert_eq!(read, provenance);
        assert!(read.matches(bytes));
        assert!(!read.matches(b"changed"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_sidecar_is_not_an_error() {
        let root = temp_project("missing");
        assert_eq!(Provenance::read(&root).unwrap(), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tabs_and_newlines_cannot_break_the_structure() {
        let escaped = escape("a\tb\nc\\d");
        assert_eq!(escaped, "a\\tb\\nc\\\\d");
        assert_eq!(unescape(&escaped), "a\tb\nc\\d");
    }

    #[test]
    fn an_incomplete_file_is_an_error_not_a_guess() {
        let root = temp_project("incomplete");
        fs::create_dir_all(root.join(SIDECAR_DIR)).unwrap();
        fs::write(root.join(SIDECAR_DIR).join(PROVENANCE_FILE), "format\t1\n").unwrap();

        let error = Provenance::read(&root).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(&root).unwrap();
    }
}
