//! What a probe needs to ask an unknown ECU for its signature.
//!
//! Autodetecting an ECU needs three protocol facts before any definition
//! exists: which query byte the family answers, how the reply is framed, and
//! how the reply is shaped. All three are protocol knowledge, so they live here
//! as constants rather than as a definition of our own. A probe carries no
//! calibration data and never becomes project data.
//!
//! The order matters. A MegaSquirt-compatible board answers `Q`; a rusEFI board
//! answers `S`. Trying both on a port answers which family is attached without
//! the user picking a definition first.
//!
//! The registry resolves the real signature to the family's own `.ini`
//! afterwards, so a probe is only ever the first question.

use crate::{BehaviorKind, LinkMode, ReplyCode, SpeeduinoBehavior};

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
pub struct ProbeSpec {
    /// Stable key used in logs and the detection result.
    pub name: &'static str,
    /// The family the probe asks for.
    pub behavior: BehaviorKind,
    /// The exact query bytes.
    pub query: &'static [u8],
    /// How the family shapes a framed reply.
    pub reply_code: ReplyCode,
    /// The framing the probe sends with.
    pub link: LinkMode,
}

impl ProbeSpec {
    /// The behavior's stable id, for logs and the detection result.
    #[must_use]
    pub fn behavior_id(self) -> &'static str {
        self.behavior.as_dyn().id()
    }
}

/// MegaSquirt-compatible boards, including Speeduino, answer `Q`.
///
/// The scan tries the legacy framing first because that is the path verified on
/// hardware, and a firmware that also speaks `msEnvelope_1.0` still answers the
/// plain query.
pub const SPEEDUINO: ProbeSpec = ProbeSpec {
    name: "speeduino",
    behavior: BehaviorKind::Speeduino(SpeeduinoBehavior),
    query: b"Q",
    reply_code: ReplyCode::Leading,
    link: LinkMode::Legacy,
};

/// rusEFI and FOME answer `S`, and reply with the data alone.
pub const RUSEFI: ProbeSpec = ProbeSpec {
    name: "rusefi",
    behavior: BehaviorKind::Rusefi(crate::RusefiBehavior),
    query: b"S",
    reply_code: ReplyCode::Absent,
    link: LinkMode::Legacy,
};

/// The probes tried on each port, in order.
pub const PROBES: [ProbeSpec; 2] = [SPEEDUINO, RUSEFI];

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use oxtuner_transport::{Transport, TransportError};

    use super::*;
    use crate::{MsProtocol, Protocol};

    /// A transport that answers the first write with one canned reply.
    struct Answer {
        sent: Vec<u8>,
        reply: Vec<u8>,
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
            _timeout: Duration,
        ) -> Result<(), TransportError> {
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
            _timeout: Duration,
        ) -> Result<(), TransportError> {
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
        }
    }

    #[test]
    fn every_probe_names_a_family_and_a_query() {
        for probe in PROBES {
            assert!(!probe.name.is_empty(), "a probe needs a key for logs");
            assert!(!probe.query.is_empty(), "{} needs a query byte", probe.name);
            assert_eq!(
                probe.behavior_id(),
                probe.name,
                "the key and the behavior should name the same family"
            );
        }
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
    fn a_probe_has_no_pages_and_no_realtime_block() {
        let protocol = MsProtocol::for_probe(&SPEEDUINO);

        assert_eq!(protocol.pages().len(), 0);
        assert!(protocol.realtime().is_none());
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
    }

    #[test]
    fn the_probes_carry_their_own_reply_shape() {
        assert_eq!(SPEEDUINO.reply_code, ReplyCode::Leading);
        assert_eq!(RUSEFI.reply_code, ReplyCode::Absent);
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
