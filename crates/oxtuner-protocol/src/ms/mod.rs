//! The MS-compatible protocol, parameterized by a [`Definition`].
//!
//! One type covers the message set Speeduino and its relatives speak: the
//! signature query, page reads, writes, burns and the realtime block. Which
//! command means what, the blocking factor, the delays and the reply sizes all
//! come from the definition — the protocol itself contains no ECU-specific
//! command letter.
//!
//! # Conversations are sequential
//!
//! Every method sends one request and consumes its complete reply before
//! returning. The caller (the core's I/O thread) runs one conversation at a
//! time on the wire; nothing here pipelines.

use std::collections::HashMap;
use std::time::Duration;

use oxtuner_ini::{CommandArguments, CommandTemplate, Definition, WireOrder};
use oxtuner_transport::{Transport, TransportError};

mod commands;
mod pages;
mod realtime;

pub use commands::RawCommandReport;
pub use pages::PageSpec;
pub use realtime::RealtimeSpec;

use pages::{PageCommands, build_pages};
use realtime::{Realtime, build_realtime};

use crate::behavior::{CrcReply, EcuBehavior, Match, PageSwitch, ReplyCode};
use crate::error::ProtocolError;
use crate::frame::Frame;

/// How the bytes are framed on the wire.
#[expect(
    clippy::exhaustive_enums,
    reason = "the protocol defines exactly the legacy and envelope framings"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkMode {
    /// A command byte, its arguments, then an unframed reply. Verified on the
    /// bench NO2C (P4) and the default.
    #[default]
    Legacy,
    /// Speeduino's `msEnvelope_1.0`: length, payload and CRC-32 in both
    /// directions.
    Envelope,
}

/// A page read request.
///
/// The protocol never guesses a timeout or a chunk size: both come from the
/// definition (`blockReadTimeout`, `blockingFactor`).
pub trait Protocol: Send {
    /// Reads the firmware signature.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the command is missing, the transport
    /// fails, or the reply is empty.
    fn signature(&mut self, io: &mut dyn Transport) -> Result<String, ProtocolError>;

    /// Reads `len` bytes from `page` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the page is unknown, the transport fails,
    /// or the reply is shorter than requested.
    fn read_page(
        &mut self,
        io: &mut dyn Transport,
        page: u8,
        offset: u16,
        len: u16,
    ) -> Result<Vec<u8>, ProtocolError>;

    /// Writes `data` to `page` at `offset`, chunked at the blocking factor.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the page is not writable, the payload is
    /// malformed, or the transport fails.
    fn write_page(
        &mut self,
        io: &mut dyn Transport,
        page: u8,
        offset: u16,
        data: &[u8],
    ) -> Result<(), ProtocolError>;

    /// Burns one page.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the page has no burn command or the
    /// transport fails.
    fn burn(&mut self, io: &mut dyn Transport, page: u8) -> Result<(), ProtocolError>;

    /// Reads one realtime block.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the definition declares no realtime
    /// command, the transport fails, or the reply is short.
    fn read_realtime(&mut self, io: &mut dyn Transport) -> Result<Vec<u8>, ProtocolError>;
}

impl std::fmt::Debug for MsProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MsProtocol")
            .field("name", &self.name)
            .field("pages", &self.pages.len())
            .field("link", &self.link)
            .finish_non_exhaustive()
    }
}

/// How long the signature query waits for an answer.
///
/// The first query races the firmware's boot after the port opens, so it gets
/// more room than a page read.
const SIGNATURE_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long the line must be quiet to end an unframed text reply.
///
/// 50 ms is far longer than the gap between bytes at 115200 baud (0.09 ms) and
/// far shorter than a user would notice.
const SIGNATURE_QUIET: Duration = Duration::from_millis(50);

/// How long a controller command waits for its first reply byte.
///
/// A raw action is often fire-and-forget, so silence is success. The wait is
/// short enough that a chain of such commands does not stall the link.
const COMMAND_REPLY_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a controller command's reply may be quiet before it ends.
const COMMAND_REPLY_QUIET: Duration = Duration::from_millis(50);

/// The most bytes one controller command's reply is drained into.
const COMMAND_REPLY_MAX: usize = 1024;

