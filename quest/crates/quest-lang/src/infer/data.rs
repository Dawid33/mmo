//! Data: the user types (their fields once, `too-many-fields`, `infinite-type`), constructions, fields,
//! indexing, slices, `in`, dict keys and storable values.
//!
//! **Constructions** take fields by name or by position (`Ledger(owner="ana", entries=[])`,
//! `Stage::Accepted(5)`), every field exactly once; positional arguments fill fields in declaration order.
//! Field types are always annotated (spec §5.5), so a construction never decides one.
//!
//! **Finite types** (decision 17): a type may contain itself, but only through something that can be empty — a
//! `list`, a `dict`, an `Option`, or an enum variant without it. "Has a finite value" is the least fixpoint
//! over the user types: a struct has one when every field's type has one, an enum when one variant's fields
//! all do; a built-in or host type always has one. It is computed by counting (each struct or variant waits on
//! the number of its fields of a user type not yet known finite), so it is linear in the number of fields. A
//! user type left without is `infinite-type`, reported once per cycle of direct containment, at the cycle's
//! smallest name.
//!
//! **Dict keys** (spec §5.8): `int`, `bool`, `str`, and enums (`Option` among them) and structs whose fields are
//! all keys, through the same rule; a recursive type is a key when nothing in it breaks the rule.

use super::{Body, Check, Class, Infer, Method, error};
use crate::ast::{Span, UnOp};
use crate::hir::{Arg, DefId, DefKind, Expr, ExprKind, Field};
use crate::ty::{Ty, TyId};
use quest_vm::consts::STRUCT_FIELDS_MAX;
use quest_vm::host::HostTy;
use std::collections::{BTreeMap, BTreeSet};

/// The fields of a struct, an event or an enum variant: names and types, in declaration order.
#[derive(Debug, Clone, Default)]
pub struct Shape {
    pub names: Vec<String>,
    pub tys: Vec<TyId>,
}

