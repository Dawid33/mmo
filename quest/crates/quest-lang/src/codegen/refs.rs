//! References (spec §5.3, design D4/D5): `&place`, `*r`, `*r = v`, the auto-dereference of fields,
//! indexing and methods, and `&self` receivers.
//!
//! **A reference is a `RefPath`** (a heap kind): `RefMake` of the place's variable word (a frame word or
//! a top-level variable's word: static, so a reference passed down calls stays valid while the frame it
//! names is live, and references never escape), extended by `RefField` / `RefIndex` / `RefKey` per step
//! (each a NEW path; the index or key is copied into it). `&p` where `p` already goes through a reference
//! `r` extends `r`'s path. Every path built is a temporary released after use.
//!
//! **Reading** through a reference is `RefLoad` (a copy of the target): `*r`, and the receiver of a
//! field, an index, a slice, `in`, a non-mutating method, a `self` method, a `for` loop (read again at
//! each step) or a `match`.
//!
//! **Writing** through a reference is take → mutate → store (design D4): every operand is evaluated
//! first (codegen carry-over 4), then `RefTake` moves the target out into a temporary, the write happens
//! there, and `RefStore` moves it back. `*r = v` is one `RefStore` of `v`'s temporary; `r.f = v` and
//! `r[k] = v` take the container (a path to it) and `SetField` / `SetIndex` / `SetKey` into it — a key store
//! through a path needs the key to exist, a `SetKey` adds it. The source of a `RefStore` is always a
//! temporary, never the reference's root word (carry-over 5).
//!
//! **`&self` methods** compile to a plain `Call` (carry-over 2) whose receiver argument is a reference to
//! the caller's place (`Typed::refs` records it; the same walk as [`Emit::place_of`] lowers it), or the
//! receiver itself when it is a reference already. A `self` method reached through a reference gets a
//! copy of the target.

use super::expr::{CallArg, may_write};
use super::place::{Path, PlaceOf, RefPlace, Step};
use super::{Emit, Val};
use crate::ast::Receiver;
use crate::hir::{DefId, Expr};
use crate::ty::{Ty, TyId};
use quest_vm::asm::SlotRef;
use quest_vm::{Builtin, Op};

/// How many references deep a type is looked through (a reference to a reference is never built).
const DEREF_DEPTH_MAX: usize = 8;

