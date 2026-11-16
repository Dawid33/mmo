//! Promises (spec §8.2; plan decisions 2, 3, 12), `await`, and event handlers (spec §7).
//!
//! **Error sets.** A promise's error set (see `ty.rs`) is open during inference: a union-find of
//! [`Ty::SetVar`]s whose roots hold the names so far. Promises that meet (a list, an assignment, `all`, an
//! annotation) share one set, the union of theirs.
//! - **Closing.** An awaited result whose errors are read — by `?` (they join the function's set), as a
//!   `match` scrutinee (exhaustiveness), or by `else err:` (the caught error) — closes its promise's set: no
//!   name joins it again, and a promise with another error meeting it is `type-mismatch`. So what was read
//!   stays true: a loop cannot later bring a promise with more errors to the same variable. `else value` and
//!   `else return …` read no error and close nothing.
//! - **Fixed sets** are a final type's (a signature's, a typed top-level `let`'s). A function's result is
//!   *opened* where it is used: each fixed set in it becomes a fresh open set holding the same names, so the
//!   promises of two calls still unite. Anything else holding a fixed set (a top-level variable) meets only an
//!   equal set, and closes the open side: a fixed set never grows.
//! - **Promise parameters.** Callees are typed before callers, so a `Promise[T]` parameter's set (empty at
//!   first) is final before any call is checked: a caller may pass only promises whose errors it holds. At each
//!   call, every promise set in an argument must be a subset of the matching set in the parameter's type; it
//!   is checked at the end of the caller's body, once the caller's sets are final, else `type-mismatch` at the
//!   argument naming the errors the callee does not handle. Awaiting a parameter is fallible with its set
//!   (empty unless the body widened it), so `return await p?` lets callers pass only promises that cannot
//!   fail. A parameter's set **accepts any** errors ([`Ty::AnySet`] in the signature: no check at a call) when
//!   its body never depends on it: the set was never closed, never met a set made outside the body (a struct
//!   field's, a top-level variable's), never went to a parameter of another function that does not accept any,
//!   and is not in the return type. E.g. `fn g(p: Promise[int]): let v = await p else 0`.
//!
//! **`await`** (decision 12: any function may await; a top-level initialiser may not, nor call a function
//! that may, directly or through what it calls: `await-in-init`; the VM would fault there) takes a
//! `Promise[T]` (else `await-not-promise`; an operand whose type is still unknown is decided to be a promise)
//! and is the fallible `T raises S`, `S` the promise's set so far (possibly empty: always fallible). `await
//! all([a, b])` is `list[T]`, fallible with the union of the sets (they share one). Every `await` is kept for
//! code generation (`Typed::awaits`).
//!
//! **Unused promises** (decision 3): a `Promise` as an expression statement is `unused-promise`; `let _ = p`
//! lets it run detached.
//!
//! **Handlers** (spec §7): an `@on(E)` function takes exactly one parameter, the event (typed by the
//! decorator, or annotated `E`), and returns nothing; else `handler-signature`. Several handlers may take one
//! event. An unknown event was reported by resolution (`unknown-event`), which types the parameter as the
//! error type. A valid handler is kept with its event's type (`Typed::handlers`).

use super::{Body, Check, Class, Infer, error};
use crate::ast::Span;
use crate::hir::{self, Arg, DefId, Expr};
use crate::ty::{SetId, Ty, TyId};
use std::collections::{BTreeMap, BTreeSet};

/// An `await`: what code generation needs (`Typed::awaits`, by the `await` expression).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Await {
    /// The promised value's type.
    pub value: TyId,
    /// The errors the awaited result can fail with, sorted (a parameter that accepts any errors: none known).
    pub raises: Vec<String>,
}

/// An open error set ([`Ty::SetVar`]): its representative, and at a representative the set's state.
#[derive(Debug, Clone)]
pub(super) struct SetVarSlot {
    parent: u32,
    names: BTreeSet<String>,
    /// An awaited result's errors were read: no new name may join.
    closed: bool,
    /// The set was read (closed) or handed to a callee's parameter: a parameter it belongs to does not
    /// accept any errors.
    pinned: bool,
    /// A parameter's set that accepts any errors (final: it becomes [`Ty::AnySet`]).
    any: bool,
}

/// A promise set passed to a callee's parameter: checked at the end of the caller's body.
pub(super) struct ArgSet {
    /// The caller's set (an open set joined with the argument's).
    set: TyId,
    /// The callee's set for it.
    allowed: SetId,
    callee: String,
    at: Span,
}

