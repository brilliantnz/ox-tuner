//! The reusable live chart.
//!
//! One component serves every live surface: the M8 five-series view, a dyno pull
//! and (later) a log replay. It owns only view state — window, pause, legend,
//! hover — and reads samples from a shared [`ChartHistory`]. Acquisition never
//! goes through it, so a slow render can only show an older window, never stall
//! the core (ADR-0014/0015).
//!
//! The public seam is an entity, not a builder: a view embeds
//! `Entity<ChartState>` and calls setters when its toolbar changes; the chart
//! emits [`ChartEvent`] when the user interacts inside it. It is reusable
//! without knowing the core, the definition or another view.
//!
//! ```text
//! ChartHistory ──extract(window, cap)──▶ ChartState ──canvas──▶ window
//!        ▲                                  │
//!        └──────── core poller ──────────────┘ (one writer, many readers)
//! ```
//!
//! Two layouts share the same data and interactions:
//!
//! - **Stacked** — one lane per series, like a datalog viewer. Each lane scales
//!   to its own values; the five-series demo uses this.
//! - **Overlay** — every visible series on one plot, each normalised to its own
//!   range. Useful when the shape matters more than the units.
//!
//! Interaction: click a legend chip to hide/show a series, drag to pan, wheel or
//! `+`/`-` to zoom, space to pause, `0` to follow live again.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, Context, ElementId, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, PathBuilder, Pixels, Point, Render, Role,
    ScrollDelta, ScrollEvent, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    canvas, div, point, px, size,
};
use oxtuner_core::{ChartHistory, Sample, SeriesPoints, TimeWindow, nice_ticks, padded_range};

/// Channel names the default five-series view prefers, in order.
///
/// A definition may spell the coolant channel `coolant` or `clt`; both are
/// accepted. The fallback is the first visible channels when none match.
pub const PREFERRED_CHANNELS: [&str; 14] = [
    "rpm",
    "map",
    "coolant",
    "clt",
    "tps",
    "afr",
    "ego",
    "lambda",
    "iat",
    "mat",
    "batteryvoltage",
    "baro",
    "advance",
    "fuel",
];

/// How the visible series are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChartMode {
    /// One lane per series, each with its own value scale.
    #[default]
    Stacked,
    /// One plot; each series is normalised to its own range.
    Overlay,
}

impl ChartMode {
    /// A stable label for logs and tests.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stacked => "stacked",
            Self::Overlay => "overlay",
        }
    }
}

/// Sizing and interaction limits.
#[derive(Debug, Clone)]
pub struct ChartConfig {
    mode: ChartMode,
    window_secs: f64,
    minimum_window_secs: f64,
    maximum_window_secs: f64,
    max_points: usize,
    lane_min_height: f32,
}

impl Default for ChartConfig {
    fn default() -> Self {
        Self {
            mode: ChartMode::Stacked,
            window_secs: 30.0,
            minimum_window_secs: 5.0,
            maximum_window_secs: 120.0,
            max_points: 1_500,
            lane_min_height: 54.0,
        }
    }
}

impl ChartConfig {
    /// A configuration with the defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// The initial layout.
    pub fn with_mode(mut self, mode: ChartMode) -> Self {
        self.mode = mode;
        self
    }

    /// The initial history window in seconds.
    pub fn with_window(mut self, seconds: f64) -> Self {
        self.window_secs = seconds.clamp(self.minimum_window_secs, self.maximum_window_secs);
        self
    }

    /// The point cap per series per frame.
    pub fn with_max_points(mut self, points: usize) -> Self {
        self.max_points = points.max(64);
        self
    }

    /// The minimum height of one stacked lane.
    pub fn with_lane_min_height(mut self, height: f32) -> Self {
        self.lane_min_height = height.max(32.0);
        self
    }

    /// The current layout.
    pub fn mode(&self) -> ChartMode {
        self.mode
    }

    /// The current window length.
    pub fn window_secs(&self) -> f64 {
        self.window_secs
    }
}

/// The visual tokens the chart paints with.
///
/// The widget crate does not read a global theme: the host supplies the colours
/// and corner radius, so `oxtuner-ui` stays a custom widget layer over GPUI
/// rather than a second theme owner. The host's theme decides the values.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartStyle {
    /// Window background behind the lane cards.
    pub background: Hsla,
    /// Card surface.
    pub surface: Hsla,
    /// Slightly raised surface for selected controls.
    pub surface_alt: Hsla,
    /// Primary text.
    pub foreground: Hsla,
    /// Axis labels and secondary text.
    pub muted: Hsla,
    /// Card border and separators.
    pub border: Hsla,
    /// Grid lines.
    pub grid: Hsla,
    /// Tooltip surface.
    pub popover: Hsla,
    /// Tooltip text.
    pub popover_foreground: Hsla,
    /// Selection and focus accent.
    pub accent: Hsla,
    /// Failure or drop warning.
    pub danger: Hsla,
    /// Series palette. The chart cycles through it; add colours for more
    /// channels.
    pub series: Vec<Hsla>,
    /// Card corner radius.
    pub radius: Pixels,
}

impl Default for ChartStyle {
    fn default() -> Self {
        let series = vec![
            Hsla {
                h: 0.55,
                s: 0.80,
                l: 0.56,
                a: 1.0,
            },
            Hsla {
                h: 0.11,
                s: 0.86,
                l: 0.58,
                a: 1.0,
            },
            Hsla {
                h: 0.00,
                s: 0.72,
                l: 0.58,
                a: 1.0,
            },
            Hsla {
                h: 0.38,
                s: 0.65,
                l: 0.50,
                a: 1.0,
            },
            Hsla {
                h: 0.76,
                s: 0.67,
                l: 0.63,
                a: 1.0,
            },
        ];
        Self {
            background: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.10,
                a: 1.0,
            },
            surface: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.135,
                a: 1.0,
            },
            surface_alt: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.19,
                a: 1.0,
            },
            foreground: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.93,
                a: 1.0,
            },
            muted: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.58,
                a: 1.0,
            },
            border: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.24,
                a: 1.0,
            },
            grid: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.22,
                a: 1.0,
            },
            popover: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.17,
                a: 1.0,
            },
            popover_foreground: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.94,
                a: 1.0,
            },
            accent: Hsla {
                h: 0.55,
                s: 0.80,
                l: 0.56,
                a: 1.0,
            },
            danger: Hsla {
                h: 0.00,
                s: 0.72,
                l: 0.58,
                a: 1.0,
            },
            series,
            radius: px(8.),
        }
    }
}

