//! Expressions: literals, variables, operators, f-strings and calls (of script functions and of the
//! prelude).
//!
//! [`Emit::eval_into`] evaluates an expression into a destination word, which then owns the value (what the
//! word held is released by the writing instruction). [`Emit::val`] gives a word to read the value
//! from: the variable's own word when the expression is one, else a temporary that [`Emit::done`] gives
//! back (dropping a heap value). [`Emit::exec`] evaluates for the effect only.

use super::{Emit, Val};
use crate::ast::{BinOp, Span, UnOp};
use crate::hir::{Arg, DefId, DefKind, Expr, ExprKind, FStrPart, Prelude};
use crate::layout::each_expr;
use crate::ty::{Ty, TyId};
use quest_vm::asm::SlotRef;
use quest_vm::{Builtin, Op};

/// The fault message of a failed `assert`.
const ASSERT_MESSAGE: &str = "assertion failed";
/// The fault message of a failed `assert_eq`.
const ASSERT_EQ_MESSAGE: &str = "assert_eq: the values differ";

/// An argument of a builtin call.
enum BArg<'e> {
    Expr(&'e Expr),
    /// An immediate int (`Fmt`'s type id).
    Imm(i64),
    /// A string constant (`Assert`'s message).
    Str(&'static str),
    /// A value already in a word (borrowed).
    Word(SlotRef, bool),
}

/// Whether evaluating `e` may run code that writes a variable (a call).
fn may_write(e: &Expr) -> bool {
    let mut found = false;
    each_expr(e, &mut |x| {
        if matches!(x.kind, ExprKind::Call(_, _) | ExprKind::Await(_)) {
            found = true;
        }
    });
    found
}

/// A literal: written straight into its destination, at any time.
fn is_const(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Unit
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
    )
}

/// The bits of a float constant, NaN canonical (design D4).
fn float_bits(f: f64) -> u64 {
    if f.is_nan() {
        0x7ff8_0000_0000_0000
    } else {
        f.to_bits()
    }
}

