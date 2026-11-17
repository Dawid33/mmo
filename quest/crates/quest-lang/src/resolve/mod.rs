//! Name resolution (spec §5.1, §5.10; plan D10 stage `resolve`): turns the module tree into the HIR,
//! where every name is an item, a binding, a host item or a prelude item.
//!
//! Passes, each over the modules in path order and the items in declaration order:
//! 1. **Collect:** every item gets its [`DefId`] and its name in its module's namespace — one namespace per
//!    module for every kind of item, `mod` and `use` included (`duplicate-def`).
//! 2. **Imports:** every `use` is resolved to what it names, repeatedly until nothing changes, so `use`
//!    cycles between modules are fine; an import that only reaches itself is `unresolved`.
//! 3. **Impls:** each `impl` finds its struct or enum, and its methods enter [`hir::Crate::methods`].
//! 4. **Bodies:** types, functions, globals, dialogues and quests. Every expression of a dialogue becomes a
//!    synthesised pure `fn` (`dialogue.rs`); a quest's members and generated names become items whose
//!    parent is the quest (`quest.rs`; see `hir/dialogue.rs`).
//!
//! Passes 1–3 live in `items.rs`, dialogues in `dialogue.rs`, quests in `quest.rs`; this file holds pass 4:
//! lookup, types, statements, patterns and expressions.
//!
//! Lookup of a single name: the bindings in scope (innermost first), then — inside a quest — the quest's
//! own names (a part's first) and builtins (`join`, `complete`, …), inside a dialogue's condition
//! `dialogue_depth`, then the module's items and imports, then the compiler's own items (`Objective`,
//! `Talked`, `DialogueEnded`, `SceneEnded`, `EndReason`, `SceneEnd`), then the host's functions, namespaces,
//! structs and events, then `after`, `cancel` and `play`, then the prelude. The callee of the statement
//! `emit Ev(…)` is always the host's `emit`, whatever a script names `emit`. A path `a::b::c` starts at `crate`, `self`, `super` or an item of the module
//! and walks modules, then an enum's variant or a struct's / enum's method. An item is visible from its own
//! module and that module's descendants; from anywhere else only if `pub` (`private`).
//!
//! Error names (spec §5.7) are global, never declared, and a space of their own. One rule where an error
//! is expected — the head of a `raise` and a pattern inside `Err(…)`: an upper-case name is always an
//! error name, even when an item has that name; a lower-case one is a binding (in `raise`, one that must be
//! in scope). Elsewhere in a pattern a lower-case name (or `_x`) binds; an upper-case name is the struct,
//! event, variant or `Some` / `None` / `Ok` / `Err` it names, and one that names nothing becomes
//! `Ctor::ErrorOrUnknown`, which the type checker resolves against the scrutinee's type (an error name, or
//! `unresolved`).

mod dialogue;
mod items;
mod quest;

use crate::ast;
use crate::diag::Diagnostic;
use crate::hir::{
    self, BUILTIN_TYPES, Ctor, DefId, DefKind, EventRef, HostItem, LocalId, ModId, PRELUDE,
    Prelude, TypeName,
};
use crate::modules::ROOT_FILE;
use crate::parse::EMIT_CALLEE;
use items::HostNames;
use quest::{QName, QuestScope};
use quest_vm::host::HostApi;
use std::collections::{BTreeMap, BTreeSet};

/// The furthest (in edits) a name may be from a name in scope for `unresolved` to suggest it.
const SUGGEST_DISTANCE_MAX: usize = 2;

/// Names longer than this (in bytes) get no suggestion: the edit distance is quadratic in length.
const SUGGEST_LEN_MAX: usize = 64;

/// Path words with a fixed meaning.
const PATH_WORDS: [&str; 3] = ["crate", "self", "super"];

/// The module tree's root.
const ROOT: ModId = ModId(0);

/// Resolves the module tree against the host's API.
pub fn resolve(krate: &ast::Crate, host: &HostApi) -> (hir::Crate, Vec<Diagnostic>) {
    let host_enums = items::host_enum_items(host);
    let mut r = Resolver::new(krate, host);
    r.collect();
    r.collect_host_enums(&host_enums);
    r.collect_builtins();
    r.resolve_imports();
    r.resolve_impls();
    r.resolve_bodies();
    let kinds = std::mem::take(&mut r.kinds);
    let defs = r
        .metas
        .iter()
        .zip(kinds)
        .map(|(m, kind)| hir::Def {
            name: m.name.clone(),
            module: m.module,
            public: m.public,
            span: m.span,
            parent: m.parent,
            // Every item's kind is set by `resolve_bodies` or where it was generated.
            kind: kind.unwrap_or(DefKind::Struct(Vec::new())),
        })
        .collect();
    let out = hir::Crate {
        modules: r.modules,
        defs,
        locals: r.locals,
        methods: r.methods,
        host_enums: r.host.enum_defs.clone(),
        builtins: r.builtins,
        expr_count: r.expr_count,
        pat_count: r.pat_count,
    };
    (out, r.diags)
}

/// What a name means outside the bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Res {
    Def(DefId),
    Mod(ModId),
    Variant(DefId, u32),
    Host(HostItem),
    /// A host namespace (`world`): only `world.<fn>` means something.
    Namespace,
    Prelude(Prelude),
    /// A function only the compiler lowers.
    Quest(hir::QuestFn),
    /// A dialogue's exit.
    Exit(DefId, u32),
    /// A quest's stage name: not a value (`Stage::Name` is).
    Stage(hir::StageIdx),
    /// Something already reported.
    Error,
}

/// A module namespace entry: what it means, or the import that will say.
#[derive(Debug, Clone, Copy)]
enum Slot {
    Res(Res),
    Import(usize),
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    slot: Slot,
    public: bool,
    span: ast::Span,
}

struct Import<'a> {
    module: ModId,
    path: &'a [String],
    span: ast::Span,
    /// A `pub use`, which may re-export only `pub` items.
    public: bool,
    done: Option<Res>,
}

/// The kind of an item, known from collection on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Fn,
    Struct,
    Enum,
    ErrorSet,
    Global,
    Event,
    Dialogue,
    Quest,
}

impl Tag {
    fn noun(self) -> &'static str {
        match self {
            Tag::Fn => "a function",
            Tag::Struct => "a struct",
            Tag::Enum => "an enum",
            Tag::ErrorSet => "an error set",
            Tag::Global => "a variable",
            Tag::Event => "an event",
            Tag::Dialogue => "a dialogue",
            Tag::Quest => "a quest",
        }
    }
}

