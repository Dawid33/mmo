//! The conversation runner (plan Task 45; spec §5.11): `quest_vm::dialogue_run` driven over compiled
//! fixtures, with an env evaluating through each instance's live `Vm` (or a `Mirror` of it).

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;
use std::sync::Arc;

use common::tree;
use quest_stub::{Runner, StubConfig, stub_api};
use quest_vm::consts::{COND_FUEL, CONVERSATION_LINGER_TICKS, MEM_BYTES_DEFAULT};
use quest_vm::dialogue::{DialogueGraph, DialogueKind};
use quest_vm::dialogue_run::{
    Attachment, Batch, ConvState, Conversation, DialogueEnv, EndReason, EventShape, InstanceId,
    Offer, PickRefused, Records,
};
use quest_vm::host::{Host, PromiseId, QueryError, Report};
use quest_vm::types::TypeDesc;
use quest_vm::wire::WireValue;
use quest_vm::{FaultKind, Fnv64, HostApi, Mirror, Program, PureFault};
use state_hash::StateHash;

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

fn player(id: i64) -> WireValue {
    WireValue::Struct {
        name: "Player".into(),
        fields: vec![
            ("id".into(), int(id)),
            ("name".into(), WireValue::Str(format!("player {id}"))),
        ],
    }
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

fn hash<T: StateHash>(t: &T) -> u64 {
    let mut h = Fnv64::default();
    t.state_hash(&mut h);
    h.finish()
}

/// The world the pure fns query: tags and skills by player id, and the detour depth.
#[derive(Clone, Default)]
struct World {
    tags: BTreeSet<(i64, String)>,
    skills: BTreeMap<(i64, String), i64>,
}

/// The host a pure evaluation sees: queries only.
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
            "has_tag" => {
                let (id, tag) = id_of(args.first())
                    .zip(text(1))
                    .ok_or(QueryError::Failed("bad has_tag".into()))?;
                Ok(WireValue::Bool(self.world.tags.contains(&(id, tag))))
            }
            "skill" => {
                let (id, s) = id_of(args.first())
                    .zip(text(1))
                    .ok_or(QueryError::Failed("bad skill".into()))?;
                Ok(int(self.world.skills.get(&(id, s)).copied().unwrap_or(0)))
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
    /// Set: evaluate over this mirror instead of the live VM.
    mirror: Option<Mirror>,
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

    /// Instance `i` of `prog`, its initialisers run.
    fn add(&mut self, i: u32, prog: &Arc<Program>) {
        let mut runner =
            Runner::new(Arc::clone(prog), MEM_BYTES_DEFAULT, StubConfig::default()).expect("loads");
        settle(&mut runner);
        self.insts.insert(
            i,
            Inst {
                prog: Arc::clone(prog),
                runner,
                mirror: None,
            },
        );
    }

    fn inst(&mut self, i: u32) -> &mut Inst {
        self.insts.get_mut(&i).expect("an instance")
    }

    /// Attachments of instance `i`'s dialogues `names`.
    fn att(&self, i: u32, names: &[&str]) -> Vec<Attachment> {
        let g = &self.insts[&i].prog.dialogues;
        names
            .iter()
            .map(|n| Attachment {
                instance: InstanceId(i),
                dialogue: g
                    .dialogues
                    .iter()
                    .position(|d| d.name == *n)
                    .and_then(|k| u16::try_from(k).ok())
                    .unwrap_or_else(|| panic!("no dialogue {n}")),
            })
            .collect()
    }

    /// Every conversation of instance `i`.
    fn all(&self, i: u32) -> Vec<Attachment> {
        let g = &self.insts[&i].prog.dialogues;
        let names: Vec<&str> = g
            .dialogues
            .iter()
            .filter(|d| d.kind == DialogueKind::Conversation)
            .map(|d| d.name.as_str())
            .collect();
        self.att(i, &names)
    }

    /// Delivers a batch's emits to their instances and runs their handlers.
    fn deliver(&mut self, b: &Batch) {
        for e in &b.emits {
            let r = &mut self.inst(e.instance.0).runner;
            r.vm.deliver(&e.event, &e.value);
            settle(r);
        }
    }

    /// Instance `i` evaluates over a mirror of all its globals from now on.
    fn mirror(&mut self, i: u32) {
        let inst = self.inst(i);
        let all: Vec<u16> = (0..inst.prog.globals.len())
            .map(|g| u16::try_from(g).unwrap())
            .collect();
        inst.mirror = Some(inst.runner.vm.mirror(&all));
    }
}

fn settle(r: &mut Runner) {
    for _ in 0..1_000 {
        if r.quiescent() {
            return;
        }
        r.step(FUEL);
    }
    panic!("the instance did not settle");
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
        match &inst.mirror {
            Some(m) => m.eval_pure(&inst.prog, f, args, COND_FUEL, &mut host),
            None => inst.runner.vm.eval_pure(f, args, COND_FUEL, &mut host),
        }
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

/// The ids of a batch's lines.
fn said(b: &Batch) -> Vec<&str> {
    b.lines.iter().map(|l| l.id.as_str()).collect()
}

fn options(o: &Offer) -> Vec<&str> {
    o.options.iter().map(|l| l.id.as_str()).collect()
}

fn offer(b: &Batch) -> &Offer {
    b.offer.as_ref().expect("an offer")
}

fn ends(b: &Batch) -> Vec<(EndReason, &str, &str)> {
    b.ended
        .iter()
        .map(|e| (e.reason, e.dialogue.as_str(), e.exit.as_str()))
        .collect()
}

/// The batch's one end.
fn ended(b: &Batch) -> (EndReason, &str, &str) {
    match ends(b).as_slice() {
        [one] => *one,
        other => panic!("not one end: {other:?}"),
    }
}

/// Spec §5.11's farmer, and the dialogue it detours into.
const FARMER: &str = r#"let accepted = false
let dog_name = "Rufus"
let farmer = Person(id=1)

event Accepted:
    player: Player

@pick(best)
dialogue farmer_talk(p: Player, npc: Person) for farmer:
    node start:
        npc: "Have you seen my dog? He ran off toward the hills."     @farmer.start
        choice:
            "I'll find him." @farmer.accept if not accepted:
                emit Accepted(player=p)
                npc: "Bless you. Bring him back before dark."          @farmer.thanks
            "What does he look like?" @farmer.ask_dog:
                npc: f"Brown, one ear up. He answers to {dog_name}."   @farmer.dog_desc
                -> start
            once "Is that blood on your sleeve?" @farmer.blood:
                detour farmer_secret
            "Goodbye." @farmer.goodbye
        npc: "Safe roads."                                             @farmer.bye

dialogue farmer_secret(p: Player, npc: Person):
    node secret when dialogue_depth() < 3:
        npc: "It's nothing. Chickens."                                 @secret.chickens

@on(Accepted)
fn accept(e):
    accepted = true
    give(e.player, Item(kind="dog whistle", count=1))
"#;

fn farmer_env() -> (Env, Vec<Attachment>) {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", FARMER)]));
    let att = env.att(1, &["farmer_talk"]);
    (env, att)
}

const ALL4: [&str; 4] = [
    "farmer.accept",
    "farmer.ask_dog",
    "farmer.blood",
    "farmer.goodbye",
];

#[test]
fn farmer_transcript() {
    let (mut env, att) = farmer_env();
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(7), person(1), 100);
    assert_eq!(said(&b), ["farmer.start"]);
    assert_eq!(b.lines[0].speaker, person(1));
    assert!(!offer(&b).menu);
    assert_eq!(options(offer(&b)), ALL4);
    assert_eq!(c.state(), &ConvState::AwaitingPick);

    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 1, 101)
        .expect("picked");
    assert_eq!(said(&b), ["farmer.dog_desc", "farmer.start"]);
    assert_eq!(b.lines[0].args, [WireValue::Str("Rufus".into())]);
    assert_eq!(options(offer(&b)), ALL4, "the `once` option is still there");

    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 2, 102)
        .expect("picked");
    assert_eq!(said(&b), ["secret.chickens", "farmer.bye"]);
    assert_eq!(ended(&b), (EndReason::Finished, "farmer_talk", "done"));
    assert!(b.offer.is_none() && b.emits.is_empty());
    assert!(env.reports.is_empty(), "{:?}", env.reports);

    // The next conversation: the `once` option was taken.
    let (_, b) = Conversation::start(&mut env, &att, &mut rec, player(7), person(1), 500);
    assert_eq!(
        options(offer(&b)),
        ["farmer.accept", "farmer.ask_dog", "farmer.goodbye"]
    );
}

