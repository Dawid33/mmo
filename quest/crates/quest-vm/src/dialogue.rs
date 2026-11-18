//! The dialogue graph format (spec §5.11): every dialogue, bark and scene of a program as plain data
//! (`Program::dialogues`), run by the dialogue runner, never by the VM.
//!
//! The steps of every dialogue live in one flat table ([`DialogueGraph::steps`]); a [`Block`] is a range of
//! it: a node's body, a group member's, an option's, a choice's fallback, a `when`'s, a `once`'s, a detour
//! arm's. Each step belongs to exactly one block, and every block is reached from a node of its dialogue by
//! exactly one path (the blocks form a tree under each node). Every expression is the id of a synthesised
//! pure fn (`Program::fns`, with a signature) taking the dialogue's cast (`p, npc` for a conversation, the
//! scene's parameters).
//!
//! Byte form (inside `Program::to_bytes`, after the quest): the dialogues as a `u32` count and their fields
//! in declaration order, then the steps as a `u32` count and each step as a tag byte and its fields. Every
//! `Vec` is a `u32` count and its elements, every `Option` a `u8` 0 or 1 and the value after a 1, every
//! enum a tag byte; `i64` as its two's-complement `u64`. `Program::validate` checks the graph against the
//! rest of the program (`dialogue::validate`).

use std::hash::Hasher;

use state_hash::StateHash;

use crate::consts::{
    DIALOGUE_NODES_MAX, GRAPH_STEPS_MAX, GROUP_MEMBERS_MAX, LINE_BASE_TICKS, LINE_MAX_TICKS,
    LINE_MIN_TICKS, LINE_TICKS_PER_CHAR, STRUCT_FIELDS_MAX,
};
use crate::program::{ID_TABLE_MAX, Parse, Program, ProgramError, Reader, Writer, invalid};
use crate::types::{TypeDesc, TypeId};

/// All dialogues of a program and their steps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DialogueGraph {
    pub dialogues: Vec<DialogueDef>,
    /// Every step of every dialogue, flat; [`Block`]s index it.
    pub steps: Vec<StepDef>,
}

/// A conversation, a bark or a scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogueDef {
    /// The qualified name: the stable key of the greeting pool and of a stateless dialogue's records.
    pub name: String,
    pub kind: DialogueKind,
    /// The cast's length: 2 for a conversation (`p, npc`), the scene's parameters (1 for a bark).
    pub cast: u8,
    /// The quest stage it is declared in (an index into `QuestInfo::stages`): attached only while that
    /// stage is current.
    pub stage: Option<u16>,
    /// `for target`: a fn of the cast giving the NPC (a conversation's).
    pub target: Option<u16>,
    /// `for npc where pred`: a `bool` fn of the cast (a conversation's).
    pub filter: Option<u16>,
    /// A scene's `where`: a `bool` fn of the cast.
    pub cast_cond: Option<u16>,
    /// A conversation's `topic`; `None` for a greeting.
    pub topic: Option<TopicDef>,
    /// `[0]` is the implicit `done`, then the declared exits in order.
    pub exits: Vec<String>,
    pub pick: Pick,
    /// Package-level content: reads no script global, emits nothing.
    pub stateless: bool,
    pub interruptible: bool,
    /// Nodes sharing a name form a node group; a group is named by the index of its first node.
    pub nodes: Vec<NodeDef>,
    /// The `where`'s conjunct count: a scene's score as an ambient candidate.
    pub score: u8,
}

/// A bark is a [`DialogueKind::Scene`] with a cast of 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogueKind {
    Conversation,
    Scene,
}

/// `@pick(…)`: how node and line groups pick a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    First,
    Best,
    BestLeastRecent,
    RandomBestLeastRecent,
}

/// `topic "text" [@id] [if cond] [priority n]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicDef {
    pub text: LineDef,
    /// A `bool` fn of the cast.
    pub cond: Option<u16>,
    pub priority: i64,
}

/// `node name [when cond]:` + its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeDef {
    pub name: String,
    pub cond: CondDef,
    /// Its condition's conjunct count (0 without one).
    pub score: u8,
    pub body: Block,
}

/// `once [per e] [if e]`, a plain condition, or nothing (both `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CondDef {
    pub once: Option<OnceDef>,
    /// A `bool` fn of the cast.
    pub guard: Option<u16>,
}

/// `once [per e]`: the record kept at `site`, per instance or per value of the `per` fn (of the cast).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnceDef {
    /// The record's key inside its dialogue (unique among the dialogue's sites).
    pub site: u32,
    pub per: Option<u16>,
}

