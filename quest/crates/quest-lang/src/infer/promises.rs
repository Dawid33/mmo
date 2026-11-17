//! Promises (spec §8.2; plan decisions 2, 3, 12, 21), `await`, and event handlers (spec §7).
//!
//! **What an `await` can fail with is decided by the promise's origin, not its type** (decision 21). A
//! promise's type is `Promise[T]`, with no error set, and promise types unify on `T` alone. Let **U** be the
//! union of the `raises` lists of every host promise function in the host's API (`HostApi::fns`). An `await`
//! fails with:
//! - exactly a host promise function's `raises` when the awaited expression is a call of it
//!   (`await world.get(k)`), or a local declared by `let` whose every assignment (`let` and `=`) is a call of
//!   host promise functions with that same `raises` list, and that is never the place of a reference (`&p`
//!   could let another promise in);
//! - **U** for every other promise: a parameter, a struct field, a list element (so `all`), a top-level
//!   variable, a function's result, a local assigned anything else. An annotation `Promise[T]` says nothing
//!   about errors either: such a promise also fails with U.
//!
//! Both sets also hold the **base promise errors** `Lost`, `Timeout` and `WrongType`
//! (`quest_vm::host::PROMISE_ERRORS`): the VM fails any awaited promise with them, whatever its host function
//! declares.
//!
//! Sound by construction: a promise is made only by a host promise function, so U holds every error any
//! promise can fail with, and the precise case sees every assignment of its local. Calls of fallible
//! functions keep their precise inferred sets (`errors.rs`).
//!
//! **`await`** (decision 12: any function may await; a top-level initialiser may not, nor call a function
//! that may, directly or through what it calls: `await-in-init`; the VM would fault there) takes a
//! `Promise[T]` (else `await-not-promise`; an operand whose type is still unknown is decided to be a promise)
//! and is the fallible `T raises S`, `S` as above (always fallible). `await all([a, b])` is `list[T]`,
//! fallible with U. Every `await` is kept for code generation (`Typed::awaits`).
//!
//! **Unused promises** (decision 3): a `Promise` as an expression statement is `unused-promise`;
//! `let _ = world.set(…)` lets it run detached. Only a promise being created is detached: `let _ = p` of a
//! place (a local, field, element or global already holding it) is `unused-promise` too, else a later
//! `await p` would fault.
//!
//! **Handlers** (spec §7): an `@on(E)` function takes exactly one parameter, the event (typed by the
//! decorator, or annotated `E`), and returns nothing; else `handler-signature`. Several handlers may take one
//! event. An unknown event was reported by resolution (`unknown-event`), which types the parameter as the
//! error type. A valid handler is kept with its event's type (`Typed::handlers`).

use super::{Body, Check, Class, Infer, error};
use crate::ast::{Span, UnOp};
use crate::hir::{self, DefId, Expr, ExprKind, FStrPart, HostItem, LocalId, Stmt, StmtKind};
use crate::ty::{SetId, Ty, TyId};
use quest_vm::host::{HostFnKind, PROMISE_ERRORS};
use std::collections::{BTreeMap, BTreeSet};

/// An `await`: what code generation needs (`Typed::awaits`, by the `await` expression).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Await {
    /// The promised value's type.
    pub value: TyId,
    /// The errors the awaited result can fail with, sorted (see the module doc).
    pub raises: Vec<String>,
}

/// A node of a body, for the walk of `promise_origins`.
enum Node<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
}

