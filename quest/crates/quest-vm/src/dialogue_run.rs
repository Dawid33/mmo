//! The conversation runner (spec §5.11 "The runner", "Pacing", "Scoring", "Edits"): a player talking to an
//! NPC, walked over the dialogue graphs (`dialogue`) of the instances that attach dialogues to it.
//!
//! The runner never touches a live VM. It reads graphs and evaluates pure fns through a [`DialogueEnv`]
//! (an instance's live VM or its mirror: the caller's choice), and returns what happened as a [`Batch`] of
//! values: lines said (ids and substitutions; the text is the caller's), the offer the player answers, the
//! events `emit`ted (the caller delivers them), the end. A [`Conversation`] and the NPC's [`Records`] are
//! plain data, hashed with [`StateHash`]: carried, saved and rolled back by the caller like any region state.
//!
//! **Talking.** The greetings attached to the NPC form one pool of their entry node groups, picked by
//! score, then fewest prior picks, then the stable key (qualified name, node position, instance). The picked
//! greeting runs; then the **topic menu**: every attached topic whose `if` holds, by priority (higher first),
//! then qualified name. A picked topic runs as a detour from the menu and returns to it, re-evaluated. A
//! menu with nothing on it ends the conversation `Finished`; with no greeting and no topic the NPC has
//! nothing to say (closed at once, no end raised). Attachments are kept by (instance, qualified name), sorted:
//! the order they come in never matters.
//!
//! **Ends.** The greeting and each topic picked from the menu is a top-level run: each raises its own
//! [`Ended`] to its own instance when it ends — `Finished` with its exit when it goes back to the menu,
//! else the reason the conversation ended while it ran. Ending at the menu raises nothing more; the
//! conversation's own end is its [`ConvState`].
//!
//! **Walking.** One call walks the steps until a choice (an [`Offer`]), a `pause`, or the end: one batch.
//! Conditions are evaluated at that moment; an `Err` counts as false (an empty substitution, an emit not
//! sent) and is reported once per fault site (instance and fn) per conversation. Group members are picked
//! by the dialogue's `@pick`; the `random_*` tie-break is [`roll`]`(now, hash(qualified name, site), member)`.
//! A choice with no option to offer says nothing and the walk goes on after it. A walk longer than
//! [`BATCH_STEPS_MAX`] steps, or a return stack past [`DIALOGUE_STACK_MAX`], ends `TooDeep`; an empty node
//! group (an entry, a `->`) ends `Finished`.
//!
//! **Picks.** An offer carries a serial; a pick names it. A pick of an older serial is `Stale` and the
//! current offer is re-sent; a menu pick of a topic no longer attached or eligible is `Stale` too, with the
//! re-evaluated menu. A timeout picks the first offered `default` option, else runs the hidden `default:`
//! block, else ends `Timeout`.
//!
//! **Records** ([`Records`], per NPC) are keyed by (instance, the dialogue's qualified name, site), so two
//! scripts never disturb each other: `once` sites (per value of `per`, by its hash), the pick counts of node
//! and line group members.
//!
//! **Positions** are paths from a node (the node's name and its member index in the group, then each
//! nested block by its index among the containing step's blocks), so [`Conversation::rebind`] finds them
//! again in an edited graph: a dialogue or node that is gone ends `Edited`; a path that no longer resolves
//! restarts its node.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;

use state_hash::StateHash;

use crate::consts::{BATCH_STEPS_MAX, CONVERSATION_LINGER_TICKS, DIALOGUE_STACK_MAX};
use crate::dialogue::{
    Block, CondDef, DialogueDef, DialogueGraph, DialogueKind, LineDef, Pick, Speaker, StepDef,
};
use crate::hash::Fnv64;
use crate::pure::PureFault;
use crate::wire::WireValue;

/// A script instance, as the caller numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstanceId(pub u32);

/// Instance `instance` attaches its dialogue `dialogue` (an index into its graph's dialogues) to the NPC.
/// Filters and stage dialogues are resolved by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Attachment {
    pub instance: InstanceId,
    pub dialogue: u16,
}

/// An event an `emit` names: its name (a script event's qualified name) and its fields in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventShape {
    pub name: String,
    pub fields: Vec<String>,
}

/// What the runner reads and evaluates through.
pub trait DialogueEnv {
    /// Instance `i`'s dialogue graph (`Program::dialogues`).
    fn graph(&self, i: InstanceId) -> Option<&DialogueGraph>;
    /// Evaluates the pure fn `f` of instance `i` with `args` (the cast), `depth` being the current detour
    /// depth (1 in the outermost dialogue): the env answers `dialogue_depth()` (the host query
    /// `quest.dialogue_depth`) from it.
    fn eval(
        &mut self,
        i: InstanceId,
        f: u16,
        args: &[WireValue],
        depth: u32,
    ) -> Result<WireValue, PureFault>;
    /// A fault of instance `i`'s pure fn (or a refused emit): told once per fault site per conversation.
    fn report(&mut self, i: InstanceId, fault: PureFault);
    /// The event `StepDef::Emit` names: a script event (`script`: an index into `Program::events`) or a
    /// host event (an index into `HostApi::events`). `None` (no such event) refuses the emit: it is
    /// reported (`PureFault::BadCall`) and skipped. The graph cannot check a host event's index at load.
    fn event(&self, i: InstanceId, event: u16, script: bool) -> Option<EventShape>;
}

/// Records of one NPC: `once` sites and pick counts, per (instance, dialogue qualified name).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Records {
    by: BTreeMap<(InstanceId, String), DialogueRecords>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DialogueRecords {
    /// `once` sites used, with the hash of the `per` value.
    once: BTreeSet<(u32, Option<u64>)>,
    /// How often each group member was picked.
    picks: BTreeMap<PickKey, u32>,
}

/// A `once` record: its site and the hash of its `per` value.
type OnceRec = (u32, Option<u64>);

/// A greeting candidate's rank, the smallest first: (score negated, picks, qualified name, node, instance).
type GreetRank = (i32, u32, String, usize, InstanceId);

