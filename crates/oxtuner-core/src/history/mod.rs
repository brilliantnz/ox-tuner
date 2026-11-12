//! Bounded realtime history and the pure chart data model.
//!
//! The live charts, a dyno pull and a log replay all need the same data
//! shaping: a bounded ring of decoded frames, a time window over it, and a
//! finite set of points per series. That logic is headless and lives here, in
//! the core, so the poller owns the only writer and every widget crate can read
//! the same history without pulling GPUI into the core (ADR-0001, ADR-0020).
//!
//! - [`ChartHistory`] — the shared ring. The core writes it; views read it on
//!   their render tick. Drop-oldest with counted overruns.
//! - [`TimeWindow`], [`SeriesPoints`] and [`decimate`] — windowed extraction,
//!   thinned to a bounded point count while keeping each bucket's min/max.
//! - [`nice_ticks`], [`padded_range`] — axis scales.
//!
//! No `gpui*` type, no I/O and no threads created here.

mod ring;
mod scale;
mod window;

pub use ring::{ChannelInfo, ChartHistory, HistoryStats, NearestSample, SeriesPoints};
pub use scale::{decimate, nice_ticks, padded_range};
pub use window::{Sample, TimeWindow};

#[cfg(test)]
mod tests {
    #![expect(
        clippy::float_cmp,
        reason = "the history tests compare exact sample values"
    )]

    use super::*;

    fn history(capacity: usize) -> ChartHistory {
        let history = ChartHistory::new(capacity);
        history.configure(vec![
            ChannelInfo::new("rpm", "rpm"),
            ChannelInfo::new("map", "kPa"),
            ChannelInfo::new("afr", "AFR"),
        ]);
        history
    }

    #[test]
    fn a_pushed_frame_is_bounded_and_accounted_for() {
        let history = history(3);
        for step in 0..5 {
            history.push_aligned(f64::from(step), vec![1000.0, 50.0, 14.7]);
        }

        let stats = history.stats();
        assert_eq!(stats.capacity, 3);
        assert_eq!(stats.frames, 3);
        assert_eq!(stats.pushed, 5);
        assert_eq!(stats.overruns, 2);
        assert_eq!(
            u64::try_from(stats.frames).unwrap_or(u64::MAX) + stats.overruns,
            stats.pushed
        );
    }

    #[test]
    fn extraction_returns_the_requested_window_in_order() {
        let history = history(10);
        for step in 0..10 {
            history.push_aligned(f64::from(step), vec![f64::from(step), 0.0, 0.0]);
        }

        let window = TimeWindow::new(3.0, 6.0);
        let series = history.extract(&["rpm"], window, 100);

        assert_eq!(series.len(), 1);
        let times: Vec<f64> = series[0].points.iter().map(|sample| sample.t).collect();
        assert_eq!(times, [3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn an_unknown_channel_yields_an_empty_series() {
        let history = history(4);
        history.push_aligned(0.0, vec![1.0, 2.0, 3.0]);

        let series = history.extract(&["nope"], TimeWindow::new(0.0, 1.0), 10);

        assert_eq!(series.len(), 1);
        assert!(
            series[0].points.is_empty(),
            "an unknown channel must yield no points"
        );
    }

    #[test]
    fn decimation_keeps_the_point_count_bounded_and_the_peaks_visible() {
        let points: Vec<Sample> = (0..6000)
            .map(|index| Sample {
                t: f64::from(index),
                v: if index == 4321 { 1000.0 } else { 1.0 },
            })
            .collect();

        let reduced = decimate(&points, 1500);

        assert!(reduced.len() <= 1500, "{} points", reduced.len());
        assert!(
            reduced.iter().any(|sample| sample.v == 1000.0),
            "the spike survives decimation"
        );
        assert_eq!(reduced.first().map(|sample| sample.t), Some(0.0));
        assert_eq!(reduced.last().map(|sample| sample.t), Some(5999.0));
    }

    #[test]
    fn a_short_series_is_returned_untouched() {
        let points: Vec<Sample> = (0..10)
            .map(|index| Sample {
                t: f64::from(index),
                v: f64::from(index),
            })
            .collect();

        assert_eq!(decimate(&points, 1500), points);
    }

    #[test]
    fn nice_ticks_cover_the_range_without_going_wild() {
        let ticks = nice_ticks(3.0, 97.0, 5);

        assert!(ticks.len() >= 3, "{ticks:?}");
        assert!(ticks.windows(2).all(|pair| pair[1] > pair[0]));
        assert!(ticks.first().is_some_and(|tick| *tick >= 3.0));
        assert!(ticks.last().is_some_and(|tick| *tick <= 97.0));
    }

    #[test]
    fn a_flat_range_is_widened_for_display() {
        let (minimum, maximum) = padded_range([5.0, 5.0]);

        assert!(minimum < 5.0);
        assert!(maximum > 5.0);
    }

    #[test]
    fn the_nearest_sample_is_found_across_series() {
        let history = history(10);
        for step in 0..5 {
            history.push_aligned(f64::from(step), vec![f64::from(step) * 10.0, 1.0, 14.0]);
        }

        let nearest = history.nearest(&["rpm", "afr"], 2.1);

        assert_eq!(nearest[0].sample.map(|sample| sample.t), Some(2.0));
        assert_eq!(nearest[0].sample.map(|sample| sample.v), Some(20.0));
        assert_eq!(nearest[1].sample.map(|sample| sample.v), Some(14.0));
    }

    #[test]
    fn a_ten_minute_session_keeps_the_ring_bounded() {
        let history = history(6_000);
        // 600 s at 100 Hz.
        for step in 0..60_000 {
            history.push_aligned(f64::from(step) * 0.01, vec![1.0, 2.0, 3.0]);
        }

        let stats = history.stats();
        assert_eq!(stats.capacity, 6_000);
        assert_eq!(stats.frames, 6_000);
        assert_eq!(stats.pushed, 60_000);
        assert_eq!(stats.overruns, 54_000);
        assert!(stats.is_accounted());
    }

    #[test]
    fn configuring_new_channels_clears_the_ring() {
        let history = history(4);
        history.push_aligned(0.0, vec![1.0, 2.0, 3.0]);

        history.configure(vec![ChannelInfo::new("tps", "%")]);

        assert_eq!(history.stats().frames, 0);
        assert_eq!(history.channels().len(), 1);
    }

    #[test]
    fn hidden_and_visible_constructors_set_the_flag() {
        assert!(!ChannelInfo::new("rpm", "rpm").hidden);
        assert!(ChannelInfo::new("rpm", "rpm").hidden().hidden);
        assert!(!ChannelInfo::new("rpm", "rpm").hidden().visible().hidden);
    }
}
