//! Quests (spec §5.12): the generated quest state, the generated fns, the stage code in handlers, the
//! objectives' commands and reads, the compiler's builtins (`join`, `after`, `play`, …) and `QuestInfo`.
//!
//! - **State** is ordinary globals with the names resolution gave them (migration keys): `Q::stage` (the
//!   `Stage` enum: a field-less variant is the immediate `(index << 1) | 1`), `Q::participants`,
//!   `Q::<Stage>.<obj>` (the built-in `Objective { state, count }`: state 0 open, 1 done, 2 failed),
//!   `Q::role.<name>` (installed by the VM; its initialiser is an empty fn that never runs), quest `let`s as
//!   `Q::<name>`, part `let`s as `Q::<part>::<name>`.
//! - **Generated fns** (`layout.rs` names them): `$enter::Q::<Stage>` sets the stage, re-opens that
//!   stage's objectives, cancels the stage-scoped timers and enqueues `Entered { stage }`; `$start::Q` enters
//!   the first stage (the VM runs it after the initialisers, before `main`, in the home only);
//!   `$transitions::Q` evaluates the current stage's completion transitions in source order and enters the
//!   first that holds; `$role_lost::Q` is the default `RoleLost` handler (`fail()` for a required role).
//! - **Handlers.** A stage's handler starts with `if stage != S: return` and its row carries the stage; a
//!   part's row carries `owner = 1 + part`. A home handler (owner 0, in a quest program) calls
//!   `$transitions::Q` at every segment end of its own body: before each `await` and at every return that
//!   is not an error's (the stage prologue's excepted: the handler did nothing). `-> Stage` calls `$enter`,
//!   then returns.
//! - **Objectives.** A command builds a new `Objective` object and moves it into the global (so a copy
//!   elsewhere is never changed): `finish` / `fail` set an open one done / failed, `progress(n)` adds `n` to
//!   an open one's count and makes it done at its `count`, `reset` re-opens it at count 0.
//! - **Builtins** are host calls in the namespace `quest` (`quest.joined`, `quest.join_refused`,
//!   `quest.left`, `quest.complete`, `quest.fail`, `quest.decline`, `quest.play`, the query
//!   `quest.completed_by`), `After` / `CancelTimer` for timers (a `Timer` is the host struct `Timer { id }`).
//! - **`Dialogue` and `Exit` values** are strings on the wire: the dialogue's qualified name, and
//!   `<dialogue>::<exit>`.

use super::expr::BArg;
use super::{Emit, Gen};
use crate::ast::{PartAnchor, Span};
use crate::diag::Diagnostic;
use crate::hir::{Arg, DefId, DefKind, DialogueKind, Expr, ExprKind, Quest, QuestFn, StageIdx};
use crate::infer::ObjectiveOp;
use crate::layout::FnKind;
use crate::ty::{Ty, TyId};
use quest_vm::asm::{FnRef, SlotRef};
use quest_vm::host::{HostFnKind, QUEST_NAMESPACE};
use quest_vm::program::{Anchor, PartInfo, QuestInfo, RepeatInfo, RoleInfo};
use quest_vm::types::{TypeDesc, TypeId};
use quest_vm::{Builtin, Op, ProgramBuilder};
use std::collections::BTreeMap;

/// An objective's states (0 is open).
const DONE: i64 = 1;
const FAILED: i64 = 2;
/// `NewList`'s flag: the elements are heap kinds.
const LIST_REFS: u16 = 1;

/// What code generation knows of the program's quest.
pub(super) struct QuestGen {
    /// Each handler's row: (owner, stage).
    places: BTreeMap<DefId, (u16, Option<u16>)>,
    /// Each objective's global, with its `count`.
    objectives: BTreeMap<DefId, Option<i64>>,
}

/// A field-less variant's immediate.
fn variant_imm(i: u32) -> i64 {
    (i64::from(i) << 1) | 1
}

fn idx16(i: usize) -> u16 {
    u16::try_from(i).unwrap_or(u16::MAX)
}

impl<'a> Gen<'a> {
    /// The program's quest and its item.
    pub(super) fn quest_hir(&self) -> Option<(DefId, &'a Quest)> {
        let krate: &'a crate::hir::Crate = self.krate;
        krate
            .defs
            .iter()
            .enumerate()
            .find_map(|(i, def)| match &def.kind {
                DefKind::Quest(q) => Some((DefId(u32::try_from(i).unwrap_or(u32::MAX)), q)),
                _ => None,
            })
    }

