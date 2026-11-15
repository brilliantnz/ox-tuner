//! ECU families and the signatures that name them.

use crate::error::{ErrorKind, RegistryError};

/// The firmware family a signature names.
///
/// `Unknown` is the closed catch-all: anything the shipped sources do not
/// claim. It never gains a download URL; it keeps the manual-upload path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Family {
    /// Speeduino, the first target.
    Speeduino,
    /// FOME, which reports itself inside a rusEFI signature.
    Fome,
    /// rusEFI.
    Rusefi,
    /// Anything else. No downloads, manual upload only.
    Unknown,
}

impl Family {
    /// The stable lowercase key for logs, the journal and cache directories.
    ///
    /// The strings match the behavior ids in `oxtuner-protocol`, so both
    /// crates file one family under one name.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Speeduino => "speeduino",
            Self::Fome => "fome",
            Self::Rusefi => "rusefi",
            Self::Unknown => "unknown",
        }
    }

    /// The display label. Family names are proper nouns, never translated.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Speeduino => "Speeduino",
            Self::Fome => "FOME",
            Self::Rusefi => "rusEFI",
            Self::Unknown => "Unknown",
        }
    }
}

/// Which family `signature` names.
///
/// The order mirrors the protocol's select: Speeduino first so a board that
/// keeps a `speeduino` signature stays Speeduino, FOME before rusEFI because
/// its signature starts with `rusEFI` as well.
pub fn family_of(signature: &str) -> Family {
    let normalized = signature.trim().to_ascii_lowercase();

    if normalized.starts_with("speeduino") {
        Family::Speeduino
    } else if normalized.contains("fome") {
        Family::Fome
    } else if normalized.starts_with("rusefi") {
        Family::Rusefi
    } else {
        Family::Unknown
    }
}

/// One exact firmware build, as its signature describes it.
///
/// The parts are private so the id cannot disagree with the signature it came
/// from: `version` and `raw` must always describe the same build, and every
/// cache key derives from both.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FirmwareId {
    family: Family,
    version: String,
    board: Option<String>,
    hash: Option<String>,
    raw: String,
}

impl FirmwareId {
    /// The family the signature names.
    pub fn family(&self) -> Family {
        self.family
    }

    /// The version part: `202501.7`, or the rusEFI dotted tail.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The rusEFI short board name, when the signature carries one.
    pub fn board(&self) -> Option<&str> {
        self.board.as_deref()
    }

    /// The rusEFI configuration hash, when the signature carries one.
    pub fn hash(&self) -> Option<&str> {
        self.hash.as_deref()
    }

    /// The signature exactly as the ECU reported it.
    pub fn raw(&self) -> &str {
        &self.raw
    }
}

/// Splits `signature` into its family and version parts.
///
/// # Errors
///
/// Returns [`ErrorKind::InvalidSignature`] when the text names no known
/// family or carries no version.
///
/// [`ErrorKind::InvalidSignature`]: crate::error::ErrorKind::InvalidSignature
pub fn parse_signature(signature: &str) -> Result<FirmwareId, RegistryError> {
    let raw = signature.trim().to_owned();
    let family = family_of(&raw);

    if family == Family::Unknown {
        return Err(RegistryError::new(
            ErrorKind::InvalidSignature,
            "the signature names no known firmware family",
        ));
    }

    let tail = raw
        .split_once([' ', '\t'])
        .map(|(_, tail)| tail.trim())
        .unwrap_or_default();

    if tail.is_empty() {
        return Err(RegistryError::new(
            ErrorKind::InvalidSignature,
            "the signature carries no version",
        ));
    }

    match family {
        Family::Rusefi => parse_rusefi_tail(&raw, tail),
        Family::Fome => parse_fome_tail(&raw, tail),
        Family::Speeduino => {
            // A path segment, not a sentence: the version addresses one file
            // under `/fw/`, so anything outside the file-name vocabulary is a
            // signature this crate cannot resolve.
            let valid = tail.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-' || byte == b'_'
            });

            if !valid {
                return Err(RegistryError::new(
                    ErrorKind::InvalidSignature,
                    "the Speeduino version is not a file-name token",
                ));
            }

            Ok(FirmwareId {
                family,
                version: tail.to_owned(),
                board: None,
                hash: None,
                raw,
            })
        }
        Family::Unknown => Err(RegistryError::new(
            ErrorKind::InvalidSignature,
            "the signature names no known firmware family",
        )),
    }
}

