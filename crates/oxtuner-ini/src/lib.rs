//! TunerStudio-style ECU definition (`.ini`) parsing for ox-tuner.
//!
//! Four passes, in the order ADR-0006 requires:
//!
//! ```text
//! bytes ──decode──▶ text ──preprocess──▶ logical lines ──resolve──▶ Definition
//!                        #if/#define/#set        sections, pages, commands
//! ```
//!
//! The settings context enters **before** protocol-command derivation, because
//! `COMMS_COMPAT` rewrites `burnCommand` and `mcu_stm32` rewrites
//! `blockingFactor` (ADR-0006). That ordering is what makes
//! [`SettingsContext`] an input to [`parse_str`] rather than a later adjustment.
//!
//! # Design rules this crate keeps
//!
//! - **Headless.** No GPUI, no I/O beyond the caller-supplied `#include`
//!   resolver, no ECU-specific code (ADR-0001).
//! - **Never panic.** Malformed text produces [`Diagnostic`]s with line numbers.
//!   A section or key this build does not model is *data*
//!   ([`Definition::unsupported`]), not a failure (ADR-0006 decision 4).
//! - **Never guess.** Presentation attributes stay symbolic
//!   ([`model::Number::Expression`]) because they re-evaluate at runtime;
//!   expressions that cannot be folded exactly stay unfolded.
//! - **Lightweight.** The dialect is hand-written. The runtime dependencies
//!   are `serde` (the derives that let `oxtuner-project` cache a frozen
//!   definition) and `tracing` (the phase spans around preprocessing, section
//!   parsing and validation); there is no INI or parsing dependency.
//! - **Versioned output.** [`PARSER_VERSION`] changes in a released build
//!   whenever the same bytes and settings can produce a different
//!   [`Definition`], so a definition cache written by an older parser is never
//!   served by a newer one. Before the first release it stays at 1 and stale
//!   caches are deleted instead.
//!
//! # Example
//!
//! ```
//! use oxtuner_ini::{parse_str, SettingsContext};
//!
//! let text = "\
//! [MegaTune]
//! signature = \"speeduino 202501\"
//! queryCommand = \"Q\"
//!
//! [Constants]
//! nPages = 1
//! pageSize = 288
//! page = 1
//! reqFuel = scalar, U16, 0, \"ms\", 0.1, 0, 0, 25.5, 1
//! ";
//!
//! let definition = parse_str(text, &SettingsContext::new()).expect("parses");
//! assert_eq!(definition.signature, "speeduino 202501");
//! assert_eq!(definition.constant("reqFuel").map(|c| c.page), Some(1));
//! ```

pub mod error;
pub mod expr;
pub mod fields;
pub mod model;
mod parser;
pub mod preprocess;
pub mod settings;
pub mod source;

pub use error::{Diagnostic, IniError, Severity};
pub use fields::{CommandArguments, CommandTemplate, Placeholder, Segment};
pub use model::{
    Axis, BitOption, BitRange, Channel, ChannelKind, CommsSettings, Constant, ConstantClass,
    ConstantFlags, Coverage, Curve, DataType, DatalogEntry, Definition, Dialog, DialogRow,
    Dimension, Endianness, Extensions, FrontPage, Gauge, Indicator, KeyAction, Menu, MenuItem,
    MenuTargetKind, Number, Offset, Ordered, Page, PcVariable, ReferenceTable, RowKind,
    ScatteredConfig, SectionCoverage, SettingGroup, SettingOption, Shape, Table, Text,
    UnsupportedSection,
};
pub use parser::{
    MAX_SPEC_VERSION, PARSER_VERSION, parse_bytes, parse_str, parse_str_with_includes,
};
pub use preprocess::{LogicalLine, Preprocessed, preprocess};
pub use settings::SettingsContext;

impl Definition {
    /// Parses a definition from text with the given settings context.
    ///
    /// Shorthand for [`parse_str`].
    ///
    /// # Errors
    ///
    /// Returns [`IniError`] when the text has a structural problem, such as a
    /// missing signature or an unbalanced conditional block.
    pub fn parse(text: &str, settings: &SettingsContext) -> Result<Self, IniError> {
        parse_str(text, settings)
    }

    /// Parses a definition from raw bytes, decoding lossily.
    ///
    /// Shorthand for [`parse_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`IniError`] when the decoded text has a structural problem,
    /// such as a missing signature or an unbalanced conditional block.
    pub fn parse_bytes(bytes: &[u8], settings: &SettingsContext) -> Result<Self, IniError> {
        parse_bytes(bytes, settings)
    }
}
