//! Flat word-addressed memory with an undo log (design D2, D3).
//!
//! Offsets are `u32` byte offsets of aligned 64-bit little-endian words. Every write
//! that changes a word logs `(offset, old value)`; a [`Checkpoint`] is an absolute
//! position in that log, so it survives `forget_before` trimming the front.
//! The log is the sim-rollback log: it is not hashed and not saved.

use std::collections::VecDeque;
use std::hash::Hasher;
use state_hash::{StateHash, hash_len};

const WORD: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemError {
    OutOfRange { off: u32 },
    Unaligned { off: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollbackError {
    /// The checkpoint's history was dropped (or it is not a position in this log).
    Forgotten,
}

/// An absolute log sequence number: how many logged writes preceded it, ever.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Checkpoint(u64);

#[derive(Clone, Debug)]
pub struct Mem {
    bytes: Box<[u8]>,
    log: VecDeque<(u32, u64)>,
    /// Absolute sequence number of `log[0]`.
    base: u64,
}

impl Mem {
    /// Zero-filled; `bytes` is rounded down to a multiple of 8.
    pub fn new(bytes: u32) -> Mem {
        Mem::from_bytes(vec![0; (bytes - bytes % WORD) as usize])
    }

    /// Adopts `bytes` as the memory, with an empty log.
    pub fn from_bytes(bytes: Vec<u8>) -> Mem {
        Mem { bytes: bytes.into_boxed_slice(), log: VecDeque::new(), base: 0 }
    }

    pub fn len(&self) -> u32 {
        self.bytes.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn index(&self, off: u32) -> Result<usize, MemError> {
        if !off.is_multiple_of(WORD) {
            return Err(MemError::Unaligned { off });
        }
        let i = off as usize;
        if i + WORD as usize > self.bytes.len() {
            return Err(MemError::OutOfRange { off });
        }
        Ok(i)
    }

    fn read_at(&self, i: usize) -> u64 {
        u64::from_le_bytes(self.bytes[i..i + 8].try_into().expect("8-byte slice"))
    }

    pub fn r64(&self, off: u32) -> Result<u64, MemError> {
        Ok(self.read_at(self.index(off)?))
    }

    /// Writes a word, logging `(off, old)` unless the value is unchanged.
    pub fn w64(&mut self, off: u32, v: u64) -> Result<(), MemError> {
        let i = self.index(off)?;
        let old = self.read_at(i);
        if old != v {
            self.log.push_back((off, old));
            self.bytes[i..i + 8].copy_from_slice(&v.to_le_bytes());
        }
        Ok(())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.base + self.log.len() as u64)
    }

    /// Undoes every write after `cp`, newest first. Nothing changes on error.
    pub fn rollback_to(&mut self, cp: Checkpoint) -> Result<(), RollbackError> {
        if cp.0 < self.base || cp.0 > self.checkpoint().0 {
            return Err(RollbackError::Forgotten);
        }
        let keep = (cp.0 - self.base) as usize;
        while self.log.len() > keep {
            let (off, old) = self.log.pop_back().expect("log longer than keep");
            let i = off as usize;
            self.bytes[i..i + 8].copy_from_slice(&old.to_le_bytes());
        }
        Ok(())
    }

    /// Drops history older than `cp`; rolling back to `cp` or later still works.
    pub fn forget_before(&mut self, cp: Checkpoint) {
        let drop = cp.0.saturating_sub(self.base).min(self.log.len() as u64) as usize;
        self.log.drain(..drop);
        self.base += drop as u64;
    }

    pub fn log_len(&self) -> usize {
        self.log.len()
    }

    /// Drops all history; every earlier checkpoint is forgotten.
    pub fn clear_log(&mut self) {
        self.base += self.log.len() as u64;
        self.log.clear();
    }
}

impl StateHash for Mem {
    /// The length as a fixed-width prefix, then the raw bytes. Never the log.
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        hash_len(self.bytes.len(), h);
        h.write(&self.bytes);
    }
}
