//! The I/O thread: the sole owner of the transport (ADR-0008).
//!
//! ```text
//! core thread ──IoRequest (SPSC)──▶ I/O thread ──Transport──▶ ECU
//!            ◀──IoReply   (SPSC)──      owns the port
//! ```
//!
//! One conversation at a time, always the whole conversation: the protocol call
//! returns before the next request is read. A request that the core abandons
//! still completes its conversation (the reply send simply finds no reader);
//! that is the cancel-on-drop rule — no half-written command stays on the wire.
//!
//! The thread is started per connection and joined on disconnect. It holds the
//! [`RecordingTransport`] for the whole session, so a trace spans reconnects.

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossfire::{Rx, TrySendError, spsc};
use oxtuner_ini::{CommandTemplate as IniCommandTemplate, Definition};
use oxtuner_protocol::{
    BehaviorKind, LinkMode, MsProtocol, PageSpec, ProbeSpec, Protocol, ProtocolError,
    RawCommandReport,
};
use oxtuner_transport::{
    PortInfo, RecordingTransport, SerialSettings, SerialTransport, Transport, TransportError, ports,
};

use crate::link::PortChoice;
use crate::service::CoreError;

/// How many requests may wait for the wire.
///
/// The core keeps at most one operation per kind in flight and only one poll at
/// a time, so this is generous; it exists so a bug cannot grow the queue
/// without bound.
const REQUEST_CAPACITY: usize = 16;

/// How many page passes a full read may take before it gives up waiting for
/// the firmware to hold still.
const STABLE_READ_ATTEMPTS: usize = 3;

/// How long to wait between two page passes while the reboot settles.
const STABLE_READ_DELAY: Duration = Duration::from_millis(400);

/// How many replies may wait for the core.
const REPLY_CAPACITY: usize = 48;

/// How long a reply may wait for the core before it is dropped.
///
/// Long enough for a busy core to drain one reply, short enough that shutdown
/// never hangs on a crowded queue.
const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

/// Everything the I/O thread needs to build its side.
pub(crate) struct IoPlan {
    /// What the protocol is built from.
    pub(crate) source: ProtocolSource,
    /// The framing override (`None` uses the source's own).
    pub(crate) link_mode: Option<LinkMode>,
    /// Where the bytes go.
    pub(crate) link: PortChoice,
}

/// What a session's protocol is built from.
///
/// A session derives everything from the definition the user opened. A probe
/// has no definition to derive from, so it carries the three facts it needs
/// itself. Both end up in one [`MsProtocol`], so the conversations have a
/// single implementation.
pub(crate) enum ProtocolSource {
    /// A full session, from the loaded definition and the family it selected.
    Definition {
        /// The definition the protocol encodes with; owned by the thread.
        definition: Box<Definition>,
        /// The behavior selected for it.
        behavior: BehaviorKind,
    },
    /// A signature probe, with no definition in hand.
    Probe(ProbeSpec),
}

impl IoPlan {
    /// A plan for a full session.
    pub(crate) fn definition(
        definition: Box<Definition>,
        behavior: BehaviorKind,
        link_mode: Option<LinkMode>,
        link: PortChoice,
    ) -> Self {
        Self {
            source: ProtocolSource::Definition {
                definition,
                behavior,
            },
            link_mode,
            link,
        }
    }

    /// A plan for a signature probe.
    pub(crate) fn probe(spec: ProbeSpec, link: PortChoice) -> Self {
        Self {
            source: ProtocolSource::Probe(spec),
            link_mode: None,
            link,
        }
    }
}

