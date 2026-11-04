//! The curve's state, free of GPUI types.
//!
//! A curve is a one-row table: the X axis holds the bins and the single row
//! holds the tuned values. The model keeps the snapshot, the selected point,
//! the optimistic value of a drag or key nudge, and the arithmetic that maps a
//! pointer position to a value. The view in [`super::Curve`] paints it and
//! turns requested edits into events; the core stays the only owner of tune
//! values (ADR-0020).

#![expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    reason = "GPUI geometry uses f32 while the tune model stores f64 and element indexes"
)]

use std::collections::HashMap;

use oxtuner_core::{TableSummary, nice_ticks, padded_range};

/// One plotted point.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    /// Bin value on the X axis.
    pub x: f64,
    /// Tuned value on the Y axis.
    pub y: f64,
}

impl CurvePoint {
    /// A point at `(x, y)`.
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// One curve as the widget draws it.
///
/// The host builds this from the core's snapshot, one point per bin. An
/// explicit Y range is what keeps the plot still while a point is dragged, so
/// a caller that knows the definition's `yAxis` should always set it. Without
/// one the widget falls back to a padded range of the values themselves.
#[derive(Clone, Debug, PartialEq)]
pub struct CurveSnapshot {
    id: String,
    title: String,
    name: String,
    units: String,
    digits: u8,
    x_label: String,
    x_units: String,
    x_digits: u8,
    y_label: String,
    x_range: Option<(f64, f64)>,
    y_range: Option<(f64, f64)>,
    divisions: usize,
    points: Vec<CurvePoint>,
    writable: bool,
}

impl CurveSnapshot {
    /// A curve with an id, a title, and its points.
    ///
    /// The cell variable defaults to the id, which is right for a curve whose
    /// id and value array share a name. Call [`Self::with_name`] when they
    /// differ.
    pub fn new(id: impl Into<String>, title: impl Into<String>, points: Vec<CurvePoint>) -> Self {
        let id = id.into();

        Self {
            name: id.clone(),
            id,
            title: title.into(),
            units: String::new(),
            digits: 0,
            x_label: String::new(),
            x_units: String::new(),
            x_digits: 0,
            y_label: String::new(),
            x_range: None,
            y_range: None,
            divisions: 4,
            points,
            writable: true,
        }
    }

    /// Reads a curve from the one-row table the core builds for it.
    ///
    /// Returns `None` for a table with more than one row. The table snapshot
    /// carries the current values but not the definition's axis limits, so a
    /// caller that has them should add [`Self::with_x_range`] and
    /// [`Self::with_y_range`].
    pub fn from_table(table: &TableSummary) -> Option<Self> {
        if table.rows != 1 || table.cols == 0 {
            return None;
        }

        let points = (0..table.cols)
            .map(|column| {
                let x = table
                    .x_axis
                    .values
                    .get(column)
                    .copied()
                    .unwrap_or(column as f64);
                let y = table.cell(0, column).unwrap_or(0.0);

                CurvePoint::new(x, y)
            })
            .collect();

        let mut snapshot = Self::new(table.id.clone(), table.title.clone(), points);
        snapshot.name.clone_from(&table.name);
        snapshot.units.clone_from(&table.units);
        snapshot.digits = table.digits;
        snapshot.x_label.clone_from(&table.x_axis.label);
        snapshot.x_units.clone_from(&table.x_axis.units);
        snapshot.x_digits = table.x_axis.digits;
        snapshot.y_label.clone_from(&table.y_axis.label);
        snapshot.writable = table.writable;

        if let Some(axes) = table.curve {
            snapshot.x_range = Some((axes.x_min, axes.x_max));
            snapshot.y_range = Some((axes.y_min, axes.y_max));
            snapshot.divisions = axes.divisions.max(2);
        }

        // The definition's value limits win over a wider display frame: a
        // drag must not aim at a value the core will clamp anyway.
        if let (Some((y_min, y_max)), Some(minimum), Some(maximum)) =
            (snapshot.y_range, table.cell_minimum, table.cell_maximum)
            && minimum < maximum
        {
            let low = y_min.max(minimum);
            let high = y_max.min(maximum);
            if low < high {
                snapshot.y_range = Some((low, high));
            }
        }

        Some(snapshot)
    }

    /// The curve id from the definition.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The window title from the definition.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The value array a committed edit writes to.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Units of the tuned values.
    pub fn units(&self) -> &str {
        &self.units
    }

    /// Digits the tuned values display with.
    pub fn digits(&self) -> u8 {
        self.digits
    }

