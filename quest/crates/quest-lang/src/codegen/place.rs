//! Places: a variable (a local or a top-level `let`) and a path of fields, list indexes and dict keys
//! from it (`q.rewards[2].count`). Writing into a place (an assignment, a mutating method) is **take →
//! mutate → put back** (design D4): each container on the path is taken out of its parent (`TakeField`
//! / `TakeIndex` / `TakeKey`, which make the parent unique first), the last one is written, then each is
//! set back into its parent in reverse (`SetField` / `SetIndex` / `SetKey`). Every index and key, the
//! assigned value and the method's arguments are evaluated before the first take (codegen carry-over 4):
//! between a take and its put-back the place holds 0.
//!
//! A path through a reference is Task 28's (`not-yet`).

use super::{Emit, Val};
use crate::ast::UnOp;
use crate::hir::{Expr, ExprKind};
use crate::ty::Ty;
use quest_vm::asm::SlotRef;
use quest_vm::{Builtin, Op};

use super::expr::may_write;

/// One step from a container to an element.
#[derive(Debug, Clone, Copy)]
pub(super) enum Step {
    Field(u16),
    Index,
    Key,
}

/// A variable and the steps from it, root first; each index or key step with its expression.
pub(super) struct Path<'e> {
    pub(super) root: SlotRef,
    pub(super) steps: Vec<(Step, Option<&'e Expr>)>,
}

/// What an expression is as a place.
pub(super) enum PlaceOf<'e> {
    Path(Path<'e>),
    /// Not a place: a value computed by the expression.
    Value,
    /// A place reached through a reference (Task 28).
    ThroughRef,
}

