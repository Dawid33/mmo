//! Errors (spec §5.7; plan decisions 2 and 11): error names and their data, error sets, `raise`, `?`, `else`.
//!
//! **Error names are global** and each has one data shape in the whole program ([`ErrShape`]): its fields, by
//! name, in order. A shape is fixed by the first site that gives it, in this order: the host's errors
//! (`HostApi::errors`; a name a host function raises that the host does not list is data-less), the
//! members with data of the `error` set items (in item order), then `raise` sites as bodies are finished
//! (callee-first, so deterministic). Every later site must agree: same fields, same types, data or no data;
//! otherwise it is `error-shape` there. A bare member of an `error` set (`error E = ChestEmpty | …`) declares
//! a data-less error, as `raise ChestEmpty` does; `raise X()` is `raise X`. A `raise` with data of a name with no shape yet must name its fields
//! (`raise NotEnough(have=3)`): positional data needs a known shape.
//!
//! **Generic host data** (decision 11): a host error field of type `Infer` (`Changed(current)`) is open until
//! its first use, which fixes it for the whole program: a `raise` giving it, or a call of a host function
//! raising the error — the field then takes the type of the call's first `Value` argument (the compared
//! value: `world.compare_and_set`'s `expected`), or else of its `Infer` result. A use at another type is
//! `error-shape`.
//!
//! A body's new shapes and instantiations (`Body::shapes`, an overlay over `Infer::errors`) and its raise
//! sites are kept only when the body finishes (`commit`): a body that waits and is typed again leaves
//! nothing behind.
//!
//! **Error sets** are sorted, deduplicated name lists (a [`SetId`](crate::ty::SetId) inside types,
//! `Sig::raises` in signatures). A function's set is inferred — the names it raises and the sets of what it
//! propagates with `?` — or pinned by `raises E` (a set item, expanded through its member sets, or one
//! name): then raising outside it is `raises-outside-pinned` at the `raise` / `?`, and the pinned set is
//! what callers see. Callee-first order makes every callee's set final before its callers are typed.
//!
//! **Must handle** (decision 2): a call of a function with a non-empty set, or of a raising host query or
//! command, has the type `T raises S` ([`Ty::Fallible`]). A promise's set is the second child of
//! `Promise[T raises S]` and is open (see `ty.rs`): a raising host promise starts with the host's list, a
//! written `Promise[T]` with none, and promises that meet (a list, an argument, `all`) share the union; `await`
//! (Task 24) turns it into the fallible result. A fallible value is consumed where it is produced: by `?`,
//! by `else`, or as a `match` scrutinee (`Infer::expr_fallible`); anywhere else (`Infer::expr`) it is
//! `unhandled-fallible`, and its value type is used from there on. `let _ = f()` is no exception. An error
//! escaping `main`, a handler or a top-level initialiser is allowed (it is reported at run time).
//!
//! **`?`** propagates a fallible value's set into the function's, or (on an `Option`) returns `None` from a
//! function returning an `Option`. **`else`** falls back on a value, on a way out (`return`, `raise`, …), or
//! on an `else err:` block whose last expression statement is its value unless the block leaves; `err` is
//! the caught error ([`Ty::Caught`]), which `raise err` re-raises. Either on anything that cannot fail is
//! `not-fallible`.
//!
//! **Typos**: a raised name that is never matched (in a pattern) or declared (a set member, a pinned name,
//! a host error) anywhere, within `SUGGEST_DISTANCE_MAX` edits of one that is, gets the warning
//! `error-typo` at its first raise site.

use super::{Body, Infer, error};
use crate::ast::Span;
use crate::diag::{Diagnostic, Severity};
use crate::hir::{
    Arg, Ctor, DefId, DefKind, ErrorMember, Expr, ExprKind, Pattern, PatternKind, Raises, Stmt,
    StmtKind,
};
use crate::ty::{Ty, TyId};
use quest_vm::host::{HostFnKind, HostTy};
use std::collections::{BTreeMap, BTreeSet};