/// A group member: a node by its group's name and its index in the group, or a line group's member.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PickKey {
    Node { name: String, member: u32 },
    Line { site: u32, member: u32 },
}

impl Records {
    fn of(&self, i: InstanceId, d: &str) -> Option<&DialogueRecords> {
        self.by.get(&(i, d.to_string()))
    }

    fn of_mut(&mut self, i: InstanceId, d: &str) -> &mut DialogueRecords {
        self.by.entry((i, d.to_string())).or_default()
    }

    fn once_used(&self, i: InstanceId, d: &str, site: u32, per: Option<u64>) -> bool {
        self.of(i, d).is_some_and(|r| r.once.contains(&(site, per)))
    }

    fn picks(&self, i: InstanceId, d: &str, key: &PickKey) -> u32 {
        self.of(i, d)
            .and_then(|r| r.picks.get(key))
            .copied()
            .unwrap_or(0)
    }

    fn picked(&mut self, i: InstanceId, d: &str, key: PickKey) {
        let n = self.of_mut(i, d).picks.entry(key).or_insert(0);
        *n = n.saturating_add(1);
    }
}

/// Why a conversation ended (the host's `EndReason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Finished,
    Left,
    Timeout,
    TooDeep,
    Edited,
    OutOfRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvState {
    /// Between calls only while a batch is being walked; never seen by a caller.
    Running,
    /// An offer is out ([`Conversation::offer`]).
    AwaitingPick,
    /// A `pause`: the walk goes on at tick `until`.
    Paused {
        until: u64,
    },
    /// Over; open until `done()` or tick `linger_until`.
    Ended {
        linger_until: u64,
    },
    Closed,
}

/// What one call produced, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    pub lines: Vec<SaidLine>,
    pub offer: Option<Offer>,
    pub emits: Vec<Emitted>,
    /// The top-level dialogue runs that ended, in order (the conversation's own end is its state).
    pub ended: Vec<Ended>,
}

/// A line: its id, its speaker and its substitutions (in hole order; an empty string where one faulted).
#[derive(Clone, Debug, PartialEq)]
pub struct SaidLine {
    pub instance: InstanceId,
    pub id: String,
    pub speaker: WireValue,
    pub args: Vec<WireValue>,
}

/// The options the player picks from (a choice's, or the topic menu when `menu`), and the serial a pick
/// must name.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub menu: bool,
    pub options: Vec<SaidLine>,
    pub timeout_at: Option<u64>,
    pub serial: u32,
}

/// An event to deliver to `instance`: `event` is its name ([`EventShape::name`]), `value` its payload (a
/// struct of the event's fields).
#[derive(Clone, Debug, PartialEq)]
pub struct Emitted {
    pub instance: InstanceId,
    pub event: String,
    pub value: WireValue,
}

/// The end of one top-level dialogue run (the picked greeting, or a topic picked from the menu): the host
/// raises `DialogueEnded` from it to `instance`. `Finished` with its exit's name (`done` for a bare
/// `return` or the end of its last node) when it returned to the menu, or when the conversation ended
/// `Finished` inside it (an empty node group); otherwise the conversation's end reason and `done`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub reason: EndReason,
    pub instance: InstanceId,
    pub dialogue: String,
    pub exit: String,
}

/// Why a pick was refused; the offer to show instead.
#[derive(Clone, Debug, PartialEq)]
pub enum PickRefused {
    NoOffer,
    Stale { resent: Offer },
    OutOfRange { resent: Offer },
}

/// A player's conversation with an NPC.
#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    player: WireValue,
    npc: WireValue,
    /// The return stack: the current dialogue last. Empty at the topic menu.
    frames: Vec<Frame>,
    /// The attached dialogues, by (instance, qualified name), sorted.
    attached: Vec<(InstanceId, String)>,
    pending: Option<Pending>,
    /// The serial of the latest offer.
    serial: u32,
    state: ConvState,
    /// Whether a top-level dialogue ran (a hub with nothing on it then ends `Finished`, else it is closed:
    /// nothing to say).
    ran: bool,
    /// Fault sites reported: (instance, fn), or (instance, `u32::from(event) | EMIT_SITE`) for an emit.
    reported: BTreeSet<(InstanceId, u32)>,
    /// The latest tick a call was made at.
    now: u64,
    /// After an edit: what the next tick re-sends (`rebind`).
    reoffer: Option<Reoffer>,
}

/// What a tick after an edit does first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reoffer {
    /// The topic menu, evaluated again.
    Menu,
    /// The choice the conversation waits at, evaluated again, with its timeout.
    Choice(Option<u64>),
    /// Walk on (a node restarted).
    Walk,
}

/// A refused emit's fault site (above any fn id).
const EMIT_SITE: u32 = 1 << 16;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    instance: InstanceId,
    /// The dialogue's index in its graph (re-found by `name` on a rebind).
    dialogue: u16,
    name: String,
    /// The node's group name and its index in the group.
    node: String,
    member: u32,
    /// The blocks being walked: the node's body first, then each nested block by its index among the
    /// blocks of the step before its parent's `next` (`StepDef::blocks`).
    cursors: Vec<Cursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cursor {
    block: Block,
    /// The next step's offset in the block.
    next: u32,
    via: u32,
}