impl Emit<'_, '_, '_> {
    /// Whether `e`'s value is a reference.
    pub(super) fn is_ref(&self, e: &Expr) -> bool {
        matches!(self.ty_kind(e), Ty::Ref(_))
    }

    /// `e`'s type seen through references.
    pub(super) fn target_ty(&self, e: &Expr) -> TyId {
        let mut ty = self.ty_of(e);
        for _ in 0..DEREF_DEPTH_MAX {
            match self.g.typed.types.get(ty) {
                Ty::Ref(inner) => ty = inner,
                _ => break,
            }
        }
        ty
    }

    /// The shape of `e`'s type seen through references.
    pub(super) fn target_kind(&self, e: &Expr) -> Ty {
        self.g.typed.types.get(self.target_ty(e))
    }

    /// `e`'s value, or what it refers to when it is a reference (a copy, in a temporary).
    pub(super) fn val_target(&mut self, e: &Expr) -> Val {
        if !self.is_ref(e) {
            return self.val(e);
        }
        let heap = self.heap_ty(self.target_ty(e));
        let t = self.temp(heap);
        let slot = self.word(t);
        self.deref_into(e, slot);
        Val {
            slot,
            heap,
            temp: Some(t),
        }
    }

    /// [`Self::vals`], each reference read through.
    pub(super) fn target_vals(&mut self, es: &[&Expr]) -> Vec<Val> {
        if !es.iter().any(|e| self.is_ref(e)) {
            return self.vals(es);
        }
        let mut out = Vec::with_capacity(es.len());
        for (i, e) in es.iter().enumerate() {
            let later_writes = es
                .get(i + 1..)
                .unwrap_or_default()
                .iter()
                .any(|x| may_write(x));
            out.push(if self.is_ref(e) {
                self.val_target(e)
            } else if later_writes {
                self.val_copy(e)
            } else {
                self.val(e)
            });
        }
        out
    }

    /// `e`'s value into `dst`, or what it refers to when it is a reference.
    pub(super) fn eval_target_into(&mut self, e: &Expr, dst: SlotRef) {
        if self.is_ref(e) {
            self.deref_into(e, dst);
        } else {
            self.eval_into(e, dst);
        }
    }

    /// `*r` into `dst`: a copy of what the reference `r` refers to.
    pub(super) fn deref_into(&mut self, r: &Expr, dst: SlotRef) {
        let v = self.val(r);
        self.fb.emit(Op::RefLoad, dst, v.slot, 0u16);
        self.done(v);
    }

    /// `&x` into `dst`.
    pub(super) fn ref_into(&mut self, x: &Expr, dst: SlotRef) {
        let Some(v) = self.ref_to(x) else {
            return;
        };
        match v.temp {
            Some(t) => {
                self.fb.emit(Op::Move, dst, v.slot, 0u16);
                self.free(t);
            }
            None => {
                if dst != v.slot {
                    self.fb.emit(Op::Dup, dst, v.slot, 0u16);
                }
            }
        }
    }

    /// A reference to the place `x` (`None`: not a place, reported). A reference expression is its own
    /// place's reference (`&r` is `r`).
    pub(super) fn ref_to(&mut self, x: &Expr) -> Option<Val> {
        match self.place_of(x) {
            PlaceOf::Path(p) => Some(self.ref_from_path(&p)),
            PlaceOf::ThroughRef(rp) => Some(self.ref_through(&rp, rp.steps.len())),
            PlaceOf::Value => {
                self.not_yet("a reference to this expression", x.span);
                None
            }
        }
    }

    /// A new path from the variable of `p` through its steps.
    fn ref_from_path(&mut self, p: &Path<'_>) -> Val {
        let t = self.temp(true);
        let slot = self.word(t);
        self.fb
            .emit(Op::RefMake, slot, p.root, u16::from(p.root_heap));
        let root = Val {
            slot,
            heap: true,
            temp: Some(t),
        };
        self.extend_ref(root, &p.steps)
    }

    /// The reference to `rp`'s base extended by its first `upto` steps: the base's own word when there
    /// are none and it is a variable.
    pub(super) fn ref_through(&mut self, rp: &RefPlace<'_>, upto: usize) -> Val {
        let base = self.val(rp.base);
        let steps = rp.steps.get(..upto).unwrap_or(&rp.steps);
        self.extend_ref(base, steps)
    }

    /// Extends the path in `cur` by `steps`, each index or key evaluated as its step is added; every
    /// intermediate path is given back.
    fn extend_ref(&mut self, mut cur: Val, steps: &[(Step, Option<&Expr>)]) -> Val {
        for (step, x) in steps {
            let t = self.temp(true);
            let w = self.word(t);
            match (step, x) {
                (Step::Field(i), _) => {
                    self.fb.emit(Op::RefField, w, cur.slot, *i);
                }
                (Step::Index | Step::Key, Some(x)) => {
                    let k = self.val(x);
                    let op = if matches!(step, Step::Index) {
                        Op::RefIndex
                    } else {
                        Op::RefKey
                    };
                    self.fb.emit(op, w, cur.slot, k.slot);
                    self.done(k);
                }
                // Never built: an index or key step has its operand.
                (Step::Index | Step::Key, None) => {}
            }
            self.done(cur);
            cur = Val {
                slot: w,
                heap: true,
                temp: Some(t),
            };
        }
        cur
    }

    /// `place = value` for a place reached through a reference.
    pub(super) fn assign_through(&mut self, rp: &RefPlace<'_>, value: &Expr) {
        let Some(&(last, key)) = rp.steps.last() else {
            // `*r = value`: stored from a temporary of its own (never from the reference's root word).
            let r = self.val(rp.base);
            let v = self.val_copy(value);
            self.fb.emit(Op::RefStore, r.slot, v.slot, 0u16);
            if let Some(t) = v.temp {
                self.free(t);
            }
            self.done(r);
            return;
        };
        // The container's path, the last index or key, the value; then take, set, store back.
        let r = self.ref_through(rp, rp.steps.len() - 1);
        let k = key.map(|x| {
            if may_write(value) {
                self.val_copy(x)
            } else {
                self.val(x)
            }
        });
        let v = self.val_copy(value);
        let t = self.temp(true);
        let w = self.word(t);
        self.fb.emit(Op::RefTake, w, r.slot, 0u16);
        let key_slot = k.as_ref().map_or(SlotRef::Invalid, |k| k.slot);
        // Moves the value in: its temporary holds nothing afterwards.
        self.emit_set(last, w, key_slot, v.slot);
        if let Some(vt) = v.temp {
            self.free(vt);
        }
        self.fb.emit(Op::RefStore, r.slot, w, 0u16);
        self.free(t);
        if let Some(k) = k {
            self.done(k);
        }
        self.done(r);
    }

    /// A mutating builtin on a container reached through a reference: the path to it, the arguments,
    /// then `RefTake` into the builtin's first argument word, the call, `RefStore` back.
    pub(super) fn mutate_through(
        &mut self,
        b: Builtin,
        rp: &RefPlace<'_>,
        args: &[&Expr],
        result: TyId,
        dst: Option<SlotRef>,
    ) {
        let r = self.ref_through(rp, rp.steps.len());
        let (run, kinds) = self.mutate_args(args);
        let base = self.word(run);
        self.fb.emit(Op::RefTake, base, r.slot, 0u16);
        let (scratch, heap) = self.mutate_call(b, run, result);
        self.fb.emit(Op::RefStore, r.slot, base, 0u16);
        self.drop_run(run, &kinds);
        self.mutate_result(scratch, heap, dst);
        self.done(r);
    }

    /// How the receiver `recv` of the script method `m` is passed: a reference to its place for
    /// `&self` (the receiver itself when it is a reference), a copy of the target for `self` through a
    /// reference, else its value.
    pub(super) fn receiver_arg<'e>(&self, m: DefId, recv: &'e Expr) -> CallArg<'e> {
        let by_ref = self
            .g
            .typed
            .sigs
            .get(&m)
            .is_some_and(|s| matches!(s.receiver, Some((Receiver::Ref, _))));
        match (by_ref, self.is_ref(recv)) {
            (true, _) => CallArg::Ref(recv),
            (false, true) => CallArg::Target(recv),
            (false, false) => CallArg::Value(recv),
        }
    }
}
