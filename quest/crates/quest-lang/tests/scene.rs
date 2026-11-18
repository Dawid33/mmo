//! Scenes and barks (plan Task 46; spec §5.11 "Scenes", "Pacing"): `quest_vm::scene_run` driven over compiled
//! fixtures, with an env evaluating through each instance's live `Vm`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use common::tree;
use quest_stub::{Runner, StubConfig, stub_api};
use quest_vm::consts::{COND_FUEL, MEM_BYTES_DEFAULT};
use quest_vm::dialogue::{DialogueGraph, DialogueKind, line_ticks};
use quest_vm::dialogue_run::{Attachment, DialogueEnv, EventShape, InstanceId, Records};
use quest_vm::host::{Host, PromiseId, QueryError, Report};
use quest_vm::scene_run::{SceneEnd, SceneOut, ScenePlay, SceneReason, cast_ambient};
use quest_vm::types::TypeDesc;
use quest_vm::wire::WireValue;
use quest_vm::{HostApi, Program, PureFault};

const FUEL: u64 = 2_000;

fn compiled(files: &[(&str, &str)]) -> Arc<Program> {
    let t = tree(files);
    match quest_lang::compile(&t, &stub_api()) {
        Ok(c) => Arc::new(c.program),
        Err(diags) => {
            let text: Vec<String> = diags.iter().map(|d| quest_lang::render(d, &t)).collect();
            panic!("compile failed:\n{}", text.join("\n"));
        }
    }
}

fn int(n: i64) -> WireValue {
    WireValue::Int(n)
}

fn person(id: i64) -> WireValue {
    WireValue::Struct {
        name: "Person".into(),
        fields: vec![("id".into(), int(id))],
    }
}

fn id_of(v: Option<&WireValue>) -> Option<i64> {
    match v {
        Some(WireValue::Struct { fields, .. }) => fields.iter().find_map(|(n, v)| match v {
            WireValue::Int(i) if n == "id" => Some(*i),
            _ => None,
        }),
        _ => None,
    }
}

/// The world the pure fns query: tags by entity id, the act, the completed quests.
#[derive(Clone, Default)]
struct World {
    tags: BTreeSet<(i64, String)>,
    act: i64,
    completed: BTreeSet<String>,
}

struct EvalHost<'a> {
    api: &'a HostApi,
    world: &'a World,
    depth: u32,
}

impl Host for EvalHost<'_> {
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, QueryError> {
        let Some(hf) = self.api.fns.get(usize::from(f)) else {
            return Err(QueryError::Failed("no such fn".into()));
        };
        let text = |k: usize| match args.get(k) {
            Some(WireValue::Str(s)) => Some(s.clone()),
            _ => None,
        };
        match hf.name.as_str() {
            "dialogue_depth" => Ok(int(i64::from(self.depth))),
            "act" => Ok(int(self.world.act)),
            "completed" => {
                let def = text(0).ok_or(QueryError::Failed("bad completed".into()))?;
                Ok(WireValue::Bool(self.world.completed.contains(&def)))
            }
            "has_tag" => {
                let (id, tag) = id_of(args.first())
                    .zip(text(1))
                    .ok_or(QueryError::Failed("bad has_tag".into()))?;
                Ok(WireValue::Bool(self.world.tags.contains(&(id, tag))))
            }
            other => Err(QueryError::Failed(format!("{other} is not answered here"))),
        }
    }
    fn check(&mut self, _: u16, _: &[WireValue]) -> Result<(), QueryError> {
        Err(QueryError::Failed("no checks".into()))
    }
    fn command(&mut self, _: u16, _: &[WireValue]) {}
    fn request(&mut self, _: PromiseId, _: u16, _: &[WireValue]) {}
    fn print(&mut self, _: &str) {}
    fn report(&mut self, _: Report) {}
    fn route(&mut self, _: &str, _: &WireValue) {}
}

struct Inst {
    prog: Arc<Program>,
    runner: Runner,
}

struct Env {
    api: HostApi,
    insts: BTreeMap<u32, Inst>,
    world: World,
    reports: Vec<(InstanceId, PureFault)>,
}

impl Env {
    fn new() -> Env {
        Env {
            api: stub_api(),
            insts: BTreeMap::new(),
            world: World::default(),
            reports: Vec::new(),
        }
    }

