//! Layout (plan D10 stage `layout`): which functions the program has and their ids, the order the
//! top-level `let`s initialise in, and `main`.
//!
//! - **Function ids** are the functions sorted by qualified name (methods included: `npc::Ledger::total`).
//!   Each top-level `let` has an initialiser function of its own, named `$init::<qualified name>`. The id
//!   order is the declaration order in the `ProgramBuilder`, so frames are laid out in it from slot 0.
//! - **Initialisation order** (spec §5.1): modules in dependency order, then each module's `let`s in
//!   declaration order. A module depends on the modules whose items its code names (a `use`d function,
//!   global, type or variant, or a method it calls). Dependencies come first; modules are visited by path
//!   (the root first), and a cycle of modules (allowed, spec §5.10) is broken where the walk meets it.
//! - **`main`** is the root module's free function `main`.
//! - **A quest's generated fns** (`codegen/quest.rs`): `$enter::Q::<Stage>` for each stage, `$start::Q`,
//!   `$transitions::Q`, and `$role_lost::Q` (the default `RoleLost` handler) when the quest has a required
//!   role and no `RoleLost` handler of its own. They sort with the others by name.

use crate::hir::{
    Crate, DefId, DefKind, EventRef, Expr, ExprKind, FStrPart, ModId, Quest, StageIdx, Stmt,
    StmtKind,
};
use crate::infer::{Method, Typed};
use std::collections::BTreeSet;

/// A function of the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FnKind {
    /// A function or a method.
    Def(DefId),
    /// The initialiser of a top-level `let`.
    Init(DefId),
    /// `$enter::Q::<Stage>` of the quest.
    Enter(DefId, StageIdx),
    /// `$start::Q`.
    Start(DefId),
    /// `$transitions::Q`.
    Transitions(DefId),
    /// `$role_lost::Q`: the default `RoleLost` handler.
    RoleLost(DefId),
}

#[derive(Debug, Clone)]
pub(crate) struct FnItem {
    pub name: String,
    pub kind: FnKind,
}

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    /// Every function, in fn-id order.
    pub fns: Vec<FnItem>,
    /// The top-level `let`s, in initialisation order.
    pub globals: Vec<DefId>,
    pub main: Option<DefId>,
}

pub(crate) fn plan(krate: &Crate, typed: &Typed) -> Layout {
    let mut fns: Vec<FnItem> = Vec::new();
    for (i, def) in krate.defs.iter().enumerate() {
        let id = DefId(u32::try_from(i).unwrap_or(u32::MAX));
        match def.kind {
            DefKind::Fn(_) => fns.push(FnItem {
                name: krate.qualified_name(id),
                kind: FnKind::Def(id),
            }),
            DefKind::Global(_) => fns.push(FnItem {
                name: format!("$init::{}", krate.qualified_name(id)),
                kind: FnKind::Init(id),
            }),
            DefKind::Quest(ref q) => {
                let name = krate.qualified_name(id);
                for (i, st) in q.stages.iter().enumerate() {
                    fns.push(FnItem {
                        name: format!("$enter::{name}::{}", st.name),
                        kind: FnKind::Enter(id, StageIdx(u32::try_from(i).unwrap_or(u32::MAX))),
                    });
                }
                fns.push(FnItem {
                    name: format!("$start::{name}"),
                    kind: FnKind::Start(id),
                });
                fns.push(FnItem {
                    name: format!("$transitions::{name}"),
                    kind: FnKind::Transitions(id),
                });
                if needs_role_lost(krate, q) {
                    fns.push(FnItem {
                        name: format!("$role_lost::{name}"),
                        kind: FnKind::RoleLost(id),
                    });
                }
            }
            _ => {}
        }
    }
    // Qualified names are unique; the id breaks a tie anyway.
    fns.sort_by(|a, b| (&a.name, def_of(a.kind)).cmp(&(&b.name, def_of(b.kind))));

    let globals = module_order(krate, typed)
        .into_iter()
        .filter_map(|m| krate.module(m))
        .flat_map(|m| m.defs.iter().copied())
        .filter(|d| matches!(krate.def(*d).map(|d| &d.kind), Some(DefKind::Global(_))))
        .collect();

    let main = krate.modules.first().and_then(|root| {
        root.defs.iter().copied().find(|d| {
            krate.def(*d).is_some_and(|def| {
                def.name == "main" && matches!(&def.kind, DefKind::Fn(f) if f.owner.is_none())
            })
        })
    });
    Layout { fns, globals, main }
}

/// The item a function belongs to: its own, or (a generated one) the quest's.
pub(crate) fn def_of(k: FnKind) -> DefId {
    match k {
        FnKind::Def(d)
        | FnKind::Init(d)
        | FnKind::Enter(d, _)
        | FnKind::Start(d)
        | FnKind::Transitions(d)
        | FnKind::RoleLost(d) => d,
    }
}

/// Whether the quest gets the default `RoleLost` handler: it has a required role and no handler of its
/// own for `RoleLost` (in the body, a stage or a part).
pub(crate) fn needs_role_lost(krate: &Crate, q: &Quest) -> bool {
    let lost = EventRef::Script(q.generated.role_lost);
    let handles = |d: &DefId| matches!(krate.def(*d).map(|d| &d.kind), Some(DefKind::Fn(f)) if f.handler == Some(lost));
    let own = q.handlers.iter().any(handles)
        || q.stages.iter().flat_map(|s| &s.handlers).any(handles)
        || q.parts.iter().flat_map(|p| &p.handlers).any(handles);
    q.roles.iter().any(|r| !r.optional) && !own
}