    /// X axis label from the curve's first `columnLabel`.
    pub fn x_label(&self) -> &str {
        &self.x_label
    }

    /// Units of the X axis bins.
    pub fn x_units(&self) -> &str {
        &self.x_units
    }

    /// Digits the X axis bins display with.
    pub fn x_digits(&self) -> u8 {
        self.x_digits
    }

    /// Y axis label from the curve's `lineLabel`.
    pub fn y_label(&self) -> &str {
        &self.y_label
    }

    /// The exact X range, when the caller supplied one.
    pub fn x_range(&self) -> Option<(f64, f64)> {
        self.x_range
    }

    /// The exact Y range, when the caller supplied one.
    pub fn y_range(&self) -> Option<(f64, f64)> {
        self.y_range
    }

    /// How many intervals the Y axis aims for.
    pub fn divisions(&self) -> usize {
        self.divisions
    }

    /// The points, in bin order.
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// Whether the session may store edits.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Replaces the X bin values, keeping the tuned values and the rest of the
    /// snapshot.
    ///
    /// A host uses this to show a pending axis edit before the core answers,
    /// so the plot and its labels move in the same frame as the table header.
    #[must_use]
    pub fn with_x_values(mut self, values: impl IntoIterator<Item = f64>) -> Self {
        for (point, x) in self.points.iter_mut().zip(values) {
            point.x = x;
        }

        self
    }

    /// Sets the value array a committed edit writes to.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sets the units of the tuned values.
    #[must_use]
    pub fn with_units(mut self, units: impl Into<String>) -> Self {
        self.units = units.into();
        self
    }

    /// Sets the digits the tuned values display with.
    #[must_use]
    pub fn with_digits(mut self, digits: u8) -> Self {
        self.digits = digits;
        self
    }

    /// Sets the X axis label and units.
    #[must_use]
    pub fn with_x_axis(
        mut self,
        label: impl Into<String>,
        units: impl Into<String>,
        digits: u8,
    ) -> Self {
        self.x_label = label.into();
        self.x_units = units.into();
        self.x_digits = digits;
        self
    }

    /// Sets the Y axis label.
    #[must_use]
    pub fn with_y_label(mut self, label: impl Into<String>) -> Self {
        self.y_label = label.into();
        self
    }

    /// Sets the exact X range, normally the definition's `xAxis` limits.
    #[must_use]
    pub fn with_x_range(mut self, minimum: f64, maximum: f64) -> Self {
        self.x_range = Some((minimum, maximum));
        self
    }

    /// Sets the exact Y range, normally the definition's `yAxis` limits.
    #[must_use]
    pub fn with_y_range(mut self, minimum: f64, maximum: f64) -> Self {
        self.y_range = Some((minimum, maximum));
        self
    }

    /// Sets how many intervals the Y axis aims for.
    #[must_use]
    pub fn with_divisions(mut self, divisions: usize) -> Self {
        self.divisions = divisions.max(2);
        self
    }

    /// Sets whether the session may store edits.
    #[must_use]
    pub fn with_writable(mut self, writable: bool) -> Self {
        self.writable = writable;
        self
    }
}

/// A drag in progress.
#[derive(Clone, Copy, Debug)]
struct Drag {
    index: usize,
    /// The value the point had when the drag began.
    start: f64,
    /// Value minus the value under the pointer at the press. A grab that lands
    /// beside the dot therefore moves the dot with the pointer instead of
    /// snapping it to the pointer.
    offset: f64,
    /// The optimistic value that existed before the drag, restored when the
    /// drag is cancelled or ends where it started.
    previous: Option<f64>,
}

/// The curve's model.
#[derive(Debug)]
pub struct CurveModel {
    snapshot: Option<CurveSnapshot>,
    selected: usize,
    hovered: Option<usize>,
    pending: HashMap<usize, f64>,
    drag: Option<Drag>,
    /// The Y range held still for the length of a drag, so a data-derived
    /// scale cannot grow under the pointer and make the point lag behind it.
    frozen: Option<(f64, f64)>,
}

impl Default for CurveModel {
    fn default() -> Self {
        Self::new()
    }
}

impl CurveModel {
    /// An empty model with no curve.
    pub fn new() -> Self {
        Self {
            snapshot: None,
            selected: 0,
            hovered: None,
            pending: HashMap::new(),
            drag: None,
            frozen: None,
        }
    }

