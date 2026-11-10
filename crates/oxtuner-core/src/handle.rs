//! The UI-side handles: commands in, events out.
//!
//! Two endpoints, chosen for who is on each end:
//!
//! - **Commands** are a bounded **MPSC** queue: many `CoreHandle` clones, one
//!   core thread. The sender is the blocking flavor but only ever `try_send`s:
//!   the UI thread must never block on the core. A full queue is reported as
//!   [`CoreError::QueueFull`] and the caller decides what to tell the user.
//! - **Events** are a bounded **SPSC** queue: the core thread produces, one
//!   event pump consumes. The receiver is the async flavor, so the UI can await
//!   it on GPUI's foreground executor without a polling loop or a second
//!   runtime. The single-consumer type makes a second pump that would silently
//!   steal events a compile error instead of a design review note.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use crossfire::{AsyncRx, MTx, Rx, SendTimeoutError, TrySendError, Tx, mpsc, spsc};

use crate::contract::{CellEdit, CommandRequest, CoreCommand, Envelope, Event, OpId};
use crate::service::CoreError;

/// How long one attempt to hand an event to the UI may wait before the core
/// looks at the shutdown flag again.
///
/// Short enough that a quit is never held up by a stalled consumer, long enough
/// that a healthy UI costs at most a few wakeups per second while busy.
const EVENT_SLICE: Duration = Duration::from_millis(25);

/// Set when shutdown has been asked for and accepted.
///
/// Shared by the handle, the thread guard and the event sender. Its only job is
/// to turn a potentially endless wait for event-queue room into a bounded one:
/// once quitting has started, a consumer that never reads must not be able to
/// keep the core thread alive.
#[derive(Debug, Default)]
pub(crate) struct StopFlag {
    requested: AtomicBool,
}

impl StopFlag {
    /// Records that shutdown has been accepted.
    pub(crate) fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }

    /// Whether shutdown has been accepted.
    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

/// Why an event was not handed to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendFailure {
    /// The UI dropped its receiver, so no event can ever arrive again.
    Closed,
    /// Shutdown was requested while the queue stayed full, so the event was
    /// dropped instead of keeping the core thread alive. Counted in
    /// [`crate::CoreReport::dropped_events`].
    Dropped,
}

/// The core's event producer. One instance, on the core thread.
///
/// Delivery is lossless while the UI is reading: a full queue is waited on, so
/// every accepted command really is answered. The wait is taken in
/// [`EVENT_SLICE`] steps, and once [`StopFlag`] says shutdown has been accepted
/// an event that still does not fit is dropped and counted rather than blocking
/// the thread — otherwise a consumer that stopped reading could keep the core
/// alive forever and hang quitting.
pub(crate) struct EventTx {
    events: Tx<spsc::Array<Event>>,
    stop: Arc<StopFlag>,
    dropped: AtomicU64,
}

impl EventTx {
    /// Wraps the channel's producer with the shutdown flag it observes.
    pub(crate) fn new(events: Tx<spsc::Array<Event>>, stop: Arc<StopFlag>) -> Self {
        Self {
            events,
            stop,
            dropped: AtomicU64::new(0),
        }
    }

    /// Hands one event to the UI.
    pub(crate) fn send(&self, mut event: Event) -> Result<(), SendFailure> {
        loop {
            match self.events.send_timeout(event, EVENT_SLICE) {
                Ok(()) => return Ok(()),
                Err(SendTimeoutError::Disconnected(_)) => return Err(SendFailure::Closed),
                Err(SendTimeoutError::Timeout(returned)) => {
                    event = returned;
                    if self.stop.is_requested() {
                        let dropped = self.dropped.fetch_add(1, Ordering::Relaxed);
                        if dropped == 0 {
                            tracing::warn!(
                                op = ?event.op(),
                                "the UI stopped reading; events are dropped so shutdown can finish"
                            );
                        } else {
                            tracing::debug!(
                                op = ?event.op(),
                                dropped = dropped + 1,
                                "event dropped during shutdown"
                            );
                        }
                        return Err(SendFailure::Dropped);
                    }
                }
            }
        }
    }