/// The longest signature reply accepted.
///
/// Generous on purpose: rusEFI and board-generated INIs append a version and a
/// board name, and a cut-off signature would be reported as a mismatch. The
/// serial reader stops at the quiet window, so this bound is a safety net, not
/// the normal end of the reply.
const SIGNATURE_MAX: usize = 512;

/// The chunk size used when a definition omits `blockingFactor`.
///
/// The smallest page in the Speeduino table is 128 bytes, so this fits any
/// firmware that forgot to state a factor.
const FALLBACK_BLOCKING_FACTOR: usize = 128;

/// A page CRC reply with a variable-length text encoding is not readable by
/// this build; neither link has a read shape for it.
fn unsupported_text_crc(page: u8) -> ProtocolError {
    ProtocolError::Unsupported(format!(
        "page {page} reports its CRC as text, which this build cannot read"
    ))
}

/// The MS-compatible protocol for one definition.
pub struct MsProtocol {
    name: &'static str,
    variables: HashMap<String, u8>,
    signature_command: Option<CommandTemplate>,
    handshake: Vec<Vec<u8>>,
    pages: Vec<PageCommands>,
    blocking_factor: usize,
    inter_write_delay: Option<Duration>,
    write_blocks: bool,
    block_read_timeout: Duration,
    page_activation_delay: Duration,
    port_open_delay: Duration,
    wire_order: WireOrder,
    realtime: Option<Realtime>,
    link: LinkMode,
    reply_code: ReplyCode,
    last_page: Option<u8>,
}

fn build_handshake(
    definition: &Definition,
    behavior: &dyn EcuBehavior,
    variables: &HashMap<String, u8>,
) -> Result<Vec<Vec<u8>>, ProtocolError> {
    behavior
        .handshake(definition)
        .iter()
        .map(|command| {
            command
                .encode(&CommandArguments::default(), &|name| {
                    variables.get(&name.to_ascii_lowercase()).copied()
                })
                .map_err(ProtocolError::Encoding)
        })
        .collect()
}

impl MsProtocol {
    /// Builds the protocol for `definition`, with `behavior` deciding the parts
    /// an INI cannot express.
    ///
    /// Fails when the definition is not an MS-compatible ECU at all, so a
    /// mis-picked file is refused before a port is opened.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Unsupported`] when no behavior claims the
    /// definition, a declared realtime size or offset does not fit the wire, or
    /// the firmware's page switch needs a burn (which this build cannot do), or
    /// [`ProtocolError::Encoding`] when a handshake command cannot be encoded.
    pub fn new(definition: &Definition, behavior: &dyn EcuBehavior) -> Result<Self, ProtocolError> {
        if behavior.matches(definition) == Match::No {
            return Err(ProtocolError::Unsupported(format!(
                "{} does not describe an ECU this build can talk to",
                definition.signature
            )));
        }

        let mut variables = HashMap::with_capacity(definition.pc_variables.len());
        for (name, _) in definition.pc_variables.iter() {
            if let Some(value) = definition.command_variable(name) {
                variables.insert(name.to_ascii_lowercase(), value);
            }
        }

        let pages = build_pages(definition, behavior, &variables);
        // Burning a page before it may be left is not something this build can
        // do inside a read or write conversation, so say so before a port is
        // opened instead of failing on the first page switch.
        if let Some(page) = pages
            .iter()
            .find(|entry| entry.switch == PageSwitch::BurnBeforeSwitch)
        {
            return Err(ProtocolError::Unsupported(format!(
                "this firmware must burn page {} before it is left; burn-before-switch paging is \
                 not implemented",
                page.spec.number
            )));
        }
        let handshake = build_handshake(definition, behavior, &variables)?;
        let realtime = build_realtime(definition)?;

        Ok(Self {
            name: behavior.name(),
            variables,
            signature_command: definition
                .query_command
                .clone()
                .filter(|command| !command.is_empty()),
            handshake,
            pages,
            blocking_factor: definition
                .comms
                .blocking_factor
                .and_then(|factor| usize::try_from(factor).ok())
                .filter(|factor| *factor > 0)
                .unwrap_or(FALLBACK_BLOCKING_FACTOR),
            inter_write_delay: definition
                .comms
                .inter_write_delay_ms
                .map(|milliseconds| Duration::from_millis(u64::from(milliseconds))),
            write_blocks: definition.comms.write_blocks,
            block_read_timeout: definition
                .comms
                .block_read_timeout_ms
                .map_or(Duration::from_secs(2), |milliseconds| {
                    Duration::from_millis(u64::from(milliseconds))
                }),
            page_activation_delay: definition
                .comms
                .page_activation_delay_ms
                .map_or(Duration::ZERO, |milliseconds| {
                    Duration::from_millis(u64::from(milliseconds))
                }),
            port_open_delay: definition
                .comms
                .delay_after_port_open_ms
                .map_or(Duration::ZERO, |milliseconds| {
                    Duration::from_millis(u64::from(milliseconds))
                }),
            wire_order: if definition.comms.force_big_endian_protocol {
                WireOrder::BigEndian
            } else {
                WireOrder::LittleEndian
            },
            realtime,
            link: default_link(definition),
            reply_code: behavior.reply_code(definition),
            last_page: None,
        })
    }

