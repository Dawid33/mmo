//! Dialogues and quests (spec §5.11–5.12, decision 19): resolved and checked, then dropped until
//! Phase 10 lowers them.

use super::{Cx, ROOT, Res, Resolver, Tag, error};
use crate::ast;
use crate::hir::{HostItem, ModId};
use crate::modules::ROOT_FILE;
use std::collections::{BTreeMap, BTreeSet};

/// The arguments `@pick` takes (spec §5.11).
const PICK_STRATEGIES: [&str; 4] = [
    "first",
    "best",
    "best_least_recent",
    "random_best_least_recent",
];

/// Functions a quest's code may call (spec §5.12); lowered in Phase 10.
pub(super) const QUEST_BUILTINS: [&str; 3] = ["complete", "fail", "decline"];

/// Functions a dialogue's conditions may call (spec §5.11); lowered in Phase 10.
pub(super) const DIALOGUE_BUILTINS: [&str; 1] = ["dialogue_depth"];

/// What a quest's own name is (spec §5.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuestName {
    Role,
    Global,
    Event,
    Stage,
    Dialogue,
    Fn,
}

/// A quest's names, seen by everything inside it.
#[derive(Default)]
pub(super) struct QuestScope {
    pub(super) names: BTreeMap<String, QuestName>,
}

impl QuestScope {
    pub(super) fn has_stage(&self, name: &str) -> bool {
        self.names.get(name) == Some(&QuestName::Stage)
    }
}

/// The dialogue whose steps are being resolved.
struct DialogueCx<'d> {
    nodes: BTreeSet<&'d str>,
    /// A bark (`(npc: Person)`) offers no choice.
    bark: bool,
}

impl<'a> Resolver<'a> {
    // Dialogues and quests (spec §5.11–5.12, decision 19): resolved and checked, then dropped.