const INN: &str = r#"let innkeeper = Person(id=2)

dialogue greeting(p: Player, npc: Person) for innkeeper:
    node hello when p.has_tag("outlaw"):
        npc: "Keep walking."                                           @greet.outlaw
    node hello when once per p:
        npc: "A new face! Welcome."                                    @greet.first
    node hello:
        => npc: "Morning."                                             @greet.a
        => npc: "Weather's turning."                                   @greet.b
        => npc: "You again?"                                           @greet.c when once per p
"#;

#[test]
fn greeting_pool_by_score() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", INN)]));
    env.world.tags.insert((1, "outlaw".into()));
    let att = env.att(1, &["greeting"]);
    let mut rec = Records::default();
    let greet = |env: &mut Env, rec: &mut Records, p: i64| {
        let (c, b) = Conversation::start(env, &att, rec, player(p), person(2), 10);
        assert_eq!(ended(&b), (EndReason::Finished, "greeting", "done"));
        assert!(matches!(c.state(), ConvState::Ended { .. }));
        said(&b).iter().map(|s| s.to_string()).collect::<Vec<_>>()
    };
    assert_eq!(greet(&mut env, &mut rec, 1), ["greet.outlaw"]);
    assert_eq!(greet(&mut env, &mut rec, 2), ["greet.first"]);
    // Then the group: `c` scores 1 (once per player), then the least recent of the rest.
    assert_eq!(greet(&mut env, &mut rec, 2), ["greet.c"]);
    assert_eq!(greet(&mut env, &mut rec, 2), ["greet.a"]);
    assert_eq!(greet(&mut env, &mut rec, 2), ["greet.b"]);
    assert_eq!(greet(&mut env, &mut rec, 2), ["greet.a"]);
    // Another player is new here.
    assert_eq!(greet(&mut env, &mut rec, 3), ["greet.first"]);
}

const TOPICS: &str = r#"let flag = false

dialogue alpha(p: Player, npc: Person) topic "Alpha" @t.alpha:
    npc: "a" @t.a
dialogue beta(p: Player, npc: Person) topic "Beta" @t.beta priority 1:
    npc: "b" @t.b
dialogue gamma(p: Player, npc: Person) topic "Gamma" @t.gamma priority 1:
    npc: "g" @t.g
dialogue hidden(p: Player, npc: Person) topic "Hidden" @t.hidden if flag priority 5:
    npc: "h" @t.h
dialogue tagged(p: Player, npc: Person) topic "Tagged" @t.tagged if p.has_tag("met"):
    npc: "t" @t.t
