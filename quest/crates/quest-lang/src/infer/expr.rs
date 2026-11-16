//! Expressions and operators.

use super::{Body, Check, Class, Infer};
use crate::ast::{BinOp, Span, UnOp};
use crate::hir::{DefKind, Expr, ExprKind, FStrPart};
use crate::ty::{Ty, TyId};

impl Infer<'_> {
    /// Types an expression and records its type.
    pub(super) fn expr(&mut self, b: &mut Body, e: &Expr) -> TyId {
        let ty = self.expr_kind(b, e);
        self.record(b, e, ty);
        ty
    }

    pub(super) fn record(&mut self, b: &mut Body, e: &Expr, ty: TyId) {
        if let Some(slot) = self.exprs.get_mut(e.id.0 as usize) {
            *slot = ty;
        }
        b.exprs.push((e.id, e.span));
    }

    /// Types an expression whose use is not typed yet (a later task's): it produces no diagnostic of its own.
    pub(super) fn sink(&mut self, b: &mut Body, e: &Expr) {
        let ty = self.expr(b, e);
        self.unify(ty, TyId::ERROR);
    }

    /// A condition: `bool`.
    pub(super) fn cond(&mut self, b: &mut Body, e: &Expr) {
        let ty = self.expr(b, e);
        if !self.unify(TyId::BOOL, ty) {
            let found = self.show(ty);
            self.mismatch(b, e.span, format!("a condition is a `bool`, not `{found}`"));
        }
    }

    fn expr_kind(&mut self, b: &mut Body, e: &Expr) -> TyId {
        match &e.kind {
            ExprKind::Error => TyId::ERROR,
            ExprKind::Unit => TyId::UNIT,
            ExprKind::Int(_) => TyId::INT,
            ExprKind::Float(_) => TyId::FLOAT,
            ExprKind::Bool(_) => TyId::BOOL,
            ExprKind::Str(_) => TyId::STR,
            ExprKind::FStr(parts) => {
                // Any type formats (Task 21 pins which).
                for p in parts {
                    if let FStrPart::Expr(x) = p {
                        self.expr(b, x);
                    }
                }
                TyId::STR
            }
            ExprKind::Local(id) => self.local_ty(*id),
            ExprKind::Def(d) => match self.krate.def(*d).map(|def| &def.kind) {
                Some(DefKind::Fn(_)) => {
                    let name = self.krate.qualified_name(*d);
                    self.mismatch(
                        b,
                        e.span,
                        format!("`{name}` is a function, not a value: call it (`{name}(…)`)"),
                    );
                    TyId::ERROR
                }
                // In a cycle (reported) until its initialiser is typed.
                Some(DefKind::Global(_)) => self.globals.get(d).copied().unwrap_or(TyId::ERROR),
                // A struct, enum or event as a value (Task 21).
                _ => TyId::ERROR,
            },
            // Data, host items and the prelude as values (Task 21); error names (Task 22).
            ExprKind::Variant(_, _)
            | ExprKind::Host(_)
            | ExprKind::Prelude(_)
            | ExprKind::ErrorName(_) => TyId::ERROR,
            ExprKind::List(items) => {
                // The first element's type (a variable only for `[]`): no variable to follow later.
                let mut elem = None;
                for x in items {
                    let ty = self.expr(b, x);
                    let Some(elem) = elem else {
                        elem = Some(ty);
                        continue;
                    };
                    if !self.unify(elem, ty) {
                        let (want, found) = (self.show(elem), self.show(ty));
                        self.mismatch(
                            b,
                            x.span,
                            format!(
                                "a list holds one type: this is `{found}`, the others `{want}`"
                            ),
                        );
                    }
                }
                let elem = elem.unwrap_or_else(|| self.fresh());
                self.types.intern(Ty::List(elem))
            }
            ExprKind::Dict(entries) => {
                let mut types: Option<(TyId, TyId)> = None;
                for (k, v) in entries {
                    let (kt, vt) = (self.expr(b, k), self.expr(b, v));
                    let Some((key, value)) = types else {
                        types = Some((kt, vt));
                        continue;
                    };
                    self.expect(b, key, kt, k.span);
                    self.expect(b, value, vt, v.span);
                }
                let (key, value) = match types {
                    Some(kv) => kv,
                    None => (self.fresh(), self.fresh()),
                };
                self.types.intern(Ty::Dict(key, value))
            }
            ExprKind::Unary(op, x) => self.unary(b, *op, x, e.span),
            ExprKind::Binary(op, l, r) => self.binary(b, *op, l, r, e.span),
            ExprKind::Call(callee, args) => self.call(b, e, callee, args),
            // Fields, methods, indexing, slices (Task 21); `?` and `else` (Task 22); `await` (Task 24).
            ExprKind::Field(x, _) | ExprKind::Try(x) | ExprKind::Await(x) => {
                self.sink(b, x);
                TyId::ERROR
            }
            ExprKind::Index(x, i) | ExprKind::Else(x, i) => {
                self.sink(b, x);
                self.sink(b, i);
                TyId::ERROR
            }
            ExprKind::Slice(x, from, to) => {
                self.sink(b, x);
                for y in [from, to].into_iter().flatten() {
                    self.sink(b, y);
                }
                TyId::ERROR
            }
            ExprKind::FallbackBlock(local, body) => {
                self.set_local(*local, TyId::ERROR);
                self.block(b, body);
                TyId::ERROR
            }
            ExprKind::Return(x) => {
                self.ret_value(b, x.as_deref(), e.span);
                TyId::NEVER
            }
            ExprKind::Break => {
                if let Some(seen) = b.loops.last_mut() {
                    *seen = true;
                }
                TyId::NEVER
            }
            ExprKind::Continue => TyId::NEVER,
            ExprKind::Raise(x) => {
                self.sink(b, x);
                TyId::NEVER
            }
        }
    }

    /// `return` with its value (`()` without one).
    pub(super) fn ret_value(&mut self, b: &mut Body, value: Option<&Expr>, at: Span) {
        b.returns = true;
        let (ty, at) = match value {
            Some(x) => (self.expr(b, x), x.span),
            None => (TyId::UNIT, at),
        };
        if !self.unify(b.ret, ty) {
            let (want, found) = (self.show(b.ret), self.show(ty));
            self.mismatch(
                b,
                at,
                format!("this function returns `{want}`, not `{found}`"),
            );
        }
    }

    fn unary(&mut self, b: &mut Body, op: UnOp, x: &Expr, at: Span) -> TyId {
        let ty = self.expr(b, x);
        match op {
            UnOp::Neg => {
                b.checks.push(Check {
                    ty,
                    class: Class::Numeric,
                    op: "-",
                    at,
                });
                ty
            }
            UnOp::Not => {
                if !self.unify(TyId::BOOL, ty) {
                    let found = self.show(ty);
                    self.mismatch(b, x.span, format!("`not` takes a `bool`, not `{found}`"));
                }
                TyId::BOOL
            }
            UnOp::Ref => self.types.intern(Ty::Ref(ty)),
            UnOp::Deref => {
                let target = self.fresh();
                let reference = self.types.intern(Ty::Ref(target));
                if !self.unify(reference, ty) {
                    let found = self.show(ty);
                    self.mismatch(b, at, format!("`*` takes a reference, not `{found}`"));
                    return TyId::ERROR;
                }
                target
            }
        }
    }

    fn binary(&mut self, b: &mut Body, op: BinOp, l: &Expr, r: &Expr, at: Span) -> TyId {
        let text = op_text(op);
        match op {
            BinOp::And | BinOp::Or => {
                self.cond(b, l);
                self.cond(b, r);
                return TyId::BOOL;
            }
            // `in` (Task 21).
            BinOp::In | BinOp::NotIn => {
                self.sink(b, l);
                self.sink(b, r);
                return TyId::BOOL;
            }
            _ => {}
        }
        let (lt, rt) = (self.expr(b, l), self.expr(b, r));
        if !self.unify(lt, rt) {
            let (ls, rs) = (self.show(lt), self.show(rt));
            self.mismatch(
                b,
                at,
                format!("`{text}` takes two values of one type, not `{ls}` and `{rs}`"),
            );
            return match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => TyId::ERROR,
                _ => TyId::BOOL,
            };
        }
        let class = match op {
            BinOp::Eq | BinOp::Ne => return TyId::BOOL,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Add => Class::NumericOrStr,
            _ => Class::Numeric,
        };
        b.checks.push(Check {
            ty: lt,
            class,
            op: text,
            at,
        });
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => lt,
            _ => TyId::BOOL,
        }
    }
}

fn op_text(op: BinOp) -> &'static str {
    match op {
        BinOp::Or => "or",
        BinOp::And => "and",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::In => "in",
        BinOp::NotIn => "not in",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
    }
}
