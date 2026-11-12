//! The virtual ECU as a [`Transport`].

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::Duration;

use oxtuner_ini::Definition;
use oxtuner_protocol::{BehaviorKind, Frame, ReplyCode, crc32};
use oxtuner_transport::{Transport, TransportError};

use crate::SimError;
use crate::ecu::{RealtimeEcu, Request, SimEcu};
use crate::faults::{Fault, FaultRule, Faults, SimOp};
use crate::memory::Memory;
use crate::realtime::Generator;
use crate::scenario::Scenario;

/// The longest a `Delay` fault blocks, so a scenario cannot stall CI.
const MAX_FAULT_DELAY: Duration = Duration::from_millis(500);

/// How many exchanges the transcript keeps.
///
/// The transcript is for determinism tests and debugging, not a datalog: a
/// ring of the last 10 000 exchanges bounds memory through a long session while
/// keeping more than any test compares.
const MAX_TRANSCRIPT_ENTRIES: usize = 10_000;

/// How the virtual ECU behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimConfig {
    /// Signature to answer with. `None` uses the definition's own, which is the
    /// normal case; `Some` produces the signature mismatch path.
    pub signature: Option<String>,
    /// Seed for the memory fill and the waveform's phase.
    pub seed: u64,
    /// Armed faults.
    pub faults: Vec<FaultRule>,
    /// Pages preloaded with bytes before the first read.
    pub pages: Vec<(u8, Vec<u8>)>,
    /// How many realtime reads report `burnPending` after a burn.
    ///
    /// A real board answers a burn before the EEPROM write starts, so the bit
    /// stays set for a while. Zero keeps the virtual burn instant.
    pub burn_pending_samples: u32,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            signature: None,
            seed: 0x5EED_2025,
            faults: Vec::new(),
            pages: Vec::new(),
            burn_pending_samples: 0,
        }
    }
}

impl From<Scenario> for SimConfig {
    fn from(scenario: Scenario) -> Self {
        Self {
            signature: scenario.signature,
            seed: scenario.seed.unwrap_or(0x5EED_2025),
            faults: scenario.faults,
            pages: scenario.pages,
            burn_pending_samples: 0,
        }
    }
}

/// What a session moved, for assertions and the connection summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Signature queries answered.
    pub signatures: u64,
    /// Page reads.
    pub reads: u64,
    /// Page writes.
    pub writes: u64,
    /// Burns.
    pub burns: u64,
    /// `[ControllerCommands]` actions.
    pub commands: u64,
    /// Realtime blocks.
    pub realtime: u64,
    /// Bytes written into pages.
    pub bytes_written: u64,
}

/// One request and its reply, for determinism tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// The exact bytes the host sent.
    pub request: Vec<u8>,
    /// The exact bytes the ECU queued back (empty for a dropped reply).
    pub reply: Vec<u8>,
}

/// Everything that crossed the virtual wire.
///
/// Keeps the **last** [`MAX_TRANSCRIPT_ENTRIES`] exchanges; older ones are
/// dropped, and [`Self::dropped`] says how many. Use it for assertions, not as
/// a session log — the trace recorder is the unbounded-until-capped record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    entries: VecDeque<TranscriptEntry>,
    dropped: u64,
}

impl Transcript {
    /// The retained entries, oldest first.
    pub fn entries(&self) -> impl Iterator<Item = &TranscriptEntry> + '_ {
        self.entries.iter()
    }

    /// Number of retained exchanges.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing crossed the wire.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Exchanges dropped after the ring filled.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    fn push(&mut self, entry: TranscriptEntry) {
        if self.entries.len() == MAX_TRANSCRIPT_ENTRIES {
            self.entries.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.entries.push_back(entry);
    }

    /// A deterministic text rendering: `TX`/`RX` hex, like a trace.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for entry in &self.entries {
            text.push_str("TX");
            for byte in &entry.request {
                let _ = write!(text, " {byte:02x}");
            }
            text.push('\n');
            if !entry.reply.is_empty() {
                text.push_str("RX");
                for byte in &entry.reply {
                    let _ = write!(text, " {byte:02x}");
                }
                text.push('\n');
            }
        }
        text
    }
}

