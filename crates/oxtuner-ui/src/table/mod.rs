//! The reusable tune table.
//!
//! A retained [`Table`] entity over the GPUI-free [`TableModel`]: snapshots in,
//! [`TableEvent`]s out. The grid reads its chrome from the application theme;
//! only [`TableStyle`] holds colours of its own, the two the heat ramp
//! constrains (ADR-0013, ADR-0020). The smaller pieces — cells, headers,
//! geometry, the heatmap palette — are internal components the table composes
//! each frame.

mod cell;
mod geometry;
mod headers;
mod heatmap;
mod model;
mod style;
// The entity lives in its own file so this seam stays a table of contents;
// the same name is deliberate, it keeps the public path `table::Table`.
mod table;

#[cfg(test)]
mod tests;

pub use model::{
    AxisEditing, AxisTarget, BulkOp, CellPos, Edge, EditError, EditOp, Editing, Extend, GridAction,
    HistoryStep, MoveDirection, PAGE_ROWS, Selection, TableModel, parse_edit,
};
pub use style::TableStyle;
pub use table::{RealtimePoint, Table, TableEvent};