/// An error name's data.
#[derive(Debug, Clone)]
pub(super) struct ErrShape {
    pub(super) fields: Vec<ErrField>,
    /// Where the shape was fixed, for messages (`declared by the host`, `raised at main.qs:3`).
    pub(super) origin: String,
}

#[derive(Debug, Clone)]
pub(super) struct ErrField {
    pub(super) name: String,
    /// `None`: a generic host field not used yet.
    pub(super) ty: Option<TyId>,
    /// A generic host field (`Infer`): its first use fixes it.
    pub(super) generic: bool,
}

impl<'k> Infer<'k> {
    // Shapes known before any body.

    /// Registers the host's errors and the `error` items' members; collects the declared names.
    pub(super) fn declared_errors(&mut self) {
        let host = self.host;
        for e in &host.errors {
            let fields = e
                .fields
                .iter()
                .map(|(name, t)| {
                    let generic = matches!(t, HostTy::Infer | HostTy::Value | HostTy::Event);
                    let ty = if generic {
                        None
                    } else {
                        Some(self.host_ty(t, &mut |_, _| TyId::ERROR))
                    };
                    ErrField {
                        name: name.clone(),
                        ty,
                        generic,
                    }
                })
                .collect();
            self.named.insert(e.name.clone());
            self.errors.entry(e.name.clone()).or_insert(ErrShape {
                fields,
                origin: "declared by the host".to_string(),
            });
        }
        for f in &host.fns {
            for name in &f.raises {
                self.errors.entry(name.clone()).or_insert(ErrShape {
                    fields: Vec::new(),
                    origin: "raised by the host".to_string(),
                });
            }
        }
        let krate = self.krate;
        for (i, def) in krate.defs.iter().enumerate() {
            let d = DefId(u32::try_from(i).unwrap_or(u32::MAX));
            match &def.kind {
                DefKind::ErrorSet(members) => {
                    let file = self.file_of(d);
                    for m in members {
                        let ErrorMember::Error(v) = m else {
                            continue;
                        };
                        // A member without fields declares a data-less error.
                        self.named.insert(v.name.clone());
                        let mut found = Vec::new();
                        let fields: Vec<ErrField> = v
                            .fields
                            .iter()
                            .map(|f| ErrField {
                                name: f.name.clone(),
                                ty: Some(self.lower(&f.ty, file, &mut found)),
                                generic: false,
                            })
                            .collect();
                        self.diags.append(&mut found);
                        let origin = format!("declared at {file}:{}", v.span.line);
                        match self.errors.get(&v.name).cloned() {
                            None => {
                                self.errors
                                    .insert(v.name.clone(), ErrShape { fields, origin });
                            }
                            Some(known) => {
                                if let Some(why) = self.declared_conflict(&v.name, &known, &fields)
                                {
                                    let message = format!(
                                        "`{}` has one data shape in the whole program: {why} ({})",
                                        v.name, known.origin
                                    );
                                    self.diags.push(error("error-shape", message, file, v.span));
                                }
                            }
                        }
                    }
                }
                DefKind::Fn(f) => {
                    if let Some(Raises::Error(name)) = &f.raises {
                        self.named.insert(name.clone());
                    }
                }
                _ => {}
            }
        }
    }

    /// Why a declared shape disagrees with a known one, if it does.
    fn declared_conflict(
        &mut self,
        name: &str,
        known: &ErrShape,
        fields: &[ErrField],
    ) -> Option<String> {
        let same_names = known.fields.len() == fields.len()
            && known
                .fields
                .iter()
                .zip(fields)
                .all(|(a, b)| a.name == b.name);
        if !same_names {
            return Some(format!(
                "it is `{}` there",
                self.shape_text(name, &known.fields)
            ));
        }
        for (a, b) in known.fields.iter().zip(fields) {
            if let (Some(x), Some(y)) = (a.ty, b.ty)
                && !self.unify(x, y)
            {
                let (x, y) = (self.show(x), self.show(y));
                return Some(format!("its field `{}` is `{x}` there, not `{y}`", a.name));
            }
        }
        None
    }

