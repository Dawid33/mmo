//! The query subset (spec §5.12; construction design §7.1): a role's `where` and a quest's `requires` are
//! evaluated and indexed by the coordinator without the VM, so each is a conjunction (`and`) of atoms:
//!
//! - a comparison of a fact to a literal or enum value (`x.occupation == Occupation::Smith`, `act() >= 2`),
//!   or `x.f in [A, B]` (a list of constants);
//! - a tag or kind test (`x.has_tag("…")`, `x.kind == …`): a `bool` fact, or a fact compared;
//! - a relation between two roles (`a.site == b.site`, `a.owner == b`, `a != b`, `related(a, b)`,
//!   `near(a, b, regions=n)`);
//! - a world fact (`act()`, `loop()`, `completed(def, …)`, `site_kind(x.site) == …`);
//! - `not atom`, and an `or` of atoms about one role (one group).
//!
//! A fact is a host query or property the host marks `fact` (its **fact table**), called on a role or a
//! fact, with constants, roles or facts as its arguments. Anything else is `bad-where`: a script `fn`, a
//! script global, a query that is not a fact, arithmetic beyond a comparison with a constant, a role
//! declared later than the one whose `where` it is (or any role, in `requires`). One diagnostic per
//! top-level conjunct, at its first problem.

use crate::ast::{BinOp, Span, UnOp};
use crate::diag::Diagnostic;
use crate::hir::{Crate, DefId, DefKind, Expr, ExprKind, HostItem, LocalId, Quest, StmtKind};
use crate::infer::{Method, Typed};
use quest_vm::host::HostApi;
use std::collections::{BTreeMap, BTreeSet};

/// `bad-where` for every quest's roles' `where` and its `requires`.
pub fn query_subset(krate: &Crate, typed: &Typed, host: &HostApi) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    for def in &krate.defs {
        let DefKind::Quest(q) = &def.kind else {
            continue;
        };
        for (i, r) in q.roles.iter().enumerate() {
            if let Some(f) = r.cond {
                Subset::new(krate, typed, host, q, f, Some(i)).check(&mut diags);
            }
        }
        for f in &q.requires {
            Subset::new(krate, typed, host, q, *f, None).check(&mut diags);
        }
    }
    diags
}

/// What a term of an atom is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Term {
    /// A literal, an enum value, `None`: what a fact is compared with.
    Const,
    /// A fact (a role is one: its identity), with the roles it is about.
    Fact(BTreeSet<usize>),
}

/// A problem: where, and what.
type Bad = (Span, String);

/// One `where` or `requires` fn.
struct Subset<'a> {
    krate: &'a Crate,
    typed: &'a Typed,
    host: &'a HostApi,
    /// The synthesised fn.
    f: DefId,
    /// The role whose `where` this is (`None`: a `requires`).
    role: Option<usize>,
    /// The fn's parameters: the roles it may name (the earlier ones, then itself), by binding.
    params: BTreeMap<LocalId, usize>,
    /// Every role's global, by index.
    globals: BTreeMap<DefId, usize>,
}

