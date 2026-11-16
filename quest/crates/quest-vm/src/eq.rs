//! Deep structural equality and key hashing, directly on the flat memory (design D4, D6, decision 12).
//!
//! # Equality
//! Immediates compare by bits (a canonical NaN equals itself, `0.0 != -0.0`). Heap values of the same
//! kind compare by content: `Str` by bytes; `ListImm` / `ListRef` by length, then element by element;
//! `Obj` by tag, field count, then field by field (the `aux` bitmap says which fields are heap kinds);
//! `Dict` by live length, then every key of one found in the other with an equal value (insertion order is
//! ignored: `{a:1, b:2} == {b:2, a:1}`). Different kinds are unequal. Two identical slots are equal without a walk. Nesting is capped
//! at `WIRE_DEPTH_MAX`: deeper is `Corrupt` (unreachable from valid memory: the compiler rejects recursive user types and types nested deeper than 32, and memory has no cycles).
//!
//! # Key hash
//! [`key_hash`] is FNV-1a 64 ([`Fnv64`]) over a type-tagged walk of the key (this replaces design D4's
//! "over the key's wire bytes": the wire format is a later task and a hash defined on memory needs none):
//! * an immediate (anything that is not a pointer, or any value of a non-ref position): tag `0x00`, then its
//!   8 little-endian bytes;
//! * `Str`: tag `0x01`, its byte length as 8 LE bytes, then the bytes;
//! * a list: tag `0x02`, its length as 8 LE bytes, then each element by these rules;
//! * `Obj`: tag `0x03`, the variant tag and the field count as 8 LE bytes each, then each field by these
//!   rules (heap-kind fields, per `aux`, recursively; the others as immediates).
//!
//! A dict or any other kind is not a valid key (`Corrupt`). Keys equal by [`deep_eq`] hash equal.
//!
//! # Reported work
//! Both return the number of words compared or hashed next to the result; callers charge fuel for it.
//! A value can share sublists (valid memory is a DAG), so a walk can be exponential in its depth: every
//! walk carries a [`Walk`] budget of `WORK_WORDS_MAX` words and fails with `ValueTooLarge` the moment it
//! is exceeded, so the reported work is always at most `WORK_WORDS_MAX`.

use crate::alloc::{Heap, Kind};
use crate::consts::{WIRE_DEPTH_MAX, WORK_WORDS_MAX};
use crate::dict;
use crate::fault::{FaultKind, VmResult};
use crate::hash::Fnv64;
use crate::object::{check_obj_shape, hdr, is_ptr, read_slot};
use crate::store::Store;
use crate::string::{bytes_of, same_text};
use std::hash::Hasher;

const T_IMM: u8 = 0;
const T_STR: u8 = 1;
const T_LIST: u8 = 2;
const T_OBJ: u8 = 3;

/// The words one structural walk has visited. One per top-level call (a lookup or `list::contains`
/// shares one across all its comparisons); passing `WORK_WORDS_MAX` stops the walk at once.
#[derive(Default)]
pub(crate) struct Walk {
    pub(crate) used: u32,
    /// Heap values entered on the way to the current one. It travels with the budget through every nested
    /// call (a dict key comparison inside a value comparison continues it), so no entry point restarts it.
    depth: usize,
}

impl Walk {
    pub(crate) fn charge(&mut self, n: u32) -> VmResult<()> {
        match self.used.checked_add(n) {
            Some(u) if u <= WORK_WORDS_MAX => {
                self.used = u;
                Ok(())
            }
            _ => Err(FaultKind::ValueTooLarge),
        }
    }
}

/// Runs `f` one heap level deeper; past `WIRE_DEPTH_MAX` levels it fails `Corrupt`.
pub(crate) fn nested<T, E: From<FaultKind>>(
    walk: &mut Walk,
    f: impl FnOnce(&mut Walk) -> Result<T, E>,
) -> Result<T, E> {
    if walk.depth >= WIRE_DEPTH_MAX {
        return Err(FaultKind::Corrupt.into());
    }
    walk.depth += 1;
    let r = f(walk);
    walk.depth -= 1;
    r
}

/// Deep structural equality of two slots. `is_ref`: the slots are heap kinds (a pointer or an immediate
/// variant); otherwise they are plain immediates compared by bits. Also returns the words compared.
pub fn deep_eq(st: &Store, heap: Heap, a: u64, b: u64, is_ref: bool) -> VmResult<(bool, u32)> {
    let mut walk = Walk::default();
    let r = deep_eq_in(st, heap, a, b, is_ref, &mut walk)?;
    Ok((r, walk.used))
}

/// [`deep_eq`] charging a budget the caller shares with its other walks.
pub(crate) fn deep_eq_in(
    st: &Store,
    heap: Heap,
    a: u64,
    b: u64,
    is_ref: bool,
    walk: &mut Walk,
) -> VmResult<bool> {
    eq(st, heap, a, b, is_ref, walk)
}