/// One request for the wire.
#[derive(Debug)]
pub(crate) enum IoOp {
    /// Read the firmware signature.
    Signature,
    /// Read the firmware signature with a short reply timeout.
    ///
    /// Used by a port probe, where a silent port must cost little.
    ProbeSignature {
        /// How long to wait for the answer.
        timeout: Duration,
    },
    /// Read a list of whole pages, in order.
    ReadPages { pages: Vec<u8> },
    /// Read every declared page.
    ReadAll,
    /// Write one or more pages, already encoded by the core.
    WritePages { pages: Vec<PageWrite> },
    /// Burn a list of pages.
    BurnPages { pages: Vec<u8> },
    /// Send one `[ControllerCommands]` action's command strings, in order.
    Command { commands: Vec<IniCommandTemplate> },
    /// Read one realtime block.
    ReadRealtime,
    /// Reopen the link and read the signature again.
    Reconnect,
    /// Save the session trace.
    SaveTrace { path: PathBuf },
    /// Close the link and stop.
    Close,
}

/// One page write: the page number and the encoded byte image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageWrite {
    /// INI page number.
    pub(crate) page: u8,
    /// Bytes to write from offset 0.
    pub(crate) data: Vec<u8>,
}

/// One request with its identity.
#[derive(Debug)]
pub(crate) struct IoRequest {
    /// Unique per connection; the reply carries it back.
    pub(crate) id: u64,
    /// What to do.
    pub(crate) op: IoOp,
}

/// One page returned by a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageRead {
    /// INI page number.
    pub(crate) page: u8,
    /// The page bytes, exactly the declared size.
    pub(crate) bytes: Vec<u8>,
}

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IoFailureKind {
    /// The ECU did not answer in time.
    Timeout,
    /// The link is gone and a reconnect is needed.
    LinkLost,
    /// The answer arrived but made no sense.
    Protocol,
    /// The definition cannot express the operation.
    Unsupported,
    /// The thread stopped before finishing.
    Stopped,
    /// The trace could not be written.
    Trace,
}

impl IoFailureKind {
    /// A stable code for logs.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::LinkLost => "link-lost",
            Self::Protocol => "protocol",
            Self::Unsupported => "unsupported",
            Self::Stopped => "stopped",
            Self::Trace => "trace",
        }
    }
}

/// A failure from the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IoFailure {
    /// What kind.
    pub(crate) kind: IoFailureKind,
    /// Text the UI can show.
    pub(crate) message: String,
}

/// The answer to one request.
#[derive(Debug)]
pub(crate) enum IoOutcome {
    /// The ECU's signature.
    Signature { signature: String },
    /// A requested list of pages.
    Pages { pages: Vec<PageRead> },
    /// Every page, in definition order. Complete: a partial read is an error.
    All { pages: Vec<PageRead> },
    /// Pages were written.
    Written { pages: usize, bytes: usize },
    /// Pages were burned.
    Burned { pages: usize },
    /// One controller command action was sent.
    Command { report: RawCommandReport },
    /// One realtime block.
    Realtime { block: Vec<u8> },
    /// The link reopened and answered again.
    Reconnected { signature: String },
    /// The trace was written.
    TraceSaved { path: PathBuf, frames: usize },
    /// The link closed.
    Closed,
    /// The operation failed.
    Failed(IoFailure),
}

/// A reply to one request.
#[derive(Debug)]
pub(crate) struct IoReply {
    /// The request's id.
    pub(crate) id: u64,
    /// What happened.
    pub(crate) outcome: IoOutcome,
}