    /// Collects the quest's handler rows and objectives.
    pub(super) fn quest_plan(&mut self) {
        let Some((def, q)) = self.quest_hir() else {
            return;
        };
        if q.stages.is_empty() {
            // `Q::stage` would have no value to start at.
            let (file, span) = self.def_site(def);
            self.diags.push(Diagnostic::error(
                "no-stage",
                format!(
                    "quest `{}` has no `stage`: a quest starts in its first stage",
                    q.name
                ),
                &file,
                span.line,
                span.col,
                span.len,
            ));
        }
        let mut places = BTreeMap::new();
        for h in &q.handlers {
            places.insert(*h, (0, None));
        }
        for (s, st) in q.stages.iter().enumerate() {
            for h in &st.handlers {
                places.insert(*h, (0, Some(idx16(s))));
            }
        }
        for (p, part) in q.parts.iter().enumerate() {
            for h in &part.handlers {
                places.insert(*h, (idx16(p).saturating_add(1), None));
            }
        }
        let objectives = q
            .stages
            .iter()
            .flat_map(|s| &s.objectives)
            .map(|o| (o.global, o.count))
            .collect();
        self.quest = Some(QuestGen { places, objectives });
    }

    /// A handler's row: (owner, stage); (0, `None`) outside a quest.
    pub(super) fn handler_place(&self, d: DefId) -> (u16, Option<u16>) {
        self.quest
            .as_ref()
            .and_then(|q| q.places.get(&d).copied())
            .unwrap_or((0, None))
    }

    /// The default `RoleLost` handler's row, after every other.
    pub(super) fn role_lost_handler(&mut self, pb: &mut ProgramBuilder) {
        let Some((d, q)) = self.quest_hir() else {
            return;
        };
        let (Some(&f), Some(ty)) = (
            self.generated.get(&FnKind::RoleLost(d)),
            self.typed.types.lookup(Ty::Adt(q.generated.role_lost)),
        ) else {
            return;
        };
        let (file, span) = self.def_site(d);
        if let Some(tid) = self.type_id(pb, ty, &file, span) {
            let name = self.krate.qualified_name(q.generated.role_lost);
            pb.owned_handler(&name, tid, f, 0, None);
        }
    }

    /// A global's index in `Program::globals`.
    fn global_index(&self, d: DefId) -> Option<u16> {
        self.globals.get(&d).map(|g| g.0)
    }

    /// Global indices, strictly increasing.
    fn global_set<'s>(&self, set: impl IntoIterator<Item = &'s DefId>) -> Vec<u16> {
        let mut out: Vec<u16> = set
            .into_iter()
            .filter_map(|d| self.global_index(*d))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The program's `QuestInfo`.
    pub(super) fn quest_info(&mut self, pb: &mut ProgramBuilder) {
        let Some((d, q)) = self.quest_hir() else {
            return;
        };
        let (file, span) = self.def_site(d);
        let fn_id = |g: &Self, f: DefId| g.fns.get(&f).map_or(u16::MAX, |r| r.0);
        let mut roles = Vec::with_capacity(q.roles.len());
        for r in &q.roles {
            let gty = self
                .typed
                .globals
                .get(&r.global)
                .copied()
                .unwrap_or(TyId::ERROR);
            // The role's own type: the global's, or what its `Option` holds.
            let ty = match (r.optional, self.typed.types.get(gty)) {
                (true, Ty::Option(t)) => t,
                _ => gty,
            };
            let Some(ty) = self.type_id(pb, ty, &file, span) else {
                return;
            };
            roles.push(RoleInfo {
                name: r.name.clone(),
                global: self.global_index(r.global).unwrap_or(u16::MAX),
                ty,
                optional: r.optional,
                cond: r.cond.map(|c| fn_id(self, c)),
            });
        }
        let parts = q
            .parts
            .iter()
            .enumerate()
            .map(|(i, p)| PartInfo {
                name: p.name.clone(),
                anchor: match p.anchor {
                    PartAnchor::On => Anchor::On,
                    PartAnchor::At => Anchor::At,
                },
                role: idx16(p.role.0 as usize),
                read_set: self
                    .typed
                    .read_sets
                    .parts
                    .get(i)
                    .map(|s| self.global_set(s))
                    .unwrap_or_default(),
            })
            .collect();
        let info = QuestInfo {
            name: q.name.clone(),
            roles,
            stages: q.stages.iter().map(|s| s.name.clone()).collect(),
            stage_global: self.global_index(q.generated.stage).unwrap_or(u16::MAX),
            participants_global: self
                .global_index(q.generated.participants)
                .unwrap_or(u16::MAX),
            home_role: idx16(q.home.0 as usize),
            repeat: q.repeatable.map(|o| RepeatInfo {
                cooldown: u64::try_from(o.cooldown).unwrap_or(0),
                exclusive: o.exclusive,
                once_per_player: o.once_per_player,
            }),
            parts,
            requires: q.requires.iter().map(|f| fn_id(self, *f)).collect(),
            start_fn: self
                .generated
                .get(&FnKind::Start(d))
                .map_or(u16::MAX, |r| r.0),
            read_set: self.global_set(&self.typed.read_sets.dialogue),
        };
        pb.set_quest(info);
    }
}

