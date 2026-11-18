//! The reply queue both simulator transports read from.
//!
//! The simulator's clock is virtual: a reply that was dropped is reported at
//! once rather than after the real timeout, so a CI suite does not spend
//! seconds proving a timeout. Both transports share these loops so the two
//! cannot answer a read differently.

use std::collections::VecDeque;

use oxtuner_transport::TransportError;

/// Appends the bytes a request produced.
pub(crate) fn fill(output: &mut VecDeque<u8>, bytes: impl IntoIterator<Item = u8>) {
    output.extend(bytes);
}

/// Reads exactly `buf.len()` bytes, or reports how many arrived.
pub(crate) fn read_exact(output: &mut VecDeque<u8>, buf: &mut [u8]) -> Result<(), TransportError> {
    let mut filled = 0;
    while filled < buf.len() {
        let Some(byte) = output.pop_front() else {
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

/// Reads until `terminator` or `max` bytes, appending to `sink`.
pub(crate) fn read_until(
    output: &mut VecDeque<u8>,
    sink: &mut Vec<u8>,
    max: usize,
    terminator: u8,
) -> Result<(), TransportError> {
    let before = sink.len();
    while sink.len() - before < max {
        let Some(byte) = output.pop_front() else {
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

/// Reads what is queued, up to `buf.len()`.
pub(crate) fn read_available(output: &mut VecDeque<u8>, buf: &mut [u8]) -> usize {
    let mut read = 0;
    while read < buf.len() {
        let Some(byte) = output.pop_front() else {
            break;
        };
        buf[read] = byte;
        read += 1;
    }

    read
}