/// A range of [`DialogueGraph::steps`]: `first .. first + len`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Block {
    pub first: u32,
    pub len: u32,
}

/// A line: its id, its speaker, its substitutions (`str` fns of the cast, in hole order) and its duration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineDef {
    pub id: String,
    pub speaker: Speaker,
    pub subst: Vec<u16>,
    /// Its duration in ticks (scenes and barks: [`line_ticks`] or the `(ticks=n)` override).
    pub ticks: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    /// A cast member, by parameter position (`p` is 0 in a conversation, `npc` 1).
    Cast(u8),
    /// Any other name in scope: a fn of the cast giving the speaker.
    Fn(u16),
}

/// `=> line [when cond]` and its steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberDef {
    pub line: LineDef,
    pub cond: CondDef,
    pub score: u8,
    pub body: Block,
}

/// An option of a `choice`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionDef {
    pub text: LineDef,
    /// Its `if`: a `bool` fn of the cast.
    pub guard: Option<u16>,
    /// A `once` option: its record's site.
    pub once: Option<u32>,
    /// Picked when the timeout runs out.
    pub default: bool,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepDef {
    Line {
        line: LineDef,
        cond: CondDef,
    },
    /// A line group; `site` keys its pick counts.
    Group {
        site: u32,
        members: Vec<MemberDef>,
    },
    /// `choice[(timeout=ticks)]:`; `fallback` is the hidden `default:`.
    Choice {
        timeout: Option<u32>,
        options: Vec<OptionDef>,
        fallback: Option<Block>,
    },
    When {
        cond: CondDef,
        body: Block,
    },
    Once {
        once: OnceDef,
        body: Block,
    },
    /// `-> group`: the index of the group's first node.
    Jump {
        group: u16,
    },
    /// `detour dialogue[:` + arms`]`: each arm by the callee's exit index.
    Detour {
        dialogue: u16,
        arms: Vec<(u16, Block)>,
    },
    /// `emit Event(…)`: `event` indexes `Program::events` when `script`, else the host's events
    /// (`HostApi::events`); `args` are fns of the cast, one per field in field order.
    Emit {
        event: u16,
        script: bool,
        args: Vec<u16>,
    },
    /// `return [exit]`: an index into the dialogue's exits (0 = `done`).
    Return {
        exit: u16,
    },
    Pause {
        ticks: u32,
    },
    /// A scene's last step `-> other_scene`.
    FollowUp {
        scene: u16,
    },
}

/// A timed line's duration from its source text's length (Unicode scalar values, f-string holes by
/// their source length): `LINE_BASE_TICKS + chars × LINE_TICKS_PER_CHAR`, clamped to
/// `[LINE_MIN_TICKS, LINE_MAX_TICKS]`.
pub fn line_ticks(source_chars: u32) -> u32 {
    LINE_BASE_TICKS
        .saturating_add(source_chars.saturating_mul(LINE_TICKS_PER_CHAR))
        .clamp(LINE_MIN_TICKS, LINE_MAX_TICKS)
}

impl DialogueGraph {
    /// Appends `steps` and returns the block holding them (an assembler helper: a block's steps are
    /// contiguous, so a step's inner blocks are appended before or after it, never inside it).
    pub fn block(&mut self, steps: Vec<StepDef>) -> Block {
        let first = u32::try_from(self.steps.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(steps.len()).unwrap_or(u32::MAX);
        self.steps.extend(steps);
        Block { first, len }
    }
}

impl Block {
    /// Its step indices, when the block lies inside a table of `n` steps.
    pub fn range(self, n: usize) -> Option<std::ops::Range<usize>> {
        let first = usize::try_from(self.first).ok()?;
        let end = first.checked_add(usize::try_from(self.len).ok()?)?;
        (end <= n).then_some(first..end)
    }
}

impl CondDef {
    /// No condition at all.
    pub const NONE: CondDef = CondDef {
        once: None,
        guard: None,
    };
}

impl StepDef {
    /// The blocks this step holds, in order.
    pub fn blocks(&self) -> Vec<Block> {
        match self {
            StepDef::Group { members, .. } => members.iter().map(|m| m.body).collect(),
            StepDef::Choice {
                options, fallback, ..
            } => options.iter().map(|o| o.body).chain(*fallback).collect(),
            StepDef::When { body, .. } | StepDef::Once { body, .. } => vec![*body],
            StepDef::Detour { arms, .. } => arms.iter().map(|a| a.1).collect(),
            StepDef::Line { .. }
            | StepDef::Jump { .. }
            | StepDef::Emit { .. }
            | StepDef::Return { .. }
            | StepDef::Pause { .. }
            | StepDef::FollowUp { .. } => Vec::new(),
        }
    }
}

// ---- StateHash -----------------------------------------------------------------------------------------

impl StateHash for DialogueGraph {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.dialogues.state_hash(h);
        self.steps.state_hash(h);
    }
}

