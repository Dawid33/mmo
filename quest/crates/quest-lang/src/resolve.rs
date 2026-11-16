//! Name resolution (spec §5.1, §5.10; plan D10 stage `resolve`): turns the module tree into the HIR,
//! where every name is an item, a binding, a host item or a prelude item.
//!
//! Passes, each over the modules in path order and the items in declaration order:
//! 1. **Collect:** every item gets its [`DefId`] and its name in its module's namespace — one namespace per
//!    module for every kind of item, `mod` and `use` included (`duplicate-def`).
//! 2. **Imports:** every `use` is resolved to what it names, repeatedly until nothing changes, so `use`
//!    cycles between modules are fine; an import that only reaches itself is `unresolved`.
//! 3. **Impls:** each `impl` finds its struct or enum, and its methods enter [`hir::Crate::methods`].
//! 4. **Bodies:** types, functions, globals; dialogues and quests are resolved and checked (decision 19),
//!    then kept only as deferred items.
//!
//! Lookup of a single name: the bindings in scope (innermost first), then — inside a quest — the quest's
//! own names, then the module's items and imports, then the host's functions, namespaces, structs and
//! events, then the prelude. A path `a::b::c` starts at `crate`, `self`, `super` or an item of the module
//! and walks modules, then an enum's variant or a struct's / enum's method. An item is visible from its own
//! module and that module's descendants; from anywhere else only if `pub` (`private`).
//!
//! Error names (spec §5.7) are global and never declared. The head of a `raise` that is not a binding is
//! an error name; in a pattern, a name that starts with an upper-case letter and names no struct, event,
//! variant or `Some` / `None` / `Ok` / `Err` is an error name. A pattern name that starts with a lower-case
//! letter or `_` is a binding.

use crate::ast;
use crate::diag::Diagnostic;
use crate::hir::{
    self, BUILTIN_TYPES, Ctor, DefId, DefKind, EventRef, HostItem, LocalId, ModId, PRELUDE,
    Prelude, TypeName,
};
use crate::modules::ROOT_FILE;
use quest_vm::host::HostApi;
use std::collections::{BTreeMap, BTreeSet};

/// The furthest (in edits) a name may be from a name in scope for `unresolved` to suggest it.
const SUGGEST_DISTANCE_MAX: usize = 2;

/// Names longer than this (in bytes) get no suggestion: the edit distance is quadratic in length.
const SUGGEST_LEN_MAX: usize = 64;

/// The arguments `@pick` takes (spec §5.11).
const PICK_STRATEGIES: [&str; 4] = [
    "first",
    "best",
    "best_least_recent",
    "random_best_least_recent",
];

/// Functions a quest's code may call (spec §5.12); lowered in Phase 10.
const QUEST_BUILTINS: [&str; 3] = ["complete", "fail", "decline"];

/// Functions a dialogue's conditions may call (spec §5.11); lowered in Phase 10.
const DIALOGUE_BUILTINS: [&str; 1] = ["dialogue_depth"];

/// Path words with a fixed meaning.
const PATH_WORDS: [&str; 3] = ["crate", "self", "super"];

/// The module tree's root.
const ROOT: ModId = ModId(0);

/// Resolves the module tree against the host's API.
pub fn resolve(krate: &ast::Crate, host: &HostApi) -> (hir::Crate, Vec<Diagnostic>) {
    let mut r = Resolver::new(krate, host);
    r.collect();
    r.resolve_imports();
    r.resolve_impls();
    let kinds = r.resolve_bodies();
    let defs = r
        .metas
        .iter()
        .zip(kinds)
        .map(|(m, kind)| hir::Def {
            name: m.name.to_string(),
            module: m.module,
            public: m.public,
            span: m.span,
            kind,
        })
        .collect();
    let out = hir::Crate {
        modules: r.modules,
        defs,
        locals: r.locals,
        methods: r.methods,
        deferred: r.deferred,
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
    /// A quest's own name, or a dialogue / quest builtin: resolved, lowered in Phase 10.
    Deferred,
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
    name: &'a str,
    module: ModId,
    public: bool,
    span: ast::Span,
    tag: Tag,
    item: &'a ast::Item,
    /// The `impl` a method is in.
    impl_index: Option<usize>,
}

struct Impl<'a> {
    module: ModId,
    name: &'a str,
    span: ast::Span,
    methods: Vec<DefId>,
    owner: Option<DefId>,
}

