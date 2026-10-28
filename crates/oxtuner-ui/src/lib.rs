//! Reusable GPUI widgets for ox-tuner.
//!
//! This crate is the project's own widget layer over GPUI and GPUI Base. It
//! does not depend on `gpui-component`: widgets take a [`chart::ChartStyle`] (or
//! an equivalent token set) from the host, draw with base primitives, and expose
//! retained state + events rather than reaching for a global theme.
//!
//! The first widget is the live chart. It is deliberately built as a retained
//! `Entity<ChartState>` with a small public API — data in, events out — so a
//! live dashboard, a dyno view and a log replay all embed the same component
//! instead of copying plot code. The data shaping lives in `oxtuner-core` and is
//! GPUI-free (ADR-0001, ADR-0020); this crate only turns it into paint calls.

#![forbid(unsafe_code)]

pub mod chart;

pub use chart::{
    ChartConfig, ChartEvent, ChartMode, ChartSeries, ChartState, ChartStyle, PREFERRED_CHANNELS,
};
