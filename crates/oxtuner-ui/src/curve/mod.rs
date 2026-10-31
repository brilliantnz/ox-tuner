//! The reusable curve editor.
//!
//! The curve draws a tune's one-row table as a line and lets the user drag a
//! point to change its value. It is the same data the table grid edits, so a
//! page can show the plot above its table and keep both in step. The view
//! lives in [`Curve`], the state in [`CurveModel`], and the snapshot contract
//! in [`CurveSnapshot`].

// The view lives in its own file so this seam stays a table of contents. The
// same name is deliberate, it keeps the public path `curve::Curve`.
#[allow(clippy::module_inception)]
mod curve;
mod model;

#[cfg(test)]
mod tests;

pub use curve::{Curve, CurveEvent};
pub use model::{CurveModel, CurvePoint, CurveSnapshot};
