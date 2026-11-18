//! Dialogues and scenes (spec §5.11): their steps become HIR data, every expression in them a synthesised
//! pure `fn` (`hir/dialogue.rs` names them).

use super::quest::QuestScope;
use super::{Cx, Res, Resolver, Tag, error};
use crate::ast;
use crate::diag::{Diagnostic, Severity};
use crate::hir::{
    self, Builtin, DefId, DefKind, EventRef, HostItem, ModId, PureSite, StageIdx, TypeName,
};
use std::collections::{BTreeMap, BTreeSet};

/// The arguments `@pick` takes (spec §5.11).
const PICK_STRATEGIES: [(&str, hir::Pick); 4] = [
    ("first", hir::Pick::First),
    ("best", hir::Pick::Best),
    ("best_least_recent", hir::Pick::BestLeastRecent),
    ("random_best_least_recent", hir::Pick::RandomBestLeastRecent),
];

/// The implicit exit every dialogue has.
const DONE: &str = "done";

/// What a synthesised fn's return type is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ret {
    Bool,
    /// The expression wrapped in `str(…)`.
    Str,
    /// Inferred.
    Inferred,
}

/// The owner of synthesised fns: where they are, how they are named and what they take.
pub(super) struct Synth<'q> {
    pub(super) module: ModId,
    pub(super) file: &'q str,
    pub(super) quest: Option<&'q QuestScope>,
    /// The quest of a quest's synthesised fn.
    pub(super) parent: Option<DefId>,
    /// The name before `$`: the dialogue's (or the quest's) name.
    pub(super) local: String,
    /// The provisional line ids' prefix: the owner's qualified name, dotted.
    pub(super) line_prefix: String,
    /// The parameters: name, resolved type, where.
    pub(super) params: Vec<(String, Option<hir::Type>, ast::Span)>,
    pub(super) in_dialogue: bool,
    /// The next `n` of each site.
    counters: BTreeMap<&'static str, u32>,
    /// The next provisional line number.
    lines: u32,
}

impl<'q> Synth<'q> {
    pub(super) fn new(
        module: ModId,
        file: &'q str,
        quest: Option<&'q QuestScope>,
        parent: Option<DefId>,
        local: String,
        line_prefix: String,
    ) -> Synth<'q> {
        Synth {
            module,
            file,
            quest,
            parent,
            local,
            line_prefix,
            params: Vec::new(),
            in_dialogue: false,
            counters: BTreeMap::new(),
            lines: 0,
        }
    }

    fn cx(&self) -> Cx<'q> {
        let mut cx = Cx::new(self.module, self.file, self.quest);
        cx.in_dialogue = self.in_dialogue;
        cx
    }
}

/// The dialogue whose steps are being resolved.
struct DialogueCx<'d> {
    kind: hir::DialogueKind,
    /// Node group by name.
    groups: BTreeMap<&'d str, u32>,
    /// The cast's names, by position.
    cast: Vec<String>,
    /// `[0]` = `done`.
    exits: Vec<String>,
}

/// A built-in type annotation.
fn builtin(b: Builtin, span: ast::Span) -> hir::Type {
    hir::Type {
        kind: hir::TypeKind::Named(TypeName::Builtin(b), Vec::new()),
        span,
    }
}

fn type_word(p: &ast::Param) -> Option<&str> {
    match p.ty.as_ref().map(|t| &t.kind) {
        Some(ast::TypeKind::Named(path, args)) if args.is_empty() => {
            path.last().map(String::as_str)
        }
        _ => None,
    }
}

