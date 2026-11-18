//! The conversation runner (`quest_vm::dialogue_run`, spec §5.11) driven by ops over a Phase 10 corpus
//! program: the first byte picks a run case with conversation dialogues (compiled once per process), every
//! further byte group is an op — `talk p n mask`, `pick which serial option`, `leave which`, `done which`,
//! `tick`, `checkpoint`, `rollback`, `edit <variant>` (the program again; the case's own edit directory,
//! else the program with a comment line before it; another Phase 10 case's program), `shuffle <8 bytes>`.
//!
//! The runner reads through the stub's own env, `quest_stub::DialogueWorld` — the dialogue mirrors, the
//! stage, the attached set, the people's queries, the faults — exactly as the stub's `World` does. What is
//! the target's own is what `World` does not expose: the instances (the case's `World` placed and settled,
//! every VM copied out into a `Runner`), the conversations and records, the events in flight (a batch's
//! emits and `Talked`, delivered at the next tick), the attachments handlers would make (a talk's `mask`
//! attaches and detaches as `attach_dialogue` / `detach_dialogue` would), and a checkpoint of all of it.
//!
//! Properties: no panic; every pick is `Ok` or a `PickRefused` whose offer is the conversation's current
//! one (`NoOffer` only when none is out); a rollback brings the world's hash back to the checkpoint's; the
//! attachments in another order (a seeded Fisher–Yates permutation) give the same batch, conversation and
//! records as in sorted order (commutativity); once the inputs stop, every conversation is `Closed` — or
//! waits on a pick with no timeout, an input the player owes — within `CONVERSATION_LINGER_TICKS` +
//! `DIALOGUE_STACK_MAX` × the longest pause or choice timeout of the program, and, left, is `Closed` within
//! the linger.
#![no_main]

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher as _;
use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use quest_corpus::{Case, RUN, config_of, is_world, tree_of};
use quest_lang::{SourceTree, compile};
use quest_stub::{
    DialogueWorld, People, Runner, StubConfig, StubHost, World, parse_people, parse_wire, vm_config,
};
use quest_vm::consts::{CONVERSATION_LINGER_TICKS, DIALOGUE_STACK_MAX, MEM_BYTES_DEFAULT};
use quest_vm::dialogue::{DialogueGraph, DialogueKind, StepDef};
use quest_vm::dialogue_run::{Attachment, Batch, ConvState, Conversation, PickRefused, Records};
use quest_vm::wire::WireValue;
use quest_vm::{Checkpoint, Fnv64, Program, Vm};
use state_hash::StateHash;

/// Ops read per input.
const OPS_MAX: usize = 64;
/// Fuel per instance per tick.
const FUEL: u64 = 5_000;
/// Ticks the placed world may take to settle before the conversations start.
const SETTLE_TICKS: u64 = 200;

/// A Phase 10 case, ready to talk in.
struct Talks {
    name: &'static str,
    program: Arc<Program>,
    people: People,
    roles: Vec<(String, WireValue)>,
    stub: StubConfig,
    /// Each instance's VM once the placed world settled.
    saves: Vec<Vec<u8>>,
    /// The edit variants' programs (`None`: does not compile or fit).
    edits: [Option<Arc<Program>>; 3],
    players: Vec<i64>,
    npcs: Vec<i64>,
    /// The longest pause or choice timeout of the program or its edits.
    longest: u64,
}

fn has_conversations(p: &Program) -> bool {
    p.dialogues
        .dialogues
        .iter()
        .any(|d| d.kind == DialogueKind::Conversation && d.cast == 2)
}