/// An item's facts from collection: everything but its resolved body.
struct Meta<'a> {
    name: String,
    module: ModId,
    public: bool,
    span: ast::Span,
    tag: Tag,
    /// The source item; `None` for a generated one (a quest's names, a synthesised fn, a compiler builtin).
    item: Option<&'a ast::Item>,
    /// The `impl` a method is in.
    impl_index: Option<usize>,
    /// The quest a quest's item belongs to.
    parent: Option<DefId>,
}

struct Impl<'a> {
    module: ModId,
    name: &'a str,
    span: ast::Span,
    methods: Vec<DefId>,
    owner: Option<DefId>,
}

/// Where an expression is resolved: its module, its bindings and what encloses it.
struct Cx<'q> {
    module: ModId,
    file: &'q str,
    /// The bindings in scope, innermost last.
    locals: Vec<(String, LocalId)>,
    /// Where each open scope starts in `locals`.
    marks: Vec<usize>,
    quest: Option<&'q QuestScope>,
    /// Inside a dialogue's synthesised fn, where `dialogue_depth()` is known.
    in_dialogue: bool,
    /// Inside a quest's handler, where `-> Stage` changes the stage.
    quest_fn: bool,
    /// Inside a `Setup` handler, where `decline()` is allowed.
    in_setup: bool,
}

impl<'q> Cx<'q> {
    fn new(module: ModId, file: &'q str, quest: Option<&'q QuestScope>) -> Cx<'q> {
        Cx {
            module,
            file,
            locals: Vec::new(),
            marks: Vec::new(),
            quest,
            in_dialogue: false,
            quest_fn: false,
            in_setup: false,
        }
    }

    fn push(&mut self) {
        self.marks.push(self.locals.len());
    }

    fn pop(&mut self) {
        if let Some(mark) = self.marks.pop() {
            self.locals.truncate(mark);
        }
    }

    fn local(&self, name: &str) -> Option<LocalId> {
        self.locals
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }
}

/// The bindings of one `case` pattern: one id per name, shared by `|` alternatives, and the names the
/// current alternative has bound so far (a second one is `duplicate-def`).
#[derive(Default)]
struct PatState {
    binds: BTreeMap<String, LocalId>,
    seen: BTreeSet<String>,
}

/// Which items a path may end at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A `use`: modules, then an item of the last module.
    Use,
    /// An expression, a type or a pattern: also an enum's variant or a type's method.
    Item,
}

/// The end of a path walk: what it reached and what it found wrong on the way, or that it reached an
/// import not resolved yet.
enum Walk {
    Found(Res, Vec<Diagnostic>),
    Pending,
}

struct Resolver<'a> {
    krate: &'a ast::Crate,
    host: HostNames,
    diags: Vec<Diagnostic>,
    modules: Vec<hir::Module>,
    module_ids: BTreeMap<&'a [String], ModId>,
    scopes: Vec<BTreeMap<&'a str, Entry>>,
    imports: Vec<Import<'a>>,
    metas: Vec<Meta<'a>>,
    impls: Vec<Impl<'a>>,
    methods: BTreeMap<(DefId, String), DefId>,
    locals: Vec<hir::Local>,
    /// Each item's resolved kind, by id: set by `resolve_bodies`, or where a generated item is made.
    kinds: Vec<Option<DefKind>>,
    builtins: hir::Builtins,
    /// The compiler's own items by name (`builtins`).
    builtin_names: BTreeMap<&'static str, DefId>,
    /// Every line id used so far, with where.
    line_ids: BTreeMap<String, (String, ast::Span)>,
    root_quest_seen: bool,
    /// The next [`hir::ExprId`].
    expr_count: u32,
    /// The next [`hir::PatId`].
    pat_count: u32,
}

fn index_u32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn is_binding_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
}

/// The number of single-character edits between `a` and `b` (Levenshtein).
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = Vec::with_capacity(prev.len());
        cur.push(i.saturating_add(1));
        for (j, cb) in b.iter().enumerate() {
            let substitute = prev
                .get(j)
                .map_or(usize::MAX, |d| d.saturating_add(usize::from(ca != *cb)));
            let delete = prev
                .get(j.saturating_add(1))
                .map_or(usize::MAX, |d| d.saturating_add(1));
            let insert = cur.last().map_or(usize::MAX, |d| d.saturating_add(1));
            cur.push(substitute.min(delete).min(insert));
        }
        prev = cur;
    }
    prev.last().copied().unwrap_or(0)
}

/// The candidate nearest to `name` within [`SUGGEST_DISTANCE_MAX`] edits: smallest distance, then
/// alphabetical.
pub(crate) fn suggest<'n>(
    name: &str,
    candidates: impl IntoIterator<Item = &'n str>,
) -> Option<&'n str> {
    if name.len() > SUGGEST_LEN_MAX {
        return None;
    }
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        if c == name
            || c.len() > SUGGEST_LEN_MAX
            || c.len().abs_diff(name.len()) > SUGGEST_DISTANCE_MAX
        {
            continue;
        }
        let d = edit_distance(name, c);
        if d <= SUGGEST_DISTANCE_MAX && best.is_none_or(|b| (d, c) < b) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

/// The diagnostic `unresolved` for `name`, with a "did you mean" note when a candidate is near.
fn unresolved<'n>(
    message: String,
    name: &str,
    candidates: impl IntoIterator<Item = &'n str>,
    file: &str,
    at: ast::Span,
) -> Diagnostic {
    let mut d = Diagnostic::error("unresolved", message, file, at.line, at.col, at.len);
    if let Some(near) = suggest(name, candidates) {
        d.notes.push(format!("did you mean `{near}`?"));
    }
    d
}

fn error(code: &'static str, message: String, file: &str, at: ast::Span) -> Diagnostic {
    Diagnostic::error(code, message, file, at.line, at.col, at.len)
}