impl<'a> Emit<'_, 'a, '_> {
    fn quest(&self) -> Option<(DefId, &'a Quest)> {
        self.g.quest_hir()
    }

    fn generated_fn(&self, k: FnKind) -> Option<FnRef> {
        self.g.generated.get(&k).copied()
    }

    /// A global's word.
    fn global_slot(&self, d: DefId) -> Option<SlotRef> {
        self.g.globals.get(&d).map(|g| SlotRef::Global(*g))
    }

    /// Calls a function of no parameters (its result, if any, stays in its frame).
    fn call0(&mut self, f: FnRef) {
        self.fb.call(f);
        self.at_line();
    }

    /// `dst = (stage == s)`.
    fn stage_is(&mut self, s: u32, dst: SlotRef) -> bool {
        let Some((_, q)) = self.quest() else {
            return false;
        };
        let Some(g) = self.global_slot(q.generated.stage) else {
            return false;
        };
        self.load_int(dst, variant_imm(s));
        self.fb.emit(Op::EqI, dst, g, dst);
        true
    }

    // Handlers.

    /// Whether this function is a home handler of the quest (it evaluates the transitions at its segment
    /// ends).
    pub(super) fn home_handler_of(&self, d: DefId) -> bool {
        self.g.quest.is_some()
            && self.g.typed.handlers.contains_key(&d)
            && self.g.handler_place(d).0 == 0
    }

    /// A stage handler's prologue: `if stage != s: return`.
    pub(super) fn stage_prologue(&mut self, s: u16) {
        let ok = self.fb.label();
        let t = self.temp(false);
        let w = self.word(t);
        if self.stage_is(u32::from(s), w) {
            self.fb.jump(Op::JmpIf, w, ok);
            self.ret_now();
        }
        self.free(t);
        self.fb.bind(ok);
    }

    /// The end of a segment of a home handler: `$transitions`.
    pub(super) fn segment_end(&mut self) {
        if !self.home_handler {
            return;
        }
        if let Some(f) = self
            .quest()
            .and_then(|(d, _)| self.generated_fn(FnKind::Transitions(d)))
        {
            self.call0(f);
        }
    }

    /// `-> Stage`: enters it, then returns.
    pub(super) fn stage_jump(&mut self, s: StageIdx, at: Span) {
        match self
            .quest()
            .and_then(|(d, _)| self.generated_fn(FnKind::Enter(d, s)))
        {
            Some(f) => self.call0(f),
            None => self.not_yet("this stage change", at),
        }
        self.ret(None);
    }

    // The generated fns.

    /// `$enter::Q::<s>`.
    pub(super) fn enter_body(&mut self, s: StageIdx) {
        let Some((d, q)) = self.quest() else {
            return self.ret_now();
        };
        let span = self.g.def_site(d).1;
        self.at(span);
        self.line = (span.line, span.col);
        let Some(stage) = q.stages.get(s.0 as usize) else {
            return self.ret_now();
        };
        if let Some(g) = self.global_slot(q.generated.stage) {
            self.immediate(g, true, variant_imm(s.0));
        }
        if !stage.objectives.is_empty() {
            let z = self.temp(false);
            let zw = self.word(z);
            self.fb.emit(Op::LoadSmall, zw, 0u16, 0u16);
            for o in &stage.objectives {
                if let Some(g) = self.global_slot(o.global) {
                    self.new_objective(g, zw, zw);
                }
            }
            self.free(z);
        }
        self.fb.emit(Op::CancelStageTimers, 0u16, 0u16, 0u16);
        // `Entered { stage: s }` into this instance's queue.
        let entered = q.generated.entered;
        let event = self.g.events.get(&entered).copied();
        let shape = self.g.shapes(entered).first();
        if let (Some(event), Some(shape)) = (event, shape) {
            let ref_bits = self.g.ref_bits(shape);
            let shape_id = self.fb.shape(1, ref_bits);
            let obj = self.temp(true);
            let ow = self.word(obj);
            self.fb.emit(Op::NewObj, ow, shape_id, 0u16);
            let v = self.temp(true);
            let vw = self.word(v);
            self.immediate(vw, true, variant_imm(s.0));
            self.fb.emit(Op::SetField, ow, 0u16, vw);
            self.free(v);
            self.fb.emit(Op::Enqueue, event, ow, 0u16);
            self.fb.emit(Op::Drop, ow, 0u16, 0u16);
            self.free(obj);
        }
        self.ret_now();
    }

