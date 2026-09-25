//! SHA-256 content identity, rendered as lowercase hex.
//!
//! The single digest implementation behind every hash sylloge records: TLS
//! peer certificates, wire and decoded body bytes, extracted text, and the
//! evidence fingerprint. It uses `ring`, which is already the TLS crypto
//! provider, so no second hash stack enters the graph.

use ring::digest::{Context, SHA256};

/// Incremental SHA-256 over bytes that arrive in pieces.
pub(crate) struct Sha256(Context);

impl Sha256 {
    pub(crate) fn new() -> Self {
        Self(Context::new(&SHA256))
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Finish and return the digest as lowercase hex.
    pub(crate) fn finish_hex(self) -> String {
        hex(self.0.finish().as_ref())
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    hash.finish_hex()
}

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_fips_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "FIPS 180-2 test vector for SHA-256(\"abc\")"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "SHA-256 of the empty string"
        );
    }

    #[test]
    fn incremental_equals_one_shot() {
        let mut hash = Sha256::new();
        hash.update(b"ab");
        hash.update(b"");
        hash.update(b"c");
        assert_eq!(
            hash.finish_hex(),
            sha256_hex(b"abc"),
            "splitting the input must not change the digest"
        );
    }
}