    /// Events that could not be delivered because shutdown was in progress.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The door commands pass through, so "shutdown" has one exact meaning.
///
/// Without it, a command could be queued between the core's last drain and the
/// moment it drops the receiver: accepted by the channel, never answered. That
/// would quietly break the crate's central promise ([`crate::CoreHandle::send`]
/// returns an id **because** it will be answered).
///
/// Holding this lock while enqueueing, and taking it once when the core stops,
/// gives the two sides a single ordering point: a command either enters before
/// the core closes (and is then answered, handled or `Rejected`) or is refused
/// with [`CoreError::Stopped`]. The lock is uncontended in normal operation —
/// one short critical section per command.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    closed: Mutex<bool>,
}

impl Gate {
    /// Runs `accept` while the queue is still open, or reports `Stopped`.
    fn while_open<T>(&self, accept: impl FnOnce() -> Result<T, CoreError>) -> Result<T, CoreError> {
        let closed = self
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *closed {
            return Err(CoreError::Stopped);
        }
        accept()
    }

    /// Closes the door. Every later send is refused.
    pub(crate) fn close(&self) {
        let mut closed = self
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *closed = true;
    }
}

/// Cloneable sender half of the core.
///
/// Cloning is cheap and every clone shares one op counter, so ids stay unique
/// even with several UI-side producers.
#[derive(Clone)]
pub struct CoreHandle {
    pub(crate) commands: MTx<mpsc::Array<Envelope>>,
    pub(crate) next_op: Arc<AtomicU64>,
    pub(crate) gate: Arc<Gate>,
    pub(crate) stop: Arc<StopFlag>,
}

impl std::fmt::Debug for CoreHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("CoreHandle").finish_non_exhaustive()
    }
}