impl ChartStyle {
    /// The colour for a series by selection order.
    pub fn color(&self, index: usize) -> Hsla {
        if self.series.is_empty() {
            return self.accent;
        }
        self.series[index % self.series.len()]
    }
}

/// One series as the chart currently shows it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct ChartSeries {
    /// Channel id (the definition's exact name).
    pub id: String,
    /// Display label.
    pub label: String,
    /// Display units.
    pub units: String,
    /// Series colour.
    pub color: Hsla,
    /// Whether the legend has it switched on.
    pub visible: bool,
}

/// What the chart asks its owner to do or know.
#[derive(Debug, Clone, PartialEq)]
pub enum ChartEvent {
    /// A legend chip was clicked.
    VisibilityChanged {
        /// Channel id.
        id: String,
        /// New visibility.
        visible: bool,
    },
    /// The time window changed by zoom or pan.
    WindowChanged {
        /// Seconds of history shown.
        seconds: f64,
    },
    /// Pause was toggled.
    PausedChanged {
        /// Whether the view is frozen.
        paused: bool,
    },
    /// The layout changed.
    ModeChanged {
        /// The new layout.
        mode: ChartMode,
    },
}

impl EventEmitter<ChartEvent> for ChartState {}

/// The retained chart view.
pub struct ChartState {
    history: ChartHistory,
    config: ChartConfig,
    style: ChartStyle,
    /// Channels the owner selected; empty means "use the defaults".
    selected: Vec<String>,
    hidden: HashSet<String>,
    paused: bool,
    paused_at: Option<f64>,
    pan_secs: f64,
    autoscale: bool,
    normalize_overlay: bool,
    locked_ranges: HashMap<String, (f64, f64)>,
    hover: Option<Point<Pixels>>,
    dragging: Option<(f32, f64)>,
    /// The whole chart body, which is also the tooltip's positioning frame.
    body_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The plot rect inside the body. The lanes and the overlay inset it for the
    /// card padding and the tick-label column, so the cursor and the trace only
    /// agree when the hover fraction is measured against this rect.
    plot_bounds: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
}

impl ChartState {
    /// A chart over `history` with the default five-series selection.
    pub fn new(history: ChartHistory, cx: &mut Context<Self>) -> Self {
        Self::with_config(history, ChartConfig::default(), cx)
    }

    /// A chart with explicit sizing and an initial layout.
    pub fn with_config(history: ChartHistory, config: ChartConfig, cx: &mut Context<Self>) -> Self {
        Self {
            history,
            config,
            style: ChartStyle::default(),
            selected: Vec::new(),
            hidden: HashSet::new(),
            paused: false,
            paused_at: None,
            pan_secs: 0.0,
            autoscale: true,
            normalize_overlay: true,
            locked_ranges: HashMap::new(),
            hover: None,
            dragging: None,
            body_bounds: Rc::new(Cell::new(Bounds::new(
                point(px(0.), px(0.)),
                size(px(0.), px(0.)),
            ))),
            plot_bounds: Rc::new(Cell::new(Bounds::new(
                point(px(0.), px(0.)),
                size(px(0.), px(0.)),
            ))),
            focus: cx.focus_handle(),
        }
    }

    /// Replaces the shared history (a new source, e.g. switching to a dyno run).
    pub fn set_history(&mut self, history: ChartHistory, cx: &mut Context<Self>) {
        self.history = history;
        self.locked_ranges.clear();
        cx.notify();
    }

    /// The history this chart reads.
    pub fn history(&self) -> &ChartHistory {
        &self.history
    }

    /// The visual tokens in use.
    pub fn style(&self) -> &ChartStyle {
        &self.style
    }

    /// Replaces the visual tokens. The host normally calls this after a theme
    /// change; identical values do not repaint.
    pub fn set_style(&mut self, style: ChartStyle, cx: &mut Context<Self>) {
        if self.style != style {
            self.style = style;
            cx.notify();
        }
    }

    /// The channels the owner selected. Empty means the defaults.
    pub fn selected(&self) -> &[String] {
        &self.selected
    }

    /// Selects the series to show. Unknown names are ignored at render time and
    /// kept, so a definition reload can bring a channel back.
    pub fn set_selected(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.selected = ids;
        cx.notify();
    }

