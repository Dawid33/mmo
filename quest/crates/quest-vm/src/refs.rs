//! Second-class references (design D4, D5; spec §5.3): `RefMake`, `RefField`, `RefIndex`, `RefKey`,
//! `RefLoad`, `RefTake`, `RefStore`.
//!
//! # The `RefPath` object
//! A reference is a path, never an address: a root static slot plus the steps from it, re-resolved on
//! every use, so a reference never dangles; it only stops resolving (the fault `BadRef`).
//! * Header: `tag` = the root slot, `aux` bit 0 = the root slot holds a heap kind (`RefMake`'s `c`; the
//!   other bits are 0), `len` = the number of steps, at most `REF_PATH_MAX`.
//! * Payload: `STEP_WORDS` words per step, in order: a step code, then its value:
//!   * `STEP_FIELD`: a field index;
//!   * `STEP_INDEX`: a list index (an `i64`'s bits, checked when resolved, so it may be negative);
//!   * `STEP_KEY` / `STEP_KEY_REF`: a dict key, an immediate / a heap kind. A heap-kind key is owned by
//!     the path: a reference is taken when the step is stored and given back when the path is reaped.
//!
//! A path is immutable once built. `RefField` / `RefIndex` / `RefKey` allocate a NEW path (the source's
//! steps plus one, heap-kind keys increfed) and leave the source valid; nothing ever makes a `RefPath`
//! unique (`make_unique` refuses one). The reaper releases the heap-kind keys from the last step down,
//! at most its budget per step, `len` being the cursor ([`reap`]).
//!
//! # Resolution
//! The walk starts at the root slot and follows each step into the container held by the place before
//! it. A place that holds 0 or an odd immediate where a container is needed, a container of the wrong
//! kind, a field index at or past the object's `len`, a list index out of range (negative included) and
//! a missing key are `BadRef`. Memory that does not parse (a header, a step code, a step after a place
//! the path itself says is not a heap kind, a root slot outside the static region) is `Corrupt`. Whether
//! the final target is a heap kind is read from its container at that moment (an object's `aux` bit,
//! `ListRef` versus `ListImm`, the dict's value flag), or from the root flag for a path of no steps.
//!
//! # The instructions
//! * `RefLoad dst, r`: the target is duplicated into `dst` (a reference for a pointer).
//! * `RefTake dst, r` / `RefStore r, src`: every container on the path is made unique IN PLACE first
//!   (the root slot's object, then each nested one; a clone replaces the pointer in its parent), as the
//!   compiler's take -> mutate -> put back chain does by hand. `RefTake` then moves a heap-kind target out
//!   (0 left behind) or copies an immediate; `RefStore` releases what a heap-kind target held and moves
//!   `src` in (its slot zeroed), or copies an immediate. A key store needs the key to exist: a key whose
//!   value was taken (0) is an existing entry, a removed key is `BadRef` (nothing is created).
//! * `RefKey` reads the key flag from the dict the source path leads to at that moment, so the source
//!   must resolve (else `BadRef`); `RefField` / `RefIndex` only append.
//!
//! Every fault of these instructions is undone by the run loop through the undo log (`interp`), so a
//! walk that cloned half a path before failing leaves nothing behind.
//!
//! # Fuel
//! `FUEL_HEAP` + 1 per step walked for `RefLoad` / `RefTake` / `RefStore`; `FUEL_ALLOC` + 1 per step
//! copied for `RefMake` and the extensions; plus, through the [`Meter`], the reported work of every clone,
//! key hash and compare on the way.

use crate::alloc::{Heap, Kind, ObjHeader};
use crate::bytecode::{Instr, Op, REF_TARGET_REF};
use crate::consts::REF_PATH_MAX;
use crate::dict;
use crate::fault::{FaultKind, VmResult};
use crate::interp::{Exec, Fault, Meter};
use crate::list;
use crate::object::{
    alloc, capacity, check_obj_shape, hdr, incref, is_ptr, make_unique, obj, read_slot, release,
    require_unique, write_header, write_slot,
};
use crate::store::Store;
use crate::vm::Vm;