    /// `$start::Q`: enters the first stage.
    pub(super) fn start_body(&mut self) {
        if let Some(f) = self
            .quest()
            .and_then(|(d, q)| (!q.stages.is_empty()).then_some(d))
            .and_then(|d| self.generated_fn(FnKind::Enter(d, StageIdx(0))))
        {
            self.call0(f);
        }
        self.ret_now();
    }

    /// `$transitions::Q`: the current stage's transitions in source order; the first that holds is
    /// entered.
    pub(super) fn transitions_body(&mut self) {
        let Some((d, q)) = self.quest() else {
            return self.ret_now();
        };
        let t = self.temp(false);
        let w = self.word(t);
        for (s, stage) in q.stages.iter().enumerate() {
            if stage.transitions.is_empty() {
                continue;
            }
            let next = self.fb.label();
            self.stage_is(u32::try_from(s).unwrap_or(u32::MAX), w);
            self.fb.jump(Op::JmpIfNot, w, next);
            for (to, cond) in &stage.transitions {
                let (Some(&c), Some(enter)) = (
                    self.g.fns.get(cond),
                    self.generated_fn(FnKind::Enter(d, *to)),
                ) else {
                    continue;
                };
                let skip = self.fb.label();
                let callee = self.fb.of(c);
                self.call0(c);
                self.fb.emit(Op::Copy, w, callee.ret(), 0u16);
                self.fb.jump(Op::JmpIfNot, w, skip);
                self.call0(enter);
                self.ret_now();
                self.fb.bind(skip);
            }
            self.ret_now();
            self.fb.bind(next);
        }
        self.free(t);
        self.ret_now();
    }

    /// `$role_lost::Q`: `fail()` when the lost role is a required one.
    pub(super) fn role_lost_body(&mut self) {
        let ev = self.fb.param(0);
        self.params = 1;
        self.heap_params.push(ev);
        self.home_handler = true;
        let Some((d, q)) = self.quest() else {
            return self.ret_now();
        };
        let span = self.g.def_site(d).1;
        self.at(span);
        self.line = (span.line, span.col);
        let (fail, end) = (self.fb.label(), self.fb.label());
        let role = self.temp(true);
        let rw = self.word(role);
        self.fb.emit(Op::GetField, rw, ev, 0u16);
        let t = self.temp(false);
        let tw = self.word(t);
        for (i, r) in q.roles.iter().enumerate() {
            if r.optional {
                continue;
            }
            self.load_int(tw, variant_imm(u32::try_from(i).unwrap_or(u32::MAX)));
            self.fb.emit(Op::EqI, tw, rw, tw);
            self.fb.jump(Op::JmpIf, tw, fail);
        }
        self.fb.jump(Op::Jmp, 0u16, end);
        self.fb.bind(fail);
        self.quest_host("fail", &[], None, span);
        self.fb.bind(end);
        self.free(t);
        self.fb.emit(Op::Drop, rw, 0u16, 0u16);
        self.free(role);
        self.segment_end();
        self.ret_now();
    }

    // Objectives.

    /// The `Objective` struct's field indices: (state, count).
    fn objective_fields(&self) -> (u16, u16) {
        let shape = self
            .g
            .krate
            .builtins
            .objective
            .and_then(|d| self.g.shapes(d).first());
        let at = |name: &str| {
            shape
                .and_then(|s| s.names.iter().position(|n| n == name))
                .map_or(u16::MAX, idx16)
        };
        (at("state"), at("count"))
    }

    /// `g = Objective(state, count)`: a new object moved into the global.
    fn new_objective(&mut self, g: SlotRef, state: SlotRef, count: SlotRef) {
        let (si, ci) = self.objective_fields();
        let shape = self.fb.shape(2, 0);
        let obj = self.temp(true);
        let ow = self.word(obj);
        self.fb.emit(Op::NewObj, ow, shape, 0u16);
        self.fb.emit(Op::SetField, ow, si, state);
        self.fb.emit(Op::SetField, ow, ci, count);
        self.fb.emit(Op::Move, g, ow, 0u16);
        self.free(obj);
    }

