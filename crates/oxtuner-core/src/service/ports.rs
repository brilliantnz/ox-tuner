//! Port presence, sampled by the core loop.
//!
//! The core owns the list because it decides connect, reconnect and auto-connect.
//! Sampling happens on the existing command loop rather than on a thread of its
//! own: the deadline is one more reason to wake, and there is nothing to start,
//! interrupt or join.
//!
//! The first sample is published like any other, so a view that opens later
//! never has to ask for the initial list: it renders whatever was last
//! published and treats "nothing published yet" as still looking.

use std::time::{Duration, Instant};

use oxtuner_transport::{PortInfo, TransportError};

use crate::contract::PortView;

/// How often the port list is re-read.
///
/// One enumeration costs about a millisecond on a machine with a handful of
/// ports, so this is well under a percent of one core. It is the whole cost of
/// noticing a plug: detection is bounded by this interval.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

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

    /// Re-reads the ports, reporting an enumeration failure to the caller.
    ///
    /// Returns the new list when it differs from the last one published,
    /// including the first sample: a view that opens later renders the last
    /// published list, so the baseline must be published too. A first sample
    /// that found no ports publishes nothing, since empty equals empty.
    pub(crate) fn refresh(&mut self) -> Result<Option<Vec<PortView>>, TransportError> {
        self.refresh_with(crate::io_thread::list_ports)
    }

    /// The same, reading through a seam so the contract can be tested.
    fn refresh_with(
        &mut self,
        read: impl FnOnce() -> Result<Vec<PortInfo>, TransportError>,
    ) -> Result<Option<Vec<PortView>>, TransportError> {
        let ports = read()?;
        let mut next: Vec<PortView> = ports.iter().map(view).collect();
        // Enumeration order is not contractual, so sorting keeps a machine that
        // returns the same set in a different order from publishing a change it
        // did not have.
        next.sort_by(|left, right| left.name.cmp(&right.name));

        let publish = should_publish(&self.list, &next);

        self.list = next;
        self.primed = true;

        Ok(publish.then(|| self.list.clone()))
    }

    /// The same, for the command loop, which has nobody to report to.
    ///
    /// A failed read leaves the last list in place rather than blanking it: a
    /// transient enumeration failure is not a machine with no ports.
    pub(crate) fn sample(&mut self) -> Option<Vec<PortView>> {
        match self.refresh() {
            Ok(published) => published,
            Err(error) => {
                tracing::debug!(%error, "serial ports could not be listed");
                None
            }
        }
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

/// Whether a sample is news.
///
/// The first sample counts when it found ports: a view that opens later
/// renders the last published list, so the baseline must be published too.
/// An empty first sample publishes nothing, since empty equals empty.
fn should_publish(previous: &[PortView], next: &[PortView]) -> bool {
    previous != next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(name: &str) -> PortView {
        PortView {
            name: name.to_owned(),
            label: name.to_owned(),
            vid: Some(0x2341),
            pid: Some(0x0042),
            serial: Some("A".to_owned()),
        }
    }

    #[test]
    fn a_first_sample_with_ports_publishes_the_baseline() {
        assert!(should_publish(&[], &[port("/dev/cu.first")]));
    }

    #[test]
    fn a_first_sample_without_ports_publishes_nothing() {
        let empty: [PortView; 0] = [];
        assert!(!should_publish(&empty, &empty));
    }

    #[test]
    fn an_unchanged_list_publishes_nothing() {
        let list = [port("/dev/cu.first")];
        assert!(!should_publish(&list, &list));
    }

    #[test]
    fn a_new_port_publishes() {
        let before = [port("/dev/cu.first")];
        let after = [port("/dev/cu.first"), port("/dev/cu.second")];
        assert!(should_publish(&before, &after));
    }

    #[test]
    fn a_removed_port_publishes() {
        let before = [port("/dev/cu.first"), port("/dev/cu.second")];
        let after = [port("/dev/cu.first")];
        assert!(should_publish(&before, &after));
    }

    fn described(vid: u16, pid: u16, product: Option<&str>) -> PortInfo {
        let mut identity = oxtuner_transport::UsbIdentity::described(vid, pid).with_serial("S");

        if let Some(product) = product {
            identity = identity.with_product(product);
        }

        PortInfo::from_parts("/dev/cu.test".to_owned(), Some(identity))
    }

    #[test]
    fn a_known_board_wins_over_its_product_string() {
        let row = view(&described(0x2341, 0x0042, Some("Generic CDC")));

        assert_eq!(row.label, "Arduino Mega 2560");
        assert_eq!(row.vid, Some(0x2341));
        assert_eq!(row.serial.as_deref(), Some("S"));
    }

    #[test]
    fn an_unknown_board_falls_back_to_its_product_string() {
        let row = view(&described(0x0A0B, 0x0C0D, Some("Some UART")));

        assert_eq!(row.label, "Some UART");
    }

    #[test]
    fn a_port_without_identity_falls_back_to_its_path() {
        let row = view(&PortInfo::from_parts("/dev/cu.test".to_owned(), None));

        assert_eq!(row.label, "/dev/cu.test");
        assert_eq!(row.vid, None);
        assert_eq!(row.serial, None);
    }

    #[test]
    fn a_failed_read_keeps_the_last_list_and_the_first_sample_pending() {
        let mut watch = PortsWatch::new();

        let error = watch
            .refresh_with(|| Err(TransportError::Open("nope".to_owned())))
            .expect_err("a failed read is reported");

        assert!(matches!(error, TransportError::Open(_)));
        assert!(
            watch.snapshot().is_empty(),
            "a failed read keeps the last list"
        );
        assert!(
            !watch.is_primed(),
            "a failed read leaves the first sample pending"
        );
    }

    #[test]
    fn a_failed_read_after_a_good_one_keeps_the_list_and_stays_primed() {
        let mut watch = PortsWatch::new();
        let one = || Ok(vec![described(0x2341, 0x0042, None)]);

        watch.refresh_with(one).expect("first read");
        assert_eq!(watch.snapshot().len(), 1);

        let failed = watch.refresh_with(|| Err(TransportError::Open("nope".to_owned())));

        assert!(matches!(failed, Err(TransportError::Open(_))));
        assert_eq!(watch.snapshot().len(), 1, "the last list stays");
        assert!(watch.is_primed(), "a failure does not unprime the watch");
    }

    #[test]
    fn the_first_sample_publishes_the_baseline_and_a_change_after_it() {
        let mut watch = PortsWatch::new();
        let one = || Ok(vec![described(0x2341, 0x0042, None)]);

        let baseline = watch.refresh_with(one).expect("reads").expect("baseline");
        assert_eq!(baseline.len(), 1);
        assert_eq!(watch.snapshot().len(), 1);

        let two = || {
            Ok(vec![
                described(0x2341, 0x0042, None),
                described(0x1A86, 0x7523, None),
            ])
        };

        let published = watch.refresh_with(two).expect("reads").expect("a change");
        assert_eq!(published.len(), 2);
    }

    #[test]
    fn an_empty_first_sample_publishes_nothing_but_primes_the_watch() {
        let mut watch = PortsWatch::new();
        let none = || Ok(Vec::new());

        assert!(watch.refresh_with(none).expect("reads").is_none());
        assert!(watch.is_primed(), "even an empty sample primes the watch");
        assert_eq!(watch.snapshot().len(), 0);
    }

    fn named(name: &str, vid: u16, pid: u16) -> PortInfo {
        PortInfo::from_parts(
            name.to_owned(),
            Some(oxtuner_transport::UsbIdentity::described(vid, pid)),
        )
    }

    #[test]
    fn the_same_ports_in_another_order_publish_nothing() {
        let mut watch = PortsWatch::new();
        let first = || {
            Ok(vec![
                named("/dev/cu.first", 0x2341, 0x0042),
                named("/dev/cu.second", 0x1A86, 0x7523),
            ])
        };
        let reversed = || {
            Ok(vec![
                named("/dev/cu.second", 0x1A86, 0x7523),
                named("/dev/cu.first", 0x2341, 0x0042),
            ])
        };

        assert!(
            watch.refresh_with(first).expect("reads").is_some(),
            "the baseline is published"
        );
        assert!(
            watch.refresh_with(reversed).expect("reads").is_none(),
            "a reordering is not a change"
        );
    }

    #[test]
    fn a_board_name_is_only_given_for_known_pairs() {
        assert_eq!(board_name(0x2341, 0x0042), Some("Arduino Mega 2560"));
        assert_eq!(board_name(0x0483, 0x3752), Some("ST-Link (not an ECU)"));
        assert_eq!(board_name(0x0000, 0x0000), None);
    }
}