/// Payload words per step: the code, then the value.
const STEP_WORDS: u32 = 2;
/// `aux` bit 0: the root slot holds a heap kind.
const ROOT_REF: u32 = 1;

const STEP_FIELD: u64 = 1;
const STEP_INDEX: u64 = 2;
const STEP_KEY: u64 = 3;
const STEP_KEY_REF: u64 = 4;

/// One step of a path.
#[derive(Clone, Copy)]
enum Step {
    Field(u32),
    Index(i64),
    /// `is_ref`: the key is a heap kind (owned by the path).
    Key {
        key: u64,
        is_ref: bool,
    },
}

impl Step {
    fn decode(code: u64, value: u64) -> VmResult<Step> {
        Ok(match code {
            STEP_FIELD => Step::Field(u32::try_from(value).map_err(|_| FaultKind::Corrupt)?),
            STEP_INDEX => Step::Index(value as i64),
            STEP_KEY => Step::Key {
                key: value,
                is_ref: false,
            },
            STEP_KEY_REF => Step::Key {
                key: value,
                is_ref: true,
            },
            _ => return Err(FaultKind::Corrupt),
        })
    }

    fn encode(self) -> (u64, u64) {
        match self {
            Step::Field(f) => (STEP_FIELD, u64::from(f)),
            Step::Index(i) => (STEP_INDEX, i as u64),
            Step::Key { key, is_ref: false } => (STEP_KEY, key),
            Step::Key { key, is_ref: true } => (STEP_KEY_REF, key),
        }
    }

    /// The value the path owns, if any (a heap-kind key).
    fn owned(self) -> Option<u64> {
        match self {
            Step::Key { key, is_ref: true } => Some(key),
            _ => None,
        }
    }
}

/// A `RefPath` header read from memory: no stray `aux` bits, at most `REF_PATH_MAX` steps, all inside
/// the block.
fn check_shape(h: &ObjHeader) -> VmResult<()> {
    let steps_fit = u64::from(h.len) * u64::from(STEP_WORDS) <= u64::from(capacity(h));
    if h.kind != Kind::RefPath || h.aux & !ROOT_REF != 0 || h.len > REF_PATH_MAX || !steps_fit {
        return Err(FaultKind::Corrupt);
    }
    Ok(())
}

fn path(st: &Store, heap: Heap, r: u64) -> VmResult<(u32, ObjHeader)> {
    let (p, h) = hdr(st, heap, r)?;
    check_shape(&h)?;
    Ok((p, h))
}

fn step_at(st: &Store, p: u32, h: &ObjHeader, i: u32) -> VmResult<Step> {
    let at = i * STEP_WORDS;
    Step::decode(read_slot(st, p, h, at)?, read_slot(st, p, h, at + 1)?)
}

/// One reap step's share for a dead path: releases the heap-kind keys of the last steps (at most the
/// budget, one per step), shrinking `h.len` as it goes; `true` when no step is left (the caller frees the
/// block).
pub(crate) fn reap(
    st: &mut Store,
    heap: Heap,
    p: u32,
    h: &mut ObjHeader,
    budget: &mut usize,
    work: &mut u32,
) -> VmResult<bool> {
    check_shape(h)?;
    while *budget > 0 && h.len > 0 {
        let step = step_at(st, p, h, h.len - 1)?;
        h.len -= 1;
        if let Some(key) = step.owned() {
            release(st, heap, key)?;
        }
        *budget -= 1;
        *work = work.saturating_add(1);
    }
    Ok(h.len == 0)
}

/// A place a path leads to. Containers are held by value (the pointer read on the way).
#[derive(Clone, Copy)]
enum Place {
    Slot(u16),
    Field(u64, u32),
    Index(u64, u32),
    Key(u64, u64),
}