    /// Adds or removes one channel from the selection.
    ///
    /// An empty selection means "the default five"; the first toggle
    /// materialises those defaults so the user's change starts from what is on
    /// screen, not from an empty set.
    pub fn toggle_selected(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.selected = self.selected_ids();
        }
        if let Some(index) = self
            .selected
            .iter()
            .position(|existing| existing.eq_ignore_ascii_case(id))
        {
            self.selected.remove(index);
        } else {
            self.selected.push(id.to_owned());
        }
        cx.notify();
    }

    /// Whether `id` is currently selected.
    ///
    /// With no explicit selection, the five defaults count as selected so the
    /// channel picker opens with what the chart is actually drawing.
    pub fn is_selected(&self, id: &str) -> bool {
        if self.selected.is_empty() {
            return self
                .selected_ids()
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(id));
        }
        self.selected
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(id))
    }

    /// Whether the legend shows `id`.
    pub fn is_visible(&self, id: &str) -> bool {
        !self
            .hidden
            .iter()
            .any(|hidden| hidden.eq_ignore_ascii_case(id))
    }

    /// Hides or shows one series.
    pub fn set_visible(&mut self, id: &str, visible: bool, cx: &mut Context<Self>) {
        if visible {
            self.hidden
                .retain(|hidden| !hidden.eq_ignore_ascii_case(id));
        } else {
            self.hidden.insert(id.to_owned());
        }
        cx.emit(ChartEvent::VisibilityChanged {
            id: id.to_owned(),
            visible,
        });
        cx.notify();
    }

    /// Toggles one series in the legend.
    pub fn toggle_visible(&mut self, id: &str, cx: &mut Context<Self>) {
        self.set_visible(id, !self.is_visible(id), cx);
    }

    /// The current layout.
    pub fn mode(&self) -> ChartMode {
        self.config.mode
    }

    /// Switches between stacked lanes and one overlaid plot.
    pub fn set_mode(&mut self, mode: ChartMode, cx: &mut Context<Self>) {
        if self.config.mode == mode {
            return;
        }
        self.config.mode = mode;
        cx.emit(ChartEvent::ModeChanged { mode });
        cx.notify();
    }

    /// The current window length in seconds.
    pub fn window_secs(&self) -> f64 {
        self.config.window_secs
    }

    /// Sets the window length, clamped to the configured range.
    pub fn set_window_secs(&mut self, seconds: f64, cx: &mut Context<Self>) {
        let seconds = seconds.clamp(
            self.config.minimum_window_secs,
            self.config.maximum_window_secs,
        );
        if (seconds - self.config.window_secs).abs() < f64::EPSILON {
            return;
        }
        self.config.window_secs = seconds;
        cx.emit(ChartEvent::WindowChanged { seconds });
        cx.notify();
    }

    /// Whether the view is frozen.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Freezes or resumes the view. Acquisition continues either way.
    pub fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        if paused {
            self.paused_at = Some(self.window().end);
        } else {
            self.paused_at = None;
            self.pan_secs = 0.0;
        }
        cx.emit(ChartEvent::PausedChanged { paused });
        cx.notify();
    }

    /// Toggles pause.
    pub fn toggle_paused(&mut self, cx: &mut Context<Self>) {
        self.set_paused(!self.paused, cx);
    }

    /// Whether the view follows the newest sample.
    pub fn is_following(&self) -> bool {
        !self.paused && self.pan_secs <= f64::EPSILON
    }

    /// Returns to the newest sample.
    pub fn follow(&mut self, cx: &mut Context<Self>) {
        self.paused = false;
        self.paused_at = None;
        self.pan_secs = 0.0;
        cx.emit(ChartEvent::PausedChanged { paused: false });
        cx.notify();
    }

    /// Whether per-window auto-scaling is on.
    pub fn is_autoscale(&self) -> bool {
        self.autoscale
    }

    /// Switches auto-scaling. Turning it off snapshots the visible ranges.
    pub fn set_autoscale(&mut self, autoscale: bool, cx: &mut Context<Self>) {
        if self.autoscale == autoscale {
            return;
        }
        self.autoscale = autoscale;
        if autoscale {
            self.locked_ranges.clear();
        } else {
            self.snapshot_ranges();
        }
        cx.notify();
    }

    /// Whether overlay mode scales each series to its own range.
    ///
    /// Different quantities (rpm, kPa, AFR) do not share a y axis; a shared
    /// scale would flatten every channel next to rpm. With this on, each series
    /// uses its own padded range and the axis reads 0-100 %, while the tooltip
    /// still reports real values. Same-unit series should turn it off.
    pub fn is_normalized_overlay(&self) -> bool {
        self.normalize_overlay
    }

    /// Switches the overlay between a shared scale and per-series scaling.
    pub fn set_normalize_overlay(&mut self, normalize: bool, cx: &mut Context<Self>) {
        if self.normalize_overlay != normalize {
            self.normalize_overlay = normalize;
            cx.notify();
        }
    }

    /// The window the chart is showing.
    pub fn window(&self) -> TimeWindow {
        let newest = self.history.newest_time().unwrap_or(0.0);
        let end = match (self.paused, self.paused_at) {
            (true, Some(at)) => at,
            _ => newest - self.pan_secs,
        };
        // The newest sample stays on the right edge even while the session is
        // shorter than the window, so the trace grows leftwards from "now"
        // instead of sitting in the left third of an empty card.
        TimeWindow::new(end - self.config.window_secs, end)
    }

    /// Pans the window by `seconds` (positive moves back in time).
    pub fn pan_by(&mut self, seconds: f64, cx: &mut Context<Self>) {
        if self.paused {
            if let Some(at) = &mut self.paused_at {
                *at -= seconds;
            }
        } else {
            let oldest = self.history.oldest_time().unwrap_or(0.0);
            let newest = self.history.newest_time().unwrap_or(0.0);
            let maximum = (newest - oldest - self.config.window_secs).max(0.0);
            self.pan_secs = (self.pan_secs + seconds).clamp(0.0, maximum);
        }
        cx.notify();
    }

    /// Multiplies the window length by `factor` (`< 1` zooms in).
    pub fn zoom_by(&mut self, factor: f64, cx: &mut Context<Self>) {
        let seconds = self.config.window_secs * factor;
        self.set_window_secs(seconds, cx);
    }

    /// Resumes live following and restores the default window.
    pub fn reset_view(&mut self, cx: &mut Context<Self>) {
        self.paused = false;
        self.paused_at = None;
        self.pan_secs = 0.0;
        self.autoscale = true;
        self.locked_ranges.clear();
        cx.notify();
    }

    /// The series the chart would draw now, in legend order.
    pub fn series(&self, cx: &App) -> Vec<ChartSeries> {
        self.resolve_series(cx)
    }

    /// A compact status line for a host toolbar or a test.
    pub fn status(&self) -> String {
        let stats = self.history.stats();
        format!(
            "{} · {:?} · {:.0}s · {} frames{}",
            self.visible_count(),
            self.config.mode,
            self.config.window_secs,
            stats.frames,
            if stats.overruns > 0 {
                format!(" · {} dropped", stats.overruns)
            } else {
                String::new()
            }
        )
    }

    /// How many series are switched on.
    pub fn visible_count(&self) -> usize {
        self.selected_ids()
            .iter()
            .filter(|id| self.is_visible(id))
            .count()
    }

    /// The channel ids this chart will draw, after defaults and filtering.
    fn selected_ids(&self) -> Vec<String> {
        let channels = self.history.channels();
        let mut ids: Vec<String> = if self.selected.is_empty() {
            default_ids(&channels)
        } else {
            self.selected.clone()
        };
        ids.retain(|id| {
            channels
                .iter()
                .any(|channel| channel.name.eq_ignore_ascii_case(id))
        });
        if ids.is_empty() {
            ids = default_ids(&channels);
        }
        ids
    }

    fn resolve_series(&self, _cx: &App) -> Vec<ChartSeries> {
        let channels = self.history.channels();
        self.selected_ids()
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                let channel = channels
                    .iter()
                    .find(|channel| channel.name.eq_ignore_ascii_case(&id));
                let name = channel.map_or_else(|| id.clone(), |channel| channel.name.clone());
                ChartSeries {
                    label: name.clone(),
                    units: channel.map_or_else(String::new, |channel| channel.units.clone()),
                    color: self.style.color(index),
                    visible: self.is_visible(&name),
                    id: name,
                }
            })
            .collect()
    }

    fn snapshot_ranges(&mut self) {
        let window = self.window();
        for id in self.selected_ids() {
            let points =
                self.history
                    .extract(std::slice::from_ref(&id), window, self.config.max_points);
            if let Some(points) = points.first() {
                let range = range_of(points);
                self.locked_ranges.insert(id, range);
            }
        }
    }

    fn range_for(&self, id: &str, points: &SeriesPoints) -> (f64, f64) {
        if !self.autoscale
            && let Some(range) = self.locked_ranges.get(id)
        {
            return *range;
        }
        range_of(points)
    }

    /// Where the cursor is across the plot, 0 at the left edge and 1 at the
    /// right, or `None` when it is outside the plot.
    ///
    /// This is measured against the plot rect rather than the body, because the
    /// time axis and the crosshair both span the plot: the body also holds the
    /// card padding and the tick-label column, so a body fraction would put the
    /// crosshair right of the cursor and pick the wrong sample.
    fn hover_fraction(&self) -> Option<f64> {
        fraction_in(self.plot_bounds.get(), self.hover?)
    }

    /// The cursor inside the body, which is the frame the tooltip is placed in.
    fn hover_point(&self) -> Option<Point<Pixels>> {
        let hover = self.hover?;
        let bounds = self.body_bounds.get();
        Some(point(hover.x - bounds.origin.x, hover.y - bounds.origin.y))
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let stacked = self.config.mode == ChartMode::Stacked;

        h_flex()
            .gap_1()
            .items_center()
            .flex_wrap()
            .child(
                self.control("chart-stacked", "Stacked", stacked, cx, |this, cx| {
                    this.set_mode(ChartMode::Stacked, cx);
                }),
            )
            .child(
                self.control("chart-overlay", "Overlay", !stacked, cx, |this, cx| {
                    this.set_mode(ChartMode::Overlay, cx);
                }),
            )
            .child(div().w(px(1.)).h_4().bg(self.style.border))
            .child(self.control(
                "chart-pause",
                if self.paused { "Resume" } else { "Pause" },
                self.paused,
                cx,
                |this, cx| this.toggle_paused(cx),
            ))
            .child(self.control(
                "chart-follow",
                "Follow",
                self.is_following(),
                cx,
                |this, cx| this.follow(cx),
            ))
            .child(
                self.control("chart-auto", "Auto", self.autoscale, cx, |this, cx| {
                    let next = !this.autoscale;
                    this.set_autoscale(next, cx);
                }),
            )
            .when(self.config.mode == ChartMode::Overlay, |this| {
                this.child(self.control(
                    "chart-normalize",
                    "Per-series",
                    self.normalize_overlay,
                    cx,
                    |this, cx| {
                        let next = !this.normalize_overlay;
                        this.set_normalize_overlay(next, cx);
                    },
                ))
            })
            .child(self.control("chart-reset", "Reset", false, cx, |this, cx| {
                this.reset_view(cx);
            }))
            .when(self.paused, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(self.style.muted)
                        .child(SharedString::from("frozen")),
                )
            })
            .into_any_element()
    }

    fn control(
        &self,
        id: &'static str,
        label: &'static str,
        selected: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let (background, foreground, border) = if selected {
            (
                self.style.surface_alt,
                self.style.foreground,
                self.style.accent,
            )
        } else {
            (self.style.surface, self.style.muted, self.style.border)
        };
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(SharedString::from(label))
            .cursor_pointer()
            .px_2()
            .py_0p5()
            .rounded(self.style.radius)
            .border_1()
            .border_color(border)
            .bg(background)
            .text_xs()
            .text_color(foreground)
            .hover({
                let hover = self.style.surface_alt;
                move |this| this.bg(hover)
            })
            .active({
                let active = self.style.accent;
                move |this| this.border_color(active)
            })
            .on_click(cx.listener(move |this, _, _, cx| action(this, cx)))
            .child(SharedString::from(label))
            .into_any_element()
    }

    fn render_legend(&self, series: &[ChartSeries], cx: &mut Context<Self>) -> AnyElement {
        let muted = self.style.muted;
        h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .children(series.iter().enumerate().map(|(index, series)| {
                let id = series.id.clone();
                let color = series.color;
                let visible = series.visible;
                let label = if series.units.is_empty() {
                    series.label.clone()
                } else {
                    format!("{} ({})", series.label, series.units)
                };
                let dot = if visible {
                    div().size(px(8.)).rounded_full().bg(color)
                } else {
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .border_1()
                        .border_color(muted)
                };
                div()
                    .id(ElementId::Name(
                        format!("chart-legend-{index}-{}", series.id).into(),
                    ))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(if visible {
                        self.style.border
                    } else {
                        self.style.background
                    })
                    .when(visible, |this| this.bg(self.style.surface))
                    .text_xs()
                    .text_color(if visible {
                        self.style.foreground
                    } else {
                        muted
                    })
                    .cursor_pointer()
                    .child(dot)
                    .child(SharedString::from(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_visible(&id, cx);
                    }))
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn render_body(&self, series: &[ChartSeries], window: TimeWindow) -> AnyElement {
        let hover_fraction = self.hover_fraction();
        let visible: Vec<&ChartSeries> = series.iter().filter(|series| series.visible).collect();
        if visible.is_empty() {
            return div()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(self.style.muted)
                .child(SharedString::from(
                    "No series selected. Click a legend chip.",
                ))
                .into_any_element();
        }
        if self.history.stats().frames == 0 {
            return div()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(self.style.muted)
                .child(SharedString::from("Waiting for realtime data…"))
                .into_any_element();
        }

        let lanes: AnyElement = match self.config.mode {
            ChartMode::Stacked => {
                let mut lanes = v_flex().size_full().min_h_0().gap_2();
                let count = visible.len();
                for (index, series) in visible.iter().enumerate() {
                    let points = self
                        .history
                        .extract(
                            std::slice::from_ref(&series.id),
                            window,
                            self.config.max_points,
                        )
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                    let range = self.range_for(&series.id, &points);
                    lanes = lanes.child(self.render_lane(
                        series,
                        &points,
                        range,
                        window,
                        hover_fraction,
                        index + 1 == count,
                    ));
                }
                lanes.into_any_element()
            }
            ChartMode::Overlay => {
                let extracted: Vec<(ChartSeries, SeriesPoints)> = visible
                    .iter()
                    .map(|series| {
                        let points = self
                            .history
                            .extract(
                                std::slice::from_ref(&series.id),
                                window,
                                self.config.max_points,
                            )
                            .into_iter()
                            .next()
                            .unwrap_or_default();
                        ((*series).clone(), points)
                    })
                    .collect();
                self.render_overlay(&extracted, window, hover_fraction)
            }
        };

        v_flex()
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(lanes)
            .child(
                canvas(
                    {
                        let slot = self.body_bounds.clone();
                        move |bounds, _, _| slot.set(bounds)
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .size_full(),
            )
            .when_some(self.render_tooltip(series, window), |this, tooltip| {
                this.child(tooltip)
            })
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_lane(
        &self,
        series: &ChartSeries,
        points: &SeriesPoints,
        range: (f64, f64),
        window: TimeWindow,
        hover_fraction: Option<f64>,
        show_time_labels: bool,
    ) -> AnyElement {
        let style = &self.style;
        let muted = style.muted;
        let grid = style.grid;
        let border = style.border;
        let surface = style.surface;
        let color = series.color;
        let samples = points.points.clone();
        let time_window = window;
        let hover_sample =
            hover_fraction.and_then(|fraction| nearest(&samples, time_window.time_at(fraction)));
        let current = self
            .history
            .latest_values(std::slice::from_ref(&series.id))
            .into_iter()
            .next()
            .and_then(|nearest| nearest.sample);
        let label = if series.units.is_empty() {
            series.label.clone()
        } else {
            format!("{} ({})", series.label, series.units)
        };
        let readout = current.map_or_else(
            || "—".to_owned(),
            |sample| {
                format!(
                    "{}{}",
                    format_value(sample.v),
                    if series.units.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", series.units)
                    }
                )
            },
        );
        let mut ticks = nice_ticks(range.0, range.1, 4);
        if ticks.len() < 2 {
            ticks = vec![range.1, range.0];
        }
        let tick_labels: Vec<AnyElement> = ticks
            .iter()
            .rev()
            .map(|tick| {
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(format_value(*tick)))
                    .into_any_element()
            })
            .collect();
        let time_labels: Vec<AnyElement> = (0..5)
            .map(|tick| {
                let t = window.time_at(tick as f64 / 4.0);
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(format_offset(t - window.end)))
                    .into_any_element()
            })
            .collect();

        v_flex()
            .flex_1()
            .min_h(px(self.config.lane_min_height))
            .w_full()
            .gap_1()
            .p_2()
            .rounded(style.radius)
            .border_1()
            .border_color(style.border)
            .bg(style.surface)
            .child(
                h_flex()
                    .h_5()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().size(px(7.)).rounded_full().bg(color))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(style.foreground)
                                    .child(SharedString::from(label)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(style.foreground)
                            .child(SharedString::from(readout)),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h(px(24.))
                    .w_full()
                    .items_stretch()
                    .child(
                        v_flex()
                            .w(px(42.))
                            .h_full()
                            .flex_shrink_0()
                            .justify_between()
                            .items_end()
                            .pr_1()
                            .children(tick_labels),
                    )
                    .child(div().relative().flex_1().min_w_0().h_full().child({
                        let slot = self.plot_bounds.clone();
                        canvas(
                            move |bounds, _, _| {
                                slot.set(bounds);
                                CanvasMetrics {
                                    origin: bounds.origin,
                                    width: f32::from(bounds.size.width),
                                    height: f32::from(bounds.size.height),
                                }
                            },
                            move |_bounds, metrics, window, _| {
                                paint_lane(
                                    window,
                                    metrics,
                                    &samples,
                                    range,
                                    &ticks,
                                    time_window,
                                    color,
                                    grid,
                                    border,
                                    surface,
                                    hover_sample,
                                    hover_fraction,
                                );
                            },
                        )
                        .size_full()
                    })),
            )
            .when(show_time_labels, |this| {
                this.child(
                    h_flex()
                        .h_4()
                        .flex_shrink_0()
                        .child(div().w(px(42.)).flex_shrink_0())
                        .child(
                            h_flex()
                                .flex_1()
                                .justify_between()
                                .text_xs()
                                .text_color(muted)
                                .children(time_labels),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_overlay(
        &self,
        series: &[(ChartSeries, SeriesPoints)],
        window: TimeWindow,
        hover_fraction: Option<f64>,
    ) -> AnyElement {
        let style = &self.style;
        let grid = style.grid;
        let border = style.border;
        let foreground = style.foreground;
        let shared_range = {
            let mut values = Vec::new();
            for (_, points) in series {
                values.extend(points.points.iter().map(|sample| sample.v));
            }
            let range = padded_range(values);
            if range.0.is_finite() && range.1.is_finite() && range.0 < range.1 {
                range
            } else {
                (0.0, 1.0)
            }
        };
        let normalized = self.normalize_overlay;
        let plotted: Vec<(Hsla, Vec<Sample>, (f64, f64))> = series
            .iter()
            .map(|(series, points)| {
                let range = if normalized {
                    range_of(points)
                } else {
                    shared_range
                };
                (series.color, points.points.clone(), range)
            })
            .collect();
        let window_copy = window;
        let hover_samples: Vec<Option<Sample>> = plotted
            .iter()
            .map(|(_, points, _)| {
                hover_fraction.and_then(|fraction| nearest(points, window_copy.time_at(fraction)))
            })
            .collect();
        let axis_range = if normalized {
            (0.0, 100.0)
        } else {
            shared_range
        };
        let ticks = if normalized {
            vec![0.0, 25.0, 50.0, 75.0, 100.0]
        } else {
            let mut ticks = nice_ticks(axis_range.0, axis_range.1, 4);
            if ticks.len() < 2 {
                ticks = vec![axis_range.1, axis_range.0];
            }
            ticks
        };
        let tick_labels: Vec<AnyElement> = ticks
            .iter()
            .rev()
            .map(|tick| {
                let label = if normalized {
                    format!("{tick:.0}%")
                } else {
                    format_value(*tick)
                };
                div()
                    .text_xs()
                    .text_color(style.muted)
                    .child(SharedString::from(label))
                    .into_any_element()
            })
            .collect();
        let scale_label = if normalized {
            "per-series scale"
        } else {
            "shared scale"
        };
        let time_labels: Vec<AnyElement> = (0..5)
            .map(|tick| {
                let t = window.time_at(tick as f64 / 4.0);
                div()
                    .text_xs()
                    .text_color(style.muted)
                    .child(SharedString::from(format_offset(t - window.end)))
                    .into_any_element()
            })
            .collect();

        v_flex()
            .flex_1()
            .min_h(px(self.config.lane_min_height))
            .w_full()
            .gap_1()
            .p_2()
            .rounded(style.radius)
            .border_1()
            .border_color(style.border)
            .bg(style.surface)
            .child(
                h_flex()
                    .h_5()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(foreground)
                            .child(SharedString::from("Overlay")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(style.muted)
                            .child(SharedString::from(format!(
                                "{} series · {scale_label}",
                                plotted.len()
                            ))),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h(px(80.))
                    .w_full()
                    .items_stretch()
                    .child(
                        v_flex()
                            .w(px(42.))
                            .h_full()
                            .flex_shrink_0()
                            .justify_between()
                            .items_end()
                            .pr_1()
                            .children(tick_labels),
                    )
                    .child(div().relative().flex_1().min_w_0().h_full().child({
                        let slot = self.plot_bounds.clone();
                        canvas(
                            move |bounds, _, _| {
                                slot.set(bounds);
                                CanvasMetrics {
                                    origin: bounds.origin,
                                    width: f32::from(bounds.size.width),
                                    height: f32::from(bounds.size.height),
                                }
                            },
                            move |_bounds, metrics, window, _| {
                                paint_grid(window, metrics, &ticks, axis_range, grid);
                                for (color, points, range) in &plotted {
                                    paint_series(
                                        window,
                                        metrics,
                                        points,
                                        *range,
                                        window_copy,
                                        *color,
                                    );
                                }
                                if let Some(fraction) = hover_fraction {
                                    let x = metrics.origin.x + px(fraction as f32 * metrics.width);
                                    paint_crosshair(
                                        window,
                                        metrics.origin,
                                        x,
                                        metrics.height,
                                        border,
                                    );
                                }
                                for (index, sample) in hover_samples.iter().enumerate() {
                                    if let Some(sample) = sample {
                                        let (color, _, range) = &plotted[index];
                                        let x =
                                            fraction_to_px(sample.t, window_copy, metrics.width)
                                                + metrics.origin.x;
                                        let y = value_to_px(sample.v, *range, metrics.height)
                                            + metrics.origin.y;
                                        paint_dot(window, point(x, y), 3.5, *color);
                                    }
                                }
                            },
                        )
                        .size_full()
                    })),
            )
            .child(
                h_flex()
                    .h_4()
                    .flex_shrink_0()
                    .child(div().w(px(42.)).flex_shrink_0())
                    .child(
                        h_flex()
                            .flex_1()
                            .justify_between()
                            .text_xs()
                            .text_color(style.muted)
                            .children(time_labels),
                    ),
            )
            .into_any_element()
    }

    fn render_tooltip(&self, series: &[ChartSeries], window: TimeWindow) -> Option<AnyElement> {
        let point = self.hover_point()?;
        let fraction = self.hover_fraction()?;
        let time = window.time_at(fraction);
        let ids: Vec<String> = series
            .iter()
            .filter(|series| series.visible)
            .map(|series| series.id.clone())
            .collect();
        if ids.is_empty() {
            return None;
        }
        let nearest = self.history.nearest(&ids, time);
        let style = &self.style;
        let width = self.body_bounds.get().size.width;
        let open_left = f32::from(point.x) > f32::from(width) * 0.6;
        let left = if open_left {
            point.x - px(250.)
        } else {
            point.x + px(14.)
        };
        Some(
            div()
                .absolute()
                .left(left.max(px(0.)))
                .top((point.y + px(12.)).max(px(0.)))
                .w(px(236.))
                .rounded(style.radius)
                .border_1()
                .border_color(style.border)
                .bg(style.popover)
                .text_color(style.popover_foreground)
                .shadow_md()
                .p_2()
                .text_xs()
                .child(
                    div()
                        .pb_1()
                        .text_color(style.muted)
                        .child(SharedString::from(format_time(time))),
                )
                .children(nearest.iter().filter_map(|nearest| {
                    let sample = nearest.sample?;
                    let series = series
                        .iter()
                        .find(|series| series.id.eq_ignore_ascii_case(&nearest.id))?;
                    Some(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().size(px(6.)).rounded_full().bg(series.color))
                            .child(
                                div()
                                    .flex_1()
                                    .text_color(style.muted)
                                    .child(SharedString::from(series.label.clone())),
                            )
                            .child(div().font_weight(gpui_kit::FontWeight::MEDIUM).child(
                                SharedString::from(format!(
                                    "{}{}",
                                    format_value(sample.v),
                                    if series.units.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" {}", series.units)
                                    }
                                )),
                            )),
                    )
                }))
                .into_any_element(),
        )
    }
}

impl Focusable for ChartState {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChartState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let series = self.resolve_series(cx);
        let window_range = self.window();
        let legend = self.render_legend(&series, cx);
        let controls = self.render_controls(cx);
        let body = self.render_body(&series, window_range);
        let focus = self.focus.clone();
        let focused = self.focus.is_focused(window);
        let style = self.style.clone();

        v_flex()
            .id("chart")
            .size_full()
            .min_h_0()
            .gap_1()
            .bg(style.background)
            .text_color(style.foreground)
            .track_focus(&focus)
            .border_1()
            .border_color(if focused {
                style.accent
            } else {
                style.background
            })
            .role(Role::Document)
            .aria_label(SharedString::from("Realtime chart"))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let span = this.window().span();
                match event.keystroke.key.as_str() {
                    "space" => this.toggle_paused(cx),
                    "left" => this.pan_by(span * 0.1, cx),
                    "right" => this.pan_by(-span * 0.1, cx),
                    "+" | "=" => this.zoom_by(0.85, cx),
                    "-" => this.zoom_by(1.18, cx),
                    "0" => this.reset_view(cx),
                    _ => {}
                }
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .flex_wrap()
                    .child(controls)
                    .child(
                        div()
                            .text_xs()
                            .text_color(style.muted)
                            .child(SharedString::from(self.status())),
                    ),
            )
            .child(legend)
            .child(
                v_flex()
                    .id("chart-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if event.click_count >= 2 {
                                this.toggle_paused(cx);
                                return;
                            }
                            this.dragging = Some((
                                f32::from(event.position.x),
                                if this.paused {
                                    this.paused_at.unwrap_or_default()
                                } else {
                                    this.pan_secs
                                },
                            ));
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        this.hover = Some(event.position);
                        if let Some((start, start_value)) = this.dragging
                            && event.pressed_button == Some(MouseButton::Left)
                        {
                            // The drag spans the plot, not the body, otherwise the
                            // data lags the cursor by the label column's share.
                            let mut width = f32::from(this.plot_bounds.get().size.width);
                            if width <= 0.0 {
                                width = f32::from(this.body_bounds.get().size.width);
                            }
                            let width = width.max(1.0);
                            let delta = (start - f32::from(event.position.x)) / width
                                * this.window().span() as f32;
                            if this.paused {
                                this.paused_at = Some(start_value + f64::from(delta));
                            } else {
                                this.pan_by(-f64::from(delta), cx);
                            }
                        }
                        cx.notify();
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            this.dragging = None;
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            this.dragging = None;
                            cx.notify();
                        }),
                    )
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered && this.hover.is_some() {
                            this.hover = None;
                            cx.notify();
                        }
                    }))
                    .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                        let lines = match event.delta {
                            ScrollDelta::Lines(delta) => f64::from(delta.y),
                            ScrollDelta::Pixels(delta) => f64::from(delta.y) / 40.0,
                        };
                        if lines.abs() > f64::EPSILON {
                            this.zoom_by(1.0 + lines.clamp(-3.0, 3.0) * 0.12, cx);
                        }
                    }))
                    .child(body),
            )
    }
}