impl Infer<'_> {
    // User types.

    /// Lowers the fields of every host struct, struct, event and enum once, then reports the types no value
    /// can have: `too-many-fields`, `infinite-type`, and dict keys that are not keys.
    pub(super) fn user_types(&mut self) {
        self.early_keys = Some(Vec::new());
        let host = self.host;
        for s in &host.structs {
            let tys = s.fields.iter().map(|(_, t)| self.host_field(t)).collect();
            let names = s.fields.iter().map(|(n, _)| n.clone()).collect();
            self.host_structs.push(Shape { names, tys });
        }
        for e in &host.events {
            let tys = e.fields.iter().map(|(_, t)| self.host_field(t)).collect();
            let names = e.fields.iter().map(|(n, _)| n.clone()).collect();
            self.host_events.push(Shape { names, tys });
        }
        let krate = self.krate;
        for (i, def) in krate.defs.iter().enumerate() {
            let d = DefId(u32::try_from(i).unwrap_or(u32::MAX));
            let file = self.file_of(d);
            let shapes = match &def.kind {
                DefKind::Struct(fields) | DefKind::Event(fields) => {
                    vec![self.shape(fields, &def.name, def.span, file)]
                }
                DefKind::Enum(variants) => variants
                    .iter()
                    .map(|v| self.shape(&v.fields, &v.name, v.span, file))
                    .collect(),
                _ => continue,
            };
            self.adts.insert(d, shapes);
        }
        for (key, file, at) in self.early_keys.take().unwrap_or_default() {
            self.bad_keys.clear();
            if let Some(d) = self.key_error(key, file, at) {
                self.diags.push(d);
            }
        }
        self.infinite_types();
    }

    /// A struct's, event's or variant's fields; more than `STRUCT_FIELDS_MAX` is `too-many-fields` at `at`.
    fn shape(&mut self, fields: &[Field], name: &str, at: Span, file: &str) -> Shape {
        if fields.len() > STRUCT_FIELDS_MAX {
            self.diags.push(error(
                "too-many-fields",
                format!(
                    "`{name}` has {} fields; at most {STRUCT_FIELDS_MAX} are allowed",
                    fields.len()
                ),
                file,
                at,
            ));
        }
        let mut found = Vec::new();
        let tys = fields
            .iter()
            .map(|f| self.lower_kept(&f.ty, file, &mut found, "stored in a field"))
            .collect();
        self.diags.append(&mut found);
        Shape {
            names: fields.iter().map(|f| f.name.clone()).collect(),
            tys,
        }
    }

    /// A host type in a host struct's or event's fields (the open kinds name no field type).
    fn host_field(&mut self, t: &HostTy) -> TyId {
        self.host_ty(t, &mut |_, _| TyId::ERROR)
    }

    /// A host type; `open` gives the type of `Value`, `Infer` and `Event`.
    pub(super) fn host_ty(
        &mut self,
        t: &HostTy,
        open: &mut dyn FnMut(&mut Self, &HostTy) -> TyId,
    ) -> TyId {
        match t {
            HostTy::Unit => TyId::UNIT,
            HostTy::Int => TyId::INT,
            HostTy::Float => TyId::FLOAT,
            HostTy::Bool => TyId::BOOL,
            HostTy::Str => TyId::STR,
            HostTy::List(inner) => {
                let inner = self.host_ty(inner, open);
                self.types.intern(Ty::List(inner))
            }
            HostTy::Option(inner) => {
                let inner = self.host_ty(inner, open);
                self.types.intern(Ty::Option(inner))
            }
            HostTy::Struct(name) => {
                let host = self.host;
                if let Some(i) = host.structs.iter().position(|s| &s.name == name) {
                    self.types.intern(Ty::HostStruct(index_u32(i)))
                } else if let Some(i) = host.events.iter().position(|e| &e.name == name) {
                    self.types.intern(Ty::HostEvent(index_u32(i)))
                } else {
                    TyId::ERROR
                }
            }
            HostTy::Enum(name) => {
                let krate = self.krate;
                let host = self.host;
                host.enums
                    .iter()
                    .position(|e| &e.name == name)
                    .and_then(|i| krate.host_enums.get(i))
                    .map_or(TyId::ERROR, |d| self.types.intern(Ty::Adt(*d)))
            }
            HostTy::Value | HostTy::Infer | HostTy::Event => open(self, t),
        }
    }

    /// Reports every user type without a finite value, once per cycle of direct containment.
    fn infinite_types(&mut self) {
        // Each (type, shape) waits on its fields of a user type; a type is finite once one shape waits on none.
        let mut pending: BTreeMap<(DefId, usize), usize> = BTreeMap::new();
        let mut users: BTreeMap<DefId, Vec<(DefId, usize)>> = BTreeMap::new();
        let mut ready: Vec<DefId> = Vec::new();
        for (d, shapes) in &self.adts {
            if shapes.is_empty() {
                ready.push(*d);
            }
            for (i, shape) in shapes.iter().enumerate() {
                let mut count = 0usize;
                for t in &shape.tys {
                    if let Ty::Adt(x) = self.types.get(*t)
                        && self.adts.contains_key(&x)
                    {
                        count += 1;
                        users.entry(x).or_default().push((*d, i));
                    }
                }
                if count == 0 {
                    ready.push(*d);
                }
                pending.insert((*d, i), count);
            }
        }
        let mut finite: BTreeSet<DefId> = BTreeSet::new();
        while let Some(d) = ready.pop() {
            if !finite.insert(d) {
                continue;
            }
            for user in users.remove(&d).unwrap_or_default() {
                if let Some(count) = pending.get_mut(&user) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        ready.push(user.0);
                    }
                }
            }
        }
        // Without a finite value: every shape holds such a type directly. Follow the first one to a cycle.
        let mut infinite: Vec<(String, DefId)> = self
            .adts
            .keys()
            .filter(|d| !finite.contains(d))
            .map(|d| (self.krate.qualified_name(*d), *d))
            .collect();
        infinite.sort();
        let mut covered: BTreeSet<DefId> = BTreeSet::new();
        for (_, start) in infinite {
            let mut path: Vec<DefId> = Vec::new();
            let mut on_path: BTreeMap<DefId, usize> = BTreeMap::new();
            let mut cur = start;
            while !covered.contains(&cur) && !on_path.contains_key(&cur) {
                on_path.insert(cur, path.len());
                path.push(cur);
                let next = self.adts.get(&cur).and_then(|shapes| {
                    shapes
                        .first()?
                        .tys
                        .iter()
                        .find_map(|t| match self.types.get(*t) {
                            Ty::Adt(x) if self.adts.contains_key(&x) && !finite.contains(&x) => {
                                Some(x)
                            }
                            _ => None,
                        })
                });
                match next {
                    Some(x) => cur = x,
                    None => break,
                }
            }
            let cycle_from = on_path.get(&cur).copied();
            covered.extend(path.iter().copied());
            let Some(from) = cycle_from else {
                continue;
            };
            let cycle = &path[from..];
            let mut names: Vec<(String, usize)> = cycle
                .iter()
                .enumerate()
                .map(|(i, d)| (self.krate.qualified_name(*d), i))
                .collect();
            let first = names.iter().min().map_or(0, |(_, i)| *i);
            names.rotate_left(first);
            let mut text: Vec<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
            let head = text.first().copied().unwrap_or("");
            text.push(head);
            let at = cycle.get(first).copied().unwrap_or(start);
            let file = self.file_of(at);
            self.diags.push(error(
                "infinite-type",
                format!(
                    "`{head}` has no finite value: it always contains itself ({}); hold it in an `Option`, a `list` or a `dict`",
                    text.join(" -> ")
                ),
                file,
                self.def_span(at),
            ));
        }
    }

    // Dict keys and storable values.

    /// A dict's key type met in an annotation: checked now, or once every user type is lowered.
    pub(super) fn annotated_key(
        &mut self,
        key: TyId,
        file: &str,
        at: Span,
        out: &mut Vec<super::Diagnostic>,
    ) {
        match &mut self.early_keys {
            Some(early) => early.push((key, file.to_string(), at)),
            None => out.extend(self.key_error(key, file.to_string(), at)),
        }
    }

    /// `bad-dict-key` when `key` cannot be a dict key, once per key type in a body (an annotated `dict` and
    /// the `{}` it holds share the mistake).
    pub(super) fn key_error(
        &mut self,
        key: TyId,
        file: String,
        at: Span,
    ) -> Option<super::Diagnostic> {
        if self.is_key(key) {
            return None;
        }
        let key = self.zonk(key);
        if !self.bad_keys.insert(key) {
            return None;
        }
        let shown = self.show(key);
        Some(error(
            "bad-dict-key",
            format!(
                "`{shown}` cannot be a dict key: keys are `int`, `bool`, `str`, and structs and enums made only of those"
            ),
            &file,
            at,
        ))
    }

    /// Marks the bad key types of the dicts inside `ty` as reported.
    pub(super) fn seen_bad_keys(&mut self, ty: TyId) {
        let mut seen: BTreeSet<TyId> = BTreeSet::new();
        let mut stack = vec![ty];
        while let Some(t) = stack.pop() {
            if !seen.insert(t) {
                continue;
            }
            let shape = self.types.get(t);
            if let Ty::Dict(k, _) = shape
                && !self.is_key(k)
            {
                self.bad_keys.insert(k);
            }
            stack.extend(shape.children());
        }
    }

    /// Whether a type can be a dict key (an unknown part is not decided here).
    pub(super) fn is_key(&mut self, ty: TyId) -> bool {
        let mut seen: BTreeSet<TyId> = BTreeSet::new();
        let mut stack = vec![ty];
        while let Some(t) = stack.pop() {
            let t = self.shallow(t);
            if !seen.insert(t) {
                continue;
            }
            match self.types.get(t) {
                Ty::Int | Ty::Bool | Ty::Str | Ty::Error | Ty::Never | Ty::Var(_) => {}
                Ty::Option(inner) => stack.push(inner),
                Ty::Adt(d) => {
                    for shape in self.adts.get(&d).into_iter().flatten() {
                        stack.extend(shape.tys.iter().copied());
                    }
                }
                Ty::HostStruct(i) => {
                    stack.extend(
                        self.host_structs
                            .get(i as usize)
                            .into_iter()
                            .flat_map(|s| s.tys.iter().copied()),
                    );
                }
                Ty::HostEvent(i) => {
                    stack.extend(
                        self.host_events
                            .get(i as usize)
                            .into_iter()
                            .flat_map(|s| s.tys.iter().copied()),
                    );
                }
                Ty::Unit
                | Ty::Float
                | Ty::List(_)
                | Ty::Dict(_, _)
                | Ty::Promise(_)
                | Ty::Ref(_)
                | Ty::Fallible(_, _)
                | Ty::Caught(_) => {
                    return false;
                }
            }
        }
        true
    }

    /// Whether a value of this type can be stored or sent: no `Promise` and no reference in it.
    pub(super) fn is_storable(&mut self, ty: TyId) -> bool {
        let mut seen: BTreeSet<TyId> = BTreeSet::new();
        let mut stack = vec![ty];
        while let Some(t) = stack.pop() {
            let t = self.shallow(t);
            if !seen.insert(t) {
                continue;
            }
            match self.types.get(t) {
                Ty::Promise(_) | Ty::Ref(_) => return false,
                Ty::Adt(d) => {
                    for shape in self.adts.get(&d).into_iter().flatten() {
                        stack.extend(shape.tys.iter().copied());
                    }
                }
                other => stack.extend(other.children()),
            }
        }
        true
    }

    // Constructions.

    /// `Name(…)` for a struct, an event, a variant or a host struct or event: checks the fields, returns `ty`.
    pub(super) fn construct(
        &mut self,
        b: &mut Body,
        call: &Expr,
        name: &str,
        shape: &Shape,
        args: &[Arg],
        ty: TyId,
    ) -> TyId {
        let mut given: Vec<Option<Span>> = vec![None; shape.names.len()];
        let mut positional = 0usize;
        for a in args {
            let found = self.expr(b, &a.value);
            let index = match &a.name {
                Some(field) => match shape.names.iter().position(|n| n == field) {
                    Some(i) => i,
                    None => {
                        b.diags.push(error(
                            "unknown-field",
                            format!("`{name}` has no field `{field}`"),
                            b.file,
                            a.value.span,
                        ));
                        self.unify(found, TyId::ERROR);
                        continue;
                    }
                },
                None => {
                    positional += 1;
                    positional - 1
                }
            };
            let Some(slot) = given.get_mut(index) else {
                b.diags.push(error(
                    "arg-count",
                    match shape.names.len() {
                        1 => format!("`{name}` has 1 field, more are given"),
                        n => format!("`{name}` has {n} fields, more are given"),
                    },
                    b.file,
                    a.value.span,
                ));
                self.unify(found, TyId::ERROR);
                continue;
            };
            if slot.replace(a.value.span).is_some() {
                let field = shape.names.get(index).map_or("", String::as_str);
                b.diags.push(error(
                    "arg-count",
                    format!("the field `{field}` of `{name}` is given twice"),
                    b.file,
                    a.value.span,
                ));
                self.unify(found, TyId::ERROR);
                continue;
            }
            let wanted = shape.tys.get(index).copied().unwrap_or(TyId::ERROR);
            self.expect(b, wanted, found, a.value.span);
        }
        let missing: Vec<String> = shape
            .names
            .iter()
            .zip(&given)
            .filter(|(_, g)| g.is_none())
            .map(|(n, _)| format!("`{n}`"))
            .collect();
        if !missing.is_empty() {
            b.diags.push(error(
                "missing-field",
                format!("`{name}` needs every field: {} missing", missing.join(", ")),
                b.file,
                call.span,
            ));
        }
        ty
    }

    // Fields, indexing, slices, `in`.

    /// `ty` without the references in front of it (fields, methods and indexing see through them).
    pub(super) fn deref(&mut self, ty: TyId) -> (TyId, bool) {
        let mut ty = self.shallow(ty);
        let mut through = false;
        // A reference to a reference is never built (references are not values); the bound is a guard.
        for _ in 0..8 {
            match self.types.get(ty) {
                Ty::Ref(inner) => {
                    ty = self.shallow(inner);
                    through = true;
                }
                _ => break,
            }
        }
        (ty, through)
    }

    /// Something (`what`: a field, a method, indexing, …) needs the type of a value that is still unknown:
    /// `needs-annotation`, at the parameter it comes from when there is one.
    pub(super) fn unknown_receiver(&mut self, b: &mut Body, ty: TyId, at: Span, what: &str) {
        match self.param_in(b, ty) {
            Some(local) => {
                if b.conflicted.insert(local) {
                    let (name, span) = self
                        .krate
                        .local(local)
                        .map_or((String::new(), Span::default()), |l| (l.name.clone(), l.span));
                    let mut d = error(
                        "needs-annotation",
                        format!("the type of `{name}` must be known for {what}: annotate it (`{name}: T`)"),
                        b.file,
                        span,
                    );
                    d.notes.push(format!("used at line {}, column {}", at.line, at.col));
                    b.diags.push(d);
                }
            }
            None => b.diags.push(error(
                "needs-annotation",
                format!("the type of this value must be known for {what}: annotate the binding it comes from"),
                b.file,
                at,
            )),
        }
        self.default_vars(ty);
    }

    /// `x.name`, not called.
    pub(super) fn field(&mut self, b: &mut Body, e: &Expr, x: &Expr, name: &str) -> TyId {
        let ty = self.expr(b, x);
        let (ty, _) = self.deref(ty);
        let shape = match self.types.get(ty) {
            Ty::Error | Ty::Never => return TyId::ERROR,
            Ty::Var(_) => {
                self.unknown_receiver(b, ty, x.span, &format!("the field `{name}`"));
                return TyId::ERROR;
            }
            // An objective's reads (`done`, `failed`, `count`) are typed with the quest checks (Task 39).
            Ty::Adt(d) if Some(d) == self.krate.builtins.objective => return TyId::ERROR,
            Ty::Adt(d) => match self.krate.def(d).map(|def| &def.kind) {
                Some(DefKind::Struct(_) | DefKind::Event(_)) => {
                    self.adts.get(&d).and_then(|s| s.first()).cloned()
                }
                _ => None,
            },
            Ty::HostStruct(i) => self.host_structs.get(i as usize).cloned(),
            Ty::HostEvent(i) => self.host_events.get(i as usize).cloned(),
            _ => None,
        };
        let found = shape.as_ref().and_then(|s| {
            let i = s.names.iter().position(|n| n == name)?;
            s.tys.get(i).copied()
        });
        if let Some(t) = found {
            return t;
        }
        // A host struct's property (`p.alive`), or a method named without its call.
        if let Ty::HostStruct(i) = self.types.get(ty) {
            let host = self.host;
            let owner = host.structs.get(i as usize).map(|s| s.name.as_str());
            if let Some(f) = host
                .fns
                .iter()
                .position(|f| f.receiver.as_deref() == owner && f.name == name)
            {
                let owner = owner.unwrap_or_default();
                if host.fns.get(f).is_some_and(|hf| hf.property) {
                    let f = u32::try_from(f).unwrap_or(u32::MAX);
                    self.methods.insert(e.id, Method::Host(f));
                    return self.host_call(b, e, f, &[]);
                }
                b.diags.push(error(
                    "missing-call",
                    format!("`{owner}.{name}` is a method, not a property: call it (`.{name}(…)`)"),
                    b.file,
                    e.span,
                ));
                return TyId::ERROR;
            }
        }
        let shown = self.show(ty);
        let is_method = matches!(self.types.get(ty), Ty::Adt(d) if self.krate.methods.contains_key(&(d, name.to_string())));
        let message = if is_method {
            format!("`{name}` is a method of `{shown}`, not a field: call it (`.{name}(…)`)")
        } else if shape.is_none() {
            format!("`{shown}` has no fields (it has no field `{name}`)")
        } else {
            format!("`{shown}` has no field `{name}`")
        };
        b.diags
            .push(error("unknown-field", message, b.file, e.span));
        TyId::ERROR
    }

    /// `x[i]`: a list's element or a dict's value.
    pub(super) fn index(&mut self, b: &mut Body, e: &Expr, x: &Expr, i: &Expr) -> TyId {
        let ty = self.expr(b, x);
        let (ty, _) = self.deref(ty);
        let (wanted, out) = match self.types.get(ty) {
            Ty::List(t) => (TyId::INT, t),
            Ty::Dict(k, v) => (k, v),
            Ty::Error | Ty::Never => (TyId::ERROR, TyId::ERROR),
            Ty::Var(_) => {
                self.unknown_receiver(b, ty, x.span, "indexing");
                (TyId::ERROR, TyId::ERROR)
            }
            Ty::Str => {
                self.mismatch(
                    b,
                    e.span,
                    "a `str` has no indexing by character: use `split`, `starts_with`, `ends_with`, `contains` or `len`".to_string(),
                );
                (TyId::ERROR, TyId::ERROR)
            }
            _ => {
                let shown = self.show(ty);
                self.mismatch(
                    b,
                    e.span,
                    format!("`{shown}` cannot be indexed: only a list or a dict"),
                );
                (TyId::ERROR, TyId::ERROR)
            }
        };
        let found = self.expr(b, i);
        self.expect(b, wanted, found, i.span);
        out
    }

    /// `x[a:b]`: a list's slice.
    pub(super) fn slice(
        &mut self,
        b: &mut Body,
        e: &Expr,
        x: &Expr,
        bounds: [Option<&Expr>; 2],
    ) -> TyId {
        let ty = self.expr(b, x);
        let (inner, _) = self.deref(ty);
        let out = match self.types.get(inner) {
            Ty::List(_) => inner,
            Ty::Error | Ty::Never => TyId::ERROR,
            Ty::Var(_) => {
                self.unknown_receiver(b, inner, x.span, "a slice");
                TyId::ERROR
            }
            Ty::Str => {
                self.mismatch(
                    b,
                    e.span,
                    "a `str` has no slicing by character: use `split`, `starts_with`, `ends_with`, `contains` or `len`".to_string(),
                );
                TyId::ERROR
            }
            _ => {
                let shown = self.show(inner);
                self.mismatch(
                    b,
                    e.span,
                    format!("`{shown}` cannot be sliced: only a list"),
                );
                TyId::ERROR
            }
        };
        for y in bounds.into_iter().flatten() {
            let found = self.expr(b, y);
            self.expect(b, TyId::INT, found, y.span);
        }
        out
    }

    /// `x in c`, `x not in c`: an element of a list, a key of a dict.
    pub(super) fn contains(&mut self, b: &mut Body, x: &Expr, c: &Expr, at: Span) -> TyId {
        let found = self.expr(b, x);
        let ty = self.expr(b, c);
        let (ty, _) = self.deref(ty);
        let wanted = match self.types.get(ty) {
            Ty::List(t) => t,
            Ty::Dict(k, _) => k,
            Ty::Error | Ty::Never => TyId::ERROR,
            Ty::Var(_) => {
                self.unknown_receiver(b, ty, c.span, "`in`");
                TyId::ERROR
            }
            Ty::Str => {
                self.mismatch(
                    b,
                    at,
                    "`in` takes a list or a dict; for a `str` use `.contains(…)`".to_string(),
                );
                TyId::ERROR
            }
            _ => {
                let shown = self.show(ty);
                self.mismatch(b, at, format!("`in` takes a list or a dict, not `{shown}`"));
                TyId::ERROR
            }
        };
        self.expect(b, wanted, found, x.span);
        TyId::BOOL
    }

    /// A dict literal's key type: a key once inference is done.
    pub(super) fn check_key(&mut self, b: &mut Body, key: TyId, at: Span) {
        b.checks.push(Check {
            ty: key,
            class: Class::Key,
            op: "a dict key",
            at,
        });
    }

    /// Whether an expression names a place a reference can point at: a binding, a top-level `let`, a field
    /// or element of a place, or `*r`.
    pub(super) fn is_place(&self, e: &Expr) -> bool {
        let mut e = e;
        loop {
            match &e.kind {
                ExprKind::Local(_) | ExprKind::Unary(UnOp::Deref, _) => return true,
                ExprKind::Def(d) => {
                    return matches!(
                        self.krate.def(*d).map(|def| &def.kind),
                        Some(DefKind::Global(_))
                    );
                }
                ExprKind::Field(x, _) | ExprKind::Index(x, _) => e = x,
                _ => return false,
            }
        }
    }
}

fn index_u32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}