#[derive(Clone, Debug, PartialEq)]
struct Pending {
    offer: Offer,
    what: Waiting,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Waiting {
    /// The topics, in offer order.
    Menu(Vec<(InstanceId, String)>),
    /// The offered options' indices in the choice step (the step before the top cursor's `next`).
    Choice(Vec<u32>),
}

/// The stateless roll the `random_*` strategies break ties with: FNV-1a of the three.
pub fn roll(tick: u64, key: u64, salt: u64) -> u64 {
    let mut h = Fnv64::default();
    h.write_u64(tick);
    h.write_u64(key);
    h.write_u64(salt);
    h.finish()
}

/// The roll key of a dialogue's site: `hash(qualified name, site)`.
fn site_key(dialogue: &str, site: u32) -> u64 {
    let mut h = Fnv64::default();
    dialogue.state_hash(&mut h);
    site.state_hash(&mut h);
    h.finish()
}

fn value_hash(v: &WireValue) -> u64 {
    let mut h = Fnv64::default();
    v.state_hash(&mut h);
    h.finish()
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The dialogue named `name` in `g`.
fn find<'g>(g: &'g DialogueGraph, name: &str) -> Option<(u16, &'g DialogueDef)> {
    g.dialogues
        .iter()
        .enumerate()
        .find(|(_, d)| d.name == name)
        .and_then(|(k, d)| Some((u16::try_from(k).ok()?, d)))
}

/// The members of the node group `group` (the index of a node; its name names the group): node indices.
fn group_members(d: &DialogueDef, group: usize) -> Vec<usize> {
    let Some(name) = d.nodes.get(group).map(|n| &n.name) else {
        return Vec::new();
    };
    (0..d.nodes.len())
        .filter(|&k| &d.nodes[k].name == name)
        .collect()
}

/// A candidate of a group pick.
struct Cand {
    /// Its index in the candidates' source order.
    pos: usize,
    score: u8,
    picks: u32,
    key: u64,
}

/// The candidate `pick` chooses: `First` the first; `Best` the highest score; `BestLeastRecent` then the
/// fewest picks; `RandomBestLeastRecent` then the lowest roll; ties by source order.
fn choose(pick: Pick, now: u64, cands: &[Cand]) -> Option<usize> {
    let rank = |c: &Cand| -> (i32, u32, u64, usize) {
        match pick {
            Pick::First => (0, 0, 0, c.pos),
            Pick::Best => (-i32::from(c.score), 0, 0, c.pos),
            Pick::BestLeastRecent => (-i32::from(c.score), c.picks, 0, c.pos),
            Pick::RandomBestLeastRecent => (
                -i32::from(c.score),
                c.picks,
                roll(now, c.key, c.pos as u64),
                c.pos,
            ),
        }
    };
    cands.iter().min_by_key(|c| rank(c)).map(|c| c.pos)
}

impl Conversation {
    /// The player starts talking to the NPC that `attached` dialogues are attached to: the greeting, then
    /// the topic menu (module docs). `attached` in any order; an entry that names no conversation is
    /// ignored.
    pub fn start(
        env: &mut dyn DialogueEnv,
        attached: &[Attachment],
        rec: &mut Records,
        player: WireValue,
        npc: WireValue,
        now: u64,
    ) -> (Conversation, Batch) {
        let mut c = Conversation {
            player,
            npc,
            frames: Vec::new(),
            attached: Vec::new(),
            pending: None,
            serial: 0,
            state: ConvState::Running,
            ran: false,
            reported: BTreeSet::new(),
            now,
            reoffer: None,
        };
        c.attached = c.normalize(&*env, attached);
        let mut w = Walk::new(&mut c, env, rec);
        if let Some((i, name, node)) = w.greeting() {
            w.enter(i, &name, node);
        }
        w.run();
        let out = w.out;
        (c, out)
    }

    /// The player picks option `option` of the offer `serial`; `attached` is the NPC's attachments now.
    pub fn pick(
        &mut self,
        env: &mut dyn DialogueEnv,
        attached: &[Attachment],
        rec: &mut Records,
        serial: u32,
        option: u32,
        now: u64,
    ) -> Result<Batch, PickRefused> {
        let (ConvState::AwaitingPick, Some(p)) = (&self.state, &self.pending) else {
            return Err(PickRefused::NoOffer);
        };
        if serial != p.offer.serial {
            return Err(PickRefused::Stale {
                resent: p.offer.clone(),
            });
        }
        let Some(k) = usize::try_from(option)
            .ok()
            .filter(|&k| k < p.offer.options.len())
        else {
            return Err(PickRefused::OutOfRange {
                resent: p.offer.clone(),
            });
        };
        let what = p.what.clone();
        self.now = self.now.max(now);
        self.attached = self.normalize(&*env, attached);
        let mut w = Walk::new(self, env, rec);
        match what {
            Waiting::Menu(topics) => {
                let Some((i, name)) = topics.get(k).cloned() else {
                    return Err(PickRefused::NoOffer);
                };
                if !w.topic_eligible(i, &name) {
                    // Detached, or no longer eligible: the menu as it is now.
                    w.c.pending = None;
                    w.c.state = ConvState::Running;
                    w.run();
                    return match (&w.c.state, w.out.offer.take()) {
                        (ConvState::AwaitingPick, Some(resent)) => {
                            Err(PickRefused::Stale { resent })
                        }
                        _ => Ok(w.out),
                    };
                }
                w.c.pending = None;
                w.c.state = ConvState::Running;
                match w.entry(i, &name, 1) {
                    Some(node) => w.enter(i, &name, node),
                    None => w.end(EndReason::Finished),
                }
            }
            Waiting::Choice(options) => {
                w.c.pending = None;
                w.c.state = ConvState::Running;
                match options.get(k) {
                    Some(&o) => w.take_option(o),
                    None => w.end(EndReason::Edited),
                }
            }
        }
        w.run();
        Ok(w.out)
    }

    /// The clock reached `now`: a pause ends, a choice times out, an ended conversation's linger runs out
    /// (`Closed`, no batch). `None` when nothing happened.
    pub fn tick(
        &mut self,
        env: &mut dyn DialogueEnv,
        rec: &mut Records,
        now: u64,
    ) -> Option<Batch> {
        self.now = self.now.max(now);
        match self.state.clone() {
            ConvState::Paused { until } if now >= until => {
                let reoffer = self.reoffer.take();
                let mut w = Walk::new(self, env, rec);
                w.c.state = ConvState::Running;
                match reoffer {
                    Some(Reoffer::Menu) => w.hub(),
                    Some(Reoffer::Choice(at)) => {
                        // A choice that is gone, or offers nothing now, is walked past.
                        if !w.reoffer(at) {
                            w.run();
                        }
                    }
                    Some(Reoffer::Walk) | None => w.run(),
                }
                Some(w.out)
            }
            ConvState::AwaitingPick => {
                let p = self.pending.as_ref()?;
                let due = p.offer.timeout_at.is_some_and(|t| now >= t);
                let Waiting::Choice(options) = &p.what else {
                    return None;
                };
                if !due {
                    return None;
                }
                let options = options.clone();
                let mut w = Walk::new(self, env, rec);
                w.c.pending = None;
                w.c.state = ConvState::Running;
                w.timeout(&options);
                w.run();
                Some(w.out)
            }
            ConvState::Ended { linger_until } if now >= linger_until => {
                self.state = ConvState::Closed;
                None
            }
            _ => None,
        }
    }

