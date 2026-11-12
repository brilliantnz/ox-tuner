//! The real serial port (`serial2`).
//!
//! Blocking I/O with per-call timeouts, used by the I/O thread only (ADR-0008).
//! The port is configured raw 8N1 with no flow control — what every Speeduino,
//! rusEFI and MegaSquirt-family ECU expects — at the baud rate the connection
//! chose.
//!
//! `serial2` was chosen over `serialport-rs` for the core path: it has no
//! `libudev` requirement on Linux (one less system package in CI and for users),
//! a simpler blocking API, and an explicit per-call read timeout. The trade-off
//! is that [`ports`] returns device paths without product descriptions, so the
//! connect dialog shows the path itself.

use std::io::{ErrorKind, Read};
use std::time::{Duration, Instant};

use serial2::SerialPort;

use crate::Transport;
use crate::error::TransportError;

/// How long a single `read` waits before returning "nothing yet".
///
/// Short enough that [`Transport::read_until`] can notice a quiet line, long
/// enough to avoid spinning.
const READ_POLL: Duration = Duration::from_millis(20);

/// An upper bound on a single write.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Reads once, turning `Ok(0)` into end of file.
///
/// `std::io::Read` defines a zero-length read on a non-empty buffer as EOF.
/// A pulled cable reports exactly that, so the conversation ends instead of
/// spinning on `Ok(0)` until the deadline and then reporting a timeout.
fn read_once(mut read: impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }

    match read.read(buf) {
        Ok(0) => Err(std::io::Error::from(ErrorKind::UnexpectedEof)),
        other => other,
    }
}

/// One port the connect dialog can offer.
///
/// `serial2` lists device paths; on macOS the callout `/dev/cu.*` entry is the
/// one to open, and picking the blocking `/dev/tty.*` twin makes the I/O thread
/// look hung (P5 finding 2), so [`ports`] filters the duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    /// Device path (`/dev/cu.usbmodem21201`, `COM3`).
    ///
    /// Private so a later metadata field (manufacturer, serial number) can be
    /// added without a second accessor or a breaking change. Use [`Self::label`].
    name: String,
}

impl PortInfo {
    /// The label to show for this port.
    pub fn label(&self) -> &str {
        &self.name
    }

    /// The device path, which is the port's stable identity.
    ///
    /// Equal to [`Self::label`] until a backend can report a product
    /// description; the two are separate accessors so they may diverge then.
    pub fn path(&self) -> &str {
        &self.name
    }
}

/// Which port and baud rate to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialSettings {
    /// Device path.
    pub port: String,
    /// Bits per second.
    pub baud: u32,
}

impl SerialSettings {
    /// The speed every Speeduino ships with.
    ///
    /// rusEFI and FOME use the same default; a definition that needs another
    /// speed says so in the connection settings.
    pub const DEFAULT_BAUD: u32 = 115_200;

    /// Settings for `port` at the default speed.
    pub fn new(port: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            baud: Self::DEFAULT_BAUD,
        }
    }

    /// The same port at another speed.
    #[must_use]
    pub fn with_baud(mut self, baud: u32) -> Self {
        self.baud = baud;
        self
    }
}

/// A serial byte transport.
pub struct SerialTransport {
    settings: SerialSettings,
    port: Option<SerialPort>,
}

impl std::fmt::Debug for SerialTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SerialTransport")
            .field("settings", &self.settings)
            .field("open", &self.port.is_some())
            .finish()
    }
}

impl SerialTransport {
    /// A transport for `settings`, not yet open.
    pub fn new(settings: SerialSettings) -> Self {
        Self {
            settings,
            port: None,
        }
    }

    /// The settings this transport opens with.
    pub fn settings(&self) -> &SerialSettings {
        &self.settings
    }

    fn port(&self) -> Result<&SerialPort, TransportError> {
        self.port.as_ref().ok_or(TransportError::NotOpen)
    }

    fn port_mut(&mut self) -> Result<&mut SerialPort, TransportError> {
        self.port.as_mut().ok_or(TransportError::NotOpen)
    }

    fn set_timeout(&mut self, timeout: Duration) -> Result<(), TransportError> {
        self.port_mut()?
            .set_read_timeout(timeout)
            .map_err(|error| TransportError::Read(error.to_string()))
    }
}

impl Transport for SerialTransport {
    fn open(&mut self) -> Result<(), TransportError> {
        if self.port.is_some() {
            return Ok(());
        }
        // A `u32` asks `serial2` for raw 8N1 with no parity and no flow control.
        let mut port =
            SerialPort::open(&self.settings.port, self.settings.baud).map_err(|error| {
                // Busy and permission problems are the two normal failures, and
                // their text is what tells the user which one happened.
                TransportError::Open(format!("{}: {error}", self.settings.port))
            })?;
        port.set_read_timeout(READ_POLL)
            .and_then(|()| port.set_write_timeout(WRITE_TIMEOUT))
            .map_err(|error| TransportError::Open(error.to_string()))?;
        tracing::info!(
            port = %self.settings.port,
            baud = self.settings.baud,
            "serial port opened"
        );
        self.port = Some(port);
        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        let port = self.port()?;
        port.write_all(bytes)
            .and_then(|()| port.flush())
            .map_err(|error| TransportError::Write(error.to_string()))
    }

    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<(), TransportError> {
        if buf.is_empty() {
            return Ok(());
        }
        // The requested deadline is the whole conversation's; a slow ECU sends
        // in pieces, so each `read` only needs to make progress before it.
        self.set_timeout(timeout)?;
        let deadline = Instant::now() + timeout;
        let mut filled = 0usize;
        while filled < buf.len() {
            let now = Instant::now();
            if now >= deadline {
                return Err(TransportError::Timeout {
                    got: filled,
                    expected: buf.len(),
                });
            }
            self.set_timeout(deadline - now)?;
            let port = self.port()?;
            match read_once(port, &mut buf[filled..]) {
                Ok(read) => filled += read,
                Err(error) if error.kind() == ErrorKind::TimedOut => {
                    return Err(TransportError::Timeout {
                        got: filled,
                        expected: buf.len(),
                    });
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                    return Err(TransportError::Closed);
                }
                Err(error) => return Err(TransportError::Read(error.to_string())),
            }
        }
        Ok(())
    }