"#;

#[test]
fn menu_sorted_by_priority_then_name() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", TOPICS)]));
    let mut att = env.all(1);
    att.reverse();
    let mut rec = Records::default();
    let (c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(3), 0);
    // No greeting: the menu at once.
    assert!(b.lines.is_empty());
    assert!(offer(&b).menu);
    assert_eq!(options(offer(&b)), ["t.beta", "t.gamma", "t.alpha"]);
    // A topic's text is the player's.
    assert_eq!(offer(&b).options[0].speaker, player(1));
    assert_eq!(c.state(), &ConvState::AwaitingPick);
}

#[test]
fn topic_returns_to_reevaluated_menu() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", TOPICS)]));
    let att = env.all(1);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(3), 0);
    let first = offer(&b).clone();
    let b = c
        .pick(&mut env, &att, &mut rec, first.serial, 2, 1)
        .expect("picked");
    assert_eq!(said(&b), ["t.a"]);
    assert_eq!(ended(&b), (EndReason::Finished, "alpha", "done"));
    assert_eq!(b.ended[0].instance, InstanceId(1));
    assert_eq!(options(offer(&b)), ["t.beta", "t.gamma", "t.alpha"]);
    assert!(offer(&b).serial != first.serial);
    // The world changed: the next return shows the topic it unlocks.
    env.world.tags.insert((1, "met".into()));
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 2)
        .expect("picked");
    assert_eq!(said(&b), ["t.b"]);
    assert_eq!(
        options(offer(&b)),
        ["t.beta", "t.gamma", "t.alpha", "t.tagged"]
    );
    // Leaving the menu ends the conversation; every run has already raised its end.
    let b = c.leave(3);
    assert!(b.ended.is_empty());
    assert!(matches!(c.state(), ConvState::Ended { .. }));
    assert!(c.leave(4) == Batch::default(), "nothing once ended");
}

#[test]
fn detour_cycle_ends() {
    let src = r#"dialogue ping(p: Player, npc: Person):
    npc: "ping" @c.ping
    detour pong
dialogue pong(p: Player, npc: Person):
    npc: "pong" @c.pong
    detour ping
"#;
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["ping"]);
    let (c, b) = Conversation::start(
        &mut env,
        &att,
        &mut Records::default(),
        player(1),
        person(1),
        0,
    );
    assert_eq!(b.lines.len(), quest_vm::consts::DIALOGUE_STACK_MAX);
    assert_eq!(ended(&b), (EndReason::TooDeep, "ping", "done"));
    assert!(matches!(c.state(), ConvState::Ended { .. }));
}

#[test]
fn self_detour_bounded_by_depth() {
    let src = r#"dialogue deep(p: Player, npc: Person):
    npc: "down" @deep.down
    when dialogue_depth() < 3:
        detour deep
    npc: "up" @deep.up
"#;
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["deep"]);
    let (_, b) = Conversation::start(
        &mut env,
        &att,
        &mut Records::default(),
        player(1),
        person(1),
        0,
    );
    assert_eq!(
        said(&b),
        [
            "deep.down",
            "deep.down",
            "deep.down",
            "deep.up",
            "deep.up",
            "deep.up"
        ]
    );
    assert_eq!(ended(&b), (EndReason::Finished, "deep", "done"));
}

#[test]
fn too_deep_ends() {
    let src = r#"dialogue again(p: Player, npc: Person):
    npc: "again" @again.line
    detour again
"#;
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["again"]);
    let (_, b) = Conversation::start(
        &mut env,
        &att,
        &mut Records::default(),
        player(1),
        person(1),
        0,
    );
    assert_eq!(b.lines.len(), quest_vm::consts::DIALOGUE_STACK_MAX);
    assert_eq!(ended(&b), (EndReason::TooDeep, "again", "done"));
    // A jump loop with no choice in it is bounded too.
    let src = "dialogue spin(p: Player, npc: Person):\n    node a:\n        -> a\n";
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["spin"]);
    let (_, b) = Conversation::start(
        &mut env,
        &att,
        &mut Records::default(),
        player(1),
        person(1),
        0,
    );
    assert_eq!(ended(&b), (EndReason::TooDeep, "spin", "done"));
}

const DEBT: &str = r#"dialogue haggle(p: Player, npc: Person) exits agreed, refused:
    npc: "Times are hard." @haggle.hard
    choice:
        "Half now, half at harvest?" @haggle.half if p.skill("trade") >= 2:
            return agreed
        "Then I'll be back." @haggle.back:
            return refused

dialogue confront(p: Player, npc: Person) exits paid, stalled:
    npc: "Oh. Them." @confront.oh
    detour haggle:
        agreed:
            npc: "Here. Tell them we're square." @confront.square
            return paid
        refused:
            npc: "Come back when you're less... you." @confront.later
            return stalled
    npc: "Unreachable." @confront.never
"#;

#[test]
fn exit_arms() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", DEBT)]));
    env.world.skills.insert((1, "trade".into()), 3);
    let att = env.att(1, &["confront"]);
    // A trader agrees.
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(4), 0);
    assert_eq!(said(&b), ["confront.oh", "haggle.hard"]);
    assert_eq!(options(offer(&b)), ["haggle.half", "haggle.back"]);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 1)
        .unwrap();
    assert_eq!(said(&b), ["confront.square"]);
    assert_eq!(ended(&b), (EndReason::Finished, "confront", "paid"));
    // Someone without the skill sees one option, and refuses.
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(2), person(4), 0);
    assert_eq!(options(offer(&b)), ["haggle.back"]);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 1)
        .unwrap();
    assert_eq!(said(&b), ["confront.later"]);
    assert_eq!(ended(&b), (EndReason::Finished, "confront", "stalled"));
    // Walking away mid-haggle: `Left`, the outermost dialogue's implicit exit.
    let (mut c, _) = Conversation::start(&mut env, &att, &mut rec, player(2), person(4), 0);
    let b = c.leave(2);
    assert_eq!(ended(&b), (EndReason::Left, "confront", "done"));
}

