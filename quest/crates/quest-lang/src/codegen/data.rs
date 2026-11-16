//! Data: constructions (structs, events, enum variants, `Some` / `None`), list and dict literals, fields,
//! indexing, slices, `in`, the built-in methods of `list`, `dict` and `str`, and an `Option`'s `else`
//! (its fallback: `errors.rs`).
//!
//! **Values** (design D4). A struct or event is an `Obj` of tag 0, its fields in declaration order; a
//! variant with fields an `Obj` whose tag is the variant's index; a variant without fields the immediate
//! `(index << 1) | 1`. `Option[T]` is the enum `None` (0) / `Some(value)` (1), as the VM's builtins build it.
//! An object is built in a temporary (`NewObj`, then each field moved in by `SetField`) and moved into its
//! destination last, so the fields may read what the destination holds.
//!
//! A mutating method (`append`, `pop`, `insert`, `remove`) works on its receiver's place: `place.rs`.

use super::{Emit, Val};
use crate::ast::Span;
use crate::hir::{Arg, DefId, Expr, ExprKind};
use crate::infer::BuiltinMethod;
use crate::ty::{Ty, TyId};
use quest_vm::asm::SlotRef;
use quest_vm::{Builtin, Op};

use super::expr::BArg;

/// `None`: the variant 0 without fields, as an immediate.
pub(super) const NONE_IMMEDIATE: i64 = 1;
/// The variant tag of `Some`.
const SOME_TAG: u16 = 1;
/// `NewList`'s flag: the elements are heap kinds.
const LIST_REFS: u16 = 1;
/// `NewDict`'s flags: the keys, the values are heap kinds.
const DICT_KEY_REFS: u16 = 1;
const DICT_VAL_REFS: u16 = 2;
/// A slice's missing upper bound: past any list (the VM clamps it to the length).
const SLICE_END: i64 = i64::MAX;

