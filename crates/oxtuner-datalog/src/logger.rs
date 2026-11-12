//! The logger thread and the live status the UI reads.

use std::{
    fs::File,
    io::{self, BufWriter, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossfire::{RecvTimeoutError, TrySendError, spsc};

use crate::csv::{LogColumn, header, row};

/// How many frames may wait for the writer. At 100 Hz this is over a minute of
/// slack; a faster producer is dropping frames because the disk cannot keep up,
/// which is exactly the condition the drop counter reports.
const QUEUE_CAPACITY: usize = 8_192;

/// How often buffered rows are flushed even when the queue is quiet.
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Flush after this many rows regardless of time, so a fast session does not
/// accumulate a large unwritten buffer.
const FLUSH_ROWS: u64 = 64;

/// How long stopping may wait for room to queue the stop message.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// A session logger: one bounded queue, one writer thread.
///
/// The poller holds a [`Logger`] and calls [`Logger::try_log`], which never
/// blocks. Dropping it flushes and joins; [`Logger::stop`] also reports what the
/// session wrote.
pub struct Logger {
    sender: Option<crossfire::Tx<spsc::Array<Message>>>,
    join: Option<JoinHandle<io::Result<()>>>,
    status: LogStatus,
    path: PathBuf,
}

/// One message for the writer thread.
enum Message {
    /// A decoded frame, in user units.
    Frame { time: f64, values: Vec<f64> },
    /// Flush and finish.
    Stop,
}

/// The live view of a running (or last) log.
///
/// Cloneable; the UI reads it on its render tick instead of waiting for a
/// per-frame event.
#[derive(Debug, Clone, Default)]
pub struct LogStatus {
    inner: Arc<StatusInner>,
}

#[derive(Debug, Default)]
struct StatusInner {
    running: AtomicBool,
    frames: AtomicU64,
    dropped: AtomicU64,
    bytes: AtomicU64,
    path: RwLock<Option<PathBuf>>,
}

/// A frozen copy of [`LogStatus`], for one render or one dialog.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionLogSnapshot {
    /// Whether the writer thread is accepting frames.
    pub running: bool,
    /// The file being written, when a log has been started.
    pub path: Option<PathBuf>,
    /// Frames written to the file.
    pub frames: u64,
    /// Frames refused because the writer queue was full.
    pub dropped: u64,
    /// Bytes written, counted after formatting.
    pub bytes: u64,
}

/// What a finished log wrote.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLogReport {
    /// The CSV file.
    pub path: PathBuf,
    /// Frames written.
    pub frames: u64,
    /// Frames the queue refused.
    pub dropped: u64,
    /// Bytes written.
    pub bytes: u64,
}

impl SessionLogSnapshot {
    /// Builds a snapshot, for callers that keep their own copy (the app's
    /// output panel does, after a stop event).
    pub fn new(
        running: bool,
        path: Option<PathBuf>,
        frames: u64,
        dropped: u64,
        bytes: u64,
    ) -> Self {
        Self {
            running,
            path,
            frames,
            dropped,
            bytes,
        }
    }
}