/// A virtual ECU built from a real definition.
pub struct SimTransport {
    ecu: SimEcu,
    memory: Memory,
    generator: Generator,
    faults: Faults,
    output: VecDeque<u8>,
    transcript: Transcript,
    counters: Counters,
    reply_code: ReplyCode,
    open: bool,
    connected: bool,
    step: u64,
    /// How long a queued burn keeps reporting `burnPending`, in reads.
    burn_pending_samples: u32,
    /// Reads left before the queued burn reports complete.
    burn_pending_left: u32,
}

impl std::fmt::Debug for SimTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SimTransport")
            .field("counters", &self.counters)
            .field("open", &self.open)
            .field("connected", &self.connected)
            .field("step", &self.step)
            .finish_non_exhaustive()
    }
}

impl SimTransport {
    /// Builds the simulator for `definition`.
    ///
    /// # Errors
    ///
    /// Returns [`SimError`] when the definition has no usable signature or
    /// pages, or when a configured page does not fit.
    pub fn new(definition: &Definition, config: SimConfig) -> Result<Self, SimError> {
        let signature = config
            .signature
            .unwrap_or_else(|| definition.signature.clone());
        let ecu = SimEcu::new(definition, signature)?;
        let sizes: Vec<(u8, u32)> = definition
            .pages
            .iter()
            .map(|page| (page.number, page.size))
            .collect();
        let mut memory = Memory::patterned(&sizes, config.seed);
        for (page, bytes) in &config.pages {
            memory.set_ram(*page, bytes)?;
        }

        let mut faults = Faults::new();
        for rule in config.faults {
            faults.arm(rule);
        }

        let reply_code = BehaviorKind::select(definition).map_or(ReplyCode::Leading, |behavior| {
            behavior.as_dyn().reply_code(definition)
        });

        Ok(Self {
            ecu,
            memory,
            generator: Generator::new(definition, config.seed),
            faults,
            output: VecDeque::new(),
            transcript: Transcript::default(),
            counters: Counters::default(),
            reply_code,
            open: false,
            connected: false,
            step: 0,
            burn_pending_samples: config.burn_pending_samples,
            burn_pending_left: 0,
        })
    }

    /// Builds the simulator from a scenario file, against `definition`.
    ///
    /// # Errors
    ///
    /// Returns [`SimError`] for the same reasons as [`Self::new`].
    pub fn with_scenario(definition: &Definition, scenario: &Scenario) -> Result<Self, SimError> {
        Self::new(definition, SimConfig::from(scenario.clone()))
    }

    /// Arms one fault.
    pub fn arm(&mut self, operation: SimOp, fault: Fault) {
        self.faults.arm(FaultRule::once(operation, fault));
    }

    /// Arms one rule.
    pub fn add_rule(&mut self, rule: FaultRule) {
        self.faults.arm(rule);
    }

    /// The page size the definition declares.
    pub fn page_size(&self, page: u8) -> Option<usize> {
        self.ecu
            .pages
            .iter()
            .find(|entry| entry.number == page)
            .map(|entry| entry.size)
    }

    /// A page's RAM contents.
    pub fn ram(&self, page: u8) -> Option<&[u8]> {
        self.memory.ram(page)
    }

    /// A page's burned contents.
    pub fn flash(&self, page: u8) -> Option<&[u8]> {
        self.memory.flash(page)
    }

    /// Replaces a page's RAM, for a test's setup.
    ///
    /// # Errors
    ///
    /// Returns [`SimError`] when the page does not exist or the bytes do not
    /// fit.
    pub fn set_ram(&mut self, page: u8, bytes: &[u8]) -> Result<(), SimError> {
        self.memory.set_ram(page, bytes)
    }

    /// What the session moved.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// Everything that crossed the virtual wire.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// Whether the simulated link is up.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Puts the link back up, keeping memory — a reconnect.
    pub fn reconnect(&mut self) {
        self.connected = true;
        self.open = true;
    }

