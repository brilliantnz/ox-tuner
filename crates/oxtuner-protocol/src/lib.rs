//! Commands and framing for an ECU, derived from its definition.
//!
//! A [`Protocol`] turns typed requests into bytes on an
//! [`oxtuner_transport::Transport`] and back again. Everything it needs about
//! the firmware's message set comes from the frozen [`oxtuner_ini::Definition`] — command templates, page sizes, the blocking factor,
//! delays and the realtime block — because that is where the user's own INI
//! says how the ECU talks.
//!
//! ```text
//! Definition ──▶ MsProtocol ──▶ Transport ──▶ ECU
//!                    ▲
//!              EcuBehavior (only what an INI cannot say)
//! ```
//!
//! # The two protocols Speeduino speaks
//!
//! - [`LinkMode::Legacy`] — a command byte, its arguments, then an unframed
//!   reply. This is the path verified read-only on the bench NO2C (P4).
//! - [`LinkMode::Envelope`] — the same command bytes inside Speeduino's
//!   `msEnvelope_1.0` frame: a two-byte big-endian length, the payload, and a
//!   CRC-32. `TunerStudio` 3 negotiates this; it is implemented here and tested
//!   against the simulator, but the legacy path stays the default until the
//!   envelope is verified on hardware (M6).
//!
//! # No retries
//!
//! The protocol never retries a conversation on its own. A timeout or a bad
//! frame is reported; the caller decides what it means.
//!
//! # Example
//!
//! ```
//! use oxtuner_ini::{SettingsContext, parse_str};
//! use oxtuner_protocol::{EcuBehavior, MsProtocol, Protocol, SpeeduinoBehavior};
//!
//! let definition = parse_str(
//!     "\
//! [MegaTune]
//! signature = \"speeduino 202501\"
//! queryCommand = \"Q\"
//!
//! [Constants]
//! nPages = 1
//! pageSize = 4
//! page = 1
//! reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
//! ",
//!     &SettingsContext::new(),
//! )
//! .expect("the definition parses");
//!
//! let behavior = SpeeduinoBehavior;
//! assert!(matches!(
//!     behavior.matches(&definition),
//!     oxtuner_protocol::Match::Exact
//! ));
//! let protocol = MsProtocol::new(&definition, &behavior).expect("the definition is complete enough");
//! assert_eq!(protocol.pages().len(), 1);
//! ```

#![forbid(unsafe_code)]

mod behavior;
mod crc32;
mod error;
mod frame;
mod ms;
mod pattern;
mod probe;

pub use behavior::{
    BehaviorKind, BurnPlan, Crc, Crc32Variant, CrcReply, EcuBehavior, FomeBehavior, Match,
    MsCompatibleBehavior, PageSwitch, ReplyCode, RusefiBehavior, SpeeduinoBehavior,
};
pub use crc32::crc32;
pub use error::ProtocolError;
pub use frame::{Frame, FrameDecoder, FrameError};
pub use ms::{
    LinkMode, MsProtocol, PROBE_PORT_SETTLE, PageSpec, Protocol, RawCommandReport, RealtimeSpec,
};
pub use oxtuner_ini::WireOrder;
pub use pattern::{CommandPattern, PatternMatch, page_identifier};
pub use probe::{COMMON_BAUDS, PROBES, ProbeSpec, RUSEFI, SPEEDUINO};
