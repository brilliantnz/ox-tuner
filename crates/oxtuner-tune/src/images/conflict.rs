use std::collections::BTreeSet;

use crate::{TuneDiff, TuneLayout, Values};

use super::TuneImages;

/// One cell's value, for a conflict report.
#[expect(
    clippy::exhaustive_enums,
    reason = "a cell value is a user number, a raw number or text"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    /// A numeric value in user units.
    Number(f64),
    /// A numeric value in raw units, because the definition's `scale` or
    /// `translate` is an expression this build could not resolve.
    Raw(f64),
    /// A text value.
    Text(String),
}

/// A cell the ECU and the working image disagree about.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    /// Variable name.
    pub name: String,
    /// Table row (`0` for scalars).
    pub row: usize,
    /// Table column (`0` for scalars).
    pub column: usize,
    /// The value the user is working with.
    pub working: CellValue,
    /// The value read back from the ECU.
    pub ecu: CellValue,
}

pub(super) fn cell_value(spec: &crate::VariableSpec, values: &Values, index: usize) -> CellValue {
    match values {
        Values::Numbers(numbers) => {
            let raw = numbers.get(index).copied().unwrap_or(0.0);
            match spec.to_user(raw) {
                Ok(user) => CellValue::Number(user),
                Err(_) => CellValue::Raw(raw),
            }
        }
        Values::Text(text) => CellValue::Text(text.clone()),
    }
}

/// The `(row, column)` a flat index addresses.
pub(super) fn cell_position(shape: crate::EntryShape, index: usize) -> (usize, usize) {
    match shape {
        crate::EntryShape::Array { cols, .. } if cols > 0 => (index / cols, index % cols),
        _ => (0, 0),
    }
}

/// Names that have any conflict overlay. Kept for callers that only need the
/// set (the save path and the dialog both do).
pub fn conflict_names(images: &TuneImages) -> BTreeSet<String> {
    images
        .conflicts()
        .into_iter()
        .map(|conflict| conflict.name)
        .collect()
}

pub(super) fn diff_writable_values(
    layout: &TuneLayout,
    left: &[Values],
    right: &[Values],
) -> TuneDiff {
    let mut diff = TuneDiff::default();
    for (index, variable) in layout.variables().iter().enumerate() {
        if variable.is_controller_owned()
            || variable.no_msq_save
            || variable.address.is_none()
            || !variable.bit_field_round_trips()
        {
            continue;
        }
        if layout.is_alias_at(index) {
            // Another variable owns these bytes; the view follows them.
            continue;
        }
        let (Some(left), Some(right)) = (left.get(index), right.get(index)) else {
            continue;
        };
        let changed = left.changed_wire_cells(right, variable);
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