/// The core's end of the I/O thread.
///
/// Dropping (or [`IoThread::shutdown`]) blocks until the request in flight
/// reaches its own I/O deadline. Quitting deliberately waits instead of
/// detaching: a detached thread would keep the serial port open while another
/// process could claim it.
pub(crate) struct IoThread {
    requests: crossfire::Tx<spsc::Array<IoRequest>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl IoThread {
    /// Builds the transport and starts the thread.
    pub(crate) fn start(plan: &IoPlan) -> Result<(Self, Rx<spsc::Array<IoReply>>), CoreError> {
        let (request_tx, request_rx) = spsc::bounded_blocking(REQUEST_CAPACITY);
        let (reply_tx, reply_rx) = spsc::bounded_blocking(REPLY_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);

        // The transport and protocol are built here, where their errors can be
        // reported as a connection failure instead of a dead thread.
        let (transport, protocol) = build(plan)?;

        let join = thread::Builder::new()
            .name("oxtuner-io".to_owned())
            .spawn(move || run(transport, protocol, request_rx, reply_tx, thread_stop))
            .map_err(CoreError::Spawn)?;

        Ok((
            Self {
                requests: request_tx,
                stop,
                join: Some(join),
            },
            reply_rx,
        ))
    }

    /// Queues one request. Never blocks: a full queue is reported.
    pub(crate) fn submit(&self, id: u64, op: IoOp) -> Result<(), CoreError> {
        match self.requests.try_send(IoRequest { id, op }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(CoreError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(CoreError::Stopped),
        }
    }

    /// Stops the thread and waits for it, bounded by its own conversation.
    pub(crate) fn shutdown(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.requests.try_send(IoRequest {
            id: 0,
            op: IoOp::Close,
        });
        if let Some(join) = self.join.take()
            && join.join().is_err()
        {
            tracing::error!("the I/O thread panicked");
        }
    }
}

impl Drop for IoThread {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Lists serial ports for the connect dialog.
pub(crate) fn list_ports() -> Result<Vec<PortInfo>, TransportError> {
    ports()
}

/// Builds the transport and protocol for a plan.
///
/// A serial device is opened here because that is the core's own capability.
/// Any other link comes from the application through [`PortChoice::Custom`],
/// so this module never names a simulator or a replay reader (ADR-0022).
fn build(plan: &IoPlan) -> Result<(RecordingTransport, MsProtocol), CoreError> {
    let label = plan.link.label();

    // Opening a link asks the source for what it knows: a session hands over
    // its definition, a probe hands over its spec. A serial device needs
    // neither.
    let transport: Box<dyn Transport> = match (&plan.link, &plan.source) {
        (PortChoice::Serial { port, baud }, _) => Box::new(SerialTransport::new(
            SerialSettings::new(port.clone()).with_baud(*baud),
        )),
        (PortChoice::Custom(link), ProtocolSource::Definition { definition, .. }) => {
            link.open(definition).map_err(CoreError::Connect)?
        }
        (PortChoice::Custom(link), ProtocolSource::Probe(spec)) => {
            link.open_for_probe(spec).map_err(CoreError::Connect)?
        }
    };

    let mut recorder = RecordingTransport::new(transport);
    recorder.note(format!("link: {label}"));

    let mut protocol = match &plan.source {
        ProtocolSource::Definition {
            definition,
            behavior,
        } => {
            recorder.note(format!("definition: {}", definition.signature));

            MsProtocol::new(definition, behavior.as_dyn())
                .map_err(|error| CoreError::Connect(error.to_string()))?
        }
        ProtocolSource::Probe(spec) => {
            recorder.note(format!("probe: {}", spec.name));

            MsProtocol::for_probe(spec)
        }
    };

    if let Some(link) = plan.link_mode {
        protocol = protocol.with_link_mode(link);
    }
    recorder.note(format!(
        "behavior: {} · framing: {:?}",
        protocol.behavior_id(),
        protocol.link_mode()
    ));

    Ok((recorder, protocol))
}

/// The thread loop.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the thread loop owns its endpoints for the life of the thread"
)]
fn run(
    mut transport: RecordingTransport,
    mut protocol: MsProtocol,
    requests: Rx<spsc::Array<IoRequest>>,
    replies: crossfire::Tx<spsc::Array<IoReply>>,
    stop: Arc<AtomicBool>,
) {
    let mut opened = false;

    while let Ok(request) = requests.recv() {
        if stop.load(Ordering::Relaxed) {
            send(
                &replies,
                IoReply {
                    id: request.id,
                    outcome: IoOutcome::Failed(IoFailure {
                        kind: IoFailureKind::Stopped,
                        message: "the I/O thread is stopping".to_owned(),
                    }),
                },
            );
            break;
        }

        // The link opens lazily, on the first operation that needs it, so a
        // trace save or a close never resets an Arduino it did not have to.
        if needs_link(&request.op) && !opened {
            match open_link(&mut transport, &protocol) {
                Ok(()) => opened = true,
                Err(error) => {
                    send(
                        &replies,
                        IoReply {
                            id: request.id,
                            outcome: IoOutcome::Failed(IoFailure {
                                kind: IoFailureKind::LinkLost,
                                message: error.to_string(),
                            }),
                        },
                    );
                    continue;
                }
            }
        }

        let (outcome, close) = execute(&mut transport, &mut protocol, request.op, &stop);
        if matches!(
            &outcome,
            IoOutcome::Failed(IoFailure {
                kind: IoFailureKind::LinkLost,
                ..
            })
        ) {
            opened = false;
        }
        if matches!(&outcome, IoOutcome::Reconnected { .. }) {
            opened = true;
        }
        send(
            &replies,
            IoReply {
                id: request.id,
                outcome,
            },
        );
        if close {
            break;
        }
    }
    transport.close();
}

/// Whether this operation needs an open link.
fn needs_link(op: &IoOp) -> bool {
    !matches!(op, IoOp::Close | IoOp::SaveTrace { .. } | IoOp::Reconnect)
}

/// Opens the link and honours `delayAfterPortOpen`.
fn open_link(
    transport: &mut RecordingTransport,
    protocol: &MsProtocol,
) -> Result<(), TransportError> {
    transport.open()?;
    let delay = protocol.port_open_delay();
    if !delay.is_zero() {
        tracing::debug!(
            delay_ms = delay.as_millis(),
            "waiting after opening the port"
        );
        std::thread::sleep(delay);
    }
    Ok(())
}

/// Reads every readable page once.
///
/// Returns the caller's answer on a stop request or a protocol failure, so the
/// caller can pass it straight back.
fn read_every_page(
    protocol: &mut MsProtocol,
    transport: &mut RecordingTransport,
    stop: &AtomicBool,
) -> Result<Vec<PageRead>, (IoOutcome, bool)> {
    // A snapshot of the specs, so the iterator's borrow of the protocol ends
    // before the first read borrows it mutably.
    let specs: Vec<PageSpec> = protocol.pages().collect();
    let mut pages = Vec::new();
    for spec in specs {
        if stop.load(Ordering::Relaxed) {
            return Err((
                IoOutcome::Failed(IoFailure {
                    kind: IoFailureKind::Stopped,
                    message: "the page read was stopped".to_owned(),
                }),
                true,
            ));
        }
        if !spec.readable || spec.size == 0 {
            continue;
        }
        match protocol.read_page_all(transport, spec.number) {
            Ok(bytes) => pages.push(PageRead {
                page: spec.number,
                bytes,
            }),
            Err(error) => return Err((failure(&error), false)),
        }
    }
    Ok(pages)
}

/// Whether two page passes carry the same bytes.
fn same_pages(before: &[PageRead], after: &[PageRead]) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after.iter())
            .all(|(a, b)| a.page == b.page && a.bytes == b.bytes)
}