    fn add(&mut self, i: u32, prog: &Arc<Program>) {
        let mut runner =
            Runner::new(Arc::clone(prog), MEM_BYTES_DEFAULT, StubConfig::default()).expect("loads");
        for _ in 0..1_000 {
            if runner.quiescent() {
                break;
            }
            runner.step(FUEL);
        }
        self.insts.insert(
            i,
            Inst {
                prog: Arc::clone(prog),
                runner,
            },
        );
    }

    /// The index of instance `i`'s dialogue `name`.
    fn index(&self, i: u32, name: &str) -> u16 {
        self.insts[&i]
            .prog
            .dialogues
            .dialogues
            .iter()
            .position(|d| d.name == name)
            .and_then(|k| u16::try_from(k).ok())
            .unwrap_or_else(|| panic!("no dialogue {name}"))
    }

    fn att(&self, i: u32, name: &str) -> Attachment {
        Attachment {
            instance: InstanceId(i),
            dialogue: self.index(i, name),
        }
    }

    fn play(&mut self, i: u32, name: &str, cast: Vec<WireValue>, now: u64) -> ScenePlay {
        let scene = self.index(i, name);
        ScenePlay::start(self, InstanceId(i), scene, cast, now)
    }
}

impl DialogueEnv for Env {
    fn graph(&self, i: InstanceId) -> Option<&DialogueGraph> {
        self.insts.get(&i.0).map(|x| &x.prog.dialogues)
    }

    fn eval(
        &mut self,
        i: InstanceId,
        f: u16,
        args: &[WireValue],
        depth: u32,
    ) -> Result<WireValue, PureFault> {
        let Some(inst) = self.insts.get_mut(&i.0) else {
            return Err(PureFault::BadCall);
        };
        let mut host = EvalHost {
            api: &self.api,
            world: &self.world,
            depth,
        };
        inst.runner.vm.eval_pure(f, args, COND_FUEL, &mut host)
    }

    fn report(&mut self, i: InstanceId, fault: PureFault) {
        self.reports.push((i, fault));
    }

    fn program_key(&self, i: InstanceId) -> Option<u64> {
        self.insts.get(&i.0).map(|x| x.prog.content_hash())
    }

    fn event(&self, i: InstanceId, event: u16, script: bool) -> Option<EventShape> {
        if !script {
            let e = self.api.events.get(usize::from(event))?;
            return Some(EventShape {
                name: e.name.clone(),
                fields: e.fields.iter().map(|f| f.0.clone()).collect(),
            });
        }
        let p = &self.insts.get(&i.0)?.prog;
        let e = p.events.get(usize::from(event))?;
        match p.types.desc(e.ty).ok()? {
            TypeDesc::Struct { fields, .. } => Some(EventShape {
                name: e.name.clone(),
                fields: fields.iter().map(|f| f.0.clone()).collect(),
            }),
            _ => None,
        }
    }
}

/// What a play did, tick by tick: `(tick, line id, speaker id)` and `(tick, scene, reason)`.
#[derive(Debug, Default, PartialEq)]
struct Log {
    lines: Vec<(u64, String, Option<i64>)>,
    ends: Vec<(u64, String, SceneReason)>,
}

impl Log {
    fn take(&mut self, now: u64, out: SceneOut) {
        for l in out.lines {
            self.lines.push((now, l.id, id_of(Some(&l.speaker))));
        }
        if let Some(SceneEnd { name, reason, .. }) = out.ended {
            self.ends.push((now, name, reason));
        }
    }

    fn ids(&self) -> Vec<&str> {
        self.lines.iter().map(|l| l.1.as_str()).collect()
    }
}

/// Ticks `play` every tick from `from` until it ends (at most `max` ticks).
fn run(env: &mut Env, rec: &mut Records, play: &mut ScenePlay, from: u64, max: u64) -> Log {
    let mut log = Log::default();
    for now in from..from + max {
        let out = play.tick(env, rec, now);
        log.take(now, out);
        if play.ended() {
            return log;
        }
    }
    panic!("the scene did not end: {log:?}");
}

/// The duration the compiler gives a line of this text.
fn ticks_of(text: &str) -> u64 {
    u64::from(line_ticks(u32::try_from(text.chars().count()).unwrap()))
}