impl<'a> Subset<'a> {
    fn new(
        krate: &'a Crate,
        typed: &'a Typed,
        host: &'a HostApi,
        q: &Quest,
        f: DefId,
        role: Option<usize>,
    ) -> Subset<'a> {
        let params = match krate.def(f).map(|d| &d.kind) {
            Some(DefKind::Fn(func)) => func
                .params
                .iter()
                .enumerate()
                .map(|(i, p)| (p.local, i))
                .collect(),
            _ => BTreeMap::new(),
        };
        Subset {
            krate,
            typed,
            host,
            f,
            role,
            params,
            globals: q
                .roles
                .iter()
                .enumerate()
                .map(|(i, r)| (r.global, i))
                .collect(),
        }
    }

    /// The fn's expression: its body is `return e`.
    fn body(&self) -> Option<&'a Expr> {
        let Some(DefKind::Fn(func)) = self.krate.def(self.f).map(|d| &d.kind) else {
            return None;
        };
        match func.body.as_slice() {
            [s] => match &s.kind {
                StmtKind::Return(Some(e)) => Some(e),
                _ => None,
            },
            _ => None,
        }
    }

    fn check(&self, diags: &mut Vec<Diagnostic>) {
        let Some(e) = self.body() else { return };
        let file = self
            .krate
            .def(self.f)
            .and_then(|d| self.krate.module(d.module))
            .map_or("", |m| m.file.as_str());
        // The conjuncts, left to right (a loop: an `and` chain is as long as the source).
        let mut conjuncts = Vec::new();
        let mut stack = vec![e];
        while let Some(x) = stack.pop() {
            match &x.kind {
                ExprKind::Binary(BinOp::And, l, r) => stack.extend([&**r, &**l]),
                _ => conjuncts.push(x),
            }
        }
        let what = if self.role.is_some() {
            "a role's `where`"
        } else {
            "`requires`"
        };
        for c in conjuncts {
            if let Err((at, why)) = self.group(c) {
                let mut d = Diagnostic::error(
                    "bad-where",
                    format!("{what} is a query the coordinator evaluates: {why}"),
                    file,
                    at.line,
                    at.col,
                    at.len,
                );
                d.notes.push(
                    "it is an `and` of atoms: a fact compared with a constant, `x.f in [A, B]`, a `bool` fact, a relation between roles, `not atom`, or an `or` of atoms about one role"
                        .to_string(),
                );
                diags.push(d);
            }
        }
    }

    /// A conjunct: an atom, or an `or` of atoms about one role.
    fn group(&self, e: &Expr) -> Result<(), Bad> {
        if !matches!(e.kind, ExprKind::Binary(BinOp::Or, _, _)) {
            return self.atom(e).map(|_| ());
        }
        let mut alternatives = Vec::new();
        let mut stack = vec![e];
        while let Some(x) = stack.pop() {
            match &x.kind {
                ExprKind::Binary(BinOp::Or, l, r) => stack.extend([&**r, &**l]),
                _ => alternatives.push(x),
            }
        }
        let mut roles = BTreeSet::new();
        for a in alternatives {
            roles.extend(self.atom(a)?);
        }
        if roles.len() > 1 {
            return Err((
                e.span,
                "an `or` joins atoms about one role (the coordinator indexes it as one filter)"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// An atom: the roles it is about.
    fn atom(&self, e: &Expr) -> Result<BTreeSet<usize>, Bad> {
        match &e.kind {
            ExprKind::Error => Ok(BTreeSet::new()),
            ExprKind::Unary(UnOp::Not, x) => match &x.kind {
                ExprKind::Binary(BinOp::And | BinOp::Or, _, _) => Err((
                    e.span,
                    "`not` applies to one atom, not to an `and` or an `or`".to_string(),
                )),
                _ => self.atom(x),
            },
            ExprKind::Binary(
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge,
                l,
                r,
            ) => match (self.term(l)?, self.term(r)?) {
                (Term::Const, Term::Const) => Err((
                    e.span,
                    "this compares two constants: compare a fact".to_string(),
                )),
                (Term::Fact(a), Term::Fact(b)) => Ok(a.union(&b).copied().collect()),
                (Term::Fact(a), Term::Const) | (Term::Const, Term::Fact(a)) => Ok(a),
            },
            ExprKind::Binary(BinOp::In | BinOp::NotIn, l, r) => {
                let Term::Fact(roles) = self.term(l)? else {
                    return Err((l.span, "`in` tests a fact".to_string()));
                };
                match &r.kind {
                    ExprKind::List(items) => {
                        for x in items {
                            if self.term(x)? != Term::Const {
                                return Err((
                                    x.span,
                                    "`in [..]` lists constants (literals or enum values)"
                                        .to_string(),
                                ));
                            }
                        }
                        Ok(roles)
                    }
                    ExprKind::Error => Ok(roles),
                    _ => Err((
                        r.span,
                        "`in` takes a list of constants written out (`in [A, B]`)".to_string(),
                    )),
                }
            }
            ExprKind::Binary(BinOp::And | BinOp::Or, _, _) => Err((
                e.span,
                "an `and` inside an `or` is not an atom: write the conditions as separate conjuncts"
                    .to_string(),
            )),
            _ => match self.term(e)? {
                Term::Fact(roles) => Ok(roles),
                Term::Const => Err((e.span, "a constant is not a condition".to_string())),
            },
        }
    }

    /// A term of an atom: a constant, a role or a fact.
    fn term(&self, e: &Expr) -> Result<Term, Bad> {
        match &e.kind {
            ExprKind::Error
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Variant(_, _)
            | ExprKind::Prelude(crate::hir::Prelude::None) => Ok(Term::Const),
            ExprKind::Unary(UnOp::Neg, x)
                if matches!(x.kind, ExprKind::Int(_) | ExprKind::Float(_)) =>
            {
                Ok(Term::Const)
            }
            ExprKind::Local(l) => match self.params.get(l) {
                Some(i) => Ok(Term::Fact(BTreeSet::from([*i]))),
                None => Err((e.span, "this is not a role".to_string())),
            },
            ExprKind::Def(d) => Err((e.span, self.def_problem(*d))),
            ExprKind::Binary(
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem,
                _,
                _,
            )
            | ExprKind::Unary(UnOp::Neg, _) => Err((
                e.span,
                "no arithmetic: compare a fact with a constant".to_string(),
            )),
            ExprKind::Field(x, name) => {
                match self.typed.methods.get(&e.id) {
                    Some(Method::Host(f)) => self.fact(*f, e.span)?,
                    // A field of a host handle (`x.id`): data the coordinator holds.
                    _ if matches!(
                        self.typed.types.get(self.typed.expr(x.id)),
                        crate::ty::Ty::HostStruct(_)
                    ) => {}
                    _ => {
                        return Err((
                            e.span,
                            format!("`.{name}` is not a fact of the host's fact table"),
                        ));
                    }
                }
                self.term(x)
            }
            ExprKind::Call(callee, args) => {
                let mut roles = BTreeSet::new();
                match &callee.kind {
                    ExprKind::Host(HostItem::Fn(f)) => self.fact(*f, e.span)?,
                    ExprKind::Field(recv, _) => match self.typed.methods.get(&e.id) {
                        Some(Method::Host(f)) => {
                            self.fact(*f, e.span)?;
                            if let Term::Fact(r) = self.term(recv)? {
                                roles.extend(r);
                            }
                        }
                        Some(Method::Script(m)) => {
                            return Err((e.span, self.script_fn(*m)));
                        }
                        _ => {
                            return Err((
                                e.span,
                                "only the host's facts are called here".to_string(),
                            ));
                        }
                    },
                    ExprKind::Def(d) => return Err((e.span, self.def_problem(*d))),
                    ExprKind::Error => return Ok(Term::Const),
                    _ => {
                        return Err((e.span, "only the host's facts are called here".to_string()));
                    }
                }
                for a in args {
                    if let Term::Fact(r) = self.term(&a.value)? {
                        roles.extend(r);
                    }
                }
                Ok(Term::Fact(roles))
            }
            _ => Err((e.span, "this is not part of the query subset".to_string())),
        }
    }

    /// The host fn `f` must be a fact.
    fn fact(&self, f: u32, at: Span) -> Result<(), Bad> {
        match self.host.fns.get(f as usize) {
            Some(hf) if hf.fact => Ok(()),
            Some(hf) => {
                let name = match hf.namespace.as_ref().or(hf.receiver.as_ref()) {
                    Some(owner) => format!("{owner}.{}", hf.name),
                    None => hf.name.clone(),
                };
                Err((
                    at,
                    format!("`{name}` is not a fact (not in the host's fact table)"),
                ))
            }
            None => Ok(()),
        }
    }

    fn script_fn(&self, d: DefId) -> String {
        format!(
            "`{}` is a script `fn`; only the host's facts are called here",
            self.krate.qualified_name(d)
        )
    }

    /// Why an item named in an atom is refused.
    fn def_problem(&self, d: DefId) -> String {
        let name = self.krate.qualified_name(d);
        if let Some(i) = self.globals.get(&d) {
            let role = self.krate.def(d).map_or(String::new(), |def| {
                def.name.trim_start_matches("role.").to_string()
            });
            return match self.role {
                Some(me) if *i > me => format!(
                    "the role `{role}` is declared later: a `where` names only its own role and earlier ones"
                ),
                Some(_) => format!("the role `{role}` is not in scope here"),
                None => format!(
                    "`requires` is checked before any role is filled: it cannot name the role `{role}`"
                ),
            };
        }
        match self.krate.def(d).map(|def| &def.kind) {
            Some(DefKind::Fn(_)) => self.script_fn(d),
            Some(DefKind::Global(_)) => format!(
                "`{name}` is a script global; the coordinator sees only facts and constants"
            ),
            _ => format!("`{name}` is not part of the query subset"),
        }
    }
}