    /// `obj.done`, `obj.failed`, `obj.count` into `dst`.
    pub(super) fn objective_read(&mut self, x: &Expr, op: ObjectiveOp, dst: SlotRef) {
        let (si, ci) = self.objective_fields();
        let v = self.val_target(x);
        match op {
            ObjectiveOp::Count => {
                self.fb.emit(Op::GetField, dst, v.slot, ci);
            }
            _ => {
                let want = if op == ObjectiveOp::Done {
                    DONE
                } else {
                    FAILED
                };
                let t = self.temp(false);
                let tw = self.word(t);
                self.fb.emit(Op::GetField, dst, v.slot, si);
                self.load_int(tw, want);
                self.fb.emit(Op::EqI, dst, dst, tw);
                self.free(t);
            }
        }
        self.done(v);
    }

    /// `obj.finish()`, `fail()`, `progress(n)`, `reset()` on the objective named by `x`.
    pub(super) fn objective_command(
        &mut self,
        e: &Expr,
        x: &Expr,
        op: ObjectiveOp,
        args: &[Arg],
        dst: Option<SlotRef>,
    ) {
        let target = match &x.kind {
            ExprKind::Def(d) => self
                .g
                .quest
                .as_ref()
                .and_then(|q| q.objectives.get(d).copied())
                .zip(self.global_slot(*d)),
            _ => None,
        };
        let Some((count, g)) = target else {
            return self.not_yet("this objective command", e.span);
        };
        let (si, ci) = self.objective_fields();
        // The argument first: it may call.
        let n = match (op, args.first()) {
            (ObjectiveOp::Progress, Some(a)) => Some(self.val_copy(&a.value)),
            _ => None,
        };
        let (st, cnt) = (self.temp(false), self.temp(false));
        let (sw, cw) = (self.word(st), self.word(cnt));
        let end = self.fb.label();
        match op {
            ObjectiveOp::Reset => {
                self.fb.emit(Op::LoadSmall, sw, 0u16, 0u16);
                self.fb.emit(Op::LoadSmall, cw, 0u16, 0u16);
            }
            _ => {
                // Only an open objective changes.
                self.fb.emit(Op::GetField, sw, g, si);
                self.fb.jump(Op::JmpIf, sw, end);
                self.fb.emit(Op::GetField, cw, g, ci);
                match (op, &n) {
                    (ObjectiveOp::Progress, Some(n)) => {
                        self.fb.emit(Op::AddI, cw, cw, n.slot);
                        // Done once the count reaches the target.
                        self.load_int(sw, count.unwrap_or(i64::MAX));
                        self.fb.emit(Op::LeI, sw, sw, cw);
                    }
                    (ObjectiveOp::Fail, _) => self.load_int(sw, FAILED),
                    _ => self.load_int(sw, DONE),
                }
            }
        }
        self.new_objective(g, sw, cw);
        self.fb.bind(end);
        self.free(cnt);
        self.free(st);
        if let Some(n) = n {
            self.done(n);
        }
        if let Some(d) = dst {
            self.fb.emit(Op::LoadSmall, d, 0u16, 0u16);
        }
    }

    // The builtins.

    /// A call of the host fn `quest.<name>` with arguments already in words (borrowed: copied into the
    /// call's run): (word, heap kind, VM type). A query's answer goes to `result` (word, VM type).
    fn quest_host(
        &mut self,
        name: &str,
        args: &[(SlotRef, bool, TypeId)],
        result: Option<(SlotRef, TypeId)>,
        at: Span,
    ) {
        let host = self.g.host;
        let found = host
            .fns
            .iter()
            .position(|hf| hf.namespace.as_deref() == Some(QUEST_NAMESPACE) && hf.name == name);
        let Some((f, hf)) = found
            .and_then(|f| Some((u16::try_from(f).ok()?, host.fns.get(f)?)))
            .filter(|(_, hf)| hf.params.len() == args.len())
        else {
            return self.not_yet(
                &format!("`{name}` with this host (no `quest.{name}` of this shape)"),
                at,
            );
        };
        let ret = match (hf.kind, result) {
            (HostFnKind::Command, None) => self.type_id(TyId::UNIT, at),
            (HostFnKind::Query, Some((_, t))) => Some(t),
            _ => None,
        };
        let Some(ret) = ret else {
            return self.not_yet(&format!("`quest.{name}` of this kind"), at);
        };
        let params: Vec<TypeId> = args.iter().map(|(_, _, t)| *t).collect();
        let site = self
            .g
            .site(&mut self.fb, f, hf.kind, params, ret, Vec::new());
        let kinds: Vec<bool> = args.iter().map(|(_, h, _)| *h).collect();
        let run = self.slots.run(&kinds);
        for (k, (src, heap, _)) in args.iter().enumerate() {
            let w = self.word(run + k as u32);
            if *heap {
                self.live.push(run + k as u32);
                self.fb.emit(Op::Dup, w, *src, 0u16);
            } else {
                self.fb.emit(Op::Copy, w, *src, 0u16);
            }
        }
        let base = if args.is_empty() {
            self.fb.ret()
        } else {
            self.word(run)
        };
        let c = result.map_or(base, |(w, _)| w);
        self.fb.emit(Op::CallHost, site, base, c);
        for (k, heap) in kinds.iter().enumerate().rev() {
            let i = run + k as u32;
            if *heap {
                let w = self.word(i);
                self.fb.emit(Op::Drop, w, 0u16, 0u16);
            }
            self.free(i);
        }
    }

