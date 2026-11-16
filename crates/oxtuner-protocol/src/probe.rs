//! What a probe needs to ask an unknown ECU for its signature.
//!
//! Autodetecting an ECU needs three protocol facts before any definition
//! exists: which query byte the family answers and how the reply is framed.
//! Both are protocol knowledge, so they live here as constants rather than as a
//! definition of our own. The reply shape comes from the family's behavior,
//! which already owns that fact. A probe carries no calibration data and never
//! becomes project data.
//!
//! The order matters. A MegaSquirt-compatible board answers `Q`; a rusEFI board
//! answers `S`. Trying both on a port answers which family is attached without
//! the user picking a definition first.
//!
//! The registry resolves the real signature to the family's own `.ini`
//! afterwards, so a probe is only ever the first question.

use std::time::Duration;

use crate::{BehaviorKind, LinkMode, RusefiBehavior, SpeeduinoBehavior};

/// How long a probe lets the port settle before it asks.
///
/// Opening a serial port resets an Arduino into its bootloader, which ignores
/// the query for about a second. A probe has no INI to state
/// `delayAfterPortOpen`, so the settle is protocol knowledge.
pub const PROBE_PORT_SETTLE: Duration = Duration::from_millis(1_000);

/// The baud rates a scan tries, in order.
///
/// The user does not choose these: a detector that asks is a detector that
/// fails. 115200 is the Speeduino/rusEFI rate, then the common `MegaSquirt`
/// rates. The list is short on purpose; every silent port costs a timeout per
/// rate.
pub const COMMON_BAUDS: [u32; 4] = [115_200, 57_600, 38_400, 9_600];

/// One signature probe: a family, the byte that asks for its signature, and
/// how that answer is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProbeSpec {
    /// The family the probe asks for, and the key logs and the detection
    /// result use.
    pub behavior: BehaviorKind,
    /// The exact query bytes.
    pub query: &'static [u8],
    /// The framing the probe sends with.
    pub link: LinkMode,
}

impl ProbeSpec {
    /// The behavior's stable id, for logs and the detection result.
    #[must_use]
    pub fn behavior_id(self) -> &'static str {
        self.behavior.as_dyn().id()
    }

    /// The same probe over another framing.
    ///
    /// A scan may try a family over both links, so the shape stays the same and
    /// only the framing changes.
    #[must_use]
    pub fn with_link(self, link: LinkMode) -> Self {
        Self { link, ..self }
    }
}

/// MegaSquirt-compatible boards, including Speeduino, answer `Q`.
///
/// The scan tries the legacy framing first because that is the path verified on
/// hardware, and a firmware that also speaks `msEnvelope_1.0` still answers the
/// plain query.
pub const SPEEDUINO: ProbeSpec = ProbeSpec {
    behavior: BehaviorKind::Speeduino(SpeeduinoBehavior),
    query: b"Q",
    link: LinkMode::Legacy,
};

/// rusEFI and FOME answer `S`, and reply with the data alone.
pub const RUSEFI: ProbeSpec = ProbeSpec {
    behavior: BehaviorKind::Rusefi(RusefiBehavior),
    query: b"S",
    link: LinkMode::Legacy,
};

