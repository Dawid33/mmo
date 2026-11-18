//! The heap instructions of design D5: `LoadStr`, `Dup` / `Move` / `Drop`, `Eq` and the object, list and
//! dict groups.
//!
//! # Ownership
//! The rules are `object`'s. A slot holding a pointer owns one reference, so an instruction that writes a
//! heap-kind slot releases what the slot held (a release of the last reference only links the object into
//! the zombie list; the run loop's reaper frees it). An instruction knows a slot is a heap kind from the op
//! itself (`Dup`, `Move`, `Drop`, the allocating ops) or from the container it reads: an object's `aux`
//! bitmap, `ListRef` versus `ListImm`, the dict's key and value flags. Immediates are plain copies.
//!
//! * `Get*` (destination, container, field / index / key): the element is duplicated out (a reference
//!   for a pointer). The container slot is borrowed.
//! * `Take*`: the element is moved out, leaving 0 in the container. The container in its slot is made
//!   unique first (the compiler emits `MakeUnique` before; doing it again is cheap and keeps a shared
//!   object from being written).
//! * `Set*` (container slot, field / index / key slot, source slot): the container is made unique in its
//!   slot, the old element released, the source moved in (its slot zeroed when the element is a heap
//!   kind; an immediate is copied). A `SetKey` key slot is **borrowed**: the instruction gives the dict a
//!   reference of its own (`dict::set` always takes one), so the key stays usable, as in `GetKey`.
//!
//! Field indexes are checked against the object's `len` (memory is untrusted): out of range is
//! `Corrupt`. List indexes out of range are `IndexOutOfRange`; a missing key in `GetKey` / `TakeKey` is
//! `MissingKey`. Everything a value-layer function reports as work is charged through the [`Meter`].

use std::sync::Arc;

use crate::alloc::Kind;
use crate::bytecode::{DICT_KEY_REFS, DICT_VAL_REFS, Instr, LIST_REFS, Op};
use crate::dict;
use crate::eq::deep_eq;
use crate::fault::FaultKind;
use crate::interp::{Exec, Fault, Meter};
use crate::list;
use crate::object::{check_obj_shape, hdr, incref, make_unique, obj, release};
use crate::string;
use crate::vm::Vm;

impl Vm {
    /// Writes `v` to `slot`; when the slot is a heap kind (`is_ref`), the value it held is released.
    pub(crate) fn put(&mut self, slot: u16, v: u64, is_ref: bool) -> Exec<()> {
        if !is_ref {
            return self.wt(slot, v);
        }
        let old = self.rd(slot)?;
        self.wt(slot, v)?;
        Ok(release(&mut self.store, self.heap, old)?)
    }

    /// `v` with a reference of its own when it is a heap kind.
    pub(crate) fn dup_out(&mut self, v: u64, is_ref: bool) -> Exec<u64> {
        if is_ref {
            incref(&mut self.store, self.heap, v)?;
        }
        Ok(v)
    }

    /// Makes the object in `slot` unique (a copy when it is shared), leaves it there and returns it.
    pub(crate) fn unique_in(&mut self, slot: u16, m: &mut Meter) -> Exec<u64> {
        let v = self.rd(slot)?;
        let (u, words) = make_unique(&mut self.store, self.heap, v)?;
        m.words(words);
        if u != v {
            // `make_unique` already gave up the slot's reference to the shared original.
            self.wt(slot, u)?;
        }
        Ok(u)
    }

    pub(crate) fn int_in(&self, slot: u16) -> Exec<i64> {
        Ok(self.rd(slot)? as i64)
    }

    /// Whether the list's elements are heap kinds.
    pub(crate) fn list_refs(&self, l: u64) -> Exec<bool> {
        match hdr(&self.store, self.heap, l)?.1.kind {
            Kind::ListRef => Ok(true),
            Kind::ListImm => Ok(false),
            _ => Err(FaultKind::Corrupt.into()),
        }
    }

    /// Whether field `i` of the object is a heap kind; a field past the object's `len` is `Corrupt`.
    fn field_refs(&self, o: u64, i: u32) -> Exec<bool> {
        let (_, h) = hdr(&self.store, self.heap, o)?;
        if h.kind != Kind::Obj || i >= h.len {
            return Err(FaultKind::Corrupt.into());
        }
        check_obj_shape(&h)?;
        Ok(h.aux & (1 << i) != 0)
    }