    /// The behavior's name, for logs and the connection summary.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Selects the framing. Legacy stays the default because it is the path
    /// verified read-only on hardware.
    #[must_use]
    pub fn with_link_mode(mut self, link: LinkMode) -> Self {
        self.link = link;
        self
    }

    /// The framing in use.
    pub fn link_mode(&self) -> LinkMode {
        self.link
    }

    /// The pages the definition declares.
    pub fn pages(&self) -> Vec<PageSpec> {
        self.pages.iter().map(|page| page.spec.clone()).collect()
    }

    /// One page, when declared.
    pub fn page(&self, number: u8) -> Option<&PageSpec> {
        self.pages
            .iter()
            .find(|page| page.spec.number == number)
            .map(|page| &page.spec)
    }

    /// The realtime block, when the definition declares one.
    pub fn realtime(&self) -> Option<RealtimeSpec> {
        self.realtime.as_ref().map(|realtime| realtime.spec)
    }

    /// How many bytes one read or write carries.
    pub fn blocking_factor(&self) -> usize {
        self.blocking_factor
    }

    /// The timeout for a page or realtime reply.
    pub fn block_read_timeout(&self) -> Duration {
        self.block_read_timeout
    }

    /// How long to wait after opening the port.
    ///
    /// `delayAfterPortOpen` covers the DTR reset an Arduino performs when the
    /// host opens the port; without it the first query races the bootloader.
    pub fn port_open_delay(&self) -> Duration {
        self.port_open_delay
    }

    /// Reads a whole page.
    ///
    /// The read is chunked at the blocking factor and the chunks are
    /// concatenated in order, so a page larger than one conversation still
    /// reads completely.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Unsupported`] when the page is not declared or
    /// has no read command, and any [`ProtocolError`] a chunked read returns.
    pub fn read_page_all(
        &mut self,
        io: &mut dyn Transport,
        page: u8,
    ) -> Result<Vec<u8>, ProtocolError> {
        let spec = self
            .page(page)
            .cloned()
            .ok_or_else(|| ProtocolError::Unsupported(format!("page {page} is not declared")))?;
        if !spec.readable {
            return Err(ProtocolError::Unsupported(format!(
                "page {page} has no read command"
            )));
        }
        let size = usize::try_from(spec.size).unwrap_or(0);
        // A definition can state a page size up to `u32::MAX`; do not reserve
        // that up front. The read itself is chunked and fails on the first
        // offset that no longer fits the wire.
        let mut bytes = Vec::with_capacity(size.min(64 * 1024));
        let mut offset = 0usize;
        while offset < size {
            let chunk = self.blocking_factor.min(size - offset);
            let count = u16::try_from(chunk).map_err(|_| {
                ProtocolError::Unsupported(format!("page {page} is larger than one conversation"))
            })?;
            let part = self.read_page(
                io,
                page,
                u16::try_from(offset).map_err(|_| {
                    ProtocolError::Unsupported(format!("page {page} is larger than 64 KiB"))
                })?,
                count,
            )?;
            bytes.extend_from_slice(&part);
            offset += chunk;
        }
        Ok(bytes)
    }