/// The Sealed Mine's `argument` (quest-examples §2), at the top level.
const ARGUMENT: &str = r#"scene argument(a: Person, b: Person) uninterruptible:
    a: "The seal held for forty years. You will not break it for silver."    @mine.arg.one
    b: "Silver feeds this village, old man."                                 @mine.arg.two
    pause 30
    a: "Silver did not feed the eleven we lost."                              @mine.arg.three
    b: "..."                                                                  @mine.arg.four (ticks=60)
    a: "Then let the stranger decide what they find down there."              @mine.arg.five
"#;

const L1: &str = "The seal held for forty years. You will not break it for silver.";
const L2: &str = "Silver feeds this village, old man.";
const L3: &str = "Silver did not feed the eleven we lost.";
const L5: &str = "Then let the stranger decide what they find down there.";

#[test]
fn argument_timeline() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", ARGUMENT)]));
    let mut rec = Records::default();
    let mut play = env.play(1, "argument", vec![person(1), person(2)], 100);
    assert!(!play.ended());
    let log = run(&mut env, &mut rec, &mut play, 100, 2_000);

    let t1 = 100;
    let t2 = t1 + ticks_of(L1);
    let t3 = t2 + ticks_of(L2) + 30;
    let t4 = t3 + ticks_of(L3);
    let t5 = t4 + 60;
    let end = t5 + ticks_of(L5);
    let line = |t: u64, id: &str, who: i64| (t, id.to_string(), Some(who));
    assert_eq!(
        log.lines,
        [
            line(t1, "mine.arg.one", 1),
            line(t2, "mine.arg.two", 2),
            line(t3, "mine.arg.three", 1),
            line(t4, "mine.arg.four", 2),
            line(t5, "mine.arg.five", 1),
        ]
    );
    assert_eq!(
        log.ends,
        [(end, "argument".to_string(), SceneReason::Finished)]
    );
    assert!(env.reports.is_empty(), "{:?}", env.reports);

    // Ticked late, the play catches up: the same lines, in order, the end at its own tick.
    let mut late = env.play(1, "argument", vec![person(1), person(2)], 100);
    let out = late.tick(&mut env, &mut rec, end + 500);
    assert_eq!(out.lines.len(), 5);
    assert_eq!(out.ended.map(|e| e.reason), Some(SceneReason::Finished));
    assert!(late.ended());
}

/// Town life's market banter (quest-examples §5), its follow-up, and the crier.
const TOWN: &str = r#"scene market_haggle(a: Person, b: Person) where a.has_tag("merchant") and b.has_tag("merchant") and a != b:
    b: "How much for the turnips?"                                @town.haggle.one
    a: "Two coppers."                                             @town.haggle.two
    b: "Two? They're half rot."                                   @town.haggle.three
    pause 20
    a: "Then it's one copper of turnip and one of rot."           @town.haggle.four (ticks=90)
    -> market_haggle_after

scene market_haggle_after(a: Person, b: Person):
    pause 40
    b: "...Fine. Two."                                            @town.after.one

scene gossip(a: Person, b: Person) where a.has_tag("merchant") and b.has_tag("merchant") and a != b:
    a: "Did you hear about the mill?"                             @town.gossip.one

scene crier(npc: Person) where npc.has_tag("town_crier"):
    => npc: "Hear ye, hear ye!"                                   @town.crier.hear_ye
    => npc: "The hollow road is closed by order of the reeve!"    @town.crier.road when act() == 1
    => npc: "The old mine is sealed again! Praise the stranger!"  @town.crier.mine when completed("errantry/act-two::SealedMine")
"#;

fn town() -> Env {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", TOWN)]));
    env
}

