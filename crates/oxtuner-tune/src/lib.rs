//! The tune: values, images, derived write state and change tracking.
//!
//! A tune is what the user edits and what the ECU stores: one raw value per
//! calibration cell, interpreted through the definition's scale, translate,
//! shape and units. This crate owns that model and nothing else (ADR-0010):
//!
//! - **no I/O** — `.msq` files are `oxtuner-project`'s job;
//! - **no wire** — write intents are data ([`WriteIntent`]); only the core
//!   executes them (ADR-0008/0011);
//! - **no UI** — GPUI lives in `oxtuner-ui`/`oxtuner-app` (ADR-0001).
//!
//! # Layers
//!
//! ```text
//! Definition (oxtuner-ini)
//!     │  Layout::build         one frozen entry per constant / PC variable
//!     ▼
//!   Layout ──▶ Tune              raw values + edit timestamps
//!     │          │
//!     │          ├── TuneDiff    what changed, how many cells, per page
//!     │          ▼
//!     └──▶ TuneImages           baseline · working · ecu.ram · ecu.flash
//!                │
//!                ├── CellState  per cell / page / tune, ADR-0010 truth table
//!                └── StateTable one pass, O(1) cell lookup for the grid
//! ```
//!
//! # Value units
//!
//! Values are stored **raw** — the number the ECU sees. `user = (raw +
//! translate) × scale` is computed on access, so a table cell, a gauge and a
//! file writer can never disagree about the conversion. Integer storage rounds
//! half away from zero; out-of-range values are flagged, never clamped (S2 is
//! the core's guardrail, ADR-0011).
//!
//! # Page numbers
//!
//! The INI is 1-based, `.msq` files are 0-based. [`IniPage`] and [`MsqPage`]
//! are distinct types that meet only in their two conversion functions.
//!
//! # Example
//!
//! ```
//! use oxtuner_ini::{SettingsContext, parse_str};
//! use oxtuner_tune::{CellState, Tune, TuneImages};
//!
//! let definition = parse_str(
//!     "\
//! [MegaTune]
//! signature = \"speeduino 202501\"
//!
//! [Constants]
//! nPages = 1
//! pageSize = 288
//! page = 1
//! reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
//! ",
//!     &SettingsContext::new(),
//! )
//! .expect("the definition parses");
//!
//! let mut images = TuneImages::from_working(Tune::from_definition(&definition));
//! assert_eq!(images.cell_state("reqFuel", 0, 0).unwrap(), CellState::Unknown);
//!
//! let ram = images.working().snapshot();
//! images.set_ecu_ram(ram).expect("same layout");
//! assert_eq!(images.cell_state("reqFuel", 0, 0).unwrap(), CellState::InRam);
//!
//! images.working_mut().set_user("reqFuel", 12.3).expect("known variable");
//! assert_eq!(images.working().scalar_raw("reqFuel").unwrap(), 123.0);
//! ```

#![forbid(unsafe_code)]

mod diff;
mod images;
mod intent;
mod layout;
mod page;
mod tune;
mod value;
mod wire;

pub use diff::TuneDiff;
pub use images::{
    CellState, CellValue, Conflict, PageState, StateTable, TuneImages, TuneStatus, conflict_names,
};
pub use intent::{IntentError, PageSet, WriteIntent};
pub use layout::{
    EntryShape, IssueSeverity, Layout, LayoutIssue, LayoutIssueKind, Owner, PageLayout,
    VariableSpec,
};
pub use page::{IniPage, MsqPage};
pub use tune::{EditOutcome, Tune, TuneError};
pub use value::{AttributeError, RawConversion, Values, is_integer, storage_range};
pub use wire::{DecodeReport, decode_page, encode_page};