/// `confront` as a topic, the way quest-examples §4 attaches it.
fn debt_topic_env() -> (Env, Vec<Attachment>) {
    let src = DEBT.replace(
        "dialogue confront(p: Player, npc: Person) exits paid, stalled:",
        "dialogue confront(p: Player, npc: Person) topic \"About the debt\" @confront.topic exits paid, stalled:",
    );
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", &src)]));
    env.world.skills.insert((1, "trade".into()), 3);
    let att = env.att(1, &["confront"]);
    (env, att)
}

#[test]
fn topic_return_raises_dialogue_ended_with_its_exit() {
    let (mut env, att) = debt_topic_env();
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(4), 0);
    assert!(b.ended.is_empty());
    assert_eq!(options(offer(&b)), ["confront.topic"]);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 1)
        .unwrap();
    assert_eq!(said(&b), ["confront.oh", "haggle.hard"]);
    assert!(
        b.ended.is_empty(),
        "the detour into `haggle` is no top-level run"
    );
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 2)
        .unwrap();
    assert_eq!(said(&b), ["confront.square"]);
    // `confront` returned `paid` and the player is back at the menu.
    assert_eq!(ended(&b), (EndReason::Finished, "confront", "paid"));
    assert_eq!(b.ended[0].instance, InstanceId(1));
    assert!(offer(&b).menu);
    assert_eq!(c.state(), &ConvState::AwaitingPick);
    let b = c.leave(3);
    assert!(b.ended.is_empty(), "nothing further: {:?}", b.ended);
    assert!(matches!(c.state(), ConvState::Ended { .. }));
}

#[test]
fn leaving_mid_topic_ends_it_left_done() {
    let (mut env, att) = debt_topic_env();
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(4), 0);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 1)
        .unwrap();
    assert_eq!(options(offer(&b)), ["haggle.half", "haggle.back"]);
    let b = c.leave(2);
    assert_eq!(ended(&b), (EndReason::Left, "confront", "done"));
    assert!(matches!(c.state(), ConvState::Ended { .. }));
}

const TIMEOUTS: &str = r#"dialogue soft(p: Player, npc: Person):
    choice(timeout=10):
        "A" @soft.a
        default "B" @soft.b:
            npc: "b chosen" @soft.bb
dialogue hidden(p: Player, npc: Person):
    choice(timeout=10):
        "A" @hidden.a
        default:
            npc: "silence" @hidden.silence
    npc: "after" @hidden.after
dialogue hard(p: Player, npc: Person):
    choice(timeout=10):
        "A" @hard.a
"#;

fn timeout_run(name: &str) -> (Conversation, Batch, Offer) {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", TIMEOUTS)]));
    let att = env.att(1, &[name]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 100);
    let o = offer(&b).clone();
    assert_eq!(o.timeout_at, Some(110));
    assert_eq!(c.tick(&mut env, &mut rec, 109), None);
    assert_eq!(c.state(), &ConvState::AwaitingPick);
    let b = c.tick(&mut env, &mut rec, 110).expect("timed out");
    (c, b, o)
}

#[test]
fn timeout_picks_default() {
    let (_, b, o) = timeout_run("soft");
    assert_eq!(options(&o), ["soft.a", "soft.b"]);
    assert_eq!(said(&b), ["soft.bb"]);
    assert_eq!(ended(&b), (EndReason::Finished, "soft", "done"));
}

#[test]
fn timeout_runs_hidden_default() {
    let (_, b, o) = timeout_run("hidden");
    assert_eq!(options(&o), ["hidden.a"], "the fallback is never offered");
    assert_eq!(said(&b), ["hidden.silence", "hidden.after"]);
    assert_eq!(ended(&b), (EndReason::Finished, "hidden", "done"));
}

#[test]
fn timeout_without_default_ends() {
    let (c, b, _) = timeout_run("hard");
    assert!(b.lines.is_empty());
    assert_eq!(ended(&b), (EndReason::Timeout, "hard", "done"));
    assert!(matches!(c.state(), ConvState::Ended { .. }));
}

#[test]
fn pause_ends_batch() {
    let src = r#"dialogue slow(p: Player, npc: Person):
    npc: "one" @slow.one
    pause 5
    npc: "two" @slow.two
"#;
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["slow"]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 20);
    assert_eq!(said(&b), ["slow.one"]);
    assert!(b.ended.is_empty() && b.offer.is_none());
    assert_eq!(c.state(), &ConvState::Paused { until: 25 });
    assert_eq!(c.tick(&mut env, &mut rec, 24), None);
    let b = c.tick(&mut env, &mut rec, 25).expect("resumed");
    assert_eq!(said(&b), ["slow.two"]);
    assert_eq!(ended(&b), (EndReason::Finished, "slow", "done"));
}

