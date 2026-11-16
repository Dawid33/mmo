//! Deep structural equality and key hashing, directly on the flat memory (design D4, D6, decision 12).
//!
//! # Equality
//! Immediates compare by bits (a canonical NaN equals itself, `0.0 != -0.0`). Heap values of the same
//! kind compare by content: `Str` by bytes; `ListImm` / `ListRef` by length, then element by element;
//! `Obj` by tag, field count, then field by field (the `aux` bitmap says which fields are heap kinds);
//! `Dict` by live length, then every key of one found in the other with an equal value (insertion order is
//! ignored: `{a:1, b:2} == {b:2, a:1}`). Different kinds are unequal. Two identical slots are equal without a walk.
//! Dict keys are compared as keys: a dict inside one is `Corrupt` (no key can hold a dict, [`key_hash`]
//! refuses one), so a key comparison inside a dict comparison never enters another dict.
//!
//! # Key hash
//! [`key_hash`] is FNV-1a 64 ([`Fnv64`]) over a type-tagged walk of the key (this replaces design D4's
//! "over the key's wire bytes": the wire format is a later task and a hash defined on memory needs none):
//! * an immediate (anything that is not a pointer, or any value of a non-ref position): tag `0x00`, then its
//!   8 little-endian bytes;
//! * `Str`: tag `0x01`, its byte length as 8 LE bytes, then the bytes;
//! * a list: tag `0x02`, its length as 8 LE bytes, then each element by these rules;
//! * `Obj`: tag `0x03`, the variant tag and the field count as 8 LE bytes each, then each field by these
//!   rules (heap-kind fields, per `aux`, as values; the others as immediates).
//!
//! A dict or any other kind is not a valid key (`Corrupt`). Keys equal by [`deep_eq`] hash equal.
//!
//! # Reported work
//! Both return the number of words compared or hashed next to the result; callers charge fuel for it.
//! A value can share sublists (valid memory is a DAG), so a walk can be exponential in its depth: every
//! walk carries a [`Walk`] budget of `WORK_WORDS_MAX` words and fails with `ValueTooLarge` the moment it
//! is exceeded, so the reported work is always at most `WORK_WORDS_MAX`.
//!
//! # No recursion
//! Values have no nesting cap (recursive data types are allowed, decision 17), so both walks are loops
//! over an explicit stack, visiting in the order a recursive pre-order walk would: the same charges, the
//! same hash input, the same first difference. Corrupt memory with a cycle ends at the budget (or as
//! `Corrupt`), never in a hang or a stack overflow.

use crate::alloc::{Heap, Kind, ObjHeader};
use crate::consts::WORK_WORDS_MAX;
use crate::dict::{self, EqStep};
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

/// The slots of a list or an object, walked from `i` on: which ones hold heap kinds.
#[derive(Clone, Copy)]
enum Slots {
    /// A list: every element is a heap kind, or none is.
    All(bool),
    /// An object: the `aux` bitmap.
    Bits(u32),
}

impl Slots {
    fn is_ref(self, i: u32) -> bool {
        match self {
            Slots::All(r) => r,
            Slots::Bits(b) => 1u32.checked_shl(i).is_some_and(|m| b & m != 0),
        }
    }
}

/// What an equality walk still has to do, innermost last.
enum EqFrame {
    /// Two slots to compare (`is_ref`: they are heap kinds).
    Pair(u64, u64, bool),
    /// Two lists or objects of the same shape, compared from slot `i` on.
    Seq {
        a: (u32, ObjHeader),
        b: (u32, ObjHeader),
        i: u32,
        slots: Slots,
    },
    /// Two dicts of the same live length, compared entry by entry.
    Dict(dict::EqCursor),
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
    eq(st, heap, a, b, is_ref, walk, false)
}

/// [`deep_eq_in`] of two dict keys: a dict inside either is `Corrupt` (see the module docs).
pub(crate) fn key_eq_in(
    st: &Store,
    heap: Heap,
    a: u64,
    b: u64,
    is_ref: bool,
    walk: &mut Walk,
) -> VmResult<bool> {
    eq(st, heap, a, b, is_ref, walk, true)
}

fn eq(
    st: &Store,
    heap: Heap,
    a: u64,
    b: u64,
    is_ref: bool,
    work: &mut Walk,
    keys: bool,
) -> VmResult<bool> {
    let mut stack = vec![EqFrame::Pair(a, b, is_ref)];
    while let Some(top) = stack.last_mut() {
        match top {
            EqFrame::Pair(a, b, is_ref) => {
                let (a, b, is_ref) = (*a, *b, *is_ref);
                stack.pop();
                work.charge(1)?;
                if a == b {
                    continue;
                }
                if !is_ref || !is_ptr(a) || !is_ptr(b) {
                    return Ok(false);
                }
                match open_pair(st, heap, a, b, work, keys)? {
                    Some(Some(f)) => stack.push(f),
                    Some(None) => {}
                    None => return Ok(false),
                }
            }
            EqFrame::Seq { a, b, i, slots } => {
                if *i >= a.1.len {
                    stack.pop();
                    continue;
                }
                let x = read_slot(st, a.0, &a.1, *i)?;
                let y = read_slot(st, b.0, &b.1, *i)?;
                let r = slots.is_ref(*i);
                *i += 1;
                stack.push(EqFrame::Pair(x, y, r));
            }
            EqFrame::Dict(c) => match c.step(st, heap, work)? {
                EqStep::Done => {
                    stack.pop();
                }
                EqStep::Differs => return Ok(false),
                EqStep::Values(x, y, r) => stack.push(EqFrame::Pair(x, y, r)),
            },
        }
    }
    Ok(true)
}

