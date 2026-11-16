//! Reference counting, the deferred-free reaper, copy-on-write and struct objects (design D4, D6).
//!
//! # Values
//! A heap-kind slot is `0` (empty), an odd immediate (a field-less variant) or a non-zero even byte
//! offset of an object ([`is_ptr`]). `incref` / `release` ignore everything but pointers.
//!
//! # Ownership (every later task relies on these rules)
//! * A slot holding a pointer owns one reference. `incref` makes a second owner; `release` gives one up.
//! * Functions that *store* a value (`list::push`, `list::insert`, `list::set`, `obj::set_field`) take
//!   ownership of it: the caller must not use its reference afterwards. When such a function returns an
//!   error the value's ownership is unspecified: the error is a fault, the segment aborts.
//! * Functions that *return* a value (`list::pop`, `list::remove`, `list::take`, `obj::take_field`) hand the
//!   reference to the caller. `list::get` / `obj::get_field` return the raw slot without touching any
//!   refcount: the caller `incref`s if it keeps the value.
//! * Mutating functions need a unique object (refcount 1; callers `make_unique` first) and return
//!   `Corrupt` otherwise: a shared object being written is a compiler bug.
//!
//! # The zombie list
//! `release` of the last reference does not free anything. It links the block into an intrusive list whose
//! head is the allocator header's zombie word. A zombie keeps its kind, class, `len`, `aux` and payload (the
//! reaper needs them to find its children); only its **refcount field is reused as the link** (the byte
//! offset of the next zombie, 0 = end). Nothing else in the header changes: the reaper's cursor is the
//! object's own state, consumed as the children are released:
//! * `ListRef`: `len` shrinks as the last element is released (children `payload[0..len]`);
//! * `Obj`: the bits of `aux` are cleared as the fields are released.
//!
//! A reap step pops the head, releases at most `REAP_CHILDREN_MAX` children (they become zombies in turn and
//! are linked ahead of it), then either frees the block or pushes it back at the head with its advanced
//! cursor. Everything lives in the flat memory and goes through the heap's write path, so a pause between
//! two steps, a segment abort and a rollback all see a consistent list. Releasing a pointer that is already
//! a zombie is not detectable (its link looks like a refcount); a freed block is.

use crate::alloc::{Heap, Kind, ObjHeader};
use crate::consts::{REAP_CHILDREN_MAX, STRUCT_FIELDS_MAX};
use crate::fault::{FaultKind, VmResult};
use crate::store::Store;

const WORD: u32 = 8;
/// Words of an object header.
pub(crate) const HEADER_WORDS: u32 = 2;

/// Non-zero and even: a heap object (design D4).
pub fn is_ptr(v: u64) -> bool {
    v != 0 && v & 1 == 0
}

/// Which words of a parked frame hold heap kinds, by function id (used for `Parked` zombies, Task 13).
pub trait FrameMaps {
    fn ref_slots(&self, fn_id: u16) -> &[u16];
}

// ---- helpers shared with `string` and `list` --------------------------------------------------------

pub(crate) fn alloc(st: &mut Store, heap: Heap, kind: Kind, payload_words: u32) -> VmResult<u32> {
    Ok(heap.alloc(st, kind, payload_words)??)
}

pub(crate) fn ptr_of(v: u64) -> VmResult<u32> {
    if !is_ptr(v) {
        return Err(FaultKind::Corrupt);
    }
    u32::try_from(v).map_err(|_| FaultKind::Corrupt)
}

/// The pointer and header of a live (not free) object.
pub(crate) fn hdr(st: &Store, heap: Heap, v: u64) -> VmResult<(u32, ObjHeader)> {
    let p = ptr_of(v)?;
    let h = heap.header(st, p)?;
    if h.kind == Kind::Free {
        return Err(FaultKind::Corrupt);
    }
    Ok((p, h))
}

/// Payload words of the block.
pub(crate) fn capacity(h: &ObjHeader) -> u32 {
    (1u32 << h.class) - HEADER_WORDS
}

fn slot_off(p: u32, h: &ObjHeader, i: u32) -> VmResult<u32> {
    if i >= capacity(h) {
        return Err(FaultKind::Corrupt);
    }
    u32::try_from(u64::from(p) + u64::from(HEADER_WORDS + i) * u64::from(WORD))
        .map_err(|_| FaultKind::Corrupt)
}

pub(crate) fn read_slot(st: &Store, p: u32, h: &ObjHeader, i: u32) -> VmResult<u64> {
    Ok(st.r(slot_off(p, h, i)?)?)
}