fn default_ids(channels: &[oxtuner_core::ChannelInfo]) -> Vec<String> {
    // Each group is one physical quantity with spelling variants. Taking the
    // first match per group keeps the default at five even when a definition
    // declares both `coolant` and `clt` or both `afr` and `ego`.
    const GROUPS: [&[&str]; 5] = [
        &["rpm"],
        &["map"],
        &["coolant", "clt"],
        &["tps"],
        &["afr", "ego", "lambda"],
    ];
    let mut ids: Vec<String> = Vec::with_capacity(5);
    for group in GROUPS {
        let found = channels.iter().find(|channel| {
            !channel.hidden
                && group
                    .iter()
                    .any(|name| channel.name.eq_ignore_ascii_case(name))
                && !ids
                    .iter()
                    .any(|id: &String| id.eq_ignore_ascii_case(&channel.name))
        });
        if let Some(channel) = found {
            ids.push(channel.name.clone());
        }
    }
    if ids.len() < 5 {
        for preferred in PREFERRED_CHANNELS {
            if ids.len() == 5 {
                break;
            }
            if let Some(channel) = channels.iter().find(|channel| {
                !channel.hidden
                    && channel.name.eq_ignore_ascii_case(preferred)
                    && !ids.iter().any(|id| id.eq_ignore_ascii_case(&channel.name))
            }) {
                ids.push(channel.name.clone());
            }
        }
    }
    if ids.is_empty() {
        ids = channels
            .iter()
            .filter(|channel| !channel.hidden)
            .take(5)
            .map(|channel| channel.name.clone())
            .collect();
    }
    ids
}