impl StateHash for DialogueDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.name.state_hash(h);
        self.kind.state_hash(h);
        self.cast.state_hash(h);
        self.stage.state_hash(h);
        self.target.state_hash(h);
        self.filter.state_hash(h);
        self.cast_cond.state_hash(h);
        self.topic.state_hash(h);
        self.exits.state_hash(h);
        self.pick.state_hash(h);
        self.stateless.state_hash(h);
        self.interruptible.state_hash(h);
        self.nodes.state_hash(h);
        self.score.state_hash(h);
    }
}

impl StateHash for DialogueKind {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u8(kind_byte(*self));
    }
}

impl StateHash for Pick {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u8(pick_byte(*self));
    }
}

impl StateHash for TopicDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.text.state_hash(h);
        self.cond.state_hash(h);
        self.priority.state_hash(h);
    }
}

impl StateHash for NodeDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.name.state_hash(h);
        self.cond.state_hash(h);
        self.score.state_hash(h);
        self.body.state_hash(h);
    }
}

impl StateHash for CondDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.once.state_hash(h);
        self.guard.state_hash(h);
    }
}

impl StateHash for OnceDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.site.state_hash(h);
        self.per.state_hash(h);
    }
}

impl StateHash for Block {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.first.state_hash(h);
        self.len.state_hash(h);
    }
}

impl StateHash for LineDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.id.state_hash(h);
        self.speaker.state_hash(h);
        self.subst.state_hash(h);
        self.ticks.state_hash(h);
    }
}

impl StateHash for Speaker {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        match self {
            Speaker::Cast(i) => {
                h.write_u8(0);
                i.state_hash(h);
            }
            Speaker::Fn(f) => {
                h.write_u8(1);
                f.state_hash(h);
            }
        }
    }
}

impl StateHash for MemberDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.line.state_hash(h);
        self.cond.state_hash(h);
        self.score.state_hash(h);
        self.body.state_hash(h);
    }
}

impl StateHash for OptionDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.text.state_hash(h);
        self.guard.state_hash(h);
        self.once.state_hash(h);
        self.default.state_hash(h);
        self.body.state_hash(h);
    }
}

impl StateHash for StepDef {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u8(step_tag(self));
        match self {
            StepDef::Line { line, cond } => {
                line.state_hash(h);
                cond.state_hash(h);
            }
            StepDef::Group { site, members } => {
                site.state_hash(h);
                members.state_hash(h);
            }
            StepDef::Choice {
                timeout,
                options,
                fallback,
            } => {
                timeout.state_hash(h);
                options.state_hash(h);
                fallback.state_hash(h);
            }
            StepDef::When { cond, body } => {
                cond.state_hash(h);
                body.state_hash(h);
            }
            StepDef::Once { once, body } => {
                once.state_hash(h);
                body.state_hash(h);
            }
            StepDef::Jump { group } => group.state_hash(h),
            StepDef::Detour { dialogue, arms } => {
                dialogue.state_hash(h);
                arms.state_hash(h);
            }
            StepDef::Emit {
                event,
                script,
                args,
            } => {
                event.state_hash(h);
                script.state_hash(h);
                args.state_hash(h);
            }
            StepDef::Return { exit } => exit.state_hash(h),
            StepDef::Pause { ticks } => ticks.state_hash(h),
            StepDef::FollowUp { scene } => scene.state_hash(h),
        }
    }
}

pub(crate) fn kind_byte(k: DialogueKind) -> u8 {
    match k {
        DialogueKind::Conversation => 0,
        DialogueKind::Scene => 1,
    }
}

pub(crate) fn pick_byte(p: Pick) -> u8 {
    match p {
        Pick::First => 0,
        Pick::Best => 1,
        Pick::BestLeastRecent => 2,
        Pick::RandomBestLeastRecent => 3,
    }
}