impl<'a> Resolver<'a> {
    /// A dialogue or scene item `d`, declared in module `m`, in a quest (its scope and the stage
    /// it is in) or not.
    pub(super) fn dialogue(
        &mut self,
        d: &ast::Dialogue,
        item: &ast::Item,
        m: ModId,
        quest: Option<(&QuestScope, Option<StageIdx>)>,
    ) -> hir::Dialogue {
        let file = self.file_of(m);
        let scope = quest.map(|(q, _)| q);
        let pick = self.dialogue_decorators(item, file);
        let kind = self.dialogue_kind(d, item.span, file);
        // The parameters: their types are resolved once, and each synthesised fn binds its own.
        let mut cx = Cx::new(m, file, scope);
        cx.push();
        let mut seen = BTreeSet::new();
        let mut params = Vec::new();
        let mut cast = Vec::new();
        for p in &d.params {
            if !seen.insert(p.name.as_str()) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("parameter `{}` is declared twice", p.name),
                    file,
                    p.span,
                ));
            }
            let ty = p.ty.as_ref().map(|t| self.ty(t, &mut cx));
            cast.push(self.bind_local(&mut cx, &p.name, p.span));
            params.push((p.name.clone(), ty, p.span));
        }
        cx.pop();
        let parent = scope.map(|q| q.quest);
        let qualified = self.qualified(m, parent, &d.name);
        let mut s = Synth::new(
            m,
            file,
            scope,
            parent,
            d.name.clone(),
            qualified.replace("::", "."),
        );
        s.params = params;
        s.in_dialogue = true;

        let (target, filter) = self.dialogue_target(d, item.span, &mut s);
        let topic = d.topic.as_ref().map(|t| hir::Topic {
            text: self.line_ref(&t.text, t.id.as_ref(), t.span, &mut s),
            cond: t
                .cond
                .as_ref()
                .map(|c| self.synth(&mut s, PureSite::Cond, c, Ret::Bool)),
            priority: t.priority.unwrap_or(0),
        });
        let mut exits = vec![DONE.to_string()];
        for (name, span) in &d.exits {
            if exits.contains(name) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("exit `{name}` is declared twice (`done` is every dialogue's)"),
                    file,
                    *span,
                ));
            } else {
                exits.push(name.clone());
            }
        }
        let cast_cond = d
            .cast_cond
            .as_ref()
            .map(|c| self.synth(&mut s, PureSite::CastCond, c, Ret::Bool));

        let mut groups: BTreeMap<&str, u32> = BTreeMap::new();
        for e in &d.body {
            if let ast::DialogueEntry::Node(n) = e {
                let next = u32::try_from(groups.len()).unwrap_or(u32::MAX);
                groups.entry(n.name.as_str()).or_insert(next);
            }
        }
        let dcx = DialogueCx {
            kind,
            groups,
            cast: d.params.iter().map(|p| p.name.clone()).collect(),
            exits: exits.clone(),
        };
        let has_nodes = !dcx.groups.is_empty();
        let mut nodes = Vec::new();
        let mut loose = Vec::new();
        let mut mixed_reported = false;
        let last_entry = d.body.len().saturating_sub(1);
        for (i, entry) in d.body.iter().enumerate() {
            let last = i == last_entry;
            match entry {
                ast::DialogueEntry::Node(n) => {
                    let cond = n.cond.as_ref().map(|c| self.cond(c, &mut s));
                    let steps = self.steps(&n.steps, &mut s, &dcx, last);
                    nodes.push(hir::Node {
                        name: n.name.clone(),
                        group: dcx.groups.get(n.name.as_str()).copied().unwrap_or(0),
                        cond,
                        score: n.cond.as_ref().map_or(0, cond_score),
                        steps,
                        span: n.span,
                    });
                }
                ast::DialogueEntry::Step(st) => {
                    if has_nodes && !mixed_reported {
                        mixed_reported = true;
                        self.diags.push(error(
                            "mixed-dialogue-body",
                            "a dialogue body is nodes or steps, not both: put these steps in a `node`"
                                .to_string(),
                            file,
                            st.span,
                        ));
                    }
                    if let Some(step) = self.step(st, &mut s, &dcx, last) {
                        loose.push(step);
                    }
                }
            }
        }
        if !has_nodes {
            nodes.push(hir::Node {
                name: String::new(),
                group: 0,
                cond: None,
                score: 0,
                steps: loose,
                span: item.span,
            });
        }
        hir::Dialogue {
            kind,
            quest: parent,
            stage: quest.and_then(|(_, stage)| stage),
            cast,
            target,
            filter,
            topic,
            exits,
            pick,
            uninterruptible: d.uninterruptible,
            cast_cond,
            score: d.cast_cond.as_ref().map_or(0, conjuncts),
            nodes,
            stateless: parent.is_none() && (filter.is_some() || kind == hir::DialogueKind::Scene),
        }
    }

    /// The qualified name of an item `name` of module `m` (under quest `parent`).
    pub(super) fn qualified(&self, m: ModId, parent: Option<DefId>, name: &str) -> String {
        let mut parts: Vec<String> = self
            .modules
            .get(m.0 as usize)
            .map(|module| module.path.clone())
            .unwrap_or_default();
        if let Some(meta) = parent.and_then(|p| self.metas.get(p.0 as usize)) {
            parts.push(meta.name.clone());
        }
        parts.push(name.to_string());
        parts.join("::")
    }

    /// `@pick(…)`, and the decorators a dialogue may not have.
    fn dialogue_decorators(&mut self, item: &ast::Item, file: &str) -> hir::Pick {
        let mut pick = None;
        for dec in &item.decorators {
            let message = match dec.name.as_str() {
                "pick" if pick.is_some() => Some("a dialogue has one `@pick`".to_string()),
                "pick" => {
                    let found = match dec.args.as_slice() {
                        [ast::Arg { name: None, value }] => match &value.kind {
                            ast::ExprKind::Path(p) => match p.as_slice() {
                                [s] => PICK_STRATEGIES
                                    .iter()
                                    .find(|(n, _)| n == s)
                                    .map(|(_, p)| *p),
                                _ => None,
                            },
                            _ => None,
                        },
                        _ => None,
                    };
                    pick = Some(found.unwrap_or(hir::Pick::BestLeastRecent));
                    found.is_none().then(|| {
                        let names: Vec<&str> = PICK_STRATEGIES.iter().map(|(n, _)| *n).collect();
                        format!("`@pick` takes one strategy: {}", names.join(", "))
                    })
                }
                "on" => Some("`@on` applies to a `fn`, not a `dialogue`".to_string()),
                other => Some(format!("unknown decorator `@{other}`")),
            };
            if let Some(message) = message {
                self.diags
                    .push(error("bad-decorator", message, file, dec.span));
            }
        }
        pick.unwrap_or(hir::Pick::BestLeastRecent)
    }

    /// Checks the parameter list (`dialogue-signature`): `(p: Player, npc: Person)` for a conversation,
    /// `(npc: Person)` for a bark, only `Person`s for a scene.
    fn dialogue_kind(&mut self, d: &ast::Dialogue, at: ast::Span, file: &str) -> hir::DialogueKind {
        if d.kind == ast::DialogueKind::Scene {
            for p in &d.params {
                if type_word(p) != Some("Person") {
                    self.diags.push(error(
                        "dialogue-signature",
                        format!(
                            "scene `{}`: its cast are `Person`s, so `{}` is `{}: Person`",
                            d.name, p.name, p.name
                        ),
                        file,
                        p.span,
                    ));
                }
            }
            if d.params.is_empty() {
                self.diags.push(error(
                    "dialogue-signature",
                    format!("scene `{}` needs a cast: `(a: Person, …)`", d.name),
                    file,
                    at,
                ));
            }
            return hir::DialogueKind::Scene;
        }
        let (ok, kind) = match d.params.as_slice() {
            [p, npc] => (
                type_word(p) == Some("Player") && type_word(npc) == Some("Person"),
                hir::DialogueKind::Conversation,
            ),
            [npc] => (type_word(npc) == Some("Person"), hir::DialogueKind::Bark),
            _ => (false, hir::DialogueKind::Conversation),
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
        kind
    }

    /// `for target` (a target fn), or `for npc where pred` (a filter fn; `npc` must be the dialogue's own
    /// `npc` parameter).
    fn dialogue_target(
        &mut self,
        d: &ast::Dialogue,
        at: ast::Span,
        s: &mut Synth,
    ) -> (Option<DefId>, Option<DefId>) {
        let Some(target) = &d.target else {
            return (None, None);
        };
        let Some(pred) = &d.filter else {
            return (
                Some(self.synth(s, PureSite::Target, target, Ret::Inferred)),
                None,
            );
        };
        let npc = d.params.last().map(|p| p.name.as_str());
        let names_npc = matches!(&target.kind, ast::ExprKind::Path(p)
            if matches!(p.as_slice(), [n] if Some(n.as_str()) == npc));
        if !names_npc {
            self.diags.push(error(
                "dialogue-signature",
                format!(
                    "`for … where …` filters the NPCs talked to: it names the dialogue's own `{}` parameter",
                    npc.unwrap_or("npc")
                ),
                s.file,
                if target.span.line == 0 { at } else { target.span },
            ));
        }
        (None, Some(self.synth(s, PureSite::Filter, pred, Ret::Bool)))
    }

    /// A synthesised pure fn of `s` computing `e` (spec §5.11): its item.
    pub(super) fn synth(
        &mut self,
        s: &mut Synth,
        site: PureSite,
        e: &ast::Expr,
        ret: Ret,
    ) -> DefId {
        let counter = s.counters.entry(site.word()).or_insert(0);
        let n = *counter;
        *counter = counter.saturating_add(1);
        let name = format!("{}${}#{n}", s.local, site.word());
        let mut cx = s.cx();
        cx.push();
        let params = s
            .params
            .iter()
            .map(|(name, ty, span)| hir::Param {
                local: self.bind_local(&mut cx, name, *span),
                ty: ty.clone(),
                span: *span,
            })
            .collect();
        let mut value = self.expr(e, &mut cx);
        cx.pop();
        if ret == Ret::Str {
            let callee = self.mk_expr(hir::ExprKind::Prelude(hir::Prelude::Str), e.span);
            value = self.mk_expr(
                hir::ExprKind::Call(Box::new(callee), vec![hir::Arg { name: None, value }]),
                e.span,
            );
        }
        let ret = match ret {
            Ret::Bool => Some(builtin(Builtin::Bool, e.span)),
            Ret::Str => Some(builtin(Builtin::Str, e.span)),
            Ret::Inferred => None,
        };
        let body = vec![hir::Stmt {
            kind: hir::StmtKind::Return(Some(value)),
            span: e.span,
        }];
        let id = self.add_member(s.module, name, e.span, Tag::Fn, None, s.parent);
        self.set_kind(
            id,
            DefKind::Fn(hir::Fn {
                owner: None,
                receiver: None,
                params,
                ret,
                raises: None,
                handler: None,
                body,
                pure: Some(site),
            }),
        );
        id
    }

    /// The index of `name` among dialogue `d`'s exits (`done` is 0).
    pub(super) fn exit_index(&self, d: DefId, name: &str) -> Option<u32> {
        if name == DONE {
            return Some(0);
        }
        let item = self.metas.get(d.0 as usize)?.item?;
        let (ast::ItemKind::Dialogue(dl) | ast::ItemKind::Scene(dl)) = &item.kind else {
            return None;
        };
        // As `Dialogue::exits`: `done` first, each name once.
        let mut exits = vec![DONE];
        for (n, _) in &dl.exits {
            if !exits.contains(&n.as_str()) {
                exits.push(n);
            }
        }
        u32::try_from(exits.iter().position(|n| *n == name)?).ok()
    }

    fn line_id(&mut self, id: &ast::LineId, file: &str) -> String {
        let key = id.path.join(".");
        self.claim_line_id(&key, file, id.span);
        key
    }

    /// Records a line id, written or provisional (`duplicate-line-id` when it was used before: an explicit
    /// `@d.0` beside the provisional `d.0` collides too).
    fn claim_line_id(&mut self, key: &str, file: &str, at: ast::Span) {
        if let Some((first_file, first)) = self.line_ids.get(key) {
            let mut d = error(
                "duplicate-line-id",
                format!("line id `@{key}` is used twice"),
                file,
                at,
            );
            d.notes
                .push(format!("first used at {first_file}:{}", first.line));
            self.diags.push(d);
        } else {
            self.line_ids
                .insert(key.to_string(), (file.to_string(), at));
        }
    }

    /// A line's text (a string or f-string literal, `bad-line-text`) and its id (`missing-line-id`, a
    /// warning, when it has none: the provisional `<owner>.<n>`).
    pub(super) fn line_ref(
        &mut self,
        text: &ast::Expr,
        id: Option<&ast::LineId>,
        at: ast::Span,
        s: &mut Synth,
    ) -> hir::LineRef {
        let n = s.lines;
        s.lines = s.lines.saturating_add(1);
        let id = match id {
            Some(id) => hir::LineId {
                path: self.line_id(id, s.file),
                provisional: false,
            },
            None => {
                let path = format!("{}.{n}", s.line_prefix);
                self.claim_line_id(&path, s.file, at);
                self.diags.push(Diagnostic {
                    severity: Severity::Warning,
                    ..error(
                        "missing-line-id",
                        format!("this line has no id (`@…`): it is `@{path}` until it gets one"),
                        s.file,
                        at,
                    )
                });
                hir::LineId {
                    path,
                    provisional: true,
                }
            }
        };
        let parts = match &text.kind {
            ast::ExprKind::Str(t) => vec![hir::LinePart::Text(t.clone())],
            ast::ExprKind::FStr(parts) => parts
                .iter()
                .map(|p| match p {
                    ast::FStrPart::Text(t) => hir::LinePart::Text(t.clone()),
                    ast::FStrPart::Expr(x) => {
                        hir::LinePart::Subst(self.synth(s, PureSite::Subst, x, Ret::Str), x.span)
                    }
                })
                .collect(),
            ast::ExprKind::Error => Vec::new(),
            _ => {
                self.diags.push(error(
                    "bad-line-text",
                    "a line's text is a string or an f-string literal".to_string(),
                    s.file,
                    text.span,
                ));
                Vec::new()
            }
        };
        hir::LineRef {
            id,
            parts,
            span: at,
        }
    }

    fn cond(&mut self, c: &ast::Cond, s: &mut Synth) -> hir::Cond {
        match c {
            ast::Cond::Once { per, guard } => hir::Cond {
                once: Some(hir::Once {
                    per: per
                        .as_ref()
                        .map(|e| self.synth(s, PureSite::Per, e, Ret::Inferred)),
                }),
                guard: guard
                    .as_ref()
                    .map(|e| self.synth(s, PureSite::Cond, e, Ret::Bool)),
            },
            ast::Cond::Expr(e) => hir::Cond {
                once: None,
                guard: Some(self.synth(s, PureSite::Cond, e, Ret::Bool)),
            },
        }
    }

    /// An int constant of at least `min` (`pause n`, `(ticks=n)`, `timeout=n`, `count n`): an int literal,
    /// negated or not, or a `let` whose value is one (`not-constant`, also for a value below `min`).
    pub(super) fn const_int(
        &mut self,
        e: &ast::Expr,
        cx: &mut Cx,
        what: &str,
        min: i64,
    ) -> Option<i64> {
        let value = match &e.kind {
            ast::ExprKind::Error => return None,
            ast::ExprKind::Path(path) => match self.path_res(path, e.span, cx) {
                Res::Error => return None,
                Res::Def(d) => match self.metas.get(d.0 as usize).and_then(|m| m.item) {
                    Some(ast::Item {
                        kind: ast::ItemKind::Let(l),
                        ..
                    }) => int_literal(&l.value),
                    _ => None,
                },
                _ => None,
            },
            _ => int_literal(e),
        };
        let message = match value {
            None => format!("{what} takes an int constant: a literal, or a `let` of one"),
            Some(v) if v < min => {
                format!("{what} takes an int constant of {min} or more, not {v}")
            }
            Some(v) => return Some(v),
        };
        self.diags
            .push(error("not-constant", message, cx.file, e.span));
        None
    }

    fn line(&mut self, l: &ast::Line, s: &mut Synth, dcx: &DialogueCx) -> hir::Line {
        let speaker = l.speaker.as_ref().and_then(|name| {
            if let Some(i) = dcx.cast.iter().position(|c| c == name) {
                return u32::try_from(i).ok().map(hir::Speaker::Cast);
            }
            if dcx.kind != hir::DialogueKind::Conversation {
                self.diags.push(error(
                    "unresolved",
                    format!("`{name}` is not in the cast: a scene's lines are said by its cast"),
                    s.file,
                    l.span,
                ));
                return None;
            }
            let path = ast::Expr {
                kind: ast::ExprKind::Path(vec![name.clone()]),
                span: l.span,
                depth: 1,
            };
            Some(hir::Speaker::Fn(self.synth(
                s,
                PureSite::Speaker,
                &path,
                Ret::Inferred,
            )))
        });
        let text = self.line_ref(&l.text, l.id.as_ref(), l.span, s);
        let ticks = l.ticks.as_ref().and_then(|t| {
            let mut cx = s.cx();
            self.const_int(t, &mut cx, "`(ticks=…)`", 0)
        });
        let cond = l.cond.as_ref().map(|c| self.cond(c, s));
        hir::Line {
            speaker,
            text,
            ticks,
            cond,
        }
    }

    /// `last`: these are the top-level steps of the body's last entry (where a scene's follow-up stands).
    fn steps(
        &mut self,
        steps: &[ast::Step],
        s: &mut Synth,
        dcx: &DialogueCx,
        last: bool,
    ) -> Vec<hir::Step> {
        let end = steps.len().saturating_sub(1);
        steps
            .iter()
            .enumerate()
            .filter_map(|(i, st)| self.step(st, s, dcx, last && i == end))
            .collect()
    }

    /// One step (`None`: dropped after its error was reported). `last`: it is the scene's last step.
    fn step(
        &mut self,
        st: &ast::Step,
        s: &mut Synth,
        dcx: &DialogueCx,
        last: bool,
    ) -> Option<hir::Step> {
        let scene_like = dcx.kind != hir::DialogueKind::Conversation;
        let kind = match &st.kind {
            ast::StepKind::Line(l) => hir::StepKind::Line(self.line(l, s, dcx)),
            ast::StepKind::Choice(args, options) => {
                match dcx.kind {
                    hir::DialogueKind::Scene => self.diags.push(error(
                        "scene-choice",
                        "a scene has nobody to answer: no `choice`".to_string(),
                        s.file,
                        st.span,
                    )),
                    hir::DialogueKind::Bark => self.diags.push(error(
                        "bark-choice",
                        "a bark has nobody to answer: no `choice`".to_string(),
                        s.file,
                        st.span,
                    )),
                    hir::DialogueKind::Conversation => {}
                }
                let mut timeout = None;
                for a in args {
                    let mut cx = s.cx();
                    if a.name.as_deref() == Some("timeout") && timeout.is_none() {
                        timeout = self.const_int(&a.value, &mut cx, "`timeout=`", 0);
                    } else {
                        self.diags.push(error(
                            "unresolved",
                            "`choice` takes one argument, `timeout=ticks`".to_string(),
                            s.file,
                            a.value.span,
                        ));
                    }
                }
                let options = options
                    .iter()
                    .map(|o| hir::ChoiceOption {
                        text: o
                            .text
                            .as_ref()
                            .map(|t| self.line_ref(t, o.id.as_ref(), o.span, s)),
                        guard: o
                            .guard
                            .as_ref()
                            .map(|g| self.synth(s, PureSite::Cond, g, Ret::Bool)),
                        once: o.mark == Some(ast::OptionMark::Once),
                        default: o.mark == Some(ast::OptionMark::Default) || o.text.is_none(),
                        steps: self.steps(&o.steps, s, dcx, false),
                        span: o.span,
                    })
                    .collect();
                hir::StepKind::Choice { timeout, options }
            }
            ast::StepKind::Group(members) => hir::StepKind::Group(
                members
                    .iter()
                    .map(|m| hir::GroupMember {
                        line: self.line(&m.line, s, dcx),
                        score: m.line.cond.as_ref().map_or(0, cond_score),
                        steps: self.steps(&m.steps, s, dcx, false),
                        span: m.span,
                    })
                    .collect(),
            ),
            ast::StepKind::When(c, steps) => {
                let c = self.cond(c, s);
                hir::StepKind::When(c, self.steps(steps, s, dcx, false))
            }
            ast::StepKind::Once(per, steps) => {
                let once = hir::Once {
                    per: per
                        .as_ref()
                        .map(|e| self.synth(s, PureSite::Per, e, Ret::Inferred)),
                };
                hir::StepKind::Once(once, self.steps(steps, s, dcx, false))
            }
            ast::StepKind::Jump(name) => {
                if let Some(g) = dcx.groups.get(name.as_str()) {
                    hir::StepKind::Jump(*g)
                } else if dcx.kind == hir::DialogueKind::Scene
                    && let Some(scene) = self.scene_named(name, s)
                {
                    self.follow_up(scene, name, st.span, s, dcx, last);
                    hir::StepKind::FollowUp(scene)
                } else {
                    self.diags.push(error(
                        "jump-target",
                        format!("`-> {name}`: this dialogue has no node `{name}`"),
                        s.file,
                        st.span,
                    ));
                    return None;
                }
            }
            ast::StepKind::Detour(name, arms) => {
                if scene_like {
                    self.diags.push(error(
                        "not-a-step",
                        "a scene or a bark has no `detour`: nobody is there to come back to"
                            .to_string(),
                        s.file,
                        st.span,
                    ));
                }
                let callee = self.dialogue_named(name, s);
                if callee.is_none() {
                    self.diags.push(error(
                        "jump-target",
                        format!("`detour {name}`: there is no dialogue `{name}`"),
                        s.file,
                        st.span,
                    ));
                }
                let mut out = Vec::new();
                let mut seen = BTreeSet::new();
                for arm in arms {
                    let steps = self.steps(&arm.steps, s, dcx, false);
                    let Some(callee) = callee else { continue };
                    if !seen.insert(arm.exit.as_str()) {
                        self.diags.push(error(
                            "duplicate-def",
                            format!("`detour {name}` has two arms for `{}`", arm.exit),
                            s.file,
                            arm.span,
                        ));
                        continue;
                    }
                    match self.exit_index(callee, &arm.exit) {
                        Some(i) => out.push((i, steps)),
                        None => self.diags.push(error(
                            "unknown-exit",
                            format!("`{name}` has no exit `{}`", arm.exit),
                            s.file,
                            arm.span,
                        )),
                    }
                }
                hir::StepKind::Detour(callee?, out)
            }
            ast::StepKind::Emit(path, args) => {
                let event = self.emitted(path, st.span, s);
                let args = args
                    .iter()
                    .map(|a| hir::EmitArg {
                        name: a.name.clone(),
                        value: self.synth(s, PureSite::EmitArg, &a.value, Ret::Inferred),
                    })
                    .collect();
                hir::StepKind::Emit(event?, args)
            }
            ast::StepKind::Return(exit) => {
                let name = exit.as_deref().unwrap_or(DONE);
                match dcx.exits.iter().position(|e| e == name) {
                    Some(i) => hir::StepKind::Return(u32::try_from(i).unwrap_or(0)),
                    None => {
                        self.diags.push(error(
                            "unknown-exit",
                            format!(
                                "`return {name}`: this dialogue declares no exit `{name}` (`exits {name}`)"
                            ),
                            s.file,
                            st.span,
                        ));
                        return None;
                    }
                }
            }
            ast::StepKind::Pause(e) => {
                let mut cx = s.cx();
                hir::StepKind::Pause(self.const_int(e, &mut cx, "`pause`", 0)?)
            }
        };
        Some(hir::Step {
            kind,
            span: st.span,
        })
    }

    /// The dialogue (or scene) `name` names, from the quest's names or the module's items.
    fn dialogue_named(&self, name: &str, s: &Synth) -> Option<DefId> {
        let d = match s.quest.and_then(|q| q.names.get(name)) {
            Some(super::QName::Def(d)) => Some(*d),
            Some(super::QName::Stage(_)) => None,
            None => match self.entry(s.module, name) {
                Some((Some(Res::Def(d)), _)) => Some(d),
                _ => None,
            },
        }?;
        (self.tag(d) == Some(Tag::Dialogue)).then_some(d)
    }

    fn scene_named(&self, name: &str, s: &Synth) -> Option<DefId> {
        let d = self.dialogue_named(name, s)?;
        let item = self.metas.get(d.0 as usize)?.item?;
        matches!(item.kind, ast::ItemKind::Scene(_)).then_some(d)
    }

    /// `-> scene` in a scene (`scene-follow-up`): the last step, to a scene whose cast is no longer.
    fn follow_up(
        &mut self,
        scene: DefId,
        name: &str,
        at: ast::Span,
        s: &Synth,
        dcx: &DialogueCx,
        last: bool,
    ) {
        if !last {
            self.diags.push(error(
                "scene-follow-up",
                format!("`-> {name}` plays a follow-up scene: it must be the scene's last step"),
                s.file,
                at,
            ));
        }
        let cast = match self
            .metas
            .get(scene.0 as usize)
            .and_then(|m| m.item)
            .map(|i| &i.kind)
        {
            Some(ast::ItemKind::Scene(d)) => d.params.len(),
            _ => 0,
        };
        if cast > dcx.cast.len() {
            self.diags.push(error(
                "scene-follow-up",
                format!(
                    "`-> {name}`: `{name}` has a cast of {cast}, this scene of {}; a follow-up takes the same cast by position",
                    dcx.cast.len()
                ),
                s.file,
                at,
            ));
        }
    }

    /// The event an `emit` names: a script event (the module's or the quest's) or a host event.
    fn emitted(&mut self, path: &[String], at: ast::Span, s: &Synth) -> Option<EventRef> {
        let mut cx = s.cx();
        match self.path_res(path, at, &mut cx) {
            Res::Def(d) if self.tag(d) == Some(Tag::Event) => Some(EventRef::Script(d)),
            Res::Host(HostItem::Event(i)) => Some(EventRef::Host(i)),
            Res::Error => None,
            _ => {
                self.diags.push(error(
                    "unknown-event",
                    format!("`emit {}`: not an event", path.join("::")),
                    s.file,
                    at,
                ));
                None
            }
        }
    }
}