fn range_of(points: &SeriesPoints) -> (f64, f64) {
    let range = padded_range(points.points.iter().map(|sample| sample.v));
    if range.0.is_finite() && range.1.is_finite() && range.0 < range.1 {
        range
    } else {
        (0.0, 1.0)
    }
}

fn nearest(points: &[Sample], time: f64) -> Option<Sample> {
    points
        .iter()
        .min_by(|left, right| (left.t - time).abs().total_cmp(&(right.t - time).abs()))
        .copied()
}

fn fraction_to_px(time: f64, window: TimeWindow, width: f32) -> Pixels {
    px((window.fraction(time) as f32 * width).clamp(0.0, width))
}

/// Where `cursor` sits across `bounds`, 0 at the left edge and 1 at the right.
///
/// Returns `None` when the point is outside the rect or the rect has no width,
/// which also covers the frame before the plots have been measured.
fn fraction_in(bounds: Bounds<Pixels>, cursor: Point<Pixels>) -> Option<f64> {
    let width = f32::from(bounds.size.width);
    if width <= 0.0 {
        return None;
    }
    let x = f32::from(cursor.x - bounds.origin.x);
    (x >= 0.0 && x <= width).then(|| f64::from(x / width))
}

fn value_to_px(value: f64, range: (f64, f64), height: f32) -> Pixels {
    let span = (range.1 - range.0).max(f64::EPSILON);
    let fraction = (1.0 - (value - range.0) / span).clamp(0.0, 1.0);
    px(fraction as f32 * height)
}