    /// Interprets one complete request and queues its reply.
    fn exchange(&mut self, bytes: &[u8]) -> Result<Vec<u8>, TransportError> {
        let (framed, payload) = match Frame::parse(bytes) {
            Ok(frame) => (true, frame.payload),
            Err(_) => (false, bytes.to_vec()),
        };

        let Some(request) = self.ecu.interpret(&payload) else {
            // The firmware ignores what it does not understand. A framed link
            // says so with an error frame; legacy stays silent.
            return Ok(if framed {
                Frame::encode(&[0x83]).unwrap_or_default()
            } else {
                Vec::new()
            });
        };
        let operation = operation_of(&request);
        let fault = self.faults.take(operation);

        if matches!(fault, Some(Fault::Disconnect)) {
            self.connected = false;
            return Err(TransportError::Write(
                "the simulated link was lost".to_owned(),
            ));
        }
        if matches!(fault, Some(Fault::DropReply)) {
            return Ok(Vec::new());
        }
        if let Some(Fault::Delay { delay }) = fault {
            std::thread::sleep(delay.min(MAX_FAULT_DELAY));
        }

        let mut reply = self.respond(request, fault.as_ref(), framed);
        if framed {
            reply = Self::frame_reply(&reply);
        }

        if let Some(fault) = &fault {
            match fault {
                Fault::Truncate { keep } => reply.truncate(*keep),
                Fault::Corrupt { at } => {
                    if !reply.is_empty() {
                        let index = at % reply.len();
                        reply[index] ^= 0xFF;
                    }
                }
                Fault::BadCrc => {
                    // Only a framed reply has a CRC to break; on a legacy link
                    // the last data byte is the closest equivalent.
                    if framed && reply.len() >= 4 {
                        let last = reply.len() - 1;
                        reply[last] ^= 0xFF;
                    } else if let Some(byte) = reply.last_mut() {
                        *byte ^= 0xFF;
                    }
                }
                Fault::DropReply
                | Fault::Delay { .. }
                | Fault::Disconnect
                | Fault::RefuseBurn
                | Fault::PartialWrite { .. } => {}
            }
        }

        Ok(reply)
    }

    /// Wraps a reply that `respond` built as a *payload*.
    fn frame_reply(payload: &[u8]) -> Vec<u8> {
        Frame::encode(payload).unwrap_or_default()
    }

    /// The payload of one response, before framing.
    fn respond(&mut self, request: Request<'_>, fault: Option<&Fault>, framed: bool) -> Vec<u8> {
        let ack = |reply_code: ReplyCode| -> Vec<u8> {
            if !framed {
                return Vec::new();
            }
            match reply_code {
                ReplyCode::Leading => vec![0],
                ReplyCode::Absent => Vec::new(),
            }
        };
        let error = |code: u8| -> Vec<u8> { if framed { vec![code] } else { Vec::new() } };

        match request {
            Request::Signature => {
                self.counters.signatures += 1;
                if framed {
                    match self.reply_code {
                        ReplyCode::Leading => {
                            let mut payload = vec![0];
                            payload.extend_from_slice(&self.ecu.signature);
                            payload
                        }
                        ReplyCode::Absent => self.ecu.signature.clone(),
                    }
                } else {
                    self.ecu.signature.clone()
                }
            }
            Request::ReadPage {
                page,
                offset,
                count,
            } => {
                self.counters.reads += 1;
                let Some(page_memory) = self.memory.page(page) else {
                    return error(0x84);
                };
                let start = usize::from(offset);
                let end = start.saturating_add(usize::from(count));
                if end > page_memory.ram.len() {
                    return error(0x84);
                }
                let data = page_memory.ram[start..end].to_vec();
                if !framed {
                    return data;
                }
                match self.reply_code {
                    ReplyCode::Leading => {
                        let mut payload = vec![0];
                        payload.extend_from_slice(&data);
                        payload
                    }
                    ReplyCode::Absent => data,
                }
            }
            Request::WritePage {
                page,
                offset,
                values,
            } => {
                self.counters.writes += 1;
                let applied = match fault {
                    Some(Fault::PartialWrite { bytes }) => values.len().min(*bytes),
                    _ => values.len(),
                };
                let Some(written) =
                    self.memory
                        .write(page, usize::from(offset), &values[..applied])
                else {
                    return error(0x84);
                };
                self.counters.bytes_written += u64::try_from(written).unwrap_or(0);
                ack(self.reply_code)
            }
            Request::Burn { page } => {
                self.counters.burns += 1;
                if matches!(fault, Some(Fault::RefuseBurn)) {
                    return error(0x83);
                }
                if self.memory.burn(page).is_none() {
                    return error(0x84);
                }
                // A real board queues the EEPROM write and answers first.
                self.burn_pending_left = self.burn_pending_samples;
                if framed { vec![0x04] } else { Vec::new() }
            }
            Request::ControllerCommand => {
                self.counters.commands += 1;
                if framed { vec![0] } else { Vec::new() }
            }
            Request::Crc {
                page,
                offset,
                count,
            } => self.crc_reply(page, offset, count, framed),
            Request::Realtime { offset, count } => self.realtime_reply(offset, count, framed),
        }
    }