    /// Reads the page's CRC-32, when the definition offers a command.
    ///
    /// The algorithm and the reply encoding come from [`EcuBehavior::crc`];
    /// this method only moves the bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the page size does not fit the command's
    /// count field, the firmware reports a text CRC, the page switch or the
    /// transport fails, or when the CRC reply is not the expected four bytes.
    pub fn read_page_crc(
        &mut self,
        io: &mut dyn Transport,
        page: u8,
    ) -> Result<Option<u32>, ProtocolError> {
        let Some(entry) = self.pages.iter().find(|entry| entry.spec.number == page) else {
            return Ok(None);
        };
        let Some(command) = entry.crc.clone() else {
            return Ok(None);
        };
        let size = u16::try_from(entry.spec.size).map_err(|_| {
            ProtocolError::Unsupported(format!(
                "page {page} is {} bytes, which does not fit the 16-bit count field of its CRC \
                 command",
                entry.spec.size
            ))
        })?;
        let identifier = entry.identifier;
        let crc_reply = entry.crc_reply.unwrap_or(CrcReply::RawBigEndian);

        // Both links read the checksum as exactly four bytes, so a text reply
        // would have its first four characters parsed as hex. Refuse instead of
        // returning a wrong checksum that read-back would treat as verified.
        if crc_reply == CrcReply::HexText {
            return Err(unsupported_text_crc(page));
        }

        self.activate(io, page)?;
        let request = self.encode(&command, u16::from_be_bytes(identifier), 0, size, &[])?;

        let bytes = match self.link {
            LinkMode::Legacy => {
                io.write_all(&request)?;
                let mut reply = [0u8; 4];
                io.read_exact_timeout(&mut reply, self.block_read_timeout)?;
                reply.to_vec()
            }
            LinkMode::Envelope => {
                let payload = self.converse(io, &request, "page CRC")?;
                self.data_payload(&payload, 4, "page CRC")?.to_vec()
            }
        };

        if bytes.len() != 4 {
            return Err(ProtocolError::UnexpectedReply(format!(
                "page {page} CRC reply is {} byte(s)",
                bytes.len()
            )));
        }
        let crc = match crc_reply {
            CrcReply::RawBigEndian => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            CrcReply::RawLittleEndian => {
                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
            }
            // Refused before any read; the arm is defensive so a new link mode
            // cannot silently fall through to a raw parse.
            CrcReply::HexText => {
                return Err(unsupported_text_crc(page));
            }
        };
        Ok(Some(crc))
    }

    /// Sends the behavior's handshake, once after the port opens.
    ///
    /// Speeduino and rusEFI need none; the hook exists for a firmware that must
    /// be woken before its first query.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the transport rejects a handshake write.
    pub fn handshake(&self, io: &mut dyn Transport) -> Result<(), ProtocolError> {
        for command in &self.handshake {
            io.write_all(command)?;
        }
        Ok(())
    }

    /// Prepares the wire for `page`, as `pageActivationDelay` and the behavior
    /// require.
    ///
    /// `ReadBeforeUse` sends a one-byte read of the target page: the legacy
    /// protocol uses that read to select the page. `BurnBeforeSwitch` is
    /// refused here; [`MsProtocol::new`] already rejects such a definition, so
    /// this arm is a guard against a behavior that starts reporting it later.
    fn activate(&mut self, io: &mut dyn Transport, page: u8) -> Result<(), ProtocolError> {
        if self.last_page == Some(page) {
            return Ok(());
        }
        let (switch, identifier, read) = self
            .pages
            .iter()
            .find(|entry| entry.spec.number == page)
            .map(|entry| (entry.switch, entry.identifier, entry.read.clone()))
            .ok_or_else(|| ProtocolError::Unsupported(format!("page {page} is not declared")))?;

        if !self.page_activation_delay.is_zero() {
            std::thread::sleep(self.page_activation_delay);
        }
        match switch {
            PageSwitch::None => {}
            PageSwitch::ReadBeforeUse => {
                let command = read.ok_or_else(|| {
                    ProtocolError::Unsupported(format!(
                        "page {page} must be selected by a read, but has no read command"
                    ))
                })?;
                let request = self.encode(&command, u16::from_be_bytes(identifier), 0, 1, &[])?;
                match self.link {
                    LinkMode::Legacy => {
                        io.write_all(&request)?;
                        let mut byte = [0u8; 1];
                        io.read_exact_timeout(&mut byte, self.block_read_timeout)?;
                    }
                    LinkMode::Envelope => {
                        let payload = self.converse(io, &request, "page select")?;
                        self.data_payload(&payload, 1, "page select")?;
                    }
                }
            }
            PageSwitch::BurnBeforeSwitch => {
                return Err(ProtocolError::Unsupported(format!(
                    "page {page} must be burned before it is left, and a burn is not part of a read \
                     session"
                )));
            }
        }
        self.last_page = Some(page);
        Ok(())
    }