    /// `Name(a: int, b: str)` (or the field list alone when `name` is empty).
    fn shape_text(&mut self, name: &str, fields: &[ErrField]) -> String {
        if fields.is_empty() {
            return format!("{name} without data");
        }
        let fields: Vec<String> = fields
            .iter()
            .map(|f| match f.ty {
                Some(t) => format!("{}: {}", f.name, self.show(t)),
                None => f.name.clone(),
            })
            .collect();
        format!("{name}({})", fields.join(", "))
    }

    /// The names of a pinned set, sorted: a set item expanded through its member sets, or one name.
    pub(super) fn pinned_names(&self, r: &Raises) -> Vec<String> {
        let start = match r {
            Raises::Error(name) => return vec![name.clone()],
            Raises::Set(d) => *d,
        };
        let mut names = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut stack = vec![start];
        while let Some(d) = stack.pop() {
            if !seen.insert(d) {
                continue;
            }
            let Some(DefKind::ErrorSet(members)) = self.krate.def(d).map(|def| &def.kind) else {
                continue;
            };
            for m in members {
                match m {
                    ErrorMember::Set(inner, _) => stack.push(*inner),
                    ErrorMember::Error(v) => {
                        names.insert(v.name.clone());
                    }
                }
            }
        }
        names.into_iter().collect()
    }

    /// The type of a call returning `ret` that can fail with `raises`.
    pub(super) fn fallible(&mut self, ret: TyId, raises: &[String]) -> TyId {
        if raises.is_empty() {
            return ret;
        }
        let set = self.types.set(raises.iter().cloned());
        self.types.intern(Ty::Fallible(ret, set))
    }

    /// The type of a call of the host function `f`, given the type of its value `ret` (a promise's: the
    /// promised value's) and of its parameters `params`: `T raises S` for a raising query or command,
    /// `Promise[T raises S]` with an open set for a promise. The generic fields of the errors it raises are fixed
    /// here at their first use (see the module doc).
    pub(super) fn host_fallible(
        &mut self,
        b: &mut Body,
        call: &Expr,
        f: u32,
        params: &[TyId],
        ret: TyId,
    ) -> TyId {
        let host = self.host;
        let Some(hf) = host.fns.get(f as usize) else {
            return ret;
        };
        let compared = hf
            .params
            .iter()
            .position(|(_, t)| *t == HostTy::Value)
            .and_then(|i| params.get(i).copied())
            .or((hf.ret == HostTy::Infer).then_some(ret));
        if let Some(ty) = compared {
            for name in &hf.raises {
                self.instantiate(b, name, ty, call.span);
            }
        }
        match hf.kind {
            // An open set: it unions with the sets of the promises it meets.
            HostFnKind::Promise => {
                let set = self.fresh_set(hf.raises.clone());
                self.types.intern(Ty::Promise(ret, set))
            }
            HostFnKind::Query | HostFnKind::Command => self.fallible(ret, &hf.raises),
        }
    }

    /// Fixes (or checks) the generic fields of `name` to `ty`, used at `at`.
    fn instantiate(&mut self, b: &mut Body, name: &str, ty: TyId, at: Span) {
        let Some(mut shape) = self.shape_of(b, name) else {
            return;
        };
        if !shape.fields.iter().any(|f| f.generic) {
            return;
        }
        let mut changed = false;
        for i in 0..shape.fields.len() {
            let Some(field) = shape.fields.get(i).cloned() else {
                continue;
            };
            if !field.generic {
                continue;
            }
            match field.ty {
                None => {
                    if let Some(f) = shape.fields.get_mut(i) {
                        f.ty = Some(ty);
                    }
                    changed = true;
                }
                Some(fixed) => {
                    if !self.unify(fixed, ty) {
                        let (was, now) = (self.show(fixed), self.show(ty));
                        let message = format!(
                            "`{name}` carries `{}` at one type in the whole program: `{was}` ({}), here `{now}`",
                            field.name, shape.origin
                        );
                        b.diags.push(error("error-shape", message, b.file, at));
                        return;
                    }
                }
            }
        }
        if changed {
            shape.origin = format!("its data's type was fixed at {}:{}", b.file, at.line);
            b.shapes.insert(name.to_string(), shape);
        }
    }