    /// The payload of one page-CRC reply, before framing.
    fn crc_reply(&self, page: u8, offset: u16, count: u16, framed: bool) -> Vec<u8> {
        let Some(page_memory) = self.memory.page(page) else {
            return error_reply(0x84, framed);
        };
        let start = usize::from(offset);
        let end = start.saturating_add(usize::from(count));
        if end > page_memory.ram.len() {
            return error_reply(0x84, framed);
        }

        let bytes = crc32(&page_memory.ram[start..end]).to_be_bytes().to_vec();
        data_payload(self.reply_code, bytes, framed)
    }

    /// The payload of one realtime reply, before framing.
    fn realtime_reply(&mut self, offset: u16, count: u16, framed: bool) -> Vec<u8> {
        self.counters.realtime += 1;
        let Some(realtime) = self.ecu.realtime() else {
            return error_reply(0x83, framed);
        };
        let Some(data) = realtime_block(
            realtime,
            &self.generator,
            self.step,
            offset,
            count,
            self.burn_pending_left > 0,
        ) else {
            return error_reply(0x84, framed);
        };

        self.step = self.step.wrapping_add(1);
        self.burn_pending_left = self.burn_pending_left.saturating_sub(1);
        data_payload(self.reply_code, data, framed)
    }
}

/// A framed error payload, or silence on a legacy link.
fn error_reply(code: u8, framed: bool) -> Vec<u8> {
    if framed { vec![code] } else { Vec::new() }
}

/// Wraps a data payload with the behavior's leading return code.
fn data_payload(reply_code: ReplyCode, data: Vec<u8>, framed: bool) -> Vec<u8> {
    if !framed {
        return data;
    }

    match reply_code {
        ReplyCode::Leading => {
            let mut payload = vec![0];
            payload.extend_from_slice(&data);
            payload
        }
        ReplyCode::Absent => data,
    }
}

/// The operation a request maps to, for fault lookup.
fn operation_of(request: &Request<'_>) -> SimOp {
    match request {
        Request::Signature => SimOp::Signature,
        Request::ReadPage { .. } => SimOp::ReadPage,
        Request::WritePage { .. } => SimOp::WritePage,
        Request::Burn { .. } => SimOp::Burn,
        Request::Crc { .. } => SimOp::Crc,
        Request::Realtime { .. } => SimOp::Realtime,
        Request::ControllerCommand => SimOp::ControllerCommand,
    }
}

/// Generates and slices the realtime block for a subrange request.
///
/// The definition's `outputChannelStartOffset` is the block's base; a request
/// at or after it maps into the generated block, so a client that reads a
/// window (rather than the whole block) still sees consistent values.
fn realtime_block(
    realtime: &RealtimeEcu,
    generator: &Generator,
    step: u64,
    offset: u16,
    count: u16,
    burn_pending: bool,
) -> Option<Vec<u8>> {
    let mut block = generator.block(step);
    if burn_pending
        && let Some((byte_offset, mask)) = realtime.burn_pending
        && let Some(byte) = block.get_mut(byte_offset)
    {
        *byte |= mask;
    }
    let start = usize::from(offset.checked_sub(realtime.start_offset)?);
    let end = start.checked_add(usize::from(count))?;
    if end > block.len() || block.len() != realtime.block_size {
        return None;
    }
    Some(block[start..end].to_vec())
}

