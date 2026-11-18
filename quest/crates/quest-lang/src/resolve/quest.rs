//! Quests (spec §5.12): a quest's members and the names it generates become items whose parent is the quest;
//! roles' `where`, `requires`, transitions and objectives' lines become synthesised pure fns.

use super::dialogue::{Ret, Synth};
use super::{Cx, ROOT, Resolver, Tag, error};
use crate::ast;
use crate::hir::{
    self, Builtin, DefId, DefKind, EventRef, ModId, PureSite, QuestItems, RoleIdx, StageIdx,
    TypeName,
};
use crate::modules::ROOT_FILE;
use std::collections::{BTreeMap, BTreeSet};

/// What a quest's own name means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QName {
    /// A member or a generated item: a role's or objective's global, a `let`, an event, a dialogue, a
    /// handler, `Stage`, `Role`, `stage`, `participants`, `Entered`, `RoleLost`.
    Def(DefId),
    /// A stage (not a value: `Stage::Name` is).
    Stage(StageIdx),
}

/// A quest's names, seen by everything inside it (a part's: the quest's and its own).
#[derive(Debug, Clone)]
pub(super) struct QuestScope {
    pub(super) quest: DefId,
    pub(super) names: BTreeMap<String, QName>,
    /// Each objective by (stage, name), for `Stage.name`.
    pub(super) objectives: BTreeMap<(String, String), DefId>,
}

impl QuestScope {
    pub(super) fn stage(&self, name: &str) -> Option<StageIdx> {
        match self.names.get(name) {
            Some(QName::Stage(s)) => Some(*s),
            _ => None,
        }
    }
}

