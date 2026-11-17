//! Calls: of a script function by name, of a method (`x.m(…)`: script, host or built-in, chosen by `x`'s
//! type), of a host function, of the prelude; and constructions (`Name(…)`, `E::V(…)`, `Some(x)`).
//!
//! Arguments are checked against the callee's final signature; a call never changes the callee's types.
//! Arguments are positional. A callee without a final signature makes the body wait for it (see the
//! module doc of `infer`).
//!
//! A method's receiver is passed as its first argument: a script method's `self` (or `&self`, for which the
//! receiver must be a place, or already a reference: `ref-not-place`), a host method's `args[0]` (plan D9).

use super::{Body, Check, Class, Infer};
use crate::ast::Receiver;
use crate::hir::{Arg, DefId, DefKind, Expr, ExprKind, HostItem, Prelude};
use crate::ty::{Ty, TyId};
use quest_vm::host::{HostFn, HostTy};

/// What a method call `x.m(…)` calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// A script method; its receiver is `x` (`&x` for a `&self` method whose `x` is not a reference).
    Script(DefId),
    /// A host method: an index into `HostApi::fns`; `x` is argument 0.
    Host(u32),
    /// A built-in method of `list`, `dict` or `str` (which one: the receiver's type).
    Builtin(BuiltinMethod),
    /// An objective's command (`find_dog.finish()`, by the call) or read (`find_dog.done`, by the field).
    Objective(super::ObjectiveOp),
}

/// The built-in methods (plan D11): `list`, `dict` and `str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinMethod {
    /// `list`, `dict`, `str`.
    Len,
    /// `list`.
    Append,
    Pop,
    Insert,
    /// `list` (by index, returns the element) and `dict` (by key, returns an `Option`).
    Remove,
    /// `dict`.
    Get,
    Keys,
    Values,
    /// `dict`, only as `for k, v in d.items()`.
    Items,
    /// `str`.
    Split,
    StartsWith,
    EndsWith,
    Contains,
    Trim,
    ToUpper,
    ToLower,
}

