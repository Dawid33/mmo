//! Event handlers, host calls, `await` and detached promises (spec §7, §8.2, §9; plan decisions 2, 3, 4,
//! 10, 21; design D5, D7, D9).
//!
//! **Handlers.** Every valid `@on(E)` function (`Typed::handlers`) is a row of the program's handler table:
//! the event's name and VM type, and the function, whose one parameter (frame word 3) the scheduler fills
//! with the event. Rows go by module path (the root first), then by declaration order in the module: the
//! order the handlers of one event run in. A handler is an ordinary function too: a script may call it.
//!
//! **Host calls** (`CallHost site, args, result`). The arguments are evaluated left to right into a run of
//! consecutive temporaries — a method's receiver first, as argument 0 (codegen carry-over 7), each read
//! through a reference — borrowed by the call and dropped after it. A host site names the host fn, its
//! kind, the VM type of each argument as this call passes it (a `Value` or `Event` parameter takes the
//! argument's own type) and the result's type (a query's answer, a promise's value; `()` for a command);
//! equal sites are shared. A query writes its answer at once, a command is buffered (its value is `()`), a
//! promise request is buffered and its id written to the result word.
//!
//! **`await`** (`Await a, b, c`; `AwaitAll` for `await all([…])`): the promise (or the list of promises)
//! is evaluated first, then the task parks there; on resume the value is in `a` (loaded against type `c`,
//! the awaited value's type: `list[T]` for `all`) and the error in `a + 1` (0: none). `a` and `a + 1` are
//! two adjacent temporaries of this frame — an [`Outcome`] consumed by `?`, `else` or `match` like a
//! fallible call's, then given back ([`Emit::end_outcome`]). The error can be any name of the awaited set
//! (`Typed::awaits`, decision 21): a host error, and every host error is a variant of the program's
//! `$Error` enum (inference declares them all), so the VM can load it. A list of promises is kept until the `AwaitAll` resumed (the VM reads it again then), then dropped.
//!
//! **Detached promises** (decision 3): `let _ = p` with a promise emits `PromiseDetach`: nobody awaits it,
//! and its record goes when it resolves.

use super::errors::Outcome;
use super::{Emit, Gen};
use crate::hir::{Arg, DefKind, EventRef, Expr, ExprKind, LocalId, Module, Prelude};
use crate::ty::{Ty, TyId};
use quest_vm::Op;
use quest_vm::ProgramBuilder;
use quest_vm::asm::{FnBuilder, SlotRef};
use quest_vm::host::HostFnKind;
use quest_vm::types::TypeId;

/// A host site's identity: host fn, kind (`HostFnKind` as a number), argument types, result type.
pub(super) type SiteKey = (u16, u8, Vec<TypeId>, TypeId);

/// A host call's operand: an argument, or a left-out parameter's default.
enum Operand<'x> {
    Expr(&'x Expr),
    Int(i64),
}

/// The name a discarding `let` binds.
const DISCARD: &str = "_";

fn kind_code(k: HostFnKind) -> u8 {
    match k {
        HostFnKind::Query => 0,
        HostFnKind::Command => 1,
        HostFnKind::Promise => 2,
    }
}

impl Gen<'_> {
    /// The handler table: modules by path, each module's handlers in declaration order.
    pub(super) fn handlers(&mut self, pb: &mut ProgramBuilder) {
        let krate = self.krate;
        let mut modules: Vec<&Module> = krate.modules.iter().collect();
        modules.sort_by(|a, b| a.path.cmp(&b.path));
        for m in modules {
            for &d in &m.defs {
                let Some(&event_ty) = self.typed.handlers.get(&d) else {
                    continue;
                };
                let event = match krate.def(d).map(|def| &def.kind) {
                    Some(DefKind::Fn(f)) => f.handler,
                    _ => None,
                };
                let name = match event {
                    Some(EventRef::Host(i)) => self.host.events.get(i as usize).map(|e| &e.name),
                    Some(EventRef::Script(e)) => krate.def(e).map(|def| &def.name),
                    None => None,
                };
                let (Some(name), Some(&f)) = (name, self.fns.get(&d)) else {
                    continue;
                };
                let (file, span) = self.def_site(d);
                if let Some(tid) = self.type_id(pb, event_ty, &file, span) {
                    pb.handler(name, tid, f);
                }
            }
        }
    }

    /// The host site for these types, made on first use.
    fn site(
        &mut self,
        fb: &mut FnBuilder<'_>,
        host_fn: u16,
        kind: HostFnKind,
        params: Vec<TypeId>,
        ret: TypeId,
        raises: Vec<String>,
    ) -> u16 {
        let key: SiteKey = (host_fn, kind_code(kind), params, ret);
        if let Some(&s) = self.sites.get(&key) {
            return s;
        }
        // One host fn declares one set, so the key need not hold it.
        let s = fb.fallible_host_site(host_fn, kind, key.2.clone(), ret, raises);
        self.sites.insert(key, s);
        s
    }
}

