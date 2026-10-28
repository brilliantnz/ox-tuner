//! Axis scales and series decimation.
//!
//! The live chart paints polylines. A 60 s window at 100 Hz holds 6 000 samples
//! per series, and drawing every one makes frame cost grow with history length
//! (the P9 finding). [`decimate`] keeps a bounded point count per series while
//! preserving each bucket's minimum and maximum, so a spike or a lean event
//! does not disappear when the history gets long.

use super::Sample;

/// Reduces a series to at most `max_points` samples.
///
/// Short series are returned unchanged. Longer ones are split into time-ordered
/// buckets and each bucket contributes its lowest and highest sample, in time
/// order. First and last samples are always kept. The result therefore carries
/// the same visual envelope as the raw series at a bounded path cost.
///
/// The cap is honoured exactly: two points per bucket plus the endpoints, with
/// `max_points / 2 - 1` buckets.
pub fn decimate(points: &[Sample], max_points: usize) -> Vec<Sample> {
    if points.len() <= max_points {
        return points.to_vec();
    }
    if max_points < 4 {
        return vec![points[0], points[points.len() - 1]];
    }

    let buckets = (max_points / 2).saturating_sub(1);
    if buckets == 0 {
        return vec![points[0], points[points.len() - 1]];
    }

    let mut reduced = Vec::with_capacity(max_points);
    reduced.push(points[0]);

    let length = points.len();
    for bucket in 0..buckets {
        let start = bucket * length / buckets;
        let end = ((bucket + 1) * length / buckets).min(length);
        if start >= end {
            continue;
        }
        let slice = &points[start..end];
        let mut lowest = 0usize;
        let mut highest = 0usize;
        for (index, sample) in slice.iter().enumerate() {
            if sample.v < slice[lowest].v {
                lowest = index;
            }
            if sample.v > slice[highest].v {
                highest = index;
            }
        }
        let (first, second) = if lowest <= highest {
            (lowest, highest)
        } else {
            (highest, lowest)
        };
        push_distinct(&mut reduced, slice[first]);
        if second != first {
            push_distinct(&mut reduced, slice[second]);
        }
    }

    let last = points[length - 1];
    if reduced.last().is_none_or(|sample| sample.t < last.t) {
        reduced.push(last);
    }
    reduced
}

fn push_distinct(points: &mut Vec<Sample>, sample: Sample) {
    if points
        .last()
        .is_none_or(|previous: &Sample| previous.t < sample.t)
    {
        points.push(sample);
    }
}

/// Adds display padding to a range and widens a flat one.
///
/// Five percent of headroom keeps a line off the frame; a series that never
/// moves still gets a readable band instead of a zero-height axis.
pub fn padded_range(values: impl IntoIterator<Item = f64>) -> (f64, f64) {
    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;
    for value in values {
        if !value.is_finite() {
            continue;
        }
        minimum = minimum.min(value);
        maximum = maximum.max(value);
    }

    if !minimum.is_finite() || !maximum.is_finite() {
        return (0.0, 1.0);
    }

    let span = maximum - minimum;
    if span.abs() < 1e-12 {
        let padding = maximum.abs().max(1.0) * 0.05;
        return (minimum - padding, maximum + padding);
    }

    let padding = span * 0.05;
    (minimum - padding, maximum + padding)
}

/// Round tick values for a value axis.
///
/// Ticks are multiples of `1`, `2` or `5` times a power of ten, and lie inside
/// the range. Fewer than two ticks means the caller should show the bounds
/// instead.
pub fn nice_ticks(minimum: f64, maximum: f64, target: usize) -> Vec<f64> {
    if !minimum.is_finite() || !maximum.is_finite() {
        return Vec::new();
    }
    let (minimum, maximum) = if minimum <= maximum {
        (minimum, maximum)
    } else {
        (maximum, minimum)
    };
    let span = maximum - minimum;
    if span.abs() < 1e-12 {
        return vec![minimum];
    }

    let target = target.clamp(2, 12);
    let raw_step = span / target as f64;
    let magnitude = 10f64.powf(raw_step.log10().floor());
    let residual = raw_step / magnitude;
    let step = magnitude
        * match residual {
            value if value <= 1.0 => 1.0,
            value if value <= 2.0 => 2.0,
            value if value <= 5.0 => 5.0,
            _ => 10.0,
        };

    let first = (minimum / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut tick = first;
    let limit = maximum + step * 1e-6;
    // A malformed range must not spin forever; the cap is far above any UI use.
    for _ in 0..64 {
        if tick > limit {
            break;
        }
        if tick >= minimum - step * 1e-6
            && ticks
                .last()
                .is_none_or(|previous: &f64| (tick - *previous).abs() > step * 1e-6)
        {
            ticks.push(tick);
        }
        tick += step;
    }
    ticks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_ignores_non_finite_values() {
        let (minimum, maximum) = padded_range([f64::NAN, 1.0, 2.0, f64::INFINITY]);

        assert!(minimum < 1.0);
        assert!(maximum > 2.0);
    }

    #[test]
    fn an_empty_range_gets_a_unit_band() {
        assert_eq!(padded_range([]), (0.0, 1.0));
    }

    #[test]
    fn ticks_are_inside_the_range_and_evenly_spaced() {
        let ticks = nice_ticks(0.0, 100.0, 5);
        assert_eq!(ticks.first(), Some(&0.0));
        assert!(ticks.last().is_some_and(|tick| *tick <= 100.0));

        let step = ticks[1] - ticks[0];
        for pair in ticks.windows(2) {
            assert!((pair[1] - pair[0] - step).abs() < 1e-9);
        }
    }

    #[test]
    fn a_one_value_range_is_a_single_tick() {
        assert_eq!(nice_ticks(5.0, 5.0, 5), vec![5.0]);
    }

    #[test]
    fn reversed_ranges_work() {
        assert_eq!(nice_ticks(100.0, 0.0, 5), nice_ticks(0.0, 100.0, 5));
    }

    #[test]
    fn decimation_keeps_time_order() {
        let points: Vec<Sample> = (0..5000)
            .map(|index| Sample {
                t: f64::from(index),
                v: (f64::from(index) * 0.37).sin(),
            })
            .collect();

        let reduced = decimate(&points, 1500);

        assert!(reduced.len() <= 1500);
        assert!(reduced.windows(2).all(|pair| pair[0].t <= pair[1].t));
    }

    #[test]
    fn decimation_preserves_low_and_high_envelopes() {
        let points: Vec<Sample> = (0..4000)
            .map(|index| Sample {
                t: f64::from(index),
                v: if index % 1000 == 500 { -50.0 } else { 10.0 },
            })
            .collect();

        let reduced = decimate(&points, 1000);

        assert!(reduced.iter().any(|sample| sample.v == -50.0));
        assert!(reduced.iter().any(|sample| sample.v == 10.0));
    }

    #[test]
    fn a_tiny_cap_still_keeps_the_ends() {
        let points: Vec<Sample> = (0..100)
            .map(|index| Sample {
                t: f64::from(index),
                v: f64::from(index),
            })
            .collect();

        let reduced = decimate(&points, 3);

        assert!(reduced.len() <= 3);
        assert_eq!(reduced.first().map(|sample| sample.t), Some(0.0));
        assert_eq!(reduced.last().map(|sample| sample.t), Some(99.0));
    }
}
