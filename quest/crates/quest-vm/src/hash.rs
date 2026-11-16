//! FNV-1a, 64 bit: the hasher every quest-vm state hash feeds.

use std::hash::Hasher;

const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over the bytes written to it. Integers go in as the native-endian bytes
/// `Hasher`'s default `write_*` produce (little-endian on every shipped target).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fnv64(pub u64);

impl Default for Fnv64 {
    fn default() -> Self {
        Fnv64(OFFSET_BASIS)
    }
}

impl Hasher for Fnv64 {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
        self.0 = h;
    }
}
