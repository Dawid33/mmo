//! Type inference (spec §5.2; plan D10 stage `infer`) over the resolved program.
//!
//! **Design.**
//! - **One body at a time.** A body is a function's (or method's, or handler's) or a top-level `let`'s
//!   initialiser. Inside it, inference is unification: every binding and expression gets a type, possibly a
//!   fresh type variable ([`Ty::Var`]), and each use adds an equation, solved at once (`Infer::unify`, with an
//!   occurs check). A variable is bound at most once (path compression only shortens a chain to the same
//!   end), so a type whose variables are all bound has a final form that never changes; it is memoised
//!   (`Infer::finals`), and every walk over a type is a loop that stops at a type with no variable. A type
//!   built step by step (`let a = [b]`, a thousand times) or a long chain of variables is then typed in
//!   linear time; an occurs check over a deep type that keeps an unbound variable is still linear per bind.
//! - **Callee-first.** Bodies are visited in the call graph's order (`callgraph`), so every function a body
//!   calls already has its final [`Sig`], and every unannotated top-level `let` it reads its final type. A call
//!   never changes the callee: arguments are checked against the callee's signature, and a parameter's type
//!   comes only from its annotation, from its own body, or (a handler's event parameter) from its `@on`.
//! - **The worklist.** The graph cannot see a method call `x.m()` (`x`'s type decides the callee). A body that
//!   reaches a function or an unannotated `let` with no final type yet records it (`Body::waits`), is stopped
//!   (its diagnostics and types are dropped), and is typed again once everything it waited on is done. When
//!   nothing is left to run but bodies wait, they wait on each other: `callgraph::stalled` reports their cycles
//!   (`recursion`, or `needs-annotation` for a `let` whose type depends on itself) exactly as for direct
//!   calls, and they are run again callee-first. A member of a reported cycle (`Infer::in_cycle`) is never
//!   waited on: a call of it before it is typed is typed as already wrong.
//! - **A parameter used at two types** (fixed by one use, contradicted by a later one) is `needs-annotation`
//!   at the parameter, with a note naming the conflicting use, instead of `type-mismatch` (spec §5.2); once
//!   per parameter (`Infer::disagree`).
//! - **End of a body** (`Infer::finish`): the deferred operator checks run (`+` on `int`, `float` or `str`,
//!   …, once the operand's type is known); a binding whose type still holds a variable is `needs-annotation`
//!   naming it (no generic functions), then the return type, then any other expression (`print(str([]))`);
//!   each report turns the variable into the error type, so one unknown gives one diagnostic. Then every type
//!   of the body is made final (variables replaced by what they were bound to) and stored in [`Typed`].
//!   A body's diagnostics are its own until then (`Body::diags`), so a body typed again leaves none behind.
//! - **No cascades.** An expression whose error was reported earlier (by any pass) has the error type, which
//!   is compatible with everything and replaces every unknown it meets (`list[_]` against it becomes
//!   `list[{error}]`). `return`, `break`, `continue` and `raise` have the never type: compatible with everything,
//!   binding nothing.
//! - **Returns.** An unannotated function's return type is a variable each `return` unifies. If the end of the
//!   body is reachable, falling off it returns `()`; but a function that also returns a value (anything but a
//!   literal `()`), or is annotated to return one, is `missing-return` at its last statement, and the
//!   fall-off decides nothing about the return type. A function that never returns normally (it always
//!   raises) returns `!`, and a call of it ends a block as `raise` does.
//!
//! - **A receiver must be known.** A field, a method, an index or `in` is looked up by the type of the value
//!   it applies to; if that is still unknown there, it is `needs-annotation` (at the parameter when it is
//!   one).
//!
//! - **Errors** (`errors.rs`): error names and their one data shape, error sets (inferred callee-first,
//!   or pinned), the must-handle rule, `raise`, `?`, `else`, the `error-typo` warning.
//!
//! - **Patterns and `match`** (`patterns.rs`): pattern typing, exhaustiveness, impossible and unreachable
//!   arms.
//!
//! - **References** (`refs.rs`): places, and `ref-escapes`. **Promises, `await` and handlers**
//!   (`promises.rs`): what an `await` fails with (by the promise's origin, decision 21), `await`, unused
//!   promises, handler signatures.
//!
//! - **Dialogue, scene and quest code** (`dialogue.rs`): `Dialogue` and `Exit` values, the compiler's
//!   builtins (`join`, `after`, `play`, …), objectives' commands and reads, and what a dialogue's
//!   synthesised fns must give (`emit` arguments, targets, speakers). Once the program is typed, the Phase 10
//!   checks run over it: `purity.rs`, `query.rs`, `readset.rs` (whose [`Typed::read_sets`] it fills).
//!
//! Files: `expr.rs` (expressions and operators), `stmt.rs` (statements, blocks, reachability), `call.rs`
//! (calls: functions, methods, the prelude, the host), `data.rs` (user types, constructions, fields,
//! collections), `errors.rs` (errors), `patterns.rs` (patterns and `match`), `refs.rs` (references),
//! `promises.rs` (promises, `await`, handlers), `dialogue.rs` (dialogue and quest code); this file holds
//! the driver, unification and the end of a body.

