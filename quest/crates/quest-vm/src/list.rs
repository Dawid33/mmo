//! Lists: `len` = element count, capacity = the block's payload words. `ListImm` holds immediates (ints,
//! floats, bools), `ListRef` holds heap-kind slots (each non-immediate element owns one reference).
//!
//! Ownership follows `object`'s rules: `push` / `insert` / `set` take the value, `pop` / `remove` / `take`
//! hand it to the caller, `get` is a raw read. Mutators need a unique list. Indexes arrive as `i64` from
//! scripts: negative or `>= len` is `IndexOutOfRange` (no wrap-around). Growth allocates the next size class,
//! moves the elements (no refcount change) and frees the old block, so `push` / `insert` return the list's
//! (possibly new) pointer.

use crate::alloc::{Heap, Kind, ObjHeader};
use crate::eq::deep_eq;
use crate::fault::{FaultKind, VmResult};
use crate::object::{
    alloc, capacity, hdr, incref, read_slot, release, require_unique, write_header, write_slot,
};
use crate::store::Store;

/// Payload words of an empty list (capacity grows by doubling the block).
const INITIAL_WORDS: u32 = 4;

fn list(st: &Store, heap: Heap, l: u64) -> VmResult<(u32, ObjHeader)> {
    let (p, h) = hdr(st, heap, l)?;
    if !matches!(h.kind, Kind::ListImm | Kind::ListRef) {
        return Err(FaultKind::Corrupt);
    }
    Ok((p, h))
}

fn unique(st: &Store, heap: Heap, l: u64) -> VmResult<(u32, ObjHeader)> {
    let (p, h) = list(st, heap, l)?;
    require_unique(&h)?;
    Ok((p, h))
}

fn index(i: i64, len: u32) -> VmResult<u32> {
    match u32::try_from(i) {
        Ok(i) if i < len => Ok(i),
        _ => Err(FaultKind::IndexOutOfRange),
    }
}

pub fn new(st: &mut Store, heap: Heap, elems_are_refs: bool) -> VmResult<u64> {
    let kind = if elems_are_refs {
        Kind::ListRef
    } else {
        Kind::ListImm
    };
    Ok(u64::from(alloc(st, heap, kind, INITIAL_WORDS)?))
}

pub fn len(st: &Store, heap: Heap, l: u64) -> VmResult<u32> {
    Ok(list(st, heap, l)?.1.len)
}

/// The raw slot; the caller `incref`s if it keeps it.
pub fn get(st: &Store, heap: Heap, l: u64, i: i64) -> VmResult<u64> {
    let (p, h) = list(st, heap, l)?;
    read_slot(st, p, &h, index(i, h.len)?)
}

/// Moves the element out, leaving 0.
pub fn take(st: &mut Store, heap: Heap, l: u64, i: i64) -> VmResult<u64> {
    let (p, h) = unique(st, heap, l)?;
    let i = index(i, h.len)?;
    let v = read_slot(st, p, &h, i)?;
    write_slot(st, heap, p, &h, i, 0)?;
    Ok(v)
}

/// Stores `v` (ownership moves in), releasing what was there.
pub fn set(st: &mut Store, heap: Heap, l: u64, i: i64, v: u64) -> VmResult<()> {
    let (p, h) = unique(st, heap, l)?;
    let i = index(i, h.len)?;
    let old = read_slot(st, p, &h, i)?;
    write_slot(st, heap, p, &h, i, v)?;
    if h.kind == Kind::ListRef {
        release(st, heap, old)?;
    }
    Ok(())
}

/// Makes room for one more element; returns the list's block (a new one when it grew) and the words the
/// growth moved and zeroed (elements copied plus the old block's payload).
fn reserve_one(st: &mut Store, heap: Heap, l: u64) -> VmResult<(u32, ObjHeader, u32)> {
    let (p, h) = unique(st, heap, l)?;
    if h.len < capacity(&h) {
        return Ok((p, h, 0));
    }
    let class = u32::from(h.class) + 1;
    let q = alloc(st, heap, h.kind, (1u32 << class) - 2)?;
    let mut nh = heap.header(st, q)?;
    nh.tag = h.tag;
    nh.len = h.len;
    nh.aux = h.aux;
    write_header(st, heap, q, nh)?;
    for i in 0..h.len {
        let w = read_slot(st, p, &h, i)?;
        write_slot(st, heap, q, &nh, i, w)?;
    }
    heap.free(st, p)?;
    Ok((q, nh, h.len + capacity(&h)))
}