impl CoreHandle {
    /// Queues a command, returning the id its reply will carry.
    ///
    /// Never blocks: a full queue is refused rather than waited on, because the
    /// caller is the UI thread.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn execute(&self, command: CoreCommand) -> Result<OpId, CoreError> {
        self.send_request(command.into())
    }

    /// Queues an internal request, returning the id its reply will carry.
    pub(crate) fn send_request(&self, command: CommandRequest) -> Result<OpId, CoreError> {
        let op = self.next_op();
        self.gate
            .while_open(|| match self.commands.try_send(Envelope { op, command }) {
                Ok(()) => {
                    tracing::trace!(%op, "command queued");
                    Ok(op)
                }
                Err(TrySendError::Full(_)) => {
                    tracing::warn!(%op, "the core command queue is full; command refused");
                    Err(CoreError::QueueFull)
                }
                Err(TrySendError::Disconnected(_)) => Err(CoreError::Stopped),
            })
    }

    /// The next id for a command about to be sent.
    fn next_op(&self) -> OpId {
        OpId::new(self.next_op.fetch_add(1, Ordering::Relaxed))
    }

    /// Loads a definition file that has already been read into memory.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn load_definition(
        &self,
        label: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::load_definition(label, bytes))
    }

    /// Opens a `TunerStudio` project folder in the core.
    ///
    /// The core reads and owns the definition and the tune; the reply is
    /// [`crate::Event::ProjectOpened`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn open_project(&self, path: impl Into<PathBuf>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::OpenProject { path: path.into() })
    }

    /// Writes the open project's changed values back to its tune file.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn save_tune(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::SaveTune)
    }

    /// Reads one table from the open project.
    ///
    /// The reply is [`crate::Event::TableRead`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn read_table(&self, name: impl Into<String>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::ReadTable { name: name.into() })
    }

    /// Applies user-unit cell edits to one table.
    ///
    /// The reply is [`crate::Event::TableEdited`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn edit_table(
        &self,
        name: impl Into<String>,
        edits: Vec<CellEdit>,
    ) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::EditTable {
            name: name.into(),
            edits,
        })
    }

    /// Applies user-unit bin edits to one table axis.
    ///
    /// The reply is [`crate::Event::TableEdited`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn edit_axis(
        &self,
        table: impl Into<String>,
        axis: crate::contract::AxisKind,
        edits: Vec<crate::contract::AxisEdit>,
    ) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::EditAxis {
            table: table.into(),
            axis,
            edits,
        })
    }

    /// Reads one dialog from the open project.
    ///
    /// The reply is [`crate::Event::DialogRead`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn read_dialog(&self, name: impl Into<String>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::ReadDialog { name: name.into() })
    }

    /// Applies a user-unit scalar edit from one dialog.
    ///
    /// The reply is [`crate::Event::DialogEdited`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn edit_scalar(
        &self,
        dialog: impl Into<String>,
        name: impl Into<String>,
        value: f64,
    ) -> Result<OpId, CoreError> {
        self.edit_scalar_at(dialog, name, None, value)
    }

    /// Applies one scalar or array-element edit from a dialog.
    ///
    /// The reply is [`crate::Event::DialogEdited`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn edit_scalar_at(
        &self,
        dialog: impl Into<String>,
        name: impl Into<String>,
        index: Option<usize>,
        value: f64,
    ) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::EditScalar {
            dialog: dialog.into(),
            name: name.into(),
            index,
            value,
        })
    }

    /// Runs one `[ControllerCommands]` action from a dialog button.
    ///
    /// The reply is [`crate::Event::ControllerActionRan`] or
    /// [`crate::Event::Failed`]. The action passes the same safety gate as a
    /// write, and the ECU image is stale after it succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn run_controller_command(
        &self,
        dialog: impl Into<String>,
        command: impl Into<String>,
    ) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::RunControllerCommand {
            dialog: dialog.into(),
            command: command.into(),
        })
    }

    /// Connects over a serial device or a link the application supplies.
    ///
    /// The reply is [`crate::Event::Connected`] or [`crate::Event::Failed`].
    /// `framing` overrides the definition's own choice of protocol envelope.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn connect(
        &self,
        port: crate::PortChoice,
        framing: Option<crate::LinkMode>,
    ) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::Connect { port, framing })
    }

    /// Reads a port's signature without opening a session.
    ///
    /// The reply is [`crate::Event::ConnectionTested`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn test_connection(
        &self,
        port: crate::PortChoice,
        framing: Option<crate::LinkMode>,
    ) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::TestConnection { port, framing })
    }

    /// Closes the connection.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn disconnect(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::Disconnect)
    }

    /// Reads every declared page into the tune images.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn read_all(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::ReadAll)
    }

    /// Takes the ECU image the last read captured as the working tune.
    ///
    /// The reply is [`crate::Event::EcuAdopted`] or [`crate::Event::Failed`].
    /// Use it after a read when the tune on the controller, not the project
    /// file, is what the next edit and write should start from.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn adopt_ecu_image(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::AdoptEcuImage)
    }

    /// Starts periodic realtime polling.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn start_realtime(&self, interval_ms: u32) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::StartRealtime { interval_ms })
    }

    /// Stops periodic realtime polling.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn stop_realtime(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::StopRealtime)
    }

    /// Saves the session trace.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn save_trace(&self, path: impl Into<PathBuf>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::SaveTrace { path: path.into() })
    }

    /// Writes the changed working values to ECU RAM and verifies them.
    ///
    /// `pages` restricts the write; an empty list means every page with a
    /// change. The reply is [`crate::Event::TuneWritten`] or
    /// [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn write_to_ram(&self, pages: Vec<u8>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::WriteToRam { pages })
    }

    /// Burns RAM to flash and verifies it.
    ///
    /// `pages` restricts the burn; an empty list means every burnable page.
    /// The reply is [`crate::Event::TuneBurned`] or [`crate::Event::Failed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn burn_to_flash(&self, pages: Vec<u8>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::BurnToFlash { pages })
    }

    /// Discards local edits after writing a snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn revert_tune(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::RevertTune)
    }

    /// Discards the local edits one table, curve or dialog owns.
    ///
    /// The reply is [`crate::Event::TableEdited`] and/or
    /// [`crate::Event::DialogEdited`], plus [`crate::Event::TuneStatusRead`].
    /// Every other target keeps its edits.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn revert_target(&self, name: impl Into<String>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::RevertTarget { name: name.into() })
    }

    /// Imports a `.msq` into the working image.
    ///
    /// A different-signature tune needs `consented: true`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn import_tune(
        &self,
        path: impl Into<PathBuf>,
        consented: bool,
    ) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::ImportTune {
            path: path.into(),
            consented,
        })
    }

    /// Restores one of the project's snapshots.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn restore_snapshot(&self, path: impl Into<PathBuf>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::RestoreSnapshot { path: path.into() })
    }

    /// Lists the project's snapshots, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn list_snapshots(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::ListSnapshots)
    }

    /// Turns the global write switch on or off.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn set_writes_enabled(&self, enabled: bool) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::SetWritesEnabled { enabled })
    }

    /// Clears the halted write path after the user reviewed a failure.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn acknowledge_write_failure(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::AcknowledgeWriteFailure)
    }

    /// Reads the project's write journal.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn read_journal(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::ReadJournal)
    }

    /// Refreshes the derived tune status.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn refresh_tune_status(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::RefreshTuneStatus)
    }

    /// Starts a CSV session log in `directory`.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn start_log(&self, directory: impl Into<PathBuf>) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::StartLog {
            directory: directory.into(),
        })
    }

    /// Stops the CSV session log.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn stop_log(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::StopLog)
    }

    /// Lists the serial ports the connect dialog can offer.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn list_ports(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::ListPorts)
    }

    /// Drops the loaded definition (and tune, if a project is open).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn unload_definition(&self) -> Result<OpId, CoreError> {
        self.send_request(CommandRequest::UnloadDefinition)
    }

    /// Asks the core to stop. The events stream reports [`Event::Stopped`].
    ///
    /// Unlike the other commands this one may block briefly: quitting must not
    /// be lost to a full queue, and the caller is on its way out anyway. The
    /// stop flag is raised first, so a core blocked handing an answer to a UI
    /// that stopped reading can still reach this command; from that point on
    /// delivery is best effort and undelivered events are counted instead of
    /// blocking the thread.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::QueueFull`] or [`CoreError::Stopped`] when the
    /// command cannot be queued.
    pub fn shutdown(&self) -> Result<OpId, CoreError> {
        let op = self.next_op();
        self.stop.request();
        self.gate.while_open(|| {
            crate::service::send_shutdown(&self.commands, op)?;
            Ok(op)
        })
    }

    /// Whether the core thread is still reachable.
    pub fn is_running(&self) -> bool {
        !self.commands.is_disconnected()
    }
}