    /// The shape of `name` as the body sees it: its own new shapes first.
    pub(super) fn shape_of(&self, b: &Body, name: &str) -> Option<ErrShape> {
        b.shapes
            .get(name)
            .or_else(|| self.errors.get(name))
            .cloned()
    }

    // `raise`.

    /// `raise x`: an error name, with or without data, or a caught error.
    pub(super) fn raise(&mut self, b: &mut Body, x: &Expr) {
        match &x.kind {
            ExprKind::ErrorName(name) => {
                self.raise_data(b, name, None, x.span);
                self.raised_name(b, name, x);
            }
            ExprKind::Call(callee, args) if matches!(callee.kind, ExprKind::ErrorName(_)) => {
                self.record(b, callee, TyId::ERROR);
                if let ExprKind::ErrorName(name) = &callee.kind {
                    self.raise_data(b, name, Some(args), x.span);
                    self.raised_name(b, name, x);
                }
            }
            _ => {
                let ty = self.expr(b, x);
                let ty = self.shallow(ty);
                match self.types.get(ty) {
                    Ty::Caught(set) => {
                        for name in self.types.names(set).to_vec() {
                            add_raise(b, name, x.span);
                        }
                    }
                    Ty::Error | Ty::Never => {}
                    Ty::Var(_) => self.unknown_receiver(b, ty, x.span, "`raise`"),
                    _ => {
                        let found = self.show(ty);
                        self.mismatch(
                            b,
                            x.span,
                            format!(
                                "`raise` takes an error name (`raise Oops`) or a caught error, not `{found}`"
                            ),
                        );
                    }
                }
            }
        }
    }

    /// Records a raised name: its set entry, its site, the operand's type.
    fn raised_name(&mut self, b: &mut Body, name: &str, x: &Expr) {
        add_raise(b, name.to_string(), x.span);
        b.raised.push((name.to_string(), x.span));
        let set = self.types.set([name.to_string()]);
        let ty = self.types.intern(Ty::Caught(set));
        self.record(b, x, ty);
    }

