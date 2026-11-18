//! [`World`]: a quest program's instances run together, standalone (spec §5.11 "The runner", "Across region
//! boundaries", §5.12, §9.1): the home (instance 0, or the only instance of a program without a quest) and
//! one instance per part (`1 + i`), all of the one program, each a [`Runner`]; the [`People`] their
//! queries read; the conversations of the NPCs and their records; the scenes being played; the dialogues
//! handlers attached; the messages between instances.
//!
//! **One tick**, in this order:
//! 1. the messages of the last tick are applied: routed events (a part's `emit` for the home, a
//!    conversation's `emit`, `Talked`, `DialogueEnded`, `SceneEnded`) enter their instance's queue, the
//!    home's mirror is installed in each part it changed for (one refused while the part is mid-segment
//!    waits for the next tick), and `Setup` is delivered when it is due;
//! 2. the inputs are delivered (host events at once; talks, picks, leaves and the ambient schedule wait for
//!    step 4);
//! 3. every instance runs one tick, in id order; then the commands the world acts on (`quest.*`,
//!    `attach_dialogue`, `play`, `bark`, `follow`) are applied;
//! 4. the conversation inputs, then every conversation and scene, are walked;
//! 5. each part's mirror of the home is taken, to be sent when it changed.
//!
//! The stub models every instance, and every NPC, as standing in a region of its own: whatever crosses
//! between them (an `emit`, a mirror, a runner's event) lands at the next tick, as it would across a seam.
//! Conditions are evaluated over the live instance (`Vm::eval_pure`) after its tick ran.
//!
//! **Talking** (spec §5.11): an NPC's attached set is the home's dialogues of the current stage and the
//! program's other dialogues whose `for` target is that NPC, the in-scope filter dialogues whose `where`
//! holds, and those handlers attached (`attach_dialogue`), minus those they detached. A conversation is
//! private: another player talking to the NPC meanwhile gets nothing, and so does a talk to a cast member
//! of an `uninterruptible` scene (an interruptible one ends `Interrupted`). Lines print as `[t] Hal: text`
//! (with `{n}` holes filled; the line id when its text is unknown or ids are asked for), offers as
//! `[t] menu: 1) … 2) …`, the end as `[t] end Hal: Finished`.
//!
//! **The quest's end:** `complete(ps)` records the completions; a one-off quest is then over
//! ([`Outcome::Completed`]), a repeatable one resets every instance (`Vm::reset`), is reaped, has its roles
//! checked again (a failing `where` reaps it for good, [`Outcome::Reaped`]) and is given `Setup` after its
//! `cooldown`. `fail()` ends it ([`Outcome::Failed`]), `decline()` refuses it ([`Outcome::Declined`]); an
//! instance that is over is neither run nor delivered to (conversations and scenes play out).

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;
use std::sync::Arc;

use quest_vm::consts::{COND_FUEL, MEM_BYTES_DEFAULT};
use quest_vm::dialogue::DialogueKind;
use quest_vm::dialogue_run::{
    Attachment, Batch, ConvState, Conversation, DialogueEnv, EndReason, EventShape, InstanceId,
    Offer, PickRefused, Records, SaidLine,
};
use quest_vm::host::{DropReason, Host, HostStruct, PromiseId, QueryError, Report};
use quest_vm::program::{Anchor, Program};
use quest_vm::scene_run::{SceneOut, ScenePlay, SceneReason, cast_ambient};
use quest_vm::types::TypeDesc;
use quest_vm::vm::{Delivery, Status};
use quest_vm::wire::{WireValue, fits};
use quest_vm::{Fnv64, LoadError, Mirror, PureFault, Vm};
use state_hash::StateHash;

use crate::api::*;
use crate::host::StubConfig;
use crate::namespace::Namespace;
use crate::people::{Input, People, QueryCtx, field, id_of, parse_wire, str_of};
use crate::runner::{Runner, vm_config};

/// Compiles the script directory an `edit` input names: the new program and its lines table.
pub type EditFn = Box<dyn FnMut(&str) -> Result<(Arc<Program>, BTreeMap<String, String>), String>>;

/// Where the quest stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Running,
    /// A one-off quest completed.
    Completed,
    Failed,
    /// `decline()` refused the placement.
    Declined,
    /// A repeatable quest whose roles no longer hold after a reset.
    Reaped,
}

/// What one [`World::step`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepReport {
    pub tick: u64,
    /// The fuel each instance used, in id order (0 for one that did not run).
    pub fuel: Vec<u64>,
    /// The instances' reports (a handler aborted, an event dropped or queued), by instance.
    pub reports: Vec<(u16, Report)>,
    /// The pure evaluations that faulted (counted as false), by instance.
    pub faults: Vec<(u32, PureFault)>,
    /// The parts a mirror of the home was installed in.
    pub mirrors: Vec<u16>,
    /// The lines printed.
    pub lines: Vec<String>,
}

struct Inst {
    runner: Runner,
    /// `home`, or the part's name.
    name: String,
}

/// What the runners read through ([`DialogueEnv`]): everything but the conversations and scenes.
struct Core {
    program: Arc<Program>,
    program_key: u64,
    insts: Vec<Inst>,
    people: People,
    completed_by: BTreeSet<i64>,
    now: u64,
    faults: Vec<(u32, PureFault)>,
    api_events: Vec<HostStruct>,
}

/// The host a pure evaluation sees: the people's queries only.
struct EvalHost<'a> {
    people: &'a People,
    completed_by: &'a BTreeSet<i64>,
    tick: u64,
    depth: u32,
}

impl Host for EvalHost<'_> {
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, QueryError> {
        let ctx = QueryCtx {
            tick: self.tick,
            depth: self.depth,
            completed_by: self.completed_by,
        };
        self.people.query(f, args, &ctx).unwrap_or_else(|| {
            Err(QueryError::Failed(format!(
                "the stub answers no query {} in a condition",
                op_name(f).unwrap_or_else(|| format!("(host fn {f})"))
            )))
        })
    }
    fn check(&mut self, f: u16, _: &[WireValue]) -> Result<(), QueryError> {
        Err(QueryError::Failed(format!(
            "host fn {f} checked in a condition"
        )))
    }
    fn command(&mut self, _: u16, _: &[WireValue]) {}
    fn request(&mut self, _: PromiseId, _: u16, _: &[WireValue]) {}
    fn print(&mut self, _: &str) {}
    fn report(&mut self, _: Report) {}
    fn route(&mut self, _: &str, _: &WireValue) {}
}