pub(crate) fn step_tag(s: &StepDef) -> u8 {
    match s {
        StepDef::Line { .. } => 0,
        StepDef::Group { .. } => 1,
        StepDef::Choice { .. } => 2,
        StepDef::When { .. } => 3,
        StepDef::Once { .. } => 4,
        StepDef::Jump { .. } => 5,
        StepDef::Detour { .. } => 6,
        StepDef::Emit { .. } => 7,
        StepDef::Return { .. } => 8,
        StepDef::Pause { .. } => 9,
        StepDef::FollowUp { .. } => 10,
    }
}

// ---- bytes ---------------------------------------------------------------------------------------------

fn write_block(w: &mut Writer, b: Block) {
    w.u32(b.first);
    w.u32(b.len);
}

fn write_opt_u32(w: &mut Writer, v: Option<u32>) {
    w.flag(v.is_some());
    if let Some(v) = v {
        w.u32(v);
    }
}

fn write_line(w: &mut Writer, l: &LineDef) {
    w.str(&l.id);
    match l.speaker {
        Speaker::Cast(i) => {
            w.u8(0);
            w.u8(i);
        }
        Speaker::Fn(f) => {
            w.u8(1);
            w.u16(f);
        }
    }
    w.u16s(&l.subst);
    w.u32(l.ticks);
}

fn write_once(w: &mut Writer, o: &OnceDef) {
    w.u32(o.site);
    w.opt_u16(o.per);
}

fn write_cond(w: &mut Writer, c: &CondDef) {
    w.flag(c.once.is_some());
    if let Some(o) = &c.once {
        write_once(w, o);
    }
    w.opt_u16(c.guard);
}

fn write_step(w: &mut Writer, s: &StepDef) {
    w.u8(step_tag(s));
    match s {
        StepDef::Line { line, cond } => {
            write_line(w, line);
            write_cond(w, cond);
        }
        StepDef::Group { site, members } => {
            w.u32(*site);
            w.count(members.len());
            for m in members {
                write_line(w, &m.line);
                write_cond(w, &m.cond);
                w.u8(m.score);
                write_block(w, m.body);
            }
        }
        StepDef::Choice {
            timeout,
            options,
            fallback,
        } => {
            write_opt_u32(w, *timeout);
            w.count(options.len());
            for o in options {
                write_line(w, &o.text);
                w.opt_u16(o.guard);
                write_opt_u32(w, o.once);
                w.flag(o.default);
                write_block(w, o.body);
            }
            w.flag(fallback.is_some());
            if let Some(b) = fallback {
                write_block(w, *b);
            }
        }
        StepDef::When { cond, body } => {
            write_cond(w, cond);
            write_block(w, *body);
        }
        StepDef::Once { once, body } => {
            write_once(w, once);
            write_block(w, *body);
        }
        StepDef::Jump { group } => w.u16(*group),
        StepDef::Detour { dialogue, arms } => {
            w.u16(*dialogue);
            w.count(arms.len());
            for (exit, body) in arms {
                w.u16(*exit);
                write_block(w, *body);
            }
        }
        StepDef::Emit {
            event,
            script,
            args,
        } => {
            w.u16(*event);
            w.flag(*script);
            w.u16s(args);
        }
        StepDef::Return { exit } => w.u16(*exit),
        StepDef::Pause { ticks } => w.u32(*ticks),
        StepDef::FollowUp { scene } => w.u16(*scene),
    }
}

/// The graph's bytes (see the module docs).
pub(crate) fn write_graph(w: &mut Writer, g: &DialogueGraph) {
    w.count(g.dialogues.len());
    for d in &g.dialogues {
        w.str(&d.name);
        w.u8(kind_byte(d.kind));
        w.u8(d.cast);
        w.opt_u16(d.stage);
        w.opt_u16(d.target);
        w.opt_u16(d.filter);
        w.opt_u16(d.cast_cond);
        w.flag(d.topic.is_some());
        if let Some(t) = &d.topic {
            write_line(w, &t.text);
            w.opt_u16(t.cond);
            w.u64(t.priority as u64);
        }
        w.count(d.exits.len());
        for e in &d.exits {
            w.str(e);
        }
        w.u8(pick_byte(d.pick));
        w.flag(d.stateless);
        w.flag(d.interruptible);
        w.count(d.nodes.len());
        for n in &d.nodes {
            w.str(&n.name);
            write_cond(w, &n.cond);
            w.u8(n.score);
            write_block(w, n.body);
        }
        w.u8(d.score);
    }
    w.count(g.steps.len());
    for s in &g.steps {
        write_step(w, s);
    }
}

