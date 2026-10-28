//! One sample of one series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Seconds, relative to the session's first frame.
    pub t: f64,
    /// The channel's value in user units.
    pub v: f64,
}

/// A half-open-by-nature time interval: `[start, end]`.
///
/// Both ends are inclusive for lookup. A window is built from a duration and a
/// right edge; panning moves the right edge back from the newest sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeWindow {
    /// Left edge, in seconds.
    pub start: f64,
    /// Right edge, in seconds.
    pub end: f64,
}

impl TimeWindow {
    /// A window with an explicit left and right edge.
    pub fn new(start: f64, end: f64) -> Self {
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        Self { start, end }
    }

    /// A window ending at `end`, `seconds` long.
    pub fn ending_at(end: f64, seconds: f64) -> Self {
        let seconds = seconds.max(0.001);
        Self::new(end - seconds, end)
    }

    /// The window's length in seconds.
    pub fn span(self) -> f64 {
        (self.end - self.start).max(f64::EPSILON)
    }

    /// Whether `time` lies inside the window.
    pub fn contains(self, time: f64) -> bool {
        time >= self.start && time <= self.end
    }

    /// Where `time` falls in the window, 0 at the left and 1 at the right.
    pub fn fraction(self, time: f64) -> f64 {
        ((time - self.start) / self.span()).clamp(0.0, 1.0)
    }

    /// The time at `fraction` across the window.
    pub fn time_at(self, fraction: f64) -> f64 {
        self.start + self.span() * fraction.clamp(0.0, 1.0)
    }

    /// Moves the window by `seconds`.
    pub fn shifted(self, seconds: f64) -> Self {
        Self::new(self.start + seconds, self.end + seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_normalises_its_edges() {
        assert_eq!(TimeWindow::new(10.0, 2.0), TimeWindow::new(2.0, 10.0));
    }

    #[test]
    fn fractions_stay_inside_the_window() {
        let window = TimeWindow::new(10.0, 20.0);

        assert_eq!(window.fraction(15.0), 0.5);
        assert_eq!(window.fraction(0.0), 0.0);
        assert_eq!(window.fraction(30.0), 1.0);
        assert_eq!(window.time_at(0.25), 12.5);
    }

    #[test]
    fn a_window_ending_at_keeps_its_length() {
        let window = TimeWindow::ending_at(60.0, 30.0);

        assert_eq!(window.start, 30.0);
        assert_eq!(window.end, 60.0);
        assert_eq!(window.span(), 30.0);
    }
}