/// Performs one operation. The flag says whether the thread should stop.
#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive match over the wire operations reads as a single state machine"
)]
fn execute(
    transport: &mut RecordingTransport,
    protocol: &mut MsProtocol,
    op: IoOp,
    stop: &AtomicBool,
) -> (IoOutcome, bool) {
    match op {
        IoOp::Close => {
            transport.close();
            (IoOutcome::Closed, true)
        }
        IoOp::Signature => match protocol.signature(transport) {
            Ok(signature) => (IoOutcome::Signature { signature }, false),
            Err(error) => (failure(&error), false),
        },
        IoOp::ProbeSignature { timeout } => match protocol.probe_signature(transport, timeout) {
            Ok(signature) => (IoOutcome::Signature { signature }, false),
            Err(error) => (failure(&error), false),
        },
        IoOp::ReadPages { pages } => {
            let mut read = Vec::with_capacity(pages.len());
            for page in pages {
                if stop.load(Ordering::Relaxed) {
                    return (
                        IoOutcome::Failed(IoFailure {
                            kind: IoFailureKind::Stopped,
                            message: "the page read was stopped".to_owned(),
                        }),
                        true,
                    );
                }
                match protocol.read_page_all(transport, page) {
                    Ok(bytes) => read.push(PageRead { page, bytes }),
                    Err(error) => return (failure(&error), false),
                }
            }
            (IoOutcome::Pages { pages: read }, false)
        }
        IoOp::ReadAll => {
            // A board reset (every port open triggers one) leaves the firmware
            // loading its tune for a moment, and a read that races that load
            // returns the previous session's RAM for some pages. Read twice and
            // only answer with a pass that matches the one before it.
            let mut previous: Option<Vec<PageRead>> = None;
            for attempt in 0..STABLE_READ_ATTEMPTS {
                let pages = match read_every_page(protocol, transport, stop) {
                    Ok(pages) => pages,
                    Err(answer) => return answer,
                };
                if let Some(before) = &previous
                    && same_pages(before, &pages)
                {
                    return (IoOutcome::All { pages }, false);
                }
                previous = Some(pages);
                if attempt + 1 < STABLE_READ_ATTEMPTS {
                    std::thread::sleep(STABLE_READ_DELAY);
                }
            }
            // Still moving after the last attempt: answer with the last pass so
            // the caller sees values rather than an error, and let the write
            // path's own conflict check refuse a plan built from a moving target.
            let pages = previous.unwrap_or_default();
            (IoOutcome::All { pages }, false)
        }
        IoOp::WritePages { pages } => {
            let count = pages.len();
            let mut bytes = 0;
            for page in pages {
                if stop.load(Ordering::Relaxed) {
                    return (
                        IoOutcome::Failed(IoFailure {
                            kind: IoFailureKind::Stopped,
                            message: "the page write was stopped".to_owned(),
                        }),
                        true,
                    );
                }
                bytes += page.data.len();
                if let Err(error) = protocol.write_page(transport, page.page, 0, &page.data) {
                    return (failure(&error), false);
                }
            }
            (
                IoOutcome::Written {
                    pages: count,
                    bytes,
                },
                false,
            )
        }
        IoOp::BurnPages { pages } => {
            let count = pages.len();
            for page in pages {
                if stop.load(Ordering::Relaxed) {
                    return (
                        IoOutcome::Failed(IoFailure {
                            kind: IoFailureKind::Stopped,
                            message: "the burn was stopped".to_owned(),
                        }),
                        true,
                    );
                }
                if let Err(error) = protocol.burn(transport, page) {
                    return (failure(&error), false);
                }
            }
            (IoOutcome::Burned { pages: count }, false)
        }
        IoOp::Command { commands } => match protocol.run_commands(transport, &commands) {
            Ok(report) => (IoOutcome::Command { report }, false),
            Err(error) => (failure(&error), false),
        },
        IoOp::ReadRealtime => match protocol.read_realtime(transport) {
            Ok(block) => (IoOutcome::Realtime { block }, false),
            Err(error) => (failure(&error), false),
        },
        IoOp::Reconnect => {
            // A port that failed a read is still open; closing first makes the
            // reopen real instead of a no-op.
            transport.close();
            if let Err(error) = transport.open() {
                return (
                    IoOutcome::Failed(IoFailure {
                        kind: IoFailureKind::LinkLost,
                        message: error.to_string(),
                    }),
                    false,
                );
            }
            match protocol.signature(transport) {
                Ok(signature) => (IoOutcome::Reconnected { signature }, false),
                Err(error) => (failure(&error), false),
            }
        }
        IoOp::SaveTrace { path } => match transport.save(&path) {
            Ok(frames) => (IoOutcome::TraceSaved { path, frames }, false),
            Err(error) => (
                IoOutcome::Failed(IoFailure {
                    kind: IoFailureKind::Trace,
                    message: error.to_string(),
                }),
                false,
            ),
        },
    }
}

