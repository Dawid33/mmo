//! Memory layout and header (design D2).
//!
//! Regions in order: header | static | heap | shadow | dirty | inbox | queue | promises |
//! parked | commands | error trace. All offsets are `u32` bytes; arithmetic is done in `u64`
//! and narrowed only after a bound check, so nothing can overflow a 32-bit `usize`.

use crate::consts::{
    CMD_BYTES, ERR_TRACE_MAX, HEADER_WORDS, HEAP_MIN_BYTES, INBOX_BYTES, PARKED_MAX, PROMISE_MAX,
    QUEUE_LEN,
};
use crate::mem::Mem;

const WORD: u64 = 8;

/// Words of one queue record (the scheduler owns its contents).
pub const QUEUE_REC_WORDS: u32 = 4;
/// Words of one promise record.
pub const PROMISE_REC_WORDS: u32 = 6;
/// Words of one parked-handler record.
pub const PARKED_REC_WORDS: u32 = 4;
/// Words of one error-trace entry.
pub const TRACE_REC_WORDS: u32 = 1;

/// `u64::from_le_bytes(*b"QSVMEM01")`.
pub const MAGIC: u64 = u64::from_le_bytes(*b"QSVMEM01");

// Header word indices. Word 0 is the magic; the static region always starts right after the
// header, so its base is not stored.
pub const H_MAGIC: u32 = 0;
pub const H_BYTECODE_VERSION: u32 = 1;
pub const H_HOST_API_VERSION: u32 = 2;
pub const H_PROGRAM_HASH: u32 = 3;
pub const H_MEM_BYTES: u32 = 4;
pub const H_EPOCH: u32 = 5;
pub const H_STATIC_WORDS: u32 = 6;
pub const H_HEAP_BASE: u32 = 7;
pub const H_HEAP_END: u32 = 8;
pub const H_SHADOW_BASE: u32 = 9;
pub const H_DIRTY_BASE: u32 = 10;
pub const H_INBOX_BASE: u32 = 11;
pub const H_INBOX_END: u32 = 12;
pub const H_QUEUE_BASE: u32 = 13;
pub const H_PROMISES_BASE: u32 = 14;
pub const H_PARKED_BASE: u32 = 15;
pub const H_CMDS_BASE: u32 = 16;
pub const H_CMDS_END: u32 = 17;
pub const H_TRACE_BASE: u32 = 18;
// Scheduler fields (reserved here, written by later tasks).
pub const H_STATUS: u32 = 19;
pub const H_TASK: u32 = 20;
pub const H_PC: u32 = 21;
pub const H_CURRENT_FN: u32 = 22;
pub const H_FUEL_DEBT: u32 = 23;
pub const H_PROMISE_SERIAL: u32 = 24;
pub const H_ARRIVAL: u32 = 25;
pub const H_QUEUE_HEAD: u32 = 26;
pub const H_QUEUE_COUNT: u32 = 27;
pub const H_CMD_LEN: u32 = 28;
pub const H_SEGMENT_OPEN: u32 = 29;
pub const H_TRACE_LEN: u32 = 30;
pub const H_INIT_CURSOR: u32 = 31;

const _: () = assert!(H_INIT_CURSOR < HEADER_WORDS);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    MemTooSmall { need: u32 },
    BadMagic,
    VersionMismatch { found: u32 },
    HostApiMismatch { found: u32 },
    ProgramMismatch,
    Invalid(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub mem_bytes: u32,
    pub static_base: u32,
    pub static_words: u32,
    pub heap_base: u32,
    pub heap_end: u32,
    pub shadow_base: u32,
    pub dirty_base: u32,
    pub inbox_base: u32,
    pub inbox_end: u32,
    pub queue_base: u32,
    pub promises_base: u32,
    pub parked_base: u32,
    pub cmds_base: u32,
    pub cmds_end: u32,
    pub trace_base: u32,
}

fn words_of(bytes: u32) -> u64 {
    u64::from(bytes) / WORD
}

