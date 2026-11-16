//! The real serial port (`serialport`).
//!
//! Blocking I/O with per-call timeouts, used by the I/O thread only (ADR-0008).
//! The port is configured raw 8N1 with no flow control, which is what every
//! Speeduino, rusEFI and MegaSquirt-family ECU expects, at the baud rate the
//! connection chose.
//!
//! The same crate is the only enumerator. Each port arrives already joined to its
//! USB device, so nothing here matches a device to a path. Opening, per-call
//! timeouts and the pulled-cable signal were measured on macOS against a real ECU
//! at 115200 8N1. The Windows and Linux error mappings come from the crate's own
//! source; only the macOS behaviour has been observed on hardware.
//!
//! `libudev` is enabled for Linux only, through the crate's own target-scoped
//! dependency; a musl build falls back to sysfs and still reports the same
//! identity.

use std::io::{ErrorKind, Read};
use std::time::{Duration, Instant};

use crate::Transport;
use crate::error::TransportError;

/// How long a single `read` waits before returning "nothing yet".
///
/// Short enough that [`Transport::read_until`] can notice a quiet line, long
/// enough to avoid spinning.
const READ_POLL: Duration = Duration::from_millis(20);

/// Reads once, turning end of file and a broken link into `UnexpectedEof`.
///
/// `std::io::Read` defines a zero-length read on a non-empty buffer as EOF, and a
/// backend reports a pulled cable in its own way. All of those are folded into
/// one signal here, so the conversation ends instead of spinning until the
/// deadline and calling it a timeout.
fn read_once(mut read: impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }

    match read.read(buf) {
        Ok(0) => Err(std::io::Error::from(ErrorKind::UnexpectedEof)),
        Err(error) if is_link_gone(&error) => Err(std::io::Error::from(ErrorKind::UnexpectedEof)),
        other => other,
    }
}

/// Whether an error means the device left rather than the reply being late.
///
/// POSIX reports a hangup as `BrokenPipe`. Windows aborts the pending read with
/// `ERROR_OPERATION_ABORTED`, which the standard library presents as `TimedOut`,
/// so the raw code is the only way to tell it apart from a genuine timeout. This
/// process never cancels its own reads, so that code on this handle is the device
/// going away.
fn is_link_gone(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::NotConnected
    ) {
        return true;
    }

    #[cfg(target_os = "windows")]
    {
        // ERROR_OPERATION_ABORTED, ERROR_DEVICE_NOT_CONNECTED, ERROR_GEN_FAILURE
        // and ERROR_BAD_COMMAND, which FTDI-class drivers return on unplug. The
        // standard library maps the first to `TimedOut` and the rest to
        // uncategorised, so the raw code is the only reliable signal left.
        //
        // ERROR_GEN_FAILURE is broad enough to cover a recoverable failure too.
        // That trade is taken deliberately: reading a transient fault as a
        // closed link costs a reconnect, while the other way round costs a
        // conversation that waits for a deadline that will never be answered.
        const WINDOWS_GONE: [i32; 4] = [995, 1_167, 31, 22];

        if error
            .raw_os_error()
            .is_some_and(|code| WINDOWS_GONE.contains(&code))
        {
            return true;
        }
    }

    // Linux reports a hangup as `BrokenPipe`, which is folded above. A `POLLERR`
    // there becomes an uncategorised error with no raw code, so it is reported as
    // a read failure rather than a closed link; the conversation still ends and
    // the reconnect follows, only the wording differs.
    false
}

/// One port the connect dialog can offer.
///
/// The path is what gets opened and what the dialog keys its selection on. The
/// identity beside it comes from the platform, already joined to the port by
/// `serialport`, so nothing here matches a USB device to a path by guessing.
///
/// On macOS the callout `/dev/cu.*` entry is the one to open, and picking the
/// blocking `/dev/tty.*` twin makes the I/O thread look hung, so [`ports`]
/// filters the duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    /// Device path (`/dev/cu.usbmodem21201`, `COM3`).
    name: String,
    /// The device behind the port, when the platform reports one.
    usb: Option<UsbIdentity>,
}

