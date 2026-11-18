//! Where a connection's bytes go.
//!
//! The core opens serial devices itself: that is the transport the product
//! ships against hardware. Everything else — the simulator today, a trace
//! replay or a network bridge later — is supplied by the composition root
//! through [`CustomLink`], so the session layer never depends on a demo
//! transport (ADR-0022).
//!
//! A custom link is asked for its transport at connect time, not when the
//! command is built, because building one may need the session's definition
//! (the simulator derives its page memory from it).

use std::fmt;
use std::sync::Arc;

use oxtuner_ini::Definition;
use oxtuner_transport::Transport;

use crate::contract::ProbeQuery;

/// A link the core cannot open by itself.
///
/// Implementations live in the application, next to the transport they wrap.
/// The core stores one behind an `Arc` and calls [`open`](Self::open) on its
/// I/O thread, so `open` must be cheap to call from a non-UI thread and must
/// not assume it runs more than once per connection attempt.
pub trait CustomLink: Send + Sync + 'static {
    /// A short name for the UI, the trace header and the write journal.
    fn label(&self) -> &str;

    /// Opens the transport against the session's definition.
    ///
    /// The error is shown to the user as a connect failure, so it should say
    /// what could not be opened rather than carry a debug dump.
    ///
    /// # Errors
    ///
    /// Returns the user-facing reason the link could not be opened.
    fn open(&self, definition: &Definition) -> Result<Box<dyn Transport>, String>;

    /// Opens the transport for a signature probe group, which has no
    /// definition.
    ///
    /// A group asks its queries over one open port, so the transport must be
    /// able to answer any of them: the families arrive in the core's own
    /// vocabulary (the protocol spec they imply is one
    /// [`crate::FirmwareProbe::spec`] call away), and each framing is the one
    /// the core will put that query in.
    ///
    /// The core stops at the first answer. A query the transport cannot answer
    /// must stay silent, like a read timeout, not fail: a read or link error
    /// ends the group and hides the query that would have answered.
    ///
    /// A link that can only answer with a definition says so, and the probe
    /// reports the reason instead of pretending to have opened a session.
    ///
    /// # Errors
    ///
    /// Returns the user-facing reason the probe cannot run on this link.
    fn open_for_probe(&self, _queries: &[ProbeQuery]) -> Result<Box<dyn Transport>, String> {
        Err(format!(
            "{} cannot answer a signature probe without a definition",
            self.label()
        ))
    }
}

/// Where a connection's bytes go.
#[expect(
    clippy::exhaustive_enums,
    reason = "a link is either a serial device or an application-supplied transport"
)]
#[derive(Clone)]
pub enum PortChoice {
    /// A serial device at a baud rate.
    Serial {
        /// Device path (`/dev/cu.usbmodem21201`, `COM3`).
        port: String,
        /// Bits per second.
        baud: u32,
    },
    /// A link the application builds.
    Custom(Arc<dyn CustomLink>),
}

impl PortChoice {
    /// A serial port at a baud rate.
    pub fn serial(port: impl Into<String>, baud: u32) -> Self {
        Self::Serial {
            port: port.into(),
            baud,
        }
    }

    /// A link the application builds.
    pub fn custom(link: impl CustomLink) -> Self {
        Self::Custom(Arc::new(link))
    }

    /// The label the UI shows for this choice.
    pub fn label(&self) -> String {
        match self {
            Self::Serial { port, .. } => port.clone(),
            Self::Custom(link) => link.label().to_owned(),
        }
    }

    /// Whether this is a serial device rather than an application link.
    pub fn is_serial(&self) -> bool {
        matches!(self, Self::Serial { .. })
    }
}

impl fmt::Debug for PortChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serial { port, baud } => formatter
                .debug_struct("Serial")
                .field("port", port)
                .field("baud", baud)
                .finish(),
            Self::Custom(link) => formatter
                .debug_tuple("Custom")
                .field(&link.label())
                .finish(),
        }
    }
}

impl PartialEq for PortChoice {
    /// Two choices are the same destination when they name the same device or
    /// the same custom link. A custom link is compared by label: two simulators
    /// with different fault rules are still the same place on screen.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Serial { port, baud },
                Self::Serial {
                    port: other_port,
                    baud: other_baud,
                },
            ) => port == other_port && baud == other_baud,
            (Self::Custom(link), Self::Custom(other_link)) => {
                Arc::ptr_eq(link, other_link) || link.label() == other_link.label()
            }
            (Self::Serial { .. }, Self::Custom(_)) | (Self::Custom(_), Self::Serial { .. }) => {
                false
            }
        }
    }
}