    /// The player walked away or closed the menu: the end (`Left`). Nothing once it has ended.
    pub fn leave(&mut self, now: u64) -> Batch {
        self.stop(EndReason::Left, now)
    }

    /// The player and the NPC moved out of talking range: the end (`OutOfRange`).
    pub fn out_of_range(&mut self, now: u64) -> Batch {
        self.stop(EndReason::OutOfRange, now)
    }

    fn stop(&mut self, reason: EndReason, now: u64) -> Batch {
        self.now = self.now.max(now);
        if matches!(self.state, ConvState::Ended { .. } | ConvState::Closed) {
            return Batch::default();
        }
        Batch {
            ended: self.finish(reason).into_iter().collect(),
            ..Batch::default()
        }
    }

    /// The client's `Done`: an ended conversation closes.
    pub fn done(&mut self) {
        if matches!(self.state, ConvState::Ended { .. }) {
            self.state = ConvState::Closed;
        }
    }

    /// After an edit (the env's graphs replaced): each frame finds its dialogue and node again by name. A
    /// dialogue or node that is gone ends the conversation `Edited` (the batch). Otherwise the conversation
    /// continues there: a pending offer is withdrawn and re-sent, evaluated again, at the next [`tick`]
    /// (a choice keeps its timeout; a pick meanwhile is `NoOffer`), and a position that no longer resolves
    /// restarts its node at the next tick. `None` when nothing ended.
    ///
    /// [`tick`]: Conversation::tick
    pub fn rebind(&mut self, env: &mut dyn DialogueEnv) -> Option<Batch> {
        if matches!(self.state, ConvState::Ended { .. } | ConvState::Closed) {
            return None;
        }
        let mut restarted = false;
        let mut gone = false;
        for f in &mut self.frames {
            match rebind_frame(env, f) {
                Rebound::Kept => {}
                Rebound::Restarted => restarted = true,
                Rebound::Gone => gone = true,
            }
        }
        if gone {
            return Some(Batch {
                ended: self.finish(EndReason::Edited).into_iter().collect(),
                ..Batch::default()
            });
        }
        if let Some(p) = self.pending.take() {
            self.reoffer = Some(match p.what {
                Waiting::Menu(_) => Reoffer::Menu,
                Waiting::Choice(_) if !restarted => Reoffer::Choice(p.offer.timeout_at),
                // Its node restarts: the walk reaches the choice again.
                Waiting::Choice(_) => Reoffer::Walk,
            });
        } else if restarted && self.reoffer.is_none() {
            self.reoffer = Some(Reoffer::Walk);
        }
        if self.reoffer.is_some() {
            self.state = ConvState::Paused { until: self.now };
        }
        None
    }

    pub fn state(&self) -> &ConvState {
        &self.state
    }

    /// The offer out, if any.
    pub fn offer(&self) -> Option<&Offer> {
        self.pending.as_ref().map(|p| &p.offer)
    }