/// An int literal, negated or not.
fn int_literal(e: &ast::Expr) -> Option<i64> {
    match &e.kind {
        ast::ExprKind::Int(i) => Some(*i),
        ast::ExprKind::Unary(ast::UnOp::Neg, x) => match &x.kind {
            ast::ExprKind::Int(i) => i.checked_neg(),
            _ => None,
        },
        _ => None,
    }
}

/// A condition's score (spec §5.11): its top-level conjuncts; `once` and `once per` count 1.
fn cond_score(c: &ast::Cond) -> u32 {
    match c {
        ast::Cond::Once { guard, .. } => 1 + guard.as_ref().map_or(0, conjuncts),
        ast::Cond::Expr(e) => conjuncts(e),
    }
}

/// The top-level conjuncts of `e`: `a and b and c` is 3; anything else (an `or`, a `not`, a call) is 1.
/// The parser keeps no parentheses, so a parenthesised `and` counts its conjuncts. A loop: an `and` chain is
/// as long as the source.
pub(super) fn conjuncts(e: &ast::Expr) -> u32 {
    let mut n = 0u32;
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        match &x.kind {
            ast::ExprKind::Binary(ast::BinOp::And, l, r) => stack.extend([&**l, &**r]),
            _ => n = n.saturating_add(1),
        }
    }
    n
}