    /// Checks a raise's data (`args`: `None` without parentheses) against the name's shape, or fixes it.
    fn raise_data(&mut self, b: &mut Body, name: &str, args: Option<&[Arg]>, at: Span) {
        // Empty parentheses are no data.
        let args = args.filter(|a| !a.is_empty());
        let found: Vec<(Option<&str>, TyId, Span)> = args
            .unwrap_or_default()
            .iter()
            .map(|a| (a.name.as_deref(), self.expr(b, &a.value), a.value.span))
            .collect();
        let shape_err = |b: &mut Body, why: String| {
            b.diags.push(error(
                "error-shape",
                format!("`{name}` has one data shape in the whole program: {why}"),
                b.file,
                at,
            ));
        };
        let Some(mut shape) = self.shape_of(b, name) else {
            // The first site: it fixes the shape.
            let mut fields: Vec<ErrField> = Vec::new();
            for (field, ty, _) in &found {
                match field {
                    Some(n) if fields.iter().all(|f| f.name != *n) => fields.push(ErrField {
                        name: n.to_string(),
                        ty: Some(*ty),
                        generic: false,
                    }),
                    Some(n) => {
                        return shape_err(b, format!("its field `{n}` is given twice"));
                    }
                    None => {
                        return shape_err(
                            b,
                            format!(
                                "name its fields where it is first raised (`raise {name}(field=…)`)"
                            ),
                        );
                    }
                }
            }
            let origin = format!("raised at {}:{}", b.file, at.line);
            b.shapes
                .insert(name.to_string(), ErrShape { fields, origin });
            return;
        };
        let origin = shape.origin.clone();
        let text = self.shape_text(name, &shape.fields);
        match args {
            None if shape.fields.is_empty() => return,
            None => {
                return shape_err(
                    b,
                    format!("it is `{text}` ({origin}), so raise it with its data"),
                );
            }
            Some(_) if shape.fields.is_empty() => {
                return shape_err(b, format!("it carries no data ({origin})"));
            }
            Some(_) => {}
        }
        let mut given = vec![false; shape.fields.len()];
        let mut positional = 0usize;
        let mut instantiated = false;
        for (field, ty, _) in &found {
            let index = match field {
                Some(n) => shape.fields.iter().position(|f| f.name == *n),
                None => {
                    positional += 1;
                    Some(positional - 1).filter(|i| *i < shape.fields.len())
                }
            };
            let Some(index) = index else {
                return shape_err(
                    b,
                    format!("it is `{text}` ({origin}): no field fits this one"),
                );
            };
            if given.get(index).copied().unwrap_or(true) {
                return shape_err(
                    b,
                    format!("it is `{text}` ({origin}): a field is given twice"),
                );
            }
            if let Some(g) = given.get_mut(index) {
                *g = true;
            }
            let Some(f) = shape.fields.get_mut(index) else {
                continue;
            };
            match f.ty {
                Some(want) => {
                    if !self.unify(want, *ty) {
                        let (want, ty) = (self.show(want), self.show(*ty));
                        let fname = f.name.clone();
                        return shape_err(
                            b,
                            format!("its field `{fname}` is `{want}` ({origin}), not `{ty}`"),
                        );
                    }
                }
                None => {
                    f.ty = Some(*ty);
                    instantiated = true;
                }
            }
        }
        if given.iter().any(|g| !g) {
            return shape_err(b, format!("it is `{text}` ({origin}): fields are missing"));
        }
        if instantiated {
            shape.origin = format!("its data's type was fixed at {}:{}", b.file, at.line);
            b.shapes.insert(name.to_string(), shape);
        }
    }

    // `?` and `else`.

    /// `x?`.
    pub(super) fn try_op(&mut self, b: &mut Body, e: &Expr, x: &Expr) -> TyId {
        let ty = self.expr_fallible(b, x);
        let ty = self.shallow(ty);
        match self.types.get(ty) {
            Ty::Fallible(value, set) => {
                for name in self.types.names(set).to_vec() {
                    add_raise(b, name, e.span);
                }
                value
            }
            Ty::Option(value) => {
                let inner = self.fresh();
                let want = self.types.intern(Ty::Option(inner));
                if !self.unify(b.ret, want) {
                    let ret = self.show(b.ret);
                    self.mismatch(
                        b,
                        e.span,
                        format!(
                            "`?` on an `Option` returns `None` from this function, which returns `{ret}`, not an `Option`"
                        ),
                    );
                }
                value
            }
            Ty::Error | Ty::Never => TyId::ERROR,
            Ty::Var(_) => {
                self.unknown_receiver(b, ty, x.span, "`?`");
                TyId::ERROR
            }
            _ => {
                let found = self.show(ty);
                b.diags.push(error(
                    "not-fallible",
                    format!("`?` takes something that can fail or an `Option`; this is `{found}`, which cannot fail"),
                    b.file,
                    e.span,
                ));
                TyId::ERROR
            }
        }
    }