#[test]
fn linger_then_closed() {
    let src = "dialogue hi(p: Player, npc: Person):\n    npc: \"hi\" @hi.hi\n";
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["hi"]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 50);
    assert_eq!(ended(&b), (EndReason::Finished, "hi", "done"));
    let until = 50 + CONVERSATION_LINGER_TICKS;
    assert_eq!(
        c.state(),
        &ConvState::Ended {
            linger_until: until
        }
    );
    assert_eq!(c.tick(&mut env, &mut rec, until - 1), None);
    assert!(matches!(c.state(), ConvState::Ended { .. }));
    assert_eq!(c.tick(&mut env, &mut rec, until), None);
    assert_eq!(c.state(), &ConvState::Closed);
    // The client's `Done` closes it at once.
    let (mut c, _) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 60);
    c.done();
    assert_eq!(c.state(), &ConvState::Closed);
    // Nothing attached: nothing to say, no end raised.
    let (c, b) = Conversation::start(&mut env, &[], &mut rec, player(1), person(1), 60);
    assert_eq!(c.state(), &ConvState::Closed);
    assert_eq!(b, Batch::default());
}

#[test]
fn faulting_guard_hides_option() {
    let src = r#"let zero = 0

dialogue d(p: Player, npc: Person):
    node start:
        npc: f"{10 / zero} apples" @d.apples
        choice:
            "Divide" @d.divide if 10 / zero == 1:
                npc: "never" @d.never
            "Again" @d.again:
                -> start
            "Bye" @d.bye
"#;
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["d"]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 0);
    assert_eq!(said(&b), ["d.apples"]);
    assert_eq!(b.lines[0].args, [WireValue::Str(String::new())]);
    assert_eq!(options(offer(&b)), ["d.again", "d.bye"]);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 1)
        .unwrap();
    assert_eq!(options(offer(&b)), ["d.again", "d.bye"]);
    // Two fault sites (the substitution, the guard), each reported once.
    assert_eq!(
        env.reports,
        [
            (InstanceId(1), PureFault::Fault(FaultKind::DivByZero)),
            (InstanceId(1), PureFault::Fault(FaultKind::DivByZero))
        ]
    );
}

#[test]
fn stale_pick_is_refused_and_menu_resent() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", TOPICS)]));
    let att = env.all(1);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(3), 0);
    let menu = offer(&b).clone();
    let b = c.pick(&mut env, &att, &mut rec, menu.serial, 2, 1).unwrap();
    let now_menu = offer(&b).clone();
    // The older serial: refused, the current offer re-sent.
    let before = hash(&c);
    assert_eq!(
        c.pick(&mut env, &att, &mut rec, menu.serial, 0, 2),
        Err(PickRefused::Stale {
            resent: now_menu.clone()
        })
    );
    assert_eq!(hash(&c), before);
    // `beta` was detached since the menu was built: refused, the menu as it is now.
    let fewer: Vec<Attachment> = env
        .all(1)
        .into_iter()
        .filter(|a| a != &env.att(1, &["beta"])[0])
        .collect();
    let Err(PickRefused::Stale { resent }) =
        c.pick(&mut env, &fewer, &mut rec, now_menu.serial, 0, 3)
    else {
        panic!("not refused");
    };
    assert_eq!(options(&resent), ["t.gamma", "t.alpha"]);
    assert!(resent.serial != now_menu.serial);
    assert_eq!(c.offer(), Some(&resent));
    let b = c
        .pick(&mut env, &fewer, &mut rec, resent.serial, 0, 4)
        .unwrap();
    assert_eq!(said(&b), ["t.g"]);
    // Once it has ended there is no offer.
    c.leave(5);
    assert_eq!(
        c.pick(&mut env, &fewer, &mut rec, resent.serial, 0, 6),
        Err(PickRefused::NoOffer)
    );
}

#[test]
fn out_of_range_pick_is_refused() {
    let (mut env, att) = farmer_env();
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 0);
    let o = offer(&b).clone();
    let before = (hash(&c), hash(&rec));
    for k in [4, 99, u32::MAX] {
        assert_eq!(
            c.pick(&mut env, &att, &mut rec, o.serial, k, 1),
            Err(PickRefused::OutOfRange { resent: o.clone() })
        );
    }
    for s in [o.serial.wrapping_add(1), u32::MAX, 0] {
        assert_eq!(
            c.pick(&mut env, &att, &mut rec, s, 0, 1),
            Err(PickRefused::Stale { resent: o.clone() })
        );
    }
    assert_eq!((hash(&c), hash(&rec)), before);
}

#[test]
fn emit_handler_changes_later_condition() {
    let src = r#"let accepted = false

event Accepted:
    player: Player

dialogue d(p: Player, npc: Person):
    node start:
        choice:
            "Accept" @d.accept if not accepted:
                emit Accepted(player=p)
                -> start
            "Look" @d.look:
                -> start

@on(Accepted)
fn on_accepted(e):
    accepted = true
"#;
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["d"]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 0);
    assert_eq!(options(offer(&b)), ["d.accept", "d.look"]);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 1)
        .unwrap();
    assert_eq!(b.emits.len(), 1);
    assert_eq!(b.emits[0].instance, InstanceId(1));
    assert_eq!(b.emits[0].event, "Accepted");
    assert_eq!(
        b.emits[0].value,
        WireValue::Struct {
            name: "Accepted".into(),
            fields: vec![("player".into(), player(1))],
        }
    );
    // The handler has not run: the condition still sees the old state.
    assert_eq!(options(offer(&b)), ["d.accept", "d.look"]);
    env.deliver(&b);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 1, 2)
        .unwrap();
    assert_eq!(options(offer(&b)), ["d.look"]);
}