mod call;
mod data;
mod dialogue;
mod errors;
mod expr;
mod patterns;
mod promises;
mod refs;
mod stmt;

pub use call::{BuiltinMethod, Method};
pub use data::Shape;
pub use dialogue::ObjectiveOp;
pub use patterns::{PatCtor, PatInfo};
pub use promises::Await;
pub use refs::{Place, PlaceRoot, PlaceStep};

use crate::ast::{Receiver, Span};
use crate::callgraph;
use crate::diag::Diagnostic;
use crate::hir::{
    self, Builtin, Crate, DefId, DefKind, EventRef, ExprId, LocalId, PatId, TypeName,
};
use crate::ty::{Printer, SetId, Ty, TyId, Types};
use quest_vm::host::HostApi;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A function's final type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sig {
    /// `self` or `&self` and its type (the owner, or a reference to it).
    pub receiver: Option<(Receiver, TyId)>,
    pub params: Vec<TyId>,
    pub ret: TyId,
    /// The error set callers see (sorted names; empty: it cannot fail): the pinned set, or the inferred one.
    pub raises: Vec<String>,
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
    /// What each method call `x.m(…)` calls, by the call's expression; also each host property read
    /// `x.name` (a `Method::Host`), by the field expression.
    pub methods: BTreeMap<ExprId, Method>,
    /// Each host call that names an argument or leaves a parameter out, by the call's expression: for each
    /// parameter in order, the index of the argument that gives it (`None`: its default).
    pub host_args: BTreeMap<ExprId, Vec<Option<u32>>>,
    /// Functions and top-level `let`s, callee-first.
    pub order: Vec<DefId>,
    /// Every error name of the program, sorted (the VM's one `Error` enum: a name's variant is its index
    /// here), with its data fields in order (none for a data-less error).
    pub errors: BTreeMap<String, Vec<(String, TyId)>>,
    /// What each pattern node matches.
    pub patterns: BTreeMap<PatId, PatInfo>,
    /// Every `await`, by its expression: its value's type and what it fails with — exactly a host promise
    /// function's `raises` for a direct call of it (or a local assigned only such calls, never referenced),
    /// else every error any host promise function raises (decision 21; `infer/promises.rs`). A
    /// [`Ty::Promise`] has no error set.
    pub awaits: BTreeMap<ExprId, Await>,
    /// The place of every `&x` (by the `&` expression) and of every receiver passed to a `&self` method as
    /// a reference it is not already (by the receiver expression).
    pub refs: BTreeMap<ExprId, Place>,
    /// Every handler with a valid signature, and its event's type.
    pub handlers: BTreeMap<DefId, TyId>,
    /// The fields of every struct and event (one shape) and enum (one per variant, in order).
    pub adts: BTreeMap<DefId, Vec<Shape>>,
    /// The fields of every host struct, by its index in `HostApi::structs` (the host's field names).
    pub host_structs: Vec<Shape>,
    /// The fields of every host event, by its index in `HostApi::events`.
    pub host_events: Vec<Shape>,
    /// The globals the dialogues and each part read (`readset.rs`).
    pub read_sets: crate::readset::ReadSets,
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
        let mut out = format!("fn({}) -> {}", params.join(", "), p.print(sig.ret));
        if !sig.raises.is_empty() {
            out.push_str(" raises ");
            out.push_str(&sig.raises.join(" | "));
        }
        out
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
        methods: BTreeMap::new(),
        host_args: BTreeMap::new(),
        in_cycle: BTreeSet::new(),
        adts: BTreeMap::new(),
        host_structs: Vec::new(),
        host_events: Vec::new(),
        early_keys: None,
        bad_keys: BTreeSet::new(),
        errors: BTreeMap::new(),
        named: BTreeSet::new(),
        raised: Vec::new(),
        patterns: BTreeMap::new(),
        awaits: BTreeMap::new(),
        refs: BTreeMap::new(),
        handlers: BTreeMap::new(),
        may_await: BTreeSet::new(),
        objectives: BTreeMap::new(),
        emit_arg_tys: BTreeMap::new(),
        diags,
    };
    inf.collect_objectives();
    inf.user_types();
    inf.declared_errors();
    inf.annotated_globals();
    inf.emit_arg_types();
    inf.self_typed_globals(&plan.cycles);
    inf.in_cycle.extend(plan.cycles.iter().flatten());
    let order = inf.run(plan.order);
    inf.dialogue_checks();
    inf.typos();
    // Every script event has a type, emitted or not: code generation registers them all.
    for (i, def) in krate.defs.iter().enumerate() {
        if matches!(def.kind, DefKind::Event(_)) {
            inf.types
                .intern(Ty::Adt(DefId(u32::try_from(i).unwrap_or(u32::MAX))));
        }
    }
    let sets: Vec<Vec<String>> = inf.sigs.values().map(|s| s.raises.clone()).collect();
    let errors = inf.error_table(&sets);
    let mut typed = Typed {
        types: inf.types,
        exprs: inf.exprs,
        locals: inf.locals,
        sigs: inf.sigs,
        globals: inf.globals,
        methods: inf.methods,
        host_args: inf.host_args,
        order,
        errors,
        patterns: inf.patterns,
        awaits: inf.awaits,
        refs: inf.refs,
        handlers: inf.handlers,
        adts: inf.adts,
        host_structs: inf.host_structs,
        host_events: inf.host_events,
        read_sets: crate::readset::ReadSets::default(),
    };
    let mut diags = inf.diags;
    // The Phase 10 checks, over the typed program (spec §5.11–5.12).
    let facts = crate::callgraph::facts(krate, &typed);
    diags.extend(crate::purity::purity_with(krate, &typed, host, &facts));
    diags.extend(crate::query::query_subset(krate, &typed, host));
    let (read_sets, mut mirror) = crate::readset::read_sets_with(krate, &typed, &facts);
    typed.read_sets = read_sets;
    diags.append(&mut mirror);
    (typed, diags)
}