/// Every module, each after the modules it depends on (a depth-first walk in path order, children of a
/// node in path order; an edge back into the walk is skipped).
fn module_order(krate: &Crate, typed: &Typed) -> Vec<ModId> {
    let n = krate.modules.len();
    let deps: Vec<Vec<usize>> = (0..n).map(|m| depends_on(krate, typed, m)).collect();
    // 0 unseen, 1 on the walk, 2 done
    let mut state = vec![0u8; n];
    let mut out = Vec::with_capacity(n);
    for root in 0..n {
        if state[root] != 0 {
            continue;
        }
        state[root] = 1;
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&mut (m, ref mut next)) = stack.last_mut() {
            if let Some(&d) = deps[m].get(*next) {
                *next += 1;
                if state[d] == 0 {
                    state[d] = 1;
                    stack.push((d, 0));
                }
            } else {
                state[m] = 2;
                out.push(ModId(u32::try_from(m).unwrap_or(u32::MAX)));
                stack.pop();
            }
        }
    }
    out
}

/// The other modules whose items module `m`'s code names, sorted.
fn depends_on(krate: &Crate, typed: &Typed, m: usize) -> Vec<usize> {
    let Some(module) = krate.modules.get(m) else {
        return Vec::new();
    };
    let mut named: BTreeSet<DefId> = BTreeSet::new();
    let mut note = |e: &Expr| match &e.kind {
        ExprKind::Def(d) | ExprKind::Variant(d, _) => {
            named.insert(*d);
        }
        ExprKind::Call(_, _) => {
            if let Some(Method::Script(d)) = typed.methods.get(&e.id) {
                named.insert(*d);
            }
        }
        _ => {}
    };
    for d in &module.defs {
        match krate.def(*d).map(|d| &d.kind) {
            Some(DefKind::Fn(f)) => each_expr_in(&f.body, &mut note),
            Some(DefKind::Global(g)) => each_expr(&g.value, &mut note),
            _ => {}
        }
    }
    let mut out: Vec<usize> = named
        .into_iter()
        .filter_map(|d| krate.def(d))
        .map(|def| def.module.0 as usize)
        .filter(|&dm| dm != m && dm < krate.modules.len())
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Calls `f` on every expression of the statements, outer before inner.
pub(crate) fn each_expr_in(body: &[Stmt], f: &mut dyn FnMut(&Expr)) {
    for s in body {
        match &s.kind {
            StmtKind::Let(_, _, e) | StmtKind::Expr(e) | StmtKind::Raise(e) => each_expr(e, f),
            StmtKind::Assign(place, value) => {
                each_expr(place, f);
                each_expr(value, f);
            }
            StmtKind::If(branches, other) => {
                for (c, b) in branches {
                    each_expr(c, f);
                    each_expr_in(b, f);
                }
                if let Some(b) = other {
                    each_expr_in(b, f);
                }
            }
            StmtKind::While(c, b) | StmtKind::For(_, c, b) => {
                each_expr(c, f);
                each_expr_in(b, f);
            }
            StmtKind::Match(e, cases) => {
                each_expr(e, f);
                for c in cases {
                    if let Some(g) = &c.guard {
                        each_expr(g, f);
                    }
                    each_expr_in(&c.body, f);
                }
            }
            StmtKind::Return(Some(e)) => each_expr(e, f),
            StmtKind::Return(None)
            | StmtKind::Pass
            | StmtKind::Break
            | StmtKind::Continue
            | StmtKind::Stage(_) => {}
        }
    }
}

/// Calls `f` on `e` and every expression inside it, outer before inner. The recursion is bounded by the
/// parser's nesting limit.
pub(crate) fn each_expr(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::Call(callee, args) => {
            each_expr(callee, f);
            for a in args {
                each_expr(&a.value, f);
            }
        }
        ExprKind::FStr(parts) => {
            for p in parts {
                if let FStrPart::Expr(x) = p {
                    each_expr(x, f);
                }
            }
        }
        ExprKind::List(items) => {
            for x in items {
                each_expr(x, f);
            }
        }
        ExprKind::Dict(entries) => {
            for (k, v) in entries {
                each_expr(k, f);
                each_expr(v, f);
            }
        }
        ExprKind::Unary(_, x)
        | ExprKind::Await(x)
        | ExprKind::Try(x)
        | ExprKind::Field(x, _)
        | ExprKind::Raise(x)
        | ExprKind::Return(Some(x)) => each_expr(x, f),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Else(a, b) => {
            each_expr(a, f);
            each_expr(b, f);
        }
        ExprKind::Slice(x, a, b) => {
            each_expr(x, f);
            for y in [a, b].into_iter().flatten() {
                each_expr(y, f);
            }
        }
        ExprKind::FallbackBlock(_, body) => each_expr_in(body, f),
        ExprKind::Error
        | ExprKind::Unit
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::Local(_)
        | ExprKind::Def(_)
        | ExprKind::Variant(_, _)
        | ExprKind::Host(_)
        | ExprKind::Prelude(_)
        | ExprKind::ErrorName(_)
        | ExprKind::Return(None)
        | ExprKind::Break
        | ExprKind::Continue
        | ExprKind::Quest(_)
        | ExprKind::Exit(_, _) => {}
    }
}