    /// Replaces the snapshot.
    ///
    /// The same curve keeps its selection and drops the optimistic values the
    /// snapshot confirms. A different curve resets the selection and any drag
    /// in progress.
    pub fn set_snapshot(&mut self, snapshot: CurveSnapshot) {
        let same = self
            .snapshot
            .as_ref()
            .is_some_and(|current| current.id == snapshot.id);

        if same {
            // A snapshot is authoritative. The core may clamp a write to the
            // variable's own limits, so waiting for the value to match would
            // leave an optimistic value on screen that never becomes true.
            if !self.is_dragging() {
                self.pending.clear();
            }
        } else {
            self.selected = 0;
            self.hovered = None;
            self.pending.clear();
            self.drag = None;
        }

        self.frozen = None;
        self.snapshot = Some(snapshot);
        self.clamp_selection();
    }

    /// Drops the snapshot and every piece of view state.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// The snapshot in use.
    pub fn snapshot(&self) -> Option<&CurveSnapshot> {
        self.snapshot.as_ref()
    }

    /// How many points the curve has.
    pub fn point_count(&self) -> usize {
        self.snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.points.len())
    }

    /// The displayed value of one point, including an optimistic edit.
    pub fn value(&self, index: usize) -> Option<f64> {
        let point = self.snapshot.as_ref()?.points.get(index)?;

        Some(self.pending.get(&index).copied().unwrap_or(point.y))
    }