/// An operator's requirement on its operands' type, checked at the end of the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// `int` or `float`: `- * / %`, unary `-`, `abs`, `min`, `max`.
    Numeric,
    /// `int`, `float` or `str`: `+`, and the order comparisons.
    NumericOrStr,
    /// A dict key (spec §5.8): `bad-dict-key`.
    Key,
    /// A value the host stores (`HostTy::Value`): no `Promise`, no reference; `not-storable`.
    Storable,
    /// An event (`HostTy::Event`): a script or host event.
    Event,
    /// Not a reference (`op`: where it goes): `ref-escapes`.
    NoRef,
    /// An expression statement's value: not a promise (`unused-promise`).
    Unused,
    /// The type of a local assigned with `=`: not a reference (`ref-reassign`).
    NotRefLocal,
    /// A place bound to `_`: not a promise (`unused-promise`; a promise is detached where it is created).
    HeldPromise,
}

/// A requirement on a type checked at the end of the body, once inference had every chance to decide it.
/// `op` names what asks (an operator, a function).
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
    /// Whether a `return` carried a value (other than a literal `()`).
    value_returns: bool,
    /// The variable of each unannotated parameter, which only the body's own uses may decide.
    param_vars: BTreeMap<u32, LocalId>,
    /// The parameters already reported as used at two types.
    conflicted: BTreeSet<LocalId>,
    /// This body's diagnostics, merged into the program's when the body is done: a body typed again
    /// leaves nothing behind.
    diags: Vec<Diagnostic>,
    /// The functions and unannotated top-level `let`s this body reached before they were typed, with the
    /// earliest site: the body is stopped and typed again after them.
    waits: BTreeMap<DefId, Span>,
    /// The error names this body raises or propagates, each with its first site.
    raises: BTreeMap<String, Span>,
    /// Error shapes this body fixed (new names, generic fields used), kept when it finishes.
    shapes: BTreeMap<String, errors::ErrShape>,
    /// The error names raised by name here, and where (for `error-typo`).
    raised: Vec<(String, Span)>,
    /// The error names this body's patterns match.
    matched: Vec<String>,
    /// What this body's pattern nodes match, kept when it finishes.
    patterns: Vec<(PatId, PatInfo)>,
    /// A top-level `let`'s initialiser (no `await`).
    in_init: bool,
    /// Each `await`: its value's type and the errors it can fail with.
    awaits: BTreeMap<ExprId, (TyId, SetId)>,
    /// Whether this body awaits, or calls a function that may.
    may_await: bool,
    /// The locals whose every assignment is a direct call of host promise functions raising one set, and
    /// that set (`promises.rs`).
    origins: BTreeMap<LocalId, SetId>,
    /// A synthesised fn computing a condition (`bool`): a wrong return is worded as a condition's.
    cond: bool,
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
            value_returns: false,
            param_vars: BTreeMap::new(),
            conflicted: BTreeSet::new(),
            diags: Vec::new(),
            waits: BTreeMap::new(),
            raises: BTreeMap::new(),
            shapes: BTreeMap::new(),
            raised: Vec::new(),
            matched: Vec::new(),
            patterns: Vec::new(),
            in_init: false,
            awaits: BTreeMap::new(),
            may_await: false,
            origins: BTreeMap::new(),
            cond: false,
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
    methods: BTreeMap<ExprId, Method>,
    host_args: BTreeMap<ExprId, Vec<Option<u32>>>,
    /// The members of every reported cycle: never waited on.
    in_cycle: BTreeSet<DefId>,
    /// The field types of every struct and script event (one shape) and enum (one per variant).
    adts: BTreeMap<DefId, Vec<data::Shape>>,
    /// The field types of every host struct and host event, by index.
    host_structs: Vec<data::Shape>,
    host_events: Vec<data::Shape>,
    /// While the user types are lowered: the key types of their dict annotations, checked once all are.
    early_keys: Option<Vec<(TyId, String, Span)>>,
    /// The key types reported as `bad-dict-key` in the body being typed.
    bad_keys: BTreeSet<TyId>,
    /// Every error name's data shape (see `errors.rs`).
    errors: BTreeMap<String, errors::ErrShape>,
    /// The error names matched in a pattern or declared (set members, pinned names, the host's).
    named: BTreeSet<String>,
    /// Every raise of an error by name: the name, the file, the site.
    raised: Vec<(String, String, Span)>,
    /// What every finished body's pattern nodes match.
    patterns: BTreeMap<PatId, PatInfo>,
    awaits: BTreeMap<ExprId, Await>,
    refs: BTreeMap<ExprId, Place>,
    handlers: BTreeMap<DefId, TyId>,
    /// The functions that may `await` (directly or through what they call).
    may_await: BTreeSet<DefId>,
    /// Every objective's global, with its `count`.
    objectives: BTreeMap<DefId, Option<i64>>,
    /// The return type of each dialogue `emit` argument's synthesised fn: its field's (`emit_arg_types`).
    emit_arg_tys: BTreeMap<DefId, TyId>,
    diags: Vec<Diagnostic>,
}