#[test]
fn emit_of_an_unknown_host_event_is_refused() {
    // The graph's host-event index is not checked at load: the env names none, the runner skips it.
    struct NoHostEvents(Env);
    impl DialogueEnv for NoHostEvents {
        fn graph(&self, i: InstanceId) -> Option<&DialogueGraph> {
            self.0.graph(i)
        }
        fn eval(
            &mut self,
            i: InstanceId,
            f: u16,
            a: &[WireValue],
            d: u32,
        ) -> Result<WireValue, PureFault> {
            self.0.eval(i, f, a, d)
        }
        fn report(&mut self, i: InstanceId, fault: PureFault) {
            self.0.report(i, fault);
        }
        fn program_key(&self, i: InstanceId) -> Option<u64> {
            self.0.program_key(i)
        }
        fn event(&self, i: InstanceId, e: u16, script: bool) -> Option<EventShape> {
            if script {
                self.0.event(i, e, true)
            } else {
                None
            }
        }
    }
    let src = r#"dialogue d(p: Player, npc: Person):
    emit PlayerNear(player=p)
    npc: "after" @d.after
"#;
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["d"]);
    let (_, b) = Conversation::start(
        &mut env,
        &att,
        &mut Records::default(),
        player(1),
        person(1),
        0,
    );
    assert_eq!(b.emits.len(), 1, "the stub's env names its host events");
    assert_eq!(b.emits[0].event, "PlayerNear");
    let mut env = NoHostEvents(env);
    let (_, b) = Conversation::start(
        &mut env,
        &att,
        &mut Records::default(),
        player(1),
        person(1),
        0,
    );
    assert!(b.emits.is_empty());
    assert_eq!(said(&b), ["d.after"]);
    assert_eq!(env.0.reports, [(InstanceId(1), PureFault::BadCall)]);
}

#[test]
fn edit_removing_current_node_ends_edited() {
    let v1 = r#"dialogue d(p: Player, npc: Person):
    node start:
        npc: "hello" @d.hello
        choice:
            "A" @d.a
            "B" @d.b
"#;
    // The node survives (a new option): the offer is re-sent at the next tick.
    let v2 = v1.replace(
        "            \"B\" @d.b\n",
        "            \"B\" @d.b\n            \"C\" @d.c\n",
    );
    // The node is gone.
    let v3 = v1.replace("node start:", "node begin:");
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", v1)]));
    let att = env.att(1, &["d"]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 0);
    let old = offer(&b).clone();

    env.inst(1).prog = compiled(&[("main.qs", &v2)]);
    assert_eq!(c.rebind(&mut env), None);
    assert_eq!(
        c.pick(&mut env, &att, &mut rec, old.serial, 0, 1),
        Err(PickRefused::NoOffer),
        "withdrawn until it is re-sent"
    );
    let b = c.tick(&mut env, &mut rec, 1).expect("re-sent");
    assert_eq!(options(offer(&b)), ["d.a", "d.b", "d.c"]);
    assert!(b.lines.is_empty(), "nothing said again");

    env.inst(1).prog = compiled(&[("main.qs", &v3)]);
    let b = c.rebind(&mut env).expect("ended");
    assert_eq!(ended(&b), (EndReason::Edited, "d", "done"));
    assert!(matches!(c.state(), ConvState::Ended { .. }));
}

#[test]
fn moved_mid_conversation_same_transcript() {
    let (mut env, att) = farmer_env();
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 0);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 1, 1)
        .unwrap();
    // The NPC crosses a seam: its conversation and records travel; the new region evaluates over a mirror.
    let (mut c2, mut rec2) = (c.clone(), rec.clone());
    assert_eq!((hash(&c2), hash(&rec2)), (hash(&c), hash(&rec)));
    let (mut there, _) = farmer_env();
    there.mirror(1);
    let mut serial = offer(&b).serial;
    for (k, now) in [(1u32, 2u64), (2, 3)] {
        let here_b = c.pick(&mut env, &att, &mut rec, serial, k, now).unwrap();
        let there_b = c2
            .pick(&mut there, &att, &mut rec2, serial, k, now)
            .unwrap();
        assert_eq!(here_b, there_b);
        assert_eq!(hash(&here_b), hash(&there_b));
        assert_eq!((hash(&c2), hash(&rec2)), (hash(&c), hash(&rec)));
        if let Some(o) = &here_b.offer {
            serial = o.serial;
        }
    }
    assert!(matches!(c2.state(), ConvState::Ended { .. }));
}

/// Three scripts attach to one NPC; `b`'s module `extra` holds a topic.
fn three(swapped: bool) -> Vec<Arc<Program>> {
    let a = [
        "dialogue hi(p: Player, npc: Person):\n    npc: \"a hi\" @a.hi\n",
        "dialogue chat(p: Player, npc: Person) topic \"A chat\" @a.chat:\n    npc: \"a chat\" @a.chat_line\n",
    ];
    let b_main = [
        "mod extra\n",
        "dialogue hi(p: Player, npc: Person):\n    node n when p.has_tag(\"seen\"):\n        npc: \"b seen\" @b.seen\n    node n:\n        npc: \"b hi\" @b.hi\n",
    ];
    let b_extra = [
        "pub dialogue rumour(p: Player, npc: Person) topic \"B rumour\" @b.rumour priority 1:\n    npc: \"b rumour\" @b.rumour_line\n",
        "pub dialogue chat(p: Player, npc: Person) topic \"B chat\" @b.chat:\n    npc: \"b chat\" @b.chat_line\n",
    ];
    // `c` shares the qualified names `hi` and `chat` with `a`: those ties are broken by program.
    let c = [
        "dialogue chat(p: Player, npc: Person) topic \"C chat\" @c.chat:\n    npc: \"c chat\" @c.chat_line\n",
        "dialogue hi(p: Player, npc: Person):\n    npc: \"c hi\" @c.hi\n",
    ];
    let join = |parts: &[&str]| {
        let mut v: Vec<&str> = parts.to_vec();
        if swapped {
            v.reverse();
        }
        v.join("\n")
    };
    vec![
        compiled(&[("main.qs", &join(&a))]),
        compiled(&[("main.qs", &join(&b_main)), ("extra.qs", &join(&b_extra))]),
        compiled(&[("main.qs", &join(&c))]),
    ]
}