/// The probes tried on each port, in order.
pub const PROBES: [ProbeSpec; 2] = [SPEEDUINO, RUSEFI];

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use oxtuner_transport::{Transport, TransportError};

    use super::*;
    use crate::{Frame, MsProtocol, Protocol, ReplyCode};

    /// A transport that answers the first write with one canned reply and
    /// records the timeout it was given.
    struct Answer {
        sent: Vec<u8>,
        reply: Vec<u8>,
        timeout: Option<Duration>,
    }

    impl Transport for Answer {
        fn open(&mut self) -> Result<(), TransportError> {
            Ok(())
        }

        fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
            self.sent.extend_from_slice(bytes);
            Ok(())
        }

        fn read_exact_timeout(
            &mut self,
            buf: &mut [u8],
            timeout: Duration,
        ) -> Result<(), TransportError> {
            self.timeout = Some(self.timeout.map_or(timeout, |seen| seen.max(timeout)));
            if buf.len() > self.reply.len() {
                return Err(TransportError::Timeout {
                    got: self.reply.len(),
                    expected: buf.len(),
                });
            }
            buf.copy_from_slice(&self.reply[..buf.len()]);
            self.reply.drain(..buf.len());
            Ok(())
        }

        fn read_until(
            &mut self,
            sink: &mut Vec<u8>,
            max: usize,
            terminator: u8,
            _quiet: Duration,
            timeout: Duration,
        ) -> Result<(), TransportError> {
            self.timeout = Some(self.timeout.map_or(timeout, |seen| seen.max(timeout)));
            let end = self
                .reply
                .iter()
                .position(|byte| *byte == terminator)
                .map_or(self.reply.len(), |index| index + 1);

            let take = end.min(max);
            sink.extend_from_slice(&self.reply[..take]);
            self.reply.drain(..take);
            Ok(())
        }

        fn read_available(&mut self, _buf: &mut [u8]) -> Result<usize, TransportError> {
            Ok(0)
        }

        fn flush_input(&mut self) {}

        fn close(&mut self) {}
    }

    fn answer(text: &[u8]) -> Answer {
        let mut reply = text.to_vec();
        reply.push(0);
        Answer {
            sent: Vec::new(),
            reply,
            timeout: None,
        }
    }

    #[test]
    fn every_probe_names_a_family_and_a_query() {
        for probe in PROBES {
            let key = probe.behavior_id();

            assert!(!key.is_empty(), "a probe needs a key for logs");
            assert!(!probe.query.is_empty(), "{key} needs a query byte");
        }
    }

    #[test]
    fn the_probes_query_different_commands() {
        assert_eq!(SPEEDUINO.query, b"Q");
        assert_eq!(RUSEFI.query, b"S");
        assert_ne!(SPEEDUINO.query, RUSEFI.query);
    }

    #[test]
    fn the_probes_ask_for_the_family_they_name() {
        assert!(matches!(SPEEDUINO.behavior, BehaviorKind::Speeduino(_)));
        assert!(matches!(RUSEFI.behavior, BehaviorKind::Rusefi(_)));
        assert_eq!(SPEEDUINO.behavior_id(), "speeduino");
        assert_eq!(RUSEFI.behavior_id(), "rusefi");
    }

    #[test]
    fn a_probe_reads_a_signature_without_a_definition() {
        let mut io = answer(b"speeduino 202501");
        let mut protocol = MsProtocol::for_probe(&SPEEDUINO);

        let signature = protocol.signature(&mut io).expect("the probe reads");

        assert_eq!(signature, "speeduino 202501");
        assert_eq!(io.sent, b"Q", "the probe sends the family's query byte");
        assert_eq!(protocol.behavior_id(), "speeduino");
        assert_eq!(protocol.link_mode(), LinkMode::Legacy);
    }

    #[test]
    fn the_rusefi_probe_sends_its_own_byte() {
        let mut io = answer(b"rusEFI master.2026.10.21.f407");
        let mut protocol = MsProtocol::for_probe(&RUSEFI);

        let signature = protocol.signature(&mut io).expect("the probe reads");

        assert_eq!(signature, "rusEFI master.2026.10.21.f407");
        assert_eq!(io.sent, b"S");
    }

    #[test]
    fn a_probe_carries_its_family_reply_shape() {
        let speeduino = MsProtocol::for_probe(&SPEEDUINO);
        let rusefi = MsProtocol::for_probe(&RUSEFI);

        assert_eq!(
            SPEEDUINO.behavior.as_dyn().default_reply_code(),
            ReplyCode::Leading
        );
        assert_eq!(
            RUSEFI.behavior.as_dyn().default_reply_code(),
            ReplyCode::Absent
        );
        // The spec does not restate the shape; the protocol takes it from the
        // family, so the two cannot disagree.
        assert_eq!(speeduino.reply_code(), ReplyCode::Leading);
        assert_eq!(rusefi.reply_code(), ReplyCode::Absent);
    }

    #[test]
    fn the_caller_timeout_reaches_the_transport() {
        let mut io = answer(b"speeduino 202501");
        let mut protocol = MsProtocol::for_probe(&SPEEDUINO);
        let short = Duration::from_millis(350);

        protocol
            .signature_with_timeout(&mut io, short)
            .expect("the probe reads");

        assert_eq!(io.timeout, Some(short), "the short timeout is used");
    }

    #[test]
    fn a_probe_waits_for_the_port_to_settle() {
        let protocol = MsProtocol::for_probe(&SPEEDUINO);

        assert_eq!(
            protocol.port_open_delay(),
            PROBE_PORT_SETTLE,
            "opening an Arduino resets it, so the probe spends the settle"
        );
    }

    #[test]
    fn a_framed_probe_reads_through_the_reply_code() {
        // A leading non-printable byte is a refusal code for a family that
        // leads with one, and plain data for a family that sends the data
        // alone. Same bytes, two families, two answers.
        let payload = [1u8, b'x'];

        let mut leading = Answer {
            sent: Vec::new(),
            reply: Frame::encode(&payload).expect("the reply frames"),
            timeout: None,
        };
        let mut speeduino = MsProtocol::for_probe(&SPEEDUINO.with_link(LinkMode::Envelope));
        let short = Duration::from_millis(350);

        let refused = speeduino.signature_with_timeout(&mut leading, short);

        assert!(
            matches!(
                refused,
                Err(crate::ProtocolError::ReturnCode { code: 1, .. })
            ),
            "{refused:?}"
        );
        assert_eq!(leading.timeout, Some(short), "the caller's timeout is used");

        let mut absent = Answer {
            sent: Vec::new(),
            reply: Frame::encode(&payload).expect("the reply frames"),
            timeout: None,
        };
        let mut rusefi = MsProtocol::for_probe(&RUSEFI.with_link(LinkMode::Envelope));

        let read = rusefi.signature_with_timeout(&mut absent, short);

        assert!(
            matches!(read, Err(crate::ProtocolError::UnexpectedReply(_))),
            "a data-only family reads the byte as data and finds no signature: {read:?}"
        );
    }

    #[test]
    fn a_framed_probe_of_a_data_only_family_keeps_the_text_whole() {
        let spec = RUSEFI.with_link(LinkMode::Envelope);
        let framed = Frame::encode(b"rusEFI master").expect("the reply frames");
        let mut io = Answer {
            sent: Vec::new(),
            reply: framed,
            timeout: None,
        };
        let mut protocol = MsProtocol::for_probe(&spec);

        let signature = protocol.signature(&mut io).expect("the probe reads");

        assert_eq!(signature, "rusEFI master");
    }

    #[test]
    fn a_probe_has_no_pages_and_no_realtime_block() {
        let protocol = MsProtocol::for_probe(&SPEEDUINO);

        assert_eq!(protocol.pages().len(), 0);
        assert!(protocol.realtime().is_none());
    }

    #[test]
    fn the_common_bauds_start_at_the_speeduino_rate() {
        assert_eq!(COMMON_BAUDS[0], 115_200);
        assert!(
            COMMON_BAUDS.windows(2).all(|pair| pair[0] > pair[1]),
            "the list is tried in order: {COMMON_BAUDS:?}"
        );
    }
}