#[test]
fn follow_up_keeps_cast() {
    let mut env = town();
    let mut rec = Records::default();
    let mut play = env.play(1, "market_haggle", vec![person(1), person(2)], 0);
    let log = run(&mut env, &mut rec, &mut play, 0, 2_000);
    assert_eq!(
        log.ids(),
        [
            "town.haggle.one",
            "town.haggle.two",
            "town.haggle.three",
            "town.haggle.four",
            "town.after.one"
        ]
    );
    let speakers: Vec<Option<i64>> = log.lines.iter().map(|l| l.2).collect();
    assert_eq!(speakers, [Some(2), Some(1), Some(2), Some(1), Some(2)]);
    // The first scene ends when its last line has been held; the follow-up opens with its pause.
    let t4 = log.lines[3].0;
    let haggle_end = t4 + 90;
    let names: Vec<(&str, SceneReason)> = log.ends.iter().map(|e| (e.1.as_str(), e.2)).collect();
    assert_eq!(
        names,
        [
            ("market_haggle", SceneReason::Finished),
            ("market_haggle_after", SceneReason::Finished)
        ]
    );
    assert_eq!(log.ends[0].0, haggle_end);
    assert_eq!(log.lines[4].0, haggle_end + 40);
    assert_eq!(play.scene(), env.index(1, "market_haggle_after"));
    assert_eq!(play.cast(), [person(1), person(2)]);
}

#[test]
fn interrupt_ends_interruptible() {
    let mut env = town();
    let mut rec = Records::default();
    let mut play = env.play(1, "market_haggle", vec![person(1), person(2)], 0);
    assert!(play.interruptible());
    let out = play.tick(&mut env, &mut rec, 0);
    assert_eq!(out.lines.len(), 1);
    let out = play.interrupt().expect("interruptible");
    let end = out.ended.expect("ended");
    assert_eq!(
        (end.name.as_str(), end.reason),
        ("market_haggle", SceneReason::Interrupted)
    );
    assert!(play.ended());
    assert_eq!(play.tick(&mut env, &mut rec, 10_000), SceneOut::default());
    assert_eq!(play.interrupt(), Some(SceneOut::default()), "only once");
}

#[test]
fn uninterruptible_refuses_interrupt() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", ARGUMENT)]));
    let mut rec = Records::default();
    let mut play = env.play(1, "argument", vec![person(1), person(2)], 0);
    assert!(!play.interruptible());
    let out = play.tick(&mut env, &mut rec, 0);
    assert_eq!(out.lines.len(), 1);
    assert_eq!(play.interrupt(), None);
    assert!(!play.ended());
    let log = run(&mut env, &mut rec, &mut play, 1, 2_000);
    assert_eq!(log.lines.len(), 4, "the argument goes on to its end");
    assert_eq!(log.ends[0].2, SceneReason::Finished);
}

#[test]
fn actor_lost_ends() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", ARGUMENT)]));
    let mut rec = Records::default();
    let mut play = env.play(1, "argument", vec![person(1), person(2)], 0);
    play.tick(&mut env, &mut rec, 0);
    let out = play.actor_lost();
    let end = out.ended.expect("ended");
    assert_eq!(
        (end.name.as_str(), end.reason, end.instance),
        ("argument", SceneReason::ActorLost, InstanceId(1))
    );
    assert!(play.ended());
    assert_eq!(play.tick(&mut env, &mut rec, 10_000), SceneOut::default());
    assert_eq!(play.actor_lost(), SceneOut::default(), "only once");

    // A cast shorter than the scene's is an actor lost from the start.
    let mut short = env.play(1, "argument", vec![person(1)], 0);
    let out = short.tick(&mut env, &mut rec, 0);
    assert!(out.lines.is_empty());
    assert_eq!(out.ended.map(|e| e.reason), Some(SceneReason::ActorLost));

    // A play of no scene ends at its first beat, saying nothing.
    let mut none = ScenePlay::start(&mut env, InstanceId(1), u16::MAX, vec![person(1)], 0);
    let out = none.tick(&mut env, &mut rec, 0);
    assert!(out.lines.is_empty() && none.ended());
    assert_eq!(out.ended.map(|e| e.reason), Some(SceneReason::Finished));
}

const BARK: &str = r#"dialogue crier_bark(npc: Person):
    => npc: "Hear ye, hear ye!"                     @crier.bark.hear_ye
    => npc: "The king has a new advisor!"           @crier.bark.advisor when act() == 2
"#;

