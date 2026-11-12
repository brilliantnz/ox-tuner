use std::collections::BTreeSet;

use super::{Number, Ordered};

/// A section this build keeps as raw data.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedSection {
    /// Section name as written.
    pub name: String,
    /// Logical lines, in order, comments already removed.
    pub lines: Vec<String>,
}

impl UnsupportedSection {
    /// Renders the section back as INI text.
    ///
    /// Capturing an unknown section is only useful if it can be handed back, so
    /// the round-trip is part of the contract: `[Name]` then the logical lines.
    /// This is **not** byte-exact: preprocessing removed comments and joined
    /// continuations before the lines got here, and unknown keys inside
    /// *modelled* sections are not captured here at all (they are reported once
    /// and preserved in the section's `raw` map where one exists).
    pub fn render(&self) -> String {
        let mut out = format!("[{}]\n", self.name);
        for line in &self.lines {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Per-section coverage of the definition.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct SectionCoverage {
    /// Section name.
    pub name: String,
    /// Entries seen.
    pub entries: usize,
    /// Entries turned into model data.
    pub understood: usize,
    /// Entries kept as raw data or dropped.
    pub unsupported: usize,
}

/// Percentage of entries understood, 100 when there are none.
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "entry counts are far below 2^53, so f64 represents them exactly"
)]
fn coverage_percent(understood: usize, entries: usize) -> f64 {
    if entries == 0 {
        return 100.0;
    }
    (understood as f64 / entries as f64) * 100.0
}

impl SectionCoverage {
    /// Percentage of entries understood, 100 when the section is empty.
    pub fn percent(&self) -> f64 {
        coverage_percent(self.understood, self.entries)
    }
}

/// Coverage for the whole file.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Coverage {
    /// One row per section, in document order.
    pub sections: Vec<SectionCoverage>,
}

impl Coverage {
    /// Percentage of all entries understood.
    pub fn percent(&self) -> f64 {
        let entries: usize = self.sections.iter().map(|section| section.entries).sum();
        let understood: usize = self.sections.iter().map(|section| section.understood).sum();
        coverage_percent(understood, entries)
    }

    /// The coverage row for one section.
    pub fn section(&self, name: &str) -> Option<&SectionCoverage> {
        self.sections
            .iter()
            .find(|section| section.name.eq_ignore_ascii_case(name))
    }
}

/// `[ConstantsExtensions]` / `[ConstantExtensions]` metadata.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Extensions {
    /// `defaultValue` entries.
    pub defaults: Ordered<Number>,
    /// `rawValue` entries (defaults in unscaled units).
    pub raw_defaults: Ordered<Number>,
    /// `maximumElements` caps for resizable arrays.
    pub maximum_elements: Ordered<u32>,
    /// Constants the controller owns; never edited locally.
    pub read_only: BTreeSet<String>,
    /// Constants that require a power cycle to take effect.
    pub power_cycle: BTreeSet<String>,
    /// Constants overwritten from the ECU on every connect.
    pub controller_priority: BTreeSet<String>,
    /// Arrays indexed low→high reversed.
    pub reverse_index: BTreeSet<String>,
    /// Constants with a per-entry byte-order flip.
    pub opposite_endian: BTreeSet<String>,
    /// Non-linear scaling tables.
    pub mapping_tables: Ordered<String>,
    /// `maintainConstantValue` rules, as written.
    pub maintain: Vec<(String, String)>,
    /// Whether missing bit options are filled with `INVALID` silently.
    pub ignore_missing_bit_options: bool,
    /// Whether boilerplate is stripped from configuration-error text.
    pub suppress_config_error_verbiage: bool,
}

/// Whether one of the name sets holds `name`.
///
/// The dialect writes names as declared, but constants are matched
/// case-insensitively elsewhere, so these lookups are too.
///
/// The largest of these sets in the reference definitions holds 114 names
/// (`requiresPowerCycle` in the 202501 Speeduino file); the others are single
/// digits. The scan avoids lowercasing a copy of every set on each parse and
/// keeps one helper for the three lookups. If a set ever grows past a few
/// hundred names, lowercase the sets once per parse and use
/// `BTreeSet::contains`.
fn set_contains(set: &BTreeSet<String>, name: &str) -> bool {
    set.iter().any(|entry| entry.eq_ignore_ascii_case(name))
}

impl Extensions {
    /// Whether a constant is controller-owned.
    #[must_use]
    pub fn is_read_only(&self, name: &str) -> bool {
        set_contains(&self.read_only, name)
    }

    /// Whether the controller overwrites `name` on every connect.
    #[must_use]
    pub fn is_controller_priority(&self, name: &str) -> bool {
        set_contains(&self.controller_priority, name)
    }

    /// Whether a change to `name` needs the ECU power-cycled before it takes
    /// effect.
    #[must_use]
    pub fn requires_power_cycle(&self, name: &str) -> bool {
        set_contains(&self.power_cycle, name)
    }
}

#[cfg(test)]
mod tests {
    use crate::settings::SettingsContext;

    #[test]
    fn read_only_matches_a_constant_declared_in_another_case() {
        let text = "\
[MegaTune]
signature = \"x\"

[Constants]
nPages = 1
pageSize = 288
page = 1
engineMake = scalar, U08, 0, \"\", 1.0, 0
boostTarget = scalar, U08, 1, \"\", 1.0, 0

[ConstantsExtensions]
readOnly = enginemake
controllerPriority = boosttarget
";

        let definition = crate::parse_str(text, &SettingsContext::new()).expect("parses");

        assert!(
            definition
                .constant("engineMake")
                .is_some_and(|constant| constant.flags.controller_priority),
            "the extension names the constant in another case"
        );
        assert!(
            definition
                .constant("boostTarget")
                .is_some_and(|constant| constant.flags.controller_priority),
            "controllerPriority folds case too"
        );
        assert!(definition.extensions.is_read_only("ENGINEMAKE"));
        assert!(definition.extensions.is_controller_priority("BOOSTTARGET"));
        assert!(!definition.extensions.requires_power_cycle("BOOSTTARGET"));
    }
}
