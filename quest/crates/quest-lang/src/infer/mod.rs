//! Type inference (spec §5.2; plan D10 stage `infer`) over the resolved program.
//!
//! **Design.**
//! - **One body at a time.** A body is a function's (or method's, or handler's) or a top-level `let`'s
//!   initialiser. Inside it, inference is unification: every binding and expression gets a type, possibly a
//!   fresh type variable ([`Ty::Var`]), and each use adds an equation, solved at once (`Infer::unify`, with an
//!   occurs check). A variable is bound at most once, so a type whose variables are all bound has a final form
//!   that never changes; it is memoised (`Infer::finals`) and every walk over a type is a loop that stops at a
//!   type with no variable, so a type built step by step (`let a = [b]`, a thousand times) costs linear time.
//! - **Callee-first.** Bodies are visited in the call graph's order (`callgraph`), so every function a body
//!   calls already has its final [`Sig`], and every unannotated top-level `let` it reads its final type. A call
//!   never changes the callee: arguments are checked against the callee's signature, and a parameter's type
//!   comes only from its annotation, from its own body, or (a handler's event parameter) from its `@on`. A
//!   callee with no signature yet is a member of a reported cycle; the call is typed as already wrong.
//! - **End of a body** (`Infer::finish`): the deferred operator checks run (`+` on `int`, `float` or `str`,
//!   …, once the operand's type is known); a binding whose type still holds a variable is `needs-annotation`
//!   naming it (no generic functions), then the return type, then any other expression (`print(str([]))`);
//!   each report turns the variable into the error type, so one unknown gives one diagnostic. Then every type
//!   of the body is made final (variables replaced by what they were bound to) and stored in [`Typed`].
//! - **No cascades.** An expression whose error was reported earlier (by any pass) has the error type, which
//!   is compatible with everything and replaces every unknown it meets (`list[_]` against it becomes
//!   `list[?]`). `return`, `break`, `continue` and `raise` have the never type: compatible with everything,
//!   binding nothing.
//! - **Returns.** An unannotated function's return type is a variable each `return` unifies. If the end of the
//!   body is reachable, falling off it returns `()`: a function that returns a value elsewhere is
//!   `missing-return`. A function that never returns normally (it always raises) returns `!`.
//!
//! **Not typed here yet** (each such node's operands are typed, then joined to the error type, so they
//! produce no diagnostic of their own): struct and variant construction, fields, methods, indexing, slices,
//! `in`, host calls and the rest of the prelude (Task 21); `raise`, `?`, `else` and error names (Task 22);
//! patterns, whose bindings have the error type (Task 23); `await` (Task 24). Method calls written `x.m()`
//! reach their callee only through `x`'s type: when Task 21 types them, a callee without a final signature
//! that is not in a reported cycle means the graph's order missed an edge, and the body must be re-queued
//! after the callee (and the cycle check redone with that edge).
//!
//! Files: `expr.rs` (expressions and operators), `stmt.rs` (statements, blocks, reachability), `call.rs`
//! (calls and the prelude); this file holds the driver, unification and the end of a body.

mod call;
mod expr;
mod stmt;

use crate::ast::{Receiver, Span};
use crate::callgraph;
use crate::diag::Diagnostic;
use crate::hir::{self, Builtin, Crate, DefId, DefKind, EventRef, ExprId, LocalId, TypeName};
use crate::ty::{Printer, Ty, TyId, Types};
use quest_vm::host::HostApi;
use std::collections::{BTreeMap, BTreeSet};

/// A function's final type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sig {
    /// `self` or `&self` and its type (the owner, or a reference to it).
    pub receiver: Option<(Receiver, TyId)>,
    pub params: Vec<TyId>,
    pub ret: TyId,
}

/// The typed program: what code generation reads. Every type here is final (no [`Ty::Var`]); something
/// already reported has the error type.
#[derive(Debug, Clone, Default)]
pub struct Typed {
    pub types: Types,
    /// By [`ExprId`]. A callee that names a function has no value type: the error type.
    pub exprs: Vec<TyId>,
    /// By [`LocalId`].
    pub locals: Vec<TyId>,
    /// Every function and method.
    pub sigs: BTreeMap<DefId, Sig>,
    /// Every top-level `let`.
    pub globals: BTreeMap<DefId, TyId>,
    /// Functions and top-level `let`s, callee-first.
    pub order: Vec<DefId>,
}

