//! The fixed geometry of the table.
//!
//! Cells live on a pixel grid rather than the rem scale: the grid is a data
//! canvas, like the chart. Fixed cell sizes keep thousands of cells aligned
//! pixel-perfect at any scale and make the virtualization arithmetic exact, so
//! the visible-range calculation never drops or duplicates a row.

use gpui_kit::Pixels;

/// Width of one cell, in pixels.
pub const CELL_WIDTH: f32 = 56.0;
/// Height of one cell, in pixels.
pub const CELL_HEIGHT: f32 = 26.0;
/// Width of the row-header column.
pub const AXIS_WIDTH: f32 = 56.0;
/// Height of the column-header band.
pub const AXIS_HEIGHT: f32 = 26.0;
/// Width of the vertical Y-axis caption band, left of the row headers.
pub const AXIS_LABEL_WIDTH: f32 = 20.0;
/// Height of the horizontal X-axis caption band, outside the column headers.
pub const AXIS_LABEL_HEIGHT: f32 = 18.0;
/// Space between a caption and the grid box it names.
pub const AXIS_LABEL_GAP: f32 = 8.0;
/// The grid line between cells, drawn as a gap rather than per-cell borders.
pub const CELL_GAP: f32 = 1.0;

/// The range of cells that can be seen, with one row/column of overscan.
///
/// Only visible cells are turned into elements. A 32×32 table is wider than the
/// editor, and a 128×128 one would otherwise build sixteen thousand cells.
pub(crate) fn visible_range(
    offset: Pixels,
    viewport: Pixels,
    step: f32,
    count: usize,
) -> (usize, usize) {
    if count == 0 {
        return (0, 0);
    }
    let viewport = f32::from(viewport);
    if viewport <= 0.0 {
        return (0, count);
    }

    let offset = f32::from(offset);
    let first = ((-offset / step).floor().max(0.0) as usize).saturating_sub(1);
    let last = (((-offset + viewport) / step).ceil().max(0.0) as usize + 1).min(count);
    (first.min(count), last.max(first).min(count))
}
