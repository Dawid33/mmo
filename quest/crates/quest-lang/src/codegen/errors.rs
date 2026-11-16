//! Errors (spec §5.7, design D5, codegen carry-over 9): `raise`, `?`, the `else` fallbacks, the program's
//! `Error` enum, and the fallible result `match` tests (`matching.rs`).
//!
//! **Values.** Every error name of the program is a variant of one enum named `Error`, variants sorted by
//! name (`Typed::errors`), declared before any other type so the scheduler's lookup by name finds it even
//! when the program has an `enum Error` of its own. A data-less error is the immediate `(tag << 1) | 1`,
//! one with data an `Obj` of that tag, its fields in the name's data order. A function's error is its
//! frame word 2 (0: none); a function that cannot fail never writes it.
//!
//! **Fallible results.** A fallible expression leaves an [`Outcome`]: the word holding its value and the
//! word holding its error (0 when it succeeded) — a callee's words 1 and 2 right after its `Call`. The
//! outcome is consumed at once, before anything else can call that callee:
//! - `x?`: `JmpIfNot err, ok`; `Move own.err, err`; `TracePush 0` at the `?`; drops; `Ret`. On an
//!   `Option`, `None` returns `None` (no trace: it is not an error).
//! - `x else v`, `x else return | break | continue | raise …`: the error is dropped, then the fallback is
//!   evaluated into the destination (or leaves).
//! - `x else err:` + block: the error moves into `err`'s word (dropped with the block); the block's last
//!   expression statement is its value, unless the block leaves.
//! - `match x:`: the value and the error move into temporaries the arms test.
//!
//! **`raise`** builds the error in its own word 2, then `TracePush 1` (a new trace) at the `raise`, drops,
//! `Ret`. The trace's entries are the `pc`s of the `TracePush`es, so each one is placed at its own site.

use super::Emit;
use super::Gen;
use super::Val;
use super::data::NONE_IMMEDIATE;
use crate::ast::{Receiver, Span};
use crate::hir::{Arg, DefKind, Expr, ExprKind, LocalId, Stmt, StmtKind};
use crate::infer::Method;
use crate::ty::{Ty, TyId};
use quest_vm::Op;
use quest_vm::ProgramBuilder;
use quest_vm::asm::SlotRef;
use quest_vm::types::TypeDesc;

/// The name of the program's error enum, which the scheduler looks up (design D9).
const ERROR_ENUM: &str = "Error";
/// `TracePush`'s operand: start a new trace (a `raise`), or append to it (a `?`).
const TRACE_NEW: u16 = 1;
const TRACE_APPEND: u16 = 0;

/// A fallible result where it was produced: its value and its error (0: none).
#[derive(Debug, Clone, Copy)]
pub(super) struct Outcome {
    pub(super) value: SlotRef,
    /// Whether the value is a heap kind.
    pub(super) heap: bool,
    pub(super) err: SlotRef,
}

impl Gen<'_> {
    /// Declares the `Error` enum (when the program has error names), before any other type.
    pub(super) fn declare_errors(&mut self, pb: &mut ProgramBuilder) {
        let typed = self.typed;
        if typed.errors.is_empty() {
            return;
        }
        let id = pb.reserve_ty();
        self.error_ty = Some(id);
        let mut variants = Vec::with_capacity(typed.errors.len());
        for (name, fields) in &typed.errors {
            let mut lowered = Vec::with_capacity(fields.len());
            for (field, ty) in fields {
                // A generic host field no use fixed has no type: it never holds a value.
                let ty = if typed.types.get(*ty) == Ty::Error {
                    TyId::UNIT
                } else {
                    *ty
                };
                let tid = self.type_id(pb, ty, "", Span::default()).unwrap_or(0);
                lowered.push((field.clone(), tid));
            }
            variants.push((name.clone(), lowered));
        }
        pb.define_ty(
            id,
            TypeDesc::Enum {
                name: ERROR_ENUM.to_string(),
                variants,
            },
        );
    }

    /// The tag of error `name`: its index among the program's error names.
    pub(super) fn error_tag(&self, name: &str) -> Option<u32> {
        let i = self.typed.errors.keys().position(|n| n == name)?;
        u32::try_from(i).ok()
    }
}