fn read_block(r: &mut Reader) -> Parse<Block> {
    Ok(Block {
        first: r.u32()?,
        len: r.u32()?,
    })
}

fn read_opt_u32(r: &mut Reader) -> Parse<Option<u32>> {
    Ok(if r.flag()? { Some(r.u32()?) } else { None })
}

fn read_line(r: &mut Reader) -> Parse<LineDef> {
    let id = r.str()?;
    let speaker = match r.u8()? {
        0 => Speaker::Cast(r.u8()?),
        1 => Speaker::Fn(r.u16()?),
        t => return invalid(format!("unknown speaker tag {t}")),
    };
    Ok(LineDef {
        id,
        speaker,
        subst: r.u16s()?,
        ticks: r.u32()?,
    })
}

fn read_once(r: &mut Reader) -> Parse<OnceDef> {
    Ok(OnceDef {
        site: r.u32()?,
        per: r.opt_u16()?,
    })
}

fn read_cond(r: &mut Reader) -> Parse<CondDef> {
    Ok(CondDef {
        once: if r.flag()? { Some(read_once(r)?) } else { None },
        guard: r.opt_u16()?,
    })
}

fn read_step(r: &mut Reader) -> Parse<StepDef> {
    Ok(match r.u8()? {
        0 => StepDef::Line {
            line: read_line(r)?,
            cond: read_cond(r)?,
        },
        1 => StepDef::Group {
            site: r.u32()?,
            members: r.vec(|r| {
                Ok(MemberDef {
                    line: read_line(r)?,
                    cond: read_cond(r)?,
                    score: r.u8()?,
                    body: read_block(r)?,
                })
            })?,
        },
        2 => StepDef::Choice {
            timeout: read_opt_u32(r)?,
            options: r.vec(|r| {
                Ok(OptionDef {
                    text: read_line(r)?,
                    guard: r.opt_u16()?,
                    once: read_opt_u32(r)?,
                    default: r.flag()?,
                    body: read_block(r)?,
                })
            })?,
            fallback: if r.flag()? {
                Some(read_block(r)?)
            } else {
                None
            },
        },
        3 => StepDef::When {
            cond: read_cond(r)?,
            body: read_block(r)?,
        },
        4 => StepDef::Once {
            once: read_once(r)?,
            body: read_block(r)?,
        },
        5 => StepDef::Jump { group: r.u16()? },
        6 => StepDef::Detour {
            dialogue: r.u16()?,
            arms: r.vec(|r| Ok((r.u16()?, read_block(r)?)))?,
        },
        7 => StepDef::Emit {
            event: r.u16()?,
            script: r.flag()?,
            args: r.u16s()?,
        },
        8 => StepDef::Return { exit: r.u16()? },
        9 => StepDef::Pause { ticks: r.u32()? },
        10 => StepDef::FollowUp { scene: r.u16()? },
        t => return invalid(format!("unknown dialogue step tag {t}")),
    })
}

/// Parses the graph's bytes; `Program::validate` checks it.
pub(crate) fn read_graph(r: &mut Reader) -> Parse<DialogueGraph> {
    let dialogues = r.vec_ids(|r| {
        let name = r.str()?;
        let kind = match r.u8()? {
            0 => DialogueKind::Conversation,
            1 => DialogueKind::Scene,
            k => return invalid(format!("unknown dialogue kind {k}")),
        };
        let cast = r.u8()?;
        let stage = r.opt_u16()?;
        let target = r.opt_u16()?;
        let filter = r.opt_u16()?;
        let cast_cond = r.opt_u16()?;
        let topic = if r.flag()? {
            Some(TopicDef {
                text: read_line(r)?,
                cond: r.opt_u16()?,
                priority: r.u64()? as i64,
            })
        } else {
            None
        };
        let exits = r.vec_ids(|r| r.str())?;
        let pick = match r.u8()? {
            0 => Pick::First,
            1 => Pick::Best,
            2 => Pick::BestLeastRecent,
            3 => Pick::RandomBestLeastRecent,
            p => return invalid(format!("unknown pick strategy {p}")),
        };
        Ok(DialogueDef {
            name,
            kind,
            cast,
            stage,
            target,
            filter,
            cast_cond,
            topic,
            exits,
            pick,
            stateless: r.flag()?,
            interruptible: r.flag()?,
            nodes: r.vec_ids(|r| {
                Ok(NodeDef {
                    name: r.str()?,
                    cond: read_cond(r)?,
                    score: r.u8()?,
                    body: read_block(r)?,
                })
            })?,
            score: r.u8()?,
        })
    })?;
    let n = r.count()?;
    if usize::try_from(n).map_or(true, |n| n > GRAPH_STEPS_MAX) {
        return invalid("the dialogue graph has more than GRAPH_STEPS_MAX steps");
    }
    let steps = r.vec_n(n, read_step)?;
    Ok(DialogueGraph { dialogues, steps })
}