impl<'k> Infer<'k> {
    // Bodies.

    /// Types every body, starting from the call graph's `order`; returns the order they were finished in.
    fn run(&mut self, order: Vec<DefId>) -> Vec<DefId> {
        let mut done = Vec::with_capacity(order.len());
        let mut queue: VecDeque<DefId> = order.into();
        // A stopped body: what it still waits on. And who waits on each.
        let mut waiting: BTreeMap<DefId, BTreeMap<DefId, Span>> = BTreeMap::new();
        let mut waited_by: BTreeMap<DefId, Vec<DefId>> = BTreeMap::new();
        loop {
            while let Some(d) = queue.pop_front() {
                let waits = match self.krate.def(d).map(|def| &def.kind) {
                    Some(DefKind::Fn(f)) => self.func(d, f),
                    Some(DefKind::Global(g)) => self.global(d, g),
                    _ => continue,
                };
                if !waits.is_empty() {
                    for on in waits.keys() {
                        waited_by.entry(*on).or_default().push(d);
                    }
                    waiting.insert(d, waits);
                    continue;
                }
                done.push(d);
                for user in waited_by.remove(&d).unwrap_or_default() {
                    let Some(rest) = waiting.get_mut(&user) else {
                        continue;
                    };
                    rest.remove(&d);
                    if rest.is_empty() {
                        waiting.remove(&user);
                        queue.push_back(user);
                    }
                }
            }
            if waiting.is_empty() {
                return done;
            }
            // Every waiting body waits on another: they hold cycles.
            let (plan, mut found) = callgraph::stalled(self.krate, &waiting);
            self.diags.append(&mut found);
            self.self_typed_globals(&plan.cycles);
            self.in_cycle.extend(plan.cycles.iter().flatten());
            waiting.clear();
            waited_by.clear();
            queue.extend(plan.order);
        }
    }

