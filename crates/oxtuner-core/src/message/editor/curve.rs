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

/// Smallest division count the frame draws.
const MIN_DIVISIONS: f64 = 2.0;

/// Largest division count the frame draws.
const MAX_DIVISIONS: f64 = 12.0;

/// Divisions when the definition states no usable count.
const FALLBACK_DIVISIONS: usize = 4;

/// Range below which two axis limits count as the same value.
const DEGENERATE_RANGE: f64 = 1e-12;

/// Padding around the values, as a share of their size, when the definition
/// states no range and every value is the same.
const HUG_PADDING: f64 = 0.05;

/// Frame when there is no range and no finite value to hug.
const EMPTY_FRAME: (f64, f64) = (0.0, 1.0);

/// A declared division count converted to the axis frame's integer count.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "the value is rounded and clamped to 2..=12 before the cast"
)]
fn division_count(value: f64) -> usize {
    value.round().clamp(MIN_DIVISIONS, MAX_DIVISIONS) as usize
}

/// The axis frame a curve draws against.
///
/// The definition's `xAxis` and `yAxis` are literals in every definition seen
/// so far. A definition that leaves them out, or writes an expression, still
/// gets a curve: the frame then hugs the bins and cells themselves.
pub(super) fn curve_axes(curve: &oxtuner_ini::Curve, x_values: &[f64], cells: &[f64]) -> CurveAxes {
    let (x_min, x_max) = axis_range(&curve.x_axis, x_values);
    let (y_min, y_max) = axis_range(&curve.y_axis, cells);
    let divisions = curve
        .y_axis
        .get(2)
        .and_then(oxtuner_ini::Number::literal)
        .filter(|value| value.is_finite())
        .map_or(FALLBACK_DIVISIONS, division_count);

    CurveAxes {
        x_min,
        x_max,
        y_min,
        y_max,
        divisions,
    }
}

/// A declared axis range, or a frame around the values when there is none.
fn axis_range(axis: &[oxtuner_ini::Number], values: &[f64]) -> (f64, f64) {
    let literal = |index: usize| {
        axis.get(index)
            .and_then(oxtuner_ini::Number::literal)
            .filter(|value| value.is_finite())
    };

    if let (Some(minimum), Some(maximum)) = (literal(0), literal(1))
        && minimum < maximum
    {
        return (minimum, maximum);
    }

    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;
    for value in values.iter().filter(|value| value.is_finite()) {
        minimum = minimum.min(*value);
        maximum = maximum.max(*value);
    }

    if !minimum.is_finite() || !maximum.is_finite() {
        return EMPTY_FRAME;
    }
    if maximum - minimum < DEGENERATE_RANGE {
        let padding = maximum.abs().max(1.0) * HUG_PADDING;
        return (minimum - padding, maximum + padding);
    }

    (minimum, maximum)
}
