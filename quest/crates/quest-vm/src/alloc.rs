//! Deterministic size-class allocator inside the flat memory (design D4).
//!
//! One implementation, two instances: the script heap (`script: true`, writes through
//! `Store::ws`, so a segment abort restores it) and the inbox heap (`script: false`, `wr`).
//! The allocator's own state is the first `ALLOC_HEADER_WORDS` words of its region, so abort and
//! rollback restore it together with the objects. Blocks are `2^class` words including the
//! 2-word object header; per-class LIFO free lists, else bump; no splitting, no coalescing.
//! A free block keeps its list link in header word 1, so its payload stays zero.
//! Every pointer is validated: corrupt memory yields a `MemError`, never a panic.

use crate::mem::MemError;
use crate::store::Store;

const WORD: u32 = 8;
/// Words of an object header.
const OBJ_HEADER_WORDS: u64 = 2;
/// Largest block class (`2^28` words); a bigger request is simply "full".
pub const CLASS_MAX: u8 = 28;
const CLASS_SLOTS: u32 = 32;

// Allocator header word indices (relative to the region base).
const A_TOP: u32 = 0;
const A_ZOMBIE: u32 = 1;
const A_USED: u32 = 2;
const A_FREE_HEADS: u32 = 3;

/// Bump top, zombie-list head (kept by `object`), used words, one free-list head per class.
pub const ALLOC_HEADER_WORDS: u32 = A_FREE_HEADS + CLASS_SLOTS;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Free = 0,
    Str,
    ListImm,
    ListRef,
    Dict,
    Raw,
    Obj,
    RefPath,
    Parked,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            0 => Kind::Free,
            1 => Kind::Str,
            2 => Kind::ListImm,
            3 => Kind::ListRef,
            4 => Kind::Dict,
            5 => Kind::Raw,
            6 => Kind::Obj,
            7 => Kind::RefPath,
            8 => Kind::Parked,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjHeader {
    pub refcount: u32,
    pub kind: Kind,
    pub class: u8,
    pub tag: u16,
    pub len: u32,
    pub aux: u32,
}

/// The region is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocError;

#[derive(Clone, Copy, Debug)]
pub struct Heap {
    pub base: u32,
    pub end: u32,
    /// `true`: the script heap (`ws`); `false`: the inbox heap (`wr`).
    pub script: bool,
}

fn block_bytes(class: u8) -> u64 {
    (1u64 << class) * u64::from(WORD)
}

impl Heap {
    /// Writes one word through this heap's path (`ws` for the script heap, `wr` for the inbox).
    pub fn w(&self, st: &mut Store, off: u32, v: u64) -> Result<(), MemError> {
        if self.script {
            st.ws(off, v)
        } else {
            st.wr(off, v)
        }
    }

    fn first_block(&self) -> u64 {
        u64::from(self.base) + u64::from(ALLOC_HEADER_WORDS) * u64::from(WORD)
    }

    fn ah(&self, word: u32) -> u32 {
        // `base + word * 8` is inside the region once `init` checked its size.
        self.base + word * WORD
    }

    /// Writes the allocator header: nothing allocated, no free blocks.
    pub fn init(&self, st: &mut Store) -> Result<(), MemError> {
        if !self.base.is_multiple_of(WORD) {
            return Err(MemError::Unaligned { off: self.base });
        }
        if self.first_block() > u64::from(self.end) {
            return Err(MemError::OutOfRange { off: self.base });
        }
        self.w(st, self.ah(A_TOP), self.first_block())?;
        self.w(st, self.ah(A_ZOMBIE), 0)?;
        self.w(st, self.ah(A_USED), 0)?;
        for c in 0..CLASS_SLOTS {
            self.w(st, self.ah(A_FREE_HEADS + c), 0)?;
        }
        Ok(())
    }

    /// The bump top, checked to lie inside the region.
    fn top(&self, st: &Store) -> Result<u32, MemError> {
        let t = st.r(self.ah(A_TOP))?;
        if t < self.first_block() || t > u64::from(self.end) || !t.is_multiple_of(u64::from(WORD)) {
            return Err(MemError::Corrupt {
                off: self.ah(A_TOP),
            });
        }
        Ok(t as u32)
    }

    /// Head of the zombie list (an object pointer, 0 = no zombies); the reaper owns its meaning.
    pub fn zombie_head(&self, st: &Store) -> Result<u64, MemError> {
        st.r(self.ah(A_ZOMBIE))
    }

    pub fn set_zombie_head(&self, st: &mut Store, v: u64) -> Result<(), MemError> {
        self.w(st, self.ah(A_ZOMBIE), v)
    }

    pub fn used_words(&self, st: &Store) -> Result<u32, MemError> {
        let u = st.r(self.ah(A_USED))?;
        u32::try_from(u).map_err(|_| MemError::Corrupt {
            off: self.ah(A_USED),
        })
    }

    fn set_used(&self, st: &mut Store, v: u64) -> Result<(), MemError> {
        self.w(st, self.ah(A_USED), v)
    }