    /// The bin value of one point.
    pub fn x(&self, index: usize) -> Option<f64> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.points.get(index))
            .map(|point| point.x)
    }

    /// The selected point.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The point under the pointer, if any.
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Marks a point as hovered, or clears the hover.
    pub fn set_hovered(&mut self, index: Option<usize>) {
        self.hovered = index.filter(|index| *index < self.point_count());
    }

    /// Selects one point. Returns whether the selection moved.
    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.point_count() || self.selected == index {
            return false;
        }

        self.selected = index;

        true
    }

    /// Moves the selection by `delta`, stopping at either end.
    pub fn select_relative(&mut self, delta: isize) -> bool {
        let count = self.point_count();
        if count == 0 || delta == 0 {
            return false;
        }

        let next = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize).min(count - 1)
        };

        self.select(next)
    }

    /// Whether a point may be edited.
    pub fn is_writable(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(CurveSnapshot::is_writable)
    }

    /// The Y range the plot draws.
    pub fn y_range(&self) -> (f64, f64) {
        if let Some(frozen) = self.frozen {
            return frozen;
        }

        if let Some(range) = self
            .snapshot
            .as_ref()
            .and_then(CurveSnapshot::y_range)
            .filter(|range| is_usable(*range))
        {
            return range;
        }

        let values = (0..self.point_count()).filter_map(|index| self.value(index));
        let range = padded_range(values);

        if is_usable(range) { range } else { (0.0, 1.0) }
    }

    /// The X range the plot draws.
    ///
    /// The definition's `xAxis` is the preferred frame, but bins are editable
    /// and a hand-edited bin can move outside it. The drawn range is the union
    /// of both, so an edited bin stays visible and the labels follow it instead
    /// of holding the point at the old edge. The expanded side carries a small
    /// margin, which may cross zero, so even a first bin of zero sits inside
    /// the plot instead of on its edge.
    pub fn x_range(&self) -> (f64, f64) {
        let declared = self
            .snapshot
            .as_ref()
            .and_then(CurveSnapshot::x_range)
            .filter(|range| is_usable(*range));
        let points = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.points.as_slice())
            .unwrap_or_default();
        let data = data_range(points.iter().map(|point| point.x));

        match (declared, data) {
            (Some((minimum, maximum)), Some((first, last))) => {
                if first >= minimum && last <= maximum {
                    return (minimum, maximum);
                }

                // A bin outside the definition's axis expands the frame, with
                // a small margin so the point sits inside the plot. The margin
                // is rounded to a fine step, which keeps the label below the
                // edited value instead of exactly on it.
                let span = maximum.max(last) - minimum.min(first);
                let margin = span * 0.05;
                let step = nice_step(span / 20.0);
                let mut expanded_min = minimum;
                let mut expanded_max = maximum;

                if first < minimum {
                    // The margin may cross zero. That is deliberate: a first
                    // bin of zero still gets room so its point sits inside the
                    // plot, not on the frame.
                    expanded_min = ((first - margin) / step).floor() * step;
                }
                if last > maximum {
                    expanded_max = ((last + margin) / step).ceil() * step;
                }

                (expanded_min, expanded_max)
            }
            (Some(range), None) => range,
            (None, Some(range)) => {
                let range = padded_range([range.0, range.1]);

                if is_usable(range) { range } else { (0.0, 1.0) }
            }
            (None, None) => (0.0, 1.0),
        }
    }

    /// The smallest change a nudge or a drag writes.
    ///
    /// The definition's display digits decide it, so a curve shown without
    /// decimals nudges by one and one shown with two decimals by a hundredth.
    pub fn step(&self) -> f64 {
        let digits = self
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.digits.min(6));

        10f64.powi(-i32::from(digits))
    }

    /// Moves the selected point by `steps` of [`Self::step`].
    ///
    /// Returns the point and its new value when it changed.
    pub fn nudge(&mut self, steps: i32) -> Option<(usize, f64)> {
        if !self.is_writable() || steps == 0 {
            return None;
        }

        let index = self.selected_index()?;
        let current = self.value(index)?;
        let value = self.clamp_value(current + f64::from(steps) * self.step());

        if value == current {
            return None;
        }

        self.pending.insert(index, value);

        Some((index, value))
    }

    /// Starts dragging one point at a Y fraction, 0 at the top of the plot.
    ///
    /// Returns whether a drag started. A read-only curve, an empty curve, or a
    /// degenerate range refuses.
    pub fn begin_drag(&mut self, index: usize, y_fraction: f64) -> bool {
        if !self.is_writable() {
            return false;
        }

        let Some(value) = self.value(index) else {
            return false;
        };
        let range = self.y_range();
        let span = range.1 - range.0;

        if !span.is_finite() || span <= 0.0 {
            return false;
        }

        self.frozen = Some(range);
        self.drag = Some(Drag {
            index,
            start: value,
            offset: value - value_at(range, y_fraction),
            previous: self.pending.get(&index).copied(),
        });
        self.selected = index;

        true
    }

    /// Moves the dragged point to a Y fraction. Returns the new value when it
    /// changed.
    pub fn drag_to(&mut self, y_fraction: f64) -> Option<(usize, f64)> {
        let drag = self.drag?;
        let range = self.y_range();
        let value = self.clamp_value(value_at(range, y_fraction) + drag.offset);

        if self.pending.get(&drag.index).copied() == Some(value) {
            return None;
        }

        self.pending.insert(drag.index, value);

        Some((drag.index, value))
    }

    /// Ends the drag. Returns the point and value to write when the drag moved
    /// it, and `None` when it ended where it started or no drag was running.
    pub fn finish_drag(&mut self) -> Option<(usize, f64)> {
        let drag = self.drag.take()?;
        self.frozen = None;

        let value = self.value(drag.index)?;

        if (value - drag.start).abs() < f64::EPSILON {
            match drag.previous {
                Some(previous) => {
                    self.pending.insert(drag.index, previous);
                }
                None => {
                    self.pending.remove(&drag.index);
                }
            }

            return None;
        }

        Some((drag.index, value))
    }

    /// Abandons the drag at its starting value.
    pub fn cancel_drag(&mut self) -> Option<usize> {
        let drag = self.drag.take()?;
        self.frozen = None;

        match drag.previous {
            Some(previous) => {
                self.pending.insert(drag.index, previous);
            }
            None => {
                self.pending.remove(&drag.index);
            }
        }

        Some(drag.index)
    }

    /// Whether a drag is running.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The point being dragged, if any.
    pub fn drag_index(&self) -> Option<usize> {
        self.drag.map(|drag| drag.index)
    }

    /// Drops optimistic values after the core rejected an edit.
    pub fn reject_pending(&mut self) {
        self.pending.clear();
        self.drag = None;
        self.frozen = None;
    }

    /// Where one point sits across the plot, 0 at the left edge and 1 at the
    /// right. Values outside the declared X range are held at the edge so the
    /// point stays reachable.
    pub fn x_fraction(&self, index: usize) -> Option<f64> {
        let x = self.x(index)?;
        let range = self.x_range();
        let span = range.1 - range.0;

        if span <= 0.0 {
            return Some(0.5);
        }

        Some(((x - range.0) / span).clamp(0.0, 1.0))
    }

    /// Where one point sits up the plot, 0 at the top edge and 1 at the
    /// bottom. Values outside the declared Y range are held at the edge.
    pub fn y_fraction(&self, index: usize) -> Option<f64> {
        let value = self.value(index)?;
        let range = self.y_range();
        let span = range.1 - range.0;

        if span <= 0.0 {
            return Some(0.5);
        }

        Some(((range.1 - value) / span).clamp(0.0, 1.0))
    }

    /// The nearest point to a plot position, in fractions, within `radius` of
    /// that point's pixel position.
    pub fn nearest(
        &self,
        x_fraction: f64,
        y_fraction: f64,
        width: f32,
        height: f32,
        radius: f32,
    ) -> Option<usize> {
        let mut nearest = None;
        let mut nearest_distance = radius;

        for index in 0..self.point_count() {
            let (Some(x), Some(y)) = (self.x_fraction(index), self.y_fraction(index)) else {
                continue;
            };

            let dx = (x - x_fraction) as f32 * width;
            let dy = (y - y_fraction) as f32 * height;
            let distance = dx.hypot(dy);

            if distance <= nearest_distance {
                nearest = Some(index);
                nearest_distance = distance;
            }
        }

        nearest
    }

    /// Snaps a value to the step and clamps it to the Y range.
    fn clamp_value(&self, value: f64) -> f64 {
        let step = self.step();
        let snapped = (value / step).round() * step;
        let range = self.y_range();
        let clamped = snapped.clamp(range.0, range.1);

        round_to_digits(
            clamped,
            self.snapshot.as_ref().map_or(0, |snapshot| snapshot.digits),
        )
    }

    fn selected_index(&self) -> Option<usize> {
        let count = self.point_count();
        (count > 0).then(|| self.selected.min(count - 1))
    }

    fn clamp_selection(&mut self) {
        let count = self.point_count();

        self.selected = if count == 0 {
            0
        } else {
            self.selected.min(count - 1)
        };

        if self.hovered.is_some_and(|index| index >= count) {
            self.hovered = None;
        }
    }
}

