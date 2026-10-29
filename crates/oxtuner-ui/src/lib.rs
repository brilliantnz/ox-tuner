//! Reusable GPUI widgets for ox-tuner.
//!
//! This crate is the project's own widget layer over GPUI and GPUI Base. It
//! does not depend on `gpui-component`: widgets take a [`chart::ChartStyle`] or
//! a [`table::TableStyle`] token set from the host, draw with base primitives,
//! and expose retained state + events rather than reaching for a global theme.
//!
//! The widgets are the live chart and the tune table. Each is built as a
//! retained entity with a small public API — data in, events out — so every
//! surface embeds the same component instead of copying plot or grid code.
//! The data shaping lives in `oxtuner-core` and is GPUI-free (ADR-0001,
//! ADR-0020); this crate only turns it into paint calls.

#![forbid(unsafe_code)]

pub mod chart;
pub mod table;

pub use chart::{
    ChartConfig, ChartEvent, ChartMode, ChartSeries, ChartState, ChartStyle, PREFERRED_CHANNELS,
};
pub use table::{RealtimePoint, Table, TableEvent, TableModel, TableStyle};
