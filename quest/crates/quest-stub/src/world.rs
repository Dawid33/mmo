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
//! Conditions, substitutions, `for` targets, filters and the stage the attached set depends on are evaluated
//! over each instance's **dialogue mirror** (its dialogue read set, `QuestInfo::read_set`, and its stage; every
//! global for a program without a quest), taken after the instances ran each tick from what they committed (an
//! instance paused mid-segment keeps its last one), with `Vm::eval_pure` on a scratch instance holding it.
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
//! ([`Outcome::Completed`]); a repeatable one ends its conversations (`Left`) and scenes, drops its
//! attachments, resets every instance (`Vm::reset`), is reaped, has its roles checked again (a failing
//! `where` reaps it for good, [`Outcome::Reaped`]) and is given `Setup` after its `cooldown`. `fail()` ends it ([`Outcome::Failed`]), `decline()` refuses it ([`Outcome::Declined`]); an
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

mod env;
mod inputs;
mod scenes;
mod talk;

use env::Core;
use talk::{Talk, conv_over, conv_quiet};

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

pub struct World {
    core: Core,
    /// The line texts by id (`quest_lang::Compiled::lines`): `{n}` holes.
    lines: BTreeMap<String, String>,
    show_ids: bool,
    fuel: u64,
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
                roles,
                mirrors: BTreeMap::new(),
            },
            lines: BTreeMap::new(),
            show_ids: false,
            fuel,
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
                w.core.roles.clone(),
            )
            .map_err(load_error)?;
            w.core.insts.push(Inst { runner, name });
        }
        for inst in &mut w.core.insts {
            inst.runner.host.in_world = true;
        }
        // The placed state is what a talk before the first commit reads (a `Setup` running over several
        // ticks takes no mirror until it ends).
        w.core.take_mirrors();
        Ok(w)
    }

    /// The `requires` conditions, then each bound role's `where`, over the home as it is.
    fn check_placement(&mut self) -> Result<(), String> {
        let program = Arc::clone(&self.core.program);
        let Some(q) = program.quest.as_ref() else {
            return Ok(());
        };
        for (k, &f) in q.requires.iter().enumerate() {
            match self.core.eval_live(0, f, &[], 1) {
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
                (r.cond, self.core.roles.iter().find(|(n, _)| *n == r.name))
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
                let given = self.core.roles.iter().find(|(n, _)| *n == earlier.name);
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
            match self.core.eval_live(0, f, &args, 1) {
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

    /// `player`'s conversation: the one not over, else one ended and not yet closed.
    pub fn conversation(&self, player: i64) -> Option<&Conversation> {
        let mine = || self.talks.values().filter(|tk| tk.player == player);
        mine()
            .find(|tk| !conv_over(&tk.conv))
            .or_else(|| mine().next())
            .map(|tk| &tk.conv)
    }

    /// Every instance's VM replaced, in id order (a loaded or migrated state; each must be made with
    /// [`crate::vm_config`] of its instance and the world's roles). `setup`: `Setup` is still delivered at the
    /// next tick (a reset state); otherwise it is not (the state already had it). The dialogue mirrors are
    /// taken again from the new VMs.
    pub fn load_vms(&mut self, vms: Vec<Vm>, setup: bool) -> Result<(), String> {
        if vms.len() != self.core.insts.len() {
            return Err(format!(
                "the state holds {} instances, the program has {}",
                vms.len(),
                self.core.insts.len()
            ));
        }
        for (inst, vm) in self.core.insts.iter_mut().zip(vms) {
            inst.runner.vm = vm;
        }
        if !setup {
            self.setup_at = None;
        }
        self.sent.clear();
        self.core.mirrors.clear();
        self.core.take_mirrors();
        Ok(())
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
        // What the conversations and scenes read: each instance's committed dialogue read set.
        self.core.take_mirrors();
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
            match self.owner_of(&event, Some(i16)) {
                Some(to) => self.pending.push((u32::from(to), event, value)),
                // The VM routes only what another instance of the program handles; one this world
                // does not have is reported, not lost silently.
                None => self.report.reports.push((
                    i16,
                    Report::EventDropped {
                        event,
                        reason: DropReason::NoHandler,
                    },
                )),
            }
        }
        self.cmds
            .extend(cmds.into_iter().map(|(f, args)| (i16, f, args)));
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
        // The run's conversations end (`Left`) and its scenes stop (no `SceneEnded`: the instance it would
        // reach is reset), and what its handlers attached or detached is dropped. Every conversation and scene
        // of this world is the quest's: there is only its program.
        let now = self.core.now;
        let npcs: Vec<i64> = self.talks.keys().copied().collect();
        for npc in npcs {
            if let Some(tk) = self.talks.get_mut(&npc) {
                let b = tk.conv.leave(now);
                self.print_batch(npc, b);
            }
        }
        for play in std::mem::take(&mut self.scenes) {
            let name = program
                .dialogues
                .dialogues
                .get(usize::from(play.scene()))
                .map_or("", |d| d.name.as_str());
            self.say(format!("end scene {name}: reset"));
        }
        self.attached.clear();
        self.detached.clear();
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
        (self.core.mirrors.len() as u64).state_hash(&mut h);
        for (i, d) in &self.core.mirrors {
            i.state_hash(&mut h);
            d.mirror.state_hash(&mut h);
        }
        h.finish()
    }
}