impl Infer<'_> {
    // Open sets.

    /// A new open error set holding `names`.
    pub(super) fn fresh_set(&mut self, names: Vec<String>) -> TyId {
        let n = u32::try_from(self.set_vars.len()).unwrap_or(u32::MAX);
        self.set_vars.push(SetVarSlot {
            parent: n,
            names: names.into_iter().collect(),
            closed: false,
            pinned: false,
            any: false,
        });
        self.types.intern(Ty::SetVar(n))
    }

    /// The representative of an open set (path halving; the bound is a guard).
    fn set_root(&mut self, mut n: u32) -> u32 {
        for _ in 0..=self.set_vars.len() {
            let Some(parent) = self.set_vars.get(n as usize).map(|s| s.parent) else {
                break;
            };
            if parent == n {
                break;
            }
            let grand = self
                .set_vars
                .get(parent as usize)
                .map_or(parent, |s| s.parent);
            if let Some(slot) = self.set_vars.get_mut(n as usize) {
                slot.parent = grand;
            }
            n = parent;
        }
        n
    }

    fn root_slot(&mut self, n: u32) -> Option<&mut SetVarSlot> {
        let root = self.set_root(n);
        self.set_vars.get_mut(root as usize)
    }

    pub(super) fn set_var_names(&mut self, n: u32) -> Vec<String> {
        self.root_slot(n)
            .map_or(Vec::new(), |s| s.names.iter().cloned().collect())
    }

    /// Whether the open set `n` is a parameter's that accepts any errors.
    pub(super) fn set_var_any(&mut self, n: u32) -> bool {
        self.root_slot(n).is_some_and(|s| s.any)
    }

    /// The names of a promise's set (a fixed, open or any-set), sorted; any-set: none.
    fn set_names(&mut self, set: TyId) -> Vec<String> {
        match self.types.get(set) {
            Ty::Set(s) => self.types.names(s).to_vec(),
            Ty::SetVar(n) => self.set_var_names(n),
            _ => Vec::new(),
        }
    }

    /// Makes two open sets one: the union of their names. False (nothing changes) when one is closed and the
    /// other brings a name it lacks.
    pub(super) fn join_set_vars(&mut self, x: u32, y: u32) -> bool {
        let (x, y) = (self.set_root(x), self.set_root(y));
        if x == y {
            return true;
        }
        let (Some(a), Some(b)) = (
            self.set_vars.get(x as usize).cloned(),
            self.set_vars.get(y as usize).cloned(),
        ) else {
            return false;
        };
        if (a.closed && !b.names.is_subset(&a.names)) || (b.closed && !a.names.is_subset(&b.names))
        {
            return false;
        }
        // The smallest index stays the representative: a set made before the body being typed keeps its
        // class marked as seen outside it (`mark_any_params`).
        let (keep, gone) = (x.min(y), x.max(y));
        let names = self
            .set_vars
            .get_mut(gone as usize)
            .map(|s| {
                s.parent = keep;
                std::mem::take(&mut s.names)
            })
            .unwrap_or_default();
        if let Some(slot) = self.set_vars.get_mut(keep as usize) {
            slot.names.extend(names);
            slot.closed = a.closed || b.closed;
            slot.pinned = a.pinned || b.pinned;
        }
        true
    }

    /// An open set meets a fixed one: they must be equal (a fixed set never grows), and the open one is
    /// closed at it.
    pub(super) fn meet_fixed(&mut self, n: u32, set: SetId) -> bool {
        let fixed: BTreeSet<String> = self.types.names(set).iter().cloned().collect();
        let Some(slot) = self.root_slot(n) else {
            return false;
        };
        if !slot.names.is_subset(&fixed) || (slot.closed && slot.names != fixed) {
            return false;
        }
        slot.names = fixed;
        slot.closed = true;
        slot.pinned = true;
        true
    }

    /// Closes a promise's set (its errors were read).
    fn close_set(&mut self, set: TyId) {
        let set = self.shallow(set);
        if let Ty::SetVar(n) = self.types.get(set)
            && let Some(slot) = self.root_slot(n)
        {
            slot.closed = true;
            slot.pinned = true;
        }
    }

    /// `x`'s errors are read (`?`, a `match`, `else err:`): when `x` is an `await`, its promise's set closes.
    pub(super) fn close_awaited(&mut self, b: &Body, x: &Expr) {
        if let Some((_, set)) = b.awaits.get(&x.id).copied() {
            self.close_set(set);
        }
    }

    /// The representatives of the open sets in `ty`.
    fn set_roots(&mut self, ty: TyId) -> BTreeSet<u32> {
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut stack = vec![ty];
        while let Some(t) = stack.pop() {
            let t = self.shallow(t);
            if !seen.insert(t) {
                continue;
            }
            match self.types.get(t) {
                Ty::SetVar(n) => {
                    out.insert(self.set_root(n));
                }
                // A final type holds no open set.
                _ if self.types.is_ground(t) => {}
                other => stack.extend(other.children()),
            }
        }
        out
    }

    /// At the end of a function's body: marks the sets of its parameters that accept any errors (see the
    /// module doc). `first_set` is the first open set the body made.
    pub(super) fn mark_any_params(&mut self, b: &Body, params: &[TyId], ret: TyId) {
        let in_ret = self.set_roots(ret);
        for p in params {
            for root in self.set_roots(*p) {
                if root < b.first_set || in_ret.contains(&root) {
                    continue;
                }
                if let Some(slot) = self.set_vars.get_mut(root as usize)
                    && !slot.pinned
                {
                    slot.any = true;
                }
            }
        }
    }

    // Fixed sets met in a body.

    /// `ty` (a final type) with every promise set in it replaced by `leaf`'s answer.
    fn reopen(&mut self, ty: TyId, mut leaf: impl FnMut(&mut Self, Ty) -> TyId) -> TyId {
        let mut done: BTreeMap<TyId, TyId> = BTreeMap::new();
        let mut stack = vec![(ty, false)];
        while let Some((t, expanded)) = stack.pop() {
            if done.contains_key(&t) {
                continue;
            }
            if self.set_free.contains(&t) {
                done.insert(t, t);
                continue;
            }
            let shape = self.types.get(t);
            if matches!(shape, Ty::Set(_) | Ty::AnySet) {
                let z = leaf(self, shape);
                done.insert(t, z);
                continue;
            }
            let children = shape.children();
            if children.is_empty() {
                done.insert(t, t);
            } else if !expanded {
                stack.push((t, true));
                stack.extend(children.into_iter().map(|c| (c, false)));
            } else {
                let made: Vec<TyId> = children
                    .iter()
                    .map(|c| done.get(c).copied().unwrap_or(*c))
                    .collect();
                if made == children {
                    if self.types.is_ground(t) {
                        self.set_free.insert(t);
                    }
                    done.insert(t, t);
                } else {
                    let z = self.types.intern(shape.with_children(&made));
                    done.insert(t, z);
                }
            }
        }
        done.get(&ty).copied().unwrap_or(ty)
    }

    /// A function's result type where it is used: each fixed set an open one holding the same names.
    pub(super) fn open_result(&mut self, ret: TyId) -> TyId {
        self.reopen(ret, |inf, shape| match shape {
            Ty::Set(s) => {
                let names = inf.types.names(s).to_vec();
                inf.fresh_set(names)
            }
            _ => inf.fresh_set(Vec::new()),
        })
    }

    /// A callee's parameter types for one call: each fixed set an open, empty one that the argument's set
    /// joins, checked against the fixed one at the end of the body (`check_arg_sets`); an any-set an open one
    /// with no check.
    pub(super) fn open_params(
        &mut self,
        b: &mut Body,
        callee: &str,
        params: &[TyId],
        args: &[Arg],
    ) -> Vec<TyId> {
        let mut out = Vec::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            let at = args.get(i).map_or(Span::default(), |a| a.value.span);
            let mut found: Vec<(TyId, SetId)> = Vec::new();
            let ty = self.reopen(*p, |inf, shape| match shape {
                Ty::Set(s) => {
                    let set = inf.fresh_set(Vec::new());
                    if let Ty::SetVar(n) = inf.types.get(set)
                        && let Some(slot) = inf.set_vars.get_mut(n as usize)
                    {
                        slot.pinned = true;
                    }
                    found.push((set, s));
                    set
                }
                _ => inf.fresh_set(Vec::new()),
            });
            for (set, allowed) in found {
                b.arg_sets.push(ArgSet {
                    set,
                    allowed,
                    callee: callee.to_string(),
                    at,
                });
            }
            out.push(ty);
        }
        out
    }

    /// At the end of a body: every promise argument's errors are among its parameter's.
    pub(super) fn check_arg_sets(&mut self, b: &mut Body) {
        for a in std::mem::take(&mut b.arg_sets) {
            let names = self.set_names(a.set);
            let allowed = self.types.names(a.allowed).to_vec();
            let extra: Vec<String> = names
                .into_iter()
                .filter(|n| allowed.binary_search(n).is_err())
                .collect();
            if extra.is_empty() {
                continue;
            }
            let handles = if allowed.is_empty() {
                "no error".to_string()
            } else {
                format!("only {}", allowed.join(" | "))
            };
            let mut d = error(
                "type-mismatch",
                format!(
                    "`{}` handles {handles} of this promise, which can also fail with {}",
                    a.callee,
                    extra.join(" | ")
                ),
                b.file,
                a.at,
            );
            d.notes.push(
                "a `Promise` parameter takes the errors its function's body knows; one awaited only with `else` and a value takes any"
                    .to_string(),
            );
            b.diags.push(d);
        }
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
            let set = self.fresh_set(Vec::new());
            end = self.types.intern(Ty::Promise(value, set));
            self.bind(n, end);
        }
        match self.types.get(end) {
            Ty::Promise(value, set) => {
                let names = self.set_names(set);
                let set_id = self.types.set(names);
                b.awaits.insert(e.id, (value, set));
                self.types.intern(Ty::Fallible(value, set_id))
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
            let set = self.zonk(set);
            let raises = self.set_names(set);
            self.awaits.insert(id, Await { value, raises });
        }
    }

    /// An expression statement: a promise there is dropped (`unused-promise`, checked once its type is known).
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
