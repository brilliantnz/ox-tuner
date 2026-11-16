//! Port presence, sampled by the core loop.
//!
//! The core owns the list because it decides connect, reconnect and auto-connect.
//! Sampling happens on the existing command loop rather than on a thread of its
//! own: the deadline is one more reason to wake, and there is nothing to start,
//! interrupt or join.

use std::time::{Duration, Instant};

use oxtuner_transport::PortInfo;

use crate::contract::PortView;

/// How often the port list is re-read.
///
/// One enumeration costs about a millisecond on a machine with a handful of
/// ports, so this is well under a percent of one core. It is the whole cost of
/// noticing a plug: detection is bounded by this interval.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The last list the core published, and when to look again.
#[derive(Debug)]
pub(crate) struct PortsWatch {
    list: Vec<PortView>,
    next_at: Instant,
    primed: bool,
}

impl PortsWatch {
    /// A watch that will sample on its first chance.
    pub(crate) fn new() -> Self {
        Self {
            list: Vec::new(),
            next_at: Instant::now(),
            primed: false,
        }
    }

    /// The current list, which is what a command answers from.
    ///
    /// Answering from the snapshot rather than enumerating is what keeps a
    /// dialog and the last event from disagreeing.
    pub(crate) fn snapshot(&self) -> &[PortView] {
        &self.list
    }

    /// How long until the next sample is due.
    pub(crate) fn remaining(&self) -> Duration {
        self.next_at.saturating_duration_since(Instant::now())
    }

    /// Whether a sample is due now.
    pub(crate) fn due(&self) -> bool {
        Instant::now() >= self.next_at
    }

    /// Moves the schedule on after a sample.
    pub(crate) fn polled(&mut self) {
        self.next_at = Instant::now() + POLL_INTERVAL;
    }

    /// Whether anything has been sampled yet.
    pub(crate) fn is_primed(&self) -> bool {
        self.primed
    }

    /// Re-reads the ports.
    ///
    /// Returns the new list when it differs from the last one, and `None` for the
    /// first sample: the ports present at start-up are a baseline, not arrivals.
    pub(crate) fn sample(&mut self) -> Option<Vec<PortView>> {
        let Ok(ports) = crate::io_thread::list_ports() else {
            return None;
        };

        let next: Vec<PortView> = ports.iter().map(view).collect();
        let changed = next != self.list;
        let was_primed = self.primed;

        self.list = next;
        self.primed = true;

        (changed && was_primed).then(|| self.list.clone())
    }
}

/// Turns one enumerated port into the shape the UI receives.
pub(crate) fn view(port: &PortInfo) -> PortView {
    let usb = port.usb();

    PortView {
        name: port.path().to_owned(),
        label: usb
            .and_then(|identity| board_name(identity.vid(), identity.pid()))
            .map_or_else(|| port.label().to_owned(), str::to_owned),
        vid: usb.map(oxtuner_transport::UsbIdentity::vid),
        pid: usb.map(oxtuner_transport::UsbIdentity::pid),
        serial: usb
            .and_then(oxtuner_transport::UsbIdentity::serial)
            .map(str::to_owned),
    }
}

/// Names for the boards and probes this build recognises.
///
/// The product string is missing on many boards and changes with the topology on
/// others, so a known vendor and product pair is the only reliable label. Probes
/// are named as such, because a programmer exposes a serial port too and must not
/// look like an ECU.
fn board_name(vid: u16, pid: u16) -> Option<&'static str> {
    match (vid, pid) {
        (0x2341, 0x0042) => Some("Arduino Mega 2560"),
        (0x2341, 0x0010) => Some("Arduino Mega 2560 (bootloader)"),
        (0x2341, 0x0043) => Some("Arduino Uno"),
        (0x1A86, 0x7523) => Some("CH340 serial"),
        (0x0403, 0x6001) => Some("FTDI FT232R"),
        (0x10C4, 0xEA60) => Some("CP2102 serial"),
        (0x0483, 0x3752) => Some("ST-Link (not an ECU)"),
        _ => None,
    }
}