/// Appends `v`; returns the list pointer and the words a growth moved (0 when it did not grow).
pub fn push(st: &mut Store, heap: Heap, l: u64, v: u64) -> VmResult<(u64, u32)> {
    let n = len(st, heap, l)?;
    insert(st, heap, l, i64::from(n), v)
}

/// Inserts at `i` (`0..=len`), shifting the tail right; ownership of `v` moves in. Returns the list
/// pointer and the words shifted plus those a growth moved.
pub fn insert(st: &mut Store, heap: Heap, l: u64, i: i64, v: u64) -> VmResult<(u64, u32)> {
    let n = len(st, heap, l)?;
    let at = match u32::try_from(i) {
        Ok(at) if at <= n => at,
        _ => return Err(FaultKind::IndexOutOfRange),
    };
    let (p, mut h, mut work) = reserve_one(st, heap, l)?;
    for j in (at..n).rev() {
        let w = read_slot(st, p, &h, j)?;
        write_slot(st, heap, p, &h, j + 1, w)?;
        work += 1;
    }
    write_slot(st, heap, p, &h, at, v)?;
    h.len = n + 1;
    write_header(st, heap, p, h)?;
    Ok((u64::from(p), work))
}

/// Removes the last element and hands it over; `None` when empty.
pub fn pop(st: &mut Store, heap: Heap, l: u64) -> VmResult<Option<u64>> {
    let (p, mut h) = unique(st, heap, l)?;
    if h.len == 0 {
        return Ok(None);
    }
    let v = read_slot(st, p, &h, h.len - 1)?;
    write_slot(st, heap, p, &h, h.len - 1, 0)?;
    h.len -= 1;
    write_header(st, heap, p, h)?;
    Ok(Some(v))
}

/// Removes element `i`, shifting the tail left, and hands it over with the words shifted.
pub fn remove(st: &mut Store, heap: Heap, l: u64, i: i64) -> VmResult<(u64, u32)> {
    let (p, mut h) = unique(st, heap, l)?;
    let at = index(i, h.len)?;
    let v = read_slot(st, p, &h, at)?;
    for j in at + 1..h.len {
        let w = read_slot(st, p, &h, j)?;
        write_slot(st, heap, p, &h, j - 1, w)?;
    }
    let work = h.len - 1 - at;
    write_slot(st, heap, p, &h, h.len - 1, 0)?;
    h.len -= 1;
    write_header(st, heap, p, h)?;
    Ok((v, work))
}

/// A new list of `l[a..b]`; `b` clamps to `len`, `a >= b` gives an empty list, a negative bound faults.
/// Elements of a `ListRef` get a reference. `l` need not be unique. Also returns the words copied.
pub fn slice(st: &mut Store, heap: Heap, l: u64, a: i64, b: i64) -> VmResult<(u64, u32)> {
    let (p, h) = list(st, heap, l)?;
    if a < 0 || b < 0 {
        return Err(FaultKind::IndexOutOfRange);
    }
    let b = u32::try_from(b).unwrap_or(u32::MAX).min(h.len);
    let a = u32::try_from(a).unwrap_or(u32::MAX);
    let n = b.saturating_sub(a);
    let q = alloc(st, heap, h.kind, n.max(INITIAL_WORDS))?;
    let mut nh = heap.header(st, q)?;
    nh.len = n;
    write_header(st, heap, q, nh)?;
    for k in 0..n {
        let w = read_slot(st, p, &h, a + k)?;
        write_slot(st, heap, q, &nh, k, w)?;
        if h.kind == Kind::ListRef {
            incref(st, heap, w)?;
        }
    }
    Ok((u64::from(q), n))
}

/// Whether an element equals `v` structurally ([`deep_eq`]; plain bits in a `ListImm`). Also returns the
/// words compared.
pub fn contains(st: &Store, heap: Heap, l: u64, v: u64) -> VmResult<(bool, u32)> {
    let (p, h) = list(st, heap, l)?;
    let mut work = 0u32;
    for i in 0..h.len {
        let w = read_slot(st, p, &h, i)?;
        let (eq, n) = deep_eq(st, heap, w, v, h.kind == Kind::ListRef)?;
        work = work.saturating_add(n);
        if eq {
            return Ok((true, work));
        }
    }
    Ok((false, work))
}
