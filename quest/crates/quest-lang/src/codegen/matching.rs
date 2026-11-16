//! `match`: each arm in order tests its pattern against the scrutinee, binding as it goes, then its guard;
//! the first arm that passes runs. Patterns are lowered from what inference recorded for each node
//! (`Typed::patterns`: the matched type, the constructor, the field each sub-pattern matches), never by
//! resolving names again.
//!
//! - The scrutinee is evaluated once, into a temporary.
//! - A constructor tests the tag (`GetTag`: an object's tag, or a field-less variant's immediate), then its
//!   fields: a binding is written straight from the field (`GetField`), any other sub-pattern tests the
//!   field in a temporary, dropped on both ways out.
//! - An arm's bindings live in a scope of their own around the arm's body. When the pattern or the guard
//!   fails, the bindings made so far are dropped (a binding's word holds 0 outside its arm).
//! - When no arm matches, the `match` faults (`AssertFailed`, codegen carry-over 3): the exhaustiveness
//!   check has a work cap past which it accepts a match without proving it. A `match` with an arm that
//!   always matches (a bare binding or `_`, no guard) has no such fault: it would be dead code.

use super::Emit;
use super::expr::{BArg, float_bits};
use crate::ast::{PatLit, Span};
use crate::hir::{Case, Expr, LocalId, Pattern, PatternKind};
use crate::infer::PatCtor;
use crate::ty::{Ty, TyId};
use quest_vm::asm::{Label, SlotRef};
use quest_vm::{Builtin, Op};
use std::collections::BTreeMap;

/// The fault message of a `match` no arm matched.
const NO_ARM_MESSAGE: &str = "no match arm matched";

/// The bindings of the arm being lowered: one word each (an `|` binds the same local in every
/// alternative).
type Bindings = BTreeMap<LocalId, SlotRef>;