    pub(super) fn dialogue(
        &mut self,
        d: &ast::Dialogue,
        item: &ast::Item,
        m: ModId,
        quest: Option<&QuestScope>,
    ) {
        let file = self.file_of(m);
        let mut cx = Cx::new(m, file, quest);
        cx.in_dialogue = true;
        self.dialogue_decorators(item, &cx);
        let bark = self.dialogue_signature(d, item.span, file);
        if let Some(target) = &d.target {
            self.expr(target, &mut cx);
        }
        cx.push();
        let mut seen = BTreeSet::new();
        for p in &d.params {
            if !seen.insert(p.name.as_str()) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("parameter `{}` is declared twice", p.name),
                    file,
                    p.span,
                ));
            }
            if let Some(t) = &p.ty {
                self.ty(t, &mut cx);
            }
            self.bind_local(&mut cx, &p.name, p.span);
        }
        if let Some(topic) = &d.topic {
            self.expr(&topic.text, &mut cx);
            if let Some(id) = &topic.id {
                self.line_id(id, file);
            }
            if let Some(c) = &topic.cond {
                self.expr(c, &mut cx);
            }
        }
        let dcx = DialogueCx {
            nodes: d
                .body
                .iter()
                .filter_map(|e| match e {
                    ast::DialogueEntry::Node(n) => Some(n.name.as_str()),
                    ast::DialogueEntry::Step(_) => None,
                })
                .collect(),
            bark,
        };
        let has_nodes = !dcx.nodes.is_empty();
        let mut mixed_reported = false;
        for entry in &d.body {
            match entry {
                ast::DialogueEntry::Node(n) => {
                    if let Some(c) = &n.cond {
                        self.cond(c, &mut cx);
                    }
                    self.steps(&n.steps, &mut cx, &dcx);
                }
                ast::DialogueEntry::Step(s) => {
                    if has_nodes && !mixed_reported {
                        mixed_reported = true;
                        self.diags.push(error(
                            "mixed-dialogue-body",
                            "a dialogue body is nodes or steps, not both: put these steps in a `node`"
                                .to_string(),
                            file,
                            s.span,
                        ));
                    }
                    self.step(s, &mut cx, &dcx);
                }
            }
        }
        cx.pop();
    }

    fn dialogue_decorators(&mut self, item: &ast::Item, cx: &Cx) {
        let mut seen_pick = false;
        for dec in &item.decorators {
            let message = match dec.name.as_str() {
                "pick" if seen_pick => Some("a dialogue has one `@pick`".to_string()),
                "pick" => {
                    seen_pick = true;
                    let ok = matches!(dec.args.as_slice(),
                        [ast::Arg { name: None, value }]
                            if matches!(&value.kind, ast::ExprKind::Path(p)
                                if matches!(p.as_slice(), [s] if PICK_STRATEGIES.contains(&s.as_str()))));
                    (!ok).then(|| {
                        format!("`@pick` takes one strategy: {}", PICK_STRATEGIES.join(", "))
                    })
                }
                "on" => Some("`@on` applies to a `fn`, not a `dialogue`".to_string()),
                other => Some(format!("unknown decorator `@{other}`")),
            };
            if let Some(message) = message {
                self.diags
                    .push(error("bad-decorator", message, cx.file, dec.span));
            }
        }
    }

    /// Checks the parameter list: `(p: Player, npc: Person)` for a conversation, `(npc: Person)` for a
    /// bark. Returns whether the dialogue is a bark.
    fn dialogue_signature(&mut self, d: &ast::Dialogue, at: ast::Span, file: &str) -> bool {
        fn type_word(p: &ast::Param) -> Option<&str> {
            match p.ty.as_ref().map(|t| &t.kind) {
                Some(ast::TypeKind::Named(path, args)) if args.is_empty() => {
                    path.last().map(String::as_str)
                }
                _ => None,
            }
        }
        let (ok, bark) = match d.params.as_slice() {
            [p, npc] => (
                type_word(p) == Some("Player") && type_word(npc) == Some("Person"),
                false,
            ),
            [npc] => (type_word(npc) == Some("Person"), true),
            _ => (false, false),
        };
        if !ok {
            self.diags.push(error(
                "dialogue-signature",
                format!(
                    "dialogue `{}` takes `(p: Player, npc: Person)`, or `(npc: Person)` for a bark",
                    d.name
                ),
                file,
                at,
            ));
        }
        ok && bark
    }

    fn line_id(&mut self, id: &ast::LineId, file: &str) {
        let key = id.path.join(".");
        if let Some((first_file, first)) = self.line_ids.get(&key) {
            let mut d = error(
                "duplicate-line-id",
                format!("line id `@{key}` is used twice"),
                file,
                id.span,
            );
            d.notes
                .push(format!("first used at {first_file}:{}", first.line));
            self.diags.push(d);
        } else {
            self.line_ids.insert(key, (file.to_string(), id.span));
        }
    }

    fn cond(&mut self, c: &ast::Cond, cx: &mut Cx) {
        match c {
            ast::Cond::Once { per, guard } => {
                for e in per.iter().chain(guard) {
                    self.expr(e, cx);
                }
            }
            ast::Cond::Expr(e) => {
                self.expr(e, cx);
            }
        }
    }

    fn line(&mut self, l: &ast::Line, cx: &mut Cx) {
        if let Some(speaker) = &l.speaker
            && cx.local(speaker).is_none()
        {
            self.path_res(std::slice::from_ref(speaker), l.span, cx);
        }
        self.expr(&l.text, cx);
        if let Some(id) = &l.id {
            self.line_id(id, cx.file);
        }
        if let Some(c) = &l.cond {
            self.cond(c, cx);
        }
    }

    fn steps(&mut self, steps: &[ast::Step], cx: &mut Cx, dcx: &DialogueCx) {
        for s in steps {
            self.step(s, cx, dcx);
        }
    }

    fn step(&mut self, s: &ast::Step, cx: &mut Cx, dcx: &DialogueCx) {
        match &s.kind {
            ast::StepKind::Line(l) => self.line(l, cx),
            ast::StepKind::Choice(args, options) => {
                if dcx.bark {
                    self.diags.push(error(
                        "bark-choice",
                        "a bark has nobody to answer: no `choice`".to_string(),
                        cx.file,
                        s.span,
                    ));
                }
                self.args(args, cx);
                for o in options {
                    if let Some(text) = &o.text {
                        self.expr(text, cx);
                    }
                    if let Some(id) = &o.id {
                        self.line_id(id, cx.file);
                    }
                    if let Some(g) = &o.guard {
                        self.expr(g, cx);
                    }
                    self.steps(&o.steps, cx, dcx);
                }
            }
            ast::StepKind::Group(members) => {
                for m in members {
                    self.line(&m.line, cx);
                    self.steps(&m.steps, cx, dcx);
                }
            }
            ast::StepKind::When(c, steps) => {
                self.cond(c, cx);
                self.steps(steps, cx, dcx);
            }
            ast::StepKind::Once(per, steps) => {
                if let Some(per) = per {
                    self.expr(per, cx);
                }
                self.steps(steps, cx, dcx);
            }
            ast::StepKind::Jump(name) => {
                if !dcx.nodes.contains(name.as_str()) {
                    self.diags.push(error(
                        "jump-target",
                        format!("`-> {name}`: this dialogue has no node `{name}`"),
                        cx.file,
                        s.span,
                    ));
                }
            }
            ast::StepKind::Detour(name) => {
                let ok = match cx.quest.and_then(|q| q.names.get(name.as_str())) {
                    Some(k) => *k == QuestName::Dialogue,
                    None => matches!(self.entry(cx.module, name),
                        Some((Some(Res::Def(d)), _)) if self.tag(d) == Some(Tag::Dialogue)),
                };
                if !ok {
                    self.diags.push(error(
                        "jump-target",
                        format!("`detour {name}`: there is no dialogue `{name}`"),
                        cx.file,
                        s.span,
                    ));
                }
            }
            ast::StepKind::Emit(path, args) => {
                self.emitted(path, s.span, cx);
                self.args(args, cx);
            }
            ast::StepKind::Return => {}
        }
    }

    /// The event an `emit` names: a script event (the module's or the quest's) or a host event.
    fn emitted(&mut self, path: &[String], at: ast::Span, cx: &mut Cx) {
        let is_event = match self.path_res(path, at, cx) {
            Res::Def(d) => self.tag(d) == Some(Tag::Event),
            Res::Host(HostItem::Event(_)) | Res::Error => true,
            Res::Deferred => match (cx.quest, path) {
                (Some(q), [name]) => q.names.get(name.as_str()) == Some(&QuestName::Event),
                _ => false,
            },
            _ => false,
        };
        if !is_event {
            self.diags.push(error(
                "unknown-event",
                format!("`emit {}`: not an event", path.join("::")),
                cx.file,
                at,
            ));
        }
    }

    pub(super) fn quest(&mut self, members: &'a [ast::Item], item: &ast::Item, m: ModId) {
        let file = self.file_of(m);
        if m != ROOT {
            self.diags.push(error(
                "quest-placement",
                format!("a `quest` belongs in the crate root `{ROOT_FILE}`"),
                file,
                item.span,
            ));
        } else if self.root_quest_seen {
            self.diags.push(error(
                "quest-placement",
                "a program has at most one `quest`".to_string(),
                file,
                item.span,
            ));
        }
        if m == ROOT {
            self.root_quest_seen = true;
        }
        let mut scope = QuestScope::default();
        for member in members {
            self.quest_name(member, &mut scope, file);
            if let ast::ItemKind::Stage(_, inner) = &member.kind {
                for sm in inner {
                    self.quest_name(sm, &mut scope, file);
                }
            }
        }
        for member in members {
            let mut cx = Cx::new(m, file, Some(&scope));
            match &member.kind {
                ast::ItemKind::Role(_, ty, req) => {
                    self.ty(ty, &mut cx);
                    if let Some(req) = req {
                        self.expr(req, &mut cx);
                    }
                }
                ast::ItemKind::Let(l) => {
                    if let Some(t) = &l.ty {
                        self.ty(t, &mut cx);
                    }
                    self.expr(&l.value, &mut cx);
                }
                ast::ItemKind::Event(_, fields) => {
                    self.fields(fields, &mut cx);
                }
                ast::ItemKind::Stage(_, inner) => {
                    for sm in inner {
                        self.quest_member(sm, m, &scope);
                    }
                }
                ast::ItemKind::Objective(..) => self.diags.push(error(
                    "quest-only",
                    "an `objective` belongs in a quest's `stage`".to_string(),
                    file,
                    member.span,
                )),
                _ => self.quest_member(member, m, &scope),
            }
        }
    }

    /// Enters a quest member's name in the quest's scope.
    fn quest_name(&mut self, member: &ast::Item, scope: &mut QuestScope, file: &str) {
        let (name, kind) = match &member.kind {
            ast::ItemKind::Role(n, ..) => (n, QuestName::Role),
            ast::ItemKind::Let(l) => (&l.name, QuestName::Global),
            ast::ItemKind::Event(n, _) => (n, QuestName::Event),
            ast::ItemKind::Stage(n, _) => (n, QuestName::Stage),
            ast::ItemKind::Dialogue(d) => (&d.name, QuestName::Dialogue),
            ast::ItemKind::Fn(f) => (&f.name, QuestName::Fn),
            _ => return,
        };
        if scope.names.contains_key(name) {
            self.diags.push(error(
                "duplicate-def",
                format!("`{name}` is defined twice in this quest"),
                file,
                member.span,
            ));
        } else {
            scope.names.insert(name.clone(), kind);
        }
    }

    /// A quest's or stage's objective, dialogue or handler.
    fn quest_member(&mut self, member: &ast::Item, m: ModId, scope: &QuestScope) {
        let file = self.file_of(m);
        let mut cx = Cx::new(m, file, Some(scope));
        match &member.kind {
            ast::ItemKind::Objective(text, id, at) => {
                self.expr(text, &mut cx);
                if let Some(id) = id {
                    self.line_id(id, file);
                }
                if let Some(at) = at {
                    self.expr(at, &mut cx);
                }
            }
            ast::ItemKind::Dialogue(d) => self.dialogue(d, member, m, Some(scope)),
            ast::ItemKind::Fn(f) => {
                cx.quest_fn = true;
                self.func(f, member, None, false, &mut cx);
            }
            _ => {}
        }
    }
}
