//! Statements, blocks, loops and function bodies.
//!
//! A block opens a scope; its heap-kind bindings are dropped where it ends. `break` and `continue` drop
//! the blocks inside the loop and the temporaries made since the loop began; `return` (and the end of
//! the body) drops every block, the heap parameters and every live temporary, then `Ret`.

use super::place::{HeldPath, PlaceOf};
use super::{Emit, Loop};
use crate::hir::{Arg, DefKind, Expr, ExprKind, LocalId, Prelude, Stmt, StmtKind};
use crate::infer::{BuiltinMethod, Method};
use crate::layout::FnKind;
use crate::ty::Ty;
use quest_vm::asm::SlotRef;
use quest_vm::{Builtin, Op};

/// Where a `for` loop reads its list or dict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// A variable: its own word, read at each step.
    Variable(SlotRef),
    /// A path from a variable: read again at each step through its operands, evaluated once.
    Reread,
    /// Any other expression: evaluated once into a hidden copy.
    Copy,
}

impl Emit<'_, '_, '_> {
    /// Emits the body of `kind`; the frame's size in words.
    pub(super) fn function(mut self, kind: FnKind) -> u32 {
        match kind {
            FnKind::Def(d) => self.fn_body(d),
            FnKind::Init(d) => {
                if let Some(DefKind::Global(global)) = self.def_kind(d) {
                    let ty = self.ty_of(&global.value);
                    if self.heap_ty(ty) {
                        let ret = self.fb.ret();
                        self.fb.mark_ref(ret);
                    }
                    self.at(global.value.span);
                    self.line = (global.value.span.line, global.value.span.col);
                    self.scopes.push(Vec::new());
                    let ret = self.fb.ret();
                    self.eval_into(&global.value, ret);
                    self.scopes.pop();
                }
                self.fb.emit(Op::Ret, 0u16, 0u16, 0u16);
            }
        }
        self.finish()
    }

    fn fn_body(&mut self, d: crate::hir::DefId) {
        let Some(DefKind::Fn(f)) = self.def_kind(d) else {
            self.fb.emit(Op::Ret, 0u16, 0u16, 0u16);
            return;
        };
        let sig = self.g.typed.sigs.get(&d).cloned();
        let mut params: Vec<LocalId> = f.receiver.iter().map(|(_, l)| *l).collect();
        params.extend(f.params.iter().map(|p| p.local));
        self.params = params.len() as u32;
        for (i, local) in params.iter().enumerate() {
            let slot = self.fb.param(u16::try_from(i).unwrap_or(u16::MAX));
            self.locals.insert(*local, slot);
            if self.heap_ty(self.g.typed.local(*local)) {
                self.heap_params.push(slot);
            }
        }
        if sig.as_ref().is_some_and(|s| self.heap_ty(s.ret)) {
            let ret = self.fb.ret();
            self.fb.mark_ref(ret);
        }
        self.block(&f.body);
        self.ret_now();
    }

    /// Drops what the function holds and returns.
    pub(super) fn ret_now(&mut self) {
        let mut drops: Vec<SlotRef> = self.scopes.iter().flatten().rev().copied().collect();
        drops.extend(self.live.iter().rev().map(|&t| self.word(t)));
        drops.extend(self.heap_params.iter().copied());
        for s in drops {
            self.fb.emit(Op::Drop, s, 0u16, 0u16);
        }
        self.fb.emit(Op::Ret, 0u16, 0u16, 0u16);
    }

    pub(super) fn block(&mut self, body: &[Stmt]) {
        let line = self.line;
        self.scopes.push(Vec::new());
        for s in body {
            self.stmt(s);
        }
        self.line = line;
        let scope = self.scopes.pop().unwrap_or_default();
        for s in scope.into_iter().rev() {
            self.fb.emit(Op::Drop, s, 0u16, 0u16);
        }
    }

    pub(super) fn stmt(&mut self, s: &Stmt) {
        self.line = (s.span.line, s.span.col);
        self.at(s.span);
        match &s.kind {
            StmtKind::Let(local, _, value) => {
                let heap = self.heap_ty(self.g.typed.local(*local));
                let i = self.slots.fresh(heap);
                let slot = self.word(i);
                self.eval_into(value, slot);
                self.detach_discarded(*local, slot);
                self.locals.insert(*local, slot);
                if heap && let Some(scope) = self.scopes.last_mut() {
                    scope.push(slot);
                }
            }
            StmtKind::Assign(place, value) => match self.variable(place) {
                Some(slot) => self.eval_into(value, slot),
                None => self.assign_place(place, value),
            },
            StmtKind::Expr(e) => self.exec(e),
            StmtKind::If(branches, other) => {
                let end = self.fb.label();
                for (i, (cond, body)) in branches.iter().enumerate() {
                    if i > 0 {
                        // An `elif` is a line of its own.
                        self.line = (cond.span.line, cond.span.col);
                        self.at(cond.span);
                    }
                    let next = self.fb.label();
                    let v = self.val(cond);
                    self.fb.jump(Op::JmpIfNot, v.slot, next);
                    self.done(v);
                    self.block(body);
                    if other.is_some() || i + 1 < branches.len() {
                        self.fb.jump(Op::Jmp, 0u16, end);
                    }
                    self.fb.bind(next);
                }
                if let Some(body) = other {
                    self.block(body);
                }
                self.fb.bind(end);
            }
            StmtKind::While(cond, body) => {
                let (top, exit) = (self.fb.label(), self.fb.label());
                self.fb.bind(top);
                let v = self.val(cond);
                self.fb.jump(Op::JmpIfNot, v.slot, exit);
                self.done(v);
                self.loop_body(body, exit, top);
                self.fb.jump(Op::Jmp, 0u16, top);
                self.fb.bind(exit);
            }
            StmtKind::For(names, iter, body) => self.for_loop(names, iter, body),
            StmtKind::Match(scrutinee, cases) => self.match_stmt(s.span, scrutinee, cases),
            StmtKind::Pass => {}
            StmtKind::Break => self.leave_loop(true),
            StmtKind::Continue => self.leave_loop(false),
            StmtKind::Return(value) => self.ret(value.as_ref()),
            StmtKind::Raise(x) => self.raise(s.span, x),
        }
    }

    /// The word of a variable read or assigned whole: a local or a top-level `let`.
    pub(super) fn variable(&self, e: &Expr) -> Option<SlotRef> {
        match &e.kind {
            ExprKind::Local(id) => self.locals.get(id).copied(),
            ExprKind::Def(d) => self.g.globals.get(d).map(|g| SlotRef::Global(*g)),
            _ => None,
        }
    }

    pub(super) fn ret(&mut self, value: Option<&Expr>) {
        if let Some(v) = value {
            let ret = self.fb.ret();
            self.eval_into(v, ret);
        }
        self.ret_now();
    }

    /// `break` (`true`) or `continue`: drops what the jump leaves behind.
    pub(super) fn leave_loop(&mut self, brk: bool) {
        let Some(l) = self.loops.last() else {
            return;
        };
        let (target, scopes, live) = (if brk { l.brk } else { l.cont }, l.scopes, l.live);
        let mut drops: Vec<SlotRef> = self
            .scopes
            .get(scopes..)
            .unwrap_or_default()
            .iter()
            .flatten()
            .rev()
            .copied()
            .collect();
        let temps: Vec<u32> = self.live.get(live..).unwrap_or_default().to_vec();
        drops.extend(temps.iter().rev().map(|&t| self.word(t)));
        for s in drops {
            self.fb.emit(Op::Drop, s, 0u16, 0u16);
        }
        self.fb.jump(Op::Jmp, 0u16, target);
    }

    /// A loop's body, with `break` going to `brk` and `continue` to `cont`.
    fn loop_body(&mut self, body: &[Stmt], brk: quest_vm::asm::Label, cont: quest_vm::asm::Label) {
        self.loops.push(Loop {
            brk,
            cont,
            scopes: self.scopes.len(),
            live: self.live.len(),
        });
        self.block(body);
        self.loops.pop();
    }

    /// `for` over a `range`, a list, a dict's keys or a dict's `items()`.
    fn for_loop(&mut self, names: &[LocalId], iter: &Expr, body: &[Stmt]) {
        match (&iter.kind, names) {
            (ExprKind::Call(callee, args), [name])
                if matches!(callee.kind, ExprKind::Prelude(Prelude::Range)) =>
            {
                return self.for_range(*name, iter, args, body);
            }
            _ => {}
        }
        // `d.items()`: the dict.
        let items = match (&iter.kind, self.g.typed.methods.get(&iter.id)) {
            (ExprKind::Call(callee, _), Some(Method::Builtin(BuiltinMethod::Items))) => {
                match &callee.kind {
                    ExprKind::Field(d, _) => Some(&**d),
                    _ => None,
                }
            }
            _ => None,
        };
        let source = items.unwrap_or(iter);
        if !matches!(self.target_kind(source), Ty::List(_) | Ty::Dict(_, _)) {
            return self.not_yet("a `for` loop over this value", iter.span);
        }
        // A path's operands are evaluated once, before the loop (their calls run once).
        let (from, held) = match self.place_of(source) {
            PlaceOf::Path(p) if p.steps.is_empty() => (Source::Variable(p.root), None),
            PlaceOf::Path(p) => (Source::Reread, Some(self.hold_path(&p))),
            PlaceOf::Value => (Source::Copy, None),
            // Read again at each step through a path built once.
            PlaceOf::ThroughRef(rp) => (Source::Reread, Some(self.hold_ref(&rp))),
        };
        let held_ref = held.as_ref();
        match self.target_kind(source) {
            Ty::Dict(_, _) => self.for_dict(names, source, from, held_ref, items.is_some(), body),
            _ => self.for_list(names, source, from, held_ref, body),
        }
        if let Some(h) = held {
            self.release_held(h);
        }
    }

    /// Reads a loop's container into `dst`: from its held path (`Reread`), or by evaluating `source`.
    fn read_source(&mut self, source: &Expr, held: Option<&HeldPath>, dst: SlotRef) {
        match held {
            Some(h) => self.read_held(h, dst),
            None => self.eval_into(source, dst),
        }
    }

    /// The bindings of a loop (fresh words), in a scope of their own around the loop: dropped where it
    /// exits (by `break` too) or by `return`.
    fn loop_bindings(&mut self, names: &[LocalId]) -> Vec<SlotRef> {
        let mut words = Vec::with_capacity(names.len());
        let mut scope = Vec::new();
        for name in names {
            let heap = self.heap_ty(self.g.typed.local(*name));
            let i = self.slots.fresh(heap);
            let w = self.word(i);
            self.locals.insert(*name, w);
            if heap {
                scope.push(w);
            }
            words.push(w);
        }
        self.scopes.push(scope);
        words
    }

    /// Closes [`Self::loop_bindings`]' scope.
    fn end_loop_bindings(&mut self) {
        let scope = self.scopes.pop().unwrap_or_default();
        for w in scope.into_iter().rev() {
            self.fb.emit(Op::Drop, w, 0u16, 0u16);
        }
    }

    /// `for x in xs`: an index from 0 while below the length, the element copied into `x` at each step.
    /// The list is read again at each step from its place (so the body's changes count, carry-over 6; a
    /// path's indexes and keys were evaluated once, before the loop),
    /// or from a hidden copy of any other expression.
    fn for_list(
        &mut self,
        names: &[LocalId],
        source: &Expr,
        from: Source,
        path: Option<&HeldPath>,
        body: &[Stmt],
    ) {
        let (i, n, test, one) = (
            self.temp(false),
            self.temp(false),
            self.temp(false),
            self.temp(false),
        );
        let (i_s, n_s, test_s, one_s) =
            (self.word(i), self.word(n), self.word(test), self.word(one));
        self.fb.emit(Op::LoadSmall, i_s, 0u16, 0u16);
        // The list's word: the variable itself, or a temporary.
        let held = match from {
            Source::Variable(_) => None,
            Source::Reread | Source::Copy => Some(self.temp(true)),
        };
        let list = match (from, held) {
            (Source::Variable(w), _) => w,
            (_, Some(t)) => self.word(t),
            _ => SlotRef::Invalid,
        };
        if from == Source::Copy {
            self.eval_into(source, list);
        }
        let x = self
            .loop_bindings(names)
            .first()
            .copied()
            .unwrap_or(SlotRef::Invalid);
        let (top, step, exit) = (self.fb.label(), self.fb.label(), self.fb.label());
        self.fb.bind(top);
        if from == Source::Reread {
            self.read_source(source, path, list);
        }
        self.fb.emit(Op::ListLen, n_s, list, 0u16);
        self.fb.emit(Op::LtI, test_s, i_s, n_s);
        self.fb.jump(Op::JmpIfNot, test_s, exit);
        self.fb.emit(Op::GetIndex, x, list, i_s);
        if from == Source::Reread {
            self.fb.emit(Op::Drop, list, 0u16, 0u16);
        }
        self.loop_body(body, exit, step);
        self.fb.bind(step);
        self.at_stmt_line();
        self.fb.emit(Op::LoadSmall, one_s, 1u16, 0u16);
        self.fb.emit(Op::AddI, i_s, i_s, one_s);
        self.fb.jump(Op::Jmp, 0u16, top);
        self.fb.bind(exit);
        if let Some(t) = held {
            self.fb.emit(Op::Drop, list, 0u16, 0u16);
            self.free(t);
        }
        self.end_loop_bindings();
        for t in [one, test, n, i] {
            self.free(t);
        }
    }

    /// `for k in d` / `for k, v in d.items()`: by the entries' stable serials (design D4, carry-over 6),
    /// `pos = 0; loop { s = DictNext(d, pos); if s < 0 break; k = DictKeyAt(d, s); v = DictValAt(d, s);
    /// pos = s + 1; body }`. The dict is read again at each step from its place, or from a hidden copy of
    /// any other expression.
    fn for_dict(
        &mut self,
        names: &[LocalId],
        source: &Expr,
        from: Source,
        path: Option<&HeldPath>,
        items: bool,
        body: &[Stmt],
    ) {
        // [the dict, the position or serial]: the builtins' argument run.
        let run = self.slots.run(&[true, false]);
        self.live.push(run);
        let (d, arg) = (self.word(run), self.word(run + 1));
        let (pos, s, test, zero, one) = (
            self.temp(false),
            self.temp(false),
            self.temp(false),
            self.temp(false),
            self.temp(false),
        );
        let (pos_s, s_s, test_s, zero_s, one_s) = (
            self.word(pos),
            self.word(s),
            self.word(test),
            self.word(zero),
            self.word(one),
        );
        self.fb.emit(Op::LoadSmall, pos_s, 0u16, 0u16);
        self.fb.emit(Op::LoadSmall, zero_s, 0u16, 0u16);
        self.fb.emit(Op::LoadSmall, one_s, 1u16, 0u16);
        if from == Source::Copy {
            self.eval_into(source, d);
        }
        let bound = self.loop_bindings(names);
        let (top, exit) = (self.fb.label(), self.fb.label());
        self.fb.bind(top);
        match from {
            Source::Variable(w) => {
                self.fb.emit(Op::Dup, d, w, 0u16);
            }
            Source::Reread => self.read_source(source, path, d),
            Source::Copy => {}
        }
        self.fb.emit(Op::Copy, arg, pos_s, 0u16);
        self.fb
            .emit(Op::CallBuiltin, Builtin::DictNext as u16, d, s_s);
        self.fb.emit(Op::LtI, test_s, s_s, zero_s);
        self.fb.jump(Op::JmpIf, test_s, exit);
        self.fb.emit(Op::Copy, arg, s_s, 0u16);
        if let Some(&k) = bound.first() {
            self.fb
                .emit(Op::CallBuiltin, Builtin::DictKeyAt as u16, d, k);
        }
        if items && let Some(&v) = bound.get(1) {
            self.fb
                .emit(Op::CallBuiltin, Builtin::DictValAt as u16, d, v);
        }
        if from != Source::Copy {
            self.fb.emit(Op::Drop, d, 0u16, 0u16);
        }
        self.fb.emit(Op::AddI, pos_s, s_s, one_s);
        self.loop_body(body, exit, top);
        self.at_stmt_line();
        self.fb.jump(Op::Jmp, 0u16, top);
        self.fb.bind(exit);
        self.fb.emit(Op::Drop, d, 0u16, 0u16);
        self.end_loop_bindings();
        for t in [one, zero, test, s, pos] {
            self.free(t);
        }
        self.free(run + 1);
        self.free(run);
    }

    /// The statement's line again (after a nested statement's).
    fn at_stmt_line(&mut self) {
        let (line, col) = self.line;
        self.fb.line_col(line, col);
    }

    /// `for x in range(n)` / `range(a, b)`: a counter from `a` (0) while below `b`, copied into `x` at
    /// each step. Both bounds are evaluated once, before the loop.
    fn for_range(&mut self, name: LocalId, iter: &Expr, args: &[Arg], body: &[Stmt]) {
        let (cur, end, test, one) = (
            self.temp(false),
            self.temp(false),
            self.temp(false),
            self.temp(false),
        );
        let (cur_s, end_s, test_s, one_s) = (
            self.word(cur),
            self.word(end),
            self.word(test),
            self.word(one),
        );
        match args {
            [to] => {
                self.fb.emit(Op::LoadSmall, cur_s, 0u16, 0u16);
                self.eval_into(&to.value, end_s);
            }
            [from, to] => {
                self.eval_into(&from.value, cur_s);
                self.eval_into(&to.value, end_s);
            }
            _ => self.not_yet("this `range`", iter.span),
        }
        let x = self.slots.fresh(false);
        let x_s = self.word(x);
        self.locals.insert(name, x_s);
        let (top, step, exit) = (self.fb.label(), self.fb.label(), self.fb.label());
        self.fb.bind(top);
        self.fb.emit(Op::LtI, test_s, cur_s, end_s);
        self.fb.jump(Op::JmpIfNot, test_s, exit);
        self.fb.emit(Op::Copy, x_s, cur_s, 0u16);
        self.loop_body(body, exit, step);
        self.fb.bind(step);
        let (line, col) = self.line;
        self.fb.line_col(line, col);
        self.fb.emit(Op::LoadSmall, one_s, 1u16, 0u16);
        self.fb.emit(Op::AddI, cur_s, cur_s, one_s);
        self.fb.jump(Op::Jmp, 0u16, top);
        self.fb.bind(exit);
        for t in [cur, end, test, one] {
            self.free(t);
        }
    }
}
