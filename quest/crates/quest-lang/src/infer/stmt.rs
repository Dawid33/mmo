//! Statements and blocks, and whether the end of a block can be reached.
//!
//! A statement **diverges** when the code after it cannot run: `return`, `raise`, `break`, `continue`
//! (statements, or the same as an expression statement), a call of a function that never returns (`-> !`);
//! an `if` with an `else` whose branches all diverge;
//! a `match` whose arms all do (it is exhaustive: `non-exhaustive` otherwise); `while true` that no `break`
//! leaves. A `for` loop never diverges.

use super::{Body, BuiltinMethod, Infer, Method};
use crate::hir::{Expr, ExprKind, LocalId, Stmt, StmtKind};
use crate::ty::{Ty, TyId};

impl Infer<'_> {
    /// Types a block; true when its end cannot be reached.
    pub(super) fn block(&mut self, b: &mut Body, body: &[Stmt]) -> bool {
        let mut diverges = false;
        for s in body {
            diverges |= self.stmt(b, s);
        }
        diverges
    }

    fn stmt(&mut self, b: &mut Body, s: &Stmt) -> bool {
        match &s.kind {
            StmtKind::Let(local, annotation, value) => {
                let found = self.expr(b, value);
                let ty = match annotation {
                    Some(t) => {
                        let wanted = self.lower(t, b.file, &mut b.diags);
                        self.expect(b, wanted, found, value.span);
                        wanted
                    }
                    None => found,
                };
                if self.is_discard(*local) && self.is_place(value) {
                    self.held_promise(b, ty, value.span);
                }
                self.set_local(*local, ty);
                b.bindings.push(*local);
                false
            }
            StmtKind::Assign(place, value) => {
                let wanted = self.expr(b, place);
                let found = self.expr(b, value);
                if matches!(self.methods.get(&place.id), Some(Method::Host(_))) {
                    b.diags.push(super::error(
                        "not-assignable",
                        "a host property is read-only: the host changes it, through a command"
                            .to_string(),
                        b.file,
                        place.span,
                    ));
                    return false;
                }
                self.expect(b, wanted, found, value.span);
                // The place's type: a reference only when it took one (else the mismatch is reported).
                match &place.kind {
                    ExprKind::Field(_, _) | ExprKind::Index(_, _) => {
                        self.no_ref(b, wanted, value.span, "stored in a field or an element");
                    }
                    ExprKind::Def(_) => {
                        self.no_ref(b, wanted, value.span, "kept in a top-level variable");
                    }
                    // A reference local (or parameter) is never rebound: its new target could be a binding
                    // of an inner block that dies before it (spec §5.3, "no dangling").
                    ExprKind::Local(_) => self.not_ref_local(b, wanted, place.span),
                    _ => {}
                }
                false
            }
            // `return`, `break`, `continue`, `raise`, or a call that never returns.
            StmtKind::Expr(e) => {
                let ty = self.expr(b, e);
                self.unused(b, ty, e.span);
                self.shallow(ty) == TyId::NEVER
            }
            StmtKind::If(branches, other) => {
                let mut all = true;
                for (c, body) in branches {
                    self.cond(b, c);
                    all &= self.block(b, body);
                }
                match other {
                    Some(body) => self.block(b, body) && all,
                    None => false,
                }
            }
            StmtKind::While(c, body) => {
                self.cond(b, c);
                b.loops.push(false);
                self.block(b, body);
                let broken = b.loops.pop().unwrap_or(false);
                matches!(c.kind, ExprKind::Bool(true)) && !broken
            }
            StmtKind::For(names, iter, body) => {
                self.for_bindings(b, names, iter);
                b.loops.push(false);
                self.block(b, body);
                b.loops.pop();
                false
            }
            StmtKind::Match(scrutinee, cases) => self.match_stmt(b, s.span, scrutinee, cases),
            StmtKind::Pass => false,
            StmtKind::Break => {
                if let Some(seen) = b.loops.last_mut() {
                    *seen = true;
                }
                true
            }
            StmtKind::Continue => true,
            StmtKind::Return(value) => {
                self.ret_value(b, value.as_ref(), s.span);
                true
            }
            StmtKind::Raise(e) => {
                self.raise(b, e);
                true
            }
        }
    }

    /// The bindings of `for`: one name over a list (its elements) or a dict (its keys); two names over
    /// `d.items()` (a dict's keys and values).
    fn for_bindings(&mut self, b: &mut Body, names: &[LocalId], iter: &Expr) {
        let elems: Vec<TyId> = match names {
            [_] => vec![self.for_one(b, iter)],
            _ => {
                let (k, v) = self.for_items(b, iter);
                vec![k, v]
            }
        };
        for (n, ty) in names
            .iter()
            .zip(elems.into_iter().chain(std::iter::repeat(TyId::ERROR)))
        {
            self.set_local(*n, ty);
            b.bindings.push(*n);
        }
    }

    /// What `for x in iter` binds `x` to.
    fn for_one(&mut self, b: &mut Body, iter: &Expr) -> TyId {
        let ty = self.expr(b, iter);
        let (ty, _) = self.deref(ty);
        match self.types.get(ty) {
            Ty::List(t) => t,
            Ty::Dict(k, _) => k,
            Ty::Error | Ty::Never => TyId::ERROR,
            Ty::Var(_) => {
                self.unknown_receiver(b, ty, iter.span, "a `for` loop");
                TyId::ERROR
            }
            _ => {
                let found = self.show(ty);
                self.mismatch(
                    b,
                    iter.span,
                    format!("a `for` loop goes over a list, a dict or a `range`, not `{found}`"),
                );
                TyId::ERROR
            }
        }
    }

    /// What `for k, v in d.items()` binds `k` and `v` to.
    fn for_items(&mut self, b: &mut Body, iter: &Expr) -> (TyId, TyId) {
        let dict = match &iter.kind {
            ExprKind::Call(callee, args) if args.is_empty() => match &callee.kind {
                ExprKind::Field(d, name) if name == "items" => Some((callee, d)),
                _ => None,
            },
            _ => None,
        };
        let Some((callee, d)) = dict else {
            self.sink(b, iter);
            self.mismatch(
                b,
                iter.span,
                "`for k, v in …` goes over a dict's `.items()`".to_string(),
            );
            return (TyId::ERROR, TyId::ERROR);
        };
        let ty = self.expr(b, d);
        self.record(b, callee, TyId::ERROR);
        self.record(b, iter, TyId::ERROR);
        let (ty, _) = self.deref(ty);
        match self.types.get(ty) {
            Ty::Dict(k, v) => {
                self.methods
                    .insert(iter.id, Method::Builtin(BuiltinMethod::Items));
                (k, v)
            }
            Ty::Error | Ty::Never => (TyId::ERROR, TyId::ERROR),
            Ty::Var(_) => {
                self.unknown_receiver(b, ty, d.span, "a `for` loop");
                (TyId::ERROR, TyId::ERROR)
            }
            _ => {
                let found = self.show(ty);
                self.mismatch(
                    b,
                    iter.span,
                    format!("`.items()` is a dict's, not `{found}`'s"),
                );
                (TyId::ERROR, TyId::ERROR)
            }
        }
    }
}