    /// The VM type of an expression's value (`None`: reported).
    fn tid_of(&mut self, x: &Expr) -> Option<TypeId> {
        let ty = self.target_ty(x);
        self.type_id(ty, x.span)
    }

    /// A call of a compiler builtin (`dialogue_depth` excepted); its value (`()` or a `Timer`) into `dst`.
    pub(super) fn quest_call(&mut self, e: &Expr, f: QuestFn, args: &[Arg], dst: Option<SlotRef>) {
        match f {
            QuestFn::Join => self.join(e, args),
            QuestFn::Leave => self.leave(e, args),
            QuestFn::Complete => self.complete(e, args),
            QuestFn::Fail => self.quest_host("fail", &[], None, e.span),
            QuestFn::Decline => self.quest_host("decline", &[], None, e.span),
            QuestFn::After => return self.after(e, args, dst),
            QuestFn::Cancel => self.cancel(e, args),
            QuestFn::Play => self.play(e, args),
            QuestFn::DialogueDepth => return self.dialogue_depth(e, args, dst),
        }
        if let Some(d) = dst {
            self.fb.emit(Op::LoadSmall, d, 0u16, 0u16);
        }
    }

    /// The participants' global, its VM type.
    fn participants(&mut self, at: Span) -> Option<(SlotRef, TypeId)> {
        let (_, q) = self.quest()?;
        let d = q.generated.participants;
        let g = self.global_slot(d)?;
        let ty = self.g.typed.globals.get(&d).copied()?;
        Some((g, self.type_id(ty, at)?))
    }

    /// `join(p)`: refused while an `exclusive` quest has others, or for a `once_per_player` player who
    /// completed it; else appended when absent (`quest.joined`).
    fn join(&mut self, e: &Expr, args: &[Arg]) {
        let (Some(a), Some((g, _)), Some((_, q))) =
            (args.first(), self.participants(e.span), self.quest())
        else {
            return self.not_yet("this `join`", e.span);
        };
        let Some(pt) = self.tid_of(&a.value) else {
            return;
        };
        let opts = q.repeatable;
        let p = self.val_copy(&a.value);
        let end = self.fb.label();
        let (c, t) = (self.temp(false), self.temp(false));
        let (cw, tw) = (self.word(c), self.word(t));
        self.builtin(
            Builtin::ListContains,
            &[BArg::Word(g, true), BArg::Word(p.slot, true)],
            TyId::BOOL,
            Some(cw),
        );
        let refuse = |s: &mut Self, ok| {
            s.quest_host("join_refused", &[(p.slot, true, pt)], None, e.span);
            s.fb.jump(Op::Jmp, 0u16, end);
            s.fb.bind(ok);
        };
        if opts.is_some_and(|o| o.exclusive) {
            let ok = self.fb.label();
            // Others in it, and not this player: refused.
            self.fb.emit(Op::ListLen, tw, g, 0u16);
            let z = self.temp(false);
            let zw = self.word(z);
            self.fb.emit(Op::LoadSmall, zw, 0u16, 0u16);
            self.fb.emit(Op::LtI, tw, zw, tw);
            self.free(z);
            self.fb.jump(Op::JmpIfNot, tw, ok);
            self.fb.jump(Op::JmpIf, cw, ok);
            refuse(self, ok);
        }
        if opts.is_some_and(|o| o.once_per_player)
            && let Some(bt) = self.type_id(TyId::BOOL, e.span)
        {
            let ok = self.fb.label();
            self.quest_host(
                "completed_by",
                &[(p.slot, true, pt)],
                Some((tw, bt)),
                e.span,
            );
            self.fb.jump(Op::JmpIfNot, tw, ok);
            refuse(self, ok);
        }
        self.fb.jump(Op::JmpIf, cw, end);
        // Appended: the list taken out of the global, pushed onto, put back.
        let run = self.slots.run(&[true, true]);
        self.live.extend([run, run + 1]);
        let (list, item) = (self.word(run), self.word(run + 1));
        self.fb.emit(Op::Move, list, g, 0u16);
        self.fb.emit(Op::Dup, item, p.slot, 0u16);
        self.fb
            .emit(Op::CallBuiltin, Builtin::ListPush as u16, list, list);
        self.fb.emit(Op::Move, g, list, 0u16);
        self.free(run + 1);
        self.free(run);
        self.quest_host("joined", &[(p.slot, true, pt)], None, e.span);
        self.fb.bind(end);
        self.free(t);
        self.free(c);
        self.done(p);
    }