/// Splits a rusEFI tail shaped `branch.YYYY.MM.DD.board.hash`.
///
/// The shape is exactly six dot-separated parts: the upstream tooling allows
/// letters, digits, `-` and `_` in the free-form parts and zero-padded digits
/// in the date parts, so anything else cannot address a published file.
fn parse_rusefi_tail(raw: &str, tail: &str) -> Result<FirmwareId, RegistryError> {
    let invalid = || {
        RegistryError::new(
            ErrorKind::InvalidSignature,
            "the rusEFI signature is not branch.date.board.hash",
        )
    };

    let parts: Vec<&str> = tail.split('.').collect();
    if parts.len() != 6 {
        return Err(invalid());
    }

    let [branch, year, month, day, board, hash] =
        [parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]];

    let token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    };
    let dated = |part: &str, width: usize| {
        part.len() == width && part.bytes().all(|byte| byte.is_ascii_digit())
    };

    if !token(branch) || !token(board) || !token(hash) {
        return Err(invalid());
    }

    if !dated(year, 4) || !dated(month, 2) || !dated(day, 2) {
        return Err(invalid());
    }

    Ok(FirmwareId {
        family: Family::Rusefi,
        version: tail.to_owned(),
        board: Some(board.to_owned()),
        hash: Some(hash.to_owned()),
        raw: raw.to_owned(),
    })
}

/// Splits a FOME tail, which is rusEFI-shaped with a `(FOME)` marker.
///
/// The marker is dropped and the rest is kept verbatim as the version: FOME
/// definitions are per-board generated files whose download mapping is still
/// open, so the registry carries the text without pretending to resolve it.
fn parse_fome_tail(raw: &str, tail: &str) -> Result<FirmwareId, RegistryError> {
    let version = tail
        .split_whitespace()
        .filter(|word| !word.eq_ignore_ascii_case("(fome)"))
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['.', ' '])
        .to_owned();

    if version.is_empty() {
        return Err(RegistryError::new(
            ErrorKind::InvalidSignature,
            "the FOME signature carries no version",
        ));
    }

    Ok(FirmwareId {
        family: Family::Fome,
        version,
        board: None,
        hash: None,
        raw: raw.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_are_found_in_select_order() {
        assert_eq!(family_of("speeduino 202501.7"), Family::Speeduino);
        assert_eq!(family_of("  Speeduino 202501.7 "), Family::Speeduino);
        assert_eq!(family_of("speeduino fome 202401"), Family::Speeduino);
        assert_eq!(
            family_of("rusEFI master.2024.02.02.alphax-4chan.1293678056"),
            Family::Rusefi
        );
        assert_eq!(
            family_of("rusEFI (FOME) .2026.06.03.proteus_h7.3416487136"),
            Family::Fome
        );
        assert_eq!(family_of("something else"), Family::Unknown);
    }

    #[test]
    fn the_ids_are_stable_lowercase_keys() {
        for family in [
            Family::Speeduino,
            Family::Fome,
            Family::Rusefi,
            Family::Unknown,
        ] {
            let id = family.id();

            assert!(
                id.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            );
        }
    }

    #[test]
    fn a_speeduino_signature_parses_to_its_version() {
        let id = parse_signature("speeduino 202501.7").expect("parses");

        assert_eq!(id.family(), Family::Speeduino);
        assert_eq!(id.version(), "202501.7");
        assert_eq!(id.board(), None);
    }

    #[test]
    fn a_rusefi_signature_parses_to_board_and_hash() {
        let id =
            parse_signature("rusEFI master.2024.02.02.alphax-4chan.1293678056").expect("parses");

        assert_eq!(id.family(), Family::Rusefi);
        assert_eq!(id.board(), Some("alphax-4chan"));
        assert_eq!(id.hash(), Some("1293678056"));
    }

    #[test]
    fn broken_signatures_are_refused() {
        for signature in [
            "",
            "   ",
            "speeduino",
            "speeduino   ",
            "rusEFI master.2024.02",
            "rusEFI master.2024.02.02..1293678056",
            "rusEFI master.2024.2.2.board.1",
            "rusEFI master.2024.02.02.extra.board.1",
            "rusEFI master.2024.02.02.bo ard.1",
            "speeduino 202501.7 extra",
            "speeduino 2025/01",
            "something else entirely",
        ] {
            let error = parse_signature(signature).expect_err("must be refused");

            assert_eq!(
                error.kind(),
                ErrorKind::InvalidSignature,
                "for {signature:?}"
            );
        }
    }

    #[test]
    fn arbitrary_text_never_panics_and_a_hit_agrees_with_its_family() {
        proptest::proptest!(|(text in "\\PC*")| {
            let family = family_of(&text);

            if let Ok(id) = parse_signature(&text) {
                assert_eq!(id.family(), family);
                assert_eq!(id.raw(), text.trim());
            }
        });
    }
}
