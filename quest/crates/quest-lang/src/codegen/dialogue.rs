//! Dialogues and scenes (spec §5.11): the HIR dialogues become the program's dialogue graph
//! (`Program::dialogues`), their line texts the lines table (`Compiled::lines`); and the program's script
//! events and the signatures of the synthesised pure fns, which the graph names.
//!
//! - **Order.** Script events and dialogues go by qualified name, so neither the module order nor the file
//!   order changes the program. A dialogue's steps are laid out in source order, depth first: a block's steps
//!   are contiguous, each nested block is reserved where its step is met and filled right after it (a loop,
//!   not recursion: steps nest as deep as the source).
//! - **Record sites** (`once` / `once per` records, `once` options, line groups' pick counts) are persistent
//!   NPC state kept across an edit, keyed by (instance, dialogue, site), so a site never depends on source
//!   position: it is the FNV-1a 32 hash of a stable **anchor** string, made from line ids —
//!   `option:<its text's id>`, `group:<its first member's id>`, `line:<id>` / `member:<id>` (a line's own
//!   `when once`), `node:<node name>|<its first line id>` (nodes of a group share a name), `once:<the first
//!   line id in its body>` and `when:<…>` likewise (a `when once …:` step). A `once:` or `when:` step whose
//!   body holds no line is anchored `<node name>#once<n>` / `<node name>#when<n>`, `n` its ordinal among the
//!   node's line-less steps of that kind (so only those shift on an edit). Two anchors of one dialogue on one
//!   site — the same anchor twice (`once: once: …` around the same first line) or a hash collision — are the
//!   error `site-collision`, never a silent merge. The first line id is the first in source order: a line, a
//!   group's first member, a choice's first option text.
//! - **Speakers.** A line without one is said by `npc` in a conversation and by the lead (the first of the
//!   cast) in a scene or bark; an option's and a topic's text by `p`.
//! - **Lines table.** Each line's source text, keyed by its id, with its f-string holes written `{0}`,
//!   `{1}`, … in `LineDef::subst` order and a literal brace doubled (`{{`, `}}`). A line's duration is its
//!   `(ticks=n)`, else `line_ticks` of its source length: the text's characters, each hole by its source
//!   length with its braces.
//! - A quest's dialogues are lowered here too, by the same rules; a stage's carries its stage.

use super::Gen;
use crate::ast::Span;
use crate::diag::Diagnostic;
use crate::hir::{
    self, DefId, DefKind, DialogueKind, EmitArg, EventRef, LinePart, LineRef, Step, StepKind,
};
use quest_vm::ProgramBuilder;
use quest_vm::dialogue::{
    Block, CondDef, DialogueDef, DialogueGraph, LineDef, MemberDef, NodeDef, OnceDef, OptionDef,
    Pick, Speaker, StepDef, TopicDef, line_ticks,
};
use std::collections::BTreeMap;

/// Line id → source text.
pub(crate) type Lines = BTreeMap<String, String>;

impl<'a> Gen<'a> {
    /// The program's script events (every `event`, a quest's and its generated `Entered` and `RoleLost`
    /// included, the compiler's own excepted), by qualified name: the operand of `Enqueue`, `After` and of
    /// a dialogue's `emit`.
    pub(super) fn script_events(&mut self, pb: &mut ProgramBuilder) {
        let krate = self.krate;
        let builtins = [
            krate.builtins.talked,
            krate.builtins.dialogue_ended,
            krate.builtins.scene_ended,
        ];
        let mut events: Vec<(String, DefId)> = (0..krate.defs.len())
            .map(|i| DefId(u32::try_from(i).unwrap_or(u32::MAX)))
            .filter(|d| {
                krate
                    .def(*d)
                    .is_some_and(|def| matches!(def.kind, DefKind::Event(_)))
                    && !builtins.contains(&Some(*d))
            })
            .map(|d| (krate.qualified_name(d), d))
            .collect();
        events.sort();
        for (name, d) in events {
            let Some(ty) = self.typed.types.lookup(crate::ty::Ty::Adt(d)) else {
                continue;
            };
            let (file, span) = self.def_site(d);
            if let Some(tid) = self.type_id(pb, ty, &file, span) {
                let e = pb.script_event(&name, tid);
                self.events.insert(d, e);
            }
        }
    }