// ---- validation ----------------------------------------------------------------------------------------

/// What a pure fn must return.
#[derive(Clone, Copy)]
enum Ret {
    Any,
    Bool,
    Str,
    Ty(TypeId),
}

struct Check<'a> {
    p: &'a Program,
    /// Per dialogue: the record sites its steps and nodes name.
    sites: Vec<Vec<u32>>,
    /// Per dialogue, per node: whether it is the first node of its group.
    group_first: Vec<Vec<bool>>,
}

impl Check<'_> {
    fn pure_fn(&self, d: &DialogueDef, f: u16, ret: Ret, what: &str) -> Result<(), ProgramError> {
        let p = self.p;
        let ok = p.fns.get(usize::from(f)).is_some_and(|i| {
            i.params == d.cast
                && i.sig.as_ref().is_some_and(|sig| match ret {
                    Ret::Any => true,
                    Ret::Bool => p.types.0.get(usize::from(sig.ret)) == Some(&TypeDesc::Bool),
                    Ret::Str => p.types.0.get(usize::from(sig.ret)) == Some(&TypeDesc::Str),
                    Ret::Ty(t) => sig.ret == t,
                })
        });
        if ok {
            Ok(())
        } else {
            invalid(format!(
                "dialogue {}: {what} (fn {f}) is not a pure fn of its {} cast members returning what it needs",
                d.name, d.cast
            ))
        }
    }

    fn cond(&mut self, di: usize, d: &DialogueDef, c: &CondDef) -> Result<(), ProgramError> {
        if let Some(o) = &c.once {
            self.once(di, d, o)?;
        }
        if let Some(g) = c.guard {
            self.pure_fn(d, g, Ret::Bool, "a condition")?;
        }
        Ok(())
    }

    fn once(&mut self, di: usize, d: &DialogueDef, o: &OnceDef) -> Result<(), ProgramError> {
        self.sites[di].push(o.site);
        match o.per {
            Some(f) => self.pure_fn(d, f, Ret::Any, "a `once per` key"),
            None => Ok(()),
        }
    }

    fn line(&self, d: &DialogueDef, l: &LineDef) -> Result<(), ProgramError> {
        match l.speaker {
            Speaker::Cast(i) if i < d.cast => {}
            Speaker::Cast(i) => {
                return invalid(format!(
                    "dialogue {}: line {} is spoken by cast member {i}, outside the cast",
                    d.name, l.id
                ));
            }
            Speaker::Fn(f) => self.pure_fn(d, f, Ret::Any, "a speaker")?,
        }
        for &f in &l.subst {
            self.pure_fn(d, f, Ret::Str, "a substitution")?;
        }
        Ok(())
    }

    /// The dialogue's own fields and its nodes' conditions (not the steps).
    fn header(&mut self, di: usize, d: &DialogueDef) -> Result<(), ProgramError> {
        let name = &d.name;
        if d.cast == 0 || usize::from(d.cast) > STRUCT_FIELDS_MAX {
            return invalid(format!("dialogue {name}: its cast is empty or too long"));
        }
        let conversation = d.kind == DialogueKind::Conversation;
        if conversation && d.cast != 2 {
            return invalid(format!(
                "dialogue {name}: a conversation's cast is `p, npc`"
            ));
        }
        if conversation && d.cast_cond.is_some()
            || !conversation && (d.target.is_some() || d.filter.is_some() || d.topic.is_some())
        {
            return invalid(format!(
                "dialogue {name}: a `for`, a filter or a topic belongs to a conversation, a `where` to a scene"
            ));
        }
        if d.target.is_some() && d.filter.is_some() {
            return invalid(format!("dialogue {name}: both a target and a filter"));
        }
        let stages = self.p.quest.as_ref().map_or(0, |q| q.stages.len());
        if d.stage.is_some_and(|s| usize::from(s) >= stages) || d.stateless && d.stage.is_some() {
            return invalid(format!("dialogue {name}: its stage is out of range"));
        }
        if d.exits.first().map(String::as_str) != Some("done")
            || d.exits.len() > ID_TABLE_MAX as usize
        {
            return invalid(format!(
                "dialogue {name}: its exits do not start with `done`"
            ));
        }
        let mut exits: Vec<&str> = d.exits.iter().map(String::as_str).collect();
        exits.sort_unstable();
        if exits.windows(2).any(|w| w[0] == w[1]) {
            return invalid(format!("dialogue {name}: two exits share a name"));
        }
        if d.nodes.is_empty() || d.nodes.len() > DIALOGUE_NODES_MAX {
            return invalid(format!("dialogue {name}: no nodes, or too many"));
        }
        if let Some(f) = d.target {
            self.pure_fn(d, f, Ret::Any, "the target")?;
        }
        if let Some(f) = d.filter {
            self.pure_fn(d, f, Ret::Bool, "the filter")?;
        }
        if let Some(f) = d.cast_cond {
            self.pure_fn(d, f, Ret::Bool, "the cast's `where`")?;
        }
        if let Some(t) = &d.topic {
            self.line(d, &t.text)?;
            if let Some(f) = t.cond {
                self.pure_fn(d, f, Ret::Bool, "the topic's condition")?;
            }
        }
        let mut seen = std::collections::BTreeMap::new();
        let mut first = Vec::with_capacity(d.nodes.len());
        for n in &d.nodes {
            self.cond(di, d, &n.cond)?;
            let members = seen.entry(n.name.as_str()).or_insert(0usize);
            first.push(*members == 0);
            *members += 1;
            if *members > GROUP_MEMBERS_MAX {
                return invalid(format!(
                    "dialogue {name}: node group {} has too many members",
                    n.name
                ));
            }
        }
        self.group_first[di] = first;
        Ok(())
    }

    /// One step of dialogue `di`: its fields, and the blocks it holds pushed onto `todo`.
    fn step(
        &mut self,
        di: usize,
        s: &StepDef,
        todo: &mut Vec<(usize, Block)>,
    ) -> Result<(), ProgramError> {
        let p = self.p;
        let g = &p.dialogues;
        let d = &g.dialogues[di];
        let name = &d.name;
        let conversation = d.kind == DialogueKind::Conversation;
        match s {
            StepDef::Line { line, cond } => {
                self.line(d, line)?;
                self.cond(di, d, cond)?;
            }
            StepDef::Group { site, members } => {
                if members.len() > GROUP_MEMBERS_MAX {
                    return invalid(format!(
                        "dialogue {name}: a line group has too many members"
                    ));
                }
                self.sites[di].push(*site);
                for m in members {
                    self.line(d, &m.line)?;
                    self.cond(di, d, &m.cond)?;
                }
            }
            StepDef::Choice {
                options, fallback, ..
            } => {
                if !conversation {
                    return invalid(format!("dialogue {name}: a scene has no `choice`"));
                }
                let defaults =
                    options.iter().filter(|o| o.default).count() + usize::from(fallback.is_some());
                if defaults > 1 {
                    return invalid(format!(
                        "dialogue {name}: a choice has more than one default"
                    ));
                }
                for o in options {
                    self.line(d, &o.text)?;
                    if let Some(f) = o.guard {
                        self.pure_fn(d, f, Ret::Bool, "an option's condition")?;
                    }
                    if let Some(site) = o.once {
                        self.sites[di].push(site);
                    }
                }
            }
            StepDef::When { cond, .. } => self.cond(di, d, cond)?,
            StepDef::Once { once, .. } => self.once(di, d, once)?,
            StepDef::Jump { group } => {
                if !self.group_first[di]
                    .get(usize::from(*group))
                    .copied()
                    .unwrap_or(false)
                {
                    return invalid(format!(
                        "dialogue {name}: a jump to {group}, which is not the first node of a group"
                    ));
                }
            }
            StepDef::Detour { dialogue, arms } => {
                let callee = g.dialogues.get(usize::from(*dialogue));
                let Some(callee) =
                    callee.filter(|c| conversation && c.kind == DialogueKind::Conversation)
                else {
                    return invalid(format!(
                        "dialogue {name}: a detour outside a conversation or to one out of range"
                    ));
                };
                let mut exits: Vec<u16> = arms.iter().map(|a| a.0).collect();
                exits.sort_unstable();
                if exits.windows(2).any(|w| w[0] == w[1])
                    || exits
                        .last()
                        .is_some_and(|&e| usize::from(e) >= callee.exits.len())
                {
                    return invalid(format!(
                        "dialogue {name}: a detour's arms name an exit twice or one out of range"
                    ));
                }
            }
            StepDef::Emit {
                event,
                script,
                args,
            } => {
                if d.stateless {
                    return invalid(format!("dialogue {name}: a stateless dialogue emits"));
                }
                if *script {
                    let fields = p.events.get(usize::from(*event)).and_then(|e| {
                        match p.types.0.get(usize::from(e.ty)) {
                            Some(TypeDesc::Struct { fields, .. }) => Some(fields),
                            _ => None,
                        }
                    });
                    match fields {
                        Some(fields) if fields.len() == args.len() => {
                            for (&a, (_, t)) in args.iter().zip(fields) {
                                self.pure_fn(d, a, Ret::Ty(*t), "an `emit` argument")?;
                            }
                        }
                        _ => {
                            return invalid(format!(
                                "dialogue {name}: an emit of script event {event}, out of range or of another arity"
                            ));
                        }
                    }
                } else {
                    // A host event's fields are the host API's; the program does not hold them.
                    for &a in args {
                        self.pure_fn(d, a, Ret::Any, "an `emit` argument")?;
                    }
                }
            }
            StepDef::Return { exit } => {
                if usize::from(*exit) >= d.exits.len() {
                    return invalid(format!("dialogue {name}: a return through no exit"));
                }
            }
            StepDef::Pause { .. } => {}
            StepDef::FollowUp { scene } => {
                let ok = !conversation
                    && g.dialogues
                        .get(usize::from(*scene))
                        .is_some_and(|t| t.kind == DialogueKind::Scene && t.cast <= d.cast);
                if !ok {
                    return invalid(format!(
                        "dialogue {name}: a follow-up outside a scene, out of range or with a longer cast"
                    ));
                }
            }
        }
        todo.extend(s.blocks().into_iter().map(|b| (di, b)));
        Ok(())
    }
}

