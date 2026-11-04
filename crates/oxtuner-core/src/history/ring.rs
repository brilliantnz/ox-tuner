//! The bounded realtime history shared by the core and every chart view.
//!
//! One ring, many readers (ADR-0014). The core pushes decoded frames; the UI
//! reads a window on its render tick. Memory is fixed when the ring is built:
//! capacity is a frame count, not a duration, so a slow poll rate never grows
//! the buffer. At the default 100 Hz ceiling a 6 000-frame ring holds a minute
//! of every channel.
//!
//! Dropping the oldest frame is counted, not silent. `pushed == frames +
//! overruns` is the accounting invariant the chart surfaces report.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
};

use super::{Sample, TimeWindow, decimate};

/// One channel the history carries.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    /// Channel name exactly as the definition writes it.
    pub name: String,
    /// Display units (empty when the definition has none).
    pub units: String,
    /// Whether the definition hides the channel from a normal live view.
    pub hidden: bool,
}

impl ChannelInfo {
    /// A visible channel with its units.
    pub fn new(name: impl Into<String>, units: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            units: units.into(),
            hidden: false,
        }
    }

    /// Marks the channel hidden, as the definition's `hide` flag does.
    #[must_use]
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }
}

/// What the ring holds, for status lines and tests.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryStats {
    /// Frames the ring can hold.
    pub capacity: usize,
    /// Frames retained now.
    pub frames: usize,
    /// Frames ever pushed since the last configure.
    pub pushed: u64,
    /// Frames dropped because the ring was full.
    pub overruns: u64,
}

impl HistoryStats {
    /// Whether every pushed frame is either retained or counted as an overrun.
    pub fn is_accounted(&self) -> bool {
        self.pushed == u64::try_from(self.frames).unwrap_or(u64::MAX) + self.overruns
    }
}

/// The nearest sample of one series to a time, for hover readouts.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct NearestSample {
    /// Channel name.
    pub id: String,
    /// The closest sample, or `None` when the series has no data.
    pub sample: Option<Sample>,
}

/// The samples of one series inside a window, already decimated.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeriesPoints {
    /// Channel name.
    pub id: String,
    /// Points in time order, at most the requested cap.
    pub points: Vec<Sample>,
    /// Lowest value among all samples in the window, not only the drawn ones.
    pub minimum: Option<f64>,
    /// Highest value among all samples in the window.
    pub maximum: Option<f64>,
}

/// One decoded frame.
#[derive(Debug)]
struct Frame {
    time: f64,
    values: Vec<f64>,
}

/// The mutable inside of the ring.
#[derive(Debug)]
struct Inner {
    /// Shared so a reader can hand the list out without copying every channel.
    channels: Arc<[ChannelInfo]>,
    index: HashMap<String, usize>,
    frames: VecDeque<Arc<Frame>>,
    capacity: usize,
    pushed: u64,
    overruns: u64,
}

impl Inner {
    /// The newest time in the ring, or zero before the first frame.
    fn newest(&self) -> Option<f64> {
        self.frames.back().map(|frame| frame.time)
    }

    fn oldest(&self) -> Option<f64> {
        self.frames.front().map(|frame| frame.time)
    }

    /// The samples of `id` inside `window`, decimated to `max_points`.
    fn series(&self, id: &str, window: TimeWindow, max_points: usize) -> SeriesPoints {
        let mut points = Vec::new();
        if let Some(index) = self.index.get(id) {
            for frame in self.frames.iter().rev() {
                if frame.time < window.start {
                    break;
                }
                if frame.time > window.end {
                    continue;
                }
                let Some(value) = frame.values.get(*index) else {
                    continue;
                };
                if value.is_finite() {
                    points.push(Sample {
                        t: frame.time,
                        v: *value,
                    });
                }
            }
            points.reverse();
        }

        let (minimum, maximum) = range(&points);
        SeriesPoints {
            id: id.to_owned(),
            points: decimate(&points, max_points),
            minimum,
            maximum,
        }
    }

    fn nearest(&self, id: &str, time: f64) -> Option<Sample> {
        let index = *self.index.get(id)?;
        let mut nearest: Option<Sample> = None;
        let mut distance = f64::INFINITY;
        for frame in self.frames.iter().rev() {
            let Some(value) = frame.values.get(index) else {
                continue;
            };
            if !value.is_finite() {
                continue;
            }
            let delta = (frame.time - time).abs();
            if delta < distance {
                distance = delta;
                nearest = Some(Sample {
                    t: frame.time,
                    v: *value,
                });
            }
            // The ring is time ordered: once we are well past the left edge of
            // the nearest candidate there is nothing closer further back.
            if frame.time < time - distance {
                break;
            }
        }
        nearest
    }

    fn latest_values(&self, ids: &[String]) -> Vec<NearestSample> {
        let Some(newest) = self.newest() else {
            return ids
                .iter()
                .map(|id| NearestSample {
                    id: id.clone(),
                    sample: None,
                })
                .collect();
        };
        ids.iter()
            .map(|id| NearestSample {
                id: id.clone(),
                sample: self.nearest(id, newest),
            })
            .collect()
    }
}

/// The shared, bounded history.
///
/// Cloneable and cheap to hand out: the core and every chart share one
/// allocation. Reads take a short read lock; no view holds it across paint.
#[derive(Debug, Clone)]
pub struct ChartHistory {
    inner: Arc<RwLock<Inner>>,
}