    /// `leave(p)`: the participants without `p` (`quest.left` when it was one).
    fn leave(&mut self, e: &Expr, args: &[Arg]) {
        let (Some(a), Some((g, _))) = (args.first(), self.participants(e.span)) else {
            return self.not_yet("this `leave`", e.span);
        };
        let Some(pt) = self.tid_of(&a.value) else {
            return;
        };
        let p = self.val_copy(&a.value);
        let (i, n, t, found, one) = (
            self.temp(false),
            self.temp(false),
            self.temp(false),
            self.temp(false),
            self.temp(false),
        );
        let (iw, nw, tw, fw, onew) = (
            self.word(i),
            self.word(n),
            self.word(t),
            self.word(found),
            self.word(one),
        );
        // [the list kept, the element]
        let run = self.slots.run(&[true, true]);
        self.live.extend([run, run + 1]);
        let (list, item) = (self.word(run), self.word(run + 1));
        self.fb.emit(Op::NewList, list, LIST_REFS, 0u16);
        self.fb.emit(Op::LoadSmall, iw, 0u16, 0u16);
        self.fb.emit(Op::LoadSmall, fw, 0u16, 0u16);
        self.fb.emit(Op::LoadSmall, onew, 1u16, 0u16);
        self.fb.emit(Op::ListLen, nw, g, 0u16);
        let (top, skip, exit) = (self.fb.label(), self.fb.label(), self.fb.label());
        self.fb.bind(top);
        self.fb.emit(Op::LtI, tw, iw, nw);
        self.fb.jump(Op::JmpIfNot, tw, exit);
        self.fb.emit(Op::GetIndex, item, g, iw);
        self.fb.emit(Op::Eq, tw, item, p.slot);
        self.fb.jump(Op::JmpIf, tw, skip);
        self.fb
            .emit(Op::CallBuiltin, Builtin::ListPush as u16, list, list);
        let next = self.fb.label();
        self.fb.jump(Op::Jmp, 0u16, next);
        self.fb.bind(skip);
        self.fb.emit(Op::Drop, item, 0u16, 0u16);
        self.fb.emit(Op::LoadSmall, fw, 1u16, 0u16);
        self.fb.bind(next);
        self.fb.emit(Op::AddI, iw, iw, onew);
        self.fb.jump(Op::Jmp, 0u16, top);
        self.fb.bind(exit);
        self.fb.emit(Op::Move, g, list, 0u16);
        self.free(run + 1);
        self.free(run);
        let end = self.fb.label();
        self.fb.jump(Op::JmpIfNot, fw, end);
        self.quest_host("left", &[(p.slot, true, pt)], None, e.span);
        self.fb.bind(end);
        for x in [one, found, t, n, i] {
            self.free(x);
        }
        self.done(p);
    }

    /// `complete()` (every participant) / `complete(players)`: `quest.complete`.
    fn complete(&mut self, e: &Expr, args: &[Arg]) {
        match args.first() {
            Some(a) => {
                let Some(t) = self.tid_of(&a.value) else {
                    return;
                };
                let v = self.val(&a.value);
                self.quest_host("complete", &[(v.slot, true, t)], None, e.span);
                self.done(v);
            }
            None => match self.participants(e.span) {
                Some((g, t)) => self.quest_host("complete", &[(g, true, t)], None, e.span),
                None => self.not_yet("this `complete`", e.span),
            },
        }
    }