impl Emit<'_, '_, '_> {
    /// Calls the fallible `x` (a script function or method), leaving its result in the callee's words;
    /// `None` when it was reported.
    pub(super) fn outcome(&mut self, x: &Expr) -> Option<Outcome> {
        let ExprKind::Call(callee, args) = &x.kind else {
            self.not_yet("this fallible expression", x.span);
            return None;
        };
        let mut exprs: Vec<&Expr> = Vec::with_capacity(args.len() + 1);
        let d = match &callee.kind {
            ExprKind::Def(d) if matches!(self.def_kind(*d), Some(DefKind::Fn(_))) => *d,
            ExprKind::Field(recv, _) => match self.g.typed.methods.get(&x.id) {
                Some(Method::Script(m)) => {
                    let by_ref = self
                        .g
                        .typed
                        .sigs
                        .get(m)
                        .is_some_and(|s| matches!(s.receiver, Some((Receiver::Ref, _))));
                    if by_ref || matches!(self.ty_kind(recv), Ty::Ref(_)) {
                        self.not_yet(
                            "a `&self` method call or a call through a reference",
                            x.span,
                        );
                        return None;
                    }
                    exprs.push(recv);
                    *m
                }
                _ => {
                    self.not_yet("this fallible call", x.span);
                    return None;
                }
            },
            _ => {
                self.not_yet("this fallible call", x.span);
                return None;
            }
        };
        exprs.extend(args.iter().map(|a| &a.value));
        let frame = self.call_raw(x, d, &exprs)?;
        let ret = self.g.typed.sigs.get(&d).map_or(TyId::UNIT, |s| s.ret);
        Some(Outcome {
            value: frame.ret(),
            heap: self.heap_ty(ret),
            err: frame.err(),
        })
    }

    /// Moves an outcome's value into `dst`.
    fn take_value(&mut self, o: Outcome, dst: SlotRef) {
        let op = if o.heap { Op::Move } else { Op::Copy };
        self.fb.emit(op, dst, o.value, 0u16);
    }

    /// Moves an outcome into temporaries of its own: (the value, the error).
    pub(super) fn hold_outcome(&mut self, o: Outcome) -> (Val, Val) {
        let v = self.temp(o.heap);
        let vs = self.word(v);
        self.take_value(o, vs);
        let e = self.temp(true);
        let es = self.word(e);
        self.fb.emit(Op::Move, es, o.err, 0u16);
        (
            Val {
                slot: vs,
                heap: o.heap,
                temp: Some(v),
            },
            Val {
                slot: es,
                heap: true,
                temp: Some(e),
            },
        )
    }

    /// `TracePush` at `at`: the error leaves this function from there.
    fn trace(&mut self, at: Span, mode: u16) {
        self.at(at);
        self.fb.emit(Op::TracePush, mode, 0u16, 0u16);
        self.fails = true;
    }

    /// `x?` into `dst` (`e`: the `?`).
    pub(super) fn try_op(&mut self, e: &Expr, x: &Expr, dst: SlotRef) {
        if self.is_option(x) {
            return self.option_try(x, dst);
        }
        let Some(o) = self.outcome(x) else {
            return;
        };
        let ok = self.fb.label();
        self.fb.jump(Op::JmpIfNot, o.err, ok);
        let own = self.fb.err();
        self.fb.emit(Op::Move, own, o.err, 0u16);
        self.trace(e.span, TRACE_APPEND);
        self.ret_now();
        self.fb.bind(ok);
        self.at_line();
        self.take_value(o, dst);
    }

    /// `x?` on an `Option`: the value of `Some`, else `None` returned.
    fn option_try(&mut self, x: &Expr, dst: SlotRef) {
        let o = self.val_copy(x);
        let tag = self.temp(false);
        let tag_s = self.word(tag);
        let some = self.fb.label();
        self.fb.emit(Op::GetTag, tag_s, o.slot, 0u16);
        self.fb.jump(Op::JmpIf, tag_s, some);
        self.free(tag);
        let ret = self.fb.ret();
        self.immediate(ret, true, NONE_IMMEDIATE);
        self.ret_now();
        self.fb.bind(some);
        self.fb.emit(Op::GetField, dst, o.slot, 0u16);
        self.done(o);
    }

    /// `x else fallback` into `dst`.
    pub(super) fn else_op(&mut self, x: &Expr, fallback: &Expr, dst: SlotRef) {
        if self.is_option(x) {
            return self.option_else(x, fallback, dst);
        }
        let Some(o) = self.outcome(x) else {
            return;
        };
        let (ok, end) = (self.fb.label(), self.fb.label());
        self.fb.jump(Op::JmpIfNot, o.err, ok);
        self.fallback(fallback, Some(o.err), dst);
        self.fb.jump(Op::Jmp, 0u16, end);
        self.fb.bind(ok);
        self.at_line();
        self.take_value(o, dst);
        self.fb.bind(end);
    }

    /// An `else`'s fallback, on the failure path, into `dst`: `err` is the word holding the error (`None`
    /// for an `Option`'s `None`), consumed here.
    pub(super) fn fallback(&mut self, fallback: &Expr, err: Option<SlotRef>, dst: SlotRef) {
        if let ExprKind::FallbackBlock(local, body) = &fallback.kind {
            return self.fallback_block(fallback, *local, body, err, dst);
        }
        if let Some(e) = err {
            self.fb.emit(Op::Drop, e, 0u16, 0u16);
        }
        self.eval_into(fallback, dst);
    }

    /// `else err:` + block: `err` bound to the error (`()` for an `Option`), the block's last expression
    /// statement its value unless the block leaves.
    fn fallback_block(
        &mut self,
        fallback: &Expr,
        local: LocalId,
        body: &[Stmt],
        err: Option<SlotRef>,
        dst: SlotRef,
    ) {
        let heap = self.heap_ty(self.g.typed.local(local));
        let i = self.slots.fresh(heap);
        let w = self.word(i);
        match err {
            Some(e) if heap => {
                self.fb.emit(Op::Move, w, e, 0u16);
            }
            Some(e) => {
                self.fb.emit(Op::Drop, e, 0u16, 0u16);
                self.fb.emit(Op::LoadSmall, w, 0u16, 0u16);
            }
            None => self.immediate(w, heap, 0),
        }
        self.locals.insert(local, w);
        let leaves = self.g.typed.types.get(self.ty_of(fallback)) == Ty::Never;
        let (stmts, value) = match body.split_last() {
            Some((last, rest)) if !leaves => match &last.kind {
                StmtKind::Expr(v) => (rest, Some((last, v))),
                _ => (body, None),
            },
            _ => (body, None),
        };
        let line = self.line;
        self.scopes.push(if heap { vec![w] } else { Vec::new() });
        for s in stmts {
            self.stmt(s);
        }
        if let Some((s, v)) = value {
            self.line = (s.span.line, s.span.col);
            self.at(s.span);
            self.eval_into(v, dst);
        }
        self.line = line;
        let scope = self.scopes.pop().unwrap_or_default();
        for s in scope.into_iter().rev() {
            self.fb.emit(Op::Drop, s, 0u16, 0u16);
        }
        self.at_line();
    }

    /// `raise x` at `at`: an error name, with or without data, or a caught error.
    pub(super) fn raise(&mut self, at: Span, x: &Expr) {
        let own = self.fb.err();
        match &x.kind {
            ExprKind::ErrorName(name) => self.error_value(x, name, &[], own),
            ExprKind::Call(callee, args) => match &callee.kind {
                ExprKind::ErrorName(name) => self.error_value(x, name, args, own),
                _ => self.not_yet("this `raise`", x.span),
            },
            _ => self.eval_into(x, own),
        }
        self.trace(at, TRACE_NEW);
        self.ret_now();
    }

    /// The error `name(args)` into `dst`: an immediate without data, else an object of the name's tag.
    fn error_value(&mut self, x: &Expr, name: &str, args: &[Arg], dst: SlotRef) {
        let typed = self.g.typed;
        let (Some(tag), Some(fields)) = (self.g.error_tag(name), typed.errors.get(name)) else {
            return self.not_yet("this error", x.span);
        };
        if args.is_empty() || fields.is_empty() {
            return self.immediate(dst, true, (i64::from(tag) << 1) | 1);
        }
        let (Ok(count), Ok(tag)) = (u8::try_from(fields.len()), u16::try_from(tag)) else {
            return self.not_yet("this error", x.span);
        };
        let ref_bits = fields
            .iter()
            .enumerate()
            .filter(|(_, (_, t))| self.heap_ty(*t))
            .fold(0u32, |bits, (i, _)| {
                bits | 1u32.checked_shl(i as u32).unwrap_or(0)
            });
        let shape = self.fb.shape(count, ref_bits);
        let obj = self.temp(true);
        let obj_s = self.word(obj);
        self.fb.emit(Op::NewObj, obj_s, shape, tag);
        // Positional data fills the fields in order; a named one names its field.
        let mut positional = 0usize;
        for a in args {
            let index = match &a.name {
                Some(n) => fields.iter().position(|(f, _)| f == n),
                None => {
                    positional += 1;
                    Some(positional - 1)
                }
            };
            let Some((index, (_, ty))) = index.and_then(|i| fields.get(i).map(|f| (i, f))) else {
                return self.not_yet("this error", x.span);
            };
            let heap = self.heap_ty(*ty);
            self.set_field_from(obj_s, index, &a.value, heap);
        }
        self.fb.emit(Op::Move, dst, obj_s, 0u16);
        self.free(obj);
    }
}