/// A batch with the instance numbers mapped out: lines (id, speaker, args), the offer (menu, option ids)
/// and the ends (reason, dialogue, exit), and the conversation's end.
type Projected = (
    Vec<(String, WireValue, Vec<WireValue>)>,
    Option<(bool, Vec<String>)>,
    Vec<(EndReason, String, String)>,
    Option<EndReason>,
);

fn project(b: &Batch) -> Projected {
    (
        b.lines
            .iter()
            .map(|l| (l.id.clone(), l.speaker.clone(), l.args.clone()))
            .collect(),
        b.offer
            .as_ref()
            .map(|o| (o.menu, o.options.iter().map(|l| l.id.clone()).collect())),
        b.ended
            .iter()
            .map(|e| (e.reason, e.dialogue.clone(), e.exit.clone()))
            .collect(),
        b.end,
    )
}

/// The whole transcript of one talk (greeting, every topic in menu order, leave) and the final hashes.
/// Program `k` is instance `numbering[k]`; the attachments come in program order `order`; `seen` tags the
/// player (`b`'s greeting then scores 1).
fn transcript(
    progs: &[Arc<Program>],
    order: &[usize],
    numbering: &[u32],
    seen: bool,
) -> (Vec<Batch>, u64, u64) {
    let mut env = Env::new();
    for (k, p) in progs.iter().enumerate() {
        env.add(numbering[k], p);
    }
    if seen {
        env.world.tags.insert((1, "seen".into()));
    }
    let att: Vec<Attachment> = order.iter().flat_map(|&k| env.all(numbering[k])).collect();
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(9), 0);
    let mut out = vec![b];
    for now in 1..10 {
        let Some(o) = out.last().and_then(|b| b.offer.clone()) else {
            break;
        };
        if o.options.len() <= usize::try_from(now - 1).unwrap() {
            out.push(c.leave(now));
            break;
        }
        let k = u32::try_from(now - 1).unwrap();
        out.push(c.pick(&mut env, &att, &mut rec, o.serial, k, now).unwrap());
    }
    (out, hash(&c), hash(&rec))
}

const PERMS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

#[test]
fn commutativity() {
    let ident = [1, 2, 3];
    let base = transcript(&three(false), &PERMS[0], &ident, true);
    // The greeting: `b`'s tagged node scores 1, the others 0.
    assert_eq!(said(&base.0[0]), ["b.seen"]);
    // Priority, then qualified name, then program (`a.chat` / `c.chat` tie on the name).
    let menu = options(offer(&base.0[0]));
    assert_eq!(menu[0], "b.rumour");
    assert_eq!(menu[3], "b.chat");
    assert_eq!(base.0.len(), 6, "greeting, four topics, the leave");
    for swapped in [false, true] {
        let progs = three(swapped);
        for order in &PERMS {
            assert_eq!(
                transcript(&progs, order, &ident, true),
                base,
                "order {order:?} swapped {swapped}"
            );
        }
    }
    // Instance numbering does not decide a tie between programs: the same transcript with the instance
    // numbers mapped out, for every numbering, with and without `b`'s scoring greeting.
    for seen in [true, false] {
        let progs = three(false);
        let want: Vec<Projected> = transcript(&progs, &PERMS[0], &ident, seen)
            .0
            .iter()
            .map(project)
            .collect();
        if !seen {
            // `a.hi`, `b.hi` (its second node) and `c.hi` all score 0 and share the name `hi`.
            assert_eq!(want[0].0.len(), 1);
            assert!(["a.hi", "b.hi", "c.hi"].contains(&want[0].0[0].0.as_str()));
        }
        for numbering in PERMS {
            let numbering: Vec<u32> = numbering
                .iter()
                .map(|&n| 10 + 7 * u32::try_from(n).unwrap())
                .collect();
            for order in &PERMS {
                let got: Vec<Projected> = transcript(&progs, order, &numbering, seen)
                    .0
                    .iter()
                    .map(project)
                    .collect();
                assert_eq!(
                    got, want,
                    "numbering {numbering:?} order {order:?} seen {seen}"
                );
            }
        }
    }
}

#[test]
fn picked_topic_with_empty_entry_raises_ended() {
    let src = r#"let open = false

dialogue shut(p: Player, npc: Person) topic "Shut" @shut.topic:
    node n when open:
        npc: "x" @shut.x
"#;
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", src)]));
    let att = env.att(1, &["shut"]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 0);
    assert_eq!(options(offer(&b)), ["shut.topic"]);
    let b = c
        .pick(&mut env, &att, &mut rec, offer(&b).serial, 0, 1)
        .unwrap();
    assert_eq!(ended(&b), (EndReason::Finished, "shut", "done"));
    assert_eq!(b.end, Some(EndReason::Finished));
    assert!(matches!(c.state(), ConvState::Ended { .. }));
}

#[test]
fn stale_topic_pick_on_an_emptied_menu_ends() {
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", TOPICS)]));
    let att = env.att(1, &["alpha"]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(3), 0);
    // `alpha` is detached before the pick: no greeting ran, and the menu is empty now.
    let b = c
        .pick(&mut env, &[], &mut rec, offer(&b).serial, 0, 1)
        .unwrap();
    assert!(b.ended.is_empty() && b.offer.is_none());
    assert_eq!(b.end, Some(EndReason::Finished));
    assert!(matches!(c.state(), ConvState::Ended { .. }));
}