impl Typed {
    pub fn expr(&self, id: ExprId) -> TyId {
        self.exprs
            .get(id.0 as usize)
            .copied()
            .unwrap_or(TyId::ERROR)
    }

    pub fn local(&self, id: LocalId) -> TyId {
        self.locals
            .get(id.0 as usize)
            .copied()
            .unwrap_or(TyId::ERROR)
    }

    /// `fn(int, str) -> bool`, with `self` / `&self` first for a method.
    pub fn print_sig(&self, sig: &Sig, krate: &Crate, host: &HostApi) -> String {
        let p = Printer {
            types: &self.types,
            krate,
            host,
        };
        let mut params: Vec<String> = Vec::new();
        if let Some((r, _)) = sig.receiver {
            params.push(match r {
                Receiver::Value => "self".to_string(),
                Receiver::Ref => "&self".to_string(),
            });
        }
        params.extend(sig.params.iter().map(|t| p.print(*t)));
        // Task 22 appends ` raises A | B` here.
        format!("fn({}) -> {}", params.join(", "), p.print(sig.ret))
    }
}

/// Types the program, visiting its bodies callee-first; reports recursion too.
pub fn infer(krate: &Crate, host: &HostApi) -> (Typed, Vec<Diagnostic>) {
    let (plan, diags) = callgraph::plan(krate);
    let mut inf = Infer {
        krate,
        host,
        types: Types::new(),
        subst: Vec::new(),
        finals: BTreeMap::new(),
        exprs: vec![TyId::ERROR; krate.expr_count as usize],
        locals: vec![TyId::ERROR; krate.locals.len()],
        sigs: BTreeMap::new(),
        globals: BTreeMap::new(),
        diags,
    };
    inf.annotated_globals();
    inf.self_typed_globals(&plan.cycles);
    for d in &plan.order {
        match krate.def(*d).map(|def| &def.kind) {
            Some(DefKind::Fn(f)) => inf.func(*d, f),
            Some(DefKind::Global(g)) => inf.global(*d, g),
            _ => {}
        }
    }
    let typed = Typed {
        types: inf.types,
        exprs: inf.exprs,
        locals: inf.locals,
        sigs: inf.sigs,
        globals: inf.globals,
        order: plan.order,
    };
    (typed, inf.diags)
}

/// An operator's requirement on its operands' type, checked at the end of the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// `int` or `float`: `- * / %`, unary `-`.
    Numeric,
    /// `int`, `float` or `str`: `+`, and the order comparisons.
    NumericOrStr,
}

#[derive(Debug, Clone, Copy)]
struct Check {
    ty: TyId,
    class: Class,
    op: &'static str,
    at: Span,
}

/// The state of the body being typed.
struct Body<'k> {
    file: &'k str,
    /// The return type `return` unifies with; the error type in an initialiser.
    ret: TyId,
    /// The bindings this body made that inference must decide, in order.
    bindings: Vec<LocalId>,
    /// The expressions typed in this body, in order.
    exprs: Vec<(ExprId, Span)>,
    checks: Vec<Check>,
    /// One entry per enclosing loop: whether a `break` leaves it.
    loops: Vec<bool>,
    /// Whether a `return` was seen.
    returns: bool,
}

impl<'k> Body<'k> {
    fn new(file: &'k str, ret: TyId) -> Body<'k> {
        Body {
            file,
            ret,
            bindings: Vec::new(),
            exprs: Vec::new(),
            checks: Vec::new(),
            loops: Vec::new(),
            returns: false,
        }
    }
}

struct Infer<'k> {
    krate: &'k Crate,
    host: &'k HostApi,
    types: Types,
    /// What each type variable is bound to. A variable is bound at most once.
    subst: Vec<Option<TyId>>,
    /// Types holding variables, all bound: their final form, which therefore never changes. Keeps every
    /// walk over a type that inference built step by step from re-following its variables.
    finals: BTreeMap<TyId, TyId>,
    exprs: Vec<TyId>,
    locals: Vec<TyId>,
    sigs: BTreeMap<DefId, Sig>,
    globals: BTreeMap<DefId, TyId>,
    diags: Vec<Diagnostic>,
}

