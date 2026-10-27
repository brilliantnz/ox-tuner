//! The tune table grid: model, view and their tests.
//!
//! The view lives in the app until the workspace can add `oxtuner-ui`: a new
//! `gpui-kit`-dependent member currently forces a Cargo re-resolution that fails
//! on the kit's optional `gpui-fast` pins. The module has no shell dependencies,
//! so the move is mechanical once that clears (ADR-0001, ADR-0013).

mod grid;
mod model;

pub(crate) use grid::{TableGrid, TableGridEvent};
pub(crate) use model::{BulkOp, EditOp, GridAction, parse_edit};

#[cfg(test)]
mod tests;