impl Emit<'_, '_, '_> {
    /// A call of host fn `f` (a method's receiver `recv` is argument 0); its result into `dst` (`None`:
    /// dropped).
    pub(super) fn host_call(
        &mut self,
        e: &Expr,
        f: u32,
        recv: Option<&Expr>,
        args: &[Arg],
        dst: Option<SlotRef>,
    ) {
        let host = self.g.host;
        let (Some(hf), Ok(host_fn)) = (host.fns.get(f as usize), u16::try_from(f)) else {
            return self.not_yet("this host call", e.span);
        };
        let mut operands: Vec<Operand<'_>> = recv.into_iter().map(Operand::Expr).collect();
        match self.g.typed.host_args.get(&e.id) {
            None => operands.extend(args.iter().map(|a| Operand::Expr(&a.value))),
            // Named arguments go to their parameters, a left-out one takes its default.
            Some(order) => {
                for ((name, _), a) in hf.params.iter().zip(order) {
                    operands.push(match a.and_then(|a| args.get(a as usize)) {
                        Some(a) => Operand::Expr(&a.value),
                        None => Operand::Int(
                            hf.defaults
                                .iter()
                                .find(|(d, _)| d == name)
                                .map_or(0, |(_, v)| *v),
                        ),
                    });
                }
            }
        }
        let mut params = Vec::with_capacity(operands.len());
        for x in &operands {
            let t = match x {
                Operand::Expr(x) => self.type_id(self.target_ty(x), x.span),
                Operand::Int(_) => self.type_id(TyId::INT, e.span),
            };
            let Some(t) = t else {
                return;
            };
            params.push(t);
        }
        let ty = self.ty_of(e);
        // A fallible call writes its error into the word after `dst` ([`Emit::host_outcome`]).
        let fallible = !hf.raises.is_empty() && hf.kind != HostFnKind::Promise;
        if fallible && dst.is_none() {
            return self.not_yet("this fallible host call", e.span);
        }
        let result = match (hf.kind, self.g.typed.types.get(ty)) {
            (HostFnKind::Promise, Ty::Promise(v)) => v,
            (HostFnKind::Command, _) => TyId::UNIT,
            (HostFnKind::Query, Ty::Fallible(v, _)) => v,
            (HostFnKind::Promise, _) => return self.not_yet("this host call", e.span),
            (HostFnKind::Query, _) => ty,
        };
        let Some(ret) = self.type_id(result, e.span) else {
            return;
        };
        let raises = if fallible {
            hf.raises.clone()
        } else {
            Vec::new()
        };
        let site = self
            .g
            .site(&mut self.fb, host_fn, hf.kind, params, ret, raises);

        let kinds: Vec<bool> = operands
            .iter()
            .map(|x| match x {
                Operand::Expr(x) => self.heap_ty(self.target_ty(x)),
                Operand::Int(_) => false,
            })
            .collect();
        let run = self.slots.run(&kinds);
        for (k, heap) in kinds.iter().enumerate() {
            if *heap {
                self.live.push(run + k as u32);
            }
        }
        for (k, x) in operands.iter().enumerate() {
            let w = self.word(run + k as u32);
            match x {
                Operand::Expr(x) => self.eval_target_into(x, w),
                Operand::Int(n) => self.load_int(w, *n),
            }
        }
        // Without arguments the run is empty: any word of the frame names it.
        let base = if operands.is_empty() {
            self.fb.ret()
        } else {
            self.word(run)
        };
        // The result word: `dst`, else a temporary (an infallible command writes none; a fallible one
        // writes the error word after `dst`).
        let (c, scratch) = match (hf.kind, dst) {
            (HostFnKind::Command, Some(d)) if fallible => (d, None),
            (HostFnKind::Command, _) => (base, None),
            (_, Some(d)) => (d, None),
            (HostFnKind::Query, None) => {
                let heap = self.heap_ty(result);
                let t = self.temp(heap);
                (self.word(t), Some((t, heap)))
            }
            (HostFnKind::Promise, None) => {
                let t = self.temp(false);
                (self.word(t), Some((t, false)))
            }
        };
        self.fb.emit(Op::CallHost, site, base, c);
        if let (HostFnKind::Command, Some(d)) = (hf.kind, dst) {
            // A command's value is `()`.
            self.fb.emit(Op::LoadSmall, d, 0u16, 0u16);
        }
        if let Some((t, heap)) = scratch {
            if heap {
                let w = self.word(t);
                self.fb.emit(Op::Drop, w, 0u16, 0u16);
            }
            self.free(t);
        }
        for (k, heap) in kinds.iter().enumerate().rev() {
            let i = run + k as u32;
            if *heap {
                let w = self.word(i);
                self.fb.emit(Op::Drop, w, 0u16, 0u16);
            }
            self.free(i);
        }
    }

    /// A call of the fallible host query or command `f` (`x`), its result left in two adjacent temporaries:
    /// the value (a command's is `()`) and the error the VM writes (0: none). A query's error is the host's
    /// answer; a command is checked by the host when issued and buffered only when the check passes.
    /// `None` when it was reported.
    pub(super) fn host_outcome(
        &mut self,
        x: &Expr,
        f: u32,
        recv: Option<&Expr>,
        args: &[Arg],
    ) -> Option<Outcome> {
        let value_ty = match (
            self.g.host.fns.get(f as usize).map(|hf| hf.kind),
            self.g.typed.types.get(self.ty_of(x)),
        ) {
            (Some(HostFnKind::Command), _) => TyId::UNIT,
            (Some(HostFnKind::Query), Ty::Fallible(v, _)) => v,
            _ => {
                self.not_yet("this fallible host call", x.span);
                return None;
            }
        };
        let heap = self.heap_ty(value_ty);
        let run = self.slots.run(&[heap, true]);
        let (value, err) = (self.word(run), self.word(run + 1));
        self.host_call(x, f, recv, args, Some(value));
        Some(Outcome {
            value,
            heap,
            err,
            temps: Some(run),
        })
    }

    /// `await x` (`e`): the task parks until the promise is resolved, or every promise of `all([…])`;
    /// the result is left in two adjacent temporaries. `None` when it was reported.
    pub(super) fn await_outcome(&mut self, e: &Expr, x: &Expr) -> Option<Outcome> {
        let Some(value) = self.g.typed.awaits.get(&e.id).map(|a| a.value) else {
            self.not_yet("this `await`", e.span);
            return None;
        };
        let tid = self.type_id(value, e.span)?;
        let heap = self.heap_ty(value);
        let list = match &x.kind {
            ExprKind::Call(callee, args)
                if matches!(callee.kind, ExprKind::Prelude(Prelude::All)) =>
            {
                match args.first() {
                    Some(a) => Some(&a.value),
                    None => {
                        self.not_yet("this `await`", e.span);
                        return None;
                    }
                }
            }
            _ => None,
        };
        // The promises first: evaluating them may call, and park, before this await's words are taken.
        let (op, src) = match list {
            Some(l) => (Op::AwaitAll, self.val(l)),
            None => (Op::Await, self.val(x)),
        };
        let run = self.slots.run(&[heap, true]);
        let (a, err) = (self.word(run), self.word(run + 1));
        self.at(e.span);
        self.fb.emit(op, a, src.slot, tid);
        self.at_line();
        self.done(src);
        Some(Outcome {
            value: a,
            heap,
            err,
            temps: Some(run),
        })
    }

    /// Gives back an outcome's temporaries (an `await`'s), once its value and error were moved out.
    pub(super) fn end_outcome(&mut self, o: Outcome) {
        if let Some(run) = o.temps {
            self.free(run);
            self.free(run + 1);
        }
    }

    /// After `let local = …` into `slot`: a promise bound to `_` is detached.
    pub(super) fn detach_discarded(&mut self, local: LocalId, slot: SlotRef) {
        let discarded = self
            .g
            .krate
            .locals
            .get(local.0 as usize)
            .is_some_and(|l| l.name == DISCARD);
        if discarded
            && matches!(
                self.g.typed.types.get(self.g.typed.local(local)),
                Ty::Promise(_)
            )
        {
            self.fb.emit(Op::PromiseDetach, slot, 0u16, 0u16);
        }
    }
}
