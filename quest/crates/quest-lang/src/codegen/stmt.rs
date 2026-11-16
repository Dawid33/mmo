//! Statements, blocks, loops and function bodies.
//!
//! A block opens a scope; its heap-kind bindings are dropped where it ends. `break` and `continue` drop
//! the blocks inside the loop and the temporaries made since the loop began; `return` (and the end of
//! the body) drops every block, the heap parameters and every live temporary, then `Ret`.

use super::{Emit, Loop};
use crate::hir::{DefKind, Expr, ExprKind, LocalId, Prelude, Stmt, StmtKind};
use crate::layout::FnKind;
use quest_vm::Op;
use quest_vm::asm::SlotRef;

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
    fn ret_now(&mut self) {
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

    fn stmt(&mut self, s: &Stmt) {
        self.line = (s.span.line, s.span.col);
        self.at(s.span);
        match &s.kind {
            StmtKind::Let(local, _, value) => {
                let heap = self.heap_ty(self.g.typed.local(*local));
                let i = self.slots.fresh(heap);
                let slot = self.word(i);
                self.eval_into(value, slot);
                self.locals.insert(*local, slot);
                if heap && let Some(scope) = self.scopes.last_mut() {
                    scope.push(slot);
                }
            }
            StmtKind::Assign(place, value) => match self.variable(place) {
                Some(slot) => self.eval_into(value, slot),
                None => self.not_yet(
                    "assignment to a field, an element or through a reference",
                    place.span,
                ),
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
            StmtKind::Match(_, _) => self.not_yet("`match`", s.span),
            StmtKind::Pass => {}
            StmtKind::Break => self.leave_loop(true),
            StmtKind::Continue => self.leave_loop(false),
            StmtKind::Return(value) => self.ret(value.as_ref()),
            StmtKind::Raise(_) => self.not_yet("`raise`", s.span),
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

    /// `for x in range(n)` / `range(a, b)`: a counter from `a` (0) while below `b`, copied into `x` at
    /// each step. Both bounds are evaluated once, before the loop.
    fn for_loop(&mut self, names: &[LocalId], iter: &Expr, body: &[Stmt]) {
        let range = match (&iter.kind, names) {
            (ExprKind::Call(callee, args), [name])
                if matches!(callee.kind, ExprKind::Prelude(Prelude::Range)) =>
            {
                Some((*name, args))
            }
            _ => None,
        };
        let Some((name, args)) = range else {
            self.not_yet("a `for` loop over a list or a dict", iter.span);
            return;
        };
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
        match args.as_slice() {
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