/// Two distinct heap pointers, one level down: `None` = unequal; `Some(None)` = equal (strings);
/// `Some(Some(frame))` = equal so far, their contents still to compare.
fn open_pair(
    st: &Store,
    heap: Heap,
    a: u64,
    b: u64,
    work: &mut Walk,
    keys: bool,
) -> VmResult<Option<Option<EqFrame>>> {
    let (pa, ha) = hdr(st, heap, a)?;
    let (pb, hb) = hdr(st, heap, b)?;
    if ha.kind != hb.kind {
        return Ok(None);
    }
    let seq = |slots| EqFrame::Seq {
        a: (pa, ha),
        b: (pb, hb),
        i: 0,
        slots,
    };
    Ok(match ha.kind {
        Kind::Str => {
            work.charge(ha.len.div_ceil(8))?;
            same_text(st, heap, a, b)?.then_some(None)
        }
        Kind::ListImm | Kind::ListRef => {
            (ha.len == hb.len).then(|| Some(seq(Slots::All(ha.kind == Kind::ListRef))))
        }
        Kind::Obj => {
            check_obj_shape(&ha)?;
            check_obj_shape(&hb)?;
            let same = ha.tag == hb.tag && ha.len == hb.len && ha.aux == hb.aux;
            same.then(|| Some(seq(Slots::Bits(ha.aux))))
        }
        Kind::Dict if !keys => {
            dict::EqCursor::open(st, heap, a, b)?.map(|c| Some(EqFrame::Dict(c)))
        }
        _ => return Err(FaultKind::Corrupt),
    })
}

/// The hash of a dict key (see the module docs for the definition). `is_ref`: the key slot is a heap kind.
/// Also returns the words hashed.
pub fn key_hash(st: &Store, heap: Heap, key: u64, is_ref: bool) -> VmResult<(u64, u32)> {
    let mut walk = Walk::default();
    let r = key_hash_in(st, heap, key, is_ref, &mut walk)?;
    Ok((r, walk.used))
}

/// What a hash walk still has to do, innermost last.
enum HashFrame {
    /// A slot to hash (`is_ref`: it is a heap kind).
    Value(u64, bool),
    /// A list or an object, hashed from slot `i` on.
    Seq {
        p: u32,
        h: ObjHeader,
        i: u32,
        slots: Slots,
    },
}

/// [`key_hash`] charging a budget the caller shares with its other walks.
pub(crate) fn key_hash_in(
    st: &Store,
    heap: Heap,
    key: u64,
    is_ref: bool,
    work: &mut Walk,
) -> VmResult<u64> {
    let mut h = Fnv64::default();
    let mut stack = vec![HashFrame::Value(key, is_ref)];
    while let Some(top) = stack.last_mut() {
        match top {
            HashFrame::Value(v, is_ref) => {
                let (v, is_ref) = (*v, *is_ref);
                stack.pop();
                work.charge(1)?;
                if !is_ref || !is_ptr(v) {
                    h.write(&[T_IMM]);
                    h.write(&v.to_le_bytes());
                    continue;
                }
                if let Some(f) = hash_heap(st, heap, v, &mut h, work)? {
                    stack.push(f);
                }
            }
            HashFrame::Seq { p, h: hd, i, slots } => {
                if *i >= hd.len {
                    stack.pop();
                    continue;
                }
                let e = read_slot(st, *p, hd, *i)?;
                let r = slots.is_ref(*i);
                *i += 1;
                stack.push(HashFrame::Value(e, r));
            }
        }
    }
    Ok(h.finish())
}

/// A heap pointer, one level down: hashes what it holds directly and returns the frame of its slots.
fn hash_heap(
    st: &Store,
    heap: Heap,
    v: u64,
    h: &mut Fnv64,
    work: &mut Walk,
) -> VmResult<Option<HashFrame>> {
    let (p, hd) = hdr(st, heap, v)?;
    let seq = |slots| HashFrame::Seq {
        p,
        h: hd,
        i: 0,
        slots,
    };
    Ok(match hd.kind {
        Kind::Str => {
            work.charge(hd.len.div_ceil(8))?;
            h.write(&[T_STR]);
            h.write(&u64::from(hd.len).to_le_bytes());
            h.write(&bytes_of(st, heap, v)?);
            None
        }
        Kind::ListImm | Kind::ListRef => {
            h.write(&[T_LIST]);
            h.write(&u64::from(hd.len).to_le_bytes());
            Some(seq(Slots::All(hd.kind == Kind::ListRef)))
        }
        Kind::Obj => {
            check_obj_shape(&hd)?;
            h.write(&[T_OBJ]);
            h.write(&u64::from(hd.tag).to_le_bytes());
            h.write(&u64::from(hd.len).to_le_bytes());
            Some(seq(Slots::Bits(hd.aux)))
        }
        _ => return Err(FaultKind::Corrupt),
    })
}