impl LogStatus {
    /// A status that is not running.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the writer is accepting frames.
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Relaxed)
    }

    /// The current file, while one is open.
    ///
    /// # Panics
    ///
    /// Panics when a writer panicked while holding the status lock.
    pub fn path(&self) -> Option<PathBuf> {
        self.inner
            .path
            .read()
            .expect("a previous panic poisoned the log status; the path is unverified")
            .clone()
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.inner.frames.load(Ordering::Relaxed)
    }

    /// Frames refused so far.
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Bytes written so far.
    pub fn bytes(&self) -> u64 {
        self.inner.bytes.load(Ordering::Relaxed)
    }

    /// A frozen copy for rendering.
    ///
    /// # Panics
    ///
    /// Panics when an earlier holder panicked while the status lock was held.
    pub fn snapshot(&self) -> SessionLogSnapshot {
        SessionLogSnapshot {
            running: self.is_running(),
            path: self.path(),
            frames: self.frames(),
            dropped: self.dropped(),
            bytes: self.bytes(),
        }
    }

    /// Marks the status stopped without a logger (session teardown).
    pub fn mark_stopped(&self) {
        self.stopped();
    }

    fn started(&self, path: &Path) {
        self.inner.running.store(true, Ordering::Relaxed);
        self.inner.frames.store(0, Ordering::Relaxed);
        self.inner.dropped.store(0, Ordering::Relaxed);
        self.inner.bytes.store(0, Ordering::Relaxed);
        *self
            .inner
            .path
            .write()
            .expect("a previous panic poisoned the log status; the path is unverified") =
            Some(path.to_owned());
    }

    fn stopped(&self) {
        self.inner.running.store(false, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Logger")
            .field("path", &self.path)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl Logger {
    /// Creates `path`, writes the header and starts the writer thread.
    ///
    /// The header write happens before the thread starts, so an unwritable file
    /// is an error the caller can report instead of a silently empty log.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the parent directory cannot be created, the
    /// file cannot be opened, or the writer thread cannot be spawned.
    pub fn start(
        path: impl AsRef<Path>,
        columns: Vec<LogColumn>,
        status: LogStatus,
    ) -> io::Result<Self> {
        let path = path.as_ref().to_owned();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File::create(&path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(header(&columns).as_bytes())?;
        writer.flush()?;

        let (sender, receiver) = spsc::bounded_blocking(QUEUE_CAPACITY);
        let thread_status = status.clone();
        let join = thread::Builder::new()
            .name("oxtuner-datalog".to_owned())
            .spawn(move || writer_loop(writer, &columns, &receiver, &thread_status))?;

        status.started(&path);
        Ok(Self {
            sender: Some(sender),
            join: Some(join),
            status,
            path,
        })
    }

    /// Queues one frame. Never blocks.
    ///
    /// Returns `false` when the frame was dropped, and counts it. A stopped
    /// logger also refuses frames; the caller can inspect [`Self::status`].
    pub fn try_log(&self, time: f64, values: &[f64]) -> bool {
        let Some(sender) = &self.sender else {
            return false;
        };
        let message = Message::Frame {
            time,
            values: values.to_vec(),
        };
        match sender.try_send(message) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.status.inner.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.status.stopped();
                false
            }
        }
    }

    /// The live status, for the UI.
    pub fn status(&self) -> &LogStatus {
        &self.status
    }

    /// The file being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stops the writer, flushes and returns what the session wrote.
    ///
    /// # Errors
    ///
    /// Returns the writer thread's final I/O error, including a panic in the
    /// thread.
    pub fn stop(mut self) -> io::Result<SessionLogReport> {
        self.finish()?;
        Ok(SessionLogReport {
            path: self.path.clone(),
            frames: self.status.frames(),
            dropped: self.status.dropped(),
            bytes: self.status.bytes(),
        })
    }

    fn finish(&mut self) -> io::Result<()> {
        if let Some(sender) = self.sender.take() {
            // The stop message must not be lost to a full queue. If it cannot
            // be queued, dropping the sender ends the loop after the drain and
            // the writer still flushes on its way out.
            let _ = sender.send_timeout(Message::Stop, STOP_WAIT);
        }
        let result = self.join.take().map_or(Ok(()), |join| {
            join.join()
                .unwrap_or_else(|_| Err(io::Error::other("the datalog thread panicked")))
        });
        self.status.stopped();
        result
    }
}

impl Drop for Logger {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            tracing::warn!(%error, "the datalog writer did not stop cleanly");
        }
    }
}

/// The writer loop: format, buffer, flush, finish.
fn writer_loop(
    mut writer: BufWriter<File>,
    columns: &[LogColumn],
    receiver: &crossfire::Rx<spsc::Array<Message>>,
    status: &LogStatus,
) -> io::Result<()> {
    let mut since_flush = 0u64;
    let mut last_flush = Instant::now();
    let mut frames = 0u64;
    let mut bytes = 0u64;

    loop {
        match receiver.recv_timeout(FLUSH_INTERVAL) {
            Ok(Message::Frame { time, values }) => {
                let line = row(time, columns, &values);
                writer.write_all(line.as_bytes())?;
                frames = frames.saturating_add(1);
                let line_bytes = u64::try_from(line.len()).unwrap_or(u64::MAX);
                bytes = bytes.saturating_add(line_bytes);
                since_flush = since_flush.saturating_add(1);
                status.inner.frames.store(frames, Ordering::Relaxed);
                status.inner.bytes.store(bytes, Ordering::Relaxed);
            }
            // The sender is gone or Stop arrived: finish and flush.
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            // A quiet queue still gets its buffered rows written, so a crash
            // cannot lose the last half second of a session.
            Err(RecvTimeoutError::Timeout) => {}
        }

        if since_flush >= FLUSH_ROWS || last_flush.elapsed() >= FLUSH_INTERVAL {
            writer.flush()?;
            since_flush = 0;
            last_flush = Instant::now();
        }
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        std::env::temp_dir().join(format!(
            "oxtuner-datalog-{}-{stamp}.csv",
            std::process::id()
        ))
    }

    #[test]
    fn a_quiet_writer_flushes_its_rows_on_the_interval() {
        let path = temp_path();
        let columns = vec![LogColumn::new("rpm", "rpm")];
        let logger = Logger::start(&path, columns, LogStatus::new()).expect("starts");
        assert!(logger.try_log(0.0, &[1000.0]));

        std::thread::sleep(FLUSH_INTERVAL + Duration::from_millis(250));

        let text = std::fs::read_to_string(&path).expect("reads the log");
        assert!(
            text.contains("1000"),
            "the row reached the file before stop: {text:?}"
        );

        let report = logger.stop().expect("stops");
        assert_eq!(report.frames, 1);
        let _ = std::fs::remove_file(&path);
    }
}
