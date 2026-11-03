//! Diagnostics and the error type for INI parsing.
//!
//! Two kinds of problem, deliberately separated:
//!
//! - **Structural** failures mean the text is not a usable definition (no
//!   sections, unbalanced `#if`, a spec version this build cannot read). These
//!   stop parsing and come back as [`IniError`].
//! - **Per-entry** problems (a bad scale, an unknown type, a duplicate name) are
//!   recorded as a [`Diagnostic`] on the frozen [`crate::Definition`] and the
//!   entry is dropped or kept as data. Nothing panics, and one bad line never
//!   costs the user the whole file.

use std::borrow::Cow;
use std::fmt;

/// How serious a diagnostic is.
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
)]
pub enum Severity {
    /// Informational: the file was read differently than written.
    Note,
    /// The entry was understood but something was adjusted or ignored.
    Warning,
    /// The entry could not be used and was dropped.
    Error,
}

impl Severity {
    /// Short lowercase name, used in logs and test output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One structured message about one place in the source.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How serious it is.
    pub severity: Severity,
    /// Stable machine-readable code (`"unknown-type"`, `"duplicate-name"`, …).
    pub code: Cow<'static, str>,
    /// What happened, in the user's terms.
    pub message: String,
    /// 1-based line in the source file, when the problem has one.
    pub line: Option<u32>,
    /// Section the entry was in (`Constants`, `TableEditor`, …), when known.
    pub section: Option<String>,
}

impl Diagnostic {
    /// A message tied to a line inside a section.
    pub fn new(
        severity: Severity,
        code: &'static str,
        message: impl Into<String>,
        line: u32,
        section: &str,
    ) -> Self {
        Self {
            severity,
            code: code.into(),
            message: message.into(),
            line: Some(line),
            section: Some(section.to_owned()),
        }
    }

    /// A message about the file as a whole.
    pub fn whole_file(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code: code.into(),
            message: message.into(),
            line: None,
            section: None,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(formatter, "{line}: {}: ", self.severity),
            None => write!(formatter, "{}: ", self.severity),
        }?;
        if let Some(section) = &self.section {
            write!(formatter, "[{section}] ")?;
        }
        write!(formatter, "{} ({})", self.message, self.code)
    }
}

/// A definition that could not be parsed at all.
///
/// Carries every diagnostic gathered before giving up, so the caller can show
/// all of them at once rather than one per attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniError {
    diagnostics: Vec<Diagnostic>,
}

impl IniError {
    /// Wraps the diagnostics that stopped the parse.
    pub fn new(diagnostics: Vec<Diagnostic>) -> Self {
        Self { diagnostics }
    }

    /// Convenience for a single structural problem.
    pub fn structural(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(vec![Diagnostic::whole_file(Severity::Error, code, message)])
    }

    /// The diagnostics that stopped the parse, in source order.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl fmt::Display for IniError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("could not parse the INI definition")?;
        for diagnostic in &self.diagnostics {
            write!(formatter, "\n  {diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for IniError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_located_diagnostic_names_the_line_and_section() {
        let diagnostic = Diagnostic::new(
            Severity::Error,
            "unknown-type",
            "unknown data type Q99",
            42,
            "Constants",
        );

        assert_eq!(
            diagnostic.to_string(),
            "42: error: [Constants] unknown data type Q99 (unknown-type)"
        );
    }

    #[test]
    fn a_whole_file_diagnostic_has_no_location() {
        let diagnostic = Diagnostic::whole_file(Severity::Error, "no-sections", "not an ini");

        assert_eq!(diagnostic.line, None);
        assert!(diagnostic.to_string().starts_with("error: not an ini"));
    }
}