impl Core {
    fn eval_at(
        &mut self,
        i: usize,
        f: u16,
        args: &[WireValue],
        depth: u32,
    ) -> Result<WireValue, PureFault> {
        let Core {
            insts,
            people,
            completed_by,
            now,
            ..
        } = self;
        let Some(inst) = insts.get_mut(i) else {
            return Err(PureFault::BadCall);
        };
        let mut host = EvalHost {
            people,
            completed_by,
            tick: *now,
            depth,
        };
        inst.runner.vm.eval_pure(f, args, COND_FUEL, &mut host)
    }

    /// The first parameters of fn `f` from `cast` (a target, a filter, a condition takes the cast).
    fn cast_args(&self, f: u16, cast: &[WireValue]) -> Vec<WireValue> {
        let n = self
            .program
            .fns
            .get(usize::from(f))
            .and_then(|x| x.sig.as_ref())
            .map_or(0, |s| s.params.len());
        cast.iter().take(n).cloned().collect()
    }
}

impl DialogueEnv for Core {
    fn graph(&self, i: InstanceId) -> Option<&quest_vm::dialogue::DialogueGraph> {
        let k = usize::try_from(i.0).ok()?;
        (k < self.insts.len()).then_some(&self.program.dialogues)
    }

    fn eval(
        &mut self,
        i: InstanceId,
        f: u16,
        args: &[WireValue],
        depth: u32,
    ) -> Result<WireValue, PureFault> {
        match usize::try_from(i.0) {
            Ok(k) => self.eval_at(k, f, args, depth),
            Err(_) => Err(PureFault::BadCall),
        }
    }

    fn report(&mut self, i: InstanceId, fault: PureFault) {
        self.faults.push((i.0, fault));
    }

    fn event(&self, _: InstanceId, event: u16, script: bool) -> Option<EventShape> {
        if !script {
            let e = self.api_events.get(usize::from(event))?;
            return Some(EventShape {
                name: e.name.clone(),
                fields: e.fields.iter().map(|f| f.0.clone()).collect(),
            });
        }
        let p = &self.program;
        let e = p.events.get(usize::from(event))?;
        match p.types.desc(e.ty).ok()? {
            TypeDesc::Struct { fields, .. } => Some(EventShape {
                name: e.name.clone(),
                fields: fields.iter().map(|f| f.0.clone()).collect(),
            }),
            _ => None,
        }
    }

    fn program_key(&self, i: InstanceId) -> Option<u64> {
        self.graph(i).map(|_| self.program_key)
    }
}

/// A player's conversation with an NPC.
struct Talk {
    player: i64,
    conv: Conversation,
}

pub struct World {
    core: Core,
    /// The line texts by id (`quest_lang::Compiled::lines`): `{n}` holes.
    lines: BTreeMap<String, String>,
    show_ids: bool,
    fuel: u64,
    roles: Vec<(String, WireValue)>,
    outcome: Outcome,
    /// The tick `Setup` is delivered at.
    setup_at: Option<u64>,
    /// The quest's namespace (`world.*`), lent to the instance running.
    namespace: Namespace,
    /// Completions by player id.
    completions: BTreeMap<i64, u32>,
    /// Conversations by NPC id.
    talks: BTreeMap<i64, Talk>,
    /// Records by NPC id (a scene's are its lead's).
    records: BTreeMap<i64, Records>,
    scenes: Vec<ScenePlay>,
    /// When each scene was last cast by the ambient schedule, by (program key, qualified name).
    played: BTreeMap<(Option<u64>, String), u64>,
    /// `attach_dialogue` / `detach_dialogue`: (instance, NPC id, dialogue).
    attached: BTreeSet<(u32, i64, String)>,
    detached: BTreeSet<(u32, i64, String)>,
    /// Events for the next tick: (instance, event name, payload).
    pending: Vec<(u32, String, WireValue)>,
    /// Mirrors of the home to install in parts at the next tick, by instance.
    pending_mirrors: BTreeMap<u16, Mirror>,
    /// The hash of the mirror last sent to each part.
    sent: BTreeMap<u16, u64>,
    /// Commands the world acts on, from the instances' tick: (instance, host fn, arguments).
    cmds: Vec<(u16, u16, Vec<WireValue>)>,
    output: Vec<String>,
    report: StepReport,
    edit: Option<EditFn>,
}

fn unit_struct(name: &str, fields: Vec<(&str, WireValue)>) -> WireValue {
    WireValue::Struct {
        name: name.into(),
        fields: fields
            .into_iter()
            .map(|(n, v)| (n.to_string(), v))
            .collect(),
    }
}

fn bare(name: &str) -> WireValue {
    WireValue::Variant {
        name: name.into(),
        fields: vec![],
    }
}

fn hash_of<T: StateHash + ?Sized>(t: &T) -> u64 {
    let mut h = Fnv64::default();
    t.state_hash(&mut h);
    h.finish()
}

/// A value as a line shows it.
fn show(v: &WireValue) -> String {
    match v {
        WireValue::Unit => "()".into(),
        WireValue::Bool(b) => b.to_string(),
        WireValue::Int(i) => i.to_string(),
        WireValue::Float(f) => f.to_string(),
        WireValue::Str(s) => s.clone(),
        WireValue::List(items) => {
            let items: Vec<String> = items.iter().map(show).collect();
            format!("[{}]", items.join(", "))
        }
        WireValue::Struct { name, fields } => match (field(v, "name"), field(v, "id")) {
            (Some(WireValue::Str(n)), _) => n.clone(),
            (_, Some(WireValue::Int(id))) => format!("{name}({id})"),
            _ => {
                let fs: Vec<String> = fields
                    .iter()
                    .map(|(n, v)| format!("{n}={}", show(v)))
                    .collect();
                format!("{name}({})", fs.join(", "))
            }
        },
        WireValue::Variant { name, fields } => match fields.as_slice() {
            [] => name.clone(),
            fs => {
                let fs: Vec<String> = fs.iter().map(|(_, v)| show(v)).collect();
                format!("{name}({})", fs.join(", "))
            }
        },
        other => format!("{other:?}"),
    }
}

/// `template`'s `{n}` holes filled from `args` (`{{` and `}}` are braces).
fn fill(template: &str, args: &[WireValue]) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut n = String::new();
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                    n.push(d);
                }
                if let Some(v) = n.parse::<usize>().ok().and_then(|k| args.get(k)) {
                    out.push_str(&show(v));
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn end_name(r: EndReason) -> &'static str {
    match r {
        EndReason::Finished => "Finished",
        EndReason::Left => "Left",
        EndReason::Timeout => "Timeout",
        EndReason::TooDeep => "TooDeep",
        EndReason::Edited => "Edited",
        EndReason::OutOfRange => "OutOfRange",
    }
}