/// The program's dialogue graph against the rest of it (see the module docs): every fn a pure fn of the
/// cast's arity (`bool` for a condition, `str` for a substitution, the field's type for a script event's
/// argument); every block inside the steps, the blocks a tree under the nodes, each step in exactly one
/// block; jumps, detours, exits and follow-ups in range; no `choice` or `detour` in a scene; sites unique
/// per dialogue; at most `GRAPH_STEPS_MAX` steps, `DIALOGUE_NODES_MAX` nodes a dialogue and
/// `GROUP_MEMBERS_MAX` members a node or line group.
pub(crate) fn validate(p: &Program) -> Result<(), ProgramError> {
    let g = &p.dialogues;
    if g.steps.len() > GRAPH_STEPS_MAX || g.dialogues.len() > ID_TABLE_MAX as usize {
        return invalid("the dialogue graph is too large");
    }
    let mut names: Vec<&str> = g.dialogues.iter().map(|d| d.name.as_str()).collect();
    names.sort_unstable();
    if names.windows(2).any(|w| w[0] == w[1]) {
        return invalid("two dialogues share a name");
    }
    let mut c = Check {
        p,
        sites: vec![Vec::new(); g.dialogues.len()],
        group_first: vec![Vec::new(); g.dialogues.len()],
    };
    let mut todo: Vec<(usize, Block)> = Vec::new();
    for (di, d) in g.dialogues.iter().enumerate() {
        c.header(di, d)?;
        todo.extend(d.nodes.iter().rev().map(|n| (di, n.body)));
    }
    // Every step is reached from the nodes exactly once: no block shares a step, every block hangs off
    // exactly one step or node (a cycle reaches some step twice, or none of its steps at all).
    let mut owned = vec![false; g.steps.len()];
    while let Some((di, b)) = todo.pop() {
        let Some(range) = b.range(g.steps.len()) else {
            return invalid(format!(
                "dialogue {}: a block lies outside the steps",
                g.dialogues[di].name
            ));
        };
        for i in range {
            if std::mem::replace(&mut owned[i], true) {
                return invalid(format!(
                    "dialogue {}: step {i} is in two blocks",
                    g.dialogues[di].name
                ));
            }
            c.step(di, &g.steps[i], &mut todo)?;
        }
    }
    if owned.iter().any(|o| !o) {
        return invalid("a dialogue step lies in no block");
    }
    for (di, sites) in c.sites.iter_mut().enumerate() {
        sites.sort_unstable();
        if sites.windows(2).any(|w| w[0] == w[1]) {
            return invalid(format!(
                "dialogue {}: two records share a site",
                g.dialogues[di].name
            ));
        }
    }
    Ok(())
}
