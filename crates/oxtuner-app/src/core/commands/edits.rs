//! Table and dialog read/edit commands.

use gpui_kit::App;
use oxtuner_core::{AxisEdit, AxisKind, CellEdit, FailureCode};

use super::send;

/// Asks the core for one table snapshot.
pub(crate) fn read_table(name: &str, cx: &mut App) {
    send(cx, FailureCode::TableUnavailable, "read-table", |handle| {
        handle.read_table(name)
    });
}

/// Asks the core to apply a dialog scalar edit.
pub(crate) fn edit_scalar(dialog: &str, name: &str, value: f64, cx: &mut App) {
    send(
        cx,
        FailureCode::DialogEditRejected,
        "edit-scalar",
        |handle| handle.edit_scalar(dialog, name, value),
    );
}

/// Asks the core for one dialog snapshot.
pub(crate) fn read_dialog(name: &str, cx: &mut App) {
    send(
        cx,
        FailureCode::DialogUnavailable,
        "read-dialog",
        |handle| handle.read_dialog(name),
    );
}

/// Asks the core to apply table cell edits.
pub(crate) fn edit_table(name: &str, edits: Vec<CellEdit>, cx: &mut App) {
    send(
        cx,
        FailureCode::TableEditRejected,
        "edit-table",
        move |handle| handle.edit_table(name, edits),
    );
}

/// Asks the core to apply table axis bin edits.
pub(crate) fn edit_axis(table: &str, axis: AxisKind, edits: Vec<AxisEdit>, cx: &mut App) {
    send(
        cx,
        FailureCode::TableEditRejected,
        "edit-axis",
        move |handle| handle.edit_axis(table, axis, edits),
    );
}