fn scene_reason_name(r: SceneReason) -> &'static str {
    match r {
        SceneReason::Finished => "Finished",
        SceneReason::Interrupted => "Interrupted",
        SceneReason::ActorLost => "ActorLost",
    }
}

/// A conversation with nothing more to happen without input: closed, or waiting for a pick with no timeout.
fn conv_quiet(c: &Conversation) -> bool {
    match c.state() {
        ConvState::Closed => true,
        ConvState::AwaitingPick => c.offer().is_none_or(|o| o.timeout_at.is_none()),
        _ => false,
    }
}

/// Over: ended or closed.
fn conv_over(c: &Conversation) -> bool {
    matches!(c.state(), ConvState::Ended { .. } | ConvState::Closed)
}

fn load_error(e: LoadError) -> String {
    match e {
        LoadError::Role(r) => {
            format!("role {r} is refused: unknown, given twice, missing, or of the wrong type")
        }
        other => format!("the program does not load: {other:?}"),
    }
}

impl World {
    /// The quest's instances placed: `requires` and each role's `where` hold (evaluated over `people`), the
    /// home and one instance per part made with `roles`, `Setup` due at the first tick. A role of the wrong
    /// type, a `requires` or a `where` that does not hold, or a program that does not load is `Err`.
    pub fn new(
        program: Arc<Program>,
        people: People,
        roles: Vec<(String, WireValue)>,
        cfg: StubConfig,
        fuel: u64,
    ) -> Result<World, String> {
        let home = Runner::for_instance(
            Arc::clone(&program),
            MEM_BYTES_DEFAULT,
            cfg.clone(),
            0,
            roles.clone(),
        )
        .map_err(load_error)?;
        let mut insts = vec![Inst {
            runner: home,
            name: "home".into(),
        }];
        let parts: Vec<String> = program
            .quest
            .as_ref()
            .map(|q| q.parts.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        let mut w = World {
            core: Core {
                program_key: program.content_hash(),
                program: Arc::clone(&program),
                insts: Vec::new(),
                people,
                completed_by: BTreeSet::new(),
                now: 0,
                faults: Vec::new(),
                api_events: stub_api().events,
            },
            lines: BTreeMap::new(),
            show_ids: false,
            fuel,
            roles,
            outcome: Outcome::Running,
            setup_at: Some(0),
            namespace: Namespace::default(),
            completions: BTreeMap::new(),
            talks: BTreeMap::new(),
            records: BTreeMap::new(),
            scenes: Vec::new(),
            played: BTreeMap::new(),
            attached: BTreeSet::new(),
            detached: BTreeSet::new(),
            pending: Vec::new(),
            pending_mirrors: BTreeMap::new(),
            sent: BTreeMap::new(),
            cmds: Vec::new(),
            output: Vec::new(),
            report: StepReport::default(),
            edit: None,
        };
        w.core.insts.append(&mut insts);
        w.check_placement()?;
        for (k, name) in parts.into_iter().enumerate() {
            let id = u16::try_from(k + 1).map_err(|_| "too many parts".to_string())?;
            let runner = Runner::for_instance(
                Arc::clone(&program),
                MEM_BYTES_DEFAULT,
                cfg.clone(),
                id,
                w.roles.clone(),
            )
            .map_err(load_error)?;
            w.core.insts.push(Inst { runner, name });
        }
        for inst in &mut w.core.insts {
            inst.runner.host.in_world = true;
        }
        Ok(w)
    }

    /// The `requires` conditions, then each bound role's `where`, over the home as it is.
    fn check_placement(&mut self) -> Result<(), String> {
        let program = Arc::clone(&self.core.program);
        let Some(q) = program.quest.as_ref() else {
            return Ok(());
        };
        for (k, &f) in q.requires.iter().enumerate() {
            match self.core.eval_at(0, f, &[], 1) {
                Ok(WireValue::Bool(true)) => {}
                other => {
                    return Err(format!(
                        "requirement {} of {} does not hold ({other:?})",
                        k + 1,
                        q.name
                    ));
                }
            }
        }
        for (k, r) in q.roles.iter().enumerate() {
            let (Some(f), Some((_, value))) =
                (r.cond, self.roles.iter().find(|(n, _)| *n == r.name))
            else {
                continue;
            };
            let params = program
                .fns
                .get(usize::from(f))
                .and_then(|x| x.sig.as_ref())
                .map(|s| s.params.clone())
                .unwrap_or_default();
            // The earlier roles (an optional one as `Some(v)` / `None` where its type asks), then this one.
            let mut args = Vec::new();
            for (j, earlier) in q.roles[..k].iter().enumerate() {
                let given = self.roles.iter().find(|(n, _)| *n == earlier.name);
                let plain = given.map(|(_, v)| v.clone());
                let wrapped = match &plain {
                    Some(v) => WireValue::Variant {
                        name: "Some".into(),
                        fields: vec![("value".into(), v.clone())],
                    },
                    None => bare("None"),
                };
                let ty = params.get(j).copied();
                let fit = |v: &WireValue| ty.is_some_and(|t| fits(&program.types, t, v));
                args.push(match plain {
                    Some(v) if fit(&v) => v,
                    _ => wrapped,
                });
            }
            args.push(value.clone());
            match self.core.eval_at(0, f, &args, 1) {
                Ok(WireValue::Bool(true)) => {}
                other => {
                    return Err(format!(
                        "role {}: its `where` does not hold ({other:?})",
                        r.name
                    ));
                }
            }
        }
        Ok(())
    }

    /// Lines print the line ids (with their substitutions) instead of the texts.
    pub fn show_ids(&mut self, on: bool) {
        self.show_ids = on;
    }

    /// The lines table (`quest_lang::Compiled::lines`) the lines' texts come from.
    pub fn set_lines(&mut self, lines: BTreeMap<String, String>) {
        self.lines = lines;
    }

    /// What compiles the directory an `edit` input names.
    pub fn set_edit(&mut self, edit: EditFn) {
        self.edit = Some(edit);
    }

    /// The tick the next [`World::step`] runs.
    pub fn now(&self) -> u64 {
        self.core.now
    }

    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// Every line printed so far, `[tick] text`.
    pub fn output(&self) -> &[String] {
        &self.output
    }

    pub fn people(&self) -> &People {
        &self.core.people
    }

    pub fn program(&self) -> &Arc<Program> {
        &self.core.program
    }

    /// Instance `i`'s VM (0 the home, `1 + i` part `i`).
    pub fn vm(&self, i: u16) -> Option<&Vm> {
        self.core.insts.get(usize::from(i)).map(|x| &x.runner.vm)
    }

    /// How often each player completed the quest.
    pub fn completions(&self) -> &BTreeMap<i64, u32> {
        &self.completions
    }

    /// The instances: 1 + the parts.
    pub fn instances(&self) -> usize {
        self.core.insts.len()
    }

    fn say(&mut self, line: String) {
        let line = format!("[{}] {line}", self.core.now);
        self.report.lines.push(line.clone());
        self.output.push(line);
    }

    /// The lowest instance (other than `not`) with a handler of `event`.
    fn owner_of(&self, event: &str, not: Option<u16>) -> Option<u16> {
        self.core
            .program
            .handlers
            .iter()
            .filter(|h| h.event == event && Some(h.owner) != not)
            .map(|h| h.owner)
            .min()
            .filter(|o| usize::from(*o) < self.core.insts.len())
    }

    /// Every instance with a handler of `event`, in id order.
    fn owners_of(&self, event: &str) -> Vec<u16> {
        let owners: BTreeSet<u16> = self
            .core
            .program
            .handlers
            .iter()
            .filter(|h| h.event == event && usize::from(h.owner) < self.core.insts.len())
            .map(|h| h.owner)
            .collect();
        owners.into_iter().collect()
    }

    /// Delivers to instance `i`; a drop is reported, except `NoHandler` when `quiet`.
    fn deliver(&mut self, i: u16, event: &str, value: &WireValue, quiet: bool) {
        if self.outcome != Outcome::Running {
            return;
        }
        let Some(inst) = self.core.insts.get_mut(usize::from(i)) else {
            return;
        };
        let report = match inst.runner.vm.deliver(event, value) {
            Delivery::Accepted => return,
            Delivery::Dropped(DropReason::NoHandler) if quiet => return,
            Delivery::Queued => Report::EventQueued {
                event: event.into(),
            },
            Delivery::Dropped(reason) => Report::EventDropped {
                event: event.into(),
                reason,
            },
        };
        self.report.reports.push((i, report));
    }

    /// An event for the next tick, to `i` when it handles it, else to the instance that does.
    fn send(&mut self, i: u32, event: String, value: WireValue) {
        let own = self
            .core
            .program
            .handlers
            .iter()
            .any(|h| h.event == event && u32::from(h.owner) == i);
        let to = if own {
            i
        } else {
            self.owner_of(&event, None).map_or(i, u32::from)
        };
        self.pending.push((to, event, value));
    }

    /// Runs one tick (module docs).
    pub fn step(&mut self, inputs: &[Input]) -> StepReport {
        let t = self.core.now;
        self.report = StepReport {
            tick: t,
            fuel: vec![0; self.core.insts.len()],
            ..StepReport::default()
        };
        // 1. The last tick's messages.
        if self.outcome == Outcome::Running {
            for (i, event, value) in std::mem::take(&mut self.pending) {
                if let Ok(i) = u16::try_from(i) {
                    self.deliver(i, &event, &value, true);
                }
            }
            self.install_mirrors();
            if self.setup_at.is_some_and(|at| at <= t) {
                self.setup_at = None;
                let setup = unit_struct("Setup", vec![]);
                for i in 0..self.core.insts.len() {
                    if let Ok(i) = u16::try_from(i) {
                        self.deliver(i, "Setup", &setup, true);
                    }
                }
            }
        } else {
            self.pending.clear();
            self.pending_mirrors.clear();
        }
        // 2. The inputs.
        let mut talk = Vec::new();
        for input in inputs {
            match input {
                Input::Talk { .. }
                | Input::Pick { .. }
                | Input::Leave { .. }
                | Input::Done { .. }
                | Input::Ambient { .. } => talk.push(input.clone()),
                Input::Event { name, fields, part } => self.event_input(name, fields, part),
                Input::Near { player, entity } => self.near(*player, *entity),
                Input::Kill { victim, by } => self.kill(*victim, *by),
                Input::Lose { entity } => self.lose(*entity),
                Input::Edit { dir } => self.edit_input(dir),
            }
        }
        // 3. The instances, then the commands the world acts on.
        if self.outcome == Outcome::Running {
            for i in 0..self.core.insts.len() {
                self.tick_instance(i);
            }
            for (i, f, args) in std::mem::take(&mut self.cmds) {
                self.world_command(i, f, &args);
            }
        }
        // 4. Conversations and scenes.
        for input in talk {
            self.talk_input(&input);
        }
        self.tick_talks();
        self.tick_scenes();
        // 5. The mirrors for the next tick.
        if self.outcome == Outcome::Running {
            self.collect_mirrors();
        }
        self.report.faults = std::mem::take(&mut self.core.faults);
        for (i, fault) in self.report.faults.clone() {
            self.say(format!("fault in a condition of instance {i}: {fault:?}"));
        }
        self.core.now += 1;
        std::mem::take(&mut self.report)
    }

    /// Steps through `script` (inputs at their ticks; one earlier than now is given now) until it is used
    /// up and the world is [`World::quiescent`], or `max_ticks` steps ran. `true` when it got there.
    pub fn run(&mut self, script: &[(u64, Input)], max_ticks: u64) -> bool {
        let mut next = 0;
        for _ in 0..max_ticks {
            if next == script.len() && self.quiescent() {
                return true;
            }
            let now = self.core.now;
            let k = script[next..].iter().take_while(|(t, _)| *t <= now).count();
            let inputs: Vec<Input> = script[next..next + k]
                .iter()
                .map(|(_, i)| i.clone())
                .collect();
            next += k;
            self.step(&inputs);
        }
        next == script.len() && self.quiescent()
    }

    fn tick_instance(&mut self, i: usize) {
        let Core {
            insts,
            people,
            completed_by,
            ..
        } = &mut self.core;
        let Some(inst) = insts.get_mut(i) else {
            return;
        };
        let host = &mut inst.runner.host;
        host.people = std::mem::take(people);
        std::mem::swap(&mut host.namespace, &mut self.namespace);
        host.completed_by.clone_from(completed_by);
        let rep = inst.runner.step(self.fuel);
        let host = &mut inst.runner.host;
        *people = std::mem::take(&mut host.people);
        std::mem::swap(&mut host.namespace, &mut self.namespace);
        let lines = std::mem::take(&mut host.output);
        let reports = std::mem::take(&mut host.reports);
        let routed = std::mem::take(&mut host.routed);
        let cmds = std::mem::take(&mut host.world_cmds);
        if let Some(f) = self.report.fuel.get_mut(i) {
            *f = rep.fuel_used;
        }
        let i16 = u16::try_from(i).unwrap_or(u16::MAX);
        for line in lines {
            self.say(line);
        }
        for r in reports {
            self.report.reports.push((i16, r));
        }
        for (event, value) in routed {
            if let Some(to) = self.owner_of(&event, Some(i16)) {
                self.pending.push((u32::from(to), event, value));
            }
        }
        self.cmds
            .extend(cmds.into_iter().map(|(f, args)| (i16, f, args)));
    }

    // ---- inputs -------------------------------------------------------------------------------------

    fn event_input(&mut self, name: &str, fields: &[(String, String)], part: &Option<String>) {
        let mut values = Vec::new();
        for (f, text) in fields {
            match parse_wire(text, &self.core.people) {
                Ok(v) => values.push((f.clone(), v)),
                Err(e) => return self.say(format!("event {name}: {e}")),
            }
        }
        let to = match part {
            None => Some(0),
            Some(p) => self
                .core
                .insts
                .iter()
                .position(|x| x.name == *p)
                .and_then(|k| u16::try_from(k).ok()),
        };
        let Some(to) = to else {
            return self.say(format!(
                "event {name}: no part {}",
                part.as_deref().unwrap_or("")
            ));
        };
        let value = WireValue::Struct {
            name: name.into(),
            fields: values,
        };
        self.deliver(to, name, &value, false);
    }

    /// The parts anchored on `entity` (`part … on role`, the role bound to it).
    fn parts_on(&self, entity: i64) -> Vec<u16> {
        let Some(q) = self.core.program.quest.as_ref() else {
            return Vec::new();
        };
        q.parts
            .iter()
            .enumerate()
            .filter(|(_, p)| p.anchor == Anchor::On)
            .filter(|(_, p)| {
                q.roles.get(usize::from(p.role)).is_some_and(|r| {
                    self.roles
                        .iter()
                        .any(|(n, v)| *n == r.name && id_of(Some(v)) == Some(entity))
                })
            })
            .filter_map(|(k, _)| u16::try_from(k + 1).ok())
            .collect()
    }

    fn near(&mut self, player: i64, entity: i64) {
        let Some(p) = self.core.people.handle(player) else {
            return self.say(format!("near: no entity {player}"));
        };
        let value = unit_struct("PlayerNear", vec![("player", p)]);
        for i in self.parts_on(entity) {
            self.deliver(i, "PlayerNear", &value, true);
        }
    }

    fn kill(&mut self, victim: i64, by: Option<i64>) {
        if let Some(e) = self.core.people.entities.get_mut(&victim) {
            e.alive = false;
        }
        let by = match by.map(|b| self.core.people.handle(b)) {
            Some(Some(p)) => WireValue::Variant {
                name: "Some".into(),
                fields: vec![("value".into(), p)],
            },
            Some(None) => return self.say("kill: an unknown killer".into()),
            None => bare("None"),
        };
        let value = unit_struct(
            "Killed",
            vec![
                (
                    "victim",
                    unit_struct("Entity", vec![("id", WireValue::Int(victim))]),
                ),
                ("by", by),
            ],
        );
        for i in self.owners_of("Killed") {
            self.deliver(i, "Killed", &value, true);
        }
        let ended: Vec<SceneOut> = self
            .scenes
            .iter_mut()
            .filter(|s| !s.ended() && s.cast().iter().any(|c| id_of(Some(c)) == Some(victim)))
            .map(ScenePlay::actor_lost)
            .collect();
        for out in ended {
            self.scene_out(out);
        }
        self.lose(victim);
    }

    /// `RoleLost` to the home for each role bound to `entity`.
    fn lose(&mut self, entity: i64) {
        let program = Arc::clone(&self.core.program);
        let Some(q) = program.quest.as_ref() else {
            return;
        };
        let Some(ev) = program
            .events
            .iter()
            .find(|e| e.name == format!("{}::RoleLost", q.name) || e.name.ends_with("::RoleLost"))
        else {
            return;
        };
        let field = match program.types.desc(ev.ty) {
            Ok(TypeDesc::Struct { fields, .. }) => fields.first().map(|f| f.0.clone()),
            _ => None,
        }
        .unwrap_or_else(|| "role".into());
        for r in &q.roles {
            let bound = self
                .roles
                .iter()
                .any(|(n, v)| *n == r.name && id_of(Some(v)) == Some(entity));
            if bound {
                let value = WireValue::Struct {
                    name: ev.name.clone(),
                    fields: vec![(field.clone(), bare(&r.name))],
                };
                self.deliver(0, &ev.name, &value, false);
            }
        }
    }

    fn edit_input(&mut self, dir: &str) {
        let Some(mut f) = self.edit.take() else {
            return self.say(format!("edit {dir}: no compiler"));
        };
        let result = f(dir);
        self.edit = Some(f);
        match result {
            Ok((program, lines)) => {
                if let Err(e) = self.edit(program, lines) {
                    self.say(format!("edit {dir}: {e}"));
                }
            }
            Err(e) => self.say(format!("edit {dir}: {e}")),
        }
    }

    /// The script is edited (spec §6.9): every instance migrates to `program` (all or none: one refused
    /// leaves them all as they were), then every conversation and scene finds its place again.
    pub fn edit(
        &mut self,
        program: Arc<Program>,
        lines: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let mut vms = Vec::new();
        for (k, inst) in self.core.insts.iter().enumerate() {
            let id = u16::try_from(k).map_err(|_| "too many instances".to_string())?;
            let cfg = vm_config(MEM_BYTES_DEFAULT, id, self.roles.clone());
            match inst.runner.vm.migrate(Arc::clone(&program), cfg) {
                Ok((vm, _)) => vms.push(vm),
                Err(e) => return Err(format!("the {} refused the edit: {e:?}", inst.name)),
            }
        }
        for (inst, vm) in self.core.insts.iter_mut().zip(vms) {
            inst.runner.vm = vm;
        }
        self.core.program_key = program.content_hash();
        self.core.program = program;
        self.lines = lines;
        self.say("edited".into());
        let npcs: Vec<i64> = self.talks.keys().copied().collect();
        for npc in npcs {
            let Some(tk) = self.talks.get_mut(&npc) else {
                continue;
            };
            if let Some(b) = tk.conv.rebind(&mut self.core) {
                self.print_batch(npc, b);
            }
        }
        let outs: Vec<SceneOut> = self
            .scenes
            .iter_mut()
            .filter_map(|s| s.rebind(&mut self.core))
            .collect();
        for out in outs {
            self.scene_out(out);
        }
        Ok(())
    }

    // ---- the commands the world acts on -------------------------------------------------------------

    fn world_command(&mut self, i: u16, f: u16, args: &[WireValue]) {
        let people = &self.core.people;
        let name =
            |v: Option<&WireValue>| match (str_of(v.and_then(|p| field(p, "name"))), id_of(v)) {
                (Some(n), _) => n.to_string(),
                (None, Some(id)) => people.name_of(id),
                _ => "?".into(),
            };
        match f {
            F_QUEST_JOINED => self.say(format!("joined {}", name(args.first()))),
            F_QUEST_LEFT => self.say(format!("left {}", name(args.first()))),
            F_QUEST_JOIN_REFUSED => self.say(format!("join refused {}", name(args.first()))),
            F_QUEST_COMPLETE => {
                let ps: Vec<WireValue> = match args.first() {
                    Some(WireValue::List(ps)) => ps.clone(),
                    _ => Vec::new(),
                };
                let names: Vec<String> = ps.iter().map(|p| name(Some(p))).collect();
                if self.outcome != Outcome::Running {
                    return;
                }
                for id in ps.iter().filter_map(|p| id_of(Some(p))) {
                    *self.completions.entry(id).or_insert(0) += 1;
                    self.core.completed_by.insert(id);
                }
                self.say(format!("completed {}", names.join(", ")));
                self.completed();
            }
            F_QUEST_FAIL if self.outcome == Outcome::Running => {
                self.say("failed".into());
                self.outcome = Outcome::Failed;
            }
            F_QUEST_DECLINE if self.outcome == Outcome::Running => {
                self.say("declined".into());
                self.outcome = Outcome::Declined;
            }
            F_QUEST_PLAY => match (str_of(args.first()), args.get(1)) {
                (Some(scene), Some(WireValue::List(cast))) => {
                    let (scene, cast) = (scene.to_string(), cast.clone());
                    self.start_scene(i, &scene, cast);
                }
                _ => self.say("play: bad arguments".into()),
            },
            F_BARK => match (args.first(), str_of(args.get(1))) {
                (Some(npc), Some(d)) => {
                    let (npc, d) = (npc.clone(), d.to_string());
                    self.start_scene(i, &d, vec![npc]);
                }
                _ => self.say("bark: bad arguments".into()),
            },
            F_ATTACH_DIALOGUE | F_DETACH_DIALOGUE => {
                let (Some(npc), Some(d)) = (id_of(args.first()), str_of(args.get(1))) else {
                    return self.say("attach_dialogue: bad arguments".into());
                };
                let key = (u32::from(i), npc, d.to_string());
                if f == F_ATTACH_DIALOGUE {
                    self.detached.remove(&key);
                    self.attached.insert(key);
                } else {
                    self.attached.remove(&key);
                    self.detached.insert(key);
                }
            }
            F_FOLLOW => {
                let (Some(c), Some(p)) = (id_of(args.first()), id_of(args.get(1))) else {
                    return self.say("follow: bad arguments".into());
                };
                let line = format!("follow {} {}", people.name_of(c), name(args.get(1)));
                if let Some(e) = self.core.people.entities.get_mut(&c) {
                    e.following = Some(p);
                }
                self.say(line);
            }
            _ => {}
        }
    }

    /// After `complete`: a one-off quest is over; a repeatable one resets (module docs).
    fn completed(&mut self) {
        let program = Arc::clone(&self.core.program);
        let Some(repeat) = program.quest.as_ref().and_then(|q| q.repeat) else {
            self.outcome = Outcome::Completed;
            return;
        };
        for k in 0..self.core.insts.len() {
            let inst = &mut self.core.insts[k];
            if let Err(e) = inst.runner.vm.reset() {
                let line = format!("the {} did not reset: {e:?}", inst.name);
                self.say(line);
            }
        }
        self.pending.clear();
        self.pending_mirrors.clear();
        self.sent.clear();
        self.say("reaped".into());
        if let Err(e) = self.check_placement() {
            self.say(format!("reaped for good: {e}"));
            self.outcome = Outcome::Reaped;
            return;
        }
        self.setup_at = Some(self.core.now.saturating_add(repeat.cooldown.max(1)));
    }

    // ---- conversations ------------------------------------------------------------------------------

    /// The home's current stage (an index into `QuestInfo::stages`).
    fn stage(&self) -> Option<u16> {
        let q = self.core.program.quest.as_ref()?;
        let home = &self.core.insts.first()?.runner.vm;
        let m = home.mirror(&[q.stage_global]);
        match m.globals.get(&q.stage_global)? {
            WireValue::Variant { name, .. } => q
                .stages
                .iter()
                .position(|s| s == name)
                .and_then(|k| u16::try_from(k).ok()),
            _ => None,
        }
    }

    /// What is attached to `npc` for `player` now (module docs).
    fn attachments(&mut self, player: &WireValue, npc: &WireValue) -> Vec<Attachment> {
        if self.outcome != Outcome::Running {
            return Vec::new();
        }
        let Some(npc_id) = id_of(Some(npc)) else {
            return Vec::new();
        };
        let stage = self.stage();
        let program = Arc::clone(&self.core.program);
        let cast = [player.clone(), npc.clone()];
        let in_stage =
            |d: &quest_vm::dialogue::DialogueDef| d.stage.is_none_or(|s| Some(s) == stage);
        let mut out = Vec::new();
        for (k, d) in program.dialogues.dialogues.iter().enumerate() {
            let Ok(k) = u16::try_from(k) else { break };
            if d.kind != DialogueKind::Conversation || d.cast != 2 || !in_stage(d) {
                continue;
            }
            if self.detached.contains(&(0, npc_id, d.name.clone())) {
                continue;
            }
            // A fault counts as false and is reported, as in any condition.
            let holds = match d.target.or(d.filter) {
                Some(f) => match self.core.eval_at(0, f, &self.core.cast_args(f, &cast), 1) {
                    Ok(v) if d.target.is_some() => id_of(Some(&v)) == Some(npc_id),
                    Ok(v) => v == WireValue::Bool(true),
                    Err(fault) => {
                        self.core.faults.push((0, fault));
                        false
                    }
                },
                None => false,
            };
            if holds {
                out.push(Attachment {
                    instance: InstanceId(0),
                    dialogue: k,
                });
            }
        }
        for (i, n, name) in &self.attached {
            if *n != npc_id {
                continue;
            }
            let found = program
                .dialogues
                .dialogues
                .iter()
                .position(|d| d.name == *name && in_stage(d))
                .and_then(|k| u16::try_from(k).ok());
            if let Some(k) = found {
                out.push(Attachment {
                    instance: InstanceId(*i),
                    dialogue: k,
                });
            }
        }
        out
    }

    /// The NPC `player` is talking to (a conversation not over).
    fn talking_to(&self, player: i64) -> Option<i64> {
        self.talks
            .iter()
            .find(|(_, tk)| tk.player == player && !conv_over(&tk.conv))
            .map(|(npc, _)| *npc)
    }

    fn talk_input(&mut self, input: &Input) {
        match *input {
            Input::Talk { player, npc } => self.talk(player, npc),
            Input::Pick { player, option } => self.pick(player, option),
            Input::Leave { player } => {
                if let Some(npc) = self.talking_to(player) {
                    let now = self.core.now;
                    if let Some(tk) = self.talks.get_mut(&npc) {
                        let b = tk.conv.leave(now);
                        self.print_batch(npc, b);
                    }
                }
            }
            Input::Done { player } => {
                for tk in self.talks.values_mut().filter(|tk| tk.player == player) {
                    tk.conv.done();
                }
                self.talks
                    .retain(|_, tk| *tk.conv.state() != ConvState::Closed);
            }
            Input::Ambient { ref npcs } => self.ambient(npcs),
            _ => {}
        }
    }

    fn talk(&mut self, player: i64, npc: i64) {
        let (Some(p), Some(n)) = (
            self.core.people.handle(player),
            self.core.people.handle(npc),
        ) else {
            return self.say(format!("talk: no entity {player} or {npc}"));
        };
        // A cast member: an interruptible scene ends; an uninterruptible one keeps the NPC busy.
        if let Some(k) = self
            .scenes
            .iter()
            .position(|s| !s.ended() && s.cast().iter().any(|c| id_of(Some(c)) == Some(npc)))
        {
            match self.scenes[k].interrupt() {
                Some(out) => self.scene_out(out),
                None => return,
            }
        }
        if let Some(tk) = self.talks.get(&npc)
            && *tk.conv.state() != ConvState::Closed
            && (tk.player != player || !conv_over(&tk.conv))
        {
            return;
        }
        if self.talking_to(player).is_some() {
            return;
        }
        let attached = self.attachments(&p, &n);
        let talked = unit_struct("Talked", vec![("player", p.clone()), ("npc", n.clone())]);
        for i in self.owners_of("Talked") {
            self.pending
                .push((u32::from(i), "Talked".into(), talked.clone()));
        }
        let now = self.core.now;
        let rec = self.records.entry(npc).or_default();
        let (conv, batch) = Conversation::start(&mut self.core, &attached, rec, p, n, now);
        self.talks.insert(npc, Talk { player, conv });
        self.print_batch(npc, batch);
        self.talks
            .retain(|_, tk| *tk.conv.state() != ConvState::Closed);
    }

    fn pick(&mut self, player: i64, option: u32) {
        let Some(npc) = self
            .talks
            .iter()
            .find(|(_, tk)| tk.player == player && tk.conv.offer().is_some())
            .map(|(npc, _)| *npc)
        else {
            return;
        };
        let (Some(p), Some(n)) = (
            self.core.people.handle(player),
            self.core.people.handle(npc),
        ) else {
            return;
        };
        let attached = self.attachments(&p, &n);
        let now = self.core.now;
        let Some(tk) = self.talks.get_mut(&npc) else {
            return;
        };
        let Some(serial) = tk.conv.offer().map(|o| o.serial) else {
            return;
        };
        let rec = self.records.entry(npc).or_default();
        let result = tk.conv.pick(
            &mut self.core,
            &attached,
            rec,
            serial,
            option.saturating_sub(1),
            now,
        );
        match result {
            Ok(b) => self.print_batch(npc, b),
            Err(PickRefused::Stale { resent } | PickRefused::OutOfRange { resent }) => {
                self.print_offer(&resent)
            }
            Err(PickRefused::NoOffer) => {}
        }
    }

    fn tick_talks(&mut self) {
        let now = self.core.now;
        let npcs: Vec<i64> = self.talks.keys().copied().collect();
        for npc in npcs {
            let Some(tk) = self.talks.get_mut(&npc) else {
                continue;
            };
            let rec = self.records.entry(npc).or_default();
            if let Some(b) = tk.conv.tick(&mut self.core, rec, now) {
                self.print_batch(npc, b);
            }
        }
        self.talks
            .retain(|_, tk| *tk.conv.state() != ConvState::Closed);
    }

    fn speaker(&self, v: &WireValue) -> String {
        match (str_of(field(v, "name")), id_of(Some(v))) {
            (Some(n), _) => n.to_string(),
            (None, Some(id)) => self.core.people.name_of(id),
            _ => show(v),
        }
    }

    fn text(&self, line: &SaidLine) -> String {
        match self.lines.get(&line.id) {
            Some(t) if !self.show_ids => fill(t, &line.args),
            _ if line.args.is_empty() => format!("@{}", line.id),
            _ => {
                let args: Vec<String> = line.args.iter().map(show).collect();
                format!("@{}({})", line.id, args.join(", "))
            }
        }
    }

    fn print_offer(&mut self, offer: &Offer) {
        let options: Vec<String> = offer
            .options
            .iter()
            .enumerate()
            .map(|(k, o)| format!("{}) {}", k + 1, self.text(o)))
            .collect();
        self.say(format!("menu: {}", options.join(" ")));
    }

    /// Prints a conversation's batch and sends what it raised.
    fn print_batch(&mut self, npc: i64, b: Batch) {
        for line in &b.lines {
            let said = format!("{}: {}", self.speaker(&line.speaker), self.text(line));
            self.say(said);
        }
        if let Some(offer) = &b.offer {
            self.print_offer(offer);
        }
        for e in b.emits {
            self.send(e.instance.0, e.event, e.value);
        }
        let player = self.talks.get(&npc).map(|tk| tk.player);
        let (p, n) = (
            player.and_then(|p| self.core.people.handle(p)),
            self.core.people.handle(npc),
        );
        for e in b.ended {
            let (Some(p), Some(n)) = (p.clone(), n.clone()) else {
                continue;
            };
            let value = unit_struct(
                "DialogueEnded",
                vec![
                    ("player", p),
                    ("npc", n),
                    ("dialogue", WireValue::Str(e.dialogue.clone())),
                    ("reason", bare(end_name(e.reason))),
                    (
                        "exit",
                        WireValue::Str(format!("{}::{}", e.dialogue, e.exit)),
                    ),
                ],
            );
            self.pending
                .push((e.instance.0, "DialogueEnded".into(), value));
        }
        if let Some(end) = b.end {
            let who = self.core.people.name_of(npc);
            self.say(format!("end {who}: {}", end_name(end)));
        }
    }

    // ---- scenes -------------------------------------------------------------------------------------

    fn start_scene(&mut self, i: u16, scene: &str, cast: Vec<WireValue>) {
        let found = self
            .core
            .program
            .dialogues
            .dialogues
            .iter()
            .position(|d| d.name == scene && d.kind == DialogueKind::Scene)
            .and_then(|k| u16::try_from(k).ok());
        let Some(k) = found else {
            return self.say(format!("play: no scene {scene}"));
        };
        let now = self.core.now;
        let play = ScenePlay::start(&mut self.core, InstanceId(u32::from(i)), k, cast, now);
        self.scenes.push(play);
    }

    /// Who is in a conversation or a scene.
    fn busy(&self, id: i64) -> bool {
        self.talks.contains_key(&id)
            || self
                .scenes
                .iter()
                .any(|s| !s.ended() && s.cast().iter().any(|c| id_of(Some(c)) == Some(id)))
    }

    fn ambient(&mut self, npcs: &[i64]) {
        if self.outcome != Outcome::Running {
            return;
        }
        let idle: Vec<WireValue> = npcs
            .iter()
            .filter(|id| !self.busy(**id))
            .filter_map(|id| self.core.people.handle(*id))
            .collect();
        let stage = self.stage();
        let candidates: Vec<Attachment> = self
            .core
            .program
            .dialogues
            .dialogues
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.kind == DialogueKind::Scene && d.stage.is_none_or(|s| Some(s) == stage)
            })
            .filter_map(|(k, _)| {
                Some(Attachment {
                    instance: InstanceId(0),
                    dialogue: u16::try_from(k).ok()?,
                })
            })
            .collect();
        let now = self.core.now;
        let Some((att, cast)) = cast_ambient(&mut self.core, &candidates, &idle, &self.played, now)
        else {
            return;
        };
        let name = self
            .core
            .program
            .dialogues
            .dialogues
            .get(usize::from(att.dialogue))
            .map(|d| d.name.clone())
            .unwrap_or_default();
        self.played.insert((Some(self.core.program_key), name), now);
        let play = ScenePlay::start(&mut self.core, att.instance, att.dialogue, cast, now);
        self.scenes.push(play);
    }

    fn tick_scenes(&mut self) {
        let now = self.core.now;
        let mut outs = Vec::new();
        for s in &mut self.scenes {
            if s.ended() {
                continue;
            }
            let lead = s.cast().first().and_then(|c| id_of(Some(c))).unwrap_or(0);
            let rec = self.records.entry(lead).or_default();
            outs.push(s.tick(&mut self.core, rec, now));
        }
        for out in outs {
            self.scene_out(out);
        }
        self.scenes.retain(|s| !s.ended());
    }

    /// Prints a scene's lines and sends what it raised.
    fn scene_out(&mut self, out: SceneOut) {
        for line in &out.lines {
            let said = format!("{}: {}", self.speaker(&line.speaker), self.text(line));
            self.say(said);
        }
        for e in out.emits {
            self.send(e.instance.0, e.event, e.value);
        }
        if let Some(end) = out.ended {
            let value = unit_struct(
                "SceneEnded",
                vec![
                    ("scene", WireValue::Str(end.name.clone())),
                    ("reason", bare(scene_reason_name(end.reason))),
                ],
            );
            self.pending
                .push((end.instance.0, "SceneEnded".into(), value));
        }
    }

    // ---- mirrors ------------------------------------------------------------------------------------

    fn collect_mirrors(&mut self) {
        let program = Arc::clone(&self.core.program);
        let Some(q) = program.quest.as_ref() else {
            return;
        };
        let Some(home) = self.core.insts.first() else {
            return;
        };
        // A segment in progress has not committed what the mirror would show.
        if home.runner.vm.in_segment() {
            return;
        }
        for (k, part) in q.parts.iter().enumerate() {
            let Ok(id) = u16::try_from(k + 1) else { break };
            let m = home.runner.vm.mirror(&part.read_set);
            let h = hash_of(&m);
            if self.sent.get(&id) != Some(&h) {
                self.sent.insert(id, h);
                self.pending_mirrors.insert(id, m);
            }
        }
    }

    fn install_mirrors(&mut self) {
        for (id, m) in std::mem::take(&mut self.pending_mirrors) {
            let Some(inst) = self.core.insts.get_mut(usize::from(id)) else {
                continue;
            };
            match inst.runner.vm.install_mirror(&m) {
                Ok(()) => self.report.mirrors.push(id),
                Err(LoadError::Busy) => {
                    self.pending_mirrors.insert(id, m);
                }
                Err(e) => {
                    let line = format!("the {} refused the home's mirror: {e:?}", inst.name);
                    self.say(line);
                }
            }
        }
    }

    // ---- state --------------------------------------------------------------------------------------

    /// Nothing will happen without an input: every instance idle (or failed) with no request, timer or
    /// emit outstanding, nothing routed or mirrored for the next tick, no `Setup` due, no scene playing,
    /// and every conversation closed or waiting for a pick with no timeout.
    pub fn quiescent(&self) -> bool {
        let insts = self.outcome != Outcome::Running
            || self.core.insts.iter().all(|x| {
                let vm = &x.runner.vm;
                (vm.is_idle() || vm.status() == Status::Failed)
                    && !x.runner.host.has_pending()
                    && x.runner.host.emitted.is_empty()
                    && vm.timers().is_empty()
            });
        insts
            && self.pending.is_empty()
            && self.pending_mirrors.is_empty()
            && (self.outcome != Outcome::Running || self.setup_at.is_none())
            && self.scenes.is_empty()
            && self.talks.values().all(|tk| conv_quiet(&tk.conv))
    }

    /// A hash of everything the world holds: each instance's VM and host state, the people, the namespace,
    /// the conversations and records, the scenes, the attachments, the messages for the next tick.
    pub fn hash64(&self) -> u64 {
        let mut h = Fnv64::default();
        self.core.now.state_hash(&mut h);
        self.core.program_key.state_hash(&mut h);
        let outcome: u8 = match self.outcome {
            Outcome::Running => 0,
            Outcome::Completed => 1,
            Outcome::Failed => 2,
            Outcome::Declined => 3,
            Outcome::Reaped => 4,
        };
        outcome.state_hash(&mut h);
        self.setup_at.state_hash(&mut h);
        self.completions.state_hash(&mut h);
        self.core.completed_by.state_hash(&mut h);
        self.core.people.state_hash(&mut h);
        self.namespace.hash_state(&mut h);
        for inst in &self.core.insts {
            inst.runner.vm.state_hash(&mut h);
            inst.runner.host.hash_state(&mut h);
        }
        (self.talks.len() as u64).state_hash(&mut h);
        for (npc, tk) in &self.talks {
            npc.state_hash(&mut h);
            tk.player.state_hash(&mut h);
            tk.conv.state_hash(&mut h);
        }
        self.records.state_hash(&mut h);
        self.scenes.state_hash(&mut h);
        self.played.state_hash(&mut h);
        self.attached.state_hash(&mut h);
        self.detached.state_hash(&mut h);
        self.pending.state_hash(&mut h);
        self.pending_mirrors.state_hash(&mut h);
        self.sent.state_hash(&mut h);
        h.finish()
    }
}
