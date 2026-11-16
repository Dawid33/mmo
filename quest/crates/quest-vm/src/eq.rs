//! Deep structural equality and key hashing, directly on the flat memory (design D4, D6, decision 12).
//!
//! # Equality
//! Immediates compare by bits (a canonical NaN equals itself, `0.0 != -0.0`). Heap values of the same
//! kind compare by content: `Str` by bytes; `ListImm` / `ListRef` by length, then element by element;
//! `Obj` by tag, field count, then field by field (the `aux` bitmap says which fields are heap kinds);
//! `Dict` by live length, then the live entries in insertion order (same keys in the same order with equal
//! values). Different kinds are unequal. Two identical slots are equal without a walk. Nesting is capped
//! at `WIRE_DEPTH_MAX`: deeper is `Corrupt` (valid memory has no cycles and the compiler bounds types).
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

fn enter(depth: usize) -> VmResult<usize> {
    if depth >= WIRE_DEPTH_MAX {
        return Err(FaultKind::Corrupt);
    }
    Ok(depth + 1)
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
    eq(st, heap, a, b, is_ref, 0, walk)
}

fn eq(
    st: &Store,
    heap: Heap,
    a: u64,
    b: u64,
    is_ref: bool,
    depth: usize,
    work: &mut Walk,
) -> VmResult<bool> {
    work.charge(1)?;
    if a == b {
        return Ok(true);
    }
    if !is_ref || !is_ptr(a) || !is_ptr(b) {
        return Ok(false);
    }
    let depth = enter(depth)?;
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
                if !eq(st, heap, x, y, refs, depth, work)? {
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
                if !eq(st, heap, x, y, ha.aux & (1 << i) != 0, depth, work)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Kind::Dict => dict_eq(st, heap, a, b, depth, work),
        _ => Err(FaultKind::Corrupt),
    }
}

fn dict_eq(
    st: &Store,
    heap: Heap,
    a: u64,
    b: u64,
    depth: usize,
    work: &mut Walk,
) -> VmResult<bool> {
    if dict::len(st, heap, a)? != dict::len(st, heap, b)? {
        return Ok(false);
    }
    let (keys_ref, vals_ref) = dict::flags(st, heap, a)?;
    if (keys_ref, vals_ref) != dict::flags(st, heap, b)? {
        return Ok(false);
    }
    let (mut pa, mut pb) = (0i64, 0i64);
    loop {
        let (na, wa) = dict::next(st, heap, a, pa)?;
        let (nb, wb) = dict::next(st, heap, b, pb)?;
        work.charge(wa)?;
        work.charge(wb)?;
        if na < 0 || nb < 0 {
            // Equal live counts: both ran out together, or the memory is inconsistent.
            return if na < 0 && nb < 0 {
                Ok(true)
            } else {
                Err(FaultKind::Corrupt)
            };
        }
        let (ka, kb) = (
            dict::key_at(st, heap, a, na)?,
            dict::key_at(st, heap, b, nb)?,
        );
        if !eq(st, heap, ka, kb, keys_ref, depth, work)? {
            return Ok(false);
        }
        let (va, vb) = (
            dict::val_at(st, heap, a, na)?,
            dict::val_at(st, heap, b, nb)?,
        );
        if !eq(st, heap, va, vb, vals_ref, depth, work)? {
            return Ok(false);
        }
        (pa, pb) = (na + 1, nb + 1);
    }
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
    hash(st, heap, key, is_ref, 0, &mut h, walk)?;
    Ok(h.finish())
}

fn hash(
    st: &Store,
    heap: Heap,
    v: u64,
    is_ref: bool,
    depth: usize,
    h: &mut Fnv64,
    work: &mut Walk,
) -> VmResult<()> {
    work.charge(1)?;
    if !is_ref || !is_ptr(v) {
        h.write(&[T_IMM]);
        h.write(&v.to_le_bytes());
        return Ok(());
    }
    let depth = enter(depth)?;
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
                hash(st, heap, e, hd.kind == Kind::ListRef, depth, h, work)?;
            }
        }
        Kind::Obj => {
            check_obj_shape(&hd)?;
            h.write(&[T_OBJ]);
            h.write(&u64::from(hd.tag).to_le_bytes());
            h.write(&u64::from(hd.len).to_le_bytes());
            for i in 0..hd.len {
                let f = read_slot(st, p, &hd, i)?;
                hash(st, heap, f, hd.aux & (1 << i) != 0, depth, h, work)?;
            }
        }
        _ => return Err(FaultKind::Corrupt),
    }
    Ok(())
}
