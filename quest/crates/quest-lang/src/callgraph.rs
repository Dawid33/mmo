//! The call graph (spec §5.2, §6.1; plan D10 stage `callgraph`): recursion is rejected, and inference gets
//! its order.
//!
//! Nodes are the functions and methods (handlers and `main` included) and the top-level `let`s. Edges:
//! - **call**: a function or an initialiser calls a function (`f(…)`, `S::make(…)`);
//! - **read**: a function or an initialiser uses a top-level `let` that has no type annotation, whose type
//!   therefore comes from its initialiser.
//!
//! **Recursion** is a cycle of call edges (only calls: recursive data is fine, decision 17). Each strongly
//! connected set of functions is reported once, as `recursive call cycle: a -> b -> a`: the cycle starts at
//! the set's smallest qualified name, follows the shortest way back to it, and the diagnostic stands at that
//! first function's call to the next one.
//!
//! **Order** is callee-first (reverse topological over both kinds of edges), ties by qualified name: every
//! function comes after everything it calls, every initialiser after what it calls, and every user of an
//! unannotated `let` after that `let`. A cycle cannot be ordered: its members come together, by name, and are
//! listed in [`Plan::cycles`] so inference can treat what it reaches too early as already wrong.
//!
//! A method call written `x.m()` is resolved by `x`'s type, during inference; the graph sees only calls whose
//! callee resolution already named. Inference stops a body that reaches a function or `let` not typed yet and
//! retries it after; the bodies left waiting on each other are a cycle, reported by [`stalled`] the same way.
//!
//! Every walk here is a loop (Tarjan's algorithm with an explicit stack), so a long call chain cannot
//! overflow the native stack; the HIR walk is structural and bounded by the parser's depth limit.
//!
//! **The typed call graph** ([`facts`]), for the Phase 10 checks once inference is done: every body's
//! calls (method calls too, by `Typed::methods`), global reads and effects (writes of globals, host
//! calls, compiler builtins, objective commands, `await`, `-> Stage`), and [`search`] / [`reach`] over it.

use crate::ast::Span;
use crate::diag::Diagnostic;
use crate::hir::{
    Crate, DefId, DefKind, Expr, ExprKind, FStrPart, HostItem, QuestFn, Stmt, StmtKind,
};
use crate::infer::{BuiltinMethod, Method, ObjectiveOp, PlaceRoot, Typed};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// What inference needs from the graph.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Every function and top-level `let`, callee-first.
    pub order: Vec<DefId>,
    /// The members of each cycle (recursion, or a `let` whose type depends on itself), sorted.
    pub cycles: Vec<Vec<DefId>>,
}

#[derive(Debug, Clone, Copy)]
struct Edge {
    to: usize,
    /// The earliest place the edge is made.
    at: Span,
    call: bool,
}

struct Graph<'k> {
    krate: &'k Crate,
    /// Sorted by key (qualified name, then id).
    nodes: Vec<DefId>,
    index: BTreeMap<DefId, usize>,
    keys: Vec<String>,
    /// Out of each node, sorted by target.
    edges: Vec<Vec<Edge>>,
}

/// Builds the graph, reports recursion and returns the order.
pub fn plan(krate: &Crate) -> (Plan, Vec<Diagnostic>) {
    Graph::new(krate).plan()
}

/// The same for the bodies inference had to stop: each waits on the functions and unannotated top-level
/// `let`s in its map (with the earliest site), which inference reached before they were typed — a method
/// call `x.m()` this graph could not see. Recursion among them is reported as for direct calls; `order` is
/// callee-first over them, and `cycles` lists every cycle (of calls, or through a `let`).
pub fn stalled(
    krate: &Crate,
    waits: &BTreeMap<DefId, BTreeMap<DefId, Span>>,
) -> (Plan, Vec<Diagnostic>) {
    let mut g = Graph::with_nodes(krate, waits.keys().copied().collect());
    g.edges = g
        .nodes
        .iter()
        .map(|d| {
            let mut out: Vec<Edge> = waits
                .get(d)
                .into_iter()
                .flatten()
                .filter_map(|(to, at)| {
                    let call = matches!(krate.def(*to).map(|t| &t.kind), Some(DefKind::Fn(_)));
                    g.index.get(to).map(|&to| Edge { to, at: *at, call })
                })
                .collect();
            out.sort_by_key(|e| e.to);
            out
        })
        .collect();
    g.plan()
}

