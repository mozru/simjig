//! Stable content hashing for cache keys and model prefixes.
//!
//! Invariant: a key depends only on the bytes fed in, never on the process, the platform's
//! `Hash` impls or the Rust release. That is why this is SHA-256 rather than
//! `std::hash::DefaultHasher`, whose algorithm is explicitly unspecified. Every field is
//! length-prefixed, so `("ab", "c")` and `("a", "bc")` hash differently.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::Error;

/// Accumulates length-prefixed fields into a SHA-256 digest.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeyHasher(Sha256);

impl KeyHasher {
    /// Starts a hash in a named domain, so keys of different kinds never collide.
    pub(crate) fn new(domain: &str) -> Self {
        let mut h = Self::default();
        h.field(domain.as_bytes());
        h
    }

    pub(crate) fn field(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update((bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
        self
    }

    pub(crate) fn str(&mut self, s: &str) -> &mut Self {
        self.field(s.as_bytes())
    }

    pub(crate) fn path(&mut self, p: &Path) -> &mut Self {
        self.field(p.as_os_str().as_encoded_bytes())
    }

    /// Hashes a file's path and its contents.
    pub(crate) fn file(&mut self, p: &Path) -> Result<&mut Self, Error> {
        let contents = std::fs::read(p).map_err(Error::io("reading", p))?;
        self.path(p);
        Ok(self.field(&contents))
    }

    /// The full digest as 64 lowercase hex digits.
    pub(crate) fn finish(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = self.0.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            hex.push(char::from(HEX[usize::from(byte >> 4)]));
            hex.push(char::from(HEX[usize::from(byte & 0xf)]));
        }
        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest_is_stable() {
        // Pinned value: if this changes, every user's cache is invalidated and every model
        // prefix changes. Only change it together with a CHANGELOG entry. Reference value from
        // Python: sha256 over (u64 LE length, bytes) for "test" then "hello".
        let mut h = KeyHasher::new("test");
        h.str("hello");
        assert_eq!(
            h.finish(),
            "b2a1b72f2e76e6de7a45696f907cdf3732933dfab9843b886202630d4405a0b8"
        );
    }

    #[test]
    fn fields_are_length_prefixed() {
        let mut a = KeyHasher::new("d");
        a.str("ab").str("c");
        let mut b = KeyHasher::new("d");
        b.str("a").str("bc");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn domains_separate_keys() {
        let mut a = KeyHasher::new("model");
        a.str("x");
        let mut b = KeyHasher::new("lib");
        b.str("x");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn file_hash_tracks_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.sv");
        std::fs::write(&p, "module a; endmodule\n").unwrap();
        let hash = || {
            let mut h = KeyHasher::new("f");
            h.file(&p).unwrap();
            h.finish()
        };
        let first = hash();
        assert_eq!(first, hash(), "same contents, same key");
        std::fs::write(&p, "module a; wire w; endmodule\n").unwrap();
        assert_ne!(first, hash(), "edited contents, new key");
    }
}