    /// `x else fallback`.
    pub(super) fn else_op(&mut self, b: &mut Body, x: &Expr, fallback: &Expr) -> TyId {
        let ty = self.expr_fallible(b, x);
        let ty = self.shallow(ty);
        let (value, caught) = match self.types.get(ty) {
            Ty::Fallible(value, set) => (value, self.types.intern(Ty::Caught(set))),
            Ty::Option(value) => (value, TyId::UNIT),
            Ty::Error | Ty::Never => (TyId::ERROR, TyId::ERROR),
            Ty::Var(_) => {
                self.unknown_receiver(b, ty, x.span, "`else`");
                (TyId::ERROR, TyId::ERROR)
            }
            _ => {
                let found = self.show(ty);
                b.diags.push(error(
                    "not-fallible",
                    format!("`else` takes something that can fail or an `Option`; this is `{found}`, which cannot fail"),
                    b.file,
                    x.span,
                ));
                (TyId::ERROR, TyId::ERROR)
            }
        };
        let found = match &fallback.kind {
            ExprKind::FallbackBlock(local, body) => {
                self.set_local(*local, caught);
                let ty = self.fallback_block(b, body, value);
                self.record(b, fallback, ty);
                ty
            }
            _ => {
                let found = self.expr(b, fallback);
                if !self.unify(value, found) {
                    let (w, f) = (self.show(value), self.show(found));
                    let message =
                        format!("the fallback is `{f}`, but the value it replaces is `{w}`");
                    self.disagree(b, value, found, fallback.span, message);
                }
                found
            }
        };
        if self.shallow(value) == TyId::NEVER {
            found
        } else {
            value
        }
    }

    /// An `else err:` block: its value (`want`), or `!` when it leaves.
    fn fallback_block(&mut self, b: &mut Body, body: &[Stmt], want: TyId) -> TyId {
        if self.block(b, body) {
            return TyId::NEVER;
        }
        let Some(last) = body.last() else {
            return TyId::ERROR;
        };
        let StmtKind::Expr(e) = &last.kind else {
            let shown = self.show(want);
            self.mismatch(
                b,
                last.span,
                format!(
                    "an `else` block ends with its value (`{shown}`) or leaves (`return`, `raise`, `break`, `continue`)"
                ),
            );
            return TyId::ERROR;
        };
        let found = self.expr_ty(e.id);
        let found = self.value_of(found);
        if !self.unify(want, found) {
            let (w, f) = (self.show(want), self.show(found));
            let message = format!(
                "this `else` block's value is `{f}`, but the value it replaces is `{w}`: end the block with a value of that type, or leave it"
            );
            self.disagree(b, want, found, e.span, message);
        }
        want
    }

    /// A fallible type's value type; any other type itself.
    pub(super) fn value_of(&mut self, ty: TyId) -> TyId {
        let end = self.shallow(ty);
        match self.types.get(end) {
            Ty::Fallible(value, _) => value,
            _ => ty,
        }
    }

    /// `unhandled-fallible` when `ty` is a fallible result met where it is not consumed; its value type.
    pub(super) fn handled(&mut self, b: &mut Body, ty: TyId, at: Span) -> TyId {
        // The type itself, not its end: who reports a mismatch later follows its variables (`param_in`).
        let end = self.shallow(ty);
        let Ty::Fallible(value, set) = self.types.get(end) else {
            return ty;
        };
        let names = self.types.names(set).join(" | ");
        b.diags.push(error(
            "unhandled-fallible",
            format!(
                "this can fail ({names}) and the failure must be handled: `?` to pass it on, `else` for a fallback, or `match`"
            ),
            b.file,
            at,
        ));
        value
    }

    // Patterns and the end of a body.

    /// Notes the error names a pattern matches (for `error-typo`).
    pub(super) fn matched_names(&self, b: &mut Body, p: &Pattern) {
        let mut stack = vec![p];
        while let Some(p) = stack.pop() {
            match &p.kind {
                PatternKind::Ctor(c, fields) => {
                    if let Ctor::Error(name) | Ctor::ErrorOrUnknown(name, _) = c {
                        b.matched.push(name.clone());
                    }
                    stack.extend(fields.iter().map(|f| &f.pattern));
                }
                PatternKind::Or(alts) => stack.extend(alts.iter()),
                PatternKind::Error
                | PatternKind::Wild
                | PatternKind::Bind(_)
                | PatternKind::Lit(_) => {}
            }
        }
    }