    pub(super) fn encode(
        &self,
        command: &CommandTemplate,
        page: u16,
        offset: u16,
        count: u16,
        values: &[u8],
    ) -> Result<Vec<u8>, ProtocolError> {
        command
            .encode(
                &CommandArguments {
                    page,
                    offset,
                    count,
                    values,
                    wire_order: self.wire_order,
                },
                &|name| self.variables.get(&name.to_ascii_lowercase()).copied(),
            )
            .map_err(ProtocolError::Encoding)
    }

    /// Sends one framed request and returns the validated reply payload.
    fn converse(
        &mut self,
        io: &mut dyn Transport,
        request: &[u8],
        _operation: &'static str,
    ) -> Result<Vec<u8>, ProtocolError> {
        let framed = Frame::encode(request)?;
        io.write_all(&framed)?;
        self.framed_reply(io)
    }

    /// Reads one complete framed reply.
    pub(super) fn framed_reply(&self, io: &mut dyn Transport) -> Result<Vec<u8>, ProtocolError> {
        let mut header = [0u8; 2];
        io.read_exact_timeout(&mut header, self.block_read_timeout)?;
        let length = Frame::length(header);
        // Reject a corrupt length before allocating or waiting for bytes that
        // cannot belong to a valid reply.
        if length > crate::frame::MAX_PAYLOAD {
            return Err(ProtocolError::BadFrame(format!(
                "the reply declares {length} payload bytes, the limit is {}",
                crate::frame::MAX_PAYLOAD
            )));
        }
        let mut rest = vec![0u8; length + 4];
        io.read_exact_timeout(&mut rest, self.block_read_timeout)?;

        let mut bytes = Vec::with_capacity(length + 6);
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&rest);
        let reply = Frame::parse(&bytes)?;
        Ok(reply.payload)
    }

    /// Checks a framed acknowledgement and returns the bytes after it.
    ///
    /// A write is acknowledged either by an empty payload (rusEFI) or by a
    /// leading zero return code (Speeduino). Anything else is a refusal.
    fn expect_ok<'a>(
        &self,
        payload: &'a [u8],
        operation: &'static str,
    ) -> Result<&'a [u8], ProtocolError> {
        match (self.reply_code, payload.first()) {
            (_, None) => Ok(&[]),
            (ReplyCode::Leading | ReplyCode::Absent, Some(0)) => Ok(&payload[1..]),
            (_, Some(code)) => Err(ProtocolError::ReturnCode {
                code: *code,
                operation,
            }),
        }
    }

    /// Splits a framed data reply into its bytes, honouring the reply code.
    fn data_payload<'a>(
        &self,
        payload: &'a [u8],
        expected: usize,
        operation: &'static str,
    ) -> Result<&'a [u8], ProtocolError> {
        let (data, expected_len) = match self.reply_code {
            ReplyCode::Leading => {
                if payload.len() != expected + 1 {
                    return Err(ProtocolError::UnexpectedReply(format!(
                        "the {operation} reply carries {} byte(s), expected {}",
                        payload.len(),
                        expected + 1
                    )));
                }
                if payload[0] != 0 {
                    return Err(ProtocolError::ReturnCode {
                        code: payload[0],
                        operation,
                    });
                }
                (&payload[1..], expected)
            }
            ReplyCode::Absent => (payload, expected),
        };
        if data.len() != expected_len {
            return Err(ProtocolError::UnexpectedReply(format!(
                "the {operation} reply carries {} byte(s), expected {expected_len}",
                data.len()
            )));
        }
        Ok(data)
    }
}

