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
//!   first that holds (`QuestInfo::transitions_fn`: the VM runs it after every committed segment of the
//!   home's handlers, `main` and the start); `$role_lost::Q` is the default `RoleLost` handler unless the
//!   quest body has one: it returns at once in a stage with a `RoleLost` handler of its own, else
//!   `fail()`s for a required role.
//! - **Handlers.** A stage's handler starts with `if stage != S: return` and its row carries the stage; a
//!   part's row carries `owner = 1 + part`. `-> Stage` calls `$enter`, then returns.
//! - **`QuestInfo`** also carries each stage's objectives (name, global, line id, hole fns, `at`, optional,
//!   count) for the journal; their texts join the lines table.
//! - **Objectives.** A command builds a new `Objective` object and moves it into the global (so a copy
//!   elsewhere is never changed): `finish` / `fail` set an open one done / failed, `progress(n)` adds `n` to
//!   an open one's count and makes it done at its `count`, `reset` re-opens it at count 0.
//! - **Builtins** and `Dialogue` / `Exit` values: `quest_builtins.rs`.

use super::dialogue::{Lines, stored_text};
use super::{Emit, Gen};
use crate::ast::{PartAnchor, Span};
use crate::diag::Diagnostic;
use crate::hir::{Arg, DefId, DefKind, Expr, ExprKind, Quest, StageIdx};
use crate::infer::ObjectiveOp;
use crate::layout::FnKind;
use crate::ty::{Ty, TyId};
use quest_vm::asm::{FnRef, SlotRef};
use quest_vm::program::{Anchor, ObjectiveInfo, PartInfo, QuestInfo, RepeatInfo, RoleInfo};
use quest_vm::{Op, ProgramBuilder};
use std::collections::BTreeMap;

/// An objective's states (0 is open).
const DONE: i64 = 1;
const FAILED: i64 = 2;

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

pub(super) fn idx16(i: usize) -> u16 {
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
    pub(super) fn quest_info(&mut self, pb: &mut ProgramBuilder, lines: &mut Lines) {
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
        // Each stage's objectives; their texts join the lines table.
        let mut objectives = Vec::with_capacity(q.stages.len());
        for st in &q.stages {
            let mut list = Vec::with_capacity(st.objectives.len());
            for o in &st.objectives {
                lines.insert(o.text.id.path.clone(), stored_text(&o.text).0);
                list.push(ObjectiveInfo {
                    name: o.name.clone(),
                    global: self.global_index(o.global).unwrap_or(u16::MAX),
                    line: o.text.id.path.clone(),
                    subst: o.text.subst().into_iter().map(|f| fn_id(self, f)).collect(),
                    at: o.at.map(|f| fn_id(self, f)),
                    optional: o.optional,
                    count: o.count,
                });
            }
            objectives.push(list);
        }
        if let Some(o) = q.repeatable
            && o.cooldown < 0
        {
            self.diags.push(Diagnostic::error(
                "bad-quest-option",
                "`cooldown` takes a number of ticks, 0 or more".to_string(),
                &file,
                span.line,
                span.col,
                span.len,
            ));
        }
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
            transitions_fn: self
                .generated
                .get(&FnKind::Transitions(d))
                .map_or(u16::MAX, |r| r.0),
            objectives,
        };
        pb.set_quest(info);
    }
}

impl<'a> Emit<'_, 'a, '_> {
    pub(super) fn quest(&self) -> Option<(DefId, &'a Quest)> {
        self.g.quest_hir()
    }

    fn generated_fn(&self, k: FnKind) -> Option<FnRef> {
        self.g.generated.get(&k).copied()
    }

    /// A global's word.
    pub(super) fn global_slot(&self, d: DefId) -> Option<SlotRef> {
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
        // A stage with a `RoleLost` handler of its own handles it there.
        let lost = crate::hir::EventRef::Script(q.generated.role_lost);
        for (s, stage) in q.stages.iter().enumerate() {
            let own = stage.handlers.iter().any(
                |h| matches!(self.def_kind(*h), Some(DefKind::Fn(f)) if f.handler == Some(lost)),
            );
            if own {
                self.stage_is(u32::try_from(s).unwrap_or(u32::MAX), tw);
                self.fb.jump(Op::JmpIf, tw, end);
            }
        }
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
                        // Done once the count reaches the target, else still open.
                        let open = self.fb.label();
                        self.load_int(sw, count.unwrap_or(i64::MAX));
                        self.fb.emit(Op::LeI, sw, sw, cw);
                        self.fb.jump(Op::JmpIfNot, sw, open);
                        self.load_int(sw, DONE);
                        self.fb.bind(open);
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
}
