//! Patterns and `match` (spec §5.6, §5.7): pattern typing, then exhaustiveness and reachability
//! (`crate::exhaust`).
//!
//! **Typing.** A pattern is typed against the type of the value it matches, seen through references (a
//! binding at the top takes the scrutinee's own type). A constructor pattern names a type: against a value
//! whose type is still unknown it decides it (`Some(_)` makes it an `Option`, `Stage::Done` a `Stage`, a
//! literal its literal type). Fields are taken by position or by name; a field may be omitted (it is `_`),
//! none may be given twice. A binding gets its field's type; the alternatives of a `|` bind the same names
//! at the same types (they share one binding per name), else `type-mismatch`.
//!
//! **Errors.** A fallible scrutinee (`match f():`) is matched by `Ok(value)` and `Err(error)`, or by an error
//! name directly (`case ChestEmpty:` is `Err(ChestEmpty)`); a caught error (`else err:` + `match err:`) by
//! its error names. An error name is checked against the set: one that cannot occur there is
//! `impossible-arm`, and that pattern matches nothing. An upper-case name that resolution could not find
//! (`Ctor::ErrorOrUnknown`) is an error name against an error; anywhere else it is `unresolved`, with the
//! suggestion resolution computed in its scope.
//!
//! **Checks.** Once every arm is typed: `non-exhaustive` at the `match` naming one value no arm matches
//! (guarded arms do not count), and the warning `unreachable-arm` for an arm earlier arms cover. A pattern
//! whose mistake was reported here or earlier counts as `_`, so it causes no further report; an arm with
//! such a pattern, and every arm after it, gets no `unreachable-arm`. A scrutinee of the error type is not
//! checked at all.
//!
//! What each pattern node matches is kept for code generation (`Typed::patterns`, [`PatInfo`]).

use super::{Body, Infer, error};
use crate::ast::{PatLit, Span};
use crate::diag::Severity;
use crate::exhaust::{self, Arm, CtorSig, Key, Pat};
use crate::hir::{
    Case, Ctor, DefKind, Expr, HostItem, LocalId, PatField, Pattern, PatternKind, Prelude,
};
use crate::ty::{SetId, Ty, TyId};
use std::collections::BTreeSet;

/// What a pattern node matches, for code generation (`Typed::patterns`, by [`PatId`](crate::hir::PatId)).
#[derive(Debug, Clone, PartialEq)]
pub struct PatInfo {
    /// The type of the value the node is matched against (a reference when matched through one).
    pub ty: TyId,
    /// A constructor node's constructor; `None` for the other nodes.
    pub ctor: Option<PatCtor>,
    /// A constructor node's sub-patterns (in source order): the index of the field each one matches
    /// (`u32::MAX` for a field reported wrong).
    pub fields: Vec<u32>,
}

/// The constructor a pattern node names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatCtor {
    Some,
    None,
    Ok,
    Err,
    /// An enum's variant, by index.
    Variant(u32),
    /// A struct or an event (script or host): its only constructor.
    Struct,
    /// An error name, in a caught error (inside `Err(…)`, or matching an `else err:` binding).
    Error(String),
    /// An error name matched directly on a fallible result: `Err(name)`.
    ErrName(String),
}

/// What one arm's pattern did.
#[derive(Default)]
struct ArmCx {
    /// The bindings made so far (in any alternative).
    bound: BTreeSet<LocalId>,
    /// The bindings made so far on the way to the current node, leaving out the other alternatives of each
    /// enclosing `|`: one met again here is bound twice (`duplicate-def`, reported by resolution).
    live: BTreeSet<LocalId>,
    /// Every binding made, in order (for comparing alternatives).
    order: Vec<LocalId>,
    /// A mistake was reported in the pattern, or part of it was matched against the error type.
    failed: bool,
}

/// The constructor a pattern names, once checked against the value's type.
struct Target {
    key: Key,
    ctor: PatCtor,
    /// For messages: `Stage::Accepted`, `Some`, `NotEnough`.
    name: String,
    names: Vec<String>,
    tys: Vec<TyId>,
}