impl Protocol for MsProtocol {
    fn signature(&mut self, io: &mut dyn Transport) -> Result<String, ProtocolError> {
        let command = self.signature_command.clone().ok_or_else(|| {
            ProtocolError::Unsupported(
                "the definition has no queryCommand, so the signature cannot be read".to_owned(),
            )
        })?;
        let request = self.encode(&command, 0, 0, 0, &[])?;
        io.flush_input();

        let reply = match self.link {
            LinkMode::Legacy => {
                io.write_all(&request)?;
                let mut reply = Vec::new();
                let outcome = io.read_until(
                    &mut reply,
                    SIGNATURE_MAX,
                    0,
                    SIGNATURE_QUIET,
                    SIGNATURE_TIMEOUT,
                );
                match outcome {
                    Ok(()) => reply,
                    Err(TransportError::Timeout { got: 0, .. }) => {
                        return Err(ProtocolError::Transport(TransportError::Timeout {
                            got: 0,
                            expected: 1,
                        }));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            LinkMode::Envelope => {
                let payload = self.converse(io, &request, "signature")?;
                // The firmware prefixes most replies with a return code, the
                // signature included; a firmware that does not is accepted too.
                match payload.split_first() {
                    Some((0, rest)) => rest.to_vec(),
                    _ => payload,
                }
            }
        };

        let text: String = reply
            .iter()
            .map(|byte| {
                if byte.is_ascii_graphic() || *byte == b' ' {
                    char::from(*byte)
                } else {
                    '\0'
                }
            })
            .collect();
        let signature = text
            .trim_matches(|character: char| character == '\0' || character.is_whitespace())
            .to_owned();
        if signature.is_empty() {
            return Err(ProtocolError::UnexpectedReply(
                "the signature reply is empty".to_owned(),
            ));
        }
        Ok(signature)
    }

    fn read_page(
        &mut self,
        io: &mut dyn Transport,
        page: u8,
        offset: u16,
        len: u16,
    ) -> Result<Vec<u8>, ProtocolError> {
        let (spec, identifier, command) = self
            .pages
            .iter()
            .find(|entry| entry.spec.number == page)
            .map(|entry| (entry.spec.clone(), entry.identifier, entry.read.clone()))
            .ok_or_else(|| ProtocolError::Unsupported(format!("page {page} is not declared")))?;
        let command = command.ok_or_else(|| {
            ProtocolError::Unsupported(format!("page {page} has no read command"))
        })?;
        let end = u32::from(offset).saturating_add(u32::from(len));
        if end > spec.size {
            return Err(ProtocolError::Unsupported(format!(
                "page {page} holds {} byte(s); {len} at {offset} does not fit",
                spec.size
            )));
        }

        self.activate(io, page)?;
        let request = self.encode(&command, u16::from_be_bytes(identifier), offset, len, &[])?;
        let size = usize::from(len);

        match self.link {
            LinkMode::Legacy => {
                let mut bytes = vec![0u8; size];
                io.write_all(&request)?;
                io.read_exact_timeout(&mut bytes, self.block_read_timeout)?;
                Ok(bytes)
            }
            LinkMode::Envelope => {
                let payload = self.converse(io, &request, "page read")?;
                Ok(self.data_payload(&payload, size, "page read")?.to_vec())
            }
        }
    }

    fn write_page(
        &mut self,
        io: &mut dyn Transport,
        page: u8,
        offset: u16,
        data: &[u8],
    ) -> Result<(), ProtocolError> {
        if data.is_empty() {
            return Ok(());
        }
        let (spec, identifier, command) = self
            .pages
            .iter()
            .find(|entry| entry.spec.number == page)
            .map(|entry| (entry.spec.clone(), entry.identifier, entry.write.clone()))
            .ok_or_else(|| ProtocolError::Unsupported(format!("page {page} is not declared")))?;
        let command = command.ok_or_else(|| {
            ProtocolError::Unsupported(format!("page {page} has no write command"))
        })?;
        let end = u32::from(offset)
            .checked_add(u32::try_from(data.len()).unwrap_or(u32::MAX))
            .ok_or_else(|| ProtocolError::Unsupported("the write range overflows".to_owned()))?;
        if end > spec.size {
            return Err(ProtocolError::Unsupported(format!(
                "page {page} holds {} byte(s); the write ends at {end}",
                spec.size
            )));
        }

        self.activate(io, page)?;
        for (index, chunk) in data.chunks(self.blocking_factor.max(1)).enumerate() {
            let chunk_offset = u32::from(offset).saturating_add(
                u32::try_from(index)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(u32::try_from(self.blocking_factor).unwrap_or(u32::MAX)),
            );
            let chunk_offset = u16::try_from(chunk_offset).map_err(|_| {
                ProtocolError::Unsupported("the write offset does not fit the wire".to_owned())
            })?;
            let chunk_len = u16::try_from(chunk.len()).map_err(|_| {
                ProtocolError::Unsupported("the write chunk does not fit the wire".to_owned())
            })?;
            let request = self.encode(
                &command,
                u16::from_be_bytes(identifier),
                chunk_offset,
                chunk_len,
                chunk,
            )?;

            match self.link {
                LinkMode::Legacy => io.write_all(&request)?,
                LinkMode::Envelope => {
                    let payload = self.converse(io, &request, "page write")?;
                    self.expect_ok(&payload, "page write")?;
                }
            }

            // `tsWriteBlocks` means the host may stream blocks back to back.
            if !self.write_blocks
                && let Some(delay) = self.inter_write_delay
                && delay > Duration::ZERO
            {
                std::thread::sleep(delay);
            }
        }
        Ok(())
    }

    fn burn(&mut self, io: &mut dyn Transport, page: u8) -> Result<(), ProtocolError> {
        let (identifier, command) = self
            .pages
            .iter()
            .find(|entry| entry.spec.number == page)
            .map(|entry| (entry.identifier, entry.burn.clone()))
            .ok_or_else(|| {
                ProtocolError::Unsupported(format!(
                    "page {page} has no burn command, so it lives in RAM only"
                ))
            })?;
        let command = command.ok_or_else(|| {
            ProtocolError::Unsupported(format!(
                "page {page} has no burn command, so it lives in RAM only"
            ))
        })?;

        self.activate(io, page)?;
        let request = self.encode(&command, u16::from_be_bytes(identifier), 0, 0, &[])?;
        match self.link {
            LinkMode::Legacy => io.write_all(&request)?,
            LinkMode::Envelope => {
                let payload = self.converse(io, &request, "burn")?;
                // The framed protocol answers a successful burn with
                // `SERIAL_RC_BURN_OK` (0x04), not the general OK code.
                match payload.first() {
                    Some(0x00 | 0x04) => {}
                    Some(code) => {
                        return Err(ProtocolError::ReturnCode {
                            code: *code,
                            operation: "burn",
                        });
                    }
                    None => {
                        return Err(ProtocolError::UnexpectedReply(
                            "the burn reply is empty".to_owned(),
                        ));
                    }
                }
            }
        }
        // A burn writes EEPROM and the firmware needs the activation delay
        // before the next page conversation.
        if !self.page_activation_delay.is_zero() {
            std::thread::sleep(self.page_activation_delay);
        }
        Ok(())
    }

    fn read_realtime(&mut self, io: &mut dyn Transport) -> Result<Vec<u8>, ProtocolError> {
        let realtime = self.realtime.clone().ok_or_else(|| {
            ProtocolError::Unsupported(
                "the definition has no ochGetCommand/ochBlockSize pair".to_owned(),
            )
        })?;
        let request = self.encode(
            &realtime.command,
            0,
            realtime.spec.start_offset,
            realtime.spec.block_size,
            &[],
        )?;
        let size = usize::from(realtime.spec.block_size);
        io.flush_input();

        match self.link {
            LinkMode::Legacy => {
                let mut bytes = vec![0u8; size];
                io.write_all(&request)?;
                io.read_exact_timeout(&mut bytes, self.block_read_timeout)?;
                Ok(bytes)
            }
            LinkMode::Envelope => {
                let payload = self.converse(io, &request, "realtime read")?;
                Ok(self.data_payload(&payload, size, "realtime read")?.to_vec())
            }
        }
    }
}

/// The framing a definition asks for.
///
/// `messageEnvelopeFormat = msEnvelope_1.0` is the dialect's way of saying the
/// firmware speaks the framed protocol; anything else starts on the legacy
/// path. [`MsProtocol::with_link_mode`] overrides the choice for a session that
/// needs the other one.
fn default_link(definition: &Definition) -> LinkMode {
    match definition.comms.envelope.as_deref() {
        Some(format) if format.eq_ignore_ascii_case("msEnvelope_1.0") => LinkMode::Envelope,
        _ => LinkMode::Legacy,
    }
}
