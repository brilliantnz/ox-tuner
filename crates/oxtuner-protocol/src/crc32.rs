//! CRC-32/ISO-HDLC, the checksum Speeduino frames carry.
//!
//! Table-driven and dependency-free: the table is built at compile time, so a
//! frame costs one lookup and one shift per byte. The polynomial and initial
//! value are the standard ones (`0xEDB88320`, `0xFFFFFFFF`, final xor
//! `0xFFFFFFFF`) — the same result `FastCRC32` produces on the firmware side.

/// The lookup table, computed by the compiler.
const TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0usize;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ 0xEDB8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

/// The CRC-32 of `bytes`, as the framed protocol sends it.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        let index = ((crc ^ u32::from(*byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[index];
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_check_vector_matches() {
        // The CRC-32/ISO-HDLC check value every implementation publishes.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn an_empty_input_is_zero() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn the_result_is_byte_stable() {
        assert_eq!(crc32(&[0x00, 0xFF, 0x10]), 0x71D2_3404);
    }
}
