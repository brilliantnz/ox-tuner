//! The UI-side handles: commands in, events out.
//!
//! Two endpoints, chosen for who is on each end (ADR-0005 rule 5):
//!
//! - **Commands** use the *blocking* sender but only ever `try_send`: the UI
//!   thread must never block on the core. A full queue is reported as
//!   [`CoreError::QueueFull`] and the caller decides what to tell the user.
//! - **Events** use the *async* receiver, so the UI can await them on GPUI's
//!   foreground executor without a polling loop or a second runtime.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crossfire::{MAsyncRx, MTx, TrySendError, mpmc};

use crate::message::{CellEdit, Command, Envelope, Event, OpId};
use crate::service::CoreError;

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
            .unwrap_or_else(|poison| poison.into_inner());
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
            .unwrap_or_else(|poison| poison.into_inner());
        *closed = true;
    }
}

/// Cloneable sender half of the core.
///
/// Cloning is cheap and every clone shares one op counter, so ids stay unique
/// even with several UI-side producers.
#[derive(Clone)]
pub struct CoreHandle {
    pub(crate) commands: MTx<mpmc::Array<Envelope>>,
    pub(crate) next_op: Arc<AtomicU64>,
    pub(crate) gate: Arc<Gate>,
}

impl CoreHandle {
    /// Queues a command, returning the id its reply will carry.
    ///
    /// Never blocks: a full queue is refused rather than waited on, because the
    /// caller is the UI thread.
    pub fn send(&self, command: Command) -> Result<OpId, CoreError> {
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
    pub fn load_definition(
        &self,
        label: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<OpId, CoreError> {
        self.send(Command::load_definition(label, bytes))
    }

    /// Opens a TunerStudio project folder in the core.
    ///
    /// The core reads and owns the definition and the tune; the reply is
    /// [`crate::Event::ProjectOpened`] or [`crate::Event::Failed`].
    pub fn open_project(&self, path: impl Into<PathBuf>) -> Result<OpId, CoreError> {
        self.send(Command::OpenProject { path: path.into() })
    }

    /// Writes the open project's changed values back to its tune file.
    pub fn save_tune(&self) -> Result<OpId, CoreError> {
        self.send(Command::SaveTune)
    }

    /// Reads one table from the open project.
    ///
    /// The reply is [`crate::Event::TableRead`] or [`crate::Event::Failed`].
    pub fn read_table(&self, name: impl Into<String>) -> Result<OpId, CoreError> {
        self.send(Command::ReadTable { name: name.into() })
    }

    /// Applies user-unit cell edits to one table.
    ///
    /// The reply is [`crate::Event::TableEdited`] or [`crate::Event::Failed`].
    pub fn edit_table(
        &self,
        name: impl Into<String>,
        edits: Vec<CellEdit>,
    ) -> Result<OpId, CoreError> {
        self.send(Command::EditTable {
            name: name.into(),
            edits,
        })
    }

    /// Applies user-unit bin edits to one table axis.
    ///
    /// The reply is [`crate::Event::TableEdited`] or [`crate::Event::Failed`].
    pub fn edit_axis(
        &self,
        table: impl Into<String>,
        axis: crate::message::AxisKind,
        edits: Vec<crate::message::AxisEdit>,
    ) -> Result<OpId, CoreError> {
        self.send(Command::EditAxis {
            table: table.into(),
            axis,
            edits,
        })
    }

    /// Reads one dialog from the open project.
    ///
    /// The reply is [`crate::Event::DialogRead`] or [`crate::Event::Failed`].
    pub fn read_dialog(&self, name: impl Into<String>) -> Result<OpId, CoreError> {
        self.send(Command::ReadDialog { name: name.into() })
    }

    /// Applies a user-unit scalar edit from one dialog.
    ///
    /// The reply is [`crate::Event::DialogEdited`] or [`crate::Event::Failed`].
    pub fn edit_scalar(
        &self,
        dialog: impl Into<String>,
        name: impl Into<String>,
        value: f64,
    ) -> Result<OpId, CoreError> {
        self.send(Command::EditScalar {
            dialog: dialog.into(),
            name: name.into(),
            value,
        })
    }

    /// Connects to the simulator or a serial device.
    ///
    /// The reply is [`crate::Event::Connected`] or [`crate::Event::Failed`].
    /// `framing` overrides the definition's own choice of protocol envelope.
    pub fn connect(
        &self,
        port: crate::PortChoice,
        framing: Option<crate::LinkMode>,
    ) -> Result<OpId, CoreError> {
        self.send(Command::Connect { port, framing })
    }

    /// Reads a port's signature without opening a session.
    ///
    /// The reply is [`crate::Event::ConnectionTested`] or [`crate::Event::Failed`].
    pub fn test_connection(
        &self,
        port: crate::PortChoice,
        framing: Option<crate::LinkMode>,
    ) -> Result<OpId, CoreError> {
        self.send(Command::TestConnection { port, framing })
    }

    /// Closes the connection.
    pub fn disconnect(&self) -> Result<OpId, CoreError> {
        self.send(Command::Disconnect)
    }

    /// Reads every declared page into the tune images.
    pub fn read_all(&self) -> Result<OpId, CoreError> {
        self.send(Command::ReadAll)
    }

    /// Starts periodic realtime polling.
    pub fn start_realtime(&self, interval_ms: u32) -> Result<OpId, CoreError> {
        self.send(Command::StartRealtime { interval_ms })
    }

    /// Stops periodic realtime polling.
    pub fn stop_realtime(&self) -> Result<OpId, CoreError> {
        self.send(Command::StopRealtime)
    }

    /// Saves the session trace.
    pub fn save_trace(&self, path: impl Into<PathBuf>) -> Result<OpId, CoreError> {
        self.send(Command::SaveTrace { path: path.into() })
    }

    /// Lists the serial ports the connect dialog can offer.
    pub fn list_ports(&self) -> Result<OpId, CoreError> {
        self.send(Command::ListPorts)
    }

    /// Drops the loaded definition (and tune, if a project is open).
    pub fn unload_definition(&self) -> Result<OpId, CoreError> {
        self.send(Command::UnloadDefinition)
    }

    /// Asks the core to stop. The events stream reports [`Event::Stopped`].
    ///
    /// Unlike the other commands this one may block briefly: quitting must not
    /// be lost to a full queue, and the caller is on its way out anyway.
    pub fn shutdown(&self) -> Result<OpId, CoreError> {
        let op = self.next_op();
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
    pub(crate) events: MAsyncRx<mpmc::Array<Event>>,
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
    events: crossfire::MRx<mpmc::Array<Event>>,
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