impl Default for ChartHistory {
    fn default() -> Self {
        Self::new(6_000)
    }
}

impl ChartHistory {
    /// A ring that holds `capacity` frames (at least two).
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                channels: Vec::new().into(),
                index: HashMap::new(),
                frames: VecDeque::new(),
                capacity: capacity.max(2),
                pushed: 0,
                overruns: 0,
            })),
        }
    }

    /// Replaces the channel list and clears every frame.
    ///
    /// Called when a definition loads or a connection opens: the values already
    /// in the ring belong to a different channel map, so keeping them would
    /// label old samples with new names.
    pub fn configure(&self, channels: Vec<ChannelInfo>) {
        let mut inner = self.write();
        inner.index.clear();
        for (position, channel) in channels.iter().enumerate() {
            inner.index.insert(channel.name.clone(), position);
        }
        inner.channels = channels.into();
        inner.frames.clear();
        inner.pushed = 0;
        inner.overruns = 0;
    }

    /// Empties the ring but keeps its channels.
    pub fn clear(&self) {
        let mut inner = self.write();
        inner.frames.clear();
        inner.pushed = 0;
        inner.overruns = 0;
    }

    /// The channels this history carries, in definition order.
    ///
    /// The list is shared: cloning the returned handle is one refcount bump, not
    /// a copy of every name and unit, so a render tick can call this freely.
    pub fn channels(&self) -> Arc<[ChannelInfo]> {
        Arc::clone(&self.read().channels)
    }

    /// Pushes one decoded frame, naming each value.
    ///
    /// Values whose channel the history does not know are ignored; channels
    /// missing from the frame become `NaN` for that row so a chart draws a gap
    /// instead of inventing a value. The same-named channel and value must
    /// match in case, because both sides come from one definition.
    pub fn push_frame(&self, time: f64, values: &[(&str, f64)]) {
        self.push_frame_iter(time, values.iter().copied());
    }

    /// [`Self::push_frame`] from an iterator, so a live poller can name the
    /// values it already has instead of collecting a slice every frame.
    pub fn push_frame_iter<'a>(&self, time: f64, values: impl IntoIterator<Item = (&'a str, f64)>) {
        if !time.is_finite() {
            return;
        }
        let mut inner = self.write();
        let mut row = vec![f64::NAN; inner.channels.len()];
        for (name, value) in values {
            if let Some(index) = inner.index.get(name) {
                row[*index] = value;
            }
        }
        let time = inner.newest().map_or(time, |newest| time.max(newest));
        inner.push(time, row);
    }

    /// Pushes a frame already aligned with [`Self::channels`], for tests and
    /// importers. A row shorter than the channel list is padded with `NaN`.
    pub fn push_aligned(&self, time: f64, values: Vec<f64>) {
        if !time.is_finite() {
            return;
        }
        let mut inner = self.write();
        let mut row = values;
        row.resize(inner.channels.len(), f64::NAN);
        let time = inner.newest().map_or(time, |newest| time.max(newest));
        inner.push(time, row);
    }

    /// The newest sample time, or `None` before the first frame.
    pub fn newest_time(&self) -> Option<f64> {
        self.read().newest()
    }

    /// The oldest retained sample time.
    pub fn oldest_time(&self) -> Option<f64> {
        self.read().oldest()
    }

    /// The ring's size and drop accounting.
    pub fn stats(&self) -> HistoryStats {
        let inner = self.read();
        HistoryStats {
            capacity: inner.capacity,
            frames: inner.frames.len(),
            pushed: inner.pushed,
            overruns: inner.overruns,
        }
    }

    /// The decimated samples of `ids` inside `window`.
    ///
    /// The result keeps the requested order and always has one entry per id, so
    /// a caller can zip it with its legend without re-checking membership.
    pub fn extract(
        &self,
        ids: &[String],
        window: TimeWindow,
        max_points: usize,
    ) -> Vec<SeriesPoints> {
        let inner = self.read();
        ids.iter()
            .map(|id| inner.series(id, window, max_points))
            .collect()
    }

    /// The nearest sample to `time` for each id, for a hover tooltip.
    pub fn nearest(&self, ids: &[String], time: f64) -> Vec<NearestSample> {
        let inner = self.read();
        ids.iter()
            .map(|id| NearestSample {
                id: id.clone(),
                sample: inner.nearest(id, time),
            })
            .collect()
    }

    /// The value each id holds at the newest frame.
    pub fn latest_values(&self, ids: &[String]) -> Vec<NearestSample> {
        self.read().latest_values(ids)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Inner {
    fn push(&mut self, time: f64, values: Vec<f64>) {
        if self.frames.len() == self.capacity {
            self.frames.pop_front();
            self.overruns = self.overruns.saturating_add(1);
        }
        self.frames.push_back(Arc::new(Frame { time, values }));
        self.pushed = self.pushed.saturating_add(1);
    }
}

fn range(points: &[Sample]) -> (Option<f64>, Option<f64>) {
    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;
    for sample in points {
        minimum = minimum.min(sample.v);
        maximum = maximum.max(sample.v);
    }
    if minimum.is_finite() && maximum.is_finite() {
        (Some(minimum), Some(maximum))
    } else {
        (None, None)
    }
}