/// The options of `repeatable quest Q(…)` and the type of each.
const QUEST_OPTIONS: [(&str, OptionTy); 3] = [
    ("cooldown", OptionTy::Int),
    ("exclusive", OptionTy::Bool),
    ("once_per_player", OptionTy::Bool),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionTy {
    Int,
    Bool,
}

fn idx(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn named(n: TypeName, span: ast::Span) -> hir::Type {
    hir::Type {
        kind: hir::TypeKind::Named(n, Vec::new()),
        span,
    }
}

/// A stage's members, found by the collection walk.
struct StageMembers<'a> {
    name: &'a str,
    span: ast::Span,
    objectives: Vec<(&'a ast::Objective, DefId, ast::Span)>,
    transitions: Vec<(&'a str, &'a ast::Expr, ast::Span)>,
    dialogues: Vec<(&'a ast::Dialogue, &'a ast::Item, DefId)>,
    fns: Vec<(&'a ast::FnDef, &'a ast::Item, DefId)>,
}

impl<'a> Resolver<'a> {
    /// The quest `q` (item `id`, declared in module `m`).
    pub(super) fn quest(
        &mut self,
        q: &'a ast::Quest,
        item: &'a ast::Item,
        m: ModId,
        id: DefId,
    ) -> hir::Quest {
        let file = self.file_of(m);
        self.quest_placement(item, m, file);
        let span = item.span;
        let gen_def = |r: &mut Self, name: &str, tag| {
            r.add_member(m, name.to_string(), span, tag, None, Some(id))
        };
        let generated = QuestItems {
            stage_enum: gen_def(self, "Stage", Tag::Enum),
            role_enum: gen_def(self, "Role", Tag::Enum),
            stage: gen_def(self, "stage", Tag::Global),
            participants: gen_def(self, "participants", Tag::Global),
            entered: gen_def(self, "Entered", Tag::Event),
            role_lost: gen_def(self, "RoleLost", Tag::Event),
        };
        let mut scope = QuestScope {
            quest: id,
            names: BTreeMap::new(),
            objectives: BTreeMap::new(),
        };
        for (name, d) in [
            ("Stage", generated.stage_enum),
            ("Role", generated.role_enum),
            ("stage", generated.stage),
            ("participants", generated.participants),
            ("Entered", generated.entered),
            ("RoleLost", generated.role_lost),
        ] {
            scope.names.insert(name.to_string(), QName::Def(d));
        }

        // Collect: every member gets its item and its name, in source order.
        let mut roles: Vec<(&ast::Role, DefId, ast::Span)> = Vec::new();
        let mut lets = Vec::new();
        let mut events = Vec::new();
        let mut dialogues = Vec::new();
        let mut fns = Vec::new();
        let mut stages: Vec<StageMembers> = Vec::new();
        let mut requires = Vec::new();
        let mut parts = Vec::new();
        for member in &q.body {
            match &member.kind {
                ast::ItemKind::Role(r) => {
                    let d = self.add_member(
                        m,
                        format!("role.{}", r.name),
                        member.span,
                        Tag::Global,
                        None,
                        Some(id),
                    );
                    self.quest_name(&mut scope, &r.name, QName::Def(d), file, member.span);
                    roles.push((r, d, member.span));
                }
                ast::ItemKind::Let(l) => {
                    let d = self.member_def(m, &l.name, member, Tag::Global, id);
                    self.quest_name(&mut scope, &l.name, QName::Def(d), file, member.span);
                    lets.push((l, d));
                }
                ast::ItemKind::Event(name, fields) => {
                    self.reserved_event(name, file, member.span);
                    let d = self.member_def(m, name, member, Tag::Event, id);
                    self.quest_name(&mut scope, name, QName::Def(d), file, member.span);
                    events.push((fields, d));
                }
                ast::ItemKind::Dialogue(dl) | ast::ItemKind::Scene(dl) => {
                    let d = self.member_def(m, &dl.name, member, Tag::Dialogue, id);
                    self.quest_name(&mut scope, &dl.name, QName::Def(d), file, member.span);
                    dialogues.push((dl, member, d));
                }
                ast::ItemKind::Fn(f) => {
                    let d = self.member_def(m, &f.name, member, Tag::Fn, id);
                    self.quest_name(&mut scope, &f.name, QName::Def(d), file, member.span);
                    fns.push((f, member, d));
                }
                ast::ItemKind::Stage(name, inner) => {
                    let s = StageIdx(idx(stages.len()));
                    self.quest_name(&mut scope, name, QName::Stage(s), file, member.span);
                    let mut st = StageMembers {
                        name,
                        span: member.span,
                        objectives: Vec::new(),
                        transitions: Vec::new(),
                        dialogues: Vec::new(),
                        fns: Vec::new(),
                    };
                    for sm in inner {
                        self.stage_member(sm, &mut st, &mut scope, m, id, file);
                    }
                    stages.push(st);
                }
                ast::ItemKind::Requires(e) => requires.push(e),
                ast::ItemKind::Part(p) => parts.push((p, member.span)),
                ast::ItemKind::Objective(..) => self.diags.push(error(
                    "quest-only",
                    "an `objective` belongs in a quest's `stage`".to_string(),
                    file,
                    member.span,
                )),
                // The grammar puts nothing else in a quest.
                _ => {}
            }
        }

        // The generated items.
        self.set_kind(
            generated.stage_enum,
            DefKind::Enum(
                stages
                    .iter()
                    .map(|s| hir::Variant {
                        name: s.name.to_string(),
                        fields: Vec::new(),
                        span: s.span,
                    })
                    .collect(),
            ),
        );
        self.set_kind(
            generated.role_enum,
            DefKind::Enum(
                roles
                    .iter()
                    .map(|(r, _, span)| hir::Variant {
                        name: r.name.clone(),
                        fields: Vec::new(),
                        span: *span,
                    })
                    .collect(),
            ),
        );
        let stage_ty = named(TypeName::Def(generated.stage_enum), span);
        let first = if stages.is_empty() {
            hir::ExprKind::Error
        } else {
            hir::ExprKind::Variant(generated.stage_enum, 0)
        };
        let first = self.mk_expr(first, span);
        self.set_kind(
            generated.stage,
            DefKind::Global(hir::Global {
                ty: Some(stage_ty.clone()),
                value: first,
            }),
        );
        let player = match self.host.structs.get("Player") {
            Some(i) => named(TypeName::HostStruct(*i), span),
            None => hir::Type {
                kind: hir::TypeKind::Error,
                span,
            },
        };
        let empty = self.mk_expr(hir::ExprKind::List(Vec::new()), span);
        self.set_kind(
            generated.participants,
            DefKind::Global(hir::Global {
                ty: Some(hir::Type {
                    kind: hir::TypeKind::Named(TypeName::Builtin(Builtin::List), vec![player]),
                    span,
                }),
                value: empty,
            }),
        );
        let field = |name: &str, ty| hir::Field {
            name: name.to_string(),
            ty,
            span,
        };
        self.set_kind(
            generated.entered,
            DefKind::Event(vec![field("stage", stage_ty)]),
        );
        self.set_kind(
            generated.role_lost,
            DefKind::Event(vec![field(
                "role",
                named(TypeName::Def(generated.role_enum), span),
            )]),
        );

        // Roles, `requires`, `let`s, events.
        let qualified = self.qualified(m, None, &q.name);
        let mut qs = Synth::new(
            m,
            file,
            Some(&scope),
            None,
            q.name.clone(),
            qualified.replace("::", "."),
        );
        let mut role_defs: Vec<hir::RoleDef> = Vec::new();
        for (i, (r, global, rspan)) in roles.iter().enumerate() {
            let mut cx = Cx::new(m, file, Some(&scope));
            let ty = self.ty(&r.ty, &mut cx);
            let as_global = |ty: &hir::Type, optional: bool| {
                if optional {
                    hir::Type {
                        kind: hir::TypeKind::Named(
                            TypeName::Builtin(Builtin::Option),
                            vec![ty.clone()],
                        ),
                        span: ty.span,
                    }
                } else {
                    ty.clone()
                }
            };
            let value = self.mk_expr(hir::ExprKind::Error, *rspan);
            self.set_kind(
                *global,
                DefKind::Global(hir::Global {
                    ty: Some(as_global(&ty, r.optional)),
                    value,
                }),
            );
            let cond = r.cond.as_ref().map(|c| {
                qs.params = role_defs
                    .iter()
                    .map(|e| (e.name.clone(), Some(as_global(&e.ty, e.optional)), *rspan))
                    .chain(std::iter::once((r.name.clone(), Some(ty.clone()), *rspan)))
                    .collect();
                self.synth(&mut qs, PureSite::Where(RoleIdx(idx(i))), c, Ret::Bool)
            });
            role_defs.push(hir::RoleDef {
                name: r.name.clone(),
                ty,
                optional: r.optional,
                cond,
                global: *global,
            });
        }
        qs.params = Vec::new();
        let requires = requires
            .into_iter()
            .map(|e| self.synth(&mut qs, PureSite::Requires, e, Ret::Bool))
            .collect();
        let globals = lets
            .iter()
            .map(|(l, d)| {
                let mut cx = Cx::new(m, file, Some(&scope));
                let g = self.global(l, &mut cx);
                self.set_kind(*d, DefKind::Global(g));
                *d
            })
            .collect();
        let event_defs = events
            .iter()
            .map(|(fields, d)| {
                let mut cx = Cx::new(m, file, Some(&scope));
                let f = self.fields(fields, &mut cx);
                self.set_kind(*d, DefKind::Event(f));
                *d
            })
            .collect();

        // Stages.
        let mut stage_defs = Vec::new();
        for (i, st) in stages.iter().enumerate() {
            let s = StageIdx(idx(i));
            let mut objectives = Vec::new();
            for (o, global, ospan) in &st.objectives {
                let text = self.line_ref(&o.text, o.id.as_ref(), *ospan, &mut qs);
                let at =
                    o.at.as_ref()
                        .map(|e| self.synth(&mut qs, PureSite::Target, e, Ret::Inferred));
                let count = o.count.as_ref().and_then(|e| {
                    let mut cx = Cx::new(m, file, Some(&scope));
                    self.const_int(e, &mut cx, "`count`", 1)
                });
                let value = self.objective_init(*ospan);
                let ty = match self.builtins.objective {
                    Some(d) => named(TypeName::Def(d), *ospan),
                    None => hir::Type {
                        kind: hir::TypeKind::Error,
                        span: *ospan,
                    },
                };
                self.set_kind(
                    *global,
                    DefKind::Global(hir::Global {
                        ty: Some(ty),
                        value,
                    }),
                );
                objectives.push(hir::ObjectiveDef {
                    name: o.name.clone(),
                    text,
                    at,
                    count,
                    optional: o.optional,
                    global: *global,
                });
            }
            let mut transitions = Vec::new();
            for (to, cond, tspan) in &st.transitions {
                match scope.stage(to) {
                    Some(t) => transitions.push((
                        t,
                        self.synth(&mut qs, PureSite::Transition(t), cond, Ret::Bool),
                    )),
                    None => self.diags.push(error(
                        "jump-target",
                        format!("`-> {to}`: this quest has no stage `{to}`"),
                        file,
                        *tspan,
                    )),
                }
            }
            let dialogues = st
                .dialogues
                .iter()
                .map(|(dl, it, d)| {
                    let h = self.dialogue(dl, it, m, Some((&scope, Some(s))));
                    self.set_kind(*d, DefKind::Dialogue(h));
                    *d
                })
                .collect();
            let handlers = st
                .fns
                .iter()
                .map(|(f, it, d)| self.quest_fn(f, it, *d, m, &scope, true, &generated))
                .collect();
            stage_defs.push(hir::StageDef {
                name: st.name.to_string(),
                objectives,
                transitions,
                dialogues,
                handlers,
                span: st.span,
            });
        }

        // The quest body's dialogues and handlers.
        let body_dialogues = dialogues
            .iter()
            .map(|(dl, it, d)| {
                let h = self.dialogue(dl, it, m, Some((&scope, None)));
                self.set_kind(*d, DefKind::Dialogue(h));
                *d
            })
            .collect();
        let handlers = fns
            .iter()
            .map(|(f, it, d)| self.quest_fn(f, it, *d, m, &scope, false, &generated))
            .collect();

        let role_names: Vec<&str> = role_defs.iter().map(|r| r.name.as_str()).collect();
        let parts = self.parts(&parts, &role_names, &scope, m, id, file, &generated);
        let home = match &q.at {
            Some(name) => self.role_index(&role_names, name, file, span),
            None => RoleIdx(0),
        };
        let repeatable = self.quest_options(q, file, span);
        hir::Quest {
            name: q.name.clone(),
            repeatable,
            home,
            roles: role_defs,
            requires,
            stages: stage_defs,
            parts,
            globals,
            events: event_defs,
            handlers,
            dialogues: body_dialogues,
            generated,
        }
    }

    /// `quest-placement`: in the crate root, at most one.
    fn quest_placement(&mut self, item: &ast::Item, m: ModId, file: &str) {
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
    }

    /// A member with a source item: an item of the quest named `name`.
    fn member_def(
        &mut self,
        m: ModId,
        name: &str,
        member: &'a ast::Item,
        tag: Tag,
        quest: DefId,
    ) -> DefId {
        self.add_member(
            m,
            name.to_string(),
            member.span,
            tag,
            Some(member),
            Some(quest),
        )
    }

    /// Enters `name` in the quest's names; a second one is `duplicate-def`.
    fn quest_name(
        &mut self,
        scope: &mut QuestScope,
        name: &str,
        what: QName,
        file: &str,
        at: ast::Span,
    ) {
        if scope.names.contains_key(name) {
            self.diags.push(error(
                "duplicate-def",
                format!("`{name}` is defined twice in this quest"),
                file,
                at,
            ));
        } else {
            scope.names.insert(name.to_string(), what);
        }
    }

    /// A member of a stage: an objective (its global, a quest-level name), a transition, a dialogue or a
    /// handler.
    fn stage_member(
        &mut self,
        sm: &'a ast::Item,
        st: &mut StageMembers<'a>,
        scope: &mut QuestScope,
        m: ModId,
        quest: DefId,
        file: &str,
    ) {
        match &sm.kind {
            ast::ItemKind::Objective(o) => {
                let d = self.add_member(
                    m,
                    format!("{}.{}", st.name, o.name),
                    sm.span,
                    Tag::Global,
                    None,
                    Some(quest),
                );
                self.quest_name(scope, &o.name, QName::Def(d), file, sm.span);
                scope
                    .objectives
                    .insert((st.name.to_string(), o.name.clone()), d);
                st.objectives.push((o, d, sm.span));
            }
            ast::ItemKind::Transition(to, cond) => st.transitions.push((to, cond, sm.span)),
            ast::ItemKind::Dialogue(dl) | ast::ItemKind::Scene(dl) => {
                let d = self.member_def(m, &dl.name, sm, Tag::Dialogue, quest);
                self.quest_name(scope, &dl.name, QName::Def(d), file, sm.span);
                st.dialogues.push((dl, sm, d));
            }
            ast::ItemKind::Fn(f) => {
                let d = self.member_def(m, &f.name, sm, Tag::Fn, quest);
                self.quest_name(scope, &f.name, QName::Def(d), file, sm.span);
                st.fns.push((f, sm, d));
            }
            // The grammar puts nothing else in a stage.
            _ => {}
        }
    }

    /// `Objective(state=0, count=0)`.
    fn objective_init(&mut self, at: ast::Span) -> hir::Expr {
        let Some(d) = self.builtins.objective else {
            return self.mk_expr(hir::ExprKind::Error, at);
        };
        let callee = self.mk_expr(hir::ExprKind::Def(d), at);
        let args = ["state", "count"]
            .into_iter()
            .map(|name| hir::Arg {
                name: Some(name.to_string()),
                value: self.mk_expr(hir::ExprKind::Int(0), at),
            })
            .collect();
        self.mk_expr(hir::ExprKind::Call(Box::new(callee), args), at)
    }

    /// A quest's (or a part's) handler, item `d`. `@on(Entered)` belongs in a stage
    /// (`entered-outside-stage`).
    #[allow(clippy::too_many_arguments)]
    fn quest_fn(
        &mut self,
        f: &ast::FnDef,
        item: &ast::Item,
        d: DefId,
        m: ModId,
        scope: &QuestScope,
        in_stage: bool,
        generated: &QuestItems,
    ) -> DefId {
        let file = self.file_of(m);
        let mut cx = Cx::new(m, file, Some(scope));
        cx.quest_fn = true;
        let h = self.func(f, item, None, false, &mut cx);
        if !in_stage && h.handler == Some(EventRef::Script(generated.entered)) {
            let at = item
                .decorators
                .iter()
                .find(|dec| dec.name == "on")
                .map_or(item.span, |dec| dec.span);
            self.diags.push(error(
                "entered-outside-stage",
                "`Entered` is delivered when a stage becomes current: handle it inside a `stage`"
                    .to_string(),
                file,
                at,
            ));
        }
        self.set_kind(d, DefKind::Fn(h));
        d
    }

    fn role_index(&mut self, roles: &[&str], name: &str, file: &str, at: ast::Span) -> RoleIdx {
        match roles.iter().position(|r| *r == name) {
            Some(i) => RoleIdx(idx(i)),
            None => {
                self.diags.push(super::unresolved(
                    format!("this quest has no role `{name}`"),
                    name,
                    roles.iter().copied(),
                    file,
                    at,
                ));
                RoleIdx(0)
            }
        }
    }

    /// The parts: their own `let`s, events and handlers (`Q::<part>::<name>`) over the quest's names; a
    /// stage, objective, dialogue, scene, part, role or `requires` inside one is `bad-part-item`.
    #[allow(clippy::too_many_arguments)]
    fn parts(
        &mut self,
        parts: &[(&'a ast::Part, ast::Span)],
        roles: &[&str],
        scope: &QuestScope,
        m: ModId,
        quest: DefId,
        file: &str,
        generated: &QuestItems,
    ) -> Vec<hir::PartDef> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for (p, pspan) in parts {
            if !seen.insert(p.name.as_str()) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("part `{}` is defined twice", p.name),
                    file,
                    *pspan,
                ));
            }
            let role = self.role_index(roles, &p.role, file, *pspan);
            let mut ps = scope.clone();
            let mut lets = Vec::new();
            let mut events = Vec::new();
            let mut fns = Vec::new();
            for member in &p.body {
                let own = |r: &mut Self, name: &str, tag, ps: &mut QuestScope| {
                    let d = r.add_member(
                        m,
                        format!("{}::{name}", p.name),
                        member.span,
                        tag,
                        Some(member),
                        Some(quest),
                    );
                    r.quest_name(ps, name, QName::Def(d), file, member.span);
                    d
                };
                match &member.kind {
                    ast::ItemKind::Let(l) => lets.push((l, own(self, &l.name, Tag::Global, &mut ps))),
                    ast::ItemKind::Event(name, fields) => {
                        self.reserved_event(name, file, member.span);
                        events.push((fields, own(self, name, Tag::Event, &mut ps)));
                    }
                    ast::ItemKind::Fn(f) => {
                        fns.push((f, member, own(self, &f.name, Tag::Fn, &mut ps)));
                    }
                    _ => self.diags.push(error(
                        "bad-part-item",
                        "a part holds `let`s, `event`s and handlers: stages, objectives, dialogues, \
                         scenes, roles, `requires` and parts belong in the quest"
                            .to_string(),
                        file,
                        member.span,
                    )),
                }
            }
            let globals = lets
                .iter()
                .map(|(l, d)| {
                    let mut cx = Cx::new(m, file, Some(&ps));
                    let g = self.global(l, &mut cx);
                    self.set_kind(*d, DefKind::Global(g));
                    *d
                })
                .collect();
            let events = events
                .iter()
                .map(|(fields, d)| {
                    let mut cx = Cx::new(m, file, Some(&ps));
                    let f = self.fields(fields, &mut cx);
                    self.set_kind(*d, DefKind::Event(f));
                    *d
                })
                .collect();
            let handlers = fns
                .iter()
                .map(|(f, it, d)| self.quest_fn(f, it, *d, m, &ps, false, generated))
                .collect();
            out.push(hir::PartDef {
                name: p.name.clone(),
                anchor: p.anchor,
                role,
                globals,
                events,
                handlers,
                span: *pspan,
            });
        }
        out
    }

    /// `repeatable quest Q(cooldown=…, exclusive=…, once_per_player=…)` (`bad-quest-option`): known names,
    /// constant values of their type, only on a `repeatable` quest.
    fn quest_options(
        &mut self,
        q: &ast::Quest,
        file: &str,
        at: ast::Span,
    ) -> Option<hir::RepeatOpts> {
        if !q.repeatable {
            if let Some(first) = q.options.first() {
                self.diags.push(error(
                    "bad-quest-option",
                    "options belong to a `repeatable quest`; a one-off quest takes none"
                        .to_string(),
                    file,
                    if first.value.span.line == 0 {
                        at
                    } else {
                        first.value.span
                    },
                ));
            }
            return None;
        }
        let mut opts = hir::RepeatOpts {
            cooldown: 0,
            exclusive: false,
            once_per_player: false,
        };
        let mut seen = BTreeSet::new();
        for a in &q.options {
            let known = a
                .name
                .as_deref()
                .and_then(|n| QUEST_OPTIONS.iter().find(|(o, _)| *o == n));
            let Some((name, ty)) = known else {
                let names: Vec<&str> = QUEST_OPTIONS.iter().map(|(n, _)| *n).collect();
                self.diags.push(error(
                    "bad-quest-option",
                    format!(
                        "a quest's options are named: {}",
                        names
                            .iter()
                            .map(|n| format!("`{n}=…`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    file,
                    a.value.span,
                ));
                continue;
            };
            if !seen.insert(*name) {
                self.diags.push(error(
                    "bad-quest-option",
                    format!("`{name}` is given twice"),
                    file,
                    a.value.span,
                ));
                continue;
            }
            match (ty, &a.value.kind) {
                (OptionTy::Int, ast::ExprKind::Int(n)) if *n >= 0 => opts.cooldown = *n,
                (OptionTy::Bool, ast::ExprKind::Bool(b)) => {
                    if *name == "exclusive" {
                        opts.exclusive = *b;
                    } else {
                        opts.once_per_player = *b;
                    }
                }
                (OptionTy::Int, _) => self.diags.push(error(
                    "bad-quest-option",
                    format!(
                        "`{name}` takes a constant number of ticks (an int literal, 0 or more)"
                    ),
                    file,
                    a.value.span,
                )),
                (OptionTy::Bool, _) => self.diags.push(error(
                    "bad-quest-option",
                    format!("`{name}` takes `true` or `false`"),
                    file,
                    a.value.span,
                )),
            }
        }
        Some(opts)
    }
}