impl Graph<'_> {
    fn plan(&self) -> (Plan, Vec<Diagnostic>) {
        let g = self;
        let mut diags = Vec::new();
        for comp in sccs(g.nodes.len(), |n| g.successors(n, true)) {
            if g.is_cycle(&comp, true) {
                diags.push(g.report(&comp));
            }
        }
        let comps = sccs(g.nodes.len(), |n| g.successors(n, false));
        let cycles = comps
            .iter()
            .filter(|c| g.is_cycle(c, false))
            .map(|c| c.iter().map(|n| g.nodes[*n]).collect())
            .collect();
        let order = g.order(&comps);
        (Plan { order, cycles }, diags)
    }
}

/// The strongly connected components of a graph of `n` nodes, each sorted, in reverse topological order
/// (a component before any that reaches it). Tarjan's algorithm, iterative.
fn sccs(n: usize, successors: impl Fn(usize) -> Vec<usize>) -> Vec<Vec<usize>> {
    const UNSEEN: usize = usize::MAX;
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    let mut next = 0usize;
    for root in 0..n {
        if index[root] != UNSEEN {
            continue;
        }
        // (node, its successors, how many are done)
        let mut work: Vec<(usize, Vec<usize>, usize)> = Vec::new();
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        work.push((root, successors(root), 0));
        while let Some((v, succ, done)) = work.last_mut() {
            let v = *v;
            if let Some(&w) = succ.get(*done) {
                *done += 1;
                if index[w] == UNSEEN {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, successors(w), 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some((parent, _, _)) = work.last() {
                low[*parent] = low[*parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut comp = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                comp.sort_unstable();
                out.push(comp);
            }
        }
    }
    out
}

impl<'k> Graph<'k> {
    fn new(krate: &'k Crate) -> Graph<'k> {
        let defs = krate
            .defs
            .iter()
            .enumerate()
            .filter(|(_, d)| matches!(d.kind, DefKind::Fn(_) | DefKind::Global(_)))
            .map(|(i, _)| DefId(u32::try_from(i).unwrap_or(u32::MAX)))
            .collect();
        let mut g = Graph::with_nodes(krate, defs);
        g.edges = g.nodes.iter().map(|d| g.edges_of(*d)).collect();
        g
    }

    /// A graph of `defs` without edges.
    fn with_nodes(krate: &'k Crate, defs: Vec<DefId>) -> Graph<'k> {
        let mut keyed: Vec<(String, DefId)> = defs
            .into_iter()
            .map(|id| (krate.qualified_name(id), id))
            .collect();
        keyed.sort();
        let nodes: Vec<DefId> = keyed.iter().map(|(_, id)| *id).collect();
        let keys = keyed.into_iter().map(|(k, _)| k).collect();
        let index = nodes.iter().enumerate().map(|(i, d)| (*d, i)).collect();
        Graph {
            krate,
            nodes,
            index,
            keys,
            edges: Vec::new(),
        }
    }

    fn successors(&self, n: usize, calls_only: bool) -> Vec<usize> {
        self.edges
            .get(n)
            .into_iter()
            .flatten()
            .filter(|e| e.call || !calls_only)
            .map(|e| e.to)
            .collect()
    }

    /// Whether a component holds a cycle: more than one node, or one with an edge to itself.
    fn is_cycle(&self, comp: &[usize], calls_only: bool) -> bool {
        match comp {
            [one] => self.successors(*one, calls_only).contains(one),
            _ => comp.len() > 1,
        }
    }

    /// The edges out of an item, one per target (the earliest site).
    fn edges_of(&self, d: DefId) -> Vec<Edge> {
        let mut found: BTreeMap<usize, Edge> = BTreeMap::new();
        let mut add = |to: DefId, at: Span, call: bool| {
            let Some(&to) = self.index.get(&to) else {
                return;
            };
            let e = found.entry(to).or_insert(Edge { to, at, call });
            if (at.line, at.col) < (e.at.line, e.at.col) {
                e.at = at;
            }
        };
        let mut walk = Walk {
            krate: self.krate,
            add: &mut add,
        };
        match self.krate.def(d).map(|d| &d.kind) {
            Some(DefKind::Fn(f)) => walk.stmts(&f.body),
            Some(DefKind::Global(g)) => walk.expr(&g.value),
            _ => {}
        }
        found.into_values().collect()
    }

    /// `recursive call cycle: a -> b -> a` for a component of the call graph.
    fn report(&self, comp: &[usize]) -> Diagnostic {
        // Nodes are sorted by key, so the component's first node has the smallest name.
        let start = comp.first().copied().unwrap_or(0);
        let path = self.shortest_cycle(start, comp);
        let mut names: Vec<&str> = path.iter().map(|n| self.key(*n)).collect();
        names.push(self.key(start));
        let next = path.get(1).copied().unwrap_or(start);
        let at = self
            .edges
            .get(start)
            .and_then(|es| es.iter().find(|e| e.to == next && e.call))
            .map_or(Span::default(), |e| e.at);
        let file = self
            .nodes
            .get(start)
            .and_then(|d| self.krate.def(*d))
            .and_then(|d| self.krate.module(d.module))
            .map_or("", |m| m.file.as_str());
        Diagnostic::error(
            "recursion",
            format!("recursive call cycle: {}", names.join(" -> ")),
            file,
            at.line,
            at.col,
            at.len,
        )
    }

    /// The nodes of the shortest call cycle through `start` inside `comp`, from `start` (breadth-first,
    /// successors by name).
    fn shortest_cycle(&self, start: usize, comp: &[usize]) -> Vec<usize> {
        let inside: BTreeSet<usize> = comp.iter().copied().collect();
        let mut parent: BTreeMap<usize, usize> = BTreeMap::new();
        let mut queue = VecDeque::from([start]);
        let mut last = None;
        'search: while let Some(v) = queue.pop_front() {
            for w in self.successors(v, true) {
                if w == start {
                    last = Some(v);
                    break 'search;
                }
                if inside.contains(&w) && !parent.contains_key(&w) {
                    parent.insert(w, v);
                    queue.push_back(w);
                }
            }
        }
        let mut path = Vec::new();
        let mut cur = last;
        while let Some(v) = cur {
            path.push(v);
            if v == start {
                break;
            }
            cur = parent.get(&v).copied();
        }
        path.reverse();
        path
    }

    fn key(&self, n: usize) -> &str {
        self.keys.get(n).map_or("", String::as_str)
    }

    /// Callee-first over the components: a component once every one it reaches is placed; among those
    /// ready, the one with the smallest name first.
    fn order(&self, comps: &[Vec<usize>]) -> Vec<DefId> {
        let mut comp_of = vec![0usize; self.nodes.len()];
        for (c, members) in comps.iter().enumerate() {
            for n in members {
                comp_of[*n] = c;
            }
        }
        let mut waits_on: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); comps.len()];
        let mut waited_by: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); comps.len()];
        for (n, es) in self.edges.iter().enumerate() {
            for e in es {
                let (from, to) = (comp_of[n], comp_of[e.to]);
                if from != to {
                    waits_on[from].insert(to);
                    waited_by[to].insert(from);
                }
            }
        }
        // A component's members are sorted by key, so its first is its smallest.
        let first = |c: usize| comps[c].first().copied().unwrap_or(0);
        let mut ready: BTreeSet<(usize, usize)> = (0..comps.len())
            .filter(|c| waits_on[*c].is_empty())
            .map(|c| (first(c), c))
            .collect();
        let mut out = Vec::with_capacity(self.nodes.len());
        while let Some((_, c)) = ready.pop_first() {
            out.extend(comps[c].iter().map(|n| self.nodes[*n]));
            for user in std::mem::take(&mut waited_by[c]) {
                waits_on[user].remove(&c);
                if waits_on[user].is_empty() {
                    ready.insert((first(user), user));
                }
            }
        }
        out
    }
}