    /// `after(ticks, Ev(…))`: `After` (a script event only); its `Timer { id }` into `dst`.
    fn after(&mut self, e: &Expr, args: &[Arg], dst: Option<SlotRef>) {
        let (Some(ticks), Some(ev)) = (args.first(), args.get(1)) else {
            return self.not_yet("this `after`", e.span);
        };
        let event = match self.ty_kind(&ev.value) {
            Ty::Adt(d) => self.g.events.get(&d).copied(),
            _ => None,
        };
        let Some(event) = event else {
            return self.not_yet("`after` of a host event", ev.value.span);
        };
        let id_field = self
            .g
            .struct_shape(self.ty_kind(e))
            .and_then(|s| s.names.iter().position(|n| n == "id"))
            .map(idx16);
        // [ticks, the event]
        let run = self.slots.run(&[false, true]);
        self.live.push(run + 1);
        let (tw, vw) = (self.word(run), self.word(run + 1));
        self.eval_into(&ticks.value, tw);
        self.eval_into(&ev.value, vw);
        let id = self.temp(false);
        let iw = self.word(id);
        self.fb.emit(Op::After, iw, tw, event);
        self.fb.emit(Op::Drop, vw, 0u16, 0u16);
        self.free(run + 1);
        self.free(run);
        if let Some(d) = dst {
            match id_field {
                Some(f) => {
                    let shape = self.fb.shape(1, 0);
                    let obj = self.temp(true);
                    let ow = self.word(obj);
                    self.fb.emit(Op::NewObj, ow, shape, 0u16);
                    self.fb.emit(Op::SetField, ow, f, iw);
                    self.fb.emit(Op::Move, d, ow, 0u16);
                    self.free(obj);
                }
                None => self.not_yet("`after` with this host (no `Timer { id }`)", e.span),
            }
        }
        self.free(id);
    }

    /// `cancel(t)`: `CancelTimer` of the timer's id.
    fn cancel(&mut self, e: &Expr, args: &[Arg]) {
        let Some(a) = args.first() else {
            return self.not_yet("this `cancel`", e.span);
        };
        let Some(f) = self.field_index(self.target_kind(&a.value), "id") else {
            return self.not_yet("`cancel` with this host (no `Timer { id }`)", e.span);
        };
        let v = self.val_target(&a.value);
        let id = self.temp(false);
        let iw = self.word(id);
        self.fb.emit(Op::GetField, iw, v.slot, f);
        self.fb.emit(Op::CancelTimer, iw, 0u16, 0u16);
        self.free(id);
        self.done(v);
    }

    /// `play(scene, a=…, …)`: `quest.play(scene, [the cast in the scene's parameter order])` (in the
    /// order given when the scene is not named).
    fn play(&mut self, e: &Expr, args: &[Arg]) {
        let Some((scene, cast)) = args.split_first() else {
            return self.not_yet("this `play`", e.span);
        };
        let order: Vec<&Arg> = match &scene.value.kind {
            ExprKind::Def(d) => match self.def_kind(*d) {
                Some(DefKind::Dialogue(dl)) if dl.kind != DialogueKind::Conversation => dl
                    .cast
                    .iter()
                    .filter_map(|l| {
                        let name = &self.g.krate.local(*l)?.name;
                        cast.iter().find(|a| a.name.as_deref() == Some(name))
                    })
                    .collect(),
                _ => cast.iter().collect(),
            },
            _ => cast.iter().collect(),
        };
        let (Some(st), Some(first)) = (self.tid_of(&scene.value), order.first()) else {
            return self.not_yet("this `play`", e.span);
        };
        let Some(person) = self.tid_of(&first.value) else {
            return;
        };
        let lt = self.fb.ty(TypeDesc::List(person));
        let s = self.val_copy(&scene.value);
        let run = self.slots.run(&[true, true]);
        self.live.extend([run, run + 1]);
        let (list, item) = (self.word(run), self.word(run + 1));
        self.fb.emit(Op::NewList, list, LIST_REFS, 0u16);
        for a in order {
            self.eval_into(&a.value, item);
            self.fb
                .emit(Op::CallBuiltin, Builtin::ListPush as u16, list, list);
        }
        self.free(run + 1);
        self.quest_host(
            "play",
            &[(s.slot, true, st), (list, true, lt)],
            None,
            e.span,
        );
        self.fb.emit(Op::Drop, list, 0u16, 0u16);
        self.free(run);
        self.done(s);
    }

    /// A dialogue's name as a value: its qualified name.
    pub(super) fn dialogue_value(&mut self, d: DefId, dst: SlotRef) {
        let name = self.g.krate.qualified_name(d);
        let i = self.fb.string(&name);
        self.fb.emit_t(Op::LoadStr, dst, i);
    }

    /// A dialogue's exit `d::name` as a value: `<qualified name>::<exit>`.
    pub(super) fn exit_value(&mut self, d: DefId, exit: u32, dst: SlotRef) {
        let exit = match self.def_kind(d) {
            Some(DefKind::Dialogue(dl)) => dl.exits.get(exit as usize).cloned(),
            _ => None,
        }
        .unwrap_or_default();
        let name = format!("{}::{exit}", self.g.krate.qualified_name(d));
        let i = self.fb.string(&name);
        self.fb.emit_t(Op::LoadStr, dst, i);
    }
}