pub(crate) fn write_slot(
    st: &mut Store,
    heap: Heap,
    p: u32,
    h: &ObjHeader,
    i: u32,
    v: u64,
) -> VmResult<()> {
    let off = slot_off(p, h, i)?;
    Ok(heap.w(st, off, v)?)
}

pub(crate) fn write_header(st: &mut Store, heap: Heap, p: u32, h: ObjHeader) -> VmResult<()> {
    Ok(heap.set_header(st, p, h)?)
}

/// The object must be unique (refcount 1).
pub(crate) fn require_unique(h: &ObjHeader) -> VmResult<()> {
    if h.refcount == 1 {
        Ok(())
    } else {
        Err(FaultKind::Corrupt)
    }
}

// ---- reference counts --------------------------------------------------------------------------------

pub fn incref(st: &mut Store, heap: Heap, v: u64) -> VmResult<()> {
    if !is_ptr(v) {
        return Ok(());
    }
    let (p, mut h) = hdr(st, heap, v)?;
    if h.refcount == 0 {
        return Err(FaultKind::Corrupt);
    }
    h.refcount = h.refcount.checked_add(1).ok_or(FaultKind::Corrupt)?;
    write_header(st, heap, p, h)
}

/// Gives up one reference; the last one links the block into the zombie list (nothing is freed here).
pub fn release(st: &mut Store, heap: Heap, v: u64) -> VmResult<()> {
    if !is_ptr(v) {
        return Ok(());
    }
    let (p, mut h) = hdr(st, heap, v)?;
    match h.refcount {
        0 => Err(FaultKind::Corrupt),
        1 => {
            let head = u32::try_from(heap.zombie_head(st)?).map_err(|_| FaultKind::Corrupt)?;
            h.refcount = head;
            write_header(st, heap, p, h)?;
            Ok(heap.set_zombie_head(st, u64::from(p))?)
        }
        n => {
            h.refcount = n - 1;
            write_header(st, heap, p, h)
        }
    }
}

// ---- the reaper --------------------------------------------------------------------------------------

/// One bounded step of deferred freeing. `false`: there was no zombie.
pub fn reap_step(st: &mut Store, heap: Heap, maps: &dyn FrameMaps) -> VmResult<bool> {
    let head = heap.zombie_head(st)?;
    if head == 0 {
        return Ok(false);
    }
    let p = ptr_of(head)?;
    let mut h = heap.header(st, p)?;
    // The one place later tasks extend: a kind with children of its own (`Dict`, `RefPath`, `Parked`).
    match h.kind {
        Kind::Str | Kind::ListImm | Kind::Raw | Kind::ListRef | Kind::Obj => {}
        Kind::Dict | Kind::RefPath | Kind::Parked => {
            let _ = maps;
            return Err(FaultKind::Corrupt);
        }
        Kind::Free => return Err(FaultKind::Corrupt),
    }
    // Pop it: children released below are linked ahead of the next zombie, not ahead of this one.
    heap.set_zombie_head(st, u64::from(h.refcount))?;
    let mut budget = REAP_CHILDREN_MAX;
    match h.kind {
        Kind::ListRef => {
            while budget > 0 && h.len > 0 {
                let child = read_slot(st, p, &h, h.len - 1)?;
                h.len -= 1;
                release(st, heap, child)?;
                budget -= 1;
            }
        }
        Kind::Obj => {
            while budget > 0 && h.aux != 0 {
                let i = h.aux.trailing_zeros();
                if i >= h.len {
                    return Err(FaultKind::Corrupt);
                }
                let child = read_slot(st, p, &h, i)?;
                h.aux &= h.aux - 1;
                release(st, heap, child)?;
                budget -= 1;
            }
        }
        _ => {}
    }
    let done = match h.kind {
        Kind::ListRef => h.len == 0,
        Kind::Obj => h.aux == 0,
        _ => true,
    };
    if done {
        heap.free(st, p)?;
    } else {
        h.refcount = u32::try_from(heap.zombie_head(st)?).map_err(|_| FaultKind::Corrupt)?;
        write_header(st, heap, p, h)?;
        heap.set_zombie_head(st, u64::from(p))?;
    }
    Ok(true)
}

// ---- copy-on-write -----------------------------------------------------------------------------------