#[test]
fn bark_is_one_actor_scene() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", BARK)]));
    let d = &env.insts[&1].prog.dialogues.dialogues[usize::from(env.index(1, "crier_bark"))];
    assert_eq!((d.kind, d.cast), (DialogueKind::Scene, 1));
    let mut rec = Records::default();
    // `bark(npc, crier_bark)` from a handler = a play with one actor.
    env.world.act = 2;
    let mut play = env.play(1, "crier_bark", vec![person(9)], 5);
    let log = run(&mut env, &mut rec, &mut play, 5, 1_000);
    assert_eq!(log.lines, [(5, "crier.bark.advisor".to_string(), Some(9))]);
    let held = ticks_of("The king has a new advisor!");
    assert_eq!(
        log.ends,
        [(5 + held, "crier_bark".to_string(), SceneReason::Finished)]
    );
}

#[test]
fn crier_group_by_act() {
    let mut env = town();
    let mut rec = Records::default();
    let cry = |env: &mut Env, rec: &mut Records| {
        let mut play = env.play(1, "crier", vec![person(3)], 0);
        run(env, rec, &mut play, 0, 1_000).ids()[0].to_string()
    };
    env.world.act = 1;
    // The act-1 line (score 1) wins over "Hear ye" (score 0), every time it holds.
    assert_eq!(cry(&mut env, &mut rec), "town.crier.road");
    assert_eq!(cry(&mut env, &mut rec), "town.crier.road");
    env.world.act = 2;
    assert_eq!(cry(&mut env, &mut rec), "town.crier.hear_ye");
    env.world
        .completed
        .insert("errantry/act-two::SealedMine".into());
    assert_eq!(cry(&mut env, &mut rec), "town.crier.mine");
    assert!(env.reports.is_empty(), "{:?}", env.reports);
}

#[test]
fn cast_ambient_deterministic() {
    let mut env = town();
    let prog = Arc::clone(&env.insts[&1].prog);
    env.add(2, &prog);
    env.world.tags.insert((1, "merchant".into()));
    env.world.tags.insert((2, "merchant".into()));
    env.world.tags.insert((3, "town_crier".into()));
    let mut cands = Vec::new();
    for i in [1, 2] {
        for name in ["market_haggle", "market_haggle_after", "gossip", "crier"] {
            cands.push(env.att(i, name));
        }
    }
    let idle = [person(3), person(1), person(2)];

    // Every rotation and the reverse of the candidates: the same pick.
    let pick_all = |env: &mut Env, played: &BTreeMap<String, u64>| {
        let first = cast_ambient(env, &cands, &idle, played, 1_000);
        for k in 0..cands.len() {
            let mut c = cands.clone();
            c.rotate_left(k);
            assert_eq!(cast_ambient(env, &c, &idle, played, 1_000), first);
            c.reverse();
            assert_eq!(cast_ambient(env, &c, &idle, played, 1_000), first);
        }
        first.expect("a pick")
    };

    // `gossip` and `market_haggle` score 3, the crier 1, the unconditional follow-up 0: by the stable key
    // (qualified name, then program, then instance), `gossip` of instance 1, cast from `idle` in order.
    let mut played = BTreeMap::new();
    let (att, cast) = pick_all(&mut env, &played);
    assert_eq!(att, env.att(1, "gossip"));
    assert_eq!(cast, [person(1), person(2)]);

    // Least recently played next: `gossip` was just played.
    played.insert("gossip".to_string(), 900);
    let (att, cast) = pick_all(&mut env, &played);
    assert_eq!(att, env.att(1, "market_haggle"));
    assert_eq!(cast, [person(1), person(2)]);
    played.insert("market_haggle".to_string(), 950);
    assert_eq!(pick_all(&mut env, &played).0, env.att(1, "gossip"));

    // With no merchants idle, the crier.
    let crier_only = [person(3)];
    let got = cast_ambient(&mut env, &cands, &crier_only, &played, 1_000);
    assert_eq!(got, Some((env.att(1, "crier"), vec![person(3)])));
    // With nobody idle, nothing; an attachment that names nothing is ignored.
    assert_eq!(cast_ambient(&mut env, &cands, &[], &played, 1_000), None);
    let junk = [
        Attachment {
            instance: InstanceId(9),
            dialogue: 0,
        },
        Attachment {
            instance: InstanceId(1),
            dialogue: u16::MAX,
        },
    ];
    assert_eq!(cast_ambient(&mut env, &junk, &idle, &played, 1_000), None);
    assert!(env.reports.is_empty(), "{:?}", env.reports);
}
