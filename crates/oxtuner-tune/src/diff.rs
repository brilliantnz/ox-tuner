//! Change summaries between two images.
//!
//! The write-state UI needs counts, not the values themselves: "12 cells not
//! sent", "3 cells in RAM not burned". A [`TuneDiff`] is that summary — the
//! names that changed, how many cells, and how the changes distribute over
//! pages. The project layer also uses the name list to rewrite only the
//! constants a save actually changed, so an untouched `.msq` stays byte-exact
//! (P10 finding 1).

use std::collections::BTreeMap;
use std::fmt;

use crate::{IniPage, Tune, TuneError, TuneLayout, Values};

/// The difference between two tunes built from the same layout.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuneDiff {
    /// Variables with at least one changed cell, in layout order.
    pub changed_variables: Vec<String>,
    /// Number of changed cells (a text value counts as one).
    pub cells: usize,
    /// Changed cells per INI page.
    pub pages: BTreeMap<IniPage, usize>,
}

impl TuneDiff {
    /// Compares `baseline` with `other`.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the tunes use different
    /// layouts.
    pub fn between(baseline: &Tune, other: &Tune) -> Result<Self, TuneError> {
        if !baseline.same_layout(other) {
            return Err(TuneError::LayoutMismatch);
        }
        Ok(diff_values(
            baseline.layout(),
            baseline.values(),
            other.values(),
        ))
    }

    /// Whether the two tunes hold the same values.
    pub fn is_empty(&self) -> bool {
        self.cells == 0
    }

    /// Number of variables with changes.
    pub fn variables(&self) -> usize {
        self.changed_variables.len()
    }
}

impl fmt::Display for TuneDiff {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.cells {
            0 => formatter.write_str("no changes"),
            1 => write!(formatter, "1 cell"),
            cells => write!(formatter, "{cells} cells"),
        }
    }
}

/// Summarises two value slices that are known to share `layout`.
///
/// `TuneImages` enforces the shared layout when it is built, so its status
/// computation uses this directly instead of re-validating on every frame.
pub(crate) fn diff_values(layout: &TuneLayout, baseline: &[Values], other: &[Values]) -> TuneDiff {
    let mut diff = TuneDiff::default();

    for (index, variable) in layout.variables().iter().enumerate() {
        if layout.is_alias_at(index) {
            // A view follows its owner and is never written to a file.
            continue;
        }
        let (Some(left), Some(right)) = (baseline.get(index), other.get(index)) else {
            continue;
        };
        let changed = match (left, right) {
            // A cell that goes on the wire is compared as the wire stores it;
            // a PC-side value and text compare exactly.
            (Values::Numbers(_), Values::Numbers(_)) if variable.ini_page().is_some() => {
                left.changed_wire_cells(right, variable)
            }
            _ => left.changed_cells(right),
        };
        if changed == 0 {
            continue;
        }

        diff.changed_variables.push(variable.name.clone());
        diff.cells += changed;
        if let Some(page) = variable.ini_page() {
            *diff.pages.entry(page).or_default() += changed;
        }
    }

    diff
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;

    fn tune() -> Tune {
        Tune::from_definition(
            &parse_str(
                r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 288
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 255, 0
table = array, U08, 1, [2x2], "", 1, 0, 0, 255, 0
"#,
                &SettingsContext::new(),
            )
            .expect("parses"),
        )
    }

    #[test]
    fn no_edits_means_an_empty_diff() {
        let tune = tune();
        let diff = TuneDiff::between(&tune, &tune.snapshot()).unwrap();
        assert!(diff.is_empty());
        assert_eq!(diff.to_string(), "no changes");
    }

    #[test]
    fn cells_and_pages_are_counted() {
        let baseline = tune();
        let mut working = baseline.snapshot();

        working.set_user("a", 2.0).unwrap();
        working.set_cell_user("table", 0, 1, 5.0).unwrap();
        working.set_cell_user("table", 1, 0, 6.0).unwrap();

        let diff = TuneDiff::between(&baseline, &working).unwrap();

        assert_eq!(diff.cells, 3);
        assert_eq!(diff.changed_variables, ["a", "table"]);
        assert_eq!(diff.pages.get(&IniPage::FIRST), Some(&3));
        assert_eq!(diff.to_string(), "3 cells");
    }

    #[test]
    fn tunes_from_different_layouts_are_not_compared() {
        let other = Tune::from_definition(
            &parse_str(
                "[MegaTune]\nsignature = \"other\"\n[Constants]\nnPages = 1\npageSize = 1\npage = 1\nb = scalar, U08, 0, \"\", 1, 0, 0, 1, 0\n",
                &SettingsContext::new(),
            )
            .expect("parses"),
        );

        assert_eq!(
            TuneDiff::between(&tune(), &other),
            Err(TuneError::LayoutMismatch)
        );
    }
}