impl Infer<'_> {
    /// `match scrutinee:` and its cases (`at`: the statement); true when every arm diverges.
    pub(super) fn match_stmt(
        &mut self,
        b: &mut Body,
        at: Span,
        scrutinee: &Expr,
        cases: &[Case],
    ) -> bool {
        // A fallible scrutinee is consumed here.
        let ty = self.expr_fallible(b, scrutinee);
        let mut arms = Vec::with_capacity(cases.len());
        let mut failed = Vec::with_capacity(cases.len());
        let mut all = !cases.is_empty();
        for c in cases {
            self.matched_names(b, &c.pattern);
            let mut cx = ArmCx::default();
            let pat = self.pattern(b, &c.pattern, ty, &mut cx);
            if let Some(g) = &c.guard {
                self.cond(b, g);
            }
            all &= self.block(b, &c.body);
            arms.push(Arm {
                pat,
                guarded: c.guard.is_some(),
            });
            failed.push(cx.failed);
        }
        if b.waits.is_empty() {
            self.match_checks(b, at, ty, &arms, &failed, cases);
        }
        all
    }

    /// `non-exhaustive` and `unreachable-arm`.
    fn match_checks(
        &mut self,
        b: &mut Body,
        at: Span,
        ty: TyId,
        arms: &[Arm],
        failed: &[bool],
        cases: &[Case],
    ) {
        let (inner, _) = self.deref(ty);
        if matches!(self.types.get(inner), Ty::Error | Ty::Never) {
            return;
        }
        let report = exhaust::check(&mut Oracle { inf: self, b: &*b }, ty, arms);
        if let Some(missing) = report.missing {
            let mut d = error(
                "non-exhaustive",
                format!("this `match` does not cover every value: `{missing}` is not matched"),
                b.file,
                at,
            );
            d.notes
                .push("add an arm for it, or `case _:` for everything else".to_string());
            b.diags.push(d);
        }
        // The first arm with a mistake hides the reachability of every later one.
        let quiet_from = failed.iter().position(|f| *f).unwrap_or(usize::MAX);
        for i in report.unreachable {
            let Some(c) = cases.get(i).filter(|_| i < quiet_from) else {
                continue;
            };
            let mut d = error(
                "unreachable-arm",
                "this arm can never match: the arms before it match everything it does".to_string(),
                b.file,
                c.span,
            );
            d.severity = Severity::Warning;
            b.diags.push(d);
        }
    }

    /// Types a pattern against a value of type `expected`; the pattern for the exhaustiveness check.
    fn pattern(&mut self, b: &mut Body, p: &Pattern, expected: TyId, cx: &mut ArmCx) -> Pat {
        let want = self.shallow(expected);
        match &p.kind {
            PatternKind::Error => {
                cx.failed = true;
                self.note_pat(b, p, want, None, Vec::new());
                Pat::Wild
            }
            PatternKind::Wild => {
                self.note_pat(b, p, want, None, Vec::new());
                Pat::Wild
            }
            PatternKind::Bind(local) => {
                let ty = if matches!(self.types.get(want), Ty::Fallible(_, _)) {
                    self.fallible_not_value(b, p.span);
                    cx.failed = true;
                    TyId::ERROR
                } else {
                    expected
                };
                self.bind_pattern_local(b, *local, ty, p.span, cx);
                self.note_pat(b, p, want, None, Vec::new());
                Pat::Wild
            }
            PatternKind::Lit(lit) => self.lit_pattern(b, p, lit, want, cx),
            PatternKind::Or(alts) => self.or_pattern(b, p, alts, expected, cx),
            PatternKind::Ctor(c, fields) => self.ctor_pattern(b, p, c, fields, want, cx),
        }
    }

    fn note_pat(
        &mut self,
        b: &mut Body,
        p: &Pattern,
        ty: TyId,
        ctor: Option<PatCtor>,
        fields: Vec<u32>,
    ) {
        b.patterns.push((p.id, PatInfo { ty, ctor, fields }));
    }

    fn fallible_not_value(&mut self, b: &mut Body, at: Span) {
        self.mismatch(
            b,
            at,
            "a fallible result is not a value: match it with `Ok(…)` and `Err(…)` (or error names)"
                .to_string(),
        );
    }

    /// A binding in a pattern. A name bound again (in another alternative) keeps its one type.
    fn bind_pattern_local(
        &mut self,
        b: &mut Body,
        local: LocalId,
        ty: TyId,
        at: Span,
        cx: &mut ArmCx,
    ) {
        if !cx.live.insert(local) {
            cx.failed = true;
            return;
        }
        cx.order.push(local);
        if !cx.bound.insert(local) {
            let have = self.local_ty(local);
            if !self.unify(have, ty) {
                let name = self.local_name(local);
                let (was, now) = (self.show(have), self.show(ty));
                self.mismatch(
                    b,
                    at,
                    format!(
                        "`{name}` is `{was}` in one alternative and `{now}` in another: every alternative binds it at one type"
                    ),
                );
                cx.failed = true;
            }
            return;
        }
        self.set_local(local, ty);
        b.bindings.push(local);
    }

    fn local_name(&self, local: LocalId) -> String {
        self.krate
            .local(local)
            .map_or(String::new(), |l| l.name.clone())
    }

    fn lit_pattern(
        &mut self,
        b: &mut Body,
        p: &Pattern,
        lit: &PatLit,
        want: TyId,
        cx: &mut ArmCx,
    ) -> Pat {
        self.note_pat(b, p, want, None, Vec::new());
        let (ty, key) = match lit {
            PatLit::Int(v) => (TyId::INT, Key::Int(*v)),
            // `-0.0` and `0.0` are equal.
            PatLit::Float(v) => (TyId::FLOAT, Key::Float((*v + 0.0).to_bits())),
            PatLit::Bool(v) => (TyId::BOOL, Key::Bool(*v)),
            PatLit::Str(s) => (TyId::STR, Key::Str(s.clone())),
        };
        let (inner, _) = self.deref(want);
        match self.types.get(inner) {
            Ty::Error | Ty::Never => {
                cx.failed = true;
                return Pat::Wild;
            }
            Ty::Fallible(_, _) => {
                self.fallible_not_value(b, p.span);
                cx.failed = true;
                return Pat::Wild;
            }
            _ => {}
        }
        if !self.unify(inner, ty) {
            let (lit, found) = (self.show(ty), self.show(inner));
            self.disagree(
                b,
                inner,
                ty,
                p.span,
                format!("this pattern is `{lit}`, but the value matched is `{found}`"),
            );
            cx.failed = true;
            return Pat::Wild;
        }
        Pat::Ctor(key, Vec::new())
    }

    /// `A | B`: every alternative binds the same names.
    fn or_pattern(
        &mut self,
        b: &mut Body,
        p: &Pattern,
        alts: &[Pattern],
        expected: TyId,
        cx: &mut ArmCx,
    ) -> Pat {
        let want = self.shallow(expected);
        self.note_pat(b, p, want, None, Vec::new());
        let mut out = Vec::with_capacity(alts.len());
        let mut first: Option<BTreeSet<LocalId>> = None;
        let mut odd: Option<LocalId> = None;
        let outer = cx.live.clone();
        let mut after = outer.clone();
        for a in alts {
            cx.live = outer.clone();
            let start = cx.order.len();
            out.push(self.pattern(b, a, expected, cx));
            let names: BTreeSet<LocalId> = cx
                .order
                .get(start..)
                .unwrap_or(&[])
                .iter()
                .copied()
                .collect();
            after.extend(names.iter().copied());
            match &first {
                None => first = Some(names),
                Some(f) => {
                    if odd.is_none() {
                        odd = f.symmetric_difference(&names).next().copied();
                    }
                }
            }
        }
        cx.live = after;
        if let Some(local) = odd {
            let name = self.local_name(local);
            self.mismatch(
                b,
                p.span,
                format!(
                    "every alternative of `|` binds the same names: `{name}` is bound in one alternative but not in another"
                ),
            );
            cx.failed = true;
        }
        Pat::Or(out)
    }

    /// Gives a constructor's sub-patterns the error type (its own mistake is reported, or its value's type
    /// is already wrong).
    fn wild_fields(&mut self, b: &mut Body, fields: &[PatField], cx: &mut ArmCx) {
        for f in fields {
            self.pattern(b, &f.pattern, TyId::ERROR, cx);
        }
    }

    fn ctor_pattern(
        &mut self,
        b: &mut Body,
        p: &Pattern,
        c: &Ctor,
        fields: &[PatField],
        want: TyId,
        cx: &mut ArmCx,
    ) -> Pat {
        let (inner, _) = self.deref(want);
        let shape = self.types.get(inner);
        if matches!(shape, Ty::Error | Ty::Never) {
            self.note_pat(b, p, want, None, Vec::new());
            self.wild_fields(b, fields, cx);
            cx.failed = true;
            return Pat::Wild;
        }
        let target = match c {
            Ctor::Error(name) | Ctor::ErrorOrUnknown(name, _)
                if matches!(shape, Ty::Caught(_) | Ty::Fallible(_, _)) =>
            {
                let (Ty::Caught(set) | Ty::Fallible(_, set)) = shape else {
                    return Pat::Wild;
                };
                let wrapped = matches!(shape, Ty::Fallible(_, _));
                return self.error_pattern(b, p, name, set, wrapped, fields, want, cx);
            }
            Ctor::ErrorOrUnknown(name, near) => {
                let mut d = error(
                    "unresolved",
                    format!("cannot find `{name}`"),
                    b.file,
                    p.span,
                );
                if let Some(near) = near {
                    d.notes.push(format!("did you mean `{near}`?"));
                }
                b.diags.push(d);
                None
            }
            _ => self.ctor_target(b, p.span, c, inner),
        };
        let Some(t) = target else {
            self.note_pat(b, p, want, None, Vec::new());
            self.wild_fields(b, fields, cx);
            cx.failed = true;
            return Pat::Wild;
        };
        let (pats, index) = self.sub_patterns(b, &t.name, &t.names, &t.tys, fields, cx);
        self.note_pat(b, p, want, Some(t.ctor), index);
        Pat::Ctor(t.key, pats)
    }

    /// What a constructor other than an error name matches, checked against (or deciding) the value's
    /// type `inner`; `None` once a mismatch is reported.
    fn ctor_target(&mut self, b: &mut Body, at: Span, c: &Ctor, inner: TyId) -> Option<Target> {
        let shape = self.types.get(inner);
        // The type the constructor builds, and what it needs of the value's type.
        let (made, target) = match c {
            Ctor::Prelude(Prelude::Some | Prelude::None) => {
                let elem = match shape {
                    Ty::Option(t) => t,
                    _ => self.fresh(),
                };
                let made = self.types.intern(Ty::Option(elem));
                let target = if *c == Ctor::Prelude(Prelude::Some) {
                    Target {
                        key: Key::Some,
                        ctor: PatCtor::Some,
                        name: "Some".to_string(),
                        names: vec![String::new()],
                        tys: vec![elem],
                    }
                } else {
                    Target {
                        key: Key::None,
                        ctor: PatCtor::None,
                        name: "None".to_string(),
                        names: Vec::new(),
                        tys: Vec::new(),
                    }
                };
                (made, target)
            }
            Ctor::Prelude(p @ (Prelude::Ok | Prelude::Err)) => {
                let name = if *p == Prelude::Ok { "Ok" } else { "Err" };
                let Ty::Fallible(value, set) = shape else {
                    let found = self.show(inner);
                    self.mismatch(
                        b,
                        at,
                        format!(
                            "`{name}(…)` matches a fallible result (`match f():`), not `{found}`"
                        ),
                    );
                    return None;
                };
                let target = if *p == Prelude::Ok {
                    Target {
                        key: Key::Ok,
                        ctor: PatCtor::Ok,
                        name: name.to_string(),
                        names: vec![String::new()],
                        tys: vec![value],
                    }
                } else {
                    Target {
                        key: Key::Err,
                        ctor: PatCtor::Err,
                        name: name.to_string(),
                        names: vec![String::new()],
                        tys: vec![self.types.intern(Ty::Caught(set))],
                    }
                };
                (inner, target)
            }
            Ctor::Variant(d, i) => {
                let variant = match self.krate.def(*d).map(|def| &def.kind) {
                    Some(DefKind::Enum(vs)) => vs.get(*i as usize),
                    _ => None,
                };
                let shape = self.adts.get(d).and_then(|s| s.get(*i as usize)).cloned();
                let (Some(v), Some(fields)) = (variant, shape) else {
                    return None;
                };
                let target = Target {
                    key: Key::Variant(*i),
                    ctor: PatCtor::Variant(*i),
                    name: format!("{}::{}", self.krate.qualified_name(*d), v.name),
                    names: fields.names,
                    tys: fields.tys,
                };
                (self.types.intern(Ty::Adt(*d)), target)
            }
            Ctor::Def(d) => {
                let fields = self.adts.get(d).and_then(|s| s.first()).cloned()?;
                let target = Target {
                    key: Key::Single,
                    ctor: PatCtor::Struct,
                    name: self.krate.qualified_name(*d),
                    names: fields.names,
                    tys: fields.tys,
                };
                (self.types.intern(Ty::Adt(*d)), target)
            }
            Ctor::Host(h) => {
                let (made, fields, name) = match h {
                    HostItem::Struct(i) => (
                        Ty::HostStruct(*i),
                        self.host_structs.get(*i as usize).cloned(),
                        self.host.structs.get(*i as usize).map(|s| s.name.clone()),
                    ),
                    HostItem::Event(i) => (
                        Ty::HostEvent(*i),
                        self.host_events.get(*i as usize).cloned(),
                        self.host.events.get(*i as usize).map(|e| e.name.clone()),
                    ),
                    HostItem::Fn(_) => return None,
                };
                let (Some(fields), Some(name)) = (fields, name) else {
                    return None;
                };
                let target = Target {
                    key: Key::Single,
                    ctor: PatCtor::Struct,
                    name,
                    names: fields.names,
                    tys: fields.tys,
                };
                (self.types.intern(made), target)
            }
            // Resolution gives no other prelude constructor; error names are handled by the caller.
            Ctor::Prelude(_) | Ctor::Error(_) | Ctor::ErrorOrUnknown(_, _) => return None,
        };
        if matches!(shape, Ty::Fallible(_, _)) && !matches!(target.key, Key::Ok | Key::Err) {
            self.fallible_not_value(b, at);
            return None;
        }
        if !self.unify(inner, made) {
            let (name, found) = (target.name.clone(), self.show(inner));
            let what = self.show(made);
            self.disagree(
                b,
                inner,
                made,
                at,
                format!("`{name}` matches a `{what}`, but the value matched is `{found}`"),
            );
            return None;
        }
        Some(target)
    }

    /// An error name matched against the error set `set`: of a caught error, or (`wrapped`) of a fallible
    /// result, where it stands for `Err(name)`.
    #[allow(clippy::too_many_arguments)]
    fn error_pattern(
        &mut self,
        b: &mut Body,
        p: &Pattern,
        name: &str,
        set: SetId,
        wrapped: bool,
        fields: &[PatField],
        want: TyId,
        cx: &mut ArmCx,
    ) -> Pat {
        let ctor = if wrapped {
            PatCtor::ErrName(name.to_string())
        } else {
            PatCtor::Error(name.to_string())
        };
        let names = self.types.names(set);
        if names.binary_search_by(|n| n.as_str().cmp(name)).is_err() {
            let set_text = names.join(" | ");
            b.diags.push(error(
                "impossible-arm",
                format!("the error `{name}` cannot occur here: this can only fail with {set_text}"),
                b.file,
                p.span,
            ));
            self.note_pat(b, p, want, Some(ctor), Vec::new());
            self.wild_fields(b, fields, cx);
            cx.failed = true;
            // It matches nothing.
            return Pat::Or(Vec::new());
        }
        let shape = self.shape_for_pattern(b, name, p.span);
        let (field_names, tys): (Vec<String>, Vec<TyId>) = shape
            .map(|s| {
                s.fields
                    .into_iter()
                    .map(|f| (f.name, f.ty.unwrap_or(TyId::ERROR)))
                    .unzip()
            })
            .unwrap_or_default();
        let (pats, index) = self.sub_patterns(b, name, &field_names, &tys, fields, cx);
        self.note_pat(b, p, want, Some(ctor), index);
        let pat = Pat::Ctor(Key::Error(name.to_string()), pats);
        if wrapped {
            Pat::Ctor(Key::Err, vec![pat])
        } else {
            pat
        }
    }

    /// A constructor's sub-patterns against its fields (`names`, `tys`), by position or by name: one per
    /// field (`_` where none is given), and the field each given one matches.
    fn sub_patterns(
        &mut self,
        b: &mut Body,
        ctor: &str,
        names: &[String],
        tys: &[TyId],
        fields: &[PatField],
        cx: &mut ArmCx,
    ) -> (Vec<Pat>, Vec<u32>) {
        let mut pats: Vec<Option<Pat>> = vec![None; names.len()];
        let mut index = Vec::with_capacity(fields.len());
        let mut positional = 0usize;
        for f in fields {
            let found = match &f.name {
                Some(n) => {
                    let i = names.iter().position(|x| !x.is_empty() && x == n);
                    if i.is_none() {
                        b.diags.push(error(
                            "unknown-field",
                            format!("`{ctor}` has no field `{n}`"),
                            b.file,
                            f.pattern.span,
                        ));
                    }
                    i
                }
                None => {
                    positional += 1;
                    let i = Some(positional - 1).filter(|i| *i < names.len());
                    if i.is_none() {
                        let message = match names.len() {
                            0 => format!("`{ctor}` has no fields, one is given"),
                            1 => format!("`{ctor}` has 1 field, more are given"),
                            n => format!("`{ctor}` has {n} fields, more are given"),
                        };
                        b.diags
                            .push(error("arg-count", message, b.file, f.pattern.span));
                    }
                    i
                }
            };
            let slot = found.and_then(|i| {
                if pats.get(i).is_some_and(Option::is_some) {
                    let field = names.get(i).map_or("", String::as_str);
                    b.diags.push(error(
                        "arg-count",
                        format!("the field `{field}` of `{ctor}` is matched twice"),
                        b.file,
                        f.pattern.span,
                    ));
                    None
                } else {
                    Some(i)
                }
            });
            match slot {
                Some(i) => {
                    let ty = tys.get(i).copied().unwrap_or(TyId::ERROR);
                    let pat = self.pattern(b, &f.pattern, ty, cx);
                    if let Some(s) = pats.get_mut(i) {
                        *s = Some(pat);
                    }
                    index.push(u32::try_from(i).unwrap_or(u32::MAX));
                }
                None => {
                    self.pattern(b, &f.pattern, TyId::ERROR, cx);
                    cx.failed = true;
                    index.push(u32::MAX);
                }
            }
        }
        let pats = pats.into_iter().map(|p| p.unwrap_or(Pat::Wild)).collect();
        (pats, index)
    }

    /// A type's constructors for the exhaustiveness check (see `exhaust::Space`).
    fn ctors_of(&mut self, b: &Body, ty: TyId) -> Option<Vec<CtorSig>> {
        let (ty, _) = self.deref(ty);
        let sig = |key: Key, name: &str, fields: Vec<TyId>| CtorSig {
            key,
            name: name.to_string(),
            fields,
        };
        match self.types.get(ty) {
            Ty::Bool => Some(vec![
                sig(Key::Bool(true), "true", Vec::new()),
                sig(Key::Bool(false), "false", Vec::new()),
            ]),
            Ty::Option(t) => Some(vec![
                sig(Key::Some, "Some", vec![t]),
                sig(Key::None, "None", Vec::new()),
            ]),
            Ty::Fallible(value, set) => {
                let caught = self.types.intern(Ty::Caught(set));
                Some(vec![
                    sig(Key::Ok, "Ok", vec![value]),
                    sig(Key::Err, "Err", vec![caught]),
                ])
            }
            Ty::Caught(set) => {
                let names = self.types.names(set).to_vec();
                Some(
                    names
                        .into_iter()
                        .map(|n| {
                            let tys = self
                                .shape_of(b, &n)
                                .map(|s| {
                                    s.fields
                                        .iter()
                                        .map(|f| f.ty.unwrap_or(TyId::ERROR))
                                        .collect()
                                })
                                .unwrap_or_default();
                            sig(Key::Error(n.clone()), &n, tys)
                        })
                        .collect(),
                )
            }
            Ty::Adt(d) => {
                let shapes = self.adts.get(&d)?;
                match self.krate.def(d).map(|def| &def.kind) {
                    Some(DefKind::Enum(variants)) => {
                        let enum_name = self.krate.qualified_name(d);
                        Some(
                            variants
                                .iter()
                                .zip(shapes)
                                .zip(0u32..)
                                .map(|((v, s), i)| {
                                    sig(
                                        Key::Variant(i),
                                        &format!("{enum_name}::{}", v.name),
                                        s.tys.clone(),
                                    )
                                })
                                .collect(),
                        )
                    }
                    Some(DefKind::Struct(_) | DefKind::Event(_)) => {
                        let tys = shapes.first()?.tys.clone();
                        Some(vec![sig(Key::Single, &self.krate.qualified_name(d), tys)])
                    }
                    _ => None,
                }
            }
            Ty::HostStruct(i) => {
                let tys = self.host_structs.get(i as usize)?.tys.clone();
                let name = self.host.structs.get(i as usize)?.name.clone();
                Some(vec![sig(Key::Single, &name, tys)])
            }
            Ty::HostEvent(i) => {
                let tys = self.host_events.get(i as usize)?.tys.clone();
                let name = self.host.events.get(i as usize)?.name.clone();
                Some(vec![sig(Key::Single, &name, tys)])
            }
            _ => None,
        }
    }
}

/// The exhaustiveness check's view of the types, from inside a body.
struct Oracle<'a, 'k, 'f> {
    inf: &'a mut Infer<'k>,
    b: &'a Body<'f>,
}

impl exhaust::Space for Oracle<'_, '_, '_> {
    fn ctors(&mut self, ty: TyId) -> Option<Vec<CtorSig>> {
        self.inf.ctors_of(self.b, ty)
    }
}