    /// Keeps what a finished body learned about errors: its shapes, raise sites and matched names.
    pub(super) fn commit_errors(&mut self, b: &mut Body) {
        for (name, mut shape) in std::mem::take(&mut b.shapes) {
            for f in &mut shape.fields {
                f.ty = f.ty.map(|t| self.zonk(t));
            }
            self.errors.insert(name, shape);
        }
        for (name, at) in std::mem::take(&mut b.raised) {
            self.raised.push((name, b.file.to_string(), at));
        }
        self.named.extend(std::mem::take(&mut b.matched));
    }

    /// A function's error set from what its body raised (`raised`: each name's first site), checked against
    /// its pinned set.
    pub(super) fn fn_raises(
        &mut self,
        b: &mut Body,
        pinned: Option<&Raises>,
        raised: BTreeMap<String, Span>,
    ) -> Vec<String> {
        let Some(pinned) = pinned else {
            return raised.into_keys().collect();
        };
        let names = self.pinned_names(pinned);
        for (name, at) in raised {
            if names.binary_search(&name).is_err() {
                b.diags.push(error(
                    "raises-outside-pinned",
                    format!(
                        "`{name}` is raised here, but this function is pinned to `raises {}`",
                        names.join(" | ")
                    ),
                    b.file,
                    at,
                ));
            }
        }
        names
    }

    /// The warnings `error-typo`, once every body is typed.
    pub(super) fn typos(&mut self) {
        let mut sites = std::mem::take(&mut self.raised);
        sites.sort_by(|a, b| (&a.1, a.2.line, a.2.col).cmp(&(&b.1, b.2.line, b.2.col)));
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (name, file, at) in sites {
            if self.named.contains(&name) || !seen.insert(name.clone()) {
                continue;
            }
            let Some(near) = crate::resolve::suggest(&name, self.named.iter().map(String::as_str))
            else {
                continue;
            };
            let mut d = Diagnostic {
                severity: Severity::Warning,
                ..error(
                    "error-typo",
                    format!(
                        "the error `{name}` is raised but never matched or declared; did you mean `{near}`?"
                    ),
                    &file,
                    at,
                )
            };
            d.notes
                .push(format!("`{near}` is matched or declared elsewhere"));
            self.diags.push(d);
        }
    }

    /// Every error name of the program and its data (what `Typed::errors` holds): the shapes, and the names
    /// of every set and declaration without one (data-less). A generic field never used has the error type.
    pub(super) fn error_table(
        &mut self,
        sets: &[Vec<String>],
    ) -> BTreeMap<String, Vec<(String, TyId)>> {
        let mut out: BTreeMap<String, Vec<(String, TyId)>> = self
            .errors
            .iter()
            .map(|(name, shape)| {
                let fields = shape
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.unwrap_or(TyId::ERROR)))
                    .collect();
                (name.clone(), fields)
            })
            .collect();
        let declared: Vec<String> = self.declared_set_names();
        for name in sets.iter().flatten().chain(&declared) {
            out.entry(name.clone()).or_default();
        }
        out
    }

    /// The names the `error` items and pinned names declare.
    fn declared_set_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        for def in &self.krate.defs {
            match &def.kind {
                DefKind::ErrorSet(members) => {
                    for m in members {
                        if let ErrorMember::Error(v) = m {
                            out.push(v.name.clone());
                        }
                    }
                }
                DefKind::Fn(f) => {
                    if let Some(Raises::Error(name)) = &f.raises {
                        out.push(name.clone());
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Adds `name` to the body's set, keeping its first site.
fn add_raise(b: &mut Body, name: String, at: Span) {
    b.raises.entry(name).or_insert(at);
}