impl Infer<'_> {
    pub(super) fn call(&mut self, b: &mut Body, e: &Expr, callee: &Expr, args: &[Arg]) -> TyId {
        let kind = match &callee.kind {
            ExprKind::Def(d) => self.krate.def(*d).map(|def| &def.kind),
            _ => None,
        };
        match (&callee.kind, kind) {
            (ExprKind::Def(d), Some(DefKind::Fn(_))) => {
                // A function name has no value type.
                self.record(b, callee, TyId::ERROR);
                let name = self.krate.qualified_name(*d);
                match self.sig_of(b, *d, e.span) {
                    Some(sig) => {
                        self.called(b, *d, e.span);
                        let mut params: Vec<TyId> =
                            sig.receiver.map(|(_, t)| t).into_iter().collect();
                        params.extend(sig.params);
                        self.args(b, &name, &params, args, e, None);
                        self.fallible(sig.ret, &sig.raises)
                    }
                    // A member of a reported cycle, or one this body waits for.
                    None => {
                        self.sink_args(b, args);
                        TyId::ERROR
                    }
                }
            }
            (ExprKind::Def(d), Some(DefKind::Struct(_) | DefKind::Event(_))) => {
                self.record(b, callee, TyId::ERROR);
                let name = self.krate.qualified_name(*d);
                let shape = self
                    .adts
                    .get(d)
                    .and_then(|s| s.first())
                    .cloned()
                    .unwrap_or_default();
                let ty = self.types.intern(Ty::Adt(*d));
                self.construct(b, e, &name, &shape, args, ty)
            }
            (ExprKind::Variant(d, i), _) => {
                self.record(b, callee, TyId::ERROR);
                let name = self.variant_name(*d, *i);
                let shape = self
                    .adts
                    .get(d)
                    .and_then(|s| s.get(*i as usize))
                    .cloned()
                    .unwrap_or_default();
                let ty = self.types.intern(Ty::Adt(*d));
                self.construct(b, e, &name, &shape, args, ty)
            }
            (ExprKind::Host(HostItem::Struct(i)), _) => {
                self.record(b, callee, TyId::ERROR);
                let shape = self
                    .host_structs
                    .get(*i as usize)
                    .cloned()
                    .unwrap_or_default();
                let name = self
                    .host
                    .structs
                    .get(*i as usize)
                    .map_or(String::new(), |s| s.name.clone());
                let ty = self.types.intern(Ty::HostStruct(*i));
                self.construct(b, e, &name, &shape, args, ty)
            }
            (ExprKind::Host(HostItem::Event(i)), _) => {
                self.record(b, callee, TyId::ERROR);
                let shape = self
                    .host_events
                    .get(*i as usize)
                    .cloned()
                    .unwrap_or_default();
                let name = self
                    .host
                    .events
                    .get(*i as usize)
                    .map_or(String::new(), |s| s.name.clone());
                let ty = self.types.intern(Ty::HostEvent(*i));
                self.construct(b, e, &name, &shape, args, ty)
            }
            (ExprKind::Host(HostItem::Fn(i)), _) => {
                self.record(b, callee, TyId::ERROR);
                self.host_call(b, e, *i, args)
            }
            (ExprKind::Prelude(p), _) => {
                self.record(b, callee, TyId::ERROR);
                self.prelude(b, e, *p, args)
            }
            (ExprKind::Quest(f), _) => {
                self.record(b, callee, TyId::ERROR);
                self.quest_call(b, e, *f, args)
            }
            (ExprKind::Field(x, name), _) => {
                self.record(b, callee, TyId::ERROR);
                self.method(b, e, x, name, args)
            }
            _ => {
                // Error names are called only as a `raise`'s head (typed there). Anything else is not callable.
                let ty = self.expr(b, callee);
                let ty = self.shallow(ty);
                if !matches!(self.types.get(ty), Ty::Error | Ty::Never) {
                    let found = self.show(ty);
                    self.mismatch(b, callee.span, format!("this is `{found}`, not a function"));
                }
                self.sink_args(b, args);
                TyId::ERROR
            }
        }
    }

    /// `Stage::Accepted`.
    pub(super) fn variant_name(&self, d: DefId, i: u32) -> String {
        let variant = match self.krate.def(d).map(|def| &def.kind) {
            Some(DefKind::Enum(vs)) => vs.get(i as usize).map_or("", |v| v.name.as_str()),
            _ => "",
        };
        format!("{}::{variant}", self.krate.qualified_name(d))
    }

    pub(super) fn sink_args(&mut self, b: &mut Body, args: &[Arg]) {
        for a in args {
            self.sink(b, &a.value);
        }
    }

    // Methods.

    /// `x.name(args)`: by `x`'s type (seen through references).
    fn method(&mut self, b: &mut Body, e: &Expr, x: &Expr, name: &str, args: &[Arg]) -> TyId {
        let ty = self.expr(b, x);
        let (ty, through_ref) = self.deref(ty);
        match self.types.get(ty) {
            Ty::Error | Ty::Never => {
                self.sink_args(b, args);
                TyId::ERROR
            }
            Ty::Var(_) => {
                self.unknown_receiver(b, ty, x.span, &format!("the method `{name}`"));
                self.sink_args(b, args);
                TyId::ERROR
            }
            Ty::Adt(d) if Some(d) == self.krate.builtins.objective => {
                self.objective_method(b, e, x, name, args)
            }
            Ty::Adt(d) => {
                let Some(m) = self.krate.methods.get(&(d, name.to_string())).copied() else {
                    return self.no_method(b, e, ty, name, args);
                };
                let qualified = self.krate.qualified_name(m);
                let Some(sig) = self.sig_of(b, m, e.span) else {
                    self.sink_args(b, args);
                    return TyId::ERROR;
                };
                match sig.receiver {
                    None => {
                        b.diags.push(super::error(
                            "unknown-method",
                            format!("`{qualified}` takes no `self`: call it as `{qualified}(…)`"),
                            b.file,
                            e.span,
                        ));
                        self.sink_args(b, args);
                        return TyId::ERROR;
                    }
                    Some((Receiver::Ref, _)) if !through_ref && !self.is_place(x) => {
                        b.diags.push(super::error(
                            "ref-not-place",
                            format!(
                                "`{qualified}` takes `&self`, so its receiver must be a place (a variable, a field or an element), not a temporary value"
                            ),
                            b.file,
                            x.span,
                        ));
                    }
                    // Passed as `&x`.
                    Some((Receiver::Ref, _)) if !through_ref => self.keep_place(x.id, x),
                    Some(_) => {}
                }
                self.methods.insert(e.id, Method::Script(m));
                self.called(b, m, e.span);
                self.args(b, &qualified, &sig.params, args, e, None);
                self.fallible(sig.ret, &sig.raises)
            }
            Ty::HostStruct(i) => {
                let host = self.host;
                let owner = host.structs.get(i as usize).map(|s| s.name.as_str());
                let found = host
                    .fns
                    .iter()
                    .position(|f| f.receiver.as_deref() == owner && f.name == name);
                match found {
                    Some(f) if host.fns.get(f).is_some_and(|hf| hf.property) => {
                        let owner = owner.unwrap_or_default();
                        b.diags.push(super::error(
                            "not-callable",
                            format!(
                                "`{owner}.{name}` is a property, not a method: read it without parentheses (`.{name}`)"
                            ),
                            b.file,
                            e.span,
                        ));
                        self.sink_args(b, args);
                        TyId::ERROR
                    }
                    Some(f) => {
                        let f = u32::try_from(f).unwrap_or(u32::MAX);
                        self.methods.insert(e.id, Method::Host(f));
                        self.host_call(b, e, f, args)
                    }
                    None => self.no_method(b, e, ty, name, args),
                }
            }
            Ty::List(_) | Ty::Dict(_, _) | Ty::Str => self.builtin_method(b, e, ty, name, args),
            _ => self.no_method(b, e, ty, name, args),
        }
    }

    fn no_method(&mut self, b: &mut Body, e: &Expr, ty: TyId, name: &str, args: &[Arg]) -> TyId {
        let shown = self.show(ty);
        b.diags.push(super::error(
            "unknown-method",
            format!("`{shown}` has no method `{name}`"),
            b.file,
            e.span,
        ));
        self.sink_args(b, args);
        TyId::ERROR
    }

    /// A built-in method of a list, a dict or a string (`ty`, seen through references).
    fn builtin_method(
        &mut self,
        b: &mut Body,
        e: &Expr,
        ty: TyId,
        name: &str,
        args: &[Arg],
    ) -> TyId {
        use BuiltinMethod as M;
        let (m, params, ret) = match (self.types.get(ty), name) {
            (Ty::List(_) | Ty::Dict(_, _) | Ty::Str, "len") => (M::Len, vec![], TyId::INT),
            (Ty::List(t), "append") => (M::Append, vec![t], TyId::UNIT),
            (Ty::List(t), "pop") => (M::Pop, vec![], self.types.intern(Ty::Option(t))),
            (Ty::List(t), "insert") => (M::Insert, vec![TyId::INT, t], TyId::UNIT),
            (Ty::List(t), "remove") => (M::Remove, vec![TyId::INT], t),
            (Ty::Dict(k, v), "get") => (M::Get, vec![k], self.types.intern(Ty::Option(v))),
            (Ty::Dict(k, v), "remove") => (M::Remove, vec![k], self.types.intern(Ty::Option(v))),
            (Ty::Dict(k, _), "keys") => (M::Keys, vec![], self.types.intern(Ty::List(k))),
            (Ty::Dict(_, v), "values") => (M::Values, vec![], self.types.intern(Ty::List(v))),
            (Ty::Dict(_, _), "items") => {
                self.mismatch(
                    b,
                    e.span,
                    "`items()` has no value of its own: it is only for `for k, v in d.items():`"
                        .to_string(),
                );
                self.sink_args(b, args);
                return TyId::ERROR;
            }
            (Ty::Str, "split") => {
                let list = self.types.intern(Ty::List(TyId::STR));
                (M::Split, vec![TyId::STR], list)
            }
            (Ty::Str, "starts_with") => (M::StartsWith, vec![TyId::STR], TyId::BOOL),
            (Ty::Str, "ends_with") => (M::EndsWith, vec![TyId::STR], TyId::BOOL),
            (Ty::Str, "contains") => (M::Contains, vec![TyId::STR], TyId::BOOL),
            (Ty::Str, "trim") => (M::Trim, vec![], TyId::STR),
            (Ty::Str, "to_upper") => (M::ToUpper, vec![], TyId::STR),
            (Ty::Str, "to_lower") => (M::ToLower, vec![], TyId::STR),
            _ => return self.no_method(b, e, ty, name, args),
        };
        self.methods.insert(e.id, Method::Builtin(m));
        if self.args(b, name, &params, args, e, None) {
            // The value a list keeps.
            let kept = match m {
                BuiltinMethod::Append => args.first(),
                BuiltinMethod::Insert => args.get(1),
                _ => None,
            };
            // The list's element type: a reference only when it took one (else the mismatch is reported).
            if let (Some(a), Some(elem)) = (kept, params.last()) {
                self.no_ref(b, *elem, a.value.span, "put in a list");
            }
        }
        ret
    }

    // The host.

    /// A host function or method `f` (plan D9): a promise returns `Promise[T]`; `Value` takes any storable
    /// value, `Infer` is the receiving type, `Event` any event. Arguments go by position, then by name; a
    /// parameter with a default may be left out (`host_args`). A property read (`x.alive`) is a call of
    /// its host method without arguments.
    pub(super) fn host_call(&mut self, b: &mut Body, e: &Expr, f: u32, args: &[Arg]) -> TyId {
        let host = self.host;
        let Some(hf) = host.fns.get(f as usize) else {
            self.sink_args(b, args);
            return TyId::ERROR;
        };
        let name = match hf.namespace.as_ref().or(hf.receiver.as_ref()) {
            Some(owner) => format!("{owner}.{}", hf.name),
            None => hf.name.clone(),
        };
        let mut params = Vec::with_capacity(hf.params.len());
        let mut open: Vec<(usize, Class)> = Vec::new();
        for (i, (_, t)) in hf.params.iter().enumerate() {
            let ty = self.host_ty(t, &mut |inf, t| {
                match t {
                    HostTy::Value => open.push((i, Class::Storable)),
                    HostTy::Event => open.push((i, Class::Event)),
                    _ => {}
                }
                inf.fresh()
            });
            params.push(ty);
        }
        // A call with the wrong count was reported; its result (often a variable) is not asked about again.
        let short = args.len() < params.len() && !hf.defaults.is_empty();
        let order: Vec<Option<u32>> = if short || args.iter().any(|a| a.name.is_some()) {
            let Some(order) = self.host_order(b, &name, hf, args, e) else {
                return TyId::ERROR;
            };
            for (p, a) in params.iter().zip(&order) {
                if let Some(a) = a.and_then(|a| args.get(a as usize)) {
                    let found = self.expr(b, &a.value);
                    self.expect(b, *p, found, a.value.span);
                }
            }
            if order.iter().enumerate().any(|(i, a)| *a != Some(i as u32)) {
                self.host_args.insert(e.id, order.clone());
            }
            order
        } else {
            if !self.args(b, &name, &params, args, e, None) {
                return TyId::ERROR;
            }
            (0..params.len()).map(|i| Some(i as u32)).collect()
        };
        // The argument at each parameter.
        let at = |i: usize| {
            order
                .get(i)
                .copied()
                .flatten()
                .and_then(|a| args.get(a as usize))
        };
        // Checked once the arguments' types are decided.
        let op: &'static str = match (hf.namespace.as_deref(), hf.name.as_str()) {
            (Some("world"), "set") => "world.set",
            (Some("world"), "compare_and_set") => "world.compare_and_set",
            (None, "emit") => "emit",
            _ => "this host function",
        };
        // One key holds one type: `new` has the type of `expected`, so `Changed(current)` (typed by
        // `expected`, decision 11) can always carry what the key holds.
        if op == "world.compare_and_set"
            && let (Some(&expected), Some(&new), Some(at)) =
                (params.get(1), params.get(2), at(2).map(|a| a.value.span))
            && !self.unify(expected, new)
        {
            let (e, f) = (self.show(expected), self.show(new));
            let mut d = super::error(
                "type-mismatch",
                format!(
                    "`world.compare_and_set` takes `new` of the type of `expected`: expected `{e}`, found `{f}`"
                ),
                b.file,
                at,
            );
            d.notes.push(
                "one key holds one type, so `Changed(current)` has exactly that type".to_string(),
            );
            b.diags.push(d);
        }
        for (i, class) in open {
            if let (Some(ty), Some(a)) = (params.get(i), at(i)) {
                b.checks.push(Check {
                    ty: *ty,
                    class,
                    op,
                    at: a.value.span,
                });
            }
        }
        let ret = self.host_ty(&hf.ret, &mut |inf, _| inf.fresh());
        // A promise carries its (open) error set; `await` makes it the fallible result.
        self.host_fallible(b, e, f, &params, ret)
    }

    /// Which argument gives each parameter of host fn `hf` when `args` name some or leave some out:
    /// positional arguments first, then named ones; a left-out parameter must have a default. `None` when it
    /// was reported (`arg-count`; the arguments are sunk).
    fn host_order(
        &mut self,
        b: &mut Body,
        name: &str,
        hf: &HostFn,
        args: &[Arg],
        call: &Expr,
    ) -> Option<Vec<Option<u32>>> {
        let mut order: Vec<Option<u32>> = vec![None; hf.params.len()];
        let mut problem = None;
        let mut named = false;
        for (k, a) in args.iter().enumerate() {
            let k = k as u32;
            match &a.name {
                None if named => {
                    problem = Some("a positional argument after a named one".to_string());
                }
                None => match order.get_mut(k as usize) {
                    Some(slot) => *slot = Some(k),
                    None => {
                        problem = Some(format!(
                            "{} parameters, {} given",
                            hf.params.len(),
                            args.len()
                        ));
                    }
                },
                Some(n) => {
                    named = true;
                    match hf.params.iter().position(|(p, _)| p == n) {
                        Some(i) if order[i].is_some() => {
                            problem = Some(format!("`{n}` given twice"));
                        }
                        Some(i) => order[i] = Some(k),
                        None => problem = Some(format!("no parameter `{n}`")),
                    }
                }
            }
            if problem.is_some() {
                break;
            }
        }
        if problem.is_none()
            && let Some(((p, _), _)) = hf
                .params
                .iter()
                .zip(&order)
                .find(|((p, _), a)| a.is_none() && !hf.defaults.iter().any(|(d, _)| d == p))
        {
            problem = Some(format!("`{p}` is not given"));
        }
        match problem {
            None => Some(order),
            Some(why) => {
                let params: Vec<&str> = hf.params.iter().map(|(p, _)| p.as_str()).collect();
                b.diags.push(super::error(
                    "arg-count",
                    format!("`{name}({})`: {why}", params.join(", ")),
                    b.file,
                    call.span,
                ));
                self.sink_args(b, args);
                None
            }
        }
    }

    // The prelude.

    fn prelude(&mut self, b: &mut Body, e: &Expr, p: Prelude, args: &[Arg]) -> TyId {
        let numeric = |b: &mut Body, ty: TyId, op: &'static str| {
            b.checks.push(Check {
                ty,
                class: Class::Numeric,
                op,
                at: e.span,
            });
        };
        let (name, params, ret, arity): (&str, Vec<TyId>, TyId, Option<&str>) = match p {
            Prelude::Print => ("print", vec![TyId::STR], TyId::UNIT, None),
            Prelude::Float => ("float", vec![TyId::INT], TyId::FLOAT, None),
            Prelude::Int => ("int", vec![TyId::FLOAT], TyId::INT, None),
            // Any value.
            Prelude::Str => ("str", vec![self.fresh()], TyId::STR, None),
            Prelude::Range => {
                let list = self.types.intern(Ty::List(TyId::INT));
                let params = if args.len() == 2 {
                    vec![TyId::INT, TyId::INT]
                } else {
                    vec![TyId::INT]
                };
                ("range", params, list, Some("1 or 2 arguments"))
            }
            Prelude::ParseInt => {
                let opt = self.types.intern(Ty::Option(TyId::INT));
                ("parse_int", vec![TyId::STR], opt, None)
            }
            Prelude::Abs => {
                let t = self.fresh();
                numeric(b, t, "abs");
                ("abs", vec![t], t, None)
            }
            Prelude::Min | Prelude::Max => {
                let t = self.fresh();
                let name = if p == Prelude::Min { "min" } else { "max" };
                numeric(b, t, name);
                (name, vec![t, t], t, None)
            }
            Prelude::Sqrt => ("sqrt", vec![TyId::FLOAT], TyId::FLOAT, None),
            Prelude::Floor => ("floor", vec![TyId::FLOAT], TyId::FLOAT, None),
            Prelude::Ceil => ("ceil", vec![TyId::FLOAT], TyId::FLOAT, None),
            Prelude::Round => ("round", vec![TyId::FLOAT], TyId::FLOAT, None),
            Prelude::Sin => ("sin", vec![TyId::FLOAT], TyId::FLOAT, None),
            Prelude::Cos => ("cos", vec![TyId::FLOAT], TyId::FLOAT, None),
            Prelude::Pow => ("pow", vec![TyId::FLOAT, TyId::FLOAT], TyId::FLOAT, None),
            Prelude::Assert => ("assert", vec![TyId::BOOL], TyId::UNIT, None),
            Prelude::AssertEq => {
                let t = self.fresh();
                ("assert_eq", vec![t, t], TyId::UNIT, None)
            }
            Prelude::All => {
                // Awaited, it fails with any promise's error (decision 21).
                let t = self.fresh();
                let promise = self.types.intern(Ty::Promise(t));
                let list = self.types.intern(Ty::List(promise));
                let all = self.types.intern(Ty::List(t));
                let ret = self.types.intern(Ty::Promise(all));
                ("all", vec![list], ret, None)
            }
            Prelude::Some => {
                let t = self.fresh();
                let opt = self.types.intern(Ty::Option(t));
                ("Some", vec![t], opt, None)
            }
            Prelude::None => {
                let t = self.fresh();
                let opt = self.types.intern(Ty::Option(t));
                ("None", vec![], opt, Some("no arguments (write `None`)"))
            }
            Prelude::Option => {
                self.mismatch(
                    b,
                    e.span,
                    "`Option` is a type: make a value with `Some(x)` or `None`".to_string(),
                );
                self.sink_args(b, args);
                return TyId::ERROR;
            }
            Prelude::Ok | Prelude::Err => {
                self.sink_args(b, args);
                return self.no_result(b, e.span, p);
            }
        };
        // A call with the wrong count was reported; its result (often a variable) is not asked about again.
        if !self.args(b, name, &params, args, e, arity) {
            return TyId::ERROR;
        }
        if p == Prelude::Some
            && let Some(a) = args.first()
        {
            let ty = self.expr_ty(a.value.id);
            self.no_ref(b, ty, a.value.span, "put in an `Option`");
        }
        ret
    }

    /// Checks a call's arguments against its parameters; `arity` words the count when it is not
    /// `params.len()`. False when the count (or a name) was wrong: the arguments were not matched.
    pub(super) fn args(
        &mut self,
        b: &mut Body,
        name: &str,
        params: &[TyId],
        args: &[Arg],
        call: &Expr,
        arity: Option<&str>,
    ) -> bool {
        if let Some(named) = args.iter().find_map(|a| a.name.as_deref()) {
            b.diags.push(super::error(
                "arg-count",
                format!("`{name}` takes its arguments by position, not by name (`{named}=`)"),
                b.file,
                call.span,
            ));
            self.sink_args(b, args);
            return false;
        }
        if args.len() != params.len() {
            let takes = match (arity, params.len()) {
                (Some(text), _) => text.to_string(),
                (None, 1) => "1 argument".to_string(),
                (None, n) => format!("{n} arguments"),
            };
            b.diags.push(super::error(
                "arg-count",
                format!("`{name}` takes {takes}, {} given", args.len()),
                b.file,
                call.span,
            ));
            self.sink_args(b, args);
            return false;
        }
        for (a, p) in args.iter().zip(params) {
            let found = self.expr(b, &a.value);
            self.expect(b, *p, found, a.value.span);
        }
        true
    }
}
