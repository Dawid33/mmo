//! References (spec §5.3): `&place`, and where a reference may go.
//!
//! **Places.** `&x` takes a place — a binding, a top-level `let`, a field or element of a place, or `*r` —
//! else `ref-not-place` (`&f()`, `&(a + b)`). A reference is a path, re-resolved on each use; the path of
//! each `&` (and of the receiver of each `&self` method call whose receiver is not already a reference) is
//! kept for code generation ([`Place`], `Typed::refs`). `&r` of a reference `r` is that same reference.
//!
//! **Second class.** A reference may be held in a local or passed as an argument, nothing else: putting one
//! in a list, a dict, an `Option`, a struct field, an error's data or a top-level variable, or returning it,
//! is `ref-escapes`. Values are checked where they go in (a list or dict literal's entries, `Some(…)`,
//! `append` / `insert`, an assignment to a field, element or top-level variable, a `raise`'s data, a
//! `return`, a top-level initialiser) once their type is known; annotations where they are written (a
//! reference inside another type, `-> &T`, a field `&T`, `let g: &T`).

use super::{Body, Check, Class, Infer, error};
use crate::ast::{Span, UnOp};
use crate::diag::Diagnostic;
use crate::hir::{self, DefId, DefKind, Expr, ExprId, ExprKind, LocalId};
use crate::ty::{Ty, TyId};

/// A place a reference points at: a root and the steps from it. A root or step whose value is itself a
/// reference is followed (the place is inside what it refers to), as field access auto-dereferences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub root: PlaceRoot,
    /// From the root outwards.
    pub steps: Vec<PlaceStep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceRoot {
    Local(LocalId),
    /// A top-level `let`.
    Global(DefId),
    /// `*r`: what the reference expression `r` refers to.
    Deref(ExprId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceStep {
    /// A field of a struct or event (script or host), by its index in declaration order.
    Field(u32),
    /// An element of a list or a value of a dict: the index (key) expression.
    Index(ExprId),
}

impl Infer<'_> {
    /// `&x`.
    pub(super) fn ref_op(&mut self, b: &mut Body, e: &Expr, x: &Expr) -> TyId {
        let ty = self.expr(b, x);
        if !self.is_place(x) {
            b.diags.push(error(
                "ref-not-place",
                "`&` takes a place (a variable, a field or an element of one, or `*r`), not a temporary value"
                    .to_string(),
                b.file,
                x.span,
            ));
            return TyId::ERROR;
        }
        self.keep_place(e.id, x);
        let end = self.shallow(ty);
        if matches!(self.types.get(end), Ty::Ref(_)) {
            return end;
        }
        self.types.intern(Ty::Ref(ty))
    }

    /// Keeps the path of the place `x` under `id` (nothing when part of it was reported wrong).
    pub(super) fn keep_place(&mut self, id: ExprId, x: &Expr) {
        let mut steps = Vec::new();
        let mut e = x;
        let root = loop {
            match &e.kind {
                ExprKind::Local(l) => break PlaceRoot::Local(*l),
                ExprKind::Def(d) => break PlaceRoot::Global(*d),
                ExprKind::Unary(UnOp::Deref, r) => break PlaceRoot::Deref(r.id),
                ExprKind::Field(inner, name) => {
                    let Some(i) = self.field_index(inner, name) else {
                        return;
                    };
                    steps.push(PlaceStep::Field(i));
                    e = inner;
                }
                ExprKind::Index(inner, i) => {
                    steps.push(PlaceStep::Index(i.id));
                    e = inner;
                }
                _ => return,
            }
        };
        steps.reverse();
        self.refs.insert(id, Place { root, steps });
    }

    /// The index of the field `name` of `x`'s type (seen through references), already typed.
    fn field_index(&mut self, x: &Expr, name: &str) -> Option<u32> {
        let ty = self.expr_ty(x.id);
        let (ty, _) = self.deref(ty);
        let shape = match self.types.get(ty) {
            Ty::Adt(d) => match self.krate.def(d).map(|def| &def.kind) {
                Some(DefKind::Struct(_) | DefKind::Event(_)) => self.adts.get(&d)?.first(),
                _ => None,
            },
            Ty::HostStruct(i) => self.host_structs.get(i as usize),
            Ty::HostEvent(i) => self.host_events.get(i as usize),
            _ => None,
        }?;
        let i = shape.names.iter().position(|n| n == name)?;
        u32::try_from(i).ok()
    }

    /// A value of type `ty` goes where a reference may not (`what`: "put in a list", …): checked once the
    /// type is known.
    pub(super) fn no_ref(&mut self, b: &mut Body, ty: TyId, at: Span, what: &'static str) {
        b.checks.push(Check {
            ty,
            class: Class::NoRef,
            op: what,
            at,
        });
    }

    /// The type of an annotation where a value is kept (a field, a result, a top-level variable): a
    /// reference there is `ref-escapes` (and the error type).
    pub(super) fn lower_kept(
        &mut self,
        t: &hir::Type,
        file: &str,
        out: &mut Vec<Diagnostic>,
        what: &'static str,
    ) -> TyId {
        if let hir::TypeKind::Ref(_) = t.kind {
            out.push(escapes(what, file, t.span));
            return TyId::ERROR;
        }
        self.lower(t, file, out)
    }
}

/// `ref-escapes`: a reference `what` ("put in a list", …).
pub(super) fn escapes(what: &str, file: &str, at: Span) -> Diagnostic {
    let mut d = error(
        "ref-escapes",
        format!("a reference cannot be {what}"),
        file,
        at,
    );
    d.notes
        .push("a reference may only be held in a local or passed as an argument".to_string());
    d
}