impl Emit<'_, '_, '_> {
    /// A word holding `e`'s value.
    pub(super) fn val(&mut self, e: &Expr) -> Val {
        if let Some(slot) = self.variable(e) {
            return Val {
                slot,
                heap: self.heap_expr(e),
                temp: None,
            };
        }
        self.val_copy(e)
    }

    /// `e`'s value in a temporary of its own.
    fn val_copy(&mut self, e: &Expr) -> Val {
        let heap = self.heap_expr(e);
        let t = self.temp(heap);
        let slot = self.word(t);
        self.eval_into(e, slot);
        Val {
            slot,
            heap,
            temp: Some(t),
        }
    }

    /// The values of operands evaluated left to right: a variable is read from its word, unless an
    /// operand to its right may write it.
    fn vals(&mut self, es: &[&Expr]) -> Vec<Val> {
        let mut out = Vec::with_capacity(es.len());
        for (i, e) in es.iter().enumerate() {
            let later_writes = es
                .get(i + 1..)
                .unwrap_or_default()
                .iter()
                .any(|x| may_write(x));
            out.push(if later_writes {
                self.val_copy(e)
            } else {
                self.val(e)
            });
        }
        out
    }

    /// Gives back a value from [`Self::val`]: a temporary is dropped (a heap kind) and freed.
    pub(super) fn done(&mut self, v: Val) {
        if let Some(t) = v.temp {
            if v.heap {
                self.fb.emit(Op::Drop, v.slot, 0u16, 0u16);
            }
            self.free(t);
        }
    }

    /// Copies a value into `dst`, which owns the copy.
    fn copy(&mut self, dst: SlotRef, src: SlotRef, heap: bool) {
        if dst != src {
            let op = if heap { Op::Dup } else { Op::Copy };
            self.fb.emit(op, dst, src, 0u16);
        }
    }

    /// Evaluates `e` for its effect; a value it produces is dropped.
    pub(super) fn exec(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Call(callee, args) => self.call(e, callee, args, None),
            ExprKind::Return(x) => self.ret(x.as_deref()),
            ExprKind::Break => self.leave_loop(true),
            ExprKind::Continue => self.leave_loop(false),
            _ => {
                let v = self.val_copy(e);
                self.done(v);
            }
        }
    }

    /// Evaluates `e` into `dst`.
    pub(super) fn eval_into(&mut self, e: &Expr, dst: SlotRef) {
        match &e.kind {
            ExprKind::Unit => {
                self.fb.emit(Op::LoadSmall, dst, 0u16, 0u16);
            }
            ExprKind::Bool(b) => {
                self.fb.emit(Op::LoadSmall, dst, u16::from(*b), 0u16);
            }
            ExprKind::Int(n) => self.load_int(dst, *n),
            ExprKind::Float(f) => {
                let c = self.fb.program().const_u64(float_bits(*f));
                self.fb.emit_t(Op::LoadConst, dst, c);
            }
            ExprKind::Str(s) => self.load_str(dst, s),
            ExprKind::FStr(parts) => self.fstring(parts, dst),
            ExprKind::Local(_) | ExprKind::Def(_) => match self.variable(e) {
                Some(src) => {
                    let heap = self.heap_expr(e);
                    self.copy(dst, src, heap);
                }
                None => self.not_yet("this item as a value", e.span),
            },
            ExprKind::Unary(UnOp::Neg, x) => {
                let op = if self.g.typed.types.get(self.ty_of(x)) == Ty::Float {
                    Op::NegF
                } else {
                    Op::NegI
                };
                let v = self.val(x);
                self.fb.emit(op, dst, v.slot, 0u16);
                self.done(v);
            }
            ExprKind::Unary(UnOp::Not, x) => {
                let v = self.val(x);
                self.fb.emit(Op::Not, dst, v.slot, 0u16);
                self.done(v);
            }
            ExprKind::Binary(op, l, r) => self.binary(e.span, *op, l, r, dst),
            ExprKind::Call(callee, args) => self.call(e, callee, args, Some(dst)),
            ExprKind::Return(x) => self.ret(x.as_deref()),
            ExprKind::Break => self.leave_loop(true),
            ExprKind::Continue => self.leave_loop(false),
            ExprKind::Unary(UnOp::Ref | UnOp::Deref, _) => self.not_yet("a reference", e.span),
            ExprKind::List(_) | ExprKind::Dict(_) => self.not_yet("a list or dict literal", e.span),
            ExprKind::Field(_, _) | ExprKind::Index(_, _) | ExprKind::Slice(_, _, _) => {
                self.not_yet("a field, an element or a slice", e.span)
            }
            ExprKind::Variant(_, _) => self.not_yet("an enum variant", e.span),
            ExprKind::Prelude(_) => self.not_yet("`None`", e.span),
            ExprKind::Await(_) => self.not_yet("`await`", e.span),
            ExprKind::Try(_)
            | ExprKind::Else(_, _)
            | ExprKind::Raise(_)
            | ExprKind::ErrorName(_)
            | ExprKind::FallbackBlock(_, _) => self.not_yet("error handling", e.span),
            // Never met: the checker reported it, or it is not a value (and was reported).
            ExprKind::Error | ExprKind::Host(_) => self.not_yet("this expression", e.span),
        }
    }

    fn load_int(&mut self, dst: SlotRef, n: i64) {
        match i16::try_from(n) {
            Ok(small) => {
                self.fb.emit(Op::LoadSmall, dst, small as u16, 0u16);
            }
            Err(_) => {
                let c = self.fb.program().const_u64(n as u64);
                self.fb.emit_t(Op::LoadConst, dst, c);
            }
        }
    }

    fn load_str(&mut self, dst: SlotRef, s: &str) {
        let i = self.fb.program().string(s);
        self.fb.emit_t(Op::LoadStr, dst, i);
    }

    fn binary(&mut self, at: Span, op: BinOp, l: &Expr, r: &Expr, dst: SlotRef) {
        match op {
            BinOp::And | BinOp::Or => {
                // Into a temporary: `dst` may be a variable the right side reads.
                let t = self.temp(false);
                let ts = self.word(t);
                self.eval_into(l, ts);
                let end = self.fb.label();
                let jump = if op == BinOp::And {
                    Op::JmpIfNot
                } else {
                    Op::JmpIf
                };
                self.fb.jump(jump, ts, end);
                self.eval_into(r, ts);
                self.fb.bind(end);
                self.fb.emit(Op::Copy, dst, ts, 0u16);
                self.free(t);
                return;
            }
            BinOp::In | BinOp::NotIn => {
                self.not_yet("`in`", at);
                return;
            }
            _ => {}
        }
        let ty = self.g.typed.types.get(self.ty_of(l));
        if ty == Ty::Str && op == BinOp::Add {
            self.builtin(
                Builtin::StrConcat,
                &[BArg::Expr(l), BArg::Expr(r)],
                TyId::STR,
                Some(dst),
            );
            return;
        }
        if ty == Ty::Float && op == BinOp::Rem {
            self.builtin(
                Builtin::ModF,
                &[BArg::Expr(l), BArg::Expr(r)],
                TyId::FLOAT,
                Some(dst),
            );
            return;
        }
        if ty == Ty::Str && matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
            // `StrCmp` gives -1 / 0 / 1, compared with 0: (op, 0 on the left).
            let (code, zero_first) = match op {
                BinOp::Lt => (Op::LtI, false),
                BinOp::Le => (Op::LeI, false),
                BinOp::Gt => (Op::LtI, true),
                _ => (Op::LeI, true),
            };
            let (c, z) = (self.temp(false), self.temp(false));
            let (cs, zs) = (self.word(c), self.word(z));
            self.builtin(
                Builtin::StrCmp,
                &[BArg::Expr(l), BArg::Expr(r)],
                TyId::INT,
                Some(cs),
            );
            self.fb.emit(Op::LoadSmall, zs, 0u16, 0u16);
            let (a, b) = if zero_first { (zs, cs) } else { (cs, zs) };
            self.fb.emit(code, dst, a, b);
            self.free(z);
            self.free(c);
            return;
        }
        // (op, swap the operands, negate the result)
        let int = |op: BinOp| match op {
            BinOp::Add => Some((Op::AddI, false)),
            BinOp::Sub => Some((Op::SubI, false)),
            BinOp::Mul => Some((Op::MulI, false)),
            BinOp::Div => Some((Op::DivI, false)),
            BinOp::Rem => Some((Op::RemI, false)),
            BinOp::Eq => Some((Op::EqI, false)),
            BinOp::Ne => Some((Op::NeI, false)),
            BinOp::Lt => Some((Op::LtI, false)),
            BinOp::Le => Some((Op::LeI, false)),
            BinOp::Gt => Some((Op::LtI, true)),
            BinOp::Ge => Some((Op::LeI, true)),
            _ => None,
        };
        let float = |op: BinOp| match op {
            BinOp::Add => Some((Op::AddF, false)),
            BinOp::Sub => Some((Op::SubF, false)),
            BinOp::Mul => Some((Op::MulF, false)),
            BinOp::Div => Some((Op::DivF, false)),
            BinOp::Eq => Some((Op::EqF, false)),
            BinOp::Ne => Some((Op::NeF, false)),
            BinOp::Lt => Some((Op::LtF, false)),
            BinOp::Le => Some((Op::LeF, false)),
            BinOp::Gt => Some((Op::LtF, true)),
            BinOp::Ge => Some((Op::LeF, true)),
            _ => None,
        };
        let (chosen, negate) = match ty {
            Ty::Int => (int(op), false),
            Ty::Float => (float(op), false),
            // Immediates compared by their bits: `()`, a promise's id.
            Ty::Bool | Ty::Unit | Ty::Promise(_) => match op {
                BinOp::Eq => (Some((Op::EqI, false)), false),
                BinOp::Ne => (Some((Op::NeI, false)), false),
                _ => (None, false),
            },
            // Heap kinds: deep structural equality.
            _ if self.heap_ty(self.ty_of(l)) => match op {
                BinOp::Eq => (Some((Op::Eq, false)), false),
                BinOp::Ne => (Some((Op::Eq, false)), true),
                _ => (None, false),
            },
            _ => (None, false),
        };
        let Some((code, swap)) = chosen else {
            self.not_yet("this operator on these operands", at);
            return;
        };
        let vs = self.vals(&[l, r]);
        let (a, b) = if swap {
            (vs[1].slot, vs[0].slot)
        } else {
            (vs[0].slot, vs[1].slot)
        };
        self.fb.emit(code, dst, a, b);
        if negate {
            self.fb.emit(Op::Not, dst, dst, 0u16);
        }
        for v in vs.into_iter().rev() {
            self.done(v);
        }
    }

    /// `f"…"`: each part as a string (`Fmt` for anything but a `str`), joined by `StrConcat`.
    fn fstring(&mut self, parts: &[FStrPart], dst: SlotRef) {
        match parts {
            [] => self.load_str(dst, ""),
            [one] => self.fpart(one, dst),
            [first, rest @ ..] => {
                // [the text so far, the next part]
                let run = self.slots.run(&[true, true]);
                self.live.extend([run, run + 1]);
                let (acc, part) = (self.word(run), self.word(run + 1));
                self.fpart(first, acc);
                for p in rest {
                    self.fpart(p, part);
                    self.fb
                        .emit(Op::CallBuiltin, Builtin::StrConcat as u16, acc, acc);
                }
                self.fb.emit(Op::Move, dst, acc, 0u16);
                self.fb.emit(Op::Drop, part, 0u16, 0u16);
                self.free(run + 1);
                self.free(run);
            }
        }
    }

    fn fpart(&mut self, p: &FStrPart, dst: SlotRef) {
        match p {
            FStrPart::Text(s) => self.load_str(dst, s),
            FStrPart::Expr(x) => self.text_of(x, dst),
        }
    }

    /// `x` as a string: itself when it is one, else formatted (design D8).
    fn text_of(&mut self, x: &Expr, dst: SlotRef) {
        let ty = self.ty_of(x);
        if self.g.typed.types.get(ty) == Ty::Str {
            self.eval_into(x, dst);
            return;
        }
        let Some(tid) = self.type_id(ty, x.span) else {
            return;
        };
        self.builtin(
            Builtin::Fmt,
            &[BArg::Expr(x), BArg::Imm(i64::from(tid))],
            TyId::STR,
            Some(dst),
        );
    }

    /// `CallBuiltin` with its arguments in a run of consecutive words, borrowed by the builtin and
    /// dropped after it. The result goes to `dst` (`None`: a unit builtin, or the result is dropped).
    fn builtin(&mut self, b: Builtin, args: &[BArg<'_>], result: TyId, dst: Option<SlotRef>) {
        let kinds: Vec<bool> = args
            .iter()
            .map(|a| match a {
                BArg::Expr(e) => self.heap_expr(e),
                BArg::Imm(_) => false,
                BArg::Str(_) => true,
                BArg::Word(_, heap) => *heap,
            })
            .collect();
        // A single variable that lives in this frame is its own run.
        let single = match args {
            [BArg::Expr(e)] => match self.variable(e) {
                Some(s @ SlotRef::Frame { .. }) => Some(s),
                _ => None,
            },
            [BArg::Word(s @ SlotRef::Frame { .. }, _)] => Some(*s),
            _ => None,
        };
        let (base, run) = match single {
            Some(s) => (s, None),
            None => {
                let run = self.slots.run(&kinds);
                for (k, heap) in kinds.iter().enumerate() {
                    if *heap {
                        self.live.push(run + k as u32);
                    }
                }
                for (k, a) in args.iter().enumerate() {
                    let w = self.word(run + k as u32);
                    match a {
                        BArg::Expr(e) => self.eval_into(e, w),
                        BArg::Imm(n) => self.load_int(w, *n),
                        BArg::Str(s) => self.load_str(w, s),
                        BArg::Word(s, heap) => self.copy(w, *s, *heap),
                    }
                }
                (self.word(run), Some(run))
            }
        };
        let unit = matches!(self.g.typed.types.get(result), Ty::Unit | Ty::Never);
        let heap = self.heap_ty(result);
        let (c, scratch) = match dst {
            Some(d) => (d, None),
            None if unit => (base, None),
            None => {
                let t = self.temp(heap);
                (self.word(t), Some((t, heap)))
            }
        };
        self.fb.emit(Op::CallBuiltin, b as u16, base, c);
        if let Some((t, heap)) = scratch {
            if heap {
                let w = self.word(t);
                self.fb.emit(Op::Drop, w, 0u16, 0u16);
            }
            self.free(t);
        }
        if let (Some(d), true) = (dst, unit) {
            // A unit builtin writes nothing: its value is `()`.
            self.fb.emit(Op::LoadSmall, d, 0u16, 0u16);
        }
        if let Some(run) = run {
            for (k, heap) in kinds.iter().enumerate().rev() {
                let i = run + k as u32;
                if *heap {
                    let w = self.word(i);
                    self.fb.emit(Op::Drop, w, 0u16, 0u16);
                }
                self.free(i);
            }
        }
    }

    /// A call `callee(args)`; its result into `dst` (`None`: dropped).
    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Arg], dst: Option<SlotRef>) {
        match &callee.kind {
            ExprKind::Def(d) if matches!(self.def_kind(*d), Some(DefKind::Fn(_))) => {
                self.call_fn(e, *d, args, dst)
            }
            ExprKind::Prelude(p) => self.prelude(e, *p, args, dst),
            ExprKind::Field(_, _) => self.not_yet("a method call", e.span),
            ExprKind::Host(_) => self.not_yet("a host call", e.span),
            _ => self.not_yet("a construction", e.span),
        }
    }

    /// A call of a script function: the arguments are evaluated left to right, then written into the
    /// callee's parameter words (literals straight there), then `Call`, then the result is moved out of
    /// the callee's word 1.
    fn call_fn(&mut self, e: &Expr, d: DefId, args: &[Arg], dst: Option<SlotRef>) {
        let Some(&f) = self.g.fns.get(&d) else {
            self.not_yet("this call", e.span);
            return;
        };
        let callee = self.fb.of(f);
        let exprs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        let mut vals: Vec<Option<Val>> = Vec::with_capacity(exprs.len());
        for (i, x) in exprs.iter().enumerate() {
            if is_const(x) {
                vals.push(None);
                continue;
            }
            let later_writes = exprs
                .get(i + 1..)
                .unwrap_or_default()
                .iter()
                .any(|y| may_write(y));
            vals.push(Some(if later_writes {
                self.val_copy(x)
            } else {
                self.val(x)
            }));
        }
        for (i, (x, v)) in exprs.iter().zip(vals).enumerate() {
            let param = callee.param(u16::try_from(i).unwrap_or(u16::MAX));
            match v {
                None => self.eval_into(x, param),
                Some(v) => match v.temp {
                    Some(t) => {
                        let op = if v.heap { Op::Move } else { Op::Copy };
                        self.fb.emit(op, param, v.slot, 0u16);
                        self.free(t);
                    }
                    None => self.copy(param, v.slot, v.heap),
                },
            }
        }
        self.at(e.span);
        self.fb.call(f);
        self.at_line();
        let ret_ty = self.g.typed.sigs.get(&d).map_or(TyId::UNIT, |s| s.ret);
        let heap = self.heap_ty(ret_ty);
        match dst {
            Some(d) if heap => {
                self.fb.emit(Op::Move, d, callee.ret(), 0u16);
            }
            Some(d) => {
                self.fb.emit(Op::Copy, d, callee.ret(), 0u16);
            }
            None if heap => {
                self.fb.emit(Op::Drop, callee.ret(), 0u16, 0u16);
            }
            None => {}
        }
    }

    /// Back to the statement's line after a call's own entry.
    fn at_line(&mut self) {
        let (line, col) = self.line;
        self.fb.line_col(line, col);
    }

    fn prelude(&mut self, e: &Expr, p: Prelude, args: &[Arg], dst: Option<SlotRef>) {
        let arg = |i: usize| args.get(i).map(|a| &a.value);
        let float_arg =
            |s: &Self| arg(0).is_some_and(|x| s.g.typed.types.get(s.ty_of(x)) == Ty::Float);
        let one = |b: Builtin, s: &mut Self| match arg(0) {
            Some(x) => s.builtin(b, &[BArg::Expr(x)], s.ty_of(e), dst),
            None => s.not_yet("this call", e.span),
        };
        let two = |b: Builtin, s: &mut Self| match (arg(0), arg(1)) {
            (Some(x), Some(y)) => s.builtin(b, &[BArg::Expr(x), BArg::Expr(y)], s.ty_of(e), dst),
            _ => s.not_yet("this call", e.span),
        };
        match p {
            Prelude::Print => one(Builtin::Print, self),
            Prelude::Float | Prelude::Int => {
                let op = if p == Prelude::Float {
                    Op::IntToFloat
                } else {
                    Op::FloatToInt
                };
                let Some(x) = arg(0) else {
                    return self.not_yet("this call", e.span);
                };
                let v = self.val(x);
                match dst {
                    Some(d) => {
                        self.fb.emit(op, d, v.slot, 0u16);
                    }
                    None => {
                        let t = self.temp(false);
                        let w = self.word(t);
                        self.fb.emit(op, w, v.slot, 0u16);
                        self.free(t);
                    }
                }
                self.done(v);
            }
            Prelude::Str => match (arg(0), dst) {
                (Some(x), Some(d)) => self.text_of(x, d),
                (Some(x), None) => {
                    let t = self.temp(true);
                    let w = self.word(t);
                    self.text_of(x, w);
                    self.fb.emit(Op::Drop, w, 0u16, 0u16);
                    self.free(t);
                }
                _ => self.not_yet("this call", e.span),
            },
            Prelude::ParseInt => one(Builtin::ParseInt, self),
            Prelude::Abs => {
                let b = if float_arg(self) {
                    Builtin::AbsF
                } else {
                    Builtin::AbsI
                };
                one(b, self)
            }
            Prelude::Min | Prelude::Max => {
                let b = match (p == Prelude::Min, float_arg(self)) {
                    (true, true) => Builtin::MinF,
                    (true, false) => Builtin::MinI,
                    (false, true) => Builtin::MaxF,
                    (false, false) => Builtin::MaxI,
                };
                two(b, self)
            }
            Prelude::Sqrt => one(Builtin::Sqrt, self),
            Prelude::Floor => one(Builtin::Floor, self),
            Prelude::Ceil => one(Builtin::Ceil, self),
            Prelude::Round => one(Builtin::Round, self),
            Prelude::Sin => one(Builtin::Sin, self),
            Prelude::Cos => one(Builtin::Cos, self),
            Prelude::Pow => two(Builtin::Pow, self),
            Prelude::Assert => match arg(0) {
                Some(x) => self.builtin(
                    Builtin::Assert,
                    &[BArg::Expr(x), BArg::Str(ASSERT_MESSAGE)],
                    TyId::UNIT,
                    dst,
                ),
                None => self.not_yet("this call", e.span),
            },
            Prelude::AssertEq => self.assert_eq(e, args, dst),
            Prelude::Range => self.not_yet("`range` outside a `for` loop", e.span),
            Prelude::All => self.not_yet("`all`", e.span),
            Prelude::Some | Prelude::None | Prelude::Option | Prelude::Ok | Prelude::Err => {
                self.not_yet("an `Option` value", e.span)
            }
        }
    }

    /// `assert_eq(a, b)`: `a == b` into a temporary, then `Assert`.
    fn assert_eq(&mut self, e: &Expr, args: &[Arg], dst: Option<SlotRef>) {
        let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
            return self.not_yet("this call", e.span);
        };
        let t = self.temp(false);
        let w = self.word(t);
        // `a == b` as `binary` emits it (the operands' type decides the instruction).
        self.binary(e.span, BinOp::Eq, &a.value, &b.value, w);
        self.builtin(
            Builtin::Assert,
            &[BArg::Word(w, false), BArg::Str(ASSERT_EQ_MESSAGE)],
            TyId::UNIT,
            dst,
        );
        self.free(t);
    }
}