/// The host API's names.
#[derive(Default)]
struct HostNames {
    /// Functions without a namespace or receiver.
    free: BTreeMap<String, u32>,
    /// Namespace → function name → index.
    namespaces: BTreeMap<String, BTreeMap<String, u32>>,
    structs: BTreeMap<String, u32>,
    events: BTreeMap<String, u32>,
}

impl HostNames {
    fn new(host: &HostApi) -> HostNames {
        let mut names = HostNames::default();
        for (i, f) in host.fns.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            if f.receiver.is_some() {
                continue;
            }
            match &f.namespace {
                Some(ns) => {
                    names
                        .namespaces
                        .entry(ns.clone())
                        .or_default()
                        .entry(f.name.clone())
                        .or_insert(i);
                }
                None => {
                    names.free.entry(f.name.clone()).or_insert(i);
                }
            }
        }
        for (i, s) in host.structs.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            names.structs.entry(s.name.clone()).or_insert(i);
        }
        for (i, e) in host.events.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            names.events.entry(e.clone()).or_insert(i);
        }
        names
    }

    fn lookup(&self, name: &str) -> Option<Res> {
        if let Some(i) = self.free.get(name) {
            Some(Res::Host(HostItem::Fn(*i)))
        } else if self.namespaces.contains_key(name) {
            Some(Res::Namespace)
        } else if let Some(i) = self.structs.get(name) {
            Some(Res::Host(HostItem::Struct(*i)))
        } else {
            self.events
                .get(name)
                .map(|i| Res::Host(HostItem::Event(*i)))
        }
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.free
            .keys()
            .chain(self.namespaces.keys())
            .chain(self.structs.keys())
            .chain(self.events.keys())
            .map(String::as_str)
    }
}

/// What a quest's own name is (spec §5.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuestName {
    Role,
    Global,
    Event,
    Stage,
    Dialogue,
    Fn,
}

/// A quest's names, seen by everything inside it.
#[derive(Default)]
struct QuestScope {
    names: BTreeMap<String, QuestName>,
}