impl Emit<'_, '_, '_> {
    /// `match scrutinee:` and its cases.
    pub(super) fn match_stmt(&mut self, at: Span, scrutinee: &Expr, cases: &[Case]) {
        if matches!(
            self.ty_kind(scrutinee),
            Ty::Ref(_) | Ty::Fallible(_, _) | Ty::Caught(_)
        ) {
            return self.not_yet("a `match` on a reference or an error", at);
        }
        let s = self.val_copy(scrutinee);
        let end = self.fb.label();
        for case in cases {
            self.line = (case.span.line, case.span.col);
            self.at(case.span);
            let fail = self.fb.label();
            let mut bound = Bindings::new();
            self.scopes.push(Vec::new());
            self.pattern(&case.pattern, s.slot, s.heap, fail, &mut bound);
            if let Some(g) = &case.guard {
                let v = self.val(g);
                self.fb.jump(Op::JmpIfNot, v.slot, fail);
                self.done(v);
            }
            self.block(&case.body);
            let scope = self.scopes.pop().unwrap_or_default();
            for w in scope.iter().rev() {
                self.fb.emit(Op::Drop, *w, 0u16, 0u16);
            }
            self.fb.jump(Op::Jmp, 0u16, end);
            self.fb.bind(fail);
            for w in scope.iter().rev() {
                self.fb.emit(Op::Drop, *w, 0u16, 0u16);
            }
        }
        // After an arm that always matches (a bare binding or `_`, no guard) the fault is dead code.
        let irrefutable = cases.iter().any(|c| {
            c.guard.is_none() && matches!(c.pattern.kind, PatternKind::Wild | PatternKind::Bind(_))
        });
        if !irrefutable {
            self.line = (at.line, at.col);
            self.at(at);
            self.builtin(
                Builtin::Assert,
                &[BArg::Imm(0), BArg::Str(NO_ARM_MESSAGE)],
                TyId::UNIT,
                None,
            );
        }
        self.fb.bind(end);
        self.done(s);
    }

    /// The word of a binding of the current arm (made on first sight, listed in the arm's scope).
    fn binding(&mut self, local: LocalId, bound: &mut Bindings) -> (SlotRef, bool) {
        let heap = self.heap_ty(self.g.typed.local(local));
        if let Some(&w) = bound.get(&local) {
            return (w, heap);
        }
        let i = self.slots.fresh(heap);
        let w = self.word(i);
        self.locals.insert(local, w);
        bound.insert(local, w);
        if heap && let Some(scope) = self.scopes.last_mut() {
            scope.push(w);
        }
        (w, heap)
    }

    /// Whether values matched by pattern node `p` are heap kinds.
    fn pat_heap(&self, p: &Pattern) -> bool {
        self.g
            .typed
            .patterns
            .get(&p.id)
            .is_some_and(|info| self.heap_ty(info.ty))
    }

    /// Tests `p` against the value in `word` (a heap kind: `heap`), binding as it goes; jumps to `fail`
    /// when it does not match.
    fn pattern(
        &mut self,
        p: &Pattern,
        word: SlotRef,
        heap: bool,
        fail: Label,
        bound: &mut Bindings,
    ) {
        match &p.kind {
            PatternKind::Error | PatternKind::Wild => {}
            PatternKind::Bind(local) => {
                let (w, _) = self.binding(*local, bound);
                let op = if heap { Op::Dup } else { Op::Copy };
                self.fb.emit(op, w, word, 0u16);
            }
            PatternKind::Lit(lit) => self.literal(lit, word, fail),
            PatternKind::Or(alts) => {
                let ok = self.fb.label();
                for (i, alt) in alts.iter().enumerate() {
                    let last = i + 1 == alts.len();
                    let next = if last { fail } else { self.fb.label() };
                    self.pattern(alt, word, heap, next, bound);
                    self.fb.jump(Op::Jmp, 0u16, ok);
                    if !last {
                        self.fb.bind(next);
                    }
                }
                self.fb.bind(ok);
            }
            PatternKind::Ctor(_, fields) => {
                let Some(info) = self.g.typed.patterns.get(&p.id).cloned() else {
                    return self.not_yet("this pattern", p.span);
                };
                let tag = match &info.ctor {
                    Some(PatCtor::None) => Some(0),
                    Some(PatCtor::Some) => Some(1),
                    Some(PatCtor::Variant(i)) => Some(*i),
                    Some(PatCtor::Struct) => None,
                    _ => return self.not_yet("matching an error", p.span),
                };
                if let Some(tag) = tag {
                    self.test_tag(word, tag, fail);
                }
                for (k, f) in fields.iter().enumerate() {
                    let Some(index) = info.fields.get(k).and_then(|&i| u16::try_from(i).ok())
                    else {
                        return self.not_yet("this pattern", p.span);
                    };
                    self.field_pattern(&f.pattern, word, index, fail, bound);
                }
            }
        }
    }

    /// Field `index` of the object in `word` against `sub`.
    fn field_pattern(
        &mut self,
        sub: &Pattern,
        word: SlotRef,
        index: u16,
        fail: Label,
        bound: &mut Bindings,
    ) {
        match &sub.kind {
            PatternKind::Error | PatternKind::Wild => {}
            PatternKind::Bind(local) => {
                let (w, _) = self.binding(*local, bound);
                self.fb.emit(Op::GetField, w, word, index);
            }
            _ => {
                let heap = self.pat_heap(sub);
                let t = self.temp(heap);
                let w = self.word(t);
                self.fb.emit(Op::GetField, w, word, index);
                if heap {
                    let (inner_fail, cont) = (self.fb.label(), self.fb.label());
                    self.pattern(sub, w, true, inner_fail, bound);
                    self.fb.emit(Op::Drop, w, 0u16, 0u16);
                    self.fb.jump(Op::Jmp, 0u16, cont);
                    self.fb.bind(inner_fail);
                    self.fb.emit(Op::Drop, w, 0u16, 0u16);
                    self.fb.jump(Op::Jmp, 0u16, fail);
                    self.fb.bind(cont);
                } else {
                    self.pattern(sub, w, false, fail, bound);
                }
                self.free(t);
            }
        }
    }

    /// Jumps to `fail` unless the variant in `word` has this tag.
    fn test_tag(&mut self, word: SlotRef, tag: u32, fail: Label) {
        let (t, k) = (self.temp(false), self.temp(false));
        let (ts, ks) = (self.word(t), self.word(k));
        self.fb.emit(Op::GetTag, ts, word, 0u16);
        self.load_int(ks, i64::from(tag));
        self.fb.emit(Op::EqI, ts, ts, ks);
        self.fb.jump(Op::JmpIfNot, ts, fail);
        self.free(k);
        self.free(t);
    }

    /// Jumps to `fail` unless the value in `word` equals the literal.
    fn literal(&mut self, lit: &PatLit, word: SlotRef, fail: Label) {
        let t = self.temp(false);
        let ts = self.word(t);
        match lit {
            PatLit::Str(s) => {
                let k = self.temp(true);
                let ks = self.word(k);
                let i = self.fb.string(s);
                self.fb.emit_t(Op::LoadStr, ks, i);
                self.fb.emit(Op::Eq, ts, word, ks);
                self.fb.emit(Op::Drop, ks, 0u16, 0u16);
                self.free(k);
            }
            PatLit::Float(f) => {
                let k = self.temp(false);
                let ks = self.word(k);
                let c = self.fb.const_u64(float_bits(*f));
                self.fb.emit_t(Op::LoadConst, ks, c);
                self.fb.emit(Op::EqF, ts, word, ks);
                self.free(k);
            }
            PatLit::Int(n) => {
                let k = self.temp(false);
                let ks = self.word(k);
                self.load_int(ks, *n);
                self.fb.emit(Op::EqI, ts, word, ks);
                self.free(k);
            }
            PatLit::Bool(b) => {
                let k = self.temp(false);
                let ks = self.word(k);
                self.load_int(ks, i64::from(*b));
                self.fb.emit(Op::EqI, ts, word, ks);
                self.free(k);
            }
        }
        self.fb.jump(Op::JmpIfNot, ts, fail);
        self.free(t);
    }
}