/// Collects the edges of one body.
struct Walk<'a, 'f> {
    krate: &'a Crate,
    add: &'f mut dyn FnMut(DefId, Span, bool),
}

impl Walk<'_, '_> {
    fn stmts(&mut self, body: &[Stmt]) {
        for s in body {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let(_, _, e) | StmtKind::Expr(e) | StmtKind::Raise(e) => self.expr(e),
            StmtKind::Assign(place, value) => {
                self.expr(place);
                self.expr(value);
            }
            StmtKind::If(branches, other) => {
                for (c, b) in branches {
                    self.expr(c);
                    self.stmts(b);
                }
                if let Some(b) = other {
                    self.stmts(b);
                }
            }
            StmtKind::While(c, b) | StmtKind::For(_, c, b) => {
                self.expr(c);
                self.stmts(b);
            }
            StmtKind::Match(e, cases) => {
                self.expr(e);
                for c in cases {
                    if let Some(g) = &c.guard {
                        self.expr(g);
                    }
                    self.stmts(&c.body);
                }
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            StmtKind::Pass | StmtKind::Break | StmtKind::Continue | StmtKind::Stage(_) => {}
        }
    }

    fn is_fn(&self, d: DefId) -> bool {
        matches!(self.krate.def(d).map(|d| &d.kind), Some(DefKind::Fn(_)))
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Call(callee, args) => {
                match &callee.kind {
                    ExprKind::Def(d) if self.is_fn(*d) => (self.add)(*d, e.span, true),
                    _ => self.expr(callee),
                }
                for a in args {
                    self.expr(&a.value);
                }
            }
            ExprKind::Def(d) => {
                if let Some(DefKind::Global(g)) = self.krate.def(*d).map(|d| &d.kind)
                    && g.ty.is_none()
                {
                    (self.add)(*d, e.span, false);
                }
            }
            ExprKind::FStr(parts) => {
                for p in parts {
                    if let FStrPart::Expr(x) = p {
                        self.expr(x);
                    }
                }
            }
            ExprKind::List(items) => {
                for x in items {
                    self.expr(x);
                }
            }
            ExprKind::Dict(entries) => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::Unary(_, x)
            | ExprKind::Await(x)
            | ExprKind::Try(x)
            | ExprKind::Field(x, _)
            | ExprKind::Raise(x) => self.expr(x),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Else(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Slice(x, a, b) => {
                self.expr(x);
                for y in [a, b].into_iter().flatten() {
                    self.expr(y);
                }
            }
            ExprKind::Return(x) => {
                if let Some(x) = x {
                    self.expr(x);
                }
            }
            ExprKind::FallbackBlock(_, body) => self.stmts(body),
            ExprKind::Error
            | ExprKind::Unit
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Local(_)
            | ExprKind::Variant(_, _)
            | ExprKind::Host(_)
            | ExprKind::Prelude(_)
            | ExprKind::ErrorName(_)
            | ExprKind::Break
            | ExprKind::Continue
            | ExprKind::Quest(_)
            | ExprKind::Exit(_, _) => {}
        }
    }
}

// The typed call graph (Phase 10 checks: `purity.rs`, `readset.rs`).

/// What a body does that the Phase 10 checks look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Changes a top-level `let` (a quest's or a part's global included): assigns it or a part of it,
    /// changes it in place (`xs.append(…)`), or takes a reference to it (`&g`, a `&self` receiver).
    Write(DefId),
    /// Calls the host fn (any kind: the checks decide by it), or reads its property.
    Host(u32),
    /// Calls a compiler builtin (never `dialogue_depth`, which only reads).
    Quest(QuestFn),
    /// An objective's command.
    Objective(ObjectiveOp),
    Await,
    /// `-> Stage`.
    Stage,
}