/// Receives core events on an async executor (the UI's foreground task).
pub struct EventStream {
    pub(crate) events: AsyncRx<spsc::Array<Event>>,
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventStream")
            .finish_non_exhaustive()
    }
}

impl EventStream {
    /// Waits for the next event. `None` means the core is gone.
    ///
    /// The future borrows the stream, so it composes with `cx.spawn` without
    /// cloning or buffering anything.
    pub async fn recv(&self) -> Option<Event> {
        self.events.recv().await.ok()
    }

    /// Takes an event if one is already waiting.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Whether the core side has closed the channel.
    pub fn is_closed(&self) -> bool {
        self.events.is_disconnected()
    }

    /// Switches to the blocking receiver, for headless callers and tests.
    ///
    /// `recv_timeout` reports both "nothing yet" and "closed" as `None`; use
    /// [`BlockingEventStream::is_closed`] to tell them apart. [`Event::Stopped`]
    /// is always the last message either way, so a loop can also stop on it.
    pub fn into_blocking(self) -> BlockingEventStream {
        BlockingEventStream {
            events: self.events.into_blocking(),
        }
    }
}

/// The blocking counterpart of [`EventStream`], for tests and CLI tools.
pub struct BlockingEventStream {
    events: Rx<spsc::Array<Event>>,
}

impl std::fmt::Debug for BlockingEventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlockingEventStream")
            .finish_non_exhaustive()
    }
}

impl BlockingEventStream {
    /// Waits for the next event. `None` means the core is gone.
    pub fn recv(&self) -> Option<Event> {
        self.events.recv().ok()
    }

    /// Waits up to `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Takes an event if one is already waiting.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Whether the core side has closed the channel.
    pub fn is_closed(&self) -> bool {
        self.events.is_disconnected()
    }
}