    pub(crate) fn exec_heap(&mut self, ins: Instr, m: &mut Meter) -> Exec<()> {
        let Instr { op, a, b, c } = ins;
        let heap = self.heap;
        match op {
            Op::LoadStr => {
                let p = Arc::clone(&self.program);
                let text = p
                    .strings
                    .get(ins.t() as usize)
                    .ok_or_else(|| Fault::corrupt("string out of range"))?;
                let v = string::new(&mut self.store, heap, text)?;
                m.words(u32::try_from(text.len().div_ceil(8)).unwrap_or(u32::MAX));
                self.put(a, v, true)
            }
            Op::Dup => {
                let v = self.rd(b)?;
                let v = self.dup_out(v, true)?;
                self.put(a, v, true)
            }
            Op::Move => {
                // Zero the source first: `Move a, a` is then a no-op.
                let v = self.rd(b)?;
                self.wt(b, 0)?;
                self.put(a, v, true)
            }
            Op::Drop => self.put(a, 0, true),
            Op::Eq => {
                let (x, y) = (self.rd(b)?, self.rd(c)?);
                let (same, words) = deep_eq(&self.store, heap, x, y, true)?;
                m.words(words);
                self.wt(a, u64::from(same))
            }
            Op::NewObj => {
                let shape = *self
                    .program
                    .shapes
                    .get(usize::from(b))
                    .ok_or_else(|| Fault::corrupt("shape out of range"))?;
                let o = obj::new(&mut self.store, heap, shape.fields, shape.ref_bits, c)?;
                self.put(a, o, true)
            }
            Op::GetField => {
                let (o, i) = (self.rd(b)?, u32::from(c));
                let r = self.field_refs(o, i)?;
                let v = obj::get_field(&self.store, heap, o, i)?;
                let v = self.dup_out(v, r)?;
                self.put(a, v, r)
            }
            Op::TakeField => {
                let o = self.unique_in(b, m)?;
                let i = u32::from(c);
                let r = self.field_refs(o, i)?;
                let v = obj::take_field(&mut self.store, heap, o, i)?;
                self.put(a, v, r)
            }
            Op::SetField => {
                let o = self.unique_in(a, m)?;
                let i = u32::from(b);
                let r = self.field_refs(o, i)?;
                let v = self.rd(c)?;
                obj::set_field(&mut self.store, heap, o, i, v)?;
                if r { self.wt(c, 0) } else { Ok(()) }
            }
            Op::GetTag => {
                let tag = obj::tag(&self.store, heap, self.rd(b)?)?;
                self.wt(a, u64::from(tag))
            }
            Op::MakeUnique => self.unique_in(a, m).map(|_| ()),
            Op::NewList => {
                let refs = match b {
                    0 => false,
                    LIST_REFS => true,
                    _ => return Err(Fault::corrupt("bad NewList flags")),
                };
                let l = list::new(&mut self.store, heap, refs)?;
                self.put(a, l, true)
            }
            Op::ListLen => {
                let n = list::len(&self.store, heap, self.rd(b)?)?;
                self.wt(a, u64::from(n))
            }
            Op::GetIndex => {
                let (l, i) = (self.rd(b)?, self.int_in(c)?);
                let r = self.list_refs(l)?;
                let v = list::get(&self.store, heap, l, i)?;
                let v = self.dup_out(v, r)?;
                self.put(a, v, r)
            }
            Op::TakeIndex => {
                let l = self.unique_in(b, m)?;
                let i = self.int_in(c)?;
                let r = self.list_refs(l)?;
                let v = list::take(&mut self.store, heap, l, i)?;
                self.put(a, v, r)
            }
            Op::SetIndex => {
                let l = self.unique_in(a, m)?;
                let i = self.int_in(b)?;
                let r = self.list_refs(l)?;
                let v = self.rd(c)?;
                list::set(&mut self.store, heap, l, i, v)?;
                if r { self.wt(c, 0) } else { Ok(()) }
            }
            Op::NewDict => {
                if b & !(DICT_KEY_REFS | DICT_VAL_REFS) != 0 {
                    return Err(Fault::corrupt("bad NewDict flags"));
                }
                let d = dict::new(
                    &mut self.store,
                    heap,
                    b & DICT_KEY_REFS != 0,
                    b & DICT_VAL_REFS != 0,
                )?;
                self.put(a, d, true)
            }
            Op::GetKey => {
                let (d, k) = (self.rd(b)?, self.rd(c)?);
                let (found, words) = dict::get(&self.store, heap, d, k)?;
                m.words(words);
                let v = found.ok_or(FaultKind::MissingKey)?;
                let (_, vr) = dict::flags(&self.store, heap, d)?;
                let v = self.dup_out(v, vr)?;
                self.put(a, v, vr)
            }
            Op::TakeKey => {
                let d = self.unique_in(b, m)?;
                let k = self.rd(c)?;
                let (found, words) = dict::take(&mut self.store, heap, d, k)?;
                m.words(words);
                let v = found.ok_or(FaultKind::MissingKey)?;
                let (_, vr) = dict::flags(&self.store, heap, d)?;
                self.put(a, v, vr)
            }
            Op::SetKey => {
                let d = self.unique_in(a, m)?;
                let (kr, vr) = dict::flags(&self.store, heap, d)?;
                let (k, v) = (self.rd(b)?, self.rd(c)?);
                // The dict takes a reference of its own: the key slot is borrowed.
                let k = self.dup_out(k, kr)?;
                let words = match dict::set(&mut self.store, heap, d, k, v) {
                    Ok((_, words)) => words,
                    Err(e) => {
                        // Short of `Corrupt` (the instance fails), `set` fails before it stores the
                        // key: the reference taken for it goes back.
                        if kr && e != FaultKind::Corrupt {
                            release(&mut self.store, heap, k)?;
                        }
                        return Err(e.into());
                    }
                };
                m.words(words);
                if vr { self.wt(c, 0) } else { Ok(()) }
            }
            _ => Err(Fault::corrupt("not a heap instruction")),
        }
    }
}