    fn read_until(
        &mut self,
        sink: &mut Vec<u8>,
        max: usize,
        terminator: u8,
        quiet: Duration,
        timeout: Duration,
    ) -> Result<(), TransportError> {
        let before = sink.len();
        let deadline = Instant::now() + timeout;
        let mut buffer = [0u8; 128];

        loop {
            if sink.len() - before >= max {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return if sink.len() == before {
                    Err(TransportError::Timeout {
                        got: 0,
                        expected: 1,
                    })
                } else {
                    Ok(())
                };
            }
            // Re-arm the OS timeout for whichever comes first, the quiet
            // window or the conversation deadline, so a late reply cannot
            // overshoot the deadline by a whole quiet period.
            self.set_timeout(quiet.min(deadline - now))?;

            // Never read more than the caller asked to keep: bytes taken
            // from the OS and then discarded would desynchronise the next
            // conversation.
            let remaining = max - (sink.len() - before);
            let take = remaining.min(buffer.len());
            let port = self.port()?;
            match read_once(port, &mut buffer[..take]) {
                Ok(read) => {
                    sink.extend_from_slice(&buffer[..read]);
                    if buffer[..read].contains(&terminator) {
                        return Ok(());
                    }
                }
                // No byte within the quiet window ends an unframed reply; a
                // framed one never gets here because it fills `max` first.
                Err(error) if error.kind() == ErrorKind::TimedOut => {
                    return if sink.len() == before {
                        Err(TransportError::Timeout {
                            got: 0,
                            expected: 1,
                        })
                    } else {
                        Ok(())
                    };
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                    return Err(TransportError::Closed);
                }
                Err(error) => return Err(TransportError::Read(error.to_string())),
            }
        }
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.set_timeout(READ_POLL)?;
        let port = self.port()?;
        match port.read(buf) {
            Ok(read) => Ok(read),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            Err(error) => Err(TransportError::Read(error.to_string())),
        }
    }

    fn flush_input(&mut self) {
        if let Some(port) = self.port.as_ref() {
            let _ = port.discard_input_buffer();
        }
    }

    fn close(&mut self) {
        if let Some(port) = self.port.as_ref() {
            let _ = port.discard_buffers();
        }
        self.port = None;
    }
}

/// Lists the ports the connect dialog can offer.
///
/// The one platform rule is macOS's duplicate `/dev/tty.*` entries: the callout
/// `/dev/cu.*` device is the one to open (P5 finding 2).
///
/// # Errors
///
/// Returns [`TransportError::Open`] when the device list cannot be enumerated.
pub fn ports() -> Result<Vec<PortInfo>, TransportError> {
    let available = SerialPort::available_ports()
        .map_err(|error| TransportError::Open(format!("could not enumerate ports: {error}")))?;
    let mut ports: Vec<PortInfo> = available
        .into_iter()
        .map(|path| PortInfo {
            name: path.display().to_string(),
        })
        .collect();

    #[cfg(target_os = "macos")]
    {
        let callouts: std::collections::HashSet<String> = ports
            .iter()
            .filter(|port| port.name.contains("/cu."))
            .map(|port| port.name.clone())
            .collect();
        ports.retain(|port| {
            !port.name.contains("/tty.")
                || !callouts.contains(&port.name.replacen("/tty.", "/cu.", 1))
        });
    }

    ports.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_is_named_by_its_path() {
        let port = PortInfo {
            name: "/dev/cu.usbmodem21201".to_owned(),
        };

        assert_eq!(port.label(), "/dev/cu.usbmodem21201");
    }

    #[test]
    fn enumerating_never_panics_without_hardware() {
        // The result depends on the machine; the invariant is that a machine
        // with no serial devices reports an empty list, not an error or panic.
        if let Ok(ports) = ports() {
            for port in ports {
                assert_ne!(port.name, "");
            }
        }
    }

    #[test]
    fn a_zero_length_read_is_end_of_file() {
        let mut empty: &[u8] = &[];
        let mut buffer = [0u8; 4];

        let error = read_once(&mut empty, &mut buffer).expect_err("a pulled cable is EOF");

        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_populated_read_is_passed_through() {
        let mut data: &[u8] = b"ok";
        let mut buffer = [0u8; 4];

        let read = read_once(&mut data, &mut buffer).expect("a non-empty read passes through");

        assert_eq!(read, 2);
        assert_eq!(&buffer[..2], b"ok");
    }

    #[test]
    fn opening_a_missing_device_is_an_error() {
        let mut transport = SerialTransport::new(SerialSettings::new("/dev/ox-tuner-not-a-port"));

        assert!(matches!(transport.open(), Err(TransportError::Open(_))));
    }
}