    /// The current detour depth (0 at the topic menu).
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// `attached` as (instance, qualified name) of conversations, sorted and deduplicated.
    fn normalize(
        &self,
        env: &dyn DialogueEnv,
        attached: &[Attachment],
    ) -> Vec<(InstanceId, String)> {
        let mut out: Vec<(InstanceId, String)> = attached
            .iter()
            .filter_map(|a| {
                let d = env
                    .graph(a.instance)?
                    .dialogues
                    .get(usize::from(a.dialogue))?;
                (d.kind == DialogueKind::Conversation && d.cast == 2)
                    .then(|| (a.instance, d.name.clone()))
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Ends the conversation: the [`Ended`] of the top-level dialogue running, if one is.
    fn finish(&mut self, reason: EndReason) -> Option<Ended> {
        let ended = self.frames.first().map(|f| Ended {
            reason,
            instance: f.instance,
            dialogue: f.name.clone(),
            exit: "done".into(),
        });
        self.frames.clear();
        self.pending = None;
        self.state = ConvState::Ended {
            linger_until: self.now.saturating_add(CONVERSATION_LINGER_TICKS),
        };
        ended
    }
}

enum Rebound {
    Kept,
    Restarted,
    Gone,
}

/// Finds frame `f`'s dialogue, node and blocks again in its instance's (edited) graph.
fn rebind_frame(env: &dyn DialogueEnv, f: &mut Frame) -> Rebound {
    let Some(g) = env.graph(f.instance) else {
        return Rebound::Gone;
    };
    let Some((di, d)) = find(g, &f.name) else {
        return Rebound::Gone;
    };
    let Some(node) = d
        .nodes
        .iter()
        .filter(|n| n.name == f.node)
        .nth(usize::try_from(f.member).unwrap_or(usize::MAX))
    else {
        return Rebound::Gone;
    };
    f.dialogue = di;
    let mut blocks = vec![node.body];
    for k in 1..f.cursors.len() {
        let parent = f.cursors[k - 1];
        let child = parent
            .next
            .checked_sub(1)
            .and_then(|o| blocks[k - 1].first.checked_add(o))
            .and_then(|s| g.steps.get(usize::try_from(s).ok()?))
            .and_then(|s| {
                s.blocks()
                    .get(usize::try_from(f.cursors[k].via).ok()?)
                    .copied()
            });
        match child {
            Some(b) => blocks.push(b),
            None => break,
        }
    }
    let fits = blocks.len() == f.cursors.len()
        && blocks
            .iter()
            .zip(&f.cursors)
            .all(|(b, c)| c.next <= b.len && b.range(g.steps.len()).is_some());
    if fits {
        for (c, b) in f.cursors.iter_mut().zip(blocks) {
            c.block = b;
        }
        Rebound::Kept
    } else {
        f.cursors = vec![Cursor {
            block: node.body,
            next: 0,
            via: 0,
        }];
        Rebound::Restarted
    }
}

/// One walk: a call's work on a conversation.
struct Walk<'a> {
    c: &'a mut Conversation,
    env: &'a mut dyn DialogueEnv,
    rec: &'a mut Records,
    out: Batch,
    steps: u32,
}

/// What the walk of one step decided.
enum Flow {
    Next,
    Stop,
}

impl<'a> Walk<'a> {
    fn new(
        c: &'a mut Conversation,
        env: &'a mut dyn DialogueEnv,
        rec: &'a mut Records,
    ) -> Walk<'a> {
        Walk {
            c,
            env,
            rec,
            out: Batch::default(),
            steps: 0,
        }
    }

    fn now(&self) -> u64 {
        self.c.now
    }

    fn def(&self, i: InstanceId, name: &str) -> Option<(u16, DialogueDef)> {
        let g = self.env.graph(i)?;
        find(g, name).map(|(k, d)| (k, d.clone()))
    }

    // ---- evaluation ------------------------------------------------------------------------------------

    fn fault(&mut self, i: InstanceId, site: u32, fault: PureFault) {
        if self.c.reported.insert((i, site)) {
            self.env.report(i, fault);
        }
    }

    /// Fn `f` of instance `i` on the cast; `None` (reported) on a fault.
    fn eval(&mut self, i: InstanceId, f: u16, depth: usize) -> Option<WireValue> {
        let args = [self.c.player.clone(), self.c.npc.clone()];
        match self.env.eval(i, f, &args, len_u32(depth)) {
            Ok(v) => Some(v),
            Err(e) => {
                self.fault(i, u32::from(f), e);
                None
            }
        }
    }

    fn truth(&mut self, i: InstanceId, f: u16, depth: usize) -> bool {
        match self.eval(i, f, depth) {
            Some(WireValue::Bool(b)) => b,
            Some(_) => {
                self.fault(i, u32::from(f), PureFault::BadCall);
                false
            }
            None => false,
        }
    }

    /// Whether `cond` holds in dialogue `d` of `i`; `Some(once record to make when it is taken)`.
    fn holds(
        &mut self,
        i: InstanceId,
        d: &str,
        cond: &CondDef,
        depth: usize,
    ) -> Option<Option<OnceRec>> {
        let once = match &cond.once {
            Some(o) => {
                let per = match o.per {
                    Some(f) => Some(value_hash(&self.eval(i, f, depth)?)),
                    None => None,
                };
                if self.rec.once_used(i, d, o.site, per) {
                    return None;
                }
                Some((o.site, per))
            }
            None => None,
        };
        if let Some(g) = cond.guard
            && !self.truth(i, g, depth)
        {
            return None;
        }
        Some(once)
    }

    fn mark(&mut self, i: InstanceId, d: &str, once: Option<OnceRec>) {
        if let Some(k) = once {
            self.rec.of_mut(i, d).once.insert(k);
        }
    }

    /// Line `l` of instance `i`, its speaker and substitutions evaluated now.
    fn line(&mut self, i: InstanceId, l: &LineDef, depth: usize) -> SaidLine {
        let speaker = match l.speaker {
            Speaker::Cast(0) => self.c.player.clone(),
            Speaker::Cast(1) => self.c.npc.clone(),
            Speaker::Cast(_) => WireValue::Unit,
            Speaker::Fn(f) => self.eval(i, f, depth).unwrap_or_default(),
        };
        let args = l
            .subst
            .iter()
            .map(|&f| {
                self.eval(i, f, depth)
                    .unwrap_or_else(|| WireValue::Str(String::new()))
            })
            .collect();
        SaidLine {
            instance: i,
            id: l.id.clone(),
            speaker,
            args,
        }
    }

    // ---- greetings, topics, entries ----------------------------------------------------------------

    /// The node a dialogue's group `group` picks by its `@pick` (`None`: an empty group), evaluated at
    /// `depth`; its once record and pick are made.
    fn pick_node(
        &mut self,
        i: InstanceId,
        d: &DialogueDef,
        group: usize,
        depth: usize,
    ) -> Option<usize> {
        let members = group_members(d, group);
        let name = d.nodes.get(group)?.name.clone();
        let mut cands = Vec::new();
        let mut onces = Vec::new();
        for (pos, &n) in members.iter().enumerate() {
            let node = &d.nodes[n];
            if let Some(once) = self.holds(i, &d.name, &node.cond, depth) {
                let key = PickKey::Node {
                    name: name.clone(),
                    member: len_u32(pos),
                };
                cands.push(Cand {
                    pos,
                    score: node.score,
                    picks: self.rec.picks(i, &d.name, &key),
                    key: site_key(&d.name, len_u32(n)),
                });
                onces.push((pos, once));
            }
            if d.pick == Pick::First && !cands.is_empty() {
                break;
            }
        }
        let pos = choose(d.pick, self.now(), &cands)?;
        let once = onces.iter().find(|(p, _)| *p == pos).and_then(|(_, o)| *o);
        self.mark(i, &d.name, once);
        self.rec.picked(
            i,
            &d.name,
            PickKey::Node {
                name,
                member: len_u32(pos),
            },
        );
        members.get(pos).copied()
    }

    /// The entry node of dialogue `name` of `i` (its first node's group), entered at `depth`.
    fn entry(&mut self, i: InstanceId, name: &str, depth: usize) -> Option<usize> {
        let (_, d) = self.def(i, name)?;
        self.pick_node(i, &d, 0, depth)
    }

    /// The greeting pool's pick (module docs): (instance, dialogue, node).
    fn greeting(&mut self) -> Option<(InstanceId, String, usize)> {
        let mut best: Option<(GreetRank, Option<OnceRec>)> = None;
        for (i, name) in self.c.attached.clone() {
            let Some((_, d)) = self.def(i, &name) else {
                continue;
            };
            if d.topic.is_some() {
                continue;
            }
            for (pos, n) in group_members(&d, 0).into_iter().enumerate() {
                let node = &d.nodes[n];
                let Some(once) = self.holds(i, &d.name, &node.cond, 1) else {
                    continue;
                };
                let key = PickKey::Node {
                    name: node.name.clone(),
                    member: len_u32(pos),
                };
                let rank = (
                    -i32::from(node.score),
                    self.rec.picks(i, &d.name, &key),
                    d.name.clone(),
                    n,
                    i,
                );
                if best.as_ref().is_none_or(|(b, _)| rank < *b) {
                    best = Some((rank, once));
                }
            }
        }
        let ((_, _, name, n, i), once) = best?;
        let (_, d) = self.def(i, &name)?;
        let node = d.nodes.get(n)?;
        let member = group_members(&d, 0)
            .iter()
            .position(|&k| k == n)
            .unwrap_or(0);
        self.mark(i, &name, once);
        self.rec.picked(
            i,
            &name,
            PickKey::Node {
                name: node.name.clone(),
                member: len_u32(member),
            },
        );
        Some((i, name, n))
    }

    /// Whether the attached topic `name` of `i` is on the menu now.
    fn topic_eligible(&mut self, i: InstanceId, name: &str) -> bool {
        if !self.c.attached.iter().any(|(j, n)| *j == i && n == name) {
            return false;
        }
        let Some((_, d)) = self.def(i, name) else {
            return false;
        };
        match &d.topic {
            Some(t) => t.cond.is_none_or(|f| self.truth(i, f, 1)),
            None => false,
        }
    }

    /// At the hub: the topic menu, or the end when it is empty.
    fn hub(&mut self) {
        let mut topics = Vec::new();
        for (i, name) in self.c.attached.clone() {
            let Some((_, d)) = self.def(i, &name) else {
                continue;
            };
            let Some(t) = &d.topic else {
                continue;
            };
            if t.cond.is_none_or(|f| self.truth(i, f, 1)) {
                topics.push((t.priority, name, i, t.text.clone()));
            }
        }
        topics.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| (&a.1, a.2).cmp(&(&b.1, b.2))));
        if topics.is_empty() {
            if !self.c.ran {
                // Neither a greeting nor a topic: nothing to say.
                self.c.state = ConvState::Closed;
            } else {
                self.end(EndReason::Finished);
            }
            return;
        }
        let options = topics
            .iter()
            .map(|(_, _, i, text)| self.line(*i, text, 1))
            .collect();
        let keys = topics.into_iter().map(|(_, n, i, _)| (i, n)).collect();
        self.offer(true, options, None, Waiting::Menu(keys));
    }