    /// The final signature of the function `f`. Without one yet, the body waits for it unless `f` is in a
    /// reported cycle.
    fn sig_of(&mut self, b: &mut Body, f: DefId, at: Span) -> Option<Sig> {
        if let Some(sig) = self.sigs.get(&f) {
            return Some(sig.clone());
        }
        self.wait(b, f, at);
        None
    }

    /// Records that `b` reached `d` (a function or an unannotated top-level `let`) before it was typed.
    fn wait(&mut self, b: &mut Body, d: DefId, at: Span) {
        if self.in_cycle.contains(&d) {
            return;
        }
        let site = b.waits.entry(d).or_insert(at);
        if (at.line, at.col) < (site.line, site.col) {
            *site = at;
        }
    }

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
                let mut found = Vec::new();
                self.bad_keys.clear();
                let ty = self.lower_kept(t, file, &mut found, "kept in a top-level variable");
                self.diags.append(&mut found);
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

    /// Types a function; what it waits on when it had to stop (then nothing of it is kept).
    fn func(&mut self, d: DefId, f: &hir::Fn) -> BTreeMap<DefId, Span> {
        self.bad_keys.clear();
        let file = self.file_of(d);
        let mut b = Body::new(file, TyId::ERROR);
        b.origins = self.promise_origins(&f.body);
        b.cond = matches!(
            f.pure,
            Some(
                hir::PureSite::Cond
                    | hir::PureSite::Filter
                    | hir::PureSite::CastCond
                    | hir::PureSite::Where(_)
                    | hir::PureSite::Requires
                    | hir::PureSite::Transition(_)
            )
        );
        b.ret = match (&f.ret, self.emit_arg_tys.get(&d)) {
            (Some(t), _) => self.lower_kept(t, file, &mut b.diags, "returned"),
            // A dialogue's `emit` argument gives its field's type (its author cannot annotate it).
            (None, Some(&t)) => t,
            (None, None) => self.fresh(),
        };
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
        let event = f.handler.map(|e| self.event_ty(e));
        let mut params = Vec::with_capacity(f.params.len());
        for (i, p) in f.params.iter().enumerate() {
            let ty = match (&p.ty, event) {
                (Some(t), Some(event)) if i == 0 => {
                    let ty = self.lower(t, file, &mut b.diags);
                    self.handler_param(&mut b, event, ty, p);
                    ty
                }
                (Some(t), _) => self.lower(t, file, &mut b.diags),
                // A handler's event parameter is typed by its `@on`.
                (None, Some(event)) if i == 0 => event,
                (None, _) => {
                    let var = self.fresh();
                    if let Ty::Var(n) = self.types.get(var) {
                        b.param_vars.insert(n, p.local);
                    }
                    var
                }
            };
            self.set_local(p.local, ty);
            b.bindings.push(p.local);
            params.push(ty);
        }
        let diverges = self.block(&mut b, &f.body);
        if !b.waits.is_empty() {
            return b.waits;
        }
        if !diverges {
            // Falling off the end returns `()`. With a value returned elsewhere it is a missing `return`,
            // and the fall-off decides nothing about the return type.
            let falls_off_to_unit = !b.value_returns && self.unify(b.ret, TyId::UNIT);
            if !falls_off_to_unit {
                let name = self.krate.qualified_name(d);
                let what = if b.value_returns {
                    "a value".to_string()
                } else {
                    format!("`{}`", self.show(b.ret))
                };
                let at = f.body.last().map_or(self.def_span(d), |s| s.span);
                b.diags.push(error(
                    "missing-return",
                    format!(
                        "`{name}` returns {what}, but its end can be reached after this without a `return`"
                    ),
                    file,
                    at,
                ));
            }
        } else if !b.returns
            && let ret = self.shallow(b.ret)
            && let Ty::Var(n) = self.types.get(ret)
        {
            // It never returns normally (it always raises).
            self.bind(n, TyId::NEVER);
        }
        self.finish(&mut b, Some(d));
        let raised = std::mem::take(&mut b.raises);
        let raises = self.fn_raises(&mut b, f.raises.as_ref(), raised);
        let ret = self.zonk(b.ret);
        let receiver = receiver.map(|(r, t)| (r, self.zonk(t)));
        let params = params.into_iter().map(|t| self.zonk(t)).collect();
        if let Some(event) = event {
            self.handler_shape(d, f, event, ret);
        }
        if b.may_await {
            self.may_await.insert(d);
        }
        self.sigs.insert(
            d,
            Sig {
                receiver,
                params,
                ret,
                raises,
            },
        );
        self.diags.append(&mut b.diags);
        BTreeMap::new()
    }

