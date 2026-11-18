//! What the conversation runner (`dialogue_run`) and the scene runner (`scene_run`) share: the NPC's
//! [`Records`], the stateless [`roll`], the pick of a group member by `@pick`, a walk's position
//! ([`Frame`]), and the steps both walk alike (lines, line groups, `when`, `once`, `emit`), evaluated
//! through an [`Eval`] over a cast.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;

use state_hash::StateHash;

use crate::dialogue::{
    Block, CondDef, DialogueDef, DialogueGraph, LineDef, MemberDef, Pick, Speaker, StepDef,
};
use crate::dialogue_run::{DialogueEnv, Emitted, InstanceId, SaidLine};
use crate::hash::Fnv64;
use crate::pure::PureFault;
use crate::wire::WireValue;

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
pub(crate) type OnceRec = (u32, Option<u64>);

/// A group member: a node by its group's name and its index in the group, or a line group's member.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PickKey {
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

    pub(crate) fn once_used(&self, i: InstanceId, d: &str, site: u32, per: Option<u64>) -> bool {
        self.of(i, d).is_some_and(|r| r.once.contains(&(site, per)))
    }

    pub(crate) fn picks(&self, i: InstanceId, d: &str, key: &PickKey) -> u32 {
        self.of(i, d)
            .and_then(|r| r.picks.get(key))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn picked(&mut self, i: InstanceId, d: &str, key: PickKey) {
        let n = self.of_mut(i, d).picks.entry(key).or_insert(0);
        *n = n.saturating_add(1);
    }

    pub(crate) fn mark(&mut self, i: InstanceId, d: &str, once: Option<OnceRec>) {
        if let Some(k) = once {
            self.of_mut(i, d).once.insert(k);
        }
    }
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
pub(crate) fn site_key(dialogue: &str, site: u32) -> u64 {
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

pub(crate) fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The dialogue named `name` in `g`.
pub(crate) fn find<'g>(g: &'g DialogueGraph, name: &str) -> Option<(u16, &'g DialogueDef)> {
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

/// `f` of dialogue `name` of `i`, borrowed (nothing cloned but what `f` returns).
pub(crate) fn with_def<R>(
    env: &dyn DialogueEnv,
    i: InstanceId,
    name: &str,
    f: impl FnOnce(u16, &DialogueDef) -> R,
) -> Option<R> {
    let g = env.graph(i)?;
    find(g, name).map(|(k, d)| f(k, d))
}

/// A node group, as a pick of it needs it: the dialogue's qualified name and `@pick`, the group's name,
/// and each member's node index, condition and score.
#[derive(Clone)]
pub(crate) struct Group {
    pub dialogue: String,
    pub pick: Pick,
    pub name: String,
    pub members: Vec<(usize, CondDef, u8)>,
}

/// The node group `group` of dialogue `name` of `i`.
pub(crate) fn group(
    env: &dyn DialogueEnv,
    i: InstanceId,
    name: &str,
    group: usize,
) -> Option<Group> {
    with_def(env, i, name, |_, d| {
        Some(Group {
            dialogue: d.name.clone(),
            pick: d.pick,
            name: d.nodes.get(group)?.name.clone(),
            members: group_members(d, group)
                .into_iter()
                .map(|n| (n, d.nodes[n].cond.clone(), d.nodes[n].score))
                .collect(),
        })
    })
    .flatten()
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

/// A refused emit's fault site (above any fn id).
pub(crate) const EMIT_SITE: u32 = 1 << 16;

/// Where a walk is inside one dialogue: its node and the blocks being walked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub instance: InstanceId,
    /// The dialogue's index in its graph (re-found by `name` on a rebind).
    pub dialogue: u16,
    pub name: String,
    /// The node's group name and its index in the group.
    pub node: String,
    pub member: u32,
    /// The blocks being walked: the node's body first, then each nested block by its index among the
    /// blocks of the step before its parent's `next` (`StepDef::blocks`).
    pub cursors: Vec<Cursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub block: Block,
    /// The next step's offset in the block.
    pub next: u32,
    pub via: u32,
}

impl Frame {
    /// A frame at the start of node `node` of dialogue `name` (index `di`) of `i`.
    pub(crate) fn at(i: InstanceId, di: u16, d: &DialogueDef, node: usize) -> Option<Frame> {
        let n = d.nodes.get(node)?;
        let member = d.nodes[..node].iter().filter(|m| m.name == n.name).count();
        Some(Frame {
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
        })
    }

    pub(crate) fn push(&mut self, block: Block, via: usize) {
        self.cursors.push(Cursor {
            block,
            next: 0,
            via: len_u32(via),
        });
    }
}

/// Step `offset` of `block` in `i`'s graph, cloned.
pub(crate) fn step_at(
    env: &dyn DialogueEnv,
    i: InstanceId,
    block: Block,
    offset: u32,
) -> Option<StepDef> {
    let g = env.graph(i)?;
    let s = usize::try_from(block.first.checked_add(offset)?).ok()?;
    g.steps.get(s).cloned()
}

/// What [`advance`] found.
pub(crate) enum Advance {
    /// The next step (the cursor moved past it).
    Step(StepDef),
    /// The innermost block ended (its cursor popped).
    Popped,
    /// The frame has no block left: its node ended.
    Empty,
    /// The step is not in the graph (an edit).
    Missing,
}

/// Moves `f` on by one step.
pub(crate) fn advance(env: &dyn DialogueEnv, f: &mut Frame) -> Advance {
    let Some(cur) = f.cursors.last_mut() else {
        return Advance::Empty;
    };
    if cur.next >= cur.block.len {
        f.cursors.pop();
        return Advance::Popped;
    }
    let (block, offset) = (cur.block, cur.next);
    cur.next += 1;
    match step_at(env, f.instance, block, offset) {
        Some(s) => Advance::Step(s),
        None => Advance::Missing,
    }
}

/// What [`Eval::common_step`] did.
pub(crate) enum Common {
    /// A line was said (with its duration in ticks).
    Said(u32),
    /// Nothing to wait for.
    Next,
    /// Not a step both runners walk alike: the caller's.
    Other(StepDef),
}

/// Evaluation over a cast: the env, the NPC's (or lead's) records, the fault sites already reported.
pub(crate) struct Eval<'s> {
    pub env: &'s mut dyn DialogueEnv,
    pub rec: &'s mut Records,
    pub cast: &'s [WireValue],
    pub reported: &'s mut BTreeSet<(InstanceId, u32)>,
    pub now: u64,
}

impl Eval<'_> {
    /// Reports `fault` once per fault site.
    pub(crate) fn fault(&mut self, i: InstanceId, site: u32, fault: PureFault) {
        if self.reported.insert((i, site)) {
            self.env.report(i, fault);
        }
    }

    /// Fn `f` of instance `i` on the cast; `None` (reported) on a fault.
    pub(crate) fn eval(&mut self, i: InstanceId, f: u16, depth: usize) -> Option<WireValue> {
        match self.env.eval(i, f, self.cast, len_u32(depth)) {
            Ok(v) => Some(v),
            Err(e) => {
                self.fault(i, u32::from(f), e);
                None
            }
        }
    }

    pub(crate) fn truth(&mut self, i: InstanceId, f: u16, depth: usize) -> bool {
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
    pub(crate) fn holds(
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

    /// Line `l` of instance `i`, its speaker and substitutions evaluated now.
    pub(crate) fn line(&mut self, i: InstanceId, l: &LineDef, depth: usize) -> SaidLine {
        let speaker = match l.speaker {
            Speaker::Cast(k) => self.cast.get(usize::from(k)).cloned().unwrap_or_default(),
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

    /// The node a dialogue's group `g` picks by its `@pick` (`None`: an empty group), evaluated at
    /// `depth`; its once record and pick are made.
    pub(crate) fn pick_node(&mut self, i: InstanceId, g: &Group, depth: usize) -> Option<usize> {
        let d = &g.dialogue;
        let mut cands = Vec::new();
        let mut onces = Vec::new();
        for (pos, (n, cond, score)) in g.members.iter().enumerate() {
            if let Some(once) = self.holds(i, d, cond, depth) {
                let key = PickKey::Node {
                    name: g.name.clone(),
                    member: len_u32(pos),
                };
                cands.push(Cand {
                    pos,
                    score: *score,
                    picks: self.rec.picks(i, d, &key),
                    key: site_key(d, len_u32(*n)),
                });
                onces.push((pos, once));
            }
            if g.pick == Pick::First && !cands.is_empty() {
                break;
            }
        }
        let pos = choose(g.pick, self.now, &cands)?;
        let once = onces.iter().find(|(p, _)| *p == pos).and_then(|(_, o)| *o);
        self.rec.mark(i, d, once);
        self.rec.picked(
            i,
            d,
            PickKey::Node {
                name: g.name.clone(),
                member: len_u32(pos),
            },
        );
        g.members.get(pos).map(|m| m.0)
    }

    /// The entry node of dialogue `name` of `i` (its first node's group), entered at `depth`.
    pub(crate) fn entry(&mut self, i: InstanceId, name: &str, depth: usize) -> Option<usize> {
        let g = group(&*self.env, i, name, 0)?;
        self.pick_node(i, &g, depth)
    }

    /// The member of the line group at `site` of dialogue `name` that the dialogue's `@pick` picks
    /// (`None`: no member holds); its once record and pick are made.
    fn pick_member(
        &mut self,
        i: InstanceId,
        name: &str,
        site: u32,
        members: &[MemberDef],
        depth: usize,
    ) -> Option<usize> {
        let pick = with_def(&*self.env, i, name, |_, d| d.pick).unwrap_or(Pick::BestLeastRecent);
        let mut cands = Vec::new();
        let mut onces = Vec::new();
        for (pos, m) in members.iter().enumerate() {
            if let Some(once) = self.holds(i, name, &m.cond, depth) {
                let key = PickKey::Line {
                    site,
                    member: len_u32(pos),
                };
                cands.push(Cand {
                    pos,
                    score: m.score,
                    picks: self.rec.picks(i, name, &key),
                    key: site_key(name, site),
                });
                onces.push((pos, once));
                if pick == Pick::First {
                    break;
                }
            }
        }
        let pos = choose(pick, self.now, &cands)?;
        let once = onces.iter().find(|(p, _)| *p == pos).and_then(|(_, o)| *o);
        self.rec.mark(i, name, once);
        self.rec.picked(
            i,
            name,
            PickKey::Line {
                site,
                member: len_u32(pos),
            },
        );
        Some(pos)
    }

    /// The `emit` of `event` of `i` with its args evaluated now; `None` when refused or an arg faulted
    /// (reported).
    pub(crate) fn emit(
        &mut self,
        i: InstanceId,
        event: u16,
        script: bool,
        args: &[u16],
        depth: usize,
    ) -> Option<Emitted> {
        let shape = self
            .env
            .event(i, event, script)
            .filter(|s| s.fields.len() == args.len());
        let Some(shape) = shape else {
            self.fault(i, EMIT_SITE | u32::from(event), PureFault::BadCall);
            return None;
        };
        let mut fields = Vec::with_capacity(args.len());
        for (name, &f) in shape.fields.iter().zip(args) {
            // A faulted arg is reported and the emit is not sent.
            fields.push((name.clone(), self.eval(i, f, depth)?));
        }
        Some(Emitted {
            instance: i,
            event: shape.name.clone(),
            value: WireValue::Struct {
                name: shape.name,
                fields,
            },
        })
    }

    /// Walks `step` of frame `f` when it is a step both runners walk alike: a line (said, when its
    /// condition holds), a line group (one member said, its steps pushed), `when`, `once`, `emit`.
    pub(crate) fn common_step(
        &mut self,
        f: &mut Frame,
        step: StepDef,
        depth: usize,
        lines: &mut Vec<SaidLine>,
        emits: &mut Vec<Emitted>,
    ) -> Common {
        let i = f.instance;
        match step {
            StepDef::Line { line, cond } => {
                if let Some(once) = self.holds(i, &f.name, &cond, depth) {
                    self.rec.mark(i, &f.name, once);
                    lines.push(self.line(i, &line, depth));
                    return Common::Said(line.ticks);
                }
            }
            StepDef::Group { site, members } => {
                if let Some(pos) = self.pick_member(i, &f.name, site, &members, depth) {
                    let m = &members[pos];
                    lines.push(self.line(i, &m.line, depth));
                    f.push(m.body, pos);
                    return Common::Said(m.line.ticks);
                }
            }
            StepDef::When { cond, body } => {
                if let Some(once) = self.holds(i, &f.name, &cond, depth) {
                    self.rec.mark(i, &f.name, once);
                    f.push(body, 0);
                }
            }
            StepDef::Once { once, body } => {
                let cond = CondDef {
                    once: Some(once),
                    guard: None,
                };
                if let Some(once) = self.holds(i, &f.name, &cond, depth) {
                    self.rec.mark(i, &f.name, once);
                    f.push(body, 0);
                }
            }
            StepDef::Emit {
                event,
                script,
                args,
            } => emits.extend(self.emit(i, event, script, &args, depth)),
            other => return Common::Other(other),
        }
        Common::Next
    }
}

// ---- StateHash -----------------------------------------------------------------------------------------

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