impl QuestScope {
    fn has_stage(&self, name: &str) -> bool {
        self.names.get(name) == Some(&QuestName::Stage)
    }
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
    in_dialogue: bool,
    /// Inside a quest's handler, where `-> Stage` changes the stage.
    quest_fn: bool,
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

/// The dialogue whose steps are being resolved.
struct DialogueCx<'d> {
    nodes: BTreeSet<&'d str>,
    /// A bark (`(npc: Person)`) offers no choice.
    bark: bool,
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
    deferred: Vec<DefId>,
    /// Every line id used so far, with where.
    line_ids: BTreeMap<String, (String, ast::Span)>,
    root_quest_seen: bool,
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
fn suggest<'n>(name: &str, candidates: impl IntoIterator<Item = &'n str>) -> Option<&'n str> {
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
            deferred: Vec::new(),
            line_ids: BTreeMap::new(),
            root_quest_seen: false,
        }
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

    // Pass 1: collect.

    fn collect(&mut self) {
        let krate = self.krate;
        for (i, m) in krate.modules.iter().enumerate() {
            let id = ModId(index_u32(i));
            self.module_ids.insert(m.path.as_slice(), id);
            let parent = m
                .path
                .split_last()
                .and_then(|(_, up)| self.module_ids.get(up).copied());
            self.modules.push(hir::Module {
                path: m.path.clone(),
                file: m.file.clone(),
                parent,
                defs: Vec::new(),
            });
            self.scopes.push(BTreeMap::new());
        }
        for (i, m) in krate.modules.iter().enumerate() {
            let id = ModId(index_u32(i));
            for item in &m.ast.items {
                self.collect_item(id, &m.path, &m.file, item);
            }
        }
    }

    fn collect_item(&mut self, m: ModId, path: &'a [String], file: &'a str, item: &'a ast::Item) {
        let def = |name: &'a String, tag| (name.as_str(), tag);
        let named = match &item.kind {
            ast::ItemKind::Fn(f) => Some(def(&f.name, Tag::Fn)),
            ast::ItemKind::Struct(n, _) => Some(def(n, Tag::Struct)),
            ast::ItemKind::Enum(n, _) => Some(def(n, Tag::Enum)),
            ast::ItemKind::ErrorSet(n, _) => Some(def(n, Tag::ErrorSet)),
            ast::ItemKind::Event(n, _) => Some(def(n, Tag::Event)),
            ast::ItemKind::Let(l) => Some(def(&l.name, Tag::Global)),
            ast::ItemKind::Dialogue(d) => Some(def(&d.name, Tag::Dialogue)),
            ast::ItemKind::Quest(n, _) => Some(def(n, Tag::Quest)),
            ast::ItemKind::Mod(name) => {
                // `mod self` and the like were reported by `load_modules`.
                if !PATH_WORDS.contains(&name.as_str()) {
                    let mut child = path.to_vec();
                    child.push(name.clone());
                    let res = self
                        .module_ids
                        .get(child.as_slice())
                        .map_or(Res::Error, |c| Res::Mod(*c));
                    self.bind(m, name, Slot::Res(res), item.public, item.span, file);
                }
                None
            }
            ast::ItemKind::Use(imports) => {
                for u in imports {
                    let Some(last) = u.path.last() else { continue };
                    let name = u.alias.as_ref().unwrap_or(last);
                    if PATH_WORDS.contains(&name.as_str()) {
                        self.diags.push(error(
                            "unresolved",
                            format!("`{name}` cannot be imported; name an item"),
                            file,
                            u.span,
                        ));
                        continue;
                    }
                    let index = self.imports.len();
                    self.imports.push(Import {
                        module: m,
                        path: &u.path,
                        span: u.span,
                        done: None,
                    });
                    self.bind(m, name, Slot::Import(index), item.public, u.span, file);
                }
                None
            }
            ast::ItemKind::Impl(name, methods) => {
                let index = self.impls.len();
                let mut ids = Vec::new();
                for method in methods {
                    if let ast::ItemKind::Fn(f) = &method.kind {
                        ids.push(self.add_def(m, &f.name, method, Tag::Fn, Some(index)));
                    }
                }
                self.impls.push(Impl {
                    module: m,
                    name,
                    span: item.span,
                    methods: ids,
                    owner: None,
                });
                None
            }
            ast::ItemKind::Role(..) => {
                self.diags.push(error(
                    "quest-only",
                    "a `role` belongs in a `quest`".to_string(),
                    file,
                    item.span,
                ));
                None
            }
            ast::ItemKind::Stage(..) => {
                self.diags.push(error(
                    "quest-only",
                    "a `stage` belongs in a `quest`".to_string(),
                    file,
                    item.span,
                ));
                None
            }
            ast::ItemKind::Objective(..) => {
                self.diags.push(error(
                    "quest-only",
                    "an `objective` belongs in a quest's `stage`".to_string(),
                    file,
                    item.span,
                ));
                None
            }
        };
        if let Some((name, tag)) = named {
            let id = self.add_def(m, name, item, tag, None);
            self.bind(
                m,
                name,
                Slot::Res(Res::Def(id)),
                item.public,
                item.span,
                file,
            );
        }
    }

    fn add_def(
        &mut self,
        m: ModId,
        name: &'a str,
        item: &'a ast::Item,
        tag: Tag,
        impl_index: Option<usize>,
    ) -> DefId {
        let id = DefId(index_u32(self.metas.len()));
        self.metas.push(Meta {
            name,
            module: m,
            public: item.public,
            span: item.span,
            tag,
            item,
            impl_index,
        });
        if let Some(module) = self.modules.get_mut(m.0 as usize) {
            module.defs.push(id);
        }
        id
    }

    /// Enters `name` in module `m`'s namespace; a second entry of one name is `duplicate-def`.
    fn bind(
        &mut self,
        m: ModId,
        name: &'a str,
        slot: Slot,
        public: bool,
        span: ast::Span,
        file: &str,
    ) {
        let Some(scope) = self.scopes.get_mut(m.0 as usize) else {
            return;
        };
        if let Some(first) = scope.get(name) {
            let mut d = error(
                "duplicate-def",
                format!("`{name}` is defined twice in this module"),
                file,
                span,
            );
            d.notes
                .push(format!("first defined at line {}", first.span.line));
            self.diags.push(d);
            return;
        }
        scope.insert(name, Entry { slot, public, span });
    }

    // Pass 2: imports.

    fn resolve_imports(&mut self) {
        loop {
            let mut progress = false;
            for i in 0..self.imports.len() {
                let Some(import) = self.imports.get(i) else {
                    continue;
                };
                if import.done.is_some() {
                    continue;
                }
                let (m, path, span) = (import.module, import.path, import.span);
                if let Walk::Found(res, mut diags) = self.walk(m, path, span, Mode::Use) {
                    self.diags.append(&mut diags);
                    if let Some(import) = self.imports.get_mut(i) {
                        import.done = Some(res);
                    }
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        for i in 0..self.imports.len() {
            let Some(import) = self.imports.get_mut(i) else {
                continue;
            };
            if import.done.is_none() {
                import.done = Some(Res::Error);
                let (m, path, span) = (import.module, import.path, import.span);
                let file = self.file_of(m);
                self.diags.push(error(
                    "unresolved",
                    format!(
                        "`{}` imports itself through a cycle of `use`s",
                        path.join("::")
                    ),
                    file,
                    span,
                ));
            }
        }
    }

    fn slot_res(&self, slot: Slot) -> Option<Res> {
        match slot {
            Slot::Res(res) => Some(res),
            Slot::Import(i) => self.imports.get(i).and_then(|imp| imp.done),
        }
    }

    /// Module `m`'s entry for `name`: what it means (`Some(None)` while its import is pending).
    fn entry(&self, m: ModId, name: &str) -> Option<(Option<Res>, Entry)> {
        let entry = *self.scopes.get(m.0 as usize)?.get(name)?;
        Some((self.slot_res(entry.slot), entry))
    }

    /// Walks a path of two or more segments, or a `use` path, from module `from`.
    fn walk(&self, from: ModId, path: &[String], span: ast::Span, mode: Mode) -> Walk {
        let file = self.file_of(from);
        let mut diags = Vec::new();
        let Some((first, rest)) = path.split_first() else {
            return Walk::Found(Res::Error, diags);
        };
        let mut cur = match first.as_str() {
            "crate" => Res::Mod(ROOT),
            "self" => Res::Mod(from),
            "super" => match self.parent_of(from) {
                Some(p) => Res::Mod(p),
                None => {
                    diags.push(error(
                        "unresolved",
                        "`super` at the crate root: there is no parent module".to_string(),
                        file,
                        span,
                    ));
                    return Walk::Found(Res::Error, diags);
                }
            },
            name => match self.entry(from, name) {
                Some((Some(res), _)) => res,
                Some((None, _)) => return Walk::Pending,
                None => {
                    let candidates = self
                        .scopes
                        .get(from.0 as usize)
                        .into_iter()
                        .flat_map(|s| s.keys().copied());
                    diags.push(unresolved(
                        format!("cannot find `{name}` in this module"),
                        name,
                        candidates,
                        file,
                        span,
                    ));
                    return Walk::Found(Res::Error, diags);
                }
            },
        };
        let mut leading = matches!(first.as_str(), "self" | "super");
        let mut walked = first.clone();
        for seg in rest {
            if seg == "super" && leading {
                if let Res::Mod(m) = cur
                    && let Some(p) = self.parent_of(m)
                {
                    cur = Res::Mod(p);
                } else {
                    diags.push(error(
                        "unresolved",
                        format!("`{walked}::super`: there is no parent module"),
                        file,
                        span,
                    ));
                    return Walk::Found(Res::Error, diags);
                }
                walked = format!("{walked}::super");
                continue;
            }
            leading = false;
            cur = match cur {
                Res::Error => return Walk::Found(Res::Error, diags),
                Res::Mod(m) => match self.entry(m, seg) {
                    Some((None, _)) => return Walk::Pending,
                    Some((Some(res), entry)) => {
                        if !entry.public && !self.is_within(from, m) {
                            diags.push(error(
                                "private",
                                format!("`{seg}` is private to module `{}`", self.module_name(m)),
                                file,
                                span,
                            ));
                        }
                        res
                    }
                    None => {
                        let candidates = self
                            .scopes
                            .get(m.0 as usize)
                            .into_iter()
                            .flat_map(|s| s.keys().copied());
                        diags.push(unresolved(
                            format!("cannot find `{seg}` in module `{}`", self.module_name(m)),
                            seg,
                            candidates,
                            file,
                            span,
                        ));
                        return Walk::Found(Res::Error, diags);
                    }
                },
                Res::Def(d) if mode == Mode::Item => match self.member(d, seg) {
                    Some(res) => {
                        if let Res::Def(method) = res
                            && let Some(meta) = self.metas.get(method.0 as usize)
                            && !meta.public
                            && !self.is_within(from, meta.module)
                        {
                            diags.push(error(
                                "private",
                                format!(
                                    "method `{seg}` is private to module `{}`",
                                    self.module_name(meta.module)
                                ),
                                file,
                                span,
                            ));
                        }
                        res
                    }
                    None => {
                        diags.push(error(
                            "unresolved",
                            format!("`{walked}` has no variant or method `{seg}`"),
                            file,
                            span,
                        ));
                        return Walk::Found(Res::Error, diags);
                    }
                },
                _ => {
                    diags.push(error(
                        "unresolved",
                        format!("`{walked}` is not a module, so `{walked}::{seg}` names nothing"),
                        file,
                        span,
                    ));
                    return Walk::Found(Res::Error, diags);
                }
            };
            walked = format!("{walked}::{seg}");
        }
        Walk::Found(cur, diags)
    }

    /// A struct's or enum's variant or method `name`.
    fn member(&self, d: DefId, name: &str) -> Option<Res> {
        let meta = self.metas.get(d.0 as usize)?;
        if let ast::ItemKind::Enum(_, variants) = &meta.item.kind
            && let Some(i) = variants.iter().position(|v| v.name == name)
        {
            return Some(Res::Variant(d, index_u32(i)));
        }
        if !matches!(meta.tag, Tag::Struct | Tag::Enum) {
            return None;
        }
        self.methods
            .get(&(d, name.to_string()))
            .map(|m| Res::Def(*m))
    }

    // Pass 3: impls.

    fn resolve_impls(&mut self) {
        for k in 0..self.impls.len() {
            let Some(imp) = self.impls.get(k) else {
                continue;
            };
            let (m, name, span) = (imp.module, imp.name, imp.span);
            let file = self.file_of(m);
            let owner = match self.entry(m, name) {
                Some((Some(Res::Def(d)), _))
                    if matches!(self.tag(d), Some(Tag::Struct | Tag::Enum)) =>
                {
                    Some(d)
                }
                Some((Some(Res::Error) | None, _)) => None,
                Some(_) => {
                    self.diags.push(error(
                        "unresolved",
                        format!("`impl {name}`: `{name}` is not a struct or an enum"),
                        file,
                        span,
                    ));
                    None
                }
                None => {
                    let candidates = self
                        .scopes
                        .get(m.0 as usize)
                        .into_iter()
                        .flat_map(|s| s.keys().copied());
                    let d = unresolved(
                        format!("`impl {name}`: cannot find `{name}`"),
                        name,
                        candidates,
                        file,
                        span,
                    );
                    self.diags.push(d);
                    None
                }
            };
            let Some(owner) = owner else { continue };
            if let Some(imp) = self.impls.get_mut(k) {
                imp.owner = Some(owner);
            }
            let methods = self
                .impls
                .get(k)
                .map(|i| i.methods.clone())
                .unwrap_or_default();
            for method in methods {
                let Some(meta) = self.metas.get(method.0 as usize) else {
                    continue;
                };
                let (mname, mspan) = (meta.name, meta.span);
                let file = self.file_of(meta.module);
                match self.methods.entry((owner, mname.to_string())) {
                    std::collections::btree_map::Entry::Occupied(_) => {
                        self.diags.push(error(
                            "duplicate-def",
                            format!("`{name}` has two methods named `{mname}`"),
                            file,
                            mspan,
                        ));
                    }
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        slot.insert(method);
                    }
                }
            }
        }
    }

    // Pass 4: bodies.

    fn resolve_bodies(&mut self) -> Vec<DefKind> {
        let mut kinds = Vec::with_capacity(self.metas.len());
        for i in 0..self.metas.len() {
            let id = DefId(index_u32(i));
            let Some(meta) = self.metas.get(i) else { break };
            let (m, item, impl_index) = (meta.module, meta.item, meta.impl_index);
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
                    let mut seen = BTreeSet::new();
                    let mut out = Vec::new();
                    for v in variants {
                        if !seen.insert(v.name.as_str()) {
                            self.diags.push(error(
                                "duplicate-def",
                                format!("variant `{}` is defined twice", v.name),
                                file,
                                v.span,
                            ));
                        }
                        out.push(self.variant(v, &mut cx));
                    }
                    DefKind::Enum(out)
                }
                ast::ItemKind::ErrorSet(_, members) => {
                    DefKind::ErrorSet(self.error_set(members, &mut Cx::new(m, file, None)))
                }
                ast::ItemKind::Let(l) => {
                    let mut cx = Cx::new(m, file, None);
                    let ty = l.ty.as_ref().map(|t| self.ty(t, &mut cx));
                    let value = self.expr(&l.value, &mut cx);
                    DefKind::Global(hir::Global { ty, value })
                }
                ast::ItemKind::Dialogue(d) => {
                    self.dialogue(d, item, m, None);
                    self.deferred.push(id);
                    DefKind::Dialogue
                }
                ast::ItemKind::Quest(_, members) => {
                    self.quest(members, item, m);
                    self.deferred.push(id);
                    DefKind::Quest
                }
                // Collected items are only the kinds above.
                _ => DefKind::Quest,
            };
            kinds.push(kind);
        }
        kinds
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
        let handler = self.fn_decorators(item, is_method, cx);
        cx.push();
        let receiver = f
            .receiver
            .map(|r| (r, self.bind_local(cx, "self", item.span)));
        let mut seen = BTreeSet::new();
        let mut params = Vec::new();
        for p in &f.params {
            if !seen.insert(p.name.as_str()) {
                self.diags.push(error(
                    "duplicate-def",
                    format!("parameter `{}` is declared twice", p.name),
                    cx.file,
                    p.span,
                ));
            }
            let ty = p.ty.as_ref().map(|t| self.ty(t, cx));
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
            Res::Error | Res::Deferred => None,
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

    /// Checks a function's decorators; returns the event of its `@on`.
    fn fn_decorators(
        &mut self,
        item: &ast::Item,
        is_method: bool,
        cx: &mut Cx,
    ) -> Option<EventRef> {
        let mut handler = None;
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
        handler
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
            Some(Res::Error | Res::Deferred) => None,
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
                    .filter(|(_, k)| **k == QuestName::Event)
                    .map(|(n, _)| n.clone()),
            );
        }
        names
    }

    // Names.

    /// What a single name means outside the bindings.
    fn lookup(&self, cx: &Cx, name: &str) -> Option<Res> {
        if let Some(q) = cx.quest
            && (q.names.contains_key(name) || QUEST_BUILTINS.contains(&name))
        {
            return Some(Res::Deferred);
        }
        if cx.in_dialogue && DIALOGUE_BUILTINS.contains(&name) {
            return Some(Res::Deferred);
        }
        if let Some((res, _)) = self.entry(cx.module, name) {
            return Some(res.unwrap_or(Res::Error));
        }
        self.host
            .lookup(name)
            .or_else(|| Prelude::from_name(name).map(Res::Prelude))
    }

    /// Every single name `cx` sees, for suggestions.
    fn value_names(&self, cx: &Cx) -> Vec<String> {
        let mut names: Vec<String> = cx.locals.iter().map(|(n, _)| n.clone()).collect();
        if let Some(q) = cx.quest {
            names.extend(q.names.keys().cloned());
            names.extend(QUEST_BUILTINS.iter().map(|s| s.to_string()));
        }
        if cx.in_dialogue {
            names.extend(DIALOGUE_BUILTINS.iter().map(|s| s.to_string()));
        }
        if let Some(scope) = self.scopes.get(cx.module.0 as usize) {
            names.extend(scope.keys().map(|s| s.to_string()));
        }
        names.extend(self.host.names().map(str::to_string));
        names.extend(PRELUDE.iter().map(|(n, _)| n.to_string()));
        names
    }

    /// Walks a multi-segment path from `cx`'s module, reporting what it finds wrong.
    fn walk_here(&mut self, cx: &Cx, path: &[String], at: ast::Span) -> Res {
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
                    self.diags.push(unresolved(
                        format!("cannot find `{name}`"),
                        name,
                        names.iter().map(String::as_str),
                        cx.file,
                        at,
                    ));
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
            // A quest's names live only in deferred items, never seen by later stages.
            Res::Mod(_) | Res::Namespace | Res::Deferred | Res::Error => hir::ExprKind::Error,
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
            if cx.quest.is_some_and(|q| q.names.contains_key(name)) {
                return None;
            }
            let item = self
                .entry(cx.module, name)
                .map(|(r, _)| r.unwrap_or(Res::Error));
            if let Some(res) = item
                && Self::type_of(res, |d| self.tag(d)).is_some()
            {
                res
            } else if let Some(i) = self.host.structs.get(name.as_str()) {
                Res::Host(HostItem::Struct(*i))
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
                let ok = cx.quest_fn && cx.quest.is_some_and(|q| q.has_stage(name));
                if !ok {
                    let message = if cx.quest_fn {
                        format!("`-> {name}`: this quest has no stage `{name}`")
                    } else {
                        format!(
                            "`-> {name}` changes a quest's stage: it belongs in a quest's handler"
                        )
                    };
                    self.diags
                        .push(error("jump-target", message, cx.file, s.span));
                }
                // A stage change lives only in a quest, which Phase 10 lowers.
                H::Pass
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
            return hir::Expr {
                kind: hir::ExprKind::Local(id),
                span: e.span,
            };
        }
        let res = if let [name] = path.as_slice() {
            self.lookup(cx, name)
        } else {
            Some(self.walk_here(cx, path, e.span))
        };
        let kind = match res {
            Some(Res::Def(d)) if self.tag(d) == Some(Tag::Global) => hir::ExprKind::Def(d),
            Some(Res::Error | Res::Deferred) => hir::ExprKind::Error,
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
        hir::Expr { kind, span: e.span }
    }

    fn case(&mut self, c: &ast::Case, cx: &mut Cx) -> hir::Case {
        cx.push();
        let mut binds = BTreeMap::new();
        let pattern = self.pattern(&c.pattern, cx, &mut binds);
        let mut made: Vec<(LocalId, String)> = binds.into_iter().map(|(n, id)| (id, n)).collect();
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

    /// A pattern. Its bindings go to `binds` (one id per name, shared by `|` alternatives), not in scope.
    fn pattern(
        &mut self,
        p: &ast::Pattern,
        cx: &mut Cx,
        binds: &mut BTreeMap<String, LocalId>,
    ) -> hir::Pattern {
        use ast::PatternKind as P;
        use hir::PatternKind as H;
        let kind = match &p.kind {
            P::Wild => H::Wild,
            P::Name(n) if is_binding_name(n) => H::Bind(match binds.get(n) {
                Some(id) => *id,
                None => {
                    let id = self.new_local(n, p.span);
                    binds.insert(n.clone(), id);
                    id
                }
            }),
            P::Name(n) => match self.ctor(std::slice::from_ref(n), p.span, cx) {
                Some(c) => H::Ctor(c, Vec::new()),
                None => H::Error,
            },
            P::Path(path) => match self.ctor(path, p.span, cx) {
                Some(c) => H::Ctor(c, Vec::new()),
                None => H::Error,
            },
            P::Ctor(path, fields) => {
                let ctor = self.ctor(path, p.span, cx);
                let fields = fields
                    .iter()
                    .map(|f| hir::PatField {
                        name: f.name.clone(),
                        pattern: self.pattern(&f.pattern, cx, binds),
                    })
                    .collect();
                match ctor {
                    Some(c) => H::Ctor(c, fields),
                    None => H::Error,
                }
            }
            P::Lit(l) => H::Lit(l.clone()),
            P::Or(alts) => H::Or(alts.iter().map(|a| self.pattern(a, cx, binds)).collect()),
        };
        hir::Pattern { kind, span: p.span }
    }

    /// What a constructor pattern's path names; an unknown single name is an error name.
    fn ctor(&mut self, path: &[String], at: ast::Span, cx: &mut Cx) -> Option<Ctor> {
        let res = if let [name] = path {
            match self.lookup(cx, name) {
                None => return Some(Ctor::Error(name.clone())),
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
            Res::Error | Res::Deferred => None,
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
        hir::Expr { kind, span: e.span }
    }

    /// `base.name`: a host namespace's function (`world.get`) when `base` is a namespace no binding or
    /// item hides, else a field or method.
    fn field(&mut self, base: &ast::Expr, name: &str, at: ast::Span, cx: &mut Cx) -> hir::ExprKind {
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

    /// The operand of `raise`: a binding (re-raising a caught error), or an error name with its data.
    fn raise(&mut self, e: &ast::Expr, cx: &mut Cx) -> hir::Expr {
        let error_name = |x: &ast::Expr, cx: &Cx| match &x.kind {
            ast::ExprKind::Path(path) => match path.as_slice() {
                [name] if cx.local(name).is_none() => Some(hir::Expr {
                    kind: hir::ExprKind::ErrorName(name.clone()),
                    span: x.span,
                }),
                _ => None,
            },
            _ => None,
        };
        if let Some(name) = error_name(e, cx) {
            return name;
        }
        if let ast::ExprKind::Call(callee, args) = &e.kind
            && let Some(name) = error_name(callee, cx)
        {
            let args = self.args(args, cx);
            return hir::Expr {
                kind: hir::ExprKind::Call(Box::new(name), args),
                span: e.span,
            };
        }
        self.expr(e, cx)
    }

    // Dialogues and quests (spec §5.11–5.12, decision 19): resolved and checked, then dropped.

    fn dialogue(
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

    fn quest(&mut self, members: &'a [ast::Item], item: &ast::Item, m: ModId) {
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