#[derive(Clone, Copy)]
struct CanvasMetrics {
    origin: Point<Pixels>,
    width: f32,
    height: f32,
}

fn paint_grid(
    window: &mut Window,
    metrics: CanvasMetrics,
    ticks: &[f64],
    range: (f64, f64),
    grid: Hsla,
) {
    let mut builder = PathBuilder::stroke(px(1.));
    for tick in ticks {
        let y = metrics.origin.y + value_to_px(*tick, range, metrics.height);
        builder.move_to(point(metrics.origin.x, y));
        builder.line_to(point(metrics.origin.x + px(metrics.width), y));
    }
    for line in 1..4 {
        let x = metrics.origin.x + px(metrics.width * line as f32 / 4.0);
        builder.move_to(point(x, metrics.origin.y));
        builder.line_to(point(x, metrics.origin.y + px(metrics.height)));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, grid);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_lane(
    window: &mut Window,
    metrics: CanvasMetrics,
    points: &[Sample],
    range: (f64, f64),
    ticks: &[f64],
    time_window: TimeWindow,
    color: Hsla,
    grid: Hsla,
    border: Hsla,
    surface: Hsla,
    hover_sample: Option<Sample>,
    hover_fraction: Option<f64>,
) {
    paint_grid(window, metrics, ticks, range, grid);
    if let Some(fraction) = hover_fraction {
        let x = metrics.origin.x + px(fraction as f32 * metrics.width);
        paint_crosshair(window, metrics.origin, x, metrics.height, border);
    }
    paint_series(window, metrics, points, range, time_window, color);
    if let Some(sample) = hover_sample {
        let x = fraction_to_px(sample.t, time_window, metrics.width) + metrics.origin.x;
        let y = value_to_px(sample.v, range, metrics.height) + metrics.origin.y;
        paint_dot(window, point(x, y), 7.0, surface.alpha(0.45));
        paint_dot(window, point(x, y), 4.0, color);
    }
}

fn paint_series(
    window: &mut Window,
    metrics: CanvasMetrics,
    points: &[Sample],
    range: (f64, f64),
    window_range: TimeWindow,
    color: Hsla,
) {
    if points.is_empty() {
        return;
    }
    if points.len() == 1 {
        let sample = points[0];
        paint_dot(
            window,
            point(
                fraction_to_px(sample.t, window_range, metrics.width) + metrics.origin.x,
                value_to_px(sample.v, range, metrics.height) + metrics.origin.y,
            ),
            3.0,
            color,
        );
        return;
    }
    let mut builder = PathBuilder::stroke(px(1.6));
    builder.move_to(point(
        fraction_to_px(points[0].t, window_range, metrics.width) + metrics.origin.x,
        value_to_px(points[0].v, range, metrics.height) + metrics.origin.y,
    ));
    for sample in &points[1..] {
        builder.line_to(point(
            fraction_to_px(sample.t, window_range, metrics.width) + metrics.origin.x,
            value_to_px(sample.v, range, metrics.height) + metrics.origin.y,
        ));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_crosshair(
    window: &mut Window,
    origin: Point<Pixels>,
    x: Pixels,
    height: f32,
    color: Hsla,
) {
    let mut builder = PathBuilder::stroke(px(1.)).dash_array(&[px(4.), px(4.)]);
    builder.move_to(point(x, origin.y));
    builder.line_to(point(x, origin.y + px(height)));
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_dot(window: &mut Window, center: Point<Pixels>, radius: f32, color: Hsla) {
    let mut builder = PathBuilder::fill();
    let steps = 16;
    for step in 0..steps {
        let angle = std::f32::consts::TAU * step as f32 / steps as f32;
        let point = point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        );
        if step == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn format_offset(seconds: f64) -> String {
    if !seconds.is_finite() {
        return "—".to_owned();
    }
    let rounded = seconds.round();
    if rounded.abs() < 0.5 {
        return "0s".to_owned();
    }
    let magnitude = rounded.abs();
    let body = if magnitude >= 3600.0 {
        format!(
            "{}:{:02.0}:{:02.0}",
            (magnitude / 3600.0).floor(),
            (magnitude / 60.0).floor() % 60.0,
            magnitude % 60.0
        )
    } else if magnitude >= 60.0 {
        format!(
            "{:.0}:{:02.0}",
            (magnitude / 60.0).floor(),
            magnitude % 60.0
        )
    } else {
        format!("{magnitude:.0}s")
    };
    format!("{}{body}", if rounded < 0.0 { "-" } else { "+" })
}

fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() {
        return "—".to_owned();
    }
    let seconds = seconds.max(0.0);
    if seconds >= 3600.0 {
        format!(
            "{:.0}:{:02.0}:{:02.0}",
            (seconds / 3600.0).floor(),
            (seconds / 60.0).floor() % 60.0,
            seconds % 60.0
        )
    } else if seconds >= 60.0 {
        format!("{:.0}:{:02.0}", (seconds / 60.0).floor(), seconds % 60.0)
    } else {
        format!("{seconds:.0}s")
    }
}

fn format_value(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }
    if value.abs() < 0.005 {
        return "0".to_owned();
    }
    if value.abs() >= 100.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

#[cfg(test)]
mod tests;