#[test]
fn rebind_keeps_a_longer_pause() {
    let v1 = "dialogue slow(p: Player, npc: Person):\n    npc: \"one\" @slow.one\n    pause 50\n    npc: \"two\" @slow.two\n";
    // One step: the position (after the pause) no longer resolves and the node restarts.
    let v2 = "dialogue slow(p: Player, npc: Person):\n    npc: \"one\" @slow.one\n";
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", v1)]));
    let att = env.att(1, &["slow"]);
    let mut rec = Records::default();
    let (mut c, _) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 0);
    assert_eq!(c.state(), &ConvState::Paused { until: 50 });
    env.inst(1).prog = compiled(&[("main.qs", v2)]);
    assert_eq!(c.rebind(&mut env), None);
    assert_eq!(c.state(), &ConvState::Paused { until: 50 });
    assert_eq!(c.tick(&mut env, &mut rec, 49), None);
    let b = c.tick(&mut env, &mut rec, 50).expect("resumed");
    assert_eq!(said(&b), ["slow.one"]);
}

#[test]
fn rebind_restarting_a_caller_ends_edited() {
    let v1 = r#"let yes = true

dialogue outer(p: Player, npc: Person):
    when yes:
        detour inner

dialogue inner(p: Player, npc: Person):
    choice:
        "A" @inner.a
"#;
    // `outer`'s path (into its `when`) no longer resolves, while `inner` still waits at its choice.
    let v2 = v1.replace(
        "    when yes:\n        detour inner\n",
        "    npc: \"x\" @outer.x\n    detour inner\n",
    );
    let mut env = Env::new();
    env.add(1, &compiled(&[("main.qs", v1)]));
    let att = env.att(1, &["outer"]);
    let mut rec = Records::default();
    let (mut c, b) = Conversation::start(&mut env, &att, &mut rec, player(1), person(1), 0);
    assert_eq!(options(offer(&b)), ["inner.a"]);
    env.inst(1).prog = compiled(&[("main.qs", &v2)]);
    let b = c.rebind(&mut env).expect("ended");
    assert_eq!(ended(&b), (EndReason::Edited, "outer", "done"));
    assert_eq!(b.end, Some(EndReason::Edited));
}

#[test]
fn hostile_graphs_and_attachments_never_panic() {
    use quest_vm::dialogue::{Block, StepDef};
    let (mut env, att) = farmer_env();
    // Unknown instances and dialogues are ignored.
    let mut bad = att.clone();
    bad.push(Attachment {
        instance: InstanceId(42),
        dialogue: 0,
    });
    bad.push(Attachment {
        instance: InstanceId(1),
        dialogue: u16::MAX,
    });
    let (_, b) = Conversation::start(
        &mut env,
        &bad,
        &mut Records::default(),
        player(1),
        person(1),
        0,
    );
    assert_eq!(said(&b), ["farmer.start"]);
    // Blocks past the step table end the conversation.
    let mut p = (*env.insts[&1].prog).clone();
    for d in &mut p.dialogues.dialogues {
        for n in &mut d.nodes {
            n.body = Block {
                first: u32::MAX - 1,
                len: u32::MAX,
            };
        }
    }
    env.inst(1).prog = Arc::new(p);
    let (_, b) = Conversation::start(
        &mut env,
        &att,
        &mut Records::default(),
        player(1),
        person(1),
        0,
    );
    assert_eq!(ended(&b).0, EndReason::Edited);
    // Steps naming nothing that exists.
    for step in [
        StepDef::Jump { group: u16::MAX },
        StepDef::Detour {
            dialogue: u16::MAX,
            arms: vec![(
                u16::MAX,
                Block {
                    first: u32::MAX,
                    len: 1,
                },
            )],
        },
        StepDef::Return { exit: u16::MAX },
        StepDef::Emit {
            event: u16::MAX,
            script: true,
            args: vec![u16::MAX],
        },
        StepDef::Line {
            line: quest_vm::dialogue::LineDef {
                id: "x".into(),
                speaker: quest_vm::dialogue::Speaker::Fn(u16::MAX),
                subst: vec![u16::MAX],
                ticks: 0,
            },
            cond: quest_vm::dialogue::CondDef {
                once: None,
                guard: Some(u16::MAX),
            },
        },
        StepDef::Pause { ticks: u32::MAX },
    ] {
        let (mut env, att) = farmer_env();
        let mut p = (*env.insts[&1].prog).clone();
        let first = p.dialogues.steps.len();
        p.dialogues.steps.push(step.clone());
        for d in &mut p.dialogues.dialogues {
            for n in &mut d.nodes {
                n.body = Block {
                    first: u32::try_from(first).unwrap(),
                    len: 1,
                };
            }
        }
        env.inst(1).prog = Arc::new(p);
        let mut rec = Records::default();
        let (mut c, _) =
            Conversation::start(&mut env, &att, &mut rec, player(1), person(1), u64::MAX - 1);
        let _ = c.tick(&mut env, &mut rec, u64::MAX);
        let _ = c.pick(&mut env, &att, &mut rec, u32::MAX, u32::MAX, u64::MAX);
        let _ = c.rebind(&mut env);
        let _ = c.leave(u64::MAX);
        assert!(
            matches!(c.state(), ConvState::Ended { .. } | ConvState::Closed),
            "{step:?}: {:?}",
            c.state()
        );
    }
}