/// Returns an object the caller may write: `v` itself when unique (or an immediate), else a shallow copy
/// (children get a reference) with `v`'s count lowered by one. The number is the payload words copied.
/// `HeapFull` leaves `v` and everything else untouched.
pub fn make_unique(st: &mut Store, heap: Heap, v: u64) -> VmResult<(u64, u32)> {
    if !is_ptr(v) {
        return Ok((v, 0));
    }
    let (p, h) = hdr(st, heap, v)?;
    match h.refcount {
        0 => return Err(FaultKind::Corrupt),
        1 => return Ok((v, 0)),
        _ => {}
    }
    // Words in use, and whether they are references (`Dict`, `RefPath`, `Parked`: later tasks).
    let words = match h.kind {
        Kind::Str => h.len.div_ceil(WORD),
        Kind::ListImm | Kind::ListRef | Kind::Raw | Kind::Obj => h.len,
        Kind::Dict | Kind::RefPath | Kind::Parked | Kind::Free => return Err(FaultKind::Corrupt),
    };
    if words > capacity(&h) {
        return Err(FaultKind::Corrupt);
    }
    let q = alloc(st, heap, h.kind, capacity(&h))?;
    let mut nh = heap.header(st, q)?;
    nh.tag = h.tag;
    nh.len = h.len;
    nh.aux = h.aux;
    write_header(st, heap, q, nh)?;
    for i in 0..words {
        let w = read_slot(st, p, &h, i)?;
        write_slot(st, heap, q, &nh, i, w)?;
        let is_ref = match h.kind {
            Kind::ListRef => true,
            Kind::Obj => i < 32 && h.aux & (1 << i) != 0,
            _ => false,
        };
        if is_ref {
            incref(st, heap, w)?;
        }
    }
    let mut old = h;
    old.refcount -= 1;
    write_header(st, heap, p, old)?;
    Ok((u64::from(q), words))
}

// ---- struct / variant objects ------------------------------------------------------------------------

/// Objects with data: structs, enum variants with fields, errors with data. `tag` = variant, `len` = field
/// count, `aux` = bitmap of the fields that hold heap kinds.
pub mod obj {
    use super::*;

    fn object(st: &Store, heap: Heap, o: u64) -> VmResult<(u32, ObjHeader)> {
        let (p, h) = hdr(st, heap, o)?;
        if h.kind != Kind::Obj {
            return Err(FaultKind::Corrupt);
        }
        Ok((p, h))
    }

    fn field(h: &ObjHeader, i: u32) -> VmResult<()> {
        if i < h.len {
            Ok(())
        } else {
            Err(FaultKind::Corrupt)
        }
    }

    /// A zeroed object (every field 0). `ref_bits` marks the heap-kind fields.
    pub fn new(st: &mut Store, heap: Heap, fields: u8, ref_bits: u32, tag: u16) -> VmResult<u64> {
        let n = usize::from(fields);
        if n > STRUCT_FIELDS_MAX || (n < 32 && ref_bits >> n != 0) {
            return Err(FaultKind::Corrupt);
        }
        let p = alloc(st, heap, Kind::Obj, u32::from(fields))?;
        let mut h = heap.header(st, p)?;
        h.tag = tag;
        h.len = u32::from(fields);
        h.aux = ref_bits;
        write_header(st, heap, p, h)?;
        Ok(u64::from(p))
    }

    /// The raw slot; no refcount changes.
    pub fn get_field(st: &Store, heap: Heap, o: u64, i: u32) -> VmResult<u64> {
        let (p, h) = object(st, heap, o)?;
        field(&h, i)?;
        read_slot(st, p, &h, i)
    }

    /// Moves the field out (the slot becomes 0); needs a unique object.
    pub fn take_field(st: &mut Store, heap: Heap, o: u64, i: u32) -> VmResult<u64> {
        let (p, h) = object(st, heap, o)?;
        require_unique(&h)?;
        field(&h, i)?;
        let v = read_slot(st, p, &h, i)?;
        write_slot(st, heap, p, &h, i, 0)?;
        Ok(v)
    }

    /// Stores `v` (ownership moves in) and releases the previous value of a heap-kind field.
    pub fn set_field(st: &mut Store, heap: Heap, o: u64, i: u32, v: u64) -> VmResult<()> {
        let (p, h) = object(st, heap, o)?;
        require_unique(&h)?;
        field(&h, i)?;
        let old = read_slot(st, p, &h, i)?;
        write_slot(st, heap, p, &h, i, v)?;
        if h.aux & (1 << i) != 0 {
            release(st, heap, old)?;
        }
        Ok(())
    }

    /// The variant tag of an object, or of an odd immediate (`(tag << 1) | 1`).
    pub fn tag(st: &Store, heap: Heap, v: u64) -> VmResult<u16> {
        if v & 1 == 1 {
            return u16::try_from(v >> 1).map_err(|_| FaultKind::Corrupt);
        }
        Ok(object(st, heap, v)?.1.tag)
    }
}