impl<'k> Infer<'k> {
    // Bodies.

    fn file_of(&self, d: DefId) -> &'k str {
        self.krate
            .def(d)
            .and_then(|def| self.krate.module(def.module))
            .map_or("", |m| m.file.as_str())
    }

    fn def_span(&self, d: DefId) -> Span {
        self.krate.def(d).map_or(Span::default(), |def| def.span)
    }

    /// Every annotated top-level `let` has its type before any body is typed.
    fn annotated_globals(&mut self) {
        for (i, def) in self.krate.defs.iter().enumerate() {
            if let DefKind::Global(hir::Global { ty: Some(t), .. }) = &def.kind {
                let d = DefId(u32::try_from(i).unwrap_or(u32::MAX));
                let file = self.file_of(d);
                let ty = self.lower(t, file);
                self.globals.insert(d, ty);
            }
        }
    }

    /// An unannotated top-level `let` in a cycle (it reads itself, or calls what reads it) is
    /// `needs-annotation`, and has the error type.
    fn self_typed_globals(&mut self, cycles: &[Vec<DefId>]) {
        for d in cycles.iter().flatten() {
            let Some(def) = self.krate.def(*d) else {
                continue;
            };
            if matches!(def.kind, DefKind::Global(hir::Global { ty: None, .. })) {
                self.diags.push(error(
                    "needs-annotation",
                    format!(
                        "the type of `{}` depends on itself: annotate it (`let {}: T = …`)",
                        def.name, def.name
                    ),
                    self.file_of(*d),
                    def.span,
                ));
                self.globals.insert(*d, TyId::ERROR);
            }
        }
    }

    fn func(&mut self, d: DefId, f: &hir::Fn) {
        let file = self.file_of(d);
        let ret = match &f.ret {
            Some(t) => self.lower(t, file),
            None => self.fresh(),
        };
        let mut b = Body::new(file, ret);
        let receiver = f.receiver.map(|(r, local)| {
            let owner = f
                .owner
                .map_or(TyId::ERROR, |o| self.types.intern(Ty::Adt(o)));
            let ty = match r {
                Receiver::Value => owner,
                Receiver::Ref => self.types.intern(Ty::Ref(owner)),
            };
            self.set_local(local, ty);
            (r, ty)
        });
        let mut params = Vec::with_capacity(f.params.len());
        for (i, p) in f.params.iter().enumerate() {
            let ty = match (&p.ty, f.handler) {
                (Some(t), _) => self.lower(t, file),
                // A handler's event parameter is typed by its `@on`.
                (None, Some(event)) if i == 0 => self.event_ty(event),
                (None, _) => self.fresh(),
            };
            self.set_local(p.local, ty);
            b.bindings.push(p.local);
            params.push(ty);
        }
        let diverges = self.block(&mut b, &f.body);
        if !diverges {
            if !self.unify(ret, TyId::UNIT) {
                let name = self.krate.qualified_name(d);
                let ret = self.show(ret);
                self.diags.push(error(
                    "missing-return",
                    format!(
                        "`{name}` returns `{ret}`, but its end can be reached without a `return`"
                    ),
                    file,
                    self.def_span(d),
                ));
            }
        } else if !b.returns
            && let Ty::Var(n) = self.types.get(self.shallow(ret))
        {
            // It never returns normally (it always raises).
            self.bind(n, TyId::NEVER);
        }
        self.finish(&mut b);
        let ret = self.ret_of(d, ret, file);
        let receiver = receiver.map(|(r, t)| (r, self.zonk(t)));
        let params = params.into_iter().map(|t| self.zonk(t)).collect();
        self.sigs.insert(
            d,
            Sig {
                receiver,
                params,
                ret,
            },
        );
    }

    /// A function's final return type; one still unknown is `needs-annotation`.
    fn ret_of(&mut self, d: DefId, ret: TyId, file: &str) -> TyId {
        let ret = self.zonk(ret);
        if self.has_var(ret) {
            let name = self.krate.qualified_name(d);
            self.diags.push(error(
                "needs-annotation",
                format!("cannot infer what `{name}` returns: annotate it (`-> T`)"),
                file,
                self.def_span(d),
            ));
            self.default_vars(ret);
        }
        self.zonk(ret)
    }

    fn global(&mut self, d: DefId, g: &hir::Global) {
        let file = self.file_of(d);
        let mut b = Body::new(file, TyId::ERROR);
        let value = self.expr(&mut b, &g.value);
        let ty = match self.globals.get(&d) {
            Some(annotated) => {
                let annotated = *annotated;
                self.expect(&b, annotated, value, g.value.span);
                annotated
            }
            None => value,
        };
        let ty = self.zonk(ty);
        if self.has_var(ty) {
            let name = self.krate.def(d).map_or("", |def| def.name.as_str());
            self.diags.push(error(
                "needs-annotation",
                format!("cannot infer the type of `{name}`: annotate it (`let {name}: T = …`)"),
                file,
                self.def_span(d),
            ));
            self.default_vars(ty);
        }
        self.finish(&mut b);
        let ty = self.zonk(ty);
        self.globals.insert(d, ty);
    }

    /// The end of a body: deferred checks, `needs-annotation`, final types.
    fn finish(&mut self, b: &mut Body) {
        for c in std::mem::take(&mut b.checks) {
            let ty = self.zonk(c.ty);
            let ok = match self.types.get(ty) {
                Ty::Error | Ty::Never | Ty::Var(_) | Ty::Int | Ty::Float => true,
                Ty::Str => c.class == Class::NumericOrStr,
                _ => false,
            };
            if !ok {
                let wanted = match c.class {
                    Class::Numeric => "`int` or `float`",
                    Class::NumericOrStr => "`int`, `float` or `str`",
                };
                let found = self.show(ty);
                self.diags.push(error(
                    "type-mismatch",
                    format!("`{}` takes {wanted}, not `{found}`", c.op),
                    b.file,
                    c.at,
                ));
            }
        }
        for local in std::mem::take(&mut b.bindings) {
            let ty = self.zonk(self.local_ty(local));
            if self.has_var(ty) {
                let (name, at) = self
                    .krate
                    .local(local)
                    .map_or((String::new(), Span::default()), |l| {
                        (l.name.clone(), l.span)
                    });
                let shown = self.show(ty);
                let mut d = error(
                    "needs-annotation",
                    format!("cannot infer the type of `{name}`: annotate it"),
                    b.file,
                    at,
                );
                if shown != "_" {
                    d.notes.push(format!("it is `{shown}`, with `_` unknown"));
                }
                self.diags.push(d);
                self.default_vars(ty);
            }
        }
        for (id, at) in std::mem::take(&mut b.exprs) {
            let ty = self.zonk(self.expr_ty(id));
            if self.has_var(ty) {
                let shown = self.show(ty);
                self.diags.push(error(
                    "needs-annotation",
                    format!(
                        "cannot infer the type of this expression (`{shown}`): annotate the binding it goes into"
                    ),
                    b.file,
                    at,
                ));
                self.default_vars(ty);
            }
            let ty = self.zonk(ty);
            if let Some(slot) = self.exprs.get_mut(id.0 as usize) {
                *slot = ty;
            }
        }
    }

    // Bindings and expressions.

    fn set_local(&mut self, id: LocalId, ty: TyId) {
        if let Some(slot) = self.locals.get_mut(id.0 as usize) {
            *slot = ty;
        }
    }

    fn local_ty(&self, id: LocalId) -> TyId {
        self.locals
            .get(id.0 as usize)
            .copied()
            .unwrap_or(TyId::ERROR)
    }

    fn expr_ty(&self, id: ExprId) -> TyId {
        self.exprs
            .get(id.0 as usize)
            .copied()
            .unwrap_or(TyId::ERROR)
    }

    // Types.

    fn fresh(&mut self) -> TyId {
        let n = u32::try_from(self.subst.len()).unwrap_or(u32::MAX);
        self.subst.push(None);
        self.types.intern(Ty::Var(n))
    }

    /// Binds an unbound variable; a bound one keeps its binding (what `finals` relies on).
    fn bind(&mut self, var: u32, ty: TyId) {
        if let Some(slot) = self.subst.get_mut(var as usize)
            && slot.is_none()
        {
            *slot = Some(ty);
        }
    }

    /// `ty` with its outer variables followed.
    fn shallow(&self, mut ty: TyId) -> TyId {
        // Bindings never form a cycle (occurs check); the bound is a guard.
        for _ in 0..=self.subst.len() {
            match self.types.get(ty) {
                Ty::Var(n) => match self.subst.get(n as usize).copied().flatten() {
                    Some(next) => ty = next,
                    None => return ty,
                },
                _ => return ty,
            }
        }
        ty
    }

    /// Makes `a` and `b` the same type; false when they cannot be (some parts may be bound by then).
    fn unify(&mut self, a: TyId, b: TyId) -> bool {
        let mut ok = true;
        let mut work = vec![(a, b)];
        while let Some((a, b)) = work.pop() {
            let (a, b) = (self.shallow(a), self.shallow(b));
            if a == b {
                continue;
            }
            match (self.types.get(a), self.types.get(b)) {
                // The error type takes the place of every unknown it meets.
                (Ty::Error, _) => self.default_vars(b),
                (_, Ty::Error) => self.default_vars(a),
                (Ty::Never, _) | (_, Ty::Never) => {}
                (Ty::Var(n), _) => ok &= self.bind_checked(n, b),
                (_, Ty::Var(n)) => ok &= self.bind_checked(n, a),
                (x, y) if std::mem::discriminant(&x) == std::mem::discriminant(&y) => {
                    let (xs, ys) = (x.children(), y.children());
                    if xs.is_empty() || xs.len() != ys.len() {
                        ok = false;
                    }
                    work.extend(xs.into_iter().zip(ys));
                }
                _ => ok = false,
            }
        }
        ok
    }

    /// Binds a variable unless the type contains it (an infinite type).
    fn bind_checked(&mut self, var: u32, ty: TyId) -> bool {
        if self.vars_in(ty).contains(&var) {
            return false;
        }
        self.bind(var, ty);
        true
    }

    /// The unbound variables in a type.
    fn vars_in(&self, ty: TyId) -> BTreeSet<u32> {
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut stack = vec![ty];
        while let Some(t) = stack.pop() {
            let t = self.shallow(t);
            if self.types.is_ground(t) || self.finals.contains_key(&t) || !seen.insert(t) {
                continue;
            }
            match self.types.get(t) {
                Ty::Var(n) => {
                    out.insert(n);
                }
                other => stack.extend(other.children()),
            }
        }
        out
    }

    fn has_var(&self, ty: TyId) -> bool {
        !self.vars_in(ty).is_empty()
    }

    /// Binds every unbound variable in `ty` to the error type (after it was reported).
    fn default_vars(&mut self, ty: TyId) {
        for v in self.vars_in(ty) {
            self.bind(v, TyId::ERROR);
        }
    }

    /// `ty` with every bound variable replaced by its binding, all the way down.
    fn zonk(&mut self, ty: TyId) -> TyId {
        let mut done: BTreeMap<TyId, TyId> = BTreeMap::new();
        let mut stack = vec![(ty, false)];
        while let Some((t, expanded)) = stack.pop() {
            if done.contains_key(&t) {
                continue;
            }
            if self.types.is_ground(t) {
                done.insert(t, t);
                continue;
            }
            if let Some(z) = self.finals.get(&t) {
                done.insert(t, *z);
                continue;
            }
            let s = self.shallow(t);
            if s != t {
                match done.get(&s) {
                    Some(z) => {
                        if self.types.is_ground(*z) {
                            self.finals.insert(t, *z);
                        }
                        done.insert(t, *z);
                    }
                    None => stack.extend([(t, false), (s, false)]),
                }
                continue;
            }
            let shape = self.types.get(t);
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
                let z = self.types.intern(shape.with_children(&made));
                if self.types.is_ground(z) {
                    self.finals.insert(t, z);
                }
                done.insert(t, z);
            }
        }
        done.get(&ty).copied().unwrap_or(ty)
    }

    /// A type as source writes it, for messages.
    fn show(&mut self, ty: TyId) -> String {
        let ty = self.zonk(ty);
        Printer {
            types: &self.types,
            krate: self.krate,
            host: self.host,
        }
        .print(ty)
    }

    fn event_ty(&mut self, event: EventRef) -> TyId {
        self.types.intern(match event {
            EventRef::Script(d) => Ty::Adt(d),
            EventRef::Host(i) => Ty::HostEvent(i),
        })
    }

    /// An annotation's type. A wrong number of type arguments is `type-mismatch`.
    fn lower(&mut self, t: &hir::Type, file: &str) -> TyId {
        match &t.kind {
            hir::TypeKind::Error => TyId::ERROR,
            hir::TypeKind::Unit => TyId::UNIT,
            hir::TypeKind::Ref(inner) => {
                let inner = self.lower(inner, file);
                self.types.intern(Ty::Ref(inner))
            }
            hir::TypeKind::Named(name, args) => {
                let args: Vec<TyId> = args.iter().map(|a| self.lower(a, file)).collect();
                let (wanted, shape) = match name {
                    TypeName::Builtin(b) => match b {
                        Builtin::Int => (0, Ty::Int),
                        Builtin::Float => (0, Ty::Float),
                        Builtin::Bool => (0, Ty::Bool),
                        Builtin::Str => (0, Ty::Str),
                        Builtin::List => (1, Ty::List(TyId::ERROR)),
                        Builtin::Option => (1, Ty::Option(TyId::ERROR)),
                        Builtin::Promise => (1, Ty::Promise(TyId::ERROR)),
                        Builtin::Dict => (2, Ty::Dict(TyId::ERROR, TyId::ERROR)),
                    },
                    TypeName::Def(d) => (0, Ty::Adt(*d)),
                    TypeName::HostStruct(i) => (0, Ty::HostStruct(*i)),
                    TypeName::HostEvent(i) => (0, Ty::HostEvent(*i)),
                };
                if args.len() != wanted {
                    let head = self.type_name_text(*name);
                    let takes = match wanted {
                        0 => "no type arguments".to_string(),
                        1 => "one type argument".to_string(),
                        n => format!("{n} type arguments"),
                    };
                    self.diags.push(error(
                        "type-mismatch",
                        format!("`{head}` takes {takes}, {} given", args.len()),
                        file,
                        t.span,
                    ));
                    return TyId::ERROR;
                }
                self.types.intern(shape.with_children(&args))
            }
        }
    }

    /// A type name as source writes it.
    fn type_name_text(&self, name: TypeName) -> String {
        match name {
            TypeName::Builtin(b) => hir::BUILTIN_TYPES
                .iter()
                .find(|(_, x)| *x == b)
                .map_or(String::new(), |(n, _)| n.to_string()),
            TypeName::Def(d) => self.krate.qualified_name(d),
            TypeName::HostStruct(i) => self
                .host
                .structs
                .get(i as usize)
                .map_or(String::new(), |s| s.name.clone()),
            TypeName::HostEvent(i) => self
                .host
                .events
                .get(i as usize)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Unifies `found` with `expected`, else `type-mismatch` at `at`; true when they agree.
    fn expect(&mut self, b: &Body, expected: TyId, found: TyId, at: Span) -> bool {
        if self.unify(expected, found) {
            return true;
        }
        let (e, f) = (self.show(expected), self.show(found));
        self.mismatch(b, at, format!("expected `{e}`, found `{f}`"));
        false
    }

    fn mismatch(&mut self, b: &Body, at: Span, message: String) {
        self.diags.push(error("type-mismatch", message, b.file, at));
    }
}

fn error(code: &'static str, message: String, file: &str, at: Span) -> Diagnostic {
    Diagnostic::error(code, message, file, at.line, at.col, at.len)
}
