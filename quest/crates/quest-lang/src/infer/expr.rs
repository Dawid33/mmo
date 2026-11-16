//! Expressions and operators.

use super::{Body, Check, Class, Infer};
use crate::ast::{BinOp, Span, UnOp};
use crate::hir::{DefKind, Expr, ExprKind, FStrPart, HostItem, PRELUDE, Prelude};
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

    /// An item used as a value that is not one: `type-mismatch`.
    fn not_a_value(&mut self, b: &mut Body, e: &Expr, message: String) -> TyId {
        self.mismatch(b, e.span, message);
        TyId::ERROR
    }

    /// A condition: `bool`.
    pub(super) fn cond(&mut self, b: &mut Body, e: &Expr) {
        let ty = self.expr(b, e);
        if !self.unify(TyId::BOOL, ty) {
            let found = self.show(ty);
            let message = format!("a condition is a `bool`, not `{found}`");
            self.disagree(b, TyId::BOOL, ty, e.span, message);
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
                // Any type formats.
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
                // Not typed yet: the body waits for it (or it is in a reported cycle).
                Some(DefKind::Global(_)) => match self.globals.get(d) {
                    Some(t) => *t,
                    None => {
                        self.wait(b, *d, e.span);
                        TyId::ERROR
                    }
                },
                Some(DefKind::Struct(_) | DefKind::Event(_)) => {
                    let name = self.krate.qualified_name(*d);
                    self.not_a_value(
                        b,
                        e,
                        format!("`{name}` is a type: construct a value with `{name}(…)`"),
                    )
                }
                Some(DefKind::Enum(_)) => {
                    let name = self.krate.qualified_name(*d);
                    self.not_a_value(
                        b,
                        e,
                        format!("`{name}` is an enum: name one of its variants (`{name}::…`)"),
                    )
                }
                Some(DefKind::ErrorSet(_)) => {
                    let name = self.krate.qualified_name(*d);
                    self.not_a_value(b, e, format!("`{name}` is an error set, not a value"))
                }
                // Dialogues and quests never reach inference.
                Some(DefKind::Dialogue | DefKind::Quest) | None => TyId::ERROR,
            },
            ExprKind::Variant(d, i) => {
                let fields = self
                    .adts
                    .get(d)
                    .and_then(|s| s.get(*i as usize))
                    .map_or(0, |s| s.names.len());
                if fields == 0 {
                    self.types.intern(Ty::Adt(*d))
                } else {
                    let name = self.variant_name(*d, *i);
                    self.not_a_value(
                        b,
                        e,
                        format!("`{name}` has fields: construct it with `{name}(…)`"),
                    )
                }
            }
            ExprKind::Host(item) => {
                let host = self.host;
                let message = match item {
                    HostItem::Fn(i) => {
                        let name = host.fns.get(*i as usize).map_or("", |f| f.name.as_str());
                        format!("`{name}` is a host function, not a value: call it")
                    }
                    HostItem::Struct(i) => {
                        let name = host
                            .structs
                            .get(*i as usize)
                            .map_or("", |s| s.name.as_str());
                        format!("`{name}` is a type: construct a value with `{name}(…)`")
                    }
                    HostItem::Event(i) => {
                        let name = host.events.get(*i as usize).map_or("", |s| s.name.as_str());
                        format!("`{name}` is a type: construct a value with `{name}(…)`")
                    }
                };
                self.not_a_value(b, e, message)
            }
            ExprKind::Prelude(p) => match p {
                Prelude::None => {
                    let t = self.fresh();
                    self.types.intern(Ty::Option(t))
                }
                // Results exist only in `match` (Task 22).
                Prelude::Ok | Prelude::Err => TyId::ERROR,
                Prelude::Option => self.not_a_value(
                    b,
                    e,
                    "`Option` is a type: make a value with `Some(x)` or `None`".to_string(),
                ),
                _ => {
                    let name = PRELUDE.iter().find(|(_, x)| x == p).map_or("", |(n, _)| *n);
                    self.not_a_value(
                        b,
                        e,
                        format!("`{name}` is a function, not a value: call it (`{name}(…)`)"),
                    )
                }
            },
            // Error names (Task 22).
            ExprKind::ErrorName(_) => TyId::ERROR,
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
                        let message = format!(
                            "a list holds one type: this is `{found}`, the others `{want}`"
                        );
                        self.disagree(b, elem, ty, x.span, message);
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
                let at = entries.first().map_or(e.span, |(k, _)| k.span);
                self.check_key(b, key, at);
                self.types.intern(Ty::Dict(key, value))
            }
            ExprKind::Unary(op, x) => self.unary(b, *op, x, e.span),
            ExprKind::Binary(op, l, r) => self.binary(b, *op, l, r, e.span),
            ExprKind::Call(callee, args) => self.call(b, e, callee, args),
            ExprKind::Field(x, name) => self.field(b, e, x, name),
            ExprKind::Index(x, i) => self.index(b, e, x, i),
            ExprKind::Slice(x, from, to) => self.slice(b, e, x, [from.as_deref(), to.as_deref()]),
            // `?` and `else` (Task 22); `await` (Task 24).
            ExprKind::Try(x) | ExprKind::Await(x) => {
                self.sink(b, x);
                TyId::ERROR
            }
            ExprKind::Else(x, i) => {
                self.sink(b, x);
                self.sink(b, i);
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
        if value.is_some_and(|x| !matches!(x.kind, ExprKind::Unit)) {
            b.value_returns = true;
        }
        let (ty, at) = match value {
            Some(x) => (self.expr(b, x), x.span),
            None => (TyId::UNIT, at),
        };
        if !self.unify(b.ret, ty) {
            let (want, found) = (self.show(b.ret), self.show(ty));
            let message = format!("this function returns `{want}`, not `{found}`");
            let ret = b.ret;
            self.disagree(b, ret, ty, at, message);
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
                    let message = format!("`not` takes a `bool`, not `{found}`");
                    self.disagree(b, TyId::BOOL, ty, x.span, message);
                }
                TyId::BOOL
            }
            UnOp::Ref => self.types.intern(Ty::Ref(ty)),
            UnOp::Deref => {
                let target = self.fresh();
                let reference = self.types.intern(Ty::Ref(target));
                if !self.unify(reference, ty) {
                    let found = self.show(ty);
                    let message = format!("`*` takes a reference, not `{found}`");
                    self.disagree(b, reference, ty, at, message);
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
            BinOp::In | BinOp::NotIn => return self.contains(b, l, r, at),
            _ => {}
        }
        let (lt, rt) = (self.expr(b, l), self.expr(b, r));
        if !self.unify(lt, rt) {
            let (ls, rs) = (self.show(lt), self.show(rt));
            let message = format!("`{text}` takes two values of one type, not `{ls}` and `{rs}`");
            self.disagree(b, lt, rt, at, message);
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