    fn offer(
        &mut self,
        menu: bool,
        options: Vec<SaidLine>,
        timeout_at: Option<u64>,
        what: Waiting,
    ) {
        self.c.serial = self.c.serial.wrapping_add(1);
        let offer = Offer {
            menu,
            options,
            timeout_at,
            serial: self.c.serial,
        };
        self.out.offer = Some(offer.clone());
        self.c.pending = Some(Pending { offer, what });
        self.c.state = ConvState::AwaitingPick;
    }

    /// Pushes a frame running node `node` of dialogue `name` of `i`.
    fn enter(&mut self, i: InstanceId, name: &str, node: usize) {
        let Some((di, d)) = self.def(i, name) else {
            return self.end(EndReason::Edited);
        };
        let Some(n) = d.nodes.get(node) else {
            return self.end(EndReason::Edited);
        };
        let member = d.nodes[..node].iter().filter(|m| m.name == n.name).count();
        self.c.frames.push(Frame {
            instance: i,
            dialogue: di,
            name: d.name.clone(),
            node: n.name.clone(),
            member: len_u32(member),
            cursors: vec![Cursor {
                block: n.body,
                next: 0,
                via: 0,
            }],
        });
    }

    // ---- the walk ------------------------------------------------------------------------------------

    /// Ends the conversation (the batch carries the end).
    fn end(&mut self, reason: EndReason) {
        let ended = self.c.finish(reason);
        self.out.ended.extend(ended);
    }

    /// Walks until an offer, a pause or the end.
    fn run(&mut self) {
        while self.c.state == ConvState::Running {
            if self.steps >= BATCH_STEPS_MAX {
                return self.end(EndReason::TooDeep);
            }
            self.steps += 1;
            if let Flow::Stop = self.step() {
                return;
            }
        }
    }

    /// The step after the top cursor's `next` (the step a nested block belongs to), cloned.
    fn step_at(&self, i: InstanceId, block: Block, offset: u32) -> Option<StepDef> {
        let g = self.env.graph(i)?;
        let s = usize::try_from(block.first.checked_add(offset)?).ok()?;
        g.steps.get(s).cloned()
    }

    fn push(&mut self, block: Block, via: usize) {
        if let Some(f) = self.c.frames.last_mut() {
            f.cursors.push(Cursor {
                block,
                next: 0,
                via: len_u32(via),
            });
        }
    }