/// Maps a protocol error to the core-facing failure.
fn failure(error: &ProtocolError) -> IoOutcome {
    IoOutcome::Failed(IoFailure {
        kind: failure_kind(error),
        message: error.to_string(),
    })
}

fn failure_kind(error: &ProtocolError) -> IoFailureKind {
    match error {
        ProtocolError::Transport(TransportError::Timeout { .. }) => IoFailureKind::Timeout,
        ProtocolError::Transport(
            TransportError::Closed
            | TransportError::Read(_)
            | TransportError::Write(_)
            | TransportError::NotOpen
            | TransportError::Open(_),
        ) => IoFailureKind::LinkLost,
        ProtocolError::Transport(TransportError::Unsupported(_))
        | ProtocolError::Unsupported(_)
        | ProtocolError::Encoding(_) => IoFailureKind::Unsupported,
        // Both error enums are non-exhaustive, so a variant added later must
        // still map to a core-facing failure instead of breaking the build.
        _ => IoFailureKind::Protocol,
    }
}

/// Sends a reply, bounded so shutdown cannot hang.
fn send(replies: &crossfire::Tx<spsc::Array<IoReply>>, reply: IoReply) {
    let id = reply.id;
    if replies.send_timeout(reply, REPLY_TIMEOUT).is_err() {
        tracing::debug!(id, "the core did not read an I/O reply in time");
    }
}