/// Tick values for an axis: the exact limits plus the round values between
/// them, with interior ticks dropped when they would sit on an endpoint.
pub(crate) fn curve_ticks(range: (f64, f64), target: usize) -> Vec<f64> {
    let (minimum, maximum) = range;

    if !minimum.is_finite() || !maximum.is_finite() || minimum >= maximum {
        return Vec::new();
    }

    let span = maximum - minimum;
    let mut ticks = vec![minimum, maximum];

    for tick in nice_ticks(minimum, maximum, target) {
        let from_start = (tick - minimum) / span;
        let from_end = (maximum - tick) / span;

        if from_start > 0.08 && from_end > 0.08 {
            ticks.push(tick);
        }
    }

    ticks.sort_by(f64::total_cmp);
    ticks.dedup_by(|left, right| (*left - *right).abs() <= span * 1e-9);

    ticks
}

/// The smallest positive gap between adjacent ticks, for label precision.
pub(crate) fn tick_step(ticks: &[f64]) -> f64 {
    ticks
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|step| *step > 0.0 && step.is_finite())
        .fold(f64::INFINITY, f64::min)
}

/// Formats an axis tick with enough decimals to tell adjacent ticks apart.
pub(crate) fn format_tick(value: f64, step: f64) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }

    let step = if step.is_finite() && step > 0.0 {
        step
    } else {
        1.0
    };
    let value = if value.abs() < step * 1e-6 {
        0.0
    } else {
        value
    };
    let digits = tick_digits(step);

    format!("{value:.digits$}")
}

fn tick_digits(step: f64) -> usize {
    let mut scale = 1.0;

    for digits in 0..=4 {
        let scaled = step * scale;

        if (scaled - scaled.round()).abs() <= 1e-6 {
            return digits;
        }

        scale *= 10.0;
    }

    4
}

/// The nearest 1, 2, or 5 times a power of ten at or above `raw`.
fn nice_step(raw: f64) -> f64 {
    if !raw.is_finite() || raw <= 0.0 {
        return 1.0;
    }

    let magnitude = 10f64.powf(raw.log10().floor());
    let residual = raw / magnitude;

    magnitude
        * match residual {
            value if value <= 1.0 => 1.0,
            value if value <= 2.0 => 2.0,
            value if value <= 5.0 => 5.0,
            _ => 10.0,
        }
}

fn data_range(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;

    for value in values {
        if value.is_finite() {
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
    }

    (minimum.is_finite() && maximum.is_finite()).then_some((minimum, maximum))
}

fn is_usable(range: (f64, f64)) -> bool {
    range.0.is_finite() && range.1.is_finite() && range.0 < range.1
}

fn value_at(range: (f64, f64), fraction: f64) -> f64 {
    let fraction = fraction.clamp(0.0, 1.0);

    range.1 - fraction * (range.1 - range.0)
}

fn round_to_digits(value: f64, digits: u8) -> f64 {
    let scale = 10f64.powi(i32::from(digits.min(6)));

    (value * scale).round() / scale
}
