//! The fixed axis frame a curve draws against.

/// The fixed axis frame of a curve, from its definition.
///
/// A curve's plot is drawn against these limits rather than against the
/// values, so a point near the top of the range stays near the top while it
/// is dragged. The frame is also what tells the UI that a one-row table is a
/// curve rather than a table with a single row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveAxes {
    /// Left edge of the X axis.
    pub x_min: f64,
    /// Right edge of the X axis.
    pub x_max: f64,
    /// Bottom edge of the Y axis.
    pub y_min: f64,
    /// Top edge of the Y axis.
    pub y_max: f64,
    /// Intervals the Y axis aims for.
    pub divisions: usize,
}