impl<'a> Resolver<'a> {
    fn new(krate: &'a ast::Crate, host: &HostApi) -> Resolver<'a> {
        Resolver {
            krate,
            host: HostNames::new(host),
            diags: Vec::new(),
            modules: Vec::new(),
            module_ids: BTreeMap::new(),
            scopes: Vec::new(),
            imports: Vec::new(),
            metas: Vec::new(),
            impls: Vec::new(),
            methods: BTreeMap::new(),
            locals: Vec::new(),
            kinds: Vec::new(),
            builtins: hir::Builtins::default(),
            builtin_names: BTreeMap::new(),
            line_ids: BTreeMap::new(),
            root_quest_seen: false,
            expr_count: 0,
            pat_count: 0,
        }
    }

    /// An expression with the next id.
    fn mk_expr(&mut self, kind: hir::ExprKind, span: ast::Span) -> hir::Expr {
        let id = hir::ExprId(self.expr_count);
        self.expr_count = self.expr_count.saturating_add(1);
        hir::Expr { id, kind, span }
    }

    fn file_of(&self, m: ModId) -> &'a str {
        self.krate
            .modules
            .get(m.0 as usize)
            .map_or(ROOT_FILE, |m| m.file.as_str())
    }

    fn parent_of(&self, m: ModId) -> Option<ModId> {
        self.modules.get(m.0 as usize).and_then(|m| m.parent)
    }

    /// `crate` for the root, else the module's path.
    fn module_name(&self, m: ModId) -> String {
        match self.modules.get(m.0 as usize) {
            Some(module) if !module.path.is_empty() => module.path.join("::"),
            _ => "crate".to_string(),
        }
    }

    /// Whether `inner` is `outer` or inside it: what may see `outer`'s private items.
    fn is_within(&self, inner: ModId, outer: ModId) -> bool {
        let mut cur = Some(inner);
        while let Some(c) = cur {
            if c == outer {
                return true;
            }
            cur = self.parent_of(c);
        }
        false
    }

    fn tag(&self, d: DefId) -> Option<Tag> {
        self.metas.get(d.0 as usize).map(|m| m.tag)
    }

    fn new_local(&mut self, name: &str, span: ast::Span) -> LocalId {
        let id = LocalId(index_u32(self.locals.len()));
        self.locals.push(hir::Local {
            name: name.to_string(),
            span,
        });
        id
    }

    fn bind_local(&mut self, cx: &mut Cx, name: &str, span: ast::Span) -> LocalId {
        let id = self.new_local(name, span);
        cx.locals.push((name.to_string(), id));
        id
    }

    // Pass 4: bodies.

    /// Sets the kind of item `d`.
    fn set_kind(&mut self, d: DefId, kind: DefKind) {
        let i = d.0 as usize;
        if self.kinds.len() <= i {
            self.kinds.resize(i.saturating_add(1), None);
        }
        if let Some(slot) = self.kinds.get_mut(i) {
            *slot = Some(kind);
        }
    }

    /// Resolves every collected item that has no kind yet; a quest's and a dialogue's items (and their
    /// synthesised fns) are added and given their kinds on the way.
    fn resolve_bodies(&mut self) {
        let collected = self.metas.len();
        if self.kinds.len() < collected {
            self.kinds.resize(collected, None);
        }
        for i in 0..collected {
            let id = DefId(index_u32(i));
            if self.kinds.get(i).is_some_and(Option::is_some) {
                continue;
            }
            let Some(meta) = self.metas.get(i) else { break };
            let (m, impl_index) = (meta.module, meta.impl_index);
            let Some(item) = meta.item else { continue };
            let file = self.file_of(m);
            let kind = match &item.kind {
                ast::ItemKind::Fn(f) => {
                    let owner = impl_index
                        .and_then(|k| self.impls.get(k))
                        .and_then(|imp| imp.owner);
                    let mut cx = Cx::new(m, file, None);
                    DefKind::Fn(self.func(f, item, owner, impl_index.is_some(), &mut cx))
                }
                ast::ItemKind::Struct(_, fields) => {
                    DefKind::Struct(self.fields(fields, &mut Cx::new(m, file, None)))
                }
                ast::ItemKind::Event(_, fields) => {
                    DefKind::Event(self.fields(fields, &mut Cx::new(m, file, None)))
                }
                ast::ItemKind::Enum(_, variants) => {
                    let mut cx = Cx::new(m, file, None);
                    DefKind::Enum(self.variants(variants, &mut cx))
                }
                ast::ItemKind::ErrorSet(_, members) => {
                    DefKind::ErrorSet(self.error_set(members, &mut Cx::new(m, file, None)))
                }
                ast::ItemKind::Let(l) => {
                    let mut cx = Cx::new(m, file, None);
                    DefKind::Global(self.global(l, &mut cx))
                }
                ast::ItemKind::Dialogue(d) | ast::ItemKind::Scene(d) => {
                    DefKind::Dialogue(self.dialogue(d, item, m, None))
                }
                ast::ItemKind::Quest(q) => DefKind::Quest(self.quest(q, item, m, id)),
                // Collected items are only the kinds above.
                _ => continue,
            };
            self.set_kind(id, kind);
        }
    }