    fn step(&mut self) -> Flow {
        let depth = self.c.frames.len();
        let Some(f) = self.c.frames.last_mut() else {
            self.hub();
            return Flow::Stop;
        };
        let (i, name) = (f.instance, f.name.clone());
        let Some(cur) = f.cursors.last_mut() else {
            self.leave_dialogue(0);
            return Flow::Next;
        };
        if cur.next >= cur.block.len {
            f.cursors.pop();
            return Flow::Next;
        }
        let (block, offset) = (cur.block, cur.next);
        cur.next += 1;
        let Some(step) = self.step_at(i, block, offset) else {
            self.end(EndReason::Edited);
            return Flow::Stop;
        };
        match step {
            StepDef::Line { line, cond } => {
                if let Some(once) = self.holds(i, &name, &cond, depth) {
                    self.mark(i, &name, once);
                    let l = self.line(i, &line, depth);
                    self.out.lines.push(l);
                }
            }
            StepDef::Group { site, members } => {
                let mut cands = Vec::new();
                let mut onces = Vec::new();
                let pick = self
                    .def(i, &name)
                    .map_or(Pick::BestLeastRecent, |d| d.1.pick);
                for (pos, m) in members.iter().enumerate() {
                    if let Some(once) = self.holds(i, &name, &m.cond, depth) {
                        let key = PickKey::Line {
                            site,
                            member: len_u32(pos),
                        };
                        cands.push(Cand {
                            pos,
                            score: m.score,
                            picks: self.rec.picks(i, &name, &key),
                            key: site_key(&name, site),
                        });
                        onces.push((pos, once));
                        if pick == Pick::First {
                            break;
                        }
                    }
                }
                if let Some(pos) = choose(pick, self.now(), &cands) {
                    let once = onces.iter().find(|(p, _)| *p == pos).and_then(|(_, o)| *o);
                    self.mark(i, &name, once);
                    self.rec.picked(
                        i,
                        &name,
                        PickKey::Line {
                            site,
                            member: len_u32(pos),
                        },
                    );
                    let m = &members[pos];
                    let l = self.line(i, &m.line, depth);
                    self.out.lines.push(l);
                    self.push(m.body, pos);
                }
            }
            StepDef::Choice {
                timeout, options, ..
            } => {
                let mut offered = Vec::new();
                let mut lines = Vec::new();
                for (k, o) in options.iter().enumerate() {
                    if o.once
                        .is_some_and(|s| self.rec.once_used(i, &name, s, None))
                    {
                        continue;
                    }
                    if o.guard.is_some_and(|g| !self.truth(i, g, depth)) {
                        continue;
                    }
                    lines.push(self.line(i, &o.text, depth));
                    offered.push(len_u32(k));
                }
                if !offered.is_empty() {
                    let at = timeout.map(|t| self.now().saturating_add(u64::from(t)));
                    self.offer(false, lines, at, Waiting::Choice(offered));
                    return Flow::Stop;
                }
            }
            StepDef::When { cond, body } => {
                if let Some(once) = self.holds(i, &name, &cond, depth) {
                    self.mark(i, &name, once);
                    self.push(body, 0);
                }
            }
            StepDef::Once { once, body } => {
                let cond = CondDef {
                    once: Some(once),
                    guard: None,
                };
                if let Some(once) = self.holds(i, &name, &cond, depth) {
                    self.mark(i, &name, once);
                    self.push(body, 0);
                }
            }
            StepDef::Jump { group } => {
                let Some((_, d)) = self.def(i, &name) else {
                    self.end(EndReason::Edited);
                    return Flow::Stop;
                };
                match self.pick_node(i, &d, usize::from(group), depth) {
                    Some(n) => {
                        self.c.frames.pop();
                        self.enter(i, &name, n);
                    }
                    None => {
                        self.end(EndReason::Finished);
                        return Flow::Stop;
                    }
                }
            }
            StepDef::Detour { dialogue, .. } => {
                if depth >= DIALOGUE_STACK_MAX {
                    self.end(EndReason::TooDeep);
                    return Flow::Stop;
                }
                let callee = self
                    .env
                    .graph(i)
                    .and_then(|g| g.dialogues.get(usize::from(dialogue)))
                    .map(|d| d.name.clone());
                let Some(callee) = callee else {
                    self.end(EndReason::Edited);
                    return Flow::Stop;
                };
                match self.entry(i, &callee, depth + 1) {
                    Some(n) => self.enter(i, &callee, n),
                    None => {
                        self.end(EndReason::Finished);
                        return Flow::Stop;
                    }
                }
            }
            StepDef::Emit {
                event,
                script,
                args,
            } => self.emit(i, event, script, &args, depth),
            StepDef::Return { exit } => self.leave_dialogue(exit),
            StepDef::Pause { ticks } => {
                self.c.state = ConvState::Paused {
                    until: self.now().saturating_add(u64::from(ticks)),
                };
                return Flow::Stop;
            }
            // A scene's step: nothing in a conversation.
            StepDef::FollowUp { .. } => {}
        }
        Flow::Next
    }

    fn emit(&mut self, i: InstanceId, event: u16, script: bool, args: &[u16], depth: usize) {
        let shape = self
            .env
            .event(i, event, script)
            .filter(|s| s.fields.len() == args.len());
        let Some(shape) = shape else {
            return self.fault(i, EMIT_SITE | u32::from(event), PureFault::BadCall);
        };
        let mut fields = Vec::with_capacity(args.len());
        for (name, &f) in shape.fields.iter().zip(args) {
            match self.eval(i, f, depth) {
                Some(v) => fields.push((name.clone(), v)),
                // Reported: the emit is not sent.
                None => return,
            }
        }
        self.out.emits.push(Emitted {
            instance: i,
            event: shape.name.clone(),
            value: WireValue::Struct {
                name: shape.name,
                fields,
            },
        });
    }

    /// The current dialogue leaves through exit `exit`: back to the caller's detour (its arm), or to the
    /// hub.
    fn leave_dialogue(&mut self, exit: u16) {
        let Some(f) = self.c.frames.pop() else {
            return;
        };
        let exit_name = self
            .def(f.instance, &f.name)
            .and_then(|(_, d)| d.exits.get(usize::from(exit)).cloned())
            .unwrap_or_else(|| "done".into());
        let Some(caller) = self.c.frames.last() else {
            // A top-level run ends: back to the menu.
            self.c.ran = true;
            self.out.ended.push(Ended {
                reason: EndReason::Finished,
                instance: f.instance,
                dialogue: f.name,
                exit: exit_name,
            });
            return;
        };
        let Some(cur) = caller.cursors.last().copied() else {
            return;
        };
        let Some(StepDef::Detour { arms, .. }) = cur
            .next
            .checked_sub(1)
            .and_then(|o| self.step_at(caller.instance, cur.block, o))
        else {
            return;
        };
        if let Some(pos) = arms.iter().position(|a| a.0 == exit) {
            self.push(arms[pos].1, pos);
        }
    }