impl PortInfo {
    /// Builds a port from parts already in hand.
    ///
    /// For callers that test against a port they describe rather than one the
    /// platform reported.
    #[doc(hidden)]
    pub fn from_parts(name: String, usb: Option<UsbIdentity>) -> Self {
        Self { name, usb }
    }

    /// The label to show for this port.
    ///
    /// The product string when the device has one, otherwise the path. Board
    /// names are the caller's business, because the product string is missing on
    /// many boards and cannot be trusted when present.
    pub fn label(&self) -> &str {
        self.usb
            .as_ref()
            .and_then(UsbIdentity::product)
            .unwrap_or(&self.name)
    }

    /// The device path, which is the port's stable identity within a session.
    pub fn path(&self) -> &str {
        &self.name
    }

    /// The device behind the port, when the platform reports one.
    pub fn usb(&self) -> Option<&UsbIdentity> {
        self.usb.as_ref()
    }
}

/// What the platform reports about the device behind a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbIdentity {
    vid: u16,
    pid: u16,
    serial: Option<String>,
    manufacturer: Option<String>,
    product: Option<String>,
    location: Option<UsbLocation>,
    interface: Option<u8>,
}

impl UsbIdentity {
    /// An identity for a vendor and product the caller already knows.
    ///
    /// Every optional field starts absent. Callers that test against a device
    /// they describe add only the fields they care about, so the call site reads
    /// as what it means.
    #[doc(hidden)]
    pub fn described(vid: u16, pid: u16) -> Self {
        Self {
            vid,
            pid,
            serial: None,
            manufacturer: None,
            product: None,
            location: None,
            interface: None,
        }
    }

    /// The same identity with a serial number.
    #[must_use]
    pub fn with_serial(mut self, serial: impl Into<String>) -> Self {
        self.serial = Some(serial.into());
        self
    }

    /// The same identity with a product string.
    #[must_use]
    pub fn with_product(mut self, product: impl Into<String>) -> Self {
        self.product = Some(product.into());
        self
    }

    /// Vendor id.
    pub fn vid(&self) -> u16 {
        self.vid
    }

    /// Product id.
    pub fn pid(&self) -> u16 {
        self.pid
    }

    /// Serial number, when the device provides one.
    ///
    /// Many clones do not, and the value is only unique when the vendor made it
    /// so, which is why it is never the only part of an identity.
    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    /// Manufacturer string, when the platform reports one.
    pub fn manufacturer(&self) -> Option<&str> {
        self.manufacturer.as_deref()
    }

    /// Product string, when the device provides one.
    ///
    /// Often absent, and never part of an identity.
    pub fn product(&self) -> Option<&str> {
        self.product.as_deref()
    }

    /// Where the device sits in the USB tree, when the platform reports it.
    pub fn location(&self) -> Option<&UsbLocation> {
        self.location.as_ref()
    }

    /// The interface this port belongs to, when the platform reports it.
    ///
    /// The number means different interfaces on different platforms, so it only
    /// compares within one platform.
    pub fn interface(&self) -> Option<u8> {
        self.interface
    }
}

/// A device's position in the USB tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbLocation {
    bus_id: String,
    port_chain: Vec<u8>,
}

impl UsbLocation {
    /// The bus the device is attached to.
    pub fn bus_id(&self) -> &str {
        &self.bus_id
    }

