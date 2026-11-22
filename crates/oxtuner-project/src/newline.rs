//! Line endings for tune files.
//!
//! Every reader accepts LF, CRLF and a lone CR, so the format does not require
//! one style. A file that moves between the reference application on Windows
//! and Ox Tuner on another host should keep whatever style the last writer
//! chose; otherwise every switch rewrites every line and every diff is the
//! whole file.
//!
//! New files are LF on every host. Properties files are creation-only today
//! and always written LF; when a later save path rewrites them, it uses the
//! same [`Newline`] rule.

/// A file's dominant line ending.
#[expect(
    clippy::exhaustive_enums,
    reason = "a text file has exactly these two line endings in scope"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Newline {
    /// Unix and mobile hosts, and the canonical in-memory form.
    #[default]
    Lf,
    /// Windows hosts and the reference sample.
    Crlf,
}

impl Newline {
    /// The dominant line ending in `bytes`, or `None` when it has no line
    /// break at all.
    ///
    /// A mixed file follows its majority. A tie picks LF, the canonical form.
    /// A lone CR counts as a line break so an old-Mac file is detected rather
    /// than treated as one long line.
    #[must_use]
    pub fn detect(bytes: &[u8]) -> Option<Self> {
        let mut crlf = 0_usize;
        let mut lf = 0_usize;
        let mut index = 0;

        while index < bytes.len() {
            match bytes[index] {
                b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                    crlf += 1;
                    index += 2;
                }
                b'\r' | b'\n' => {
                    lf += 1;
                    index += 1;
                }
                _ => index += 1,
            }
        }

        match crlf.cmp(&lf) {
            std::cmp::Ordering::Greater => Some(Self::Crlf),
            std::cmp::Ordering::Less => Some(Self::Lf),
            std::cmp::Ordering::Equal if crlf > 0 => Some(Self::Lf),
            std::cmp::Ordering::Equal => None,
        }
    }

    /// Rewrites `text` with this line ending, from any input style.
    #[must_use]
    pub fn encode(self, text: &str) -> String {
        let text = normalize(text);
        match self {
            Self::Lf => text,
            Self::Crlf => text.replace('\n', "\r\n"),
        }
    }
}

/// The file style a new project uses.
///
/// LF on every host, so creation output is deterministic and the golden tests
/// do not depend on the machine. Every reader in scope accepts LF, and a later
/// save preserves whatever style the file already carries.
#[must_use]
pub(crate) const fn new_file() -> Newline {
    Newline::Lf
}

/// Normalizes CRLF and lone CR to LF.
#[must_use]
pub(crate) fn normalize(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }

    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_majority_line_ending_wins() {
        assert_eq!(Newline::detect(b"a\r\nb\r\nc\n"), Some(Newline::Crlf));
        assert_eq!(Newline::detect(b"a\nb\nc\r\n"), Some(Newline::Lf));
        assert_eq!(Newline::detect(b"a\rb\r"), Some(Newline::Lf));
        assert_eq!(Newline::detect(b"no break"), None);
    }

    #[test]
    fn encoding_normalizes_every_style() {
        assert_eq!(Newline::Lf.encode("a\r\nb\rc\n"), "a\nb\nc\n");
        assert_eq!(Newline::Crlf.encode("a\r\nb\rc\n"), "a\r\nb\r\nc\r\n");
    }

    #[test]
    fn a_new_file_is_lf_on_every_host() {
        assert_eq!(new_file(), Newline::Lf);
    }
}