    /// The offered option `o` of the choice (the step before the top cursor's `next`) is taken.
    fn take_option(&mut self, o: u32) {
        let Some((i, name, choice)) = self.current_choice() else {
            return self.end(EndReason::Edited);
        };
        let StepDef::Choice { options, .. } = choice else {
            return self.end(EndReason::Edited);
        };
        let Some(opt) = usize::try_from(o).ok().and_then(|k| options.get(k)) else {
            return self.end(EndReason::Edited);
        };
        if let Some(site) = opt.once {
            self.mark(i, &name, Some((site, None)));
        }
        self.push(opt.body, usize::try_from(o).unwrap_or(usize::MAX));
    }

    /// (instance, dialogue, the step) of the choice the conversation waits at.
    fn current_choice(&self) -> Option<(InstanceId, String, StepDef)> {
        let f = self.c.frames.last()?;
        let cur = f.cursors.last()?;
        let s = self.step_at(f.instance, cur.block, cur.next.checked_sub(1)?)?;
        Some((f.instance, f.name.clone(), s))
    }

    /// The offered choice timed out.
    fn timeout(&mut self, offered: &[u32]) {
        let Some((
            _,
            _,
            StepDef::Choice {
                options, fallback, ..
            },
        )) = self.current_choice()
        else {
            return self.end(EndReason::Edited);
        };
        let default = offered.iter().copied().find(|&k| {
            usize::try_from(k)
                .ok()
                .and_then(|k| options.get(k))
                .is_some_and(|o| o.default)
        });
        match (default, fallback) {
            (Some(k), _) => self.take_option(k),
            (None, Some(b)) => self.push(b, options.len()),
            (None, None) => self.end(EndReason::Timeout),
        }
    }

    /// Re-offers the choice the conversation waits at (after an edit); `false` when it is no choice.
    fn reoffer(&mut self, timeout_at: Option<u64>) -> bool {
        let Some((i, name, StepDef::Choice { options, .. })) = self.current_choice() else {
            return false;
        };
        let depth = self.c.frames.len();
        let mut offered = Vec::new();
        let mut lines = Vec::new();
        for (k, o) in options.iter().enumerate() {
            if o.once
                .is_some_and(|s| self.rec.once_used(i, &name, s, None))
            {
                continue;
            }
            if o.guard.is_some_and(|g| !self.truth(i, g, depth)) {
                continue;
            }
            lines.push(self.line(i, &o.text, depth));
            offered.push(len_u32(k));
        }
        if offered.is_empty() {
            return false;
        }
        self.offer(false, lines, timeout_at, Waiting::Choice(offered));
        true
    }
}

// ---- StateHash -----------------------------------------------------------------------------------------

impl StateHash for InstanceId {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.0.state_hash(h);
    }
}

impl StateHash for Records {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.by.state_hash(h);
    }
}

impl StateHash for DialogueRecords {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.once.state_hash(h);
        self.picks.state_hash(h);
    }
}

impl StateHash for PickKey {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        match self {
            PickKey::Node { name, member } => {
                h.write_u8(0);
                name.state_hash(h);
                member.state_hash(h);
            }
            PickKey::Line { site, member } => {
                h.write_u8(1);
                site.state_hash(h);
                member.state_hash(h);
            }
        }
    }
}

impl StateHash for EndReason {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u8(match self {
            EndReason::Finished => 0,
            EndReason::Left => 1,
            EndReason::Timeout => 2,
            EndReason::TooDeep => 3,
            EndReason::Edited => 4,
            EndReason::OutOfRange => 5,
        });
    }
}

impl StateHash for ConvState {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        match self {
            ConvState::Running => h.write_u8(0),
            ConvState::AwaitingPick => h.write_u8(1),
            ConvState::Paused { until } => {
                h.write_u8(2);
                until.state_hash(h);
            }
            ConvState::Ended { linger_until } => {
                h.write_u8(3);
                linger_until.state_hash(h);
            }
            ConvState::Closed => h.write_u8(4),
        }
    }
}

impl StateHash for Batch {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.lines.state_hash(h);
        self.offer.state_hash(h);
        self.emits.state_hash(h);
        self.ended.state_hash(h);
    }
}

impl StateHash for SaidLine {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.instance.state_hash(h);
        self.id.state_hash(h);
        self.speaker.state_hash(h);
        self.args.state_hash(h);
    }
}

impl StateHash for Offer {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.menu.state_hash(h);
        self.options.state_hash(h);
        self.timeout_at.state_hash(h);
        self.serial.state_hash(h);
    }
}

impl StateHash for Emitted {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.instance.state_hash(h);
        self.event.state_hash(h);
        self.value.state_hash(h);
    }
}

impl StateHash for Ended {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.reason.state_hash(h);
        self.instance.state_hash(h);
        self.dialogue.state_hash(h);
        self.exit.state_hash(h);
    }
}

impl StateHash for Conversation {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.player.state_hash(h);
        self.npc.state_hash(h);
        self.frames.state_hash(h);
        self.attached.state_hash(h);
        self.pending.state_hash(h);
        self.serial.state_hash(h);
        self.state.state_hash(h);
        self.ran.state_hash(h);
        self.reported.state_hash(h);
        self.now.state_hash(h);
        self.reoffer.state_hash(h);
    }
}

impl StateHash for Frame {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.instance.state_hash(h);
        self.dialogue.state_hash(h);
        self.name.state_hash(h);
        self.node.state_hash(h);
        self.member.state_hash(h);
        self.cursors.state_hash(h);
    }
}

impl StateHash for Cursor {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.block.state_hash(h);
        self.next.state_hash(h);
        self.via.state_hash(h);
    }
}

impl StateHash for Pending {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.offer.state_hash(h);
        match &self.what {
            Waiting::Menu(topics) => {
                h.write_u8(0);
                topics.state_hash(h);
            }
            Waiting::Choice(options) => {
                h.write_u8(1);
                options.state_hash(h);
            }
        }
    }
}

impl StateHash for Reoffer {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        match self {
            Reoffer::Menu => h.write_u8(0),
            Reoffer::Choice(at) => {
                h.write_u8(1);
                at.state_hash(h);
            }
            Reoffer::Walk => h.write_u8(2),
        }
    }
}