    /// Every synthesised pure fn's signature (the cast, its return type): the host evaluates them.
    pub(super) fn pure_sigs(&mut self, pb: &mut ProgramBuilder) {
        let krate = self.krate;
        for (i, def) in krate.defs.iter().enumerate() {
            let d = DefId(u32::try_from(i).unwrap_or(u32::MAX));
            let DefKind::Fn(f) = &def.kind else { continue };
            let (Some(_), Some(sig), Some(&r)) =
                (f.pure, self.typed.sigs.get(&d), self.fns.get(&d))
            else {
                continue;
            };
            let (file, span) = self.def_site(d);
            let mut params = Vec::with_capacity(sig.params.len());
            for t in &sig.params {
                params.extend(self.type_id(pb, *t, &file, span));
            }
            let ret = self.type_id(pb, sig.ret, &file, span);
            if let (Some(ret), true) = (ret, params.len() == sig.params.len()) {
                pb.set_sig(r, params, ret);
            }
        }
    }

    /// The dialogue graph of every dialogue and scene (a quest's too: a stage's carries its stage), and
    /// their lines.
    pub(super) fn dialogues(&mut self, pb: &mut ProgramBuilder) -> Lines {
        let krate = self.krate;
        let mut order: Vec<(String, DefId)> = (0..krate.defs.len())
            .map(|i| DefId(u32::try_from(i).unwrap_or(u32::MAX)))
            .filter(|d| {
                krate
                    .def(*d)
                    .is_some_and(|def| matches!(def.kind, DefKind::Dialogue(_)))
            })
            .map(|d| (krate.qualified_name(d), d))
            .collect();
        order.sort();
        let index: BTreeMap<DefId, u16> = order
            .iter()
            .enumerate()
            .map(|(i, (_, d))| (*d, u16::try_from(i).unwrap_or(u16::MAX)))
            .collect();
        let mut lower = Lower {
            g: self,
            index: &index,
            graph: DialogueGraph::default(),
            lines: Lines::new(),
            sites: BTreeMap::new(),
            node: String::new(),
            lineless: BTreeMap::new(),
            kind: DialogueKind::Conversation,
            file: String::new(),
        };
        for (name, d) in &order {
            if let Some(DefKind::Dialogue(dl)) = krate.def(*d).map(|def| &def.kind) {
                lower.file = lower.g.def_site(*d).0;
                let def = lower.dialogue(name, dl);
                lower.graph.dialogues.push(def);
            }
        }
        let Lower { graph, lines, .. } = lower;
        pb.set_dialogues(graph);
        lines
    }
}

/// Lowers dialogues one after another into one graph.
struct Lower<'l, 'a> {
    g: &'l mut Gen<'a>,
    /// Each lowered dialogue's index in the graph.
    index: &'l BTreeMap<DefId, u16>,
    graph: DialogueGraph,
    lines: Lines,
    /// The record sites of the dialogue being lowered, with their anchors.
    sites: BTreeMap<u32, String>,
    /// The node being lowered, and its line-less `once:` / `when:` steps so far.
    node: String,
    lineless: BTreeMap<&'static str, u32>,
    kind: DialogueKind,
    file: String,
}

/// FNV-1a, 32 bits: a record site from its anchor (fixed, unlike std's `Hash`).
fn fnv1a32(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// The first line id among `steps`, in source order (a line, a group's first member, a choice's first
/// option text), nested steps included.
fn first_line(steps: &[Step]) -> Option<&str> {
    let mut stack: Vec<&Step> = steps.iter().rev().collect();
    while let Some(s) = stack.pop() {
        let found = match &s.kind {
            StepKind::Line(l) => Some(&l.text),
            StepKind::Group(members) => members.first().map(|m| &m.line.text),
            StepKind::Choice { options, .. } => options.iter().find_map(|o| o.text.as_ref()),
            _ => None,
        };
        if let Some(t) = found {
            return Some(&t.id.path);
        }
        stack.extend(s.inner().into_iter().rev());
    }
    None
}

/// A line's text as the lines table keeps it (holes `{0}`, `{1}`, … in order, a literal brace doubled), and
/// its length for `line_ticks`: the text's characters, each hole by its source length with its braces.
pub(super) fn stored_text(text: &LineRef) -> (String, u32) {
    let mut stored = String::new();
    let mut chars: u32 = 0;
    let mut holes = 0usize;
    for part in &text.parts {
        match part {
            LinePart::Text(t) => {
                chars = chars.saturating_add(u32::try_from(t.chars().count()).unwrap_or(u32::MAX));
                for c in t.chars() {
                    match c {
                        '{' => stored.push_str("{{"),
                        '}' => stored.push_str("}}"),
                        c => stored.push(c),
                    }
                }
            }
            LinePart::Subst(_, span) => {
                stored.push_str(&format!("{{{holes}}}"));
                holes += 1;
                chars = chars.saturating_add(span.len.saturating_add(2));
            }
        }
    }
    (stored, chars)
}

fn sat_u8(n: u32) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}