fn eq(st: &Store, heap: Heap, a: u64, b: u64, is_ref: bool, work: &mut Walk) -> VmResult<bool> {
    work.charge(1)?;
    if a == b {
        return Ok(true);
    }
    if !is_ref || !is_ptr(a) || !is_ptr(b) {
        return Ok(false);
    }
    nested(work, |work| eq_heap(st, heap, a, b, work))
}

/// Two distinct heap pointers, one level down.
fn eq_heap(st: &Store, heap: Heap, a: u64, b: u64, work: &mut Walk) -> VmResult<bool> {
    let (pa, ha) = hdr(st, heap, a)?;
    let (pb, hb) = hdr(st, heap, b)?;
    if ha.kind != hb.kind {
        return Ok(false);
    }
    match ha.kind {
        Kind::Str => {
            work.charge(ha.len.div_ceil(8))?;
            same_text(st, heap, a, b)
        }
        Kind::ListImm | Kind::ListRef => {
            if ha.len != hb.len {
                return Ok(false);
            }
            let refs = ha.kind == Kind::ListRef;
            for i in 0..ha.len {
                let x = read_slot(st, pa, &ha, i)?;
                let y = read_slot(st, pb, &hb, i)?;
                if !eq(st, heap, x, y, refs, work)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Kind::Obj => {
            check_obj_shape(&ha)?;
            check_obj_shape(&hb)?;
            if ha.tag != hb.tag || ha.len != hb.len || ha.aux != hb.aux {
                return Ok(false);
            }
            for i in 0..ha.len {
                let x = read_slot(st, pa, &ha, i)?;
                let y = read_slot(st, pb, &hb, i)?;
                if !eq(st, heap, x, y, ha.aux & (1 << i) != 0, work)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Kind::Dict => dict_eq(st, heap, a, b, work),
        _ => Err(FaultKind::Corrupt),
    }
}

fn dict_eq(st: &Store, heap: Heap, a: u64, b: u64, work: &mut Walk) -> VmResult<bool> {
    dict::equal(st, heap, a, b, work, &mut |x, y, refs, walk| {
        eq(st, heap, x, y, refs, walk)
    })
}

/// The hash of a dict key (see the module docs for the definition). `is_ref`: the key slot is a heap kind.
/// Also returns the words hashed.
pub fn key_hash(st: &Store, heap: Heap, key: u64, is_ref: bool) -> VmResult<(u64, u32)> {
    let mut walk = Walk::default();
    let r = key_hash_in(st, heap, key, is_ref, &mut walk)?;
    Ok((r, walk.used))
}

/// [`key_hash`] charging a budget the caller shares with its other walks.
pub(crate) fn key_hash_in(
    st: &Store,
    heap: Heap,
    key: u64,
    is_ref: bool,
    walk: &mut Walk,
) -> VmResult<u64> {
    let mut h = Fnv64::default();
    hash(st, heap, key, is_ref, &mut h, walk)?;
    Ok(h.finish())
}

fn hash(
    st: &Store,
    heap: Heap,
    v: u64,
    is_ref: bool,
    h: &mut Fnv64,
    work: &mut Walk,
) -> VmResult<()> {
    work.charge(1)?;
    if !is_ref || !is_ptr(v) {
        h.write(&[T_IMM]);
        h.write(&v.to_le_bytes());
        return Ok(());
    }
    nested(work, |work| hash_heap(st, heap, v, h, work))
}

/// A heap pointer, one level down.
fn hash_heap(st: &Store, heap: Heap, v: u64, h: &mut Fnv64, work: &mut Walk) -> VmResult<()> {
    let (p, hd) = hdr(st, heap, v)?;
    match hd.kind {
        Kind::Str => {
            work.charge(hd.len.div_ceil(8))?;
            h.write(&[T_STR]);
            h.write(&u64::from(hd.len).to_le_bytes());
            h.write(&bytes_of(st, heap, v)?);
        }
        Kind::ListImm | Kind::ListRef => {
            h.write(&[T_LIST]);
            h.write(&u64::from(hd.len).to_le_bytes());
            for i in 0..hd.len {
                let e = read_slot(st, p, &hd, i)?;
                hash(st, heap, e, hd.kind == Kind::ListRef, h, work)?;
            }
        }
        Kind::Obj => {
            check_obj_shape(&hd)?;
            h.write(&[T_OBJ]);
            h.write(&u64::from(hd.tag).to_le_bytes());
            h.write(&u64::from(hd.len).to_le_bytes());
            for i in 0..hd.len {
                let f = read_slot(st, p, &hd, i)?;
                hash(st, heap, f, hd.aux & (1 << i) != 0, h, work)?;
            }
        }
        _ => return Err(FaultKind::Corrupt),
    }
    Ok(())
}