fn longest(g: &DialogueGraph) -> u64 {
    g.steps
        .iter()
        .map(|s| match s {
            StepDef::Pause { ticks } => u64::from(*ticks),
            StepDef::Choice {
                timeout: Some(t), ..
            } => u64::from(*t),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

fn compiled(tree: &SourceTree) -> Option<Arc<Program>> {
    let p = compile(tree, &quest_stub::stub_api()).ok()?.program;
    quest_stub::bind(&p).ok()?;
    Some(Arc::new(p))
}

/// The case's sources under its first sub-directory with a `main.qs` (an `edit` target).
fn edit_dir(case: &Case) -> Option<SourceTree> {
    let (dir, _) = case
        .files
        .iter()
        .find_map(|(p, _)| p.strip_suffix("/main.qs").map(|d| (d, ())))?;
    let prefix = format!("{dir}/");
    Some(SourceTree {
        files: case
            .files
            .iter()
            .filter_map(|(p, t)| Some((p.strip_prefix(&prefix)?.to_string(), t.to_string())))
            .collect(),
    })
}

/// The case's own sources (without any sub-directory).
fn own_tree(case: &Case) -> SourceTree {
    let mut t = tree_of(case);
    t.files.retain(|p, _| !p.contains('/'));
    t
}

fn cases() -> &'static [Talks] {
    static CASES: OnceLock<Vec<Talks>> = OnceLock::new();
    CASES.get_or_init(|| {
        let mut out: Vec<Talks> = RUN
            .iter()
            .filter(|c| is_world(c))
            .filter_map(|case| {
                let tree = own_tree(case);
                let program = compiled(&tree)?;
                if !has_conversations(&program) {
                    return None;
                }
                let people = case
                    .people
                    .map_or_else(|| Ok(People::default()), parse_people)
                    .ok()?;
                let cfg = config_of(case);
                let roles = cfg
                    .roles
                    .iter()
                    .map(|(n, w)| Some((n.clone(), parse_wire(w, &people).ok()?)))
                    .collect::<Option<Vec<_>>>()?;
                let stub = StubConfig {
                    latency: cfg.latency,
                    failures: cfg.failures.clone(),
                    chest: cfg.chest.clone(),
                    ..StubConfig::default()
                };
                let mut w = World::new(
                    Arc::clone(&program),
                    people.clone(),
                    roles.clone(),
                    stub.clone(),
                    FUEL,
                    MEM_BYTES_DEFAULT,
                )
                .ok()?;
                w.start_main();
                for _ in 0..SETTLE_TICKS {
                    if w.quiescent() {
                        break;
                    }
                    w.step(&[]);
                }
                let saves = (0..w.instances())
                    .filter_map(|k| Some(w.vm(u16::try_from(k).ok()?)?.save()))
                    .collect();
                let mut commented = tree.clone();
                if let Some(main) = commented.files.get_mut("main.qs") {
                    main.insert_str(0, "# edited\n");
                }
                let second = edit_dir(case).unwrap_or(commented);
                let (mut players, mut npcs) = (Vec::new(), Vec::new());
                for (id, e) in &people.entities {
                    match e.kind {
                        quest_stub::EntityKind::Player => players.push(*id),
                        quest_stub::EntityKind::Person | quest_stub::EntityKind::Creature => {
                            npcs.push(*id)
                        }
                        _ => {}
                    }
                }
                if players.is_empty() || npcs.is_empty() {
                    return None;
                }
                Some(Talks {
                    name: case.name,
                    longest: longest(&program.dialogues),
                    edits: [Some(Arc::clone(&program)), compiled(&second), None],
                    program,
                    people,
                    roles,
                    stub,
                    saves,
                    players,
                    npcs,
                })
            })
            .collect();
        // The third variant: the next case's program.
        let others: Vec<Arc<Program>> = out.iter().map(|t| Arc::clone(&t.program)).collect();
        let n = others.len();
        for (k, t) in out.iter_mut().enumerate() {
            t.edits[2] = Some(Arc::clone(&others[(k + 1) % n]));
        }
        for t in &mut out {
            let l = t
                .edits
                .iter()
                .flatten()
                .map(|p| longest(&p.dialogues))
                .max()
                .unwrap_or(0);
            t.longest = t.longest.max(l);
        }
        out
    })
}

/// A conversation with an NPC.
#[derive(Clone)]
struct Talk {
    player: i64,
    conv: Conversation,
}

/// An instance at a checkpoint: its VM's undo position, its host, its clock.
type InstSnapshot = (Checkpoint, StubHost, u64);

/// What a checkpoint keeps.
struct Snapshot {
    now: u64,
    env: DialogueWorld,
    talks: BTreeMap<i64, Talk>,
    records: BTreeMap<i64, Records>,
    pending: Vec<(u32, String, WireValue)>,
    attached: BTreeSet<(u32, i64, String)>,
    detached: BTreeSet<(u32, i64, String)>,
    insts: Vec<InstSnapshot>,
    hash: u64,
}

struct Sim<'a> {
    case: &'a Talks,
    /// The instances, in id order.
    insts: Vec<Runner>,
    env: DialogueWorld,
    now: u64,
    talks: BTreeMap<i64, Talk>,
    records: BTreeMap<i64, Records>,
    /// Events for the next tick: (instance, event, payload).
    pending: Vec<(u32, String, WireValue)>,
    /// What handlers attached and detached: (instance, NPC id, dialogue).
    attached: BTreeSet<(u32, i64, String)>,
    detached: BTreeSet<(u32, i64, String)>,
    snapshot: Option<Snapshot>,
    /// The seed of the order the attachments are handed over in.
    shuffle: u64,
}

fn struct_of(name: &str, fields: Vec<(&str, WireValue)>) -> WireValue {
    WireValue::Struct {
        name: name.into(),
        fields: fields
            .into_iter()
            .map(|(n, v)| (n.to_string(), v))
            .collect(),
    }
}

/// SplitMix64: the next value of `state`.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// `att` permuted by a Fisher–Yates shuffle seeded with `seed`.
fn shuffled(att: &[Attachment], seed: u64) -> Vec<Attachment> {
    let mut out = att.to_vec();
    let mut state = seed;
    for k in (1..out.len()).rev() {
        let j = (splitmix(&mut state) % (k as u64 + 1)) as usize;
        out.swap(k, j);
    }
    out
}

impl<'a> Sim<'a> {
    fn new(case: &'a Talks) -> Option<Sim<'a>> {
        let mut insts = Vec::new();
        for (k, bytes) in case.saves.iter().enumerate() {
            let id = u16::try_from(k).ok()?;
            let mut runner = Runner::for_instance(
                Arc::clone(&case.program),
                MEM_BYTES_DEFAULT,
                case.stub.clone(),
                id,
                case.roles.clone(),
            )
            .ok()?;
            runner.vm = Vm::load(
                Arc::clone(&case.program),
                vm_config(MEM_BYTES_DEFAULT, id, case.roles.clone()),
                bytes,
            )
            .ok()?;
            runner.host.people = case.people.clone();
            insts.push(runner);
        }
        let mut env = DialogueWorld::new(
            Arc::clone(&case.program),
            case.people.clone(),
            case.roles.clone(),
            MEM_BYTES_DEFAULT,
        );
        env.take_mirrors(insts.iter().map(|r| &r.vm));
        Some(Sim {
            case,
            insts,
            env,
            now: 0,
            talks: BTreeMap::new(),
            records: BTreeMap::new(),
            pending: Vec::new(),
            attached: BTreeSet::new(),
            detached: BTreeSet::new(),
            snapshot: None,
            shuffle: 0,
        })
    }

    /// The world's hash: the clock, each instance's VM and clock, the mirrors, the conversations, the
    /// records, the events in flight, the attachments.
    fn hash(&self) -> u64 {
        let mut h = Fnv64::default();
        self.now.state_hash(&mut h);
        self.env.program_key.state_hash(&mut h);
        for r in &self.insts {
            r.vm.hash64().state_hash(&mut h);
            r.now.state_hash(&mut h);
        }
        self.env.hash_mirrors(&mut h);
        (self.talks.len() as u64).state_hash(&mut h);
        for (npc, tk) in &self.talks {
            npc.state_hash(&mut h);
            tk.player.state_hash(&mut h);
            tk.conv.state_hash(&mut h);
        }
        self.records.state_hash(&mut h);
        (self.pending.len() as u64).state_hash(&mut h);
        for (i, e, v) in &self.pending {
            i.state_hash(&mut h);
            e.state_hash(&mut h);
            v.state_hash(&mut h);
        }
        self.attached.state_hash(&mut h);
        self.detached.state_hash(&mut h);
        h.finish()
    }

    /// The NPC's attachments now, as the stub's `World` computes them.
    fn attachments(&mut self, player: &WireValue, npc: &WireValue) -> Vec<Attachment> {
        self.env.now = self.now;
        let att = self
            .env
            .attachments(player, npc, &self.attached, &self.detached);
        // A fault counts as false; the env keeps it for a caller that reports it.
        self.env.faults.clear();
        att
    }

    /// What handlers would do before a talk: for each conversation dialogue `k` (cast 2) and instance
    /// `i`, `attach_dialogue` when bit `(3k + i) % 7` of `mask` is set; with bit 7 set, `detach_dialogue`
    /// of the home's dialogues it did not attach.
    fn handlers_attach(&mut self, npc: i64, mask: u8) {
        let program = Arc::clone(&self.env.program);
        for (k, d) in program.dialogues.dialogues.iter().enumerate() {
            if d.kind != DialogueKind::Conversation || d.cast != 2 {
                continue;
            }
            for i in 0..self.insts.len() {
                let Ok(i32) = u32::try_from(i) else { break };
                let key = (i32, npc, d.name.clone());
                if mask & (1 << ((3 * k + i) % 7)) != 0 {
                    self.detached.remove(&key);
                    self.attached.insert(key);
                } else if i == 0 && mask & 0x80 != 0 {
                    self.attached.remove(&key);
                    self.detached.insert(key);
                }
            }
        }
    }

    /// The emits of a batch for the next tick.
    fn batch(&mut self, b: &Batch) {
        for e in &b.emits {
            self.pending
                .push((e.instance.0, e.event.clone(), e.value.clone()));
        }
    }

    fn cast(&self, player: i64, npc: i64) -> Option<(WireValue, WireValue)> {
        Some((
            self.case.people.handle(player)?,
            self.case.people.handle(npc)?,
        ))
    }

    fn talk(&mut self, p: u8, n: u8, mask: u8) {
        let player = self.case.players[usize::from(p) % self.case.players.len()];
        let npc = self.case.npcs[usize::from(n) % self.case.npcs.len()];
        if self
            .talks
            .get(&npc)
            .is_some_and(|tk| *tk.conv.state() != ConvState::Closed)
        {
            return;
        }
        let Some((pv, nv)) = self.cast(player, npc) else {
            return;
        };
        self.handlers_attach(npc, mask);
        let att = self.attachments(&pv, &nv);
        let now = self.now;
        // Commutativity: the attachments in sorted order, on copies.
        let mut rec_sorted = self.records.get(&npc).cloned().unwrap_or_default();
        let mut sorted = att.clone();
        sorted.sort();
        let (conv_sorted, batch_sorted) = Conversation::start(
            &mut self.env,
            &sorted,
            &mut rec_sorted,
            pv.clone(),
            nv.clone(),
            now,
        );
        let rec = self.records.entry(npc).or_default();
        let (conv, batch) = Conversation::start(
            &mut self.env,
            &shuffled(&att, self.shuffle),
            rec,
            pv.clone(),
            nv.clone(),
            now,
        );
        self.env.faults.clear();
        let name = self.case.name;
        assert_eq!(
            batch, batch_sorted,
            "{name}: the attachments' order changed a talk's batch"
        );
        assert_eq!(
            conv, conv_sorted,
            "{name}: the attachments' order changed a conversation"
        );
        assert_eq!(
            self.records.get(&npc),
            Some(&rec_sorted),
            "{name}: the attachments' order changed the records"
        );
        self.batch(&batch);
        let talked = struct_of("Talked", vec![("player", pv), ("npc", nv)]);
        for i in 0..self.insts.len() {
            if let Ok(i) = u32::try_from(i) {
                self.pending.push((i, "Talked".into(), talked.clone()));
            }
        }
        self.talks.insert(npc, Talk { player, conv });
    }

    /// The NPC of the `which`-th conversation.
    fn nth(&self, which: u8) -> Option<i64> {
        if self.talks.is_empty() {
            return None;
        }
        self.talks
            .keys()
            .nth(usize::from(which) % self.talks.len())
            .copied()
    }

    fn pick(&mut self, which: u8, serial: u8, option: u8) {
        let Some(npc) = self.nth(which) else { return };
        let Some(tk) = self.talks.get(&npc).cloned() else {
            return;
        };
        let Some((pv, nv)) = self.cast(tk.player, npc) else {
            return;
        };
        let att = self.attachments(&pv, &nv);
        // Mostly the serial out; sometimes another one.
        let serial = match (serial & 3, tk.conv.offer()) {
            (0, _) | (_, None) => u32::from(serial >> 2),
            (_, Some(o)) => o.serial,
        };
        let option = u32::from(option % 6);
        let now = self.now;
        let rec_before = self.records.get(&npc).cloned().unwrap_or_default();
        let (mut conv_sorted, mut rec_sorted) = (tk.conv.clone(), rec_before.clone());
        let mut sorted = att.clone();
        sorted.sort();
        let r_sorted =
            conv_sorted.pick(&mut self.env, &sorted, &mut rec_sorted, serial, option, now);
        let mut conv = tk.conv;
        let mut rec = rec_before;
        let r = conv.pick(
            &mut self.env,
            &shuffled(&att, self.shuffle),
            &mut rec,
            serial,
            option,
            now,
        );
        self.env.faults.clear();
        let name = self.case.name;
        assert_eq!(
            r, r_sorted,
            "{name}: the attachments' order changed a pick's batch"
        );
        assert_eq!(
            conv, conv_sorted,
            "{name}: the attachments' order changed a conversation"
        );
        assert_eq!(
            rec, rec_sorted,
            "{name}: the attachments' order changed the records"
        );
        match &r {
            Ok(b) => self.batch(b),
            Err(PickRefused::NoOffer) => assert!(
                conv.offer().is_none(),
                "{name}: a pick refused `NoOffer` beside the offer {:?}",
                conv.offer()
            ),
            Err(PickRefused::Stale { resent } | PickRefused::OutOfRange { resent }) => assert_eq!(
                Some(resent),
                conv.offer(),
                "{name}: a refused pick carries another offer than the current one"
            ),
        }
        self.records.insert(npc, rec);
        if let Some(t) = self.talks.get_mut(&npc) {
            t.conv = conv;
        }
    }

    fn leave(&mut self, which: u8) {
        let Some(npc) = self.nth(which) else { return };
        let now = self.now;
        let Some(tk) = self.talks.get_mut(&npc) else {
            return;
        };
        let b = tk.conv.leave(now);
        self.batch(&b);
    }

    fn done(&mut self, which: u8) {
        if let Some(npc) = self.nth(which)
            && let Some(tk) = self.talks.get_mut(&npc)
        {
            tk.conv.done();
        }
    }

    /// One tick: the events in flight delivered, every instance run, the mirrors taken, every
    /// conversation's clock moved on.
    fn tick(&mut self) {
        for (i, event, value) in std::mem::take(&mut self.pending) {
            if let Some(r) = usize::try_from(i).ok().and_then(|i| self.insts.get_mut(i)) {
                let _ = r.vm.deliver(&event, &value);
            }
        }
        for r in &mut self.insts {
            r.host.output.clear();
            r.host.reports.clear();
            r.step(FUEL);
        }
        self.env.take_mirrors(self.insts.iter().map(|r| &r.vm));
        let now = self.now;
        self.env.now = now;
        let npcs: Vec<i64> = self.talks.keys().copied().collect();
        for npc in npcs {
            let Some(tk) = self.talks.get_mut(&npc) else {
                continue;
            };
            let rec = self.records.entry(npc).or_default();
            if let Some(b) = tk.conv.tick(&mut self.env, rec, now) {
                self.batch(&b);
            }
        }
        self.env.faults.clear();
        self.now += 1;
    }

    fn checkpoint(&mut self) {
        let hash = self.hash();
        self.snapshot = Some(Snapshot {
            now: self.now,
            env: self.env.clone(),
            talks: self.talks.clone(),
            records: self.records.clone(),
            pending: self.pending.clone(),
            attached: self.attached.clone(),
            detached: self.detached.clone(),
            insts: self
                .insts
                .iter()
                .map(|r| (r.vm.checkpoint(), r.host.clone(), r.now))
                .collect(),
            hash,
        });
    }

    fn rollback(&mut self) {
        let Some(s) = &self.snapshot else { return };
        let name = self.case.name;
        self.now = s.now;
        self.env = s.env.clone();
        self.talks = s.talks.clone();
        self.records = s.records.clone();
        self.pending = s.pending.clone();
        self.attached = s.attached.clone();
        self.detached = s.detached.clone();
        for (r, (cp, host, now)) in self.insts.iter_mut().zip(&s.insts) {
            r.vm.rollback_to(*cp)
                .unwrap_or_else(|e| panic!("{name}: a VM refused the rollback: {e:?}"));
            r.host = host.clone();
            r.now = *now;
        }
        assert_eq!(
            self.hash(),
            s.hash,
            "{name}: the world's hash after a rollback"
        );
    }

    /// The script edited (as the stub's `World::edit`): every instance migrated (all or none), the
    /// mirrors taken again, every conversation rebound. A checkpoint does not survive it (its undo log is
    /// the old instances').
    fn edit(&mut self, variant: u8) {
        let Some(program) = self.case.edits[usize::from(variant) % 3].clone() else {
            return;
        };
        let mut vms = Vec::new();
        for (k, r) in self.insts.iter().enumerate() {
            let Ok(id) = u16::try_from(k) else { return };
            let cfg = vm_config(MEM_BYTES_DEFAULT, id, self.env.roles.clone());
            let Ok(copy) = Vm::load(Arc::clone(&self.env.program), cfg.clone(), &r.vm.save())
            else {
                return;
            };
            match copy.migrate(Arc::clone(&program), cfg) {
                Ok((vm, _)) => vms.push(vm),
                Err(_) => return,
            }
        }
        for (r, vm) in self.insts.iter_mut().zip(vms) {
            r.vm = vm;
        }
        self.env.set_program(program);
        self.env.take_mirrors(self.insts.iter().map(|r| &r.vm));
        self.snapshot = None;
        let npcs: Vec<i64> = self.talks.keys().copied().collect();
        for npc in npcs {
            let Some(tk) = self.talks.get_mut(&npc) else {
                continue;
            };
            if let Some(b) = tk.conv.rebind(&mut self.env) {
                self.batch(&b);
            }
        }
        self.env.faults.clear();
    }

    /// The inputs stopped: every conversation closes, or waits on a pick with no timeout, within the bound;
    /// left, it closes within the linger.
    fn settle(&mut self) {
        let bound = CONVERSATION_LINGER_TICKS
            + u64::try_from(DIALOGUE_STACK_MAX).unwrap_or(u64::MAX) * self.case.longest.max(1)
            + 1;
        for _ in 0..bound {
            self.tick();
        }
        let name = self.case.name;
        for (npc, tk) in &self.talks {
            let owed = *tk.conv.state() == ConvState::AwaitingPick
                && tk.conv.offer().is_some_and(|o| o.timeout_at.is_none());
            assert!(
                owed || *tk.conv.state() == ConvState::Closed,
                "{name}: the conversation with {npc} is {:?} {bound} ticks after the inputs stopped",
                tk.conv.state()
            );
        }
        let now = self.now;
        let mut left = Vec::new();
        for tk in self.talks.values_mut() {
            left.push(tk.conv.leave(now));
        }
        for b in &left {
            self.batch(b);
        }
        for _ in 0..=CONVERSATION_LINGER_TICKS {
            self.tick();
        }
        for (npc, tk) in &self.talks {
            assert_eq!(
                *tk.conv.state(),
                ConvState::Closed,
                "{name}: the conversation with {npc}, left, is not closed after the linger"
            );
        }
    }
}

/// The ops' bytes, read in turn (zeros past the end).
struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let Some((&b, rest)) = self.0.split_first() else {
            return 0;
        };
        self.0 = rest;
        b
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&pick, ops)) = data.split_first() else {
        return;
    };
    let cases = cases();
    if cases.is_empty() {
        return;
    }
    let Some(mut sim) = Sim::new(&cases[usize::from(pick) % cases.len()]) else {
        return;
    };
    let mut b = Bytes(ops);
    for _ in 0..OPS_MAX {
        if b.0.is_empty() {
            break;
        }
        match b.next() % 9 {
            0 => {
                let (p, n, mask) = (b.next(), b.next(), b.next());
                sim.talk(p, n, mask);
            }
            1 => {
                let (which, serial, option) = (b.next(), b.next(), b.next());
                sim.pick(which, serial, option);
            }
            2 => sim.leave(b.next()),
            3 => sim.done(b.next()),
            4 => sim.tick(),
            5 => sim.checkpoint(),
            6 => sim.rollback(),
            7 => sim.edit(b.next()),
            _ => sim.shuffle = u64::from_le_bytes(std::array::from_fn(|_| b.next())),
        }
    }
    sim.settle();
});