/// What a resolved path leads to: the place, whether it holds a heap kind, its value (raw, no reference
/// taken) and the number of steps walked.
struct Target {
    place: Place,
    is_ref: bool,
    value: u64,
    steps: u32,
}

impl Vm {
    pub(crate) fn exec_ref(&mut self, ins: Instr, m: &mut Meter) -> Exec<()> {
        let Instr { op, a, b, c } = ins;
        let heap = self.heap;
        match op {
            Op::RefMake => {
                let root_ref = match c {
                    0 => 0,
                    REF_TARGET_REF => ROOT_REF,
                    _ => return Err(Fault::corrupt("bad RefMake flag")),
                };
                let p = alloc(&mut self.store, heap, Kind::RefPath, 0)?;
                let mut h = heap.header(&self.store, p)?;
                h.tag = b;
                h.aux = root_ref;
                write_header(&mut self.store, heap, p, h)?;
                self.put(a, u64::from(p), true)
            }
            Op::RefField => self.extend(a, b, Step::Field(u32::from(c)), m),
            Op::RefIndex => {
                let i = self.int_in(c)?;
                self.extend(a, b, Step::Index(i), m)
            }
            Op::RefKey => {
                // The key flag lives in the dict the base path leads to now.
                let t = self.resolve_ref(self.rd(b)?, false, m)?;
                if !t.is_ref || !is_ptr(t.value) {
                    return Err(FaultKind::BadRef.into());
                }
                if hdr(&self.store, heap, t.value)?.1.kind != Kind::Dict {
                    return Err(FaultKind::BadRef.into());
                }
                let (kr, _) = dict::flags(&self.store, heap, t.value)?;
                let key = self.rd(c)?;
                self.extend(a, b, Step::Key { key, is_ref: kr }, m)
            }
            Op::RefLoad => {
                let t = self.resolve_ref(self.rd(b)?, false, m)?;
                m.charge(u64::from(t.steps));
                let v = self.dup_out(t.value, t.is_ref)?;
                self.put(a, v, t.is_ref)
            }
            Op::RefTake => {
                let t = self.resolve_ref(self.rd(b)?, true, m)?;
                m.charge(u64::from(t.steps));
                if t.is_ref {
                    self.write_place(t.place, 0, m)?;
                }
                self.put(a, t.value, t.is_ref)
            }
            Op::RefStore => {
                let t = self.resolve_ref(self.rd(a)?, true, m)?;
                m.charge(u64::from(t.steps));
                let v = self.rd(b)?;
                self.write_place(t.place, v, m)?;
                if t.is_ref {
                    release(&mut self.store, heap, t.value)?;
                    self.wt(b, 0)?;
                }
                Ok(())
            }
            _ => Err(Fault::corrupt("not a reference instruction")),
        }
    }

    /// `dst` = a new path: the one in `base`'s slot plus `step`. A heap-kind key in `step` is borrowed
    /// from its slot: the new path takes a reference of its own.
    fn extend(&mut self, dst: u16, base: u16, step: Step, m: &mut Meter) -> Exec<()> {
        let heap = self.heap;
        let (p, h) = path(&self.store, heap, self.rd(base)?)?;
        let n = h.len + 1;
        if n > REF_PATH_MAX {
            return Err(Fault::corrupt("a reference path deeper than REF_PATH_MAX"));
        }
        m.charge(u64::from(h.len));
        let q = alloc(&mut self.store, heap, Kind::RefPath, n * STEP_WORDS)?;
        let mut nh = heap.header(&self.store, q)?;
        nh.tag = h.tag;
        nh.aux = h.aux;
        nh.len = n;
        write_header(&mut self.store, heap, q, nh)?;
        for i in 0..n {
            let s = if i < h.len {
                step_at(&self.store, p, &h, i)?
            } else {
                step
            };
            let (code, value) = s.encode();
            write_slot(&mut self.store, heap, q, &nh, i * STEP_WORDS, code)?;
            write_slot(&mut self.store, heap, q, &nh, i * STEP_WORDS + 1, value)?;
            if let Some(key) = s.owned() {
                incref(&mut self.store, heap, key)?;
            }
        }
        self.put(dst, u64::from(q), true)
    }