    fn variants(&mut self, variants: &[ast::Variant], cx: &mut Cx) -> Vec<hir::Variant> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for v in variants {
            if !seen.insert(v.name.as_str()) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("variant `{}` is defined twice", v.name),
                    cx.file,
                    v.span,
                ));
            }
            out.push(self.variant(v, cx));
        }
        out
    }

    fn global(&mut self, l: &ast::Let, cx: &mut Cx) -> hir::Global {
        let ty = l.ty.as_ref().map(|t| self.ty(t, cx));
        let value = self.expr(&l.value, cx);
        hir::Global { ty, value }
    }

    fn fields(&mut self, fields: &[ast::Field], cx: &mut Cx) -> Vec<hir::Field> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for f in fields {
            if !seen.insert(f.name.as_str()) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("field `{}` is defined twice", f.name),
                    cx.file,
                    f.span,
                ));
            }
            out.push(hir::Field {
                name: f.name.clone(),
                ty: self.ty(&f.ty, cx),
                span: f.span,
            });
        }
        out
    }

    fn variant(&mut self, v: &ast::Variant, cx: &mut Cx) -> hir::Variant {
        hir::Variant {
            name: v.name.clone(),
            fields: self.fields(&v.fields, cx),
            span: v.span,
        }
    }

    fn error_set(&mut self, members: &[ast::Variant], cx: &mut Cx) -> Vec<hir::ErrorMember> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for v in members {
            if !seen.insert(v.name.as_str()) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("`{}` is listed twice in this error set", v.name),
                    cx.file,
                    v.span,
                ));
            }
            let set = if v.fields.is_empty() {
                match self.entry(cx.module, &v.name) {
                    Some((Some(Res::Def(d)), _)) if self.tag(d) == Some(Tag::ErrorSet) => Some(d),
                    _ => None,
                }
            } else {
                None
            };
            out.push(match set {
                Some(d) => hir::ErrorMember::Set(d, v.span),
                None => hir::ErrorMember::Error(self.variant(v, cx)),
            });
        }
        out
    }

    /// A function or method. `cx` holds no bindings yet; the parameters open its scope.
    fn func(
        &mut self,
        f: &ast::FnDef,
        item: &ast::Item,
        owner: Option<DefId>,
        is_method: bool,
        cx: &mut Cx,
    ) -> hir::Fn {
        let (handler, unknown_event) = self.fn_decorators(item, is_method, cx);
        cx.in_setup =
            matches!(handler, Some(EventRef::Host(i)) if self.host.events.get("Setup") == Some(&i));
        cx.push();
        let receiver = f
            .receiver
            .map(|r| (r, self.bind_local(cx, "self", item.span)));
        let mut seen = BTreeSet::new();
        let mut params = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            if !seen.insert(p.name.as_str()) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("parameter `{}` is declared twice", p.name),
                    cx.file,
                    p.span,
                ));
            }
            let mut ty = p.ty.as_ref().map(|t| self.ty(t, cx));
            if unknown_event && i == 0 && ty.is_none() {
                // The `@on` was reported: its event parameter is not asked about again.
                ty = Some(hir::Type {
                    kind: hir::TypeKind::Error,
                    span: p.span,
                });
            }
            let local = self.bind_local(cx, &p.name, p.span);
            params.push(hir::Param {
                local,
                ty,
                span: p.span,
            });
        }
        let ret = f.ret.as_ref().map(|t| self.ty(t, cx));
        let raises = f
            .raises
            .as_ref()
            .and_then(|p| self.raises(p, item.span, cx));
        let body = self.stmts(&f.body, cx);
        cx.pop();
        hir::Fn {
            owner,
            receiver,
            params,
            ret,
            raises,
            handler,
            body,
            pure: None,
        }
    }

    fn raises(&mut self, path: &ast::Path, at: ast::Span, cx: &mut Cx) -> Option<hir::Raises> {
        let res = if let [name] = path.as_slice() {
            match self.lookup(cx, name) {
                None => return Some(hir::Raises::Error(name.clone())),
                Some(res) => res,
            }
        } else {
            self.walk_here(cx, path, at)
        };
        match res {
            Res::Def(d) if self.tag(d) == Some(Tag::ErrorSet) => Some(hir::Raises::Set(d)),
            Res::Error => None,
            _ => {
                self.diags.push(error(
                    "unresolved",
                    format!(
                        "`raises {}`: not an error set or an error name",
                        path.join("::")
                    ),
                    cx.file,
                    at,
                ));
                None
            }
        }
    }

    /// Checks a function's decorators; returns the event of its `@on`, and whether an `@on` named no event
    /// (reported here).
    fn fn_decorators(
        &mut self,
        item: &ast::Item,
        is_method: bool,
        cx: &mut Cx,
    ) -> (Option<EventRef>, bool) {
        let mut handler = None;
        let mut unknown = false;
        let mut seen_on = false;
        for dec in &item.decorators {
            match dec.name.as_str() {
                "on" if is_method => self.diags.push(error(
                    "bad-decorator",
                    "`@on` makes a free `fn` a handler, not a method".to_string(),
                    cx.file,
                    dec.span,
                )),
                "on" if seen_on => self.diags.push(error(
                    "bad-decorator",
                    "a `fn` handles one event: one `@on`".to_string(),
                    cx.file,
                    dec.span,
                )),
                "on" => {
                    seen_on = true;
                    handler = self.handler_event(dec, cx);
                    unknown = handler.is_none();
                }
                "pick" => self.diags.push(error(
                    "bad-decorator",
                    "`@pick` applies to a `dialogue`, not a `fn`".to_string(),
                    cx.file,
                    dec.span,
                )),
                other => self.diags.push(error(
                    "bad-decorator",
                    format!("unknown decorator `@{other}`"),
                    cx.file,
                    dec.span,
                )),
            }
        }
        (handler, unknown)
    }

    /// The event `@on(…)` names, or `unknown-event`.
    fn handler_event(&mut self, dec: &ast::Decorator, cx: &mut Cx) -> Option<EventRef> {
        let path = match dec.args.as_slice() {
            [ast::Arg { name: None, value }] => match &value.kind {
                ast::ExprKind::Path(p) => Some(p),
                _ => None,
            },
            _ => None,
        };
        let Some(path) = path else {
            self.diags.push(error(
                "unknown-event",
                "`@on` takes one event name: `@on(PlayerClicked)`".to_string(),
                cx.file,
                dec.span,
            ));
            return None;
        };
        let res = if let [name] = path.as_slice() {
            self.lookup(cx, name)
        } else {
            Some(self.walk_here(cx, path, dec.span))
        };
        match res {
            Some(Res::Def(d)) if self.tag(d) == Some(Tag::Event) => Some(EventRef::Script(d)),
            Some(Res::Host(HostItem::Event(i))) => Some(EventRef::Host(i)),
            Some(Res::Error) => None,
            _ => {
                let name = path.join("::");
                let mut d = error(
                    "unknown-event",
                    format!("`{name}` is not an event"),
                    cx.file,
                    dec.span,
                );
                let events = self.event_names(cx);
                if let Some(near) = suggest(&name, events.iter().map(String::as_str)) {
                    d.notes.push(format!("did you mean `{near}`?"));
                }
                self.diags.push(d);
                None
            }
        }
    }

    /// The events a single name may name from `cx`.
    fn event_names(&self, cx: &Cx) -> Vec<String> {
        let mut names: Vec<String> = self.host.events.keys().cloned().collect();
        names.extend(
            self.builtin_names
                .iter()
                .filter(|(_, d)| self.tag(**d) == Some(Tag::Event))
                .map(|(n, _)| n.to_string()),
        );
        if let Some(scope) = self.scopes.get(cx.module.0 as usize) {
            for (name, entry) in scope {
                if let Some(Res::Def(d)) = self.slot_res(entry.slot)
                    && self.tag(d) == Some(Tag::Event)
                {
                    names.push(name.to_string());
                }
            }
        }
        if let Some(q) = cx.quest {
            names.extend(
                q.names
                    .iter()
                    .filter(|(_, k)| matches!(k, QName::Def(d) if self.tag(*d) == Some(Tag::Event)))
                    .map(|(n, _)| n.clone()),
            );
        }
        names
    }

    // Names.

    /// What a single name means outside the bindings.
    fn lookup(&self, cx: &Cx, name: &str) -> Option<Res> {
        if name == EMIT_CALLEE {
            return self
                .host
                .free
                .get("emit")
                .map(|i| Res::Host(HostItem::Fn(*i)));
        }
        if let Some(q) = cx.quest {
            match q.names.get(name) {
                Some(QName::Def(d)) => return Some(Res::Def(*d)),
                Some(QName::Stage(s)) => return Some(Res::Stage(*s)),
                None => {}
            }
            if let Some(f) = hir::QUEST_FNS.iter().find(|f| f.name() == name) {
                return Some(Res::Quest(*f));
            }
        }
        if cx.in_dialogue && name == hir::QuestFn::DialogueDepth.name() {
            return Some(Res::Quest(hir::QuestFn::DialogueDepth));
        }
        if let Some((res, _)) = self.entry(cx.module, name) {
            return Some(res.unwrap_or(Res::Error));
        }
        if let Some(d) = self.builtin_names.get(name) {
            return Some(Res::Def(*d));
        }
        self.host
            .lookup(name)
            .or_else(|| {
                hir::SCRIPT_FNS
                    .iter()
                    .find(|f| f.name() == name)
                    .map(|f| Res::Quest(*f))
            })
            .or_else(|| Prelude::from_name(name).map(Res::Prelude))
    }

    /// Every single name `cx` sees, for suggestions.
    fn value_names(&self, cx: &Cx) -> Vec<String> {
        let mut names: Vec<String> = cx.locals.iter().map(|(n, _)| n.clone()).collect();
        if let Some(q) = cx.quest {
            names.extend(q.names.keys().cloned());
            names.extend(hir::QUEST_FNS.iter().map(|f| f.name().to_string()));
        }
        if cx.in_dialogue {
            names.push(hir::QuestFn::DialogueDepth.name().to_string());
        }
        if let Some(scope) = self.scopes.get(cx.module.0 as usize) {
            names.extend(scope.keys().map(|s| s.to_string()));
        }
        names.extend(self.builtin_names.keys().map(|s| s.to_string()));
        names.extend(self.host.names().map(str::to_string));
        names.extend(hir::SCRIPT_FNS.iter().map(|f| f.name().to_string()));
        names.extend(PRELUDE.iter().map(|(n, _)| n.to_string()));
        names
    }

    /// Walks a multi-segment path from `cx`'s module (or from a quest's own name: `Stage::Search`,
    /// `Role::dog`, `confront::paid`), reporting what it finds wrong.
    fn walk_here(&mut self, cx: &Cx, path: &[String], at: ast::Span) -> Res {
        if let (Some(q), Some((first, rest))) = (cx.quest, path.split_first())
            && let Some(start) = q.names.get(first)
        {
            let mut cur = match start {
                QName::Def(d) => Res::Def(*d),
                QName::Stage(_) => {
                    self.diags.push(error(
                        "unresolved",
                        format!("`{first}` is a stage: its value is `Stage::{first}`"),
                        cx.file,
                        at,
                    ));
                    return Res::Error;
                }
            };
            let mut walked = first.clone();
            for seg in rest {
                let next = match cur {
                    Res::Def(d) => self.member(d, seg),
                    _ => None,
                };
                let Some(next) = next else {
                    self.diags.push(error(
                        "unresolved",
                        format!("`{walked}` has no variant, exit or method `{seg}`"),
                        cx.file,
                        at,
                    ));
                    return Res::Error;
                };
                cur = next;
                walked = format!("{walked}::{seg}");
            }
            return cur;
        }
        match self.walk(cx.module, path, at, Mode::Item) {
            Walk::Found(res, mut diags) => {
                self.diags.append(&mut diags);
                res
            }
            // Every import is resolved before any body.
            Walk::Pending => Res::Error,
        }
    }

    /// What a path means as a value, reporting a module or a namespace used as one.
    fn path_res(&mut self, path: &[String], at: ast::Span, cx: &mut Cx) -> Res {
        let res = if let [name] = path {
            match self.lookup(cx, name) {
                Some(res) => res,
                None => {
                    let names = self.value_names(cx);
                    let shown = if name == EMIT_CALLEE { "emit" } else { name };
                    let mut d = unresolved(
                        format!("cannot find `{shown}`"),
                        name,
                        names.iter().map(String::as_str),
                        cx.file,
                        at,
                    );
                    if name == "len" {
                        d.notes.push(
                            "length is a method: `x.len()` on a `str`, `list` or `dict`"
                                .to_string(),
                        );
                    }
                    self.diags.push(d);
                    return Res::Error;
                }
            }
        } else {
            self.walk_here(cx, path, at)
        };
        match res {
            Res::Mod(_) => {
                self.diags.push(error(
                    "unresolved",
                    format!("`{}` is a module, not a value", path.join("::")),
                    cx.file,
                    at,
                ));
                Res::Error
            }
            Res::Stage(_) => {
                let name = path.join("::");
                self.diags.push(error(
                    "unresolved",
                    format!("`{name}` is a stage: its value is `Stage::{name}`"),
                    cx.file,
                    at,
                ));
                Res::Error
            }
            Res::Quest(hir::QuestFn::Decline) if !cx.in_setup => {
                self.diags.push(error(
                    "decline-outside-setup",
                    "`decline()` refuses a placement: it is allowed only in a `Setup` handler"
                        .to_string(),
                    cx.file,
                    at,
                ));
                res
            }
            Res::Namespace => {
                let name = path.join("::");
                self.diags.push(error(
                    "unresolved",
                    format!(
                        "`{name}` is a host namespace: call its functions as `{name}.<name>(…)`"
                    ),
                    cx.file,
                    at,
                ));
                Res::Error
            }
            other => other,
        }
    }

    fn res_expr(res: Res) -> hir::ExprKind {
        match res {
            Res::Def(d) => hir::ExprKind::Def(d),
            Res::Variant(d, i) => hir::ExprKind::Variant(d, i),
            Res::Host(h) => hir::ExprKind::Host(h),
            Res::Prelude(p) => hir::ExprKind::Prelude(p),
            Res::Quest(f) => hir::ExprKind::Quest(f),
            Res::Exit(d, i) => hir::ExprKind::Exit(d, i),
            // Reported where they were found.
            Res::Mod(_) | Res::Namespace | Res::Stage(_) | Res::Error => hir::ExprKind::Error,
        }
    }

    // Types.

    fn ty(&mut self, t: &ast::Type, cx: &mut Cx) -> hir::Type {
        let kind = match &t.kind {
            ast::TypeKind::Unit => hir::TypeKind::Unit,
            ast::TypeKind::Ref(inner) => hir::TypeKind::Ref(Box::new(self.ty(inner, cx))),
            ast::TypeKind::Named(path, args) => {
                let args: Vec<hir::Type> = args.iter().map(|a| self.ty(a, cx)).collect();
                match self.type_name(path, t.span, cx) {
                    Some(name) => hir::TypeKind::Named(name, args),
                    None => hir::TypeKind::Error,
                }
            }
        };
        hir::Type { kind, span: t.span }
    }

    fn type_name(&mut self, path: &[String], at: ast::Span, cx: &mut Cx) -> Option<TypeName> {
        let res = if let [name] = path {
            let quest = cx.quest.and_then(|q| q.names.get(name));
            let item = match quest {
                Some(QName::Def(d)) => Some(Res::Def(*d)),
                Some(QName::Stage(s)) => Some(Res::Stage(*s)),
                None => self
                    .entry(cx.module, name)
                    .map(|(r, _)| r.unwrap_or(Res::Error))
                    .or_else(|| self.builtin_names.get(name.as_str()).map(|d| Res::Def(*d))),
            };
            if let Some(res) = item
                && Self::type_of(res, |d| self.tag(d)).is_some()
            {
                res
            } else if let Some(i) = self.host.structs.get(name.as_str()) {
                Res::Host(HostItem::Struct(*i))
            } else if let Some(d) = self.host.enums.get(name.as_str()) {
                Res::Def(*d)
            } else if let Some(i) = self.host.events.get(name.as_str()) {
                Res::Host(HostItem::Event(*i))
            } else if let Some((_, b)) = BUILTIN_TYPES.iter().find(|(n, _)| n == name) {
                return Some(TypeName::Builtin(*b));
            } else if let Some(res) = item {
                res
            } else {
                let mut names: Vec<String> =
                    BUILTIN_TYPES.iter().map(|(n, _)| n.to_string()).collect();
                names.extend(self.host.structs.keys().cloned());
                names.extend(self.host.enums.keys().cloned());
                names.extend(self.host.events.keys().cloned());
                if let Some(scope) = self.scopes.get(cx.module.0 as usize) {
                    names.extend(scope.keys().map(|s| s.to_string()));
                }
                self.diags.push(unresolved(
                    format!("cannot find type `{name}`"),
                    name,
                    names.iter().map(String::as_str),
                    cx.file,
                    at,
                ));
                return None;
            }
        } else {
            self.walk_here(cx, path, at)
        };
        if res == Res::Error {
            return None;
        }
        let name = Self::type_of(res, |d| self.tag(d));
        if name.is_none() {
            self.diags.push(error(
                "unresolved",
                format!("`{}` is not a type", path.join("::")),
                cx.file,
                at,
            ));
        }
        name
    }

    fn type_of(res: Res, tag: impl Fn(DefId) -> Option<Tag>) -> Option<TypeName> {
        match res {
            Res::Def(d) if matches!(tag(d), Some(Tag::Struct | Tag::Enum | Tag::Event)) => {
                Some(TypeName::Def(d))
            }
            Res::Host(HostItem::Struct(i)) => Some(TypeName::HostStruct(i)),
            Res::Host(HostItem::Event(i)) => Some(TypeName::HostEvent(i)),
            _ => None,
        }
    }

    // Statements.

    fn stmts(&mut self, body: &[ast::Stmt], cx: &mut Cx) -> Vec<hir::Stmt> {
        body.iter().map(|s| self.stmt(s, cx)).collect()
    }

    /// A block: its own scope.
    fn block(&mut self, body: &[ast::Stmt], cx: &mut Cx) -> Vec<hir::Stmt> {
        cx.push();
        let out = self.stmts(body, cx);
        cx.pop();
        out
    }

    fn stmt(&mut self, s: &ast::Stmt, cx: &mut Cx) -> hir::Stmt {
        use ast::StmtKind as S;
        use hir::StmtKind as H;
        let kind = match &s.kind {
            S::Let(l) => {
                let value = self.expr(&l.value, cx);
                let ty = l.ty.as_ref().map(|t| self.ty(t, cx));
                H::Let(self.bind_local(cx, &l.name, s.span), ty, value)
            }
            S::Assign(place, value) => {
                let place = self.place(place, cx);
                H::Assign(place, self.expr(value, cx))
            }
            S::Expr(e) => H::Expr(self.expr(e, cx)),
            S::If(branches, other) => H::If(
                branches
                    .iter()
                    .map(|(c, b)| (self.expr(c, cx), self.block(b, cx)))
                    .collect(),
                other.as_ref().map(|b| self.block(b, cx)),
            ),
            S::While(c, b) => H::While(self.expr(c, cx), self.block(b, cx)),
            S::For(names, iter, body) => {
                let iter = self.expr(iter, cx);
                cx.push();
                let ids = names
                    .iter()
                    .map(|n| self.bind_local(cx, n, s.span))
                    .collect();
                let body = self.stmts(body, cx);
                cx.pop();
                H::For(ids, iter, body)
            }
            S::Match(e, cases) => H::Match(
                self.expr(e, cx),
                cases.iter().map(|c| self.case(c, cx)).collect(),
            ),
            S::Pass => H::Pass,
            S::Break => H::Break,
            S::Continue => H::Continue,
            S::Return(e) => H::Return(e.as_ref().map(|e| self.expr(e, cx))),
            S::Raise(e) => H::Raise(self.raise(e, cx)),
            S::Jump(name) => {
                let stage = cx.quest.and_then(|q| q.stage(name)).filter(|_| cx.quest_fn);
                match stage {
                    Some(idx) => H::Stage(idx),
                    None => {
                        let message = if cx.quest_fn {
                            format!("`-> {name}`: this quest has no stage `{name}`")
                        } else {
                            format!(
                                "`-> {name}` changes a quest's stage: it belongs in a quest's handler"
                            )
                        };
                        self.diags
                            .push(error("jump-target", message, cx.file, s.span));
                        H::Pass
                    }
                }
            }
        };
        hir::Stmt { kind, span: s.span }
    }

    /// The left side of an assignment: `x = …` assigns the nearest binding or a global.
    fn place(&mut self, e: &ast::Expr, cx: &mut Cx) -> hir::Expr {
        let ast::ExprKind::Path(path) = &e.kind else {
            return self.expr(e, cx);
        };
        if let [name] = path.as_slice()
            && let Some(id) = cx.local(name)
        {
            return self.mk_expr(hir::ExprKind::Local(id), e.span);
        }
        let res = if let [name] = path.as_slice() {
            self.lookup(cx, name)
        } else {
            Some(self.walk_here(cx, path, e.span))
        };
        let kind = match res {
            Some(Res::Def(d)) if self.tag(d) == Some(Tag::Global) => hir::ExprKind::Def(d),
            Some(Res::Error) => hir::ExprKind::Error,
            _ => {
                let name = path.join("::");
                self.diags.push(error(
                    "assign-undeclared",
                    format!("no variable `{name}` to assign: declare it with `let {name} = …`"),
                    cx.file,
                    e.span,
                ));
                hir::ExprKind::Error
            }
        };
        self.mk_expr(kind, e.span)
    }

    fn case(&mut self, c: &ast::Case, cx: &mut Cx) -> hir::Case {
        cx.push();
        let mut st = PatState::default();
        let pattern = self.pattern(&c.pattern, cx, &mut st, false);
        let mut made: Vec<(LocalId, String)> =
            st.binds.into_iter().map(|(n, id)| (id, n)).collect();
        made.sort();
        cx.locals.extend(made.into_iter().map(|(id, n)| (n, id)));
        let guard = c.guard.as_ref().map(|g| self.expr(g, cx));
        let body = self.stmts(&c.body, cx);
        cx.pop();
        hir::Case {
            pattern,
            guard,
            body,
            span: c.span,
        }
    }

    // Patterns.

    /// A pattern. Its bindings go to `st` (not in scope yet). `in_err`: the pattern stands inside `Err(…)`,
    /// where an upper-case name is an error name.
    fn pattern(
        &mut self,
        p: &ast::Pattern,
        cx: &mut Cx,
        st: &mut PatState,
        in_err: bool,
    ) -> hir::Pattern {
        use ast::PatternKind as P;
        use hir::PatternKind as H;
        let kind = match &p.kind {
            P::Wild => H::Wild,
            P::Name(n) if is_binding_name(n) => {
                if !st.seen.insert(n.clone()) {
                    self.diags.push(error(
                        "duplicate-def",
                        format!("`{n}` is bound twice in this pattern"),
                        cx.file,
                        p.span,
                    ));
                }
                H::Bind(match st.binds.get(n) {
                    Some(id) => *id,
                    None => {
                        let id = self.new_local(n, p.span);
                        st.binds.insert(n.clone(), id);
                        id
                    }
                })
            }
            P::Name(n) => match self.ctor(std::slice::from_ref(n), p.span, cx, in_err) {
                Some(c) => H::Ctor(c, Vec::new()),
                None => H::Error,
            },
            P::Path(path) => match self.ctor(path, p.span, cx, in_err) {
                Some(c) => H::Ctor(c, Vec::new()),
                None => H::Error,
            },
            P::Ctor(path, fields) => {
                let ctor = self.ctor(path, p.span, cx, in_err);
                let inner_err = ctor == Some(Ctor::Prelude(Prelude::Err));
                let fields = fields
                    .iter()
                    .map(|f| hir::PatField {
                        name: f.name.clone(),
                        pattern: self.pattern(&f.pattern, cx, st, inner_err),
                    })
                    .collect();
                match ctor {
                    Some(c) => H::Ctor(c, fields),
                    None => H::Error,
                }
            }
            P::Lit(l) => H::Lit(l.clone()),
            P::Or(alts) => {
                // Each alternative binds afresh; the names after the `|` are those of any alternative.
                let before = st.seen.clone();
                let mut after = before.clone();
                let mut out = Vec::with_capacity(alts.len());
                for a in alts {
                    st.seen = before.clone();
                    out.push(self.pattern(a, cx, st, in_err));
                    after.extend(st.seen.iter().cloned());
                }
                st.seen = after;
                H::Or(out)
            }
        };
        let id = hir::PatId(self.pat_count);
        self.pat_count = self.pat_count.saturating_add(1);
        hir::Pattern {
            id,
            kind,
            span: p.span,
        }
    }

    /// What a constructor pattern's path names. Inside `Err(…)` an upper-case single name is always an
    /// error name; elsewhere one that names nothing is left to the type checker (`ErrorOrUnknown`).
    fn ctor(&mut self, path: &[String], at: ast::Span, cx: &mut Cx, in_err: bool) -> Option<Ctor> {
        let res = if let [name] = path {
            if in_err && !is_binding_name(name) {
                return Some(Ctor::Error(name.clone()));
            }
            match self.lookup(cx, name) {
                // Maybe an error name: the type checker decides, knowing the scrutinee's type.
                None if !is_binding_name(name) => {
                    let names = self.value_names(cx);
                    let near = suggest(name, names.iter().map(String::as_str)).map(str::to_string);
                    return Some(Ctor::ErrorOrUnknown(name.clone(), near));
                }
                None => {
                    let names = self.value_names(cx);
                    self.diags.push(unresolved(
                        format!("cannot find `{name}`"),
                        name,
                        names.iter().map(String::as_str),
                        cx.file,
                        at,
                    ));
                    return None;
                }
                Some(res) => res,
            }
        } else {
            self.walk_here(cx, path, at)
        };
        match res {
            Res::Prelude(p @ (Prelude::Some | Prelude::None | Prelude::Ok | Prelude::Err)) => {
                Some(Ctor::Prelude(p))
            }
            Res::Variant(d, i) => Some(Ctor::Variant(d, i)),
            Res::Def(d) if matches!(self.tag(d), Some(Tag::Struct | Tag::Event)) => {
                Some(Ctor::Def(d))
            }
            Res::Host(h @ (HostItem::Struct(_) | HostItem::Event(_))) => Some(Ctor::Host(h)),
            Res::Error => None,
            other => {
                let what = match other {
                    Res::Def(d) => self.tag(d).map_or("an item", Tag::noun),
                    Res::Mod(_) => "a module",
                    Res::Namespace => "a host namespace",
                    _ => "a function",
                };
                self.diags.push(error(
                    "unresolved",
                    format!(
                        "`{}` is {what}: a pattern names a variant, a struct or an error",
                        path.join("::")
                    ),
                    cx.file,
                    at,
                ));
                None
            }
        }
    }

    // Expressions.

    fn boxed(&mut self, e: &ast::Expr, cx: &mut Cx) -> Box<hir::Expr> {
        Box::new(self.expr(e, cx))
    }

    fn args(&mut self, args: &[ast::Arg], cx: &mut Cx) -> Vec<hir::Arg> {
        args.iter()
            .map(|a| hir::Arg {
                name: a.name.clone(),
                value: self.expr(&a.value, cx),
            })
            .collect()
    }

    fn expr(&mut self, e: &ast::Expr, cx: &mut Cx) -> hir::Expr {
        use ast::ExprKind as A;
        use hir::ExprKind as H;
        let kind = match &e.kind {
            A::Error => H::Error,
            A::Unit => H::Unit,
            A::Int(i) => H::Int(*i),
            A::Float(f) => H::Float(*f),
            A::Bool(b) => H::Bool(*b),
            A::Str(s) => H::Str(s.clone()),
            A::FStr(parts) => H::FStr(
                parts
                    .iter()
                    .map(|p| match p {
                        ast::FStrPart::Text(t) => hir::FStrPart::Text(t.clone()),
                        ast::FStrPart::Expr(x) => hir::FStrPart::Expr(self.expr(x, cx)),
                    })
                    .collect(),
            ),
            A::Path(path) => {
                if let [name] = path.as_slice()
                    && let Some(id) = cx.local(name)
                {
                    H::Local(id)
                } else {
                    let res = self.path_res(path, e.span, cx);
                    Self::res_expr(res)
                }
            }
            A::List(items) => H::List(items.iter().map(|x| self.expr(x, cx)).collect()),
            A::Dict(entries) => H::Dict(
                entries
                    .iter()
                    .map(|(k, v)| (self.expr(k, cx), self.expr(v, cx)))
                    .collect(),
            ),
            A::Unary(op, x) => H::Unary(*op, self.boxed(x, cx)),
            A::Binary(op, a, b) => H::Binary(*op, self.boxed(a, cx), self.boxed(b, cx)),
            A::Await(x) => H::Await(self.boxed(x, cx)),
            A::Try(x) => H::Try(self.boxed(x, cx)),
            A::Call(callee, args) => H::Call(self.boxed(callee, cx), self.args(args, cx)),
            A::Field(base, name) => self.field(base, name, e.span, cx),
            A::Index(a, b) => H::Index(self.boxed(a, cx), self.boxed(b, cx)),
            A::Slice(x, a, b) => H::Slice(
                self.boxed(x, cx),
                a.as_ref().map(|a| self.boxed(a, cx)),
                b.as_ref().map(|b| self.boxed(b, cx)),
            ),
            A::Else(a, b) => H::Else(self.boxed(a, cx), self.boxed(b, cx)),
            A::Return(x) => H::Return(x.as_ref().map(|x| self.boxed(x, cx))),
            A::Break => H::Break,
            A::Continue => H::Continue,
            A::Raise(x) => H::Raise(Box::new(self.raise(x, cx))),
            A::FallbackBlock(name, body) => {
                cx.push();
                let id = self.bind_local(cx, name, e.span);
                let body = self.stmts(body, cx);
                cx.pop();
                H::FallbackBlock(id, body)
            }
        };
        self.mk_expr(kind, e.span)
    }

    /// `base.name`: a host namespace's function (`world.get`) when `base` is a namespace no binding or
    /// item hides, else a field or method.
    fn field(&mut self, base: &ast::Expr, name: &str, at: ast::Span, cx: &mut Cx) -> hir::ExprKind {
        // `Stage.objective`: another stage's objective.
        if let ast::ExprKind::Path(path) = &base.kind
            && let [stage] = path.as_slice()
            && cx.local(stage).is_none()
            && let Some(q) = cx.quest
            && q.stage(stage).is_some()
        {
            if let Some(d) = q.objectives.get(&(stage.clone(), name.to_string())) {
                return hir::ExprKind::Def(*d);
            }
            let d = unresolved(
                format!("stage `{stage}` has no objective `{name}`"),
                name,
                q.objectives
                    .keys()
                    .filter(|(s, _)| s == stage)
                    .map(|(_, o)| o.as_str()),
                cx.file,
                at,
            );
            self.diags.push(d);
            return hir::ExprKind::Error;
        }
        if let ast::ExprKind::Path(path) = &base.kind
            && let [ns] = path.as_slice()
            && cx.local(ns).is_none()
            && self.lookup(cx, ns) == Some(Res::Namespace)
        {
            let fns = self.host.namespaces.get(ns.as_str());
            if let Some(i) = fns.and_then(|f| f.get(name)) {
                return hir::ExprKind::Host(HostItem::Fn(*i));
            }
            let d = unresolved(
                format!("the host namespace `{ns}` has no function `{name}`"),
                name,
                fns.into_iter().flat_map(|f| f.keys().map(String::as_str)),
                cx.file,
                at,
            );
            self.diags.push(d);
            return hir::ExprKind::Error;
        }
        hir::ExprKind::Field(self.boxed(base, cx), name.to_string())
    }

    /// The operand of `raise`. Its head name (`raise X`, `raise X(…)`): an upper-case name is always an
    /// error name (error names are their own global space, spec §5.7), a lower-case one must be a binding
    /// (re-raising a caught error).
    fn raise(&mut self, e: &ast::Expr, cx: &mut Cx) -> hir::Expr {
        let (head, args) = match &e.kind {
            ast::ExprKind::Path(path) => (Some((path, e.span)), None),
            ast::ExprKind::Call(callee, args) => match &callee.kind {
                ast::ExprKind::Path(path) => (Some((path, callee.span)), Some(args)),
                _ => (None, None),
            },
            _ => (None, None),
        };
        let Some((path, at)) = head else {
            return self.expr(e, cx);
        };
        let [name] = path.as_slice() else {
            return self.expr(e, cx);
        };
        let kind = if !is_binding_name(name) {
            hir::ExprKind::ErrorName(name.clone())
        } else if let Some(id) = cx.local(name) {
            hir::ExprKind::Local(id)
        } else {
            let d = unresolved(
                format!(
                    "cannot find `{name}`: `raise` takes a caught error's binding or an error name"
                ),
                name,
                cx.locals.iter().map(|(n, _)| n.as_str()),
                cx.file,
                at,
            );
            self.diags.push(d);
            hir::ExprKind::Error
        };
        let head = self.mk_expr(kind, at);
        match args {
            None => head,
            Some(args) => {
                let args = self.args(args, cx);
                self.mk_expr(hir::ExprKind::Call(Box::new(head), args), e.span)
            }
        }
    }
}