/// The request id type, so a caller cannot confuse it with an [`OpId`].
///
/// [`OpId`]: crate::contract::OpId
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RequestId(pub(crate) u64);

impl RequestId {
    /// The first id of a connection.
    pub(crate) const FIRST: Self = Self(1);

    /// The next id.
    pub(crate) fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtuner_sim::{SimConfig, SimTransport};

    /// A test link: the simulator through the same seam the application uses.
    struct TestSim(SimConfig);

    impl crate::link::CustomLink for TestSim {
        fn label(&self) -> &'static str {
            "simulator"
        }

        fn open(&self, definition: &Definition) -> Result<Box<dyn Transport>, String> {
            SimTransport::new(definition, self.0.clone())
                .map(|sim| {
                    let transport: Box<dyn Transport> = Box::new(sim);
                    transport
                })
                .map_err(|error| error.to_string())
        }
    }

    fn definition() -> Definition {
        Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
pageReadCommand = \"p%2i%2o%2c\"
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
",
            &oxtuner_ini::SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn the_io_thread_answers_a_signature_query() {
        let definition = definition();
        let behavior = BehaviorKind::select(&definition).expect("a family");
        let plan = IoPlan::definition(
            Box::new(definition),
            behavior,
            None,
            PortChoice::custom(TestSim(SimConfig::default())),
        );
        let (thread, replies) = IoThread::start(&plan).expect("starts");
        thread.submit(1, IoOp::Signature).expect("queued");

        let reply = replies
            .recv_timeout(Duration::from_secs(2))
            .expect("a reply");
        assert!(
            matches!(reply.outcome, IoOutcome::Signature { .. }),
            "{:?}",
            reply.outcome
        );
        thread.shutdown();
    }

    #[test]
    fn request_ids_advance() {
        assert_eq!(RequestId::FIRST.next().0, 2);
    }

    #[test]
    fn a_plan_reports_the_link_it_will_open() {
        let link = PortChoice::serial("/dev/null", 115_200);
        assert_eq!(link.label(), "/dev/null");
        assert!(link.is_serial());

        let link = PortChoice::custom(TestSim(SimConfig::default()));
        assert!(!link.is_serial());
        assert_eq!(link.label(), "simulator");
    }
}