impl Emit<'_, '_, '_> {
    /// The shape of `e`'s type.
    pub(super) fn ty_kind(&self, e: &Expr) -> Ty {
        self.g.typed.types.get(self.ty_of(e))
    }

    /// An immediate into `dst`, which may be a heap-kind word holding an object: it is dropped first
    /// (`LoadSmall` / `LoadConst` do not release what they overwrite).
    pub(super) fn immediate(&mut self, dst: SlotRef, heap: bool, v: i64) {
        if heap {
            self.fb.emit(Op::Drop, dst, 0u16, 0u16);
        }
        self.load_int(dst, v);
    }

    /// `Name(fields…)` of a struct or event (`variant` `None`), or `Enum::Variant(fields…)`.
    pub(super) fn construct(
        &mut self,
        e: &Expr,
        d: DefId,
        variant: Option<u32>,
        args: &[Arg],
        dst: SlotRef,
    ) {
        let shapes = self.g.shapes(d);
        let Some(shape) = shapes.get(variant.unwrap_or(0) as usize) else {
            return self.not_yet("this construction", e.span);
        };
        let tag = variant.unwrap_or(0);
        if variant.is_some() && shape.tys.is_empty() {
            return self.immediate(dst, true, (i64::from(tag) << 1) | 1);
        }
        let (Ok(fields), Ok(tag)) = (u8::try_from(shape.tys.len()), u16::try_from(tag)) else {
            return self.not_yet("this construction", e.span);
        };
        let ref_bits = self.g.ref_bits(shape);
        let shape_id = self.fb.shape(fields, ref_bits);
        let obj = self.temp(true);
        let obj_s = self.word(obj);
        self.fb.emit(Op::NewObj, obj_s, shape_id, tag);
        // Positional arguments fill the fields in order; a named one names its field.
        let mut positional = 0usize;
        for a in args {
            let index = match &a.name {
                Some(n) => shape.names.iter().position(|f| f == n),
                None => {
                    positional += 1;
                    Some(positional - 1)
                }
            };
            let Some(index) = index else {
                return self.not_yet("this construction", e.span);
            };
            let heap = shape.tys.get(index).is_some_and(|t| self.heap_ty(*t));
            self.set_field_from(obj_s, index, &a.value, heap);
        }
        self.fb.emit(Op::Move, dst, obj_s, 0u16);
        self.free(obj);
    }

    /// Evaluates `value` and moves it into field `index` of the object in `obj`.
    pub(super) fn set_field_from(&mut self, obj: SlotRef, index: usize, value: &Expr, heap: bool) {
        let t = self.temp(heap);
        let w = self.word(t);
        self.eval_into(value, w);
        self.fb.emit(
            Op::SetField,
            obj,
            u16::try_from(index).unwrap_or(u16::MAX),
            w,
        );
        self.free(t);
    }

    /// `Some(x)`.
    pub(super) fn some(&mut self, x: &Expr, dst: SlotRef) {
        let heap = self.heap_expr(x);
        let shape = self.fb.shape(1, u32::from(heap));
        let obj = self.temp(true);
        let obj_s = self.word(obj);
        self.fb.emit(Op::NewObj, obj_s, shape, SOME_TAG);
        self.set_field_from(obj_s, 0, x, heap);
        self.fb.emit(Op::Move, dst, obj_s, 0u16);
        self.free(obj);
    }

    /// `None`.
    pub(super) fn none(&mut self, dst: SlotRef) {
        self.immediate(dst, true, NONE_IMMEDIATE);
    }

    /// `[a, b, …]`: a new list, each element pushed in order.
    pub(super) fn list_literal(&mut self, e: &Expr, elems: &[Expr], dst: SlotRef) {
        let Ty::List(elem) = self.ty_kind(e) else {
            return self.not_yet("this list", e.span);
        };
        let heap = self.heap_ty(elem);
        // [the list, the element being pushed]
        let run = self.slots.run(&[true, heap]);
        self.live.push(run);
        if heap {
            self.live.push(run + 1);
        }
        let (list, item) = (self.word(run), self.word(run + 1));
        self.fb
            .emit(Op::NewList, list, if heap { LIST_REFS } else { 0 }, 0u16);
        for x in elems {
            self.eval_into(x, item);
            // Moves the element in (its word zeroed for a heap kind); writes no result.
            self.fb
                .emit(Op::CallBuiltin, Builtin::ListPush as u16, list, list);
        }
        self.fb.emit(Op::Move, dst, list, 0u16);
        self.free(run + 1);
        self.free(run);
    }

    /// `{k: v, …}`: a new dict, each entry set in order.
    pub(super) fn dict_literal(&mut self, e: &Expr, pairs: &[(Expr, Expr)], dst: SlotRef) {
        let Ty::Dict(k, v) = self.ty_kind(e) else {
            return self.not_yet("this dict", e.span);
        };
        let (kh, vh) = (self.heap_ty(k), self.heap_ty(v));
        let flags = if kh { DICT_KEY_REFS } else { 0 } | if vh { DICT_VAL_REFS } else { 0 };
        let d = self.temp(true);
        let ds = self.word(d);
        self.fb.emit(Op::NewDict, ds, flags, 0u16);
        for (key, value) in pairs {
            let kt = self.temp(kh);
            let ks = self.word(kt);
            self.eval_into(key, ks);
            let vt = self.temp(vh);
            let vs = self.word(vt);
            self.eval_into(value, vs);
            // The key is borrowed, the value moved in.
            self.fb.emit(Op::SetKey, ds, ks, vs);
            self.free(vt);
            if kh {
                self.fb.emit(Op::Drop, ks, 0u16, 0u16);
            }
            self.free(kt);
        }
        self.fb.emit(Op::Move, dst, ds, 0u16);
        self.free(d);
    }

    /// The index of field `name` of a struct or event type, `None` if it is not one.
    pub(super) fn field_index(&self, ty: Ty, name: &str) -> Option<u16> {
        let Ty::Adt(d) = ty else {
            return None;
        };
        if self.g.is_enum(d) {
            return None;
        }
        let i = self
            .g
            .shapes(d)
            .first()?
            .names
            .iter()
            .position(|n| n == name)?;
        u16::try_from(i).ok()
    }

    /// `x.name`.
    pub(super) fn field(&mut self, e: &Expr, x: &Expr, name: &str, dst: SlotRef) {
        let Some(i) = self.field_index(self.ty_kind(x), name) else {
            return self.not_yet("a field of this value (through a reference?)", e.span);
        };
        let v = self.val(x);
        self.fb.emit(Op::GetField, dst, v.slot, i);
        self.done(v);
    }

    /// `x[i]`: a list's element or a dict's value.
    pub(super) fn index(&mut self, e: &Expr, x: &Expr, i: &Expr, dst: SlotRef) {
        let op = match self.ty_kind(x) {
            Ty::List(_) => Op::GetIndex,
            Ty::Dict(_, _) => Op::GetKey,
            _ => return self.not_yet("indexing through a reference", e.span),
        };
        let vs = self.vals(&[x, i]);
        self.fb.emit(op, dst, vs[0].slot, vs[1].slot);
        self.done_all(vs);
    }

    /// `x[a:b]`; a missing bound is 0 or the end.
    pub(super) fn slice(&mut self, e: &Expr, x: &Expr, bounds: [Option<&Expr>; 2], dst: SlotRef) {
        if !matches!(self.ty_kind(x), Ty::List(_)) {
            return self.not_yet("a slice through a reference", e.span);
        }
        let lo = bounds[0].map_or(BArg::Imm(0), BArg::Expr);
        let hi = bounds[1].map_or(BArg::Imm(SLICE_END), BArg::Expr);
        let ty = self.ty_of(e);
        self.builtin(Builtin::ListSlice, &[BArg::Expr(x), lo, hi], ty, Some(dst));
    }

    /// `x in c` (`negate`: `not in`): an element of a list, a key of a dict. `x` is evaluated first.
    pub(super) fn contains(&mut self, at: Span, x: &Expr, c: &Expr, negate: bool, dst: SlotRef) {
        let b = match self.ty_kind(c) {
            Ty::List(_) => Builtin::ListContains,
            Ty::Dict(_, _) => Builtin::DictContains,
            _ => return self.not_yet("`in` through a reference", at),
        };
        let vs = self.vals(&[x, c]);
        let args = [
            BArg::Word(vs[1].slot, vs[1].heap),
            BArg::Word(vs[0].slot, vs[0].heap),
        ];
        self.builtin(b, &args, TyId::BOOL, Some(dst));
        if negate {
            self.fb.emit(Op::Not, dst, dst, 0u16);
        }
        self.done_all(vs);
    }

    /// Gives back values in reverse order.
    pub(super) fn done_all(&mut self, vs: Vec<Val>) {
        for v in vs.into_iter().rev() {
            self.done(v);
        }
    }

    /// `recv.m(args)` of a built-in method; the result into `dst` (`None`: dropped).
    pub(super) fn builtin_method(
        &mut self,
        e: &Expr,
        m: BuiltinMethod,
        recv: &Expr,
        args: &[Arg],
        dst: Option<SlotRef>,
    ) {
        use BuiltinMethod as M;
        let rty = self.ty_kind(recv);
        let (b, mutates) = match (m, rty) {
            (_, Ty::Ref(_)) => return self.not_yet("a method call through a reference", e.span),
            (M::Len, Ty::List(_)) => {
                let v = self.val(recv);
                match dst {
                    Some(d) => {
                        self.fb.emit(Op::ListLen, d, v.slot, 0u16);
                    }
                    None => {
                        let t = self.temp(false);
                        let w = self.word(t);
                        self.fb.emit(Op::ListLen, w, v.slot, 0u16);
                        self.free(t);
                    }
                }
                self.done(v);
                return;
            }
            (M::Len, Ty::Dict(_, _)) => (Builtin::DictLen, false),
            (M::Len, _) => (Builtin::StrLen, false),
            (M::Append, _) => (Builtin::ListPush, true),
            (M::Pop, _) => (Builtin::ListPop, true),
            (M::Insert, _) => (Builtin::ListInsert, true),
            (M::Remove, Ty::Dict(_, _)) => (Builtin::DictRemove, true),
            (M::Remove, _) => (Builtin::ListRemove, true),
            (M::Get, _) => (Builtin::DictGet, false),
            (M::Keys, _) => (Builtin::DictKeys, false),
            (M::Values, _) => (Builtin::DictValues, false),
            (M::Split, _) => (Builtin::StrSplit, false),
            (M::StartsWith, _) => (Builtin::StrStartsWith, false),
            (M::EndsWith, _) => (Builtin::StrEndsWith, false),
            (M::Contains, _) => (Builtin::StrContains, false),
            (M::Trim, _) => (Builtin::StrTrim, false),
            (M::ToUpper, _) => (Builtin::StrUpper, false),
            (M::ToLower, _) => (Builtin::StrLower, false),
            (M::Items, _) => return self.not_yet("`items()` outside a `for` loop", e.span),
        };
        let result = self.ty_of(e);
        let rest: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        if mutates {
            self.mutate(b, recv, &rest, result, dst);
        } else {
            let mut bargs = vec![BArg::Expr(recv)];
            bargs.extend(rest.iter().map(|x| BArg::Expr(x)));
            self.builtin(b, &bargs, result, dst);
        }
    }

    /// `x else fallback` on an `Option`: the value of `Some`, else the fallback (a value, a way out, or an
    /// `else err:` block whose `err` is `()`).
    pub(super) fn option_else(&mut self, x: &Expr, fallback: &Expr, dst: SlotRef) {
        let o = self.val_copy(x);
        let tag = self.temp(false);
        let tag_s = self.word(tag);
        let (none, end) = (self.fb.label(), self.fb.label());
        self.fb.emit(Op::GetTag, tag_s, o.slot, 0u16);
        self.fb.jump(Op::JmpIfNot, tag_s, none);
        self.free(tag);
        self.fb.emit(Op::GetField, dst, o.slot, 0u16);
        self.fb.jump(Op::Jmp, 0u16, end);
        self.fb.bind(none);
        self.fallback(fallback, None, dst);
        self.fb.bind(end);
        self.done(o);
    }

    /// Whether `e` is an `Option` (for `else`).
    pub(super) fn is_option(&self, e: &Expr) -> bool {
        matches!(self.ty_kind(e), Ty::Option(_))
    }

    /// A construction callee: the struct or event, or the enum and variant.
    pub(super) fn constructor(&self, callee: &Expr) -> Option<(DefId, Option<u32>)> {
        match &callee.kind {
            ExprKind::Variant(d, i) => Some((*d, Some(*i))),
            ExprKind::Def(d) => match self.def_kind(*d) {
                Some(crate::hir::DefKind::Struct(_) | crate::hir::DefKind::Event(_)) => {
                    Some((*d, None))
                }
                _ => None,
            },
            _ => None,
        }
    }
}
