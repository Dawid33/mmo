//! Script/runtime write paths with abortable segments (design D3).
//!
//! While a segment is open the first `ws` to a script word copies its old value to the
//! shadow and sets its dirty bit. Every write goes through `Mem::w64`, so the undo log
//! covers shadow, bitmap and the segment flag too; there is no second log.

use crate::layout::{H_SEGMENT_OPEN, Layout, LoadError};
use crate::mem::{Mem, MemError};

#[derive(Clone, Debug)]
pub struct Store {
    pub mem: Mem,
    pub layout: Layout,
}

impl Store {
    /// A zeroed memory of `mem_bytes` with its header written and an empty undo log.
    pub fn new(static_words: u32, mem_bytes: u32) -> Result<Store, LoadError> {
        let layout = Layout::plan(static_words, mem_bytes)?;
        let mut mem = Mem::new(mem_bytes);
        layout.write_header(&mut mem);
        mem.clear_log();
        Ok(Store { mem, layout })
    }

    pub fn r(&self, off: u32) -> Result<u64, MemError> {
        self.mem.r64(off)
    }

    /// Script-region write; shadows the old value on the first write in an open segment.
    pub fn ws(&mut self, off: u32, v: u64) -> Result<(), MemError> {
        if !off.is_multiple_of(8) {
            return Err(MemError::Unaligned { off });
        }
        if !self.layout.is_script(off) {
            return Err(MemError::OutOfRange { off });
        }
        if self.segment_is_open() {
            let rel = u64::from(off - self.layout.static_base);
            let word = rel / 8;
            let dirty_off = self.dirty_off(word);
            let bit = 1u64 << (word % 64);
            let dirty = self.mem.r64(dirty_off)?;
            if dirty & bit == 0 {
                let old = self.mem.r64(off)?;
                self.mem.w64(self.layout.shadow_base + rel as u32, old)?;
                self.mem.w64(dirty_off, dirty | bit)?;
            }
        }
        self.mem.w64(off, v)
    }

    /// Runtime-region write (header, inbox, queue, promises, parked, commands, trace).
    pub fn wr(&mut self, off: u32, v: u64) -> Result<(), MemError> {
        let l = &self.layout;
        let script_or_shadow = off >= l.static_base && off < l.inbox_base;
        if script_or_shadow {
            return Err(MemError::OutOfRange { off });
        }
        self.mem.w64(off, v)
    }

    /// Opens a segment; nested opens keep the existing one.
    pub fn segment_open(&mut self) {
        if !self.segment_is_open() {
            let _ = self.mem.w64(H_SEGMENT_OPEN * 8, 1);
        }
    }

    /// Makes the segment's writes final.
    pub fn segment_commit(&mut self) {
        if !self.segment_is_open() {
            return;
        }
        self.sweep_dirty(false);
        let _ = self.mem.w64(H_SEGMENT_OPEN * 8, 0);
    }

    /// Restores every word the segment wrote from the shadow.
    pub fn segment_abort(&mut self) {
        if !self.segment_is_open() {
            return;
        }
        self.sweep_dirty(true);
        let _ = self.mem.w64(H_SEGMENT_OPEN * 8, 0);
    }

    pub fn segment_is_open(&self) -> bool {
        self.mem.r64(H_SEGMENT_OPEN * 8).is_ok_and(|v| v != 0)
    }

    fn dirty_off(&self, word: u64) -> u32 {
        self.layout.dirty_base + (word / 64 * 8) as u32
    }

    /// Clears the dirty bitmap, writing shadowed words back first when `restore`.
    fn sweep_dirty(&mut self, restore: bool) {
        let l = self.layout;
        let script_words = u64::from(l.heap_end - l.static_base) / 8;
        for dw in 0..script_words.div_ceil(64) {
            let dirty_off = self.dirty_off(dw * 64);
            let Ok(bits) = self.mem.r64(dirty_off) else {
                continue;
            };
            if bits == 0 {
                continue;
            }
            if restore {
                for b in 0..64u64 {
                    // Stray bits past the last script word are ignored (and cleared below).
                    if bits & (1 << b) != 0 && dw * 64 + b < script_words {
                        let rel = ((dw * 64 + b) * 8) as u32;
                        if let Ok(old) = self.mem.r64(l.shadow_base + rel) {
                            let _ = self.mem.w64(l.static_base + rel, old);
                        }
                    }
                }
            }
            let _ = self.mem.w64(dirty_off, 0);
        }
    }
}