fn sat_u16(n: u32) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn ticks(n: i64) -> u32 {
    u32::try_from(n.max(0)).unwrap_or(u32::MAX)
}

impl<'a> Lower<'_, 'a> {
    /// A synthesised fn's id (an unknown one is out of range, and refused by validation).
    fn f(&self, d: DefId) -> u16 {
        self.g.fns.get(&d).map_or(u16::MAX, |r| r.0)
    }

    /// The record site of `anchor` (`site-collision` at `at` when another record of the dialogue has it).
    fn site(&mut self, anchor: String, at: Span) -> u32 {
        let site = fnv1a32(&anchor);
        match self.sites.get(&site) {
            Some(other) => {
                let message = if *other == anchor {
                    format!(
                        "two records of this dialogue share the anchor `{anchor}`: give one of them a line of its own"
                    )
                } else {
                    format!(
                        "the records anchored `{anchor}` and `{other}` hash to the same site: change a line id"
                    )
                };
                self.g.diags.push(Diagnostic::error(
                    "site-collision",
                    message,
                    &self.file,
                    at.line,
                    at.col,
                    at.len,
                ));
            }
            None => {
                self.sites.insert(site, anchor);
            }
        }
        site
    }

    /// The anchor of a `once:` or `when:` step (`kind`) holding `steps`.
    fn block_anchor(&mut self, kind: &'static str, steps: &[Step]) -> String {
        match first_line(steps) {
            Some(id) => format!("{kind}:{id}"),
            None => {
                let n = self.lineless.entry(kind).or_insert(0);
                let anchor = format!("{}#{kind}{n}", self.node);
                *n = n.saturating_add(1);
                anchor
            }
        }
    }

    fn dialogue(&mut self, name: &str, dl: &'a hir::Dialogue) -> DialogueDef {
        self.sites.clear();
        self.kind = dl.kind;
        let topic = dl.topic.as_ref().map(|t| TopicDef {
            text: self.line_def(&t.text, Speaker::Cast(0), None),
            cond: t.cond.map(|c| self.f(c)),
            priority: t.priority,
        });
        // A node group is named by its first node.
        let mut first: BTreeMap<u32, u16> = BTreeMap::new();
        for (i, n) in dl.nodes.iter().enumerate() {
            first
                .entry(n.group)
                .or_insert(u16::try_from(i).unwrap_or(u16::MAX));
        }
        let mut nodes = Vec::with_capacity(dl.nodes.len());
        for n in &dl.nodes {
            self.node = n.name.clone();
            self.lineless.clear();
            let anchor = format!("node:{}|{}", n.name, first_line(&n.steps).unwrap_or(""));
            let cond = self.cond(n.cond.as_ref(), anchor, n.span);
            let body = self.body(&n.steps, &first);
            nodes.push(NodeDef {
                name: n.name.clone(),
                cond,
                score: sat_u8(n.score),
                body,
            });
        }
        DialogueDef {
            name: name.to_string(),
            kind: match dl.kind {
                DialogueKind::Conversation => quest_vm::dialogue::DialogueKind::Conversation,
                DialogueKind::Bark | DialogueKind::Scene => quest_vm::dialogue::DialogueKind::Scene,
            },
            cast: u8::try_from(dl.cast.len()).unwrap_or(u8::MAX),
            stage: dl.stage.map(|s| sat_u16(s.0)),
            target: dl.target.map(|t| self.f(t)),
            filter: dl.filter.map(|t| self.f(t)),
            cast_cond: dl.cast_cond.map(|t| self.f(t)),
            topic,
            exits: dl.exits.clone(),
            pick: match dl.pick {
                hir::Pick::First => Pick::First,
                hir::Pick::Best => Pick::Best,
                hir::Pick::BestLeastRecent => Pick::BestLeastRecent,
                hir::Pick::RandomBestLeastRecent => Pick::RandomBestLeastRecent,
            },
            stateless: dl.stateless,
            interruptible: !dl.uninterruptible,
            nodes,
            score: sat_u8(dl.score),
        }
    }

    /// Reserves a block for `steps`, to be filled later (`todo`).
    fn reserve(&mut self, steps: &'a [Step], todo: &mut Vec<(u32, &'a [Step])>) -> Block {
        let first = u32::try_from(self.graph.steps.len()).unwrap_or(u32::MAX);
        self.graph
            .steps
            .extend(steps.iter().map(|_| StepDef::Pause { ticks: 0 }));
        todo.push((first, steps));
        Block {
            first,
            len: u32::try_from(steps.len()).unwrap_or(u32::MAX),
        }
    }

    /// A node's body and every block inside it, depth first in source order.
    fn body(&mut self, steps: &'a [Step], first: &BTreeMap<u32, u16>) -> Block {
        let mut todo = Vec::new();
        let block = self.reserve(steps, &mut todo);
        let mut stack: Vec<(u32, std::slice::Iter<'a, Step>)> =
            todo.drain(..).map(|(at, s)| (at, s.iter())).collect();
        while let Some(top) = stack.last_mut() {
            let Some(step) = top.1.next() else {
                stack.pop();
                continue;
            };
            let at = top.0;
            top.0 = top.0.saturating_add(1);
            let def = self.step(step, first, &mut todo);
            if let Some(slot) = self.graph.steps.get_mut(at as usize) {
                *slot = def;
            }
            // The step's blocks next, the first on top.
            stack.extend(todo.drain(..).rev().map(|(at, s)| (at, s.iter())));
        }
        block
    }

    fn once(&mut self, o: &hir::Once, anchor: String, at: Span) -> OnceDef {
        OnceDef {
            site: self.site(anchor, at),
            per: o.per.map(|p| self.f(p)),
        }
    }

    /// A condition; its `once` record is anchored `anchor` (taken only when there is one).
    fn cond(&mut self, c: Option<&hir::Cond>, anchor: String, at: Span) -> CondDef {
        match c {
            None => CondDef::NONE,
            Some(c) => CondDef {
                once: c.once.as_ref().map(|o| self.once(o, anchor, at)),
                guard: c.guard.map(|g| self.f(g)),
            },
        }
    }

    /// A line's speaker when it names none: `npc` in a conversation, the lead in a scene or bark.
    fn default_speaker(&self) -> Speaker {
        match self.kind {
            DialogueKind::Conversation => Speaker::Cast(1),
            DialogueKind::Bark | DialogueKind::Scene => Speaker::Cast(0),
        }
    }

    fn line(&mut self, l: &hir::Line) -> LineDef {
        let speaker = match &l.speaker {
            Some(hir::Speaker::Cast(i)) => Speaker::Cast(u8::try_from(*i).unwrap_or(u8::MAX)),
            Some(hir::Speaker::Fn(d)) => Speaker::Fn(self.f(*d)),
            None => self.default_speaker(),
        };
        self.line_def(&l.text, speaker, l.ticks)
    }

    /// A line's definition; its text joins the lines table.
    fn line_def(&mut self, text: &LineRef, speaker: Speaker, ticks_n: Option<i64>) -> LineDef {
        let (stored, chars) = stored_text(text);
        let subst = text.subst().into_iter().map(|d| self.f(d)).collect();
        self.lines.insert(text.id.path.clone(), stored);
        LineDef {
            id: text.id.path.clone(),
            speaker,
            subst,
            ticks: ticks_n.map_or_else(|| line_ticks(chars), ticks),
        }
    }

    /// One step; the blocks it holds are reserved and queued on `todo` in order.
    fn step(
        &mut self,
        s: &'a Step,
        first: &BTreeMap<u32, u16>,
        todo: &mut Vec<(u32, &'a [Step])>,
    ) -> StepDef {
        match &s.kind {
            StepKind::Line(l) => {
                let line = self.line(l);
                let anchor = format!("line:{}", l.text.id.path);
                let cond = self.cond(l.cond.as_ref(), anchor, s.span);
                StepDef::Line { line, cond }
            }
            StepKind::Choice { timeout, options } => {
                let mut out = Vec::with_capacity(options.len());
                let mut fallback = None;
                for o in options {
                    let Some(text) = &o.text else {
                        fallback = Some(self.reserve(&o.steps, todo));
                        continue;
                    };
                    let anchor = format!("option:{}", text.id.path);
                    let text = self.line_def(text, Speaker::Cast(0), None);
                    let once = o.once.then(|| self.site(anchor, o.span));
                    out.push(OptionDef {
                        text,
                        guard: o.guard.map(|g| self.f(g)),
                        once,
                        default: o.default,
                        body: self.reserve(&o.steps, todo),
                    });
                }
                StepDef::Choice {
                    timeout: timeout.map(ticks),
                    options: out,
                    fallback,
                }
            }
            StepKind::Group(members) => {
                let anchor = match members.first() {
                    Some(m) => format!("group:{}", m.line.text.id.path),
                    None => format!("{}#group", self.node),
                };
                let site = self.site(anchor, s.span);
                let members = members
                    .iter()
                    .map(|m| {
                        let line = self.line(&m.line);
                        let anchor = format!("member:{}", m.line.text.id.path);
                        let cond = self.cond(m.line.cond.as_ref(), anchor, m.span);
                        MemberDef {
                            line,
                            cond,
                            score: sat_u8(m.score),
                            body: self.reserve(&m.steps, todo),
                        }
                    })
                    .collect();
                StepDef::Group { site, members }
            }
            StepKind::When(c, steps) => {
                let anchor = match c.once {
                    Some(_) => self.block_anchor("when", steps),
                    None => String::new(),
                };
                let cond = self.cond(Some(c), anchor, s.span);
                StepDef::When {
                    cond,
                    body: self.reserve(steps, todo),
                }
            }
            StepKind::Once(o, steps) => {
                let anchor = self.block_anchor("once", steps);
                let once = self.once(o, anchor, s.span);
                StepDef::Once {
                    once,
                    body: self.reserve(steps, todo),
                }
            }
            StepKind::Jump(group) => StepDef::Jump {
                group: first.get(group).copied().unwrap_or(u16::MAX),
            },
            StepKind::FollowUp(scene) => StepDef::FollowUp {
                scene: self.index.get(scene).copied().unwrap_or(u16::MAX),
            },
            StepKind::Detour(callee, arms) => StepDef::Detour {
                dialogue: self.index.get(callee).copied().unwrap_or(u16::MAX),
                arms: arms
                    .iter()
                    .map(|(exit, steps)| (sat_u16(*exit), self.reserve(steps, todo)))
                    .collect(),
            },
            StepKind::Emit(event, args) => self.emit(*event, args, s),
            StepKind::Return(exit) => StepDef::Return {
                exit: sat_u16(*exit),
            },
            StepKind::Pause(n) => StepDef::Pause { ticks: ticks(*n) },
        }
    }

    /// `emit Ev(…)`: the event and its argument fns in field order.
    fn emit(&mut self, event: EventRef, args: &[EmitArg], s: &Step) -> StepDef {
        let (event, script, names) = match event {
            EventRef::Script(d) => {
                let Some(&e) = self.g.events.get(&d) else {
                    let file = self.file.clone();
                    self.g
                        .not_yet("an `emit` of this event in a dialogue", &file, s.span);
                    return StepDef::Pause { ticks: 0 };
                };
                let names = self.g.shapes(d).first().map(|s| s.names.clone());
                (e, true, names)
            }
            EventRef::Host(i) => (
                u16::try_from(i).unwrap_or(u16::MAX),
                false,
                self.g
                    .typed
                    .host_events
                    .get(i as usize)
                    .map(|s| s.names.clone()),
            ),
        };
        let names = names.unwrap_or_default();
        // As inference reads them: by name, or positionally in order.
        let mut by_field: Vec<Option<u16>> = vec![None; names.len()];
        let mut positional = 0usize;
        for a in args {
            let index = match &a.name {
                Some(field) => names.iter().position(|n| n == field),
                None => {
                    positional += 1;
                    Some(positional - 1)
                }
            };
            if let Some(slot) = index.and_then(|i| by_field.get_mut(i)) {
                *slot = Some(self.f(a.value));
            }
        }
        StepDef::Emit {
            event,
            script,
            args: by_field.into_iter().flatten().collect(),
        }
    }
}
