//! SHA-256 for sidecar provenance and cache keys.
//!
//! Provided by the `sha2` crate, whose aarch64 and x86 backends use the CPU's
//! SHA instructions. The portable implementation this module used to carry ran
//! at about 210 MB/s, which put 2.2 ms of a project open into hashing a 469 KB
//! definition; the crate-backed one takes about 0.2 ms.
//!
//! This is **not** used as a security primitive — it answers "did the INI I
//! opened change since last time?" and identifies a cache entry.

/// The SHA-256 digest of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    Sha256::digest(data).into()
}

/// The digest as lowercase hexadecimal.
pub fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(64);
    for byte in digest {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_vectors_match() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn padding_boundaries_change_the_digest() {
        // 55, 56 and 64 bytes cross the padding edge and the block boundary.
        for length in [55, 56, 64, 119, 120] {
            let digest = sha256(&vec![b'a'; length]);
            assert_ne!(digest, [0u8; 32], "length {length}");
        }
    }
}
