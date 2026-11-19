//! The frozen definition model: what the rest of ox-tuner consumes.
//!
//! Everything here is plain data with no GPUI, no I/O and no ECU knowledge.
//! The parser builds it; nothing mutates it afterwards.
//!
//! Order is preserved wherever the dialect makes it meaningful. `[Constants]`
//! load order drives calibration-file field order, so a `HashMap` alone would
//! lose information that later crates need.
//!
//! # Frozen field sets
//!
//! The model structs carry no `#[non_exhaustive]`. A definition is plain data
//! that the parser alone builds, `serde` round-trips into the project cache,
//! and every consumer reads. Adding a field is therefore a breaking change for
//! downstream crates, and the field set stays frozen until the first published
//! release. The enums next to these structs opt out of `clippy`'s exhaustive
//! lints for the same reason, so a new variant cannot arrive unnoticed either.

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
mod sections;
mod settings;
mod types;

pub use channels::{Channel, ChannelKind, CommsSettings, CommsSource, ScatteredConfig};
pub use commands::{ControllerCommand, DatalogEntry, KeyAction, ReferenceTable};
pub use constants::{
    BitOption, BitRange, Constant, ConstantClass, ConstantFlags, Offset, PcVariable,
};
pub use coverage::{Coverage, Extensions, SectionCoverage, UnsupportedSection};
pub use definition::Capabilities;
pub use definition::Definition;
pub use dialogs::{Dialog, DialogRow, RowClassification, RowKind};
pub use frontpage::{FrontPage, Gauge, Indicator};
pub use menus::{Menu, MenuItem, MenuTargetKind};
pub use ordered::Ordered;
pub use pages::{Axis, Curve, Page, Table};
pub use sections::{ConfigEntry, ConfigSection, LoggerDefinition, LoggerField};
pub use settings::{SettingGroup, SettingOption};
pub use types::{DataType, Dimension, Endianness, Number, Shape, Text};
pub(crate) use types::{clear_text_cache, intern_text};

#[cfg(test)]
mod tests;