    /// Types a top-level `let`'s initialiser; what it waits on when it had to stop.
    fn global(&mut self, d: DefId, g: &hir::Global) -> BTreeMap<DefId, Span> {
        self.bad_keys.clear();
        if let Some(annotated) = self.globals.get(&d).copied() {
            // Its annotation's bad keys were reported with it.
            self.seen_bad_keys(annotated);
        }
        let file = self.file_of(d);
        let mut b = Body::new(file, TyId::ERROR);
        b.in_init = true;
        let value = self.expr(&mut b, &g.value);
        if !b.waits.is_empty() {
            return b.waits;
        }
        let ty = match self.globals.get(&d) {
            Some(annotated) => {
                let annotated = *annotated;
                self.expect(&mut b, annotated, value, g.value.span);
                annotated
            }
            None => {
                self.no_ref(&mut b, value, g.value.span, "kept in a top-level variable");
                value
            }
        };
        if self.has_var(ty) {
            let name = self.krate.def(d).map_or("", |def| def.name.as_str());
            b.diags.push(error(
                "needs-annotation",
                format!("cannot infer the type of `{name}`: annotate it (`let {name}: T = …`)"),
                file,
                self.def_span(d),
            ));
            self.default_vars(ty);
        }
        self.finish(&mut b, None);
        let ty = self.zonk(ty);
        self.globals.insert(d, ty);
        self.diags.append(&mut b.diags);
        BTreeMap::new()
    }

