//! Calls: of a script function or method by name, and of the prelude's conversions, `print` and `range`.
//!
//! Arguments are checked against the callee's final signature; a call never changes the callee's types.
//! Arguments are positional.

use super::{Body, Infer};
use crate::hir::{Arg, DefKind, Expr, ExprKind, Prelude};
use crate::ty::{Ty, TyId};

impl Infer<'_> {
    pub(super) fn call(&mut self, b: &mut Body, e: &Expr, callee: &Expr, args: &[Arg]) -> TyId {
        match &callee.kind {
            ExprKind::Def(d)
                if matches!(
                    self.krate.def(*d).map(|def| &def.kind),
                    Some(DefKind::Fn(_))
                ) =>
            {
                // A function name has no value type.
                self.record(b, callee, TyId::ERROR);
                let name = self.krate.qualified_name(*d);
                match self.sigs.get(d).cloned() {
                    Some(sig) => {
                        let mut params: Vec<TyId> =
                            sig.receiver.map(|(_, t)| t).into_iter().collect();
                        params.extend(sig.params);
                        self.args(b, &name, &params, args, e, None);
                        sig.ret
                    }
                    // A member of a reported cycle.
                    None => {
                        self.sink_args(b, args);
                        TyId::ERROR
                    }
                }
            }
            ExprKind::Prelude(p) if prelude_typed(*p) => {
                self.record(b, callee, TyId::ERROR);
                self.prelude(b, e, *p, args)
            }
            _ => {
                // Constructions, methods, host calls and the rest of the prelude (Tasks 21, 22, 24) are typed
                // as values there: the error type here. Anything else is not callable.
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

    fn sink_args(&mut self, b: &mut Body, args: &[Arg]) {
        for a in args {
            self.sink(b, &a.value);
        }
    }

    fn prelude(&mut self, b: &mut Body, e: &Expr, p: Prelude, args: &[Arg]) -> TyId {
        let (name, params, ret, arity) = match p {
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
            // Not routed here (see `prelude_typed`).
            _ => {
                self.sink_args(b, args);
                return TyId::ERROR;
            }
        };
        self.args(b, name, &params, args, e, arity);
        ret
    }

    /// Checks a call's arguments against its parameters; `arity` words the count when it is not
    /// `params.len()`.
    fn args(
        &mut self,
        b: &mut Body,
        name: &str,
        params: &[TyId],
        args: &[Arg],
        call: &Expr,
        arity: Option<&str>,
    ) {
        if let Some(named) = args.iter().find_map(|a| a.name.as_deref()) {
            self.diags.push(super::error(
                "arg-count",
                format!("`{name}` takes its arguments by position, not by name (`{named}=`)"),
                b.file,
                call.span,
            ));
            self.sink_args(b, args);
            return;
        }
        if args.len() != params.len() {
            let takes = match (arity, params.len()) {
                (Some(text), _) => text.to_string(),
                (None, 1) => "1 argument".to_string(),
                (None, n) => format!("{n} arguments"),
            };
            self.diags.push(super::error(
                "arg-count",
                format!("`{name}` takes {takes}, {} given", args.len()),
                b.file,
                call.span,
            ));
            self.sink_args(b, args);
            return;
        }
        for (a, p) in args.iter().zip(params) {
            let found = self.expr(b, &a.value);
            self.expect(b, *p, found, a.value.span);
        }
    }
}

/// The prelude functions typed so far (the rest in Tasks 21, 22, 24).
fn prelude_typed(p: Prelude) -> bool {
    matches!(
        p,
        Prelude::Print | Prelude::Range | Prelude::Float | Prelude::Int | Prelude::Str
    )
}
