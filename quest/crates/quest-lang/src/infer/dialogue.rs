//! Dialogue, scene and quest code (spec §5.11–5.12):
//! - the compiler's builtins: `join(p: Player)`, `leave(p: Player)`, `complete()` / `complete(players:
//!   list[Player])`, `fail()`, `decline()` (unit); `after(ticks: int, e: <event>) -> Timer`, `cancel(t: Timer)`,
//!   `play(scene: Dialogue, <cast by name>: Person, …)`, and the runner's `dialogue_depth() -> int`;
//! - an objective's commands `finish()`, `fail()`, `reset()`, `progress(n: int)` (`bad-objective-call` on
//!   an objective without `count`, or on anything but the objective by name) and its reads `done`, `failed`
//!   (`bool`), `count` (`int`), recorded as [`Method::Objective`];
//! - once every body is typed, what a dialogue's synthesised fns must give: an `emit` argument its field's
//!   type (every field exactly once), a `for` target and a line's speaker a `Person` or an `Option[Person]`,
//!   an objective's `at` a host handle (an entity or a place) or an `Option` of one.
//!
//! A dialogue's name is a `Dialogue` value and `d::exit` an `Exit` value (`expr.rs`); a condition is a
//! `bool` by its synthesised fn's return type (`ret_value` words that mismatch for a condition).

use super::{Body, Check, Class, Infer, Method};
use crate::ast::Span;
use crate::hir::{
    Arg, DefId, DefKind, Dialogue, DialogueKind, EmitArg, EventRef, Expr, ExprKind, QuestFn,
    Speaker, StepKind,
};
use crate::ty::{Ty, TyId};
use std::collections::BTreeMap;

/// An objective's commands and reads (spec §5.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ObjectiveOp {
    Finish,
    Fail,
    Reset,
    Progress,
    Done,
    Failed,
    Count,
}

impl ObjectiveOp {
    /// A command (handlers only) rather than a pure read.
    pub fn is_command(self) -> bool {
        matches!(
            self,
            ObjectiveOp::Finish | ObjectiveOp::Fail | ObjectiveOp::Reset | ObjectiveOp::Progress
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            ObjectiveOp::Finish => "finish",
            ObjectiveOp::Fail => "fail",
            ObjectiveOp::Reset => "reset",
            ObjectiveOp::Progress => "progress",
            ObjectiveOp::Done => "done",
            ObjectiveOp::Failed => "failed",
            ObjectiveOp::Count => "count",
        }
    }
}

const COMMANDS: [ObjectiveOp; 4] = [
    ObjectiveOp::Finish,
    ObjectiveOp::Fail,
    ObjectiveOp::Reset,
    ObjectiveOp::Progress,
];
const READS: [ObjectiveOp; 3] = [ObjectiveOp::Done, ObjectiveOp::Failed, ObjectiveOp::Count];

