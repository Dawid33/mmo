//! Statements and blocks, and whether the end of a block can be reached.
//!
//! A statement **diverges** when the code after it cannot run: `return`, `raise`, `break`, `continue`
//! (statements, or the same as an expression statement); an `if` with an `else` whose branches all diverge;
//! a `match` whose arms all do (it is exhaustive, Task 23); `while true` that no `break` leaves. A `for`
//! loop never diverges.

use super::{Body, Infer};
use crate::hir::{Expr, ExprKind, Pattern, PatternKind, Stmt, StmtKind};
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
                        let wanted = self.lower(t, b.file);
                        self.expect(b, wanted, found, value.span);
                        wanted
                    }
                    None => found,
                };
                self.set_local(*local, ty);
                b.bindings.push(*local);
                false
            }
            StmtKind::Assign(place, value) => {
                let wanted = self.expr(b, place);
                let found = self.expr(b, value);
                self.expect(b, wanted, found, value.span);
                false
            }
            StmtKind::Expr(e) => {
                self.expr(b, e);
                diverging_expr(e)
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
            StmtKind::Match(scrutinee, cases) => {
                // Patterns (Task 23): the scrutinee is not constrained by them yet, their bindings are `?`.
                self.sink(b, scrutinee);
                let mut all = !cases.is_empty();
                for c in cases {
                    self.pattern_bindings(&c.pattern);
                    if let Some(g) = &c.guard {
                        self.cond(b, g);
                    }
                    all &= self.block(b, &c.body);
                }
                all
            }
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
                // Error values (Task 22).
                self.sink(b, e);
                true
            }
        }
    }

    /// The bindings of `for`: one name over a list (its elements) or a dict (its keys).
    fn for_bindings(&mut self, b: &mut Body, names: &[crate::hir::LocalId], iter: &Expr) {
        let ty = self.expr(b, iter);
        let ty = self.shallow(ty);
        let elem = match self.types.get(ty) {
            Ty::List(t) => t,
            Ty::Dict(k, _) => k,
            // Unknown: the iterable's own binding is `needs-annotation`.
            Ty::Error | Ty::Never | Ty::Var(_) => TyId::ERROR,
            _ => {
                let found = self.show(ty);
                self.mismatch(
                    b,
                    iter.span,
                    format!("a `for` loop goes over a list, a dict or a `range`, not `{found}`"),
                );
                TyId::ERROR
            }
        };
        match names {
            [one] => {
                self.set_local(*one, elem);
                b.bindings.push(*one);
            }
            // `for k, v in d.items()` (Task 21).
            _ => {
                for n in names {
                    self.set_local(*n, TyId::ERROR);
                }
            }
        }
    }

    /// Gives every binding of a pattern the error type (patterns are typed in Task 23).
    fn pattern_bindings(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Bind(local) => self.set_local(*local, TyId::ERROR),
            PatternKind::Ctor(_, fields) => {
                for f in fields {
                    self.pattern_bindings(&f.pattern);
                }
            }
            PatternKind::Or(alts) => {
                for a in alts {
                    self.pattern_bindings(a);
                }
            }
            PatternKind::Error | PatternKind::Wild | PatternKind::Lit(_) => {}
        }
    }
}

/// An expression statement after which nothing runs.
fn diverging_expr(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Return(_) | ExprKind::Break | ExprKind::Continue | ExprKind::Raise(_)
    )
}
