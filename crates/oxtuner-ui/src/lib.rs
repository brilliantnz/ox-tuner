//! Reusable GPUI widgets for ox-tuner.
//!
//! This crate is the project's own widget layer over GPUI and GPUI Base. The
//! chart takes a [`chart::ChartStyle`] token set from the host; the table reads
//! its chrome from the application theme and keeps only the heat ramp's own
//! colours: the value fills and the two [`table::TableStyle`] tokens the ramp
//! constrains. Both draw with base primitives and expose retained state +
//! events rather than reaching further into globals.
//!
//! The widgets are the live chart, the tune table, the tune curve, and the
//! tuning indicator strip. Each is built as a retained entity with a small
//! public API — data in, events out — so every surface embeds the same
//! component instead of copying plot or grid code. The data shaping lives in
//! `oxtuner-core` and is GPUI-free (ADR-0001, ADR-0020); this crate only turns
//! it into paint calls.
//!
//! [`dock`] and [`tabs`] are the dock skin and the tab bar, taken from
//! gpui-component (Apache-2.0, see `LICENSE-APACHE` beside this crate) so
//! the workspace owns its chrome: the tab shape, the tab strip, the panel
//! frames, the drag preview and the drop targets. The dock's behavior — the
//! layout tree, splits, drag geometry and persistence — stays in
//! `gpui_kit::base::dock`; these modules are its presentation.

#![forbid(unsafe_code)]

mod caption;

pub mod chart;
pub mod curve;
/// The dock skin.
///
/// Taken from gpui-component and kept close to the release it came from, so a
/// later upgrade can be compared against upstream instead of rewritten. That
/// is why this module opts out of the documentation and pedantic lints the
/// rest of the crate follows.
pub mod dock;
pub mod indicator;
pub mod table;
/// The tab and tab bar.
///
/// Vendored with [`dock`], under the same rule: closeness to upstream beats
/// satisfying the crate's own lints inside code we did not write.
pub mod tabs;

pub use chart::{
    ChartConfig, ChartEvent, ChartMode, ChartSeries, ChartState, ChartStyle, PREFERRED_CHANNELS,
};
pub use curve::{Curve, CurveEvent, CurveModel, CurvePoint, CurveSnapshot};
pub use indicator::{IndicatorState, IndicatorStrip, IndicatorStyle, IndicatorTone, IndicatorView};
pub use table::{RealtimePoint, Table, TableEvent, TableModel, TableStyle};