impl Transport for SimTransport {
    fn open(&mut self) -> Result<(), TransportError> {
        self.open = true;
        self.connected = true;
        self.output.clear();
        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        if !self.connected {
            return Err(TransportError::Closed);
        }
        let reply = self.exchange(bytes)?;
        self.transcript.push(TranscriptEntry {
            request: bytes.to_vec(),
            reply: reply.clone(),
        });
        self.output.extend(reply);
        Ok(())
    }

    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        _timeout: Duration,
    ) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        let mut filled = 0;
        while filled < buf.len() {
            let Some(byte) = self.output.pop_front() else {
                // The simulator's clock is virtual: a reply that was dropped is
                // reported at once rather than after the real timeout, so a
                // CI suite does not spend seconds proving a timeout.
                return Err(TransportError::Timeout {
                    got: filled,
                    expected: buf.len(),
                });
            };
            buf[filled] = byte;
            filled += 1;
        }
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
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        let before = sink.len();
        while sink.len() - before < max {
            let Some(byte) = self.output.pop_front() else {
                break;
            };
            sink.push(byte);
            if byte == terminator {
                break;
            }
        }
        if sink.len() == before {
            return Err(TransportError::Timeout {
                got: 0,
                expected: 1,
            });
        }
        Ok(())
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        let mut read = 0;
        while read < buf.len() {
            let Some(byte) = self.output.pop_front() else {
                break;
            };
            buf[read] = byte;
            read += 1;
        }
        Ok(read)
    }

    fn flush_input(&mut self) {
        self.output.clear();
    }

    fn close(&mut self) {
        self.open = false;
        self.connected = false;
        self.output.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_past_the_page_end_is_an_error_and_changes_nothing() {
        use oxtuner_ini::SettingsContext;

        let definition = Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
value = scalar, U08, 0, \"x\", 1, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut transport =
            SimTransport::new(&definition, SimConfig::default()).expect("simulates");
        let before = transport.ram(1).expect("page 1").to_vec();

        let reply = transport.respond(
            Request::WritePage {
                page: 1,
                offset: 6,
                values: &[1, 2, 3, 4],
            },
            None,
            true,
        );

        assert_eq!(reply, vec![0x84]);
        assert_eq!(transport.ram(1), Some(before.as_slice()));
        assert_eq!(transport.counters().bytes_written, 0);
        assert_eq!(transport.counters().writes, 1);

        let unknown = transport.respond(
            Request::WritePage {
                page: 9,
                offset: 0,
                values: &[1],
            },
            None,
            true,
        );
        assert_eq!(unknown, vec![0x84], "an unknown page is refused too");

        let legacy = transport.respond(
            Request::WritePage {
                page: 1,
                offset: 6,
                values: &[1, 2, 3, 4],
            },
            None,
            false,
        );
        assert!(legacy.is_empty(), "a legacy link stays silent on refusal");
        assert_eq!(transport.ram(1), Some(before.as_slice()));
    }

    #[test]
    fn the_transcript_keeps_the_last_entries_and_counts_the_drops() {
        let mut transcript = Transcript::default();
        for index in 0..=MAX_TRANSCRIPT_ENTRIES {
            transcript.push(TranscriptEntry {
                request: vec![u8::try_from(index).unwrap_or(0)],
                reply: Vec::new(),
            });
        }

        assert_eq!(transcript.len(), MAX_TRANSCRIPT_ENTRIES);
        assert_eq!(transcript.dropped(), 1);
        assert_eq!(
            transcript.entries().next().map(|entry| entry.request[0]),
            Some(1),
            "the oldest entry was dropped"
        );
    }
}