    /// Allocates a block for `payload_words` words: refcount 1, zeroed payload, `len = 0`.
    pub fn alloc(
        &self,
        st: &mut Store,
        kind: Kind,
        payload_words: u32,
    ) -> Result<Result<u32, AllocError>, MemError> {
        if kind == Kind::Free {
            return Err(MemError::Corrupt { off: self.base });
        }
        let total = u64::from(payload_words) + OBJ_HEADER_WORDS;
        let class = (64 - (total - 1).leading_zeros()).max(1) as u8;
        if class > CLASS_MAX {
            return Ok(Err(AllocError));
        }
        let words = 1u64 << class;
        let used = u64::from(self.used_words(st)?);
        let head_off = self.ah(A_FREE_HEADS + u32::from(class));
        let head = st.r(head_off)?;
        let ptr = if head != 0 {
            let ptr = self.check_ptr(st, head)?;
            let h = self.read_header(st, ptr)?;
            if h.kind != Kind::Free || h.class != class {
                return Err(MemError::Corrupt { off: ptr });
            }
            let next = st.r(ptr + WORD)?;
            self.w(st, head_off, next)?;
            ptr
        } else {
            let top = self.top(st)?;
            let new_top = u64::from(top) + block_bytes(class);
            if new_top > u64::from(self.end) {
                return Ok(Err(AllocError));
            }
            self.w(st, self.ah(A_TOP), new_top)?;
            top
        };
        self.set_used(st, used + words)?;
        self.write_header(
            st,
            ptr,
            ObjHeader {
                refcount: 1,
                kind,
                class,
                tag: 0,
                len: 0,
                aux: 0,
            },
        )?;
        Ok(Ok(ptr))
    }

    /// Zeroes the payload and pushes the block on its class list.
    pub fn free(&self, st: &mut Store, ptr: u32) -> Result<(), MemError> {
        let h = self.header(st, ptr)?;
        if h.kind == Kind::Free {
            return Err(MemError::Corrupt { off: ptr });
        }
        let used = u64::from(self.used_words(st)?);
        let words = 1u64 << h.class;
        let used = used.checked_sub(words).ok_or(MemError::Corrupt {
            off: self.ah(A_USED),
        })?;
        let head_off = self.ah(A_FREE_HEADS + u32::from(h.class));
        let head = st.r(head_off)?;
        for i in OBJ_HEADER_WORDS..words {
            self.w(st, ptr + (i as u32) * WORD, 0)?;
        }
        self.w(
            st,
            ptr,
            u64::from(Kind::Free as u8) << 32 | u64::from(h.class) << 40,
        )?;
        self.w(st, ptr + WORD, head)?;
        self.w(st, head_off, u64::from(ptr))?;
        self.set_used(st, used)
    }

    /// `ptr` as a block address: aligned, past the allocator header, below the bump top.
    fn check_ptr(&self, st: &Store, ptr: u64) -> Result<u32, MemError> {
        let Ok(p) = u32::try_from(ptr) else {
            return Err(MemError::OutOfRange { off: u32::MAX });
        };
        if !p.is_multiple_of(WORD) {
            return Err(MemError::Unaligned { off: p });
        }
        let top = self.top(st)?;
        if u64::from(p) < self.first_block() || u64::from(p) + 2 * u64::from(WORD) > u64::from(top)
        {
            return Err(MemError::OutOfRange { off: p });
        }
        Ok(p)
    }

    fn read_header(&self, st: &Store, ptr: u32) -> Result<ObjHeader, MemError> {
        let w0 = st.r(ptr)?;
        let w1 = st.r(ptr + WORD)?;
        let kind = Kind::from_u8((w0 >> 32) as u8).ok_or(MemError::Corrupt { off: ptr })?;
        let class = (w0 >> 40) as u8;
        let top = u64::from(self.top(st)?);
        if class == 0 || class > CLASS_MAX || u64::from(ptr) + block_bytes(class) > top {
            return Err(MemError::Corrupt { off: ptr });
        }
        Ok(ObjHeader {
            refcount: w0 as u32,
            kind,
            class,
            tag: (w0 >> 48) as u16,
            len: w1 as u32,
            aux: (w1 >> 32) as u32,
        })
    }

    fn write_header(&self, st: &mut Store, ptr: u32, h: ObjHeader) -> Result<(), MemError> {
        let w0 = u64::from(h.refcount)
            | u64::from(h.kind as u8) << 32
            | u64::from(h.class) << 40
            | u64::from(h.tag) << 48;
        let w1 = u64::from(h.len) | u64::from(h.aux) << 32;
        self.w(st, ptr, w0)?;
        self.w(st, ptr + WORD, w1)
    }

    pub fn header(&self, st: &Store, ptr: u32) -> Result<ObjHeader, MemError> {
        let p = self.check_ptr(st, u64::from(ptr))?;
        self.read_header(st, p)
    }

    /// Rewrites the header; the block's size class cannot change.
    pub fn set_header(&self, st: &mut Store, ptr: u32, h: ObjHeader) -> Result<(), MemError> {
        let old = self.header(st, ptr)?;
        if old.class != h.class {
            return Err(MemError::Corrupt { off: ptr });
        }
        self.write_header(st, ptr, h)
    }
}