impl Emit<'_, '_, '_> {
    /// `e` as a place.
    pub(super) fn place_of<'e>(&self, e: &'e Expr) -> PlaceOf<'e> {
        let mut steps = Vec::new();
        let mut cur = e;
        loop {
            if matches!(self.ty_kind(cur), Ty::Ref(_)) {
                return PlaceOf::ThroughRef;
            }
            match &cur.kind {
                ExprKind::Local(_) | ExprKind::Def(_) => {
                    let Some(root) = self.variable(cur) else {
                        return PlaceOf::Value;
                    };
                    steps.reverse();
                    return PlaceOf::Path(Path { root, steps });
                }
                ExprKind::Field(x, name) => {
                    match self.field_index(self.ty_kind(x), name) {
                        Some(i) => steps.push((Step::Field(i), None)),
                        None if matches!(self.ty_kind(x), Ty::Ref(_)) => {
                            return PlaceOf::ThroughRef;
                        }
                        None => return PlaceOf::Value,
                    }
                    cur = x;
                }
                ExprKind::Index(x, i) => {
                    match self.ty_kind(x) {
                        Ty::List(_) => steps.push((Step::Index, Some(i))),
                        Ty::Dict(_, _) => steps.push((Step::Key, Some(i))),
                        Ty::Ref(_) => return PlaceOf::ThroughRef,
                        _ => return PlaceOf::Value,
                    }
                    cur = x;
                }
                ExprKind::Unary(UnOp::Deref, _) => return PlaceOf::ThroughRef,
                _ => return PlaceOf::Value,
            }
        }
    }

    /// The indexes and keys of the path, left to right, then nothing: `rest` is what is evaluated after
    /// them (a variable used as a key is copied when something after it may write it).
    fn operands(&mut self, path: &Path<'_>, rest: &[&Expr]) -> Vec<Option<Val>> {
        let mut all: Vec<&Expr> = path.steps.iter().filter_map(|(_, x)| *x).collect();
        all.extend_from_slice(rest);
        let mut out = Vec::with_capacity(path.steps.len());
        let mut j = 0usize;
        for (_, x) in &path.steps {
            let Some(x) = x else {
                out.push(None);
                continue;
            };
            j += 1;
            let later_writes = all
                .get(j..)
                .unwrap_or_default()
                .iter()
                .any(|y| may_write(y));
            out.push(Some(if later_writes {
                self.val_copy(x)
            } else {
                self.val(x)
            }));
        }
        out
    }

    /// The word of an index or key step's operand (a field step has none: never emitted).
    fn operand(ops: &[Option<Val>], k: usize) -> SlotRef {
        ops.get(k)
            .and_then(|v| v.as_ref())
            .map_or(SlotRef::Invalid, |v| v.slot)
    }

    fn emit_take(&mut self, step: Step, dst: SlotRef, container: SlotRef, key: SlotRef) {
        match step {
            Step::Field(i) => self.fb.emit(Op::TakeField, dst, container, i),
            Step::Index => self.fb.emit(Op::TakeIndex, dst, container, key),
            Step::Key => self.fb.emit(Op::TakeKey, dst, container, key),
        };
    }

    fn emit_set(&mut self, step: Step, container: SlotRef, key: SlotRef, src: SlotRef) {
        match step {
            Step::Field(i) => self.fb.emit(Op::SetField, container, i, src),
            Step::Index => self.fb.emit(Op::SetIndex, container, key, src),
            Step::Key => self.fb.emit(Op::SetKey, container, key, src),
        };
    }

    /// Takes the containers of the first `upto` steps out of their parents: the word of the last one
    /// taken (the root when none), and the temporaries holding them, in order.
    fn take_to(
        &mut self,
        path: &Path<'_>,
        ops: &[Option<Val>],
        upto: usize,
    ) -> (SlotRef, Vec<u32>) {
        let mut cur = path.root;
        let mut temps = Vec::with_capacity(upto);
        for (k, (step, _)) in path.steps.iter().take(upto).enumerate() {
            let t = self.temp(true);
            let w = self.word(t);
            self.emit_take(*step, w, cur, Self::operand(ops, k));
            temps.push(t);
            cur = w;
        }
        (cur, temps)
    }

    /// Sets the containers [`Self::take_to`] took back into their parents, innermost first.
    fn put_back(&mut self, path: &Path<'_>, ops: &[Option<Val>], temps: Vec<u32>) {
        for k in (0..temps.len()).rev() {
            let parent = match k.checked_sub(1).and_then(|p| temps.get(p)) {
                Some(&p) => self.word(p),
                None => path.root,
            };
            let (Some(&t), Some((step, _))) = (temps.get(k), path.steps.get(k)) else {
                continue;
            };
            let w = self.word(t);
            self.emit_set(*step, parent, Self::operand(ops, k), w);
            self.free(t);
        }
    }

    /// `place = value` for a place with at least one step.
    pub(super) fn assign_place(&mut self, place: &Expr, value: &Expr) {
        let path = match self.place_of(place) {
            PlaceOf::Path(p) => p,
            PlaceOf::ThroughRef => {
                return self.not_yet("assignment through a reference", place.span);
            }
            PlaceOf::Value => return self.not_yet("assignment to this expression", place.span),
        };
        let Some(&(last, _)) = path.steps.last() else {
            return self.eval_into(value, path.root);
        };
        let n = path.steps.len();
        let ops = self.operands(&path, &[value]);
        let v = self.val_copy(value);
        let (cur, temps) = self.take_to(&path, &ops, n - 1);
        // Moves the value in: its temporary holds nothing afterwards.
        self.emit_set(last, cur, Self::operand(&ops, n - 1), v.slot);
        if let Some(t) = v.temp {
            self.free(t);
        }
        self.put_back(&path, &ops, temps);
        for v in ops.into_iter().rev().flatten() {
            self.done(v);
        }
    }

    /// A mutating builtin (`ListPush`, `ListPop`, `ListInsert`, `ListRemove`, `DictRemove`) on the
    /// container `recv` with `args`: the arguments are evaluated, the container is taken out of its place
    /// into the builtin's first argument word, mutated there and put back. A receiver that is not a place
    /// is a temporary, mutated and dropped. The result into `dst` (`None`: dropped).
    pub(super) fn mutate(
        &mut self,
        b: Builtin,
        recv: &Expr,
        args: &[&Expr],
        result: crate::ty::TyId,
        dst: Option<SlotRef>,
    ) {
        use super::expr::BArg;
        let path = match self.place_of(recv) {
            PlaceOf::Path(p) => p,
            PlaceOf::ThroughRef => {
                return self.not_yet("a method call through a reference", recv.span);
            }
            PlaceOf::Value => {
                let mut bargs = vec![BArg::Expr(recv)];
                bargs.extend(args.iter().map(|x| BArg::Expr(x)));
                return self.builtin(b, &bargs, result, dst);
            }
        };
        let ops = self.operands(&path, args);
        let mut kinds = vec![true];
        kinds.extend(args.iter().map(|x| self.heap_expr(x)));
        let run = self.slots.run(&kinds);
        for (k, heap) in kinds.iter().enumerate() {
            if *heap {
                self.live.push(run + k as u32);
            }
        }
        for (k, x) in args.iter().enumerate() {
            let w = self.word(run + 1 + k as u32);
            self.eval_into(x, w);
        }
        let base = self.word(run);
        let n = path.steps.len();
        let last = path.steps.last().map(|(s, _)| *s);
        let (cur, temps) = match last {
            None => {
                self.fb.emit(Op::Move, base, path.root, 0u16);
                (path.root, Vec::new())
            }
            Some(step) => {
                let (cur, temps) = self.take_to(&path, &ops, n - 1);
                self.emit_take(step, base, cur, Self::operand(&ops, n - 1));
                (cur, temps)
            }
        };
        // The result goes to a word of its own: `dst` may be the place's variable.
        let unit = matches!(self.g.typed.types.get(result), Ty::Unit | Ty::Never);
        let heap = self.heap_ty(result);
        let scratch = (!unit).then(|| self.temp(heap));
        let c = scratch.map_or(base, |t| self.word(t));
        self.fb.emit(Op::CallBuiltin, b as u16, base, c);
        match last {
            None => {
                self.fb.emit(Op::Move, path.root, base, 0u16);
            }
            Some(step) => self.emit_set(step, cur, Self::operand(&ops, n - 1), base),
        }
        self.put_back(&path, &ops, temps);
        for (k, heap) in kinds.iter().enumerate().rev() {
            let i = run + k as u32;
            if *heap {
                let w = self.word(i);
                self.fb.emit(Op::Drop, w, 0u16, 0u16);
            }
            self.free(i);
        }
        match (scratch, dst) {
            (Some(t), Some(d)) => {
                let w = self.word(t);
                self.fb
                    .emit(if heap { Op::Move } else { Op::Copy }, d, w, 0u16);
                self.free(t);
            }
            (Some(t), None) => {
                let w = self.word(t);
                if heap {
                    self.fb.emit(Op::Drop, w, 0u16, 0u16);
                }
                self.free(t);
            }
            (None, Some(d)) => {
                // A unit builtin writes nothing: its value is `()`.
                self.fb.emit(Op::LoadSmall, d, 0u16, 0u16);
            }
            (None, None) => {}
        }
        for v in ops.into_iter().rev().flatten() {
            self.done(v);
        }
    }
}