impl Infer<'_> {
    /// Every objective's global and its `count`, from the quests.
    pub(super) fn collect_objectives(&mut self) {
        for def in &self.krate.defs {
            if let DefKind::Quest(q) = &def.kind {
                for o in q.stages.iter().flat_map(|s| &s.objectives) {
                    self.objectives.insert(o.global, o.count);
                }
            }
        }
    }

    /// A host struct's type by name (`Player`, `Person`, `Timer`); the error type when the host has none.
    fn host_struct(&mut self, name: &str) -> TyId {
        match self.host.structs.iter().position(|s| s.name == name) {
            Some(i) => self
                .types
                .intern(Ty::HostStruct(u32::try_from(i).unwrap_or(u32::MAX))),
            None => TyId::ERROR,
        }
    }

    /// A call of a compiler builtin.
    pub(super) fn quest_call(&mut self, b: &mut Body, e: &Expr, f: QuestFn, args: &[Arg]) -> TyId {
        let name = f.name();
        let (params, ret, arity) = match f {
            QuestFn::Join | QuestFn::Leave => (vec![self.host_struct("Player")], TyId::UNIT, None),
            QuestFn::Complete if args.is_empty() => (Vec::new(), TyId::UNIT, None),
            QuestFn::Complete => {
                let player = self.host_struct("Player");
                let list = self.types.intern(Ty::List(player));
                (
                    vec![list],
                    TyId::UNIT,
                    Some("no arguments, or 1 (a `list[Player]`)"),
                )
            }
            QuestFn::Fail | QuestFn::Decline => (Vec::new(), TyId::UNIT, None),
            QuestFn::DialogueDepth => (Vec::new(), TyId::INT, None),
            QuestFn::After => {
                let event = self.fresh();
                let timer = self.host_struct("Timer");
                (vec![TyId::INT, event], timer, None)
            }
            QuestFn::Cancel => (vec![self.host_struct("Timer")], TyId::UNIT, None),
            QuestFn::Play => return self.play(b, e, args),
        };
        if !self.args(b, name, &params, args, e, arity) {
            return TyId::ERROR;
        }
        if f == QuestFn::After
            && let (Some(event), Some(a)) = (params.get(1), args.get(1))
        {
            b.checks.push(Check {
                ty: *event,
                class: Class::Event,
                op: "after",
                at: a.value.span,
            });
        }
        ret
    }

    /// `play(scene, a=…, b=…)`: a scene (or a bark) and its cast by name, each a `Person`. When the scene is
    /// named, its cast is known: every member exactly once.
    fn play(&mut self, b: &mut Body, e: &Expr, args: &[Arg]) -> TyId {
        let Some((scene, cast)) = args.split_first() else {
            b.diags.push(super::error(
                "arg-count",
                "`play` takes a scene and its cast by name: `play(scene, a=…, …)`".to_string(),
                b.file,
                e.span,
            ));
            return TyId::UNIT;
        };
        let found = if scene.name.is_none() {
            self.expr(b, &scene.value)
        } else {
            b.diags.push(super::error(
                "arg-count",
                "`play` takes the scene first, by position".to_string(),
                b.file,
                scene.value.span,
            ));
            self.sink(b, &scene.value);
            TyId::ERROR
        };
        let dialogue = self.types.intern(Ty::Dialogue);
        self.expect(b, dialogue, found, scene.value.span);
        // The named scene's cast.
        let known: Option<Vec<String>> = match &scene.value.kind {
            ExprKind::Def(d) => match self.krate.def(*d).map(|def| &def.kind) {
                Some(DefKind::Dialogue(dl)) => {
                    if dl.kind == DialogueKind::Conversation {
                        let qualified = self.krate.qualified_name(*d);
                        self.mismatch(
                            b,
                            scene.value.span,
                            format!(
                                "`play` plays a scene or a bark; `{qualified}` is a conversation (attach it to an NPC)"
                            ),
                        );
                    }
                    Some(
                        dl.cast
                            .iter()
                            .map(|l| {
                                self.krate
                                    .local(*l)
                                    .map_or(String::new(), |l| l.name.clone())
                            })
                            .collect(),
                    )
                }
                _ => None,
            },
            _ => None,
        };
        let person = self.host_struct("Person");
        let mut given: Vec<&str> = Vec::new();
        for a in cast {
            let found = self.expr(b, &a.value);
            match &a.name {
                None => b.diags.push(super::error(
                    "arg-count",
                    "`play` takes the cast by name (`a=npc`)".to_string(),
                    b.file,
                    a.value.span,
                )),
                Some(n) if given.contains(&n.as_str()) => b.diags.push(super::error(
                    "arg-count",
                    format!("`{n}` is given twice"),
                    b.file,
                    a.value.span,
                )),
                Some(n) if known.as_ref().is_some_and(|k| !k.contains(n)) => {
                    b.diags.push(super::error(
                        "arg-count",
                        format!("the scene has no cast member `{n}`"),
                        b.file,
                        a.value.span,
                    ));
                }
                Some(n) => {
                    given.push(n);
                    self.expect(b, person, found, a.value.span);
                }
            }
        }
        if let Some(known) = known {
            let missing: Vec<String> = known
                .iter()
                .filter(|k| !given.contains(&k.as_str()))
                .map(|k| format!("`{k}`"))
                .collect();
            if !missing.is_empty() {
                b.diags.push(super::error(
                    "arg-count",
                    format!(
                        "`play` needs the whole cast: {} missing",
                        missing.join(", ")
                    ),
                    b.file,
                    e.span,
                ));
            }
        }
        TyId::UNIT
    }

    /// `obj.name(args)` on an objective: one of its commands, on the objective by name.
    pub(super) fn objective_method(
        &mut self,
        b: &mut Body,
        e: &Expr,
        x: &Expr,
        name: &str,
        args: &[Arg],
    ) -> TyId {
        let Some(op) = COMMANDS.into_iter().find(|op| op.name() == name) else {
            let what = if READS.iter().any(|op| op.name() == name) {
                format!("`{name}` is an objective's read: write it without parentheses (`.{name}`)")
            } else {
                format!(
                    "an objective has no method `{name}`: its commands are `finish()`, `fail()`, `reset()` and `progress(n)`"
                )
            };
            b.diags
                .push(super::error("bad-objective-call", what, b.file, e.span));
            self.sink_args(b, args);
            return TyId::ERROR;
        };
        let objective = match &x.kind {
            ExprKind::Def(d) => self.objectives.get(d).map(|count| (*d, *count)),
            _ => None,
        };
        match objective {
            None => b.diags.push(super::error(
                "bad-objective-call",
                format!(
                    "`{name}()` changes an objective's state: call it on the objective by name (`find_dog.{name}()`)"
                ),
                b.file,
                e.span,
            )),
            Some((d, None)) if op == ObjectiveOp::Progress => {
                let qualified = self.krate.qualified_name(d);
                b.diags.push(super::error(
                    "bad-objective-call",
                    format!(
                        "`{qualified}` has no `count`: `progress(n)` counts towards one (`objective … count n`); use `finish()`"
                    ),
                    b.file,
                    e.span,
                ));
            }
            Some(_) => {}
        }
        self.methods.insert(e.id, Method::Objective(op));
        let params = if op == ObjectiveOp::Progress {
            vec![TyId::INT]
        } else {
            Vec::new()
        };
        if !self.args(b, name, &params, args, e, None) {
            return TyId::ERROR;
        }
        TyId::UNIT
    }

    /// `obj.name` on an objective: `done`, `failed` (`bool`) or `count` (`int`).
    pub(super) fn objective_read(&mut self, b: &mut Body, e: &Expr, name: &str) -> TyId {
        let Some(op) = READS.into_iter().find(|op| op.name() == name) else {
            let message = if COMMANDS.iter().any(|op| op.name() == name) {
                format!("`{name}` is an objective's command: call it (`.{name}()`)")
            } else {
                format!("an objective has `done`, `failed` and `count`, not `{name}`")
            };
            b.diags
                .push(super::error("unknown-field", message, b.file, e.span));
            return TyId::ERROR;
        };
        self.methods.insert(e.id, Method::Objective(op));
        match op {
            ObjectiveOp::Count => TyId::INT,
            _ => TyId::BOOL,
        }
    }

    /// Before any body: the field each dialogue `emit` argument's synthesised fn gives, by its name or
    /// position (`Infer::emit_arg_tys`), so `None` or `[]` need no annotation the author cannot write. A
    /// field of an open type (a host `Value`) is left to inference; what does not match a field is left to
    /// `emit_args`.
    pub(super) fn emit_arg_types(&mut self) {
        let krate = self.krate;
        let mut out = BTreeMap::new();
        for def in &krate.defs {
            let DefKind::Dialogue(dl) = &def.kind else {
                continue;
            };
            for step in dl.steps() {
                let StepKind::Emit(event, args) = &step.kind else {
                    continue;
                };
                let shape = match event {
                    EventRef::Script(d) => self.adts.get(d).and_then(|s| s.first()).cloned(),
                    EventRef::Host(i) => self.host_events.get(*i as usize).cloned(),
                };
                let Some(shape) = shape else { continue };
                let mut positional = 0usize;
                for a in args {
                    let index = match &a.name {
                        Some(field) => shape.names.iter().position(|n| n == field),
                        None => {
                            positional += 1;
                            Some(positional - 1)
                        }
                    };
                    if let Some(&t) = index.and_then(|i| shape.tys.get(i))
                        && !self.has_var(t)
                    {
                        out.insert(a.value, t);
                    }
                }
            }
        }
        self.emit_arg_tys = out;
    }

    // After every body.

    /// What the synthesised fns of every dialogue and quest must give: `emit` arguments, targets, speakers.
    pub(super) fn dialogue_checks(&mut self) {
        let krate = self.krate;
        for (i, def) in krate.defs.iter().enumerate() {
            let d = DefId(u32::try_from(i).unwrap_or(u32::MAX));
            let file = self.file_of(d);
            match &def.kind {
                DefKind::Dialogue(dl) => self.dialogue_fns(dl, file),
                DefKind::Quest(q) => {
                    for o in q.stages.iter().flat_map(|s| &s.objectives) {
                        if let Some(at) = o.at {
                            self.entity_fn(at);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn dialogue_fns(&mut self, dl: &Dialogue, file: &str) {
        if let Some(t) = dl.target {
            self.person_fn(t, "a `for` target");
        }
        for step in dl.steps() {
            let speakers: Vec<DefId> = match &step.kind {
                StepKind::Line(l) => l.speaker.iter().cloned().collect(),
                StepKind::Group(members) => members
                    .iter()
                    .filter_map(|m| m.line.speaker.clone())
                    .collect(),
                _ => Vec::new(),
            }
            .into_iter()
            .filter_map(|s| match s {
                Speaker::Fn(f) => Some(f),
                Speaker::Cast(_) => None,
            })
            .collect();
            for s in speakers {
                self.person_fn(s, "a line's speaker");
            }
            if let StepKind::Emit(event, args) = &step.kind {
                self.emit_args(*event, args, file, step.span);
            }
        }
    }

    /// The return type of the synthesised fn `f`; `None` when it was not typed.
    fn ret_of(&self, f: DefId) -> Option<TyId> {
        self.sigs.get(&f).map(|s| s.ret)
    }

    /// An objective's `at` (its marker) gives an entity or a place: a host handle, or an `Option` of one
    /// (`type-mismatch` at its expression).
    fn entity_fn(&mut self, f: DefId) {
        let Some(ret) = self.ret_of(f) else { return };
        let inner = match self.types.get(ret) {
            Ty::Option(t) => t,
            _ => ret,
        };
        if matches!(
            self.types.get(inner),
            Ty::HostStruct(_) | Ty::Error | Ty::Never
        ) {
            return;
        }
        let found = self.show(ret);
        let (file, at) = (self.file_of(f), self.def_span(f));
        self.diags.push(super::error(
            "type-mismatch",
            format!(
                "an objective's `at` marks an entity or a place (a `Person`, a `Creature`, …, or an `Option` of one), not `{found}`"
            ),
            file,
            at,
        ));
    }

    /// `f` gives a `Person` or an `Option[Person]` (`type-mismatch` at its expression): the type of the
    /// dialogue's `npc`, its last parameter, which `f` takes too.
    fn person_fn(&mut self, f: DefId, what: &str) {
        let Some(ret) = self.ret_of(f) else { return };
        let person = match self.sigs.get(&f).and_then(|s| s.params.last()) {
            Some(npc) => *npc,
            None => self.host_struct("Person"),
        };
        let optional = self.types.intern(Ty::Option(person));
        if self.fits(person, ret) || self.fits(optional, ret) {
            return;
        }
        let found = self.show(ret);
        let file = self.file_of(f);
        let at = self.def_span(f);
        self.diags.push(super::error(
            "type-mismatch",
            format!("{what} is a `Person` or an `Option[Person]`, not `{found}`"),
            file,
            at,
        ));
    }

    /// `emit Ev(…)` in a dialogue: every field exactly once, each argument's fn giving the field's type.
    fn emit_args(&mut self, event: EventRef, args: &[EmitArg], file: &str, at: Span) {
        let (name, shape) = match event {
            EventRef::Script(d) => (
                self.krate.qualified_name(d),
                self.adts.get(&d).and_then(|s| s.first()).cloned(),
            ),
            EventRef::Host(i) => (
                self.host
                    .events
                    .get(i as usize)
                    .map_or(String::new(), |e| e.name.clone()),
                self.host_events.get(i as usize).cloned(),
            ),
        };
        let Some(shape) = shape else { return };
        let mut given: Vec<bool> = vec![false; shape.names.len()];
        let mut positional = 0usize;
        for a in args {
            let (afile, aspan) = (self.file_of(a.value), self.def_span(a.value));
            let index = match &a.name {
                Some(field) => match shape.names.iter().position(|n| n == field) {
                    Some(i) => i,
                    None => {
                        self.diags.push(super::error(
                            "unknown-field",
                            format!("`{name}` has no field `{field}`"),
                            afile,
                            aspan,
                        ));
                        continue;
                    }
                },
                None => {
                    positional += 1;
                    positional - 1
                }
            };
            let Some(slot) = given.get_mut(index) else {
                self.diags.push(super::error(
                    "arg-count",
                    format!(
                        "`{name}` has {} field(s), more are given",
                        shape.names.len()
                    ),
                    afile,
                    aspan,
                ));
                continue;
            };
            if std::mem::replace(slot, true) {
                let field = shape.names.get(index).map_or("", String::as_str);
                self.diags.push(super::error(
                    "arg-count",
                    format!("the field `{field}` of `{name}` is given twice"),
                    afile,
                    aspan,
                ));
                continue;
            }
            let (Some(want), Some(found)) = (shape.tys.get(index).copied(), self.ret_of(a.value))
            else {
                continue;
            };
            if !self.fits(want, found) {
                let (w, f) = (self.show(want), self.show(found));
                let field = shape.names.get(index).map_or("", String::as_str);
                self.diags.push(super::error(
                    "type-mismatch",
                    format!("the field `{field}` of `{name}` is `{w}`, not `{f}`"),
                    afile,
                    aspan,
                ));
            }
        }
        let missing: Vec<String> = shape
            .names
            .iter()
            .zip(&given)
            .filter(|(_, g)| !**g)
            .map(|(n, _)| format!("`{n}`"))
            .collect();
        if !missing.is_empty() {
            self.diags.push(super::error(
                "missing-field",
                format!("`{name}` needs every field: {} missing", missing.join(", ")),
                file,
                at,
            ));
        }
    }

    /// Whether the final types `want` and `found` agree: equal, or equal but for parts already reported (the
    /// error type) or that never produce a value.
    fn fits(&self, want: TyId, found: TyId) -> bool {
        let mut work = vec![(want, found)];
        while let Some((a, b)) = work.pop() {
            if a == b {
                continue;
            }
            let (x, y) = (self.types.get(a), self.types.get(b));
            match (x, y) {
                (Ty::Error | Ty::Never, _) | (_, Ty::Error | Ty::Never) => {}
                _ if x.with_children(&[]) == y.with_children(&[]) => {
                    let (xs, ys) = (x.children(), y.children());
                    if xs.is_empty() || xs.len() != ys.len() {
                        return false;
                    }
                    work.extend(xs.into_iter().zip(ys));
                }
                _ => return false,
            }
        }
        true
    }
}