impl Layout {
    /// Sizes the regions for `static_words` static slots in `mem_bytes` of memory (D2).
    pub fn plan(static_words: u32, mem_bytes: u32) -> Result<Layout, LoadError> {
        let queue = u64::from(QUEUE_LEN as u32) * u64::from(QUEUE_REC_WORDS);
        let promises = u64::from(PROMISE_MAX as u32) * u64::from(PROMISE_REC_WORDS);
        let parked = u64::from(PARKED_MAX as u32) * u64::from(PARKED_REC_WORDS);
        let trace = u64::from(ERR_TRACE_MAX as u32) * u64::from(TRACE_REC_WORDS);
        let inbox = words_of(INBOX_BYTES);
        let cmds = words_of(CMD_BYTES);
        let fixed = u64::from(HEADER_WORDS) + inbox + queue + promises + parked + cmds + trace;

        // Smallest R with floor(R * 64 / 129) >= static_words + heap minimum.
        let t = u64::from(static_words) + words_of(HEAP_MIN_BYTES);
        let need_words = fixed + (t * 129).div_ceil(64);
        let need = u32::try_from(need_words * WORD).unwrap_or(u32::MAX);

        let total = words_of(mem_bytes);
        if total < need_words {
            return Err(LoadError::MemTooSmall { need });
        }
        let r = total - fixed;
        let s = r * 64 / 129; // script words
        let dirty = s.div_ceil(64);

        let mut at = u64::from(HEADER_WORDS);
        let mut next = |len: u64| {
            let base = at * WORD;
            at += len;
            base as u32
        };
        let static_base = next(u64::from(static_words));
        let heap_base = next(s - u64::from(static_words));
        let heap_end = heap_base + ((s - u64::from(static_words)) * WORD) as u32;
        let shadow_base = next(s);
        let dirty_base = next(dirty);
        let inbox_base = next(inbox);
        let queue_base = next(queue);
        let promises_base = next(promises);
        let parked_base = next(parked);
        let cmds_base = next(cmds);
        let trace_base = next(trace);
        Ok(Layout {
            mem_bytes,
            static_base,
            static_words,
            heap_base,
            heap_end,
            shadow_base,
            dirty_base,
            inbox_base,
            inbox_end: queue_base,
            queue_base,
            promises_base,
            parked_base,
            cmds_base,
            cmds_end: trace_base,
            trace_base,
        })
    }

    /// Writes the magic, `mem_bytes`, `static_words` and every region base (logged writes).
    /// A memory shorter than the header is left untouched.
    pub fn write_header(&self, mem: &mut Mem) {
        let fields: [(u32, u64); 18] = [
            (H_MAGIC, MAGIC),
            (H_MEM_BYTES, u64::from(self.mem_bytes)),
            (H_STATIC_WORDS, u64::from(self.static_words)),
            (H_HEAP_BASE, u64::from(self.heap_base)),
            (H_HEAP_END, u64::from(self.heap_end)),
            (H_SHADOW_BASE, u64::from(self.shadow_base)),
            (H_DIRTY_BASE, u64::from(self.dirty_base)),
            (H_INBOX_BASE, u64::from(self.inbox_base)),
            (H_INBOX_END, u64::from(self.inbox_end)),
            (H_QUEUE_BASE, u64::from(self.queue_base)),
            (H_PROMISES_BASE, u64::from(self.promises_base)),
            (H_PARKED_BASE, u64::from(self.parked_base)),
            (H_CMDS_BASE, u64::from(self.cmds_base)),
            (H_CMDS_END, u64::from(self.cmds_end)),
            (H_TRACE_BASE, u64::from(self.trace_base)),
            (H_BYTECODE_VERSION, 0),
            (H_HOST_API_VERSION, 0),
            (H_PROGRAM_HASH, 0),
        ];
        for (w, v) in fields {
            // Out of range only when the memory is shorter than the header: nothing to write.
            let _ = mem.w64(w * 8, v);
        }
    }

    /// Reads the layout back and checks it is exactly what `plan` would produce for this
    /// memory, so a corrupt or foreign header cannot yield out-of-range regions.
    pub fn read_header(mem: &Mem) -> Result<Layout, LoadError> {
        let word = |w: u32| mem.r64(w * 8);
        let short = || LoadError::Invalid("memory shorter than the header".into());
        if word(H_MAGIC).map_err(|_| short())? != MAGIC {
            return Err(LoadError::BadMagic);
        }
        let narrow = |w: u32| -> Result<u32, LoadError> {
            let v = word(w).map_err(|_| short())?;
            u32::try_from(v).map_err(|_| LoadError::Invalid(format!("header word {w} out of range")))
        };
        let mem_bytes = narrow(H_MEM_BYTES)?;
        if mem_bytes != mem.len() {
            return Err(LoadError::Invalid("header mem_bytes differs from the memory".into()));
        }
        let planned = Layout::plan(narrow(H_STATIC_WORDS)?, mem_bytes)?;
        let stored = [
            (H_HEAP_BASE, planned.heap_base),
            (H_HEAP_END, planned.heap_end),
            (H_SHADOW_BASE, planned.shadow_base),
            (H_DIRTY_BASE, planned.dirty_base),
            (H_INBOX_BASE, planned.inbox_base),
            (H_INBOX_END, planned.inbox_end),
            (H_QUEUE_BASE, planned.queue_base),
            (H_PROMISES_BASE, planned.promises_base),
            (H_PARKED_BASE, planned.parked_base),
            (H_CMDS_BASE, planned.cmds_base),
            (H_CMDS_END, planned.cmds_end),
            (H_TRACE_BASE, planned.trace_base),
        ];
        for (w, expect) in stored {
            if narrow(w)? != expect {
                return Err(LoadError::Invalid(format!("header word {w} disagrees with the layout")));
            }
        }
        Ok(planned)
    }

    /// Offset of static slot `slot` (frames, then globals).
    pub fn slot_off(&self, slot: u16) -> u32 {
        self.static_base.saturating_add(u32::from(slot) * 8)
    }

    /// Whether `off` is in a script region (static or heap).
    pub fn is_script(&self, off: u32) -> bool {
        off >= self.static_base && off < self.heap_end
    }
}