    /// The ports traversed on that bus, from the root hub outward.
    pub fn port_chain(&self) -> &[u8] {
        &self.port_chain
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
    port: Option<Box<dyn serialport::SerialPort>>,
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

    fn port_mut(&mut self) -> Result<&mut Box<dyn serialport::SerialPort>, TransportError> {
        self.port.as_mut().ok_or(TransportError::NotOpen)
    }

    fn set_timeout(&mut self, timeout: Duration) -> Result<(), TransportError> {
        self.port_mut()?
            .set_timeout(timeout)
            .map_err(|error| TransportError::Read(error.to_string()))
    }
}

impl Transport for SerialTransport {
    fn open(&mut self) -> Result<(), TransportError> {
        if self.port.is_some() {
            return Ok(());
        }
        // Raw 8N1 with no parity and no flow control, which is what every
        // Speeduino, rusEFI and MegaSquirt-family ECU expects.
        let port = serialport::new(&self.settings.port, self.settings.baud)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(READ_POLL)
            .open()
            .map_err(|error| {
                // Busy and permission problems are the two normal failures, and
                // their text is what tells the user which one happened.
                TransportError::Open(format!("{}: {error}", self.settings.port))
            })?;
        tracing::info!(
            port = %self.settings.port,
            baud = self.settings.baud,
            "serial port opened"
        );
        self.port = Some(port);
        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        let port = self.port_mut()?;

        match port.write_all(bytes).and_then(|()| port.flush()) {
            Ok(()) => Ok(()),
            Err(error) if is_link_gone(&error) => Err(TransportError::Closed),
            Err(error) => Err(TransportError::Write(error.to_string())),
        }
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
            let port = self.port_mut()?;
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
            let port = self.port_mut()?;
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
        let port = self.port_mut()?;

        // The same fold as every other read: a pulled cable must not be reported
        // as a port with nothing to say.
        match read_once(port, buf) {
            Ok(read) => Ok(read),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => Err(TransportError::Closed),
            Err(error) => Err(TransportError::Read(error.to_string())),
        }
    }

    fn flush_input(&mut self) {
        if let Some(port) = self.port.as_mut() {
            let _ = port.clear(serialport::ClearBuffer::Input);
        }
    }

    fn close(&mut self) {
        if let Some(port) = self.port.as_mut() {
            let _ = port.clear(serialport::ClearBuffer::All);
        }
        // Dropping the handle closes the descriptor; the crate has no explicit
        // close, and a reconnected transport opens a fresh one.
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
    let available = serialport::available_ports()
        .map_err(|error| TransportError::Open(format!("could not enumerate ports: {error}")))?;

    let mut ports: Vec<PortInfo> = available
        .into_iter()
        .map(|port| PortInfo {
            name: port.port_name,
            usb: match port.port_type {
                serialport::SerialPortType::UsbPort(info) => Some(usb_identity(&info)),
                _ => None,
            },
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

/// Copies the platform's device information into the type this crate exposes.
fn usb_identity(info: &serialport::UsbPortInfo) -> UsbIdentity {
    UsbIdentity {
        vid: info.vid,
        pid: info.pid,
        serial: info.serial_number.clone(),
        manufacturer: info.manufacturer.clone(),
        product: info.product.clone(),
        location: info.location.as_ref().map(|location| UsbLocation {
            bus_id: location.bus_id().to_owned(),
            port_chain: location.port_chain().to_vec(),
        }),
        interface: info.interface,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_is_named_by_its_path() {
        let port = PortInfo {
            name: "/dev/cu.usbmodem21201".to_owned(),
            usb: None,
        };

        assert_eq!(port.label(), "/dev/cu.usbmodem21201");
        assert_eq!(port.path(), "/dev/cu.usbmodem21201");
    }

    #[test]
    fn a_port_with_a_product_is_labelled_by_it() {
        let port = PortInfo {
            name: "/dev/cu.usbserial-A50285BI".to_owned(),
            usb: Some(UsbIdentity {
                vid: 0x0403,
                pid: 0x6001,
                serial: Some("A50285BI".to_owned()),
                manufacturer: None,
                product: Some("FT232R USB UART".to_owned()),
                location: None,
                interface: Some(0),
            }),
        };

        assert_eq!(port.label(), "FT232R USB UART");
        assert_eq!(port.path(), "/dev/cu.usbserial-A50285BI");
        assert_eq!(port.usb().map(UsbIdentity::vid), Some(0x0403));
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