/// One body's calls, global reads and effects, each with its site, in source order.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    pub(crate) calls: Vec<(DefId, Span)>,
    pub(crate) reads: Vec<(DefId, Span)>,
    pub(crate) effects: Vec<(Effect, Span)>,
}

/// The facts of every function and method, from the typed program (method calls included, by
/// `Typed::methods`).
pub(crate) fn facts(krate: &Crate, typed: &Typed) -> BTreeMap<DefId, Facts> {
    let mut out = BTreeMap::new();
    for (i, def) in krate.defs.iter().enumerate() {
        if let DefKind::Fn(f) = &def.kind {
            let mut w = FactWalk {
                krate,
                typed,
                facts: Facts::default(),
            };
            w.stmts(&f.body);
            out.insert(DefId(u32::try_from(i).unwrap_or(u32::MAX)), w.facts);
        }
    }
    out
}

/// What [`search`] found: `what`, in the body reached through `chain` (the callees after the start, empty
/// when it is the start's own), and `at`, the site in the start (the effect itself, or the call that leads
/// to it).
#[derive(Debug, Clone)]
pub(crate) struct Hit<T> {
    pub(crate) what: T,
    pub(crate) chain: Vec<DefId>,
    pub(crate) at: Span,
}

/// Breadth-first from `start` along calls (each body's in source order): the first body for which `find`
/// finds something, with the shortest chain to it. A cycle (already reported as recursion) is walked once.
pub(crate) fn search<T>(
    facts: &BTreeMap<DefId, Facts>,
    start: DefId,
    mut find: impl FnMut(DefId, &Facts) -> Option<(T, Span)>,
) -> Option<Hit<T>> {
    // Each reached body: who called it first, and where.
    let mut parent: BTreeMap<DefId, (DefId, Span)> = BTreeMap::new();
    let mut queue = VecDeque::from([start]);
    let mut seen = BTreeSet::from([start]);
    while let Some(d) = queue.pop_front() {
        let Some(f) = facts.get(&d) else { continue };
        if let Some((what, at)) = find(d, f) {
            let mut chain = Vec::new();
            let mut cur = d;
            let mut site = at;
            while cur != start {
                chain.push(cur);
                let Some((p, s)) = parent.get(&cur).copied() else {
                    break;
                };
                cur = p;
                site = s;
            }
            chain.reverse();
            return Some(Hit {
                what,
                chain,
                at: site,
            });
        }
        for (callee, at) in &f.calls {
            if seen.insert(*callee) {
                parent.insert(*callee, (d, *at));
                queue.push_back(*callee);
            }
        }
    }
    None
}