    /// The end of a body: deferred checks, `needs-annotation` (bindings, then the return type of the
    /// function `ret_of`, then other expressions), final types (patterns' too: a variable left there is the
    /// error type, silently).
    fn finish(&mut self, b: &mut Body, ret_of: Option<DefId>) {
        for c in std::mem::take(&mut b.checks) {
            let ty = self.zonk(c.ty);
            if let Some(d) = self.check(c, ty, b.file) {
                b.diags.push(d);
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
                b.diags.push(d);
                self.default_vars(ty);
            }
            let ty = self.zonk(ty);
            self.set_local(local, ty);
        }
        if let Some(d) = ret_of
            && self.has_var(b.ret)
        {
            let name = self.krate.qualified_name(d);
            b.diags.push(error(
                "needs-annotation",
                format!("cannot infer what `{name}` returns: annotate it (`-> T`)"),
                b.file,
                self.def_span(d),
            ));
            self.default_vars(b.ret);
        }
        for (id, at) in std::mem::take(&mut b.exprs) {
            let ty = self.zonk(self.expr_ty(id));
            if self.has_var(ty) {
                let shown = self.show(ty);
                b.diags.push(error(
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
        self.keep_awaits(b);
        // Errors first: a generic error field a pattern left undecided stays open (`commit_errors`), and is
        // not fixed to the error type by the defaulting below.
        self.commit_errors(b);
        for (id, mut info) in std::mem::take(&mut b.patterns) {
            // A wildcard over an undecided field: no diagnostic, but no variable in `Typed` either.
            if self.has_var(info.ty) {
                self.default_vars(info.ty);
            }
            info.ty = self.zonk(info.ty);
            self.patterns.insert(id, info);
        }
    }

    /// A deferred check of `c.ty` (now `ty`): its diagnostic, if it fails. An unknown type passes (it is
    /// reported as unknown).
    fn check(&mut self, c: Check, ty: TyId, file: &str) -> Option<Diagnostic> {
        let shape = self.types.get(ty);
        if matches!(shape, Ty::Error | Ty::Never | Ty::Var(_)) {
            return None;
        }
        let (code, message) = match c.class {
            Class::Numeric | Class::NumericOrStr => {
                let ok = match shape {
                    Ty::Int | Ty::Float => true,
                    Ty::Str => c.class == Class::NumericOrStr,
                    _ => false,
                };
                if ok {
                    return None;
                }
                let wanted = match c.class {
                    Class::Numeric => "`int` or `float`",
                    _ => "`int`, `float` or `str`",
                };
                let found = self.show(ty);
                (
                    "type-mismatch",
                    format!("`{}` takes {wanted}, not `{found}`", c.op),
                )
            }
            Class::Key => {
                return self.key_error(ty, file.to_string(), c.at);
            }
            Class::Storable => {
                if self.is_storable(ty) {
                    return None;
                }
                let found = self.show(ty);
                // Its unknown parts (`Promise[_]`) are not asked about again.
                self.default_vars(ty);
                (
                    "not-storable",
                    format!(
                        "`{}` stores its value, and a `{found}` cannot be stored: it holds a `Promise` or a reference",
                        c.op
                    ),
                )
            }
            Class::Event => {
                let is_event = match shape {
                    Ty::HostEvent(_) => true,
                    Ty::Adt(d) => matches!(
                        self.krate.def(d).map(|def| &def.kind),
                        Some(DefKind::Event(_))
                    ),
                    _ => false,
                };
                if is_event {
                    return None;
                }
                let found = self.show(ty);
                (
                    "type-mismatch",
                    format!("`{}` takes an event, not `{found}`", c.op),
                )
            }
            Class::NoRef => {
                if !matches!(shape, Ty::Ref(_)) {
                    return None;
                }
                return Some(refs::escapes(c.op, file, c.at));
            }
            Class::NotRefLocal => {
                if !matches!(shape, Ty::Ref(_)) {
                    return None;
                }
                (
                    "ref-reassign",
                    "a reference cannot be reassigned; bind a new one with `let`".to_string(),
                )
            }
            Class::HeldPromise => {
                if !matches!(shape, Ty::Promise(_)) {
                    return None;
                }
                let mut d = error(
                    "unused-promise",
                    "a promise already held cannot be detached: `await` it".to_string(),
                    file,
                    c.at,
                );
                d.notes.push(
                    "detach a promise where it is created: `let _ = world.set(…)`".to_string(),
                );
                return Some(d);
            }
            Class::Unused => {
                if !matches!(shape, Ty::Promise(_)) {
                    return None;
                }
                (
                    "unused-promise",
                    "this promise is dropped: `await` it, or keep it with `let _ = …` to let it run detached"
                        .to_string(),
                )
            }
        };
        Some(error(code, message, file, c.at))
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

    /// `ty` with its outer variables followed. Every variable passed on the way is pointed straight at the
    /// end (path compression: a binding is only rewritten to what it already means).
    fn shallow(&mut self, ty: TyId) -> TyId {
        let mut end = ty;
        let mut passed = Vec::new();
        // Bindings never form a cycle (occurs check); the bound is a guard.
        for _ in 0..=self.subst.len() {
            crate::work::add_type_visit();
            let Ty::Var(n) = self.types.get(end) else {
                break;
            };
            match self.subst.get(n as usize).copied().flatten() {
                Some(next) => {
                    passed.push(n);
                    end = next;
                }
                None => break,
            }
        }
        if passed.len() > 1 {
            for n in passed {
                if let Some(slot) = self.subst.get_mut(n as usize) {
                    *slot = Some(end);
                }
            }
        }
        end
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
                // The same head (constructor and any data that is not a child type: a fallible result's
                // error set) with children: unify the children.
                (x, y) if x.with_children(&[]) == y.with_children(&[]) => {
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
    fn vars_in(&mut self, ty: TyId) -> BTreeSet<u32> {
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut stack = vec![ty];
        while let Some(t) = stack.pop() {
            crate::work::add_type_visit();
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

    fn has_var(&mut self, ty: TyId) -> bool {
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
            crate::work::add_type_visit();
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
    fn lower(&mut self, t: &hir::Type, file: &str, out: &mut Vec<Diagnostic>) -> TyId {
        match &t.kind {
            hir::TypeKind::Error => TyId::ERROR,
            hir::TypeKind::Unit => TyId::UNIT,
            hir::TypeKind::Ref(inner) => {
                let inner = self.lower_kept(inner, file, out, "referred to by a reference");
                self.types.intern(Ty::Ref(inner))
            }
            hir::TypeKind::Named(name, args) => {
                let args: Vec<TyId> = args
                    .iter()
                    .map(|a| self.lower_kept(a, file, out, "inside another type"))
                    .collect();
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
                        Builtin::Dialogue => (0, Ty::Dialogue),
                        Builtin::Exit => (0, Ty::Exit),
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
                    out.push(error(
                        "type-mismatch",
                        format!("`{head}` takes {takes}, {} given", args.len()),
                        file,
                        t.span,
                    ));
                    return TyId::ERROR;
                }
                if let (Ty::Dict(_, _), Some(key), Some(k)) =
                    (shape, args.first(), arg_spans(t).first())
                {
                    self.annotated_key(*key, file, *k, out);
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
                .map_or(String::new(), |e| e.name.clone()),
        }
    }

    /// Unifies `found` with `expected`, else reports the disagreement at `at`; true when they agree.
    fn expect(&mut self, b: &mut Body, expected: TyId, found: TyId, at: Span) -> bool {
        if self.unify(expected, found) {
            return true;
        }
        let (e, f) = (self.show(expected), self.show(found));
        self.disagree(
            b,
            expected,
            found,
            at,
            format!("expected `{e}`, found `{f}`"),
        );
        false
    }

    /// Reports a failed unification of `x` and `y` at `at`. When the type of an unannotated parameter,
    /// fixed by an earlier use, is part of the conflict, it is `needs-annotation` at that parameter (spec
    /// §5.2: a parameter used at two incompatible types asks for an annotation), once per parameter;
    /// otherwise `type-mismatch` with `message`.
    fn disagree(&mut self, b: &mut Body, x: TyId, y: TyId, at: Span, message: String) {
        for (side, other) in [(x, y), (y, x)] {
            let Some(local) = self.param_in(b, side) else {
                continue;
            };
            if b.conflicted.insert(local) {
                let (name, span) = self
                    .krate
                    .local(local)
                    .map_or((String::new(), Span::default()), |l| {
                        (l.name.clone(), l.span)
                    });
                let (was, now) = (self.show(side), self.show(other));
                let mut d = error(
                    "needs-annotation",
                    format!(
                        "`{name}` is used as `{was}` and as `{now}`: annotate it (`{name}: T`)"
                    ),
                    b.file,
                    span,
                );
                d.notes.push(format!(
                    "the use at line {}, column {} needs `{now}`",
                    at.line, at.col
                ));
                b.diags.push(d);
            }
            return;
        }
        self.mismatch(b, at, message);
    }

    /// The first (by id) unannotated parameter whose variable `ty` passes through.
    fn param_in(&self, b: &Body, ty: TyId) -> Option<LocalId> {
        let mut found: Option<LocalId> = None;
        let mut seen = BTreeSet::new();
        let mut stack = vec![ty];
        while let Some(t) = stack.pop() {
            if self.types.is_ground(t) || !seen.insert(t) {
                continue;
            }
            match self.types.get(t) {
                Ty::Var(n) => {
                    if let Some(l) = b.param_vars.get(&n) {
                        found = Some(found.map_or(*l, |f| f.min(*l)));
                    }
                    if let Some(next) = self.subst.get(n as usize).copied().flatten() {
                        stack.push(next);
                    }
                }
                other => stack.extend(other.children()),
            }
        }
        found
    }

    fn mismatch(&mut self, b: &mut Body, at: Span, message: String) {
        b.diags.push(error("type-mismatch", message, b.file, at));
    }
}

/// The spans of a named type's arguments.
fn arg_spans(t: &hir::Type) -> Vec<Span> {
    match &t.kind {
        hir::TypeKind::Named(_, args) => args.iter().map(|a| a.span).collect(),
        _ => Vec::new(),
    }
}

fn error(code: &'static str, message: String, file: &str, at: Span) -> Diagnostic {
    Diagnostic::error(code, message, file, at.line, at.col, at.len)
}