impl Infer<'_> {
    // Origins.

    /// U: every error a promise can fail with (the union of the host promise functions' `raises`).
    fn any_promise_errors(&mut self) -> SetId {
        let host = self.host;
        let names = host
            .fns
            .iter()
            .filter(|f| f.kind == HostFnKind::Promise)
            .flat_map(|f| f.raises.iter().cloned())
            .chain(PROMISE_ERRORS.iter().map(|n| n.to_string()));
        self.types.set(names)
    }

    /// The `raises` of the host promise function `e` calls, if `e` is such a call.
    fn host_promise_call(&mut self, e: &Expr) -> Option<SetId> {
        let ExprKind::Call(callee, _) = &e.kind else {
            return None;
        };
        let ExprKind::Host(HostItem::Fn(i)) = callee.kind else {
            return None;
        };
        let host = self.host;
        let f = host
            .fns
            .get(i as usize)
            .filter(|f| f.kind == HostFnKind::Promise)?;
        let names = f
            .raises
            .iter()
            .cloned()
            .chain(PROMISE_ERRORS.iter().map(|n| n.to_string()));
        Some(self.types.set(names))
    }

    /// The locals of a function's body whose promise has a precise set (see the module doc), with it.
    pub(super) fn promise_origins(&mut self, body: &[Stmt]) -> BTreeMap<LocalId, SetId> {
        let mut assigned: BTreeMap<LocalId, Vec<&Expr>> = BTreeMap::new();
        let mut declared: BTreeSet<LocalId> = BTreeSet::new();
        let mut referenced: BTreeSet<LocalId> = BTreeSet::new();
        let mut stack: Vec<Node> = body.iter().rev().map(Node::Stmt).collect();
        while let Some(n) = stack.pop() {
            match n {
                Node::Stmt(s) => match &s.kind {
                    StmtKind::Let(l, _, v) => {
                        declared.insert(*l);
                        assigned.entry(*l).or_default().push(v);
                        stack.push(Node::Expr(v));
                    }
                    StmtKind::Assign(place, v) => {
                        if let ExprKind::Local(l) = place.kind {
                            assigned.entry(l).or_default().push(v);
                        }
                        stack.extend([Node::Expr(place), Node::Expr(v)]);
                    }
                    StmtKind::Expr(e) | StmtKind::Raise(e) | StmtKind::Return(Some(e)) => {
                        stack.push(Node::Expr(e));
                    }
                    StmtKind::If(branches, other) => {
                        for (c, body) in branches {
                            stack.push(Node::Expr(c));
                            stack.extend(body.iter().map(Node::Stmt));
                        }
                        stack.extend(other.iter().flatten().map(Node::Stmt));
                    }
                    StmtKind::While(c, body) | StmtKind::For(_, c, body) => {
                        stack.push(Node::Expr(c));
                        stack.extend(body.iter().map(Node::Stmt));
                    }
                    StmtKind::Match(x, cases) => {
                        stack.push(Node::Expr(x));
                        for c in cases {
                            stack.extend(c.guard.iter().map(Node::Expr));
                            stack.extend(c.body.iter().map(Node::Stmt));
                        }
                    }
                    StmtKind::Return(None)
                    | StmtKind::Pass
                    | StmtKind::Stage(_)
                    | StmtKind::Break
                    | StmtKind::Continue => {}
                },
                Node::Expr(e) => match &e.kind {
                    ExprKind::Unary(UnOp::Ref, x) => {
                        let mut root = &**x;
                        while let ExprKind::Field(inner, _) | ExprKind::Index(inner, _) = &root.kind
                        {
                            root = inner;
                        }
                        if let ExprKind::Local(l) = root.kind {
                            referenced.insert(l);
                        }
                        stack.push(Node::Expr(x));
                    }
                    ExprKind::FStr(parts) => {
                        for p in parts {
                            if let FStrPart::Expr(x) = p {
                                stack.push(Node::Expr(x));
                            }
                        }
                    }
                    ExprKind::List(items) => stack.extend(items.iter().map(Node::Expr)),
                    ExprKind::Dict(entries) => {
                        for (k, v) in entries {
                            stack.extend([Node::Expr(k), Node::Expr(v)]);
                        }
                    }
                    ExprKind::Unary(_, x)
                    | ExprKind::Await(x)
                    | ExprKind::Try(x)
                    | ExprKind::Field(x, _)
                    | ExprKind::Raise(x)
                    | ExprKind::Return(Some(x)) => stack.push(Node::Expr(x)),
                    ExprKind::Binary(_, x, y) | ExprKind::Index(x, y) | ExprKind::Else(x, y) => {
                        stack.extend([Node::Expr(x), Node::Expr(y)]);
                    }
                    ExprKind::Slice(x, from, to) => {
                        stack.push(Node::Expr(x));
                        stack.extend(from.iter().chain(to.iter()).map(|y| Node::Expr(y)));
                    }
                    ExprKind::Call(callee, args) => {
                        stack.push(Node::Expr(callee));
                        stack.extend(args.iter().map(|a| Node::Expr(&a.value)));
                    }
                    ExprKind::FallbackBlock(_, body) => stack.extend(body.iter().map(Node::Stmt)),
                    _ => {}
                },
            }
        }
        let mut out = BTreeMap::new();
        for l in declared {
            if referenced.contains(&l) {
                continue;
            }
            let mut set: Option<SetId> = None;
            let mut precise = true;
            for v in assigned.get(&l).into_iter().flatten() {
                match (self.host_promise_call(v), set) {
                    (Some(s), None) => set = Some(s),
                    (Some(s), Some(known)) if s == known => {}
                    _ => precise = false,
                }
            }
            if let (true, Some(set)) = (precise, set) {
                out.insert(l, set);
            }
        }
        out
    }

    // `await`.

    /// `await x`.
    pub(super) fn await_op(&mut self, b: &mut Body, e: &Expr, x: &Expr) -> TyId {
        if b.in_init {
            b.diags.push(error(
                "await-in-init",
                "`await` is not allowed in a top-level variable's initialiser".to_string(),
                b.file,
                e.span,
            ));
            self.sink(b, x);
            return TyId::ERROR;
        }
        b.may_await = true;
        let ty = self.expr(b, x);
        let mut end = self.shallow(ty);
        if let Ty::Var(n) = self.types.get(end) {
            // Only a promise can be awaited.
            let value = self.fresh();
            end = self.types.intern(Ty::Promise(value));
            self.bind(n, end);
        }
        match self.types.get(end) {
            Ty::Promise(value) => {
                let set = match &x.kind {
                    ExprKind::Local(l) => b.origins.get(l).copied(),
                    _ => self.host_promise_call(x),
                };
                let set = match set {
                    Some(set) => set,
                    None => self.any_promise_errors(),
                };
                b.awaits.insert(e.id, (value, set));
                self.types.intern(Ty::Fallible(value, set))
            }
            Ty::Error | Ty::Never => TyId::ERROR,
            _ => {
                let found = self.show(end);
                b.diags.push(error(
                    "await-not-promise",
                    format!("`await` takes a `Promise`, not `{found}`"),
                    b.file,
                    x.span,
                ));
                TyId::ERROR
            }
        }
    }

    /// A call of the function `f`: one that may `await` makes the caller one too; a top-level initialiser
    /// cannot call it (`await-in-init`).
    pub(super) fn called(&mut self, b: &mut Body, f: DefId, at: Span) {
        if !self.may_await.contains(&f) {
            return;
        }
        if b.in_init {
            let name = self.krate.qualified_name(f);
            b.diags.push(error(
                "await-in-init",
                format!(
                    "`{name}` may `await` (itself or through what it calls), which a top-level variable's initialiser cannot"
                ),
                b.file,
                at,
            ));
        } else {
            b.may_await = true;
        }
    }

    /// At the end of a body (its types final): keeps its `await`s for code generation.
    pub(super) fn keep_awaits(&mut self, b: &mut Body) {
        for (id, (value, set)) in std::mem::take(&mut b.awaits) {
            let value = self.zonk(value);
            let raises = self.types.names(set).to_vec();
            self.awaits.insert(id, Await { value, raises });
        }
    }

    /// An expression statement: a promise there is dropped (`unused-promise`, checked once its type is known).
    /// `let _ = x` of a place `x`: `x` is not a promise (`unused-promise`). Only a promise just created
    /// (`let _ = world.set(…)`) is detached; one already held would be detached under its holder.
    pub(super) fn held_promise(&mut self, b: &mut Body, ty: TyId, at: Span) {
        b.checks.push(Check {
            ty,
            class: Class::HeldPromise,
            op: "",
            at,
        });
    }

    /// Whether `l` is `_`.
    pub(super) fn is_discard(&self, l: LocalId) -> bool {
        self.krate
            .locals
            .get(l.0 as usize)
            .is_some_and(|x| x.name == "_")
    }

    pub(super) fn unused(&mut self, b: &mut Body, ty: TyId, at: Span) {
        b.checks.push(Check {
            ty,
            class: Class::Unused,
            op: "",
            at,
        });
    }

    // Handlers.

    /// A handler's parameter `p` annotated as `ty`: it must be the event's type.
    pub(super) fn handler_param(&mut self, b: &mut Body, event: TyId, ty: TyId, p: &hir::Param) {
        if self.unify(event, ty) {
            return;
        }
        let (want, found) = (self.show(event), self.show(ty));
        b.diags.push(error(
            "handler-signature",
            format!("this handler takes `{want}` (its `@on`), not `{found}`"),
            b.file,
            p.span,
        ));
    }

    /// A handler's shape once its body is typed: one parameter, nothing returned. A valid one is kept.
    pub(super) fn handler_shape(&mut self, d: DefId, f: &hir::Fn, event: TyId, ret: TyId) {
        let name = self.krate.qualified_name(d);
        let file = self.file_of(d);
        let at = self.def_span(d);
        let mut ok = true;
        if f.params.len() != 1 {
            ok = false;
            self.diags.push(error(
                "handler-signature",
                format!(
                    "the handler `{name}` takes one parameter, its event (`fn {name}(e):`), not {}",
                    f.params.len()
                ),
                file,
                at,
            ));
        }
        if !matches!(self.types.get(ret), Ty::Unit | Ty::Never | Ty::Error) {
            ok = false;
            let shown = self.show(ret);
            self.diags.push(error(
                "handler-signature",
                format!("the handler `{name}` returns `{shown}`: a handler returns nothing"),
                file,
                at,
            ));
        }
        if ok {
            self.handlers.insert(d, event);
        }
    }
}