    /// Walks the path `r`. `unique`: every container on the way is made unique in place.
    fn resolve_ref(&mut self, r: u64, unique: bool, m: &mut Meter) -> Exec<Target> {
        let heap = self.heap;
        let (p, h) = path(&self.store, heap, r)?;
        let root = h.tag;
        if u32::from(root) >= self.store.layout.static_words {
            return Err(Fault::corrupt("a reference root outside the static region"));
        }
        let mut t = Target {
            place: Place::Slot(root),
            is_ref: h.aux & ROOT_REF != 0,
            value: self.rd(root)?,
            steps: h.len,
        };
        for i in 0..h.len {
            let step = step_at(&self.store, p, &h, i)?;
            if !t.is_ref {
                return Err(Fault::corrupt("a reference step into an immediate"));
            }
            if !is_ptr(t.value) {
                return Err(FaultKind::BadRef.into());
            }
            let cont = t.value;
            let (_, ch) = hdr(&self.store, heap, cont)?;
            // Check the step and read the value it leads to before any clone: a clone holds the same
            // words, so they stay valid for the copy.
            let (place, is_ref, value) = match (step, ch.kind) {
                (Step::Field(f), Kind::Obj) => {
                    check_obj_shape(&ch)?;
                    if f >= ch.len {
                        return Err(FaultKind::BadRef.into());
                    }
                    let v = obj::get_field(&self.store, heap, cont, f)?;
                    (Place::Field(cont, f), ch.aux & (1 << f) != 0, v)
                }
                (Step::Index(ix), Kind::ListImm | Kind::ListRef) => {
                    let at = match u32::try_from(ix) {
                        Ok(at) if at < ch.len => at,
                        _ => return Err(FaultKind::BadRef.into()),
                    };
                    let v = list::get(&self.store, heap, cont, ix)?;
                    (Place::Index(cont, at), ch.kind == Kind::ListRef, v)
                }
                (Step::Key { key, .. }, Kind::Dict) => {
                    let (found, words) = dict::get(&self.store, heap, cont, key)?;
                    m.words(words);
                    let v = found.ok_or(FaultKind::BadRef)?;
                    let (_, vr) = dict::flags(&self.store, heap, cont)?;
                    (Place::Key(cont, key), vr, v)
                }
                _ => return Err(FaultKind::BadRef.into()),
            };
            let place = if unique {
                let (u, words) = make_unique(&mut self.store, heap, cont)?;
                m.words(words);
                if u != cont {
                    // `make_unique` gave up the parent's reference to the shared original.
                    self.write_place(t.place, u, m)?;
                }
                match place {
                    Place::Field(_, f) => Place::Field(u, f),
                    Place::Index(_, at) => Place::Index(u, at),
                    Place::Key(_, key) => Place::Key(u, key),
                    Place::Slot(s) => Place::Slot(s),
                }
            } else {
                place
            };
            t = Target {
                place,
                is_ref,
                value,
                steps: h.len,
            };
        }
        Ok(t)
    }

    /// Writes `v` into a place, touching no refcount (the caller accounts for both values). The
    /// container must be unique.
    fn write_place(&mut self, place: Place, v: u64, m: &mut Meter) -> Exec<()> {
        let heap = self.heap;
        match place {
            Place::Slot(s) => self.wt(s, v),
            Place::Field(c, i) | Place::Index(c, i) => {
                let (p, h) = hdr(&self.store, heap, c)?;
                require_unique(&h)?;
                Ok(write_slot(&mut self.store, heap, p, &h, i, v)?)
            }
            Place::Key(d, key) => {
                let (old, words) = dict::replace(&mut self.store, heap, d, key, v)?;
                m.words(words);
                old.map(|_| ())
                    .ok_or_else(|| Fault::corrupt("a resolved key vanished"))
            }
        }
    }
}