/// Every body reachable from `starts` along calls, the starts included.
pub(crate) fn reach(facts: &BTreeMap<DefId, Facts>, starts: &[DefId]) -> BTreeSet<DefId> {
    let mut seen: BTreeSet<DefId> = starts.iter().copied().collect();
    let mut stack: Vec<DefId> = starts.to_vec();
    while let Some(d) = stack.pop() {
        for (callee, _) in facts.get(&d).map_or(&[][..], |f| f.calls.as_slice()) {
            if seen.insert(*callee) {
                stack.push(*callee);
            }
        }
    }
    seen
}

/// Collects one body's [`Facts`].
struct FactWalk<'a> {
    krate: &'a Crate,
    typed: &'a Typed,
    facts: Facts,
}

impl FactWalk<'_> {
    fn is_global(&self, d: DefId) -> bool {
        matches!(self.krate.def(d).map(|d| &d.kind), Some(DefKind::Global(_)))
    }

    fn effect(&mut self, e: Effect, at: Span) {
        self.facts.effects.push((e, at));
    }

    fn stmts(&mut self, body: &[Stmt]) {
        for s in body {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let(_, _, e) | StmtKind::Expr(e) | StmtKind::Raise(e) => self.expr(e),
            StmtKind::Assign(place, value) => {
                self.place(place);
                self.expr(value);
            }
            StmtKind::If(branches, other) => {
                for (c, b) in branches {
                    self.expr(c);
                    self.stmts(b);
                }
                if let Some(b) = other {
                    self.stmts(b);
                }
            }
            StmtKind::While(c, b) | StmtKind::For(_, c, b) => {
                self.expr(c);
                self.stmts(b);
            }
            StmtKind::Match(e, cases) => {
                self.expr(e);
                for c in cases {
                    if let Some(g) = &c.guard {
                        self.expr(g);
                    }
                    self.stmts(&c.body);
                }
            }
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            StmtKind::Stage(_) => self.effect(Effect::Stage, s.span),
            StmtKind::Pass | StmtKind::Break | StmtKind::Continue => {}
        }
    }

    /// A place that is written: a global at its root is a write; indices and `*r`'s reference are read.
    fn place(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Def(d) if self.is_global(*d) => self.effect(Effect::Write(*d), e.span),
            ExprKind::Field(x, _) => self.place(x),
            ExprKind::Index(x, i) => {
                self.place(x);
                self.expr(i);
            }
            _ => self.expr(e),
        }
    }

    fn expr(&mut self, e: &Expr) {
        if let Some(place) = self.typed.refs.get(&e.id)
            && let PlaceRoot::Global(g) = place.root
        {
            self.effect(Effect::Write(g), e.span);
        }
        match &e.kind {
            ExprKind::Call(callee, args) => {
                self.call(e, callee);
                for a in args {
                    self.expr(&a.value);
                }
            }
            ExprKind::Def(d) => {
                if self.is_global(*d) {
                    self.facts.reads.push((*d, e.span));
                }
            }
            ExprKind::Field(x, _) => {
                if let Some(Method::Host(f)) = self.typed.methods.get(&e.id) {
                    self.effect(Effect::Host(*f), e.span);
                }
                self.expr(x);
            }
            ExprKind::Await(x) => {
                self.effect(Effect::Await, e.span);
                self.expr(x);
            }
            ExprKind::FStr(parts) => {
                for p in parts {
                    if let FStrPart::Expr(x) = p {
                        self.expr(x);
                    }
                }
            }
            ExprKind::List(items) => {
                for x in items {
                    self.expr(x);
                }
            }
            ExprKind::Dict(entries) => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::Unary(_, x) | ExprKind::Try(x) | ExprKind::Raise(x) => self.expr(x),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Else(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Slice(x, a, b) => {
                self.expr(x);
                for y in [a, b].into_iter().flatten() {
                    self.expr(y);
                }
            }
            ExprKind::Return(x) => {
                if let Some(x) = x {
                    self.expr(x);
                }
            }
            ExprKind::FallbackBlock(_, body) => self.stmts(body),
            ExprKind::Error
            | ExprKind::Unit
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Local(_)
            | ExprKind::Variant(_, _)
            | ExprKind::Host(_)
            | ExprKind::Prelude(_)
            | ExprKind::ErrorName(_)
            | ExprKind::Break
            | ExprKind::Continue
            | ExprKind::Quest(_)
            | ExprKind::Exit(_, _) => {}
        }
    }

    /// The callee of the call `e`.
    fn call(&mut self, e: &Expr, callee: &Expr) {
        match &callee.kind {
            ExprKind::Def(d)
                if matches!(self.krate.def(*d).map(|d| &d.kind), Some(DefKind::Fn(_))) =>
            {
                self.facts.calls.push((*d, e.span));
            }
            ExprKind::Host(HostItem::Fn(f)) => self.effect(Effect::Host(*f), e.span),
            ExprKind::Quest(QuestFn::DialogueDepth) => {}
            ExprKind::Quest(f) => self.effect(Effect::Quest(*f), e.span),
            ExprKind::Field(recv, _) => {
                match self.typed.methods.get(&e.id) {
                    Some(Method::Script(m)) => self.facts.calls.push((*m, e.span)),
                    Some(Method::Host(f)) => self.effect(Effect::Host(*f), e.span),
                    Some(Method::Objective(op)) if op.is_command() => {
                        self.effect(Effect::Objective(*op), e.span);
                    }
                    Some(Method::Builtin(
                        BuiltinMethod::Append
                        | BuiltinMethod::Pop
                        | BuiltinMethod::Insert
                        | BuiltinMethod::Remove,
                    )) => {
                        // Changes its receiver in place.
                        self.place(recv);
                        return;
                    }
                    _ => {}
                }
                self.expr(recv);
            }
            _ => self.expr(callee),
        }
    }
}
