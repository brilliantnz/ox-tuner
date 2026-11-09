//! The frozen definition model: what the rest of ox-tuner consumes.
//!
//! Everything here is plain data with no GPUI, no I/O and no ECU knowledge.
//! The parser builds it; nothing mutates it afterwards.
//!
//! Order is preserved wherever the dialect makes it meaningful. `[Constants]`
//! load order drives calibration-file field order, so a `HashMap` alone would
//! lose information that later crates need.

mod channels;
mod commands;
mod constants;
mod coverage;
mod definition;
mod dialogs;
mod frontpage;
mod menus;
mod ordered;
mod pages;
mod settings;
mod types;

pub use channels::{Channel, ChannelKind, CommsSettings, ScatteredConfig};
pub use commands::{ControllerCommand, DatalogEntry, KeyAction, ReferenceTable};
pub use constants::{
    BitOption, BitRange, Constant, ConstantClass, ConstantFlags, Offset, PcVariable,
};
pub use coverage::{Coverage, Extensions, SectionCoverage, UnsupportedSection};
pub use definition::Definition;
pub use dialogs::{Dialog, DialogRow, RowKind};
pub use frontpage::{FrontPage, Gauge, Indicator};
pub use menus::{Menu, MenuItem, MenuTargetKind};
pub use ordered::Ordered;
pub use pages::{Axis, Curve, Page, Table};
pub use settings::{SettingGroup, SettingOption};
pub use types::{DataType, Dimension, Endianness, Number, Shape, Text};

#[cfg(test)]
mod tests;
