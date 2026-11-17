//! Passes 1–3 of resolution: collecting items into module namespaces, resolving `use` to a fixpoint,
//! walking paths, and attaching `impl` methods; plus the host API's names.

use super::{
    Entry, Impl, Import, Meta, Mode, PATH_WORDS, ROOT, Res, Resolver, Slot, Tag, Walk, error,
    index_u32, unresolved,
};
use crate::ast;
use crate::hir::{self, DefId, HostItem, ModId};
use quest_vm::host::{HostApi, QUEST_NAMESPACE};
use std::collections::BTreeMap;

/// The host API's names.
#[derive(Default)]
pub(super) struct HostNames {
    /// Functions without a namespace or receiver.
    pub(super) free: BTreeMap<String, u32>,
    /// Namespace → function name → index.
    pub(super) namespaces: BTreeMap<String, BTreeMap<String, u32>>,
    pub(super) structs: BTreeMap<String, u32>,
    pub(super) events: BTreeMap<String, u32>,
    /// Each host enum's item (`collect_host_enums`), by name.
    pub(super) enums: BTreeMap<String, DefId>,
    /// The same items by index into `HostApi::enums`.
    pub(super) enum_defs: Vec<DefId>,
}

/// A host enum as an `enum` item of the crate root (data-less variants), so it resolves, type-checks,
/// matches and lowers like a script enum; its name is the host's, outside every module's namespace.
pub(super) fn host_enum_items(host: &HostApi) -> Vec<ast::Item> {
    host.enums
        .iter()
        .map(|e| ast::Item {
            kind: ast::ItemKind::Enum(
                e.name.clone(),
                e.variants
                    .iter()
                    .map(|v| ast::Variant {
                        name: v.clone(),
                        fields: Vec::new(),
                        span: ast::Span::default(),
                    })
                    .collect(),
            ),
            public: true,
            decorators: Vec::new(),
            span: ast::Span::default(),
            depth: 0,
        })
        .collect()
}

impl HostNames {
    pub(super) fn new(host: &HostApi) -> HostNames {
        let mut names = HostNames::default();
        for (i, f) in host.fns.iter().enumerate() {
            let Ok(i) = u32::try_from(i) else { break };
            // The `quest` namespace is the host side of the quest builtins: the compiler lowers to it, no
            // script names it.
            if f.receiver.is_some() || f.namespace.as_deref() == Some(QUEST_NAMESPACE) {
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
            names.events.entry(e.name.clone()).or_insert(i);
        }
        names
    }

    pub(super) fn lookup(&self, name: &str) -> Option<Res> {
        if let Some(i) = self.free.get(name) {
            Some(Res::Host(HostItem::Fn(*i)))
        } else if self.namespaces.contains_key(name) {
            Some(Res::Namespace)
        } else if let Some(i) = self.structs.get(name) {
            Some(Res::Host(HostItem::Struct(*i)))
        } else if let Some(d) = self.enums.get(name) {
            Some(Res::Def(*d))
        } else {
            self.events
                .get(name)
                .map(|i| Res::Host(HostItem::Event(*i)))
        }
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.free
            .keys()
            .chain(self.namespaces.keys())
            .chain(self.structs.keys())
            .chain(self.enums.keys())
            .chain(self.events.keys())
            .map(String::as_str)
    }
}

impl<'a> Resolver<'a> {
    // Pass 1: collect.

    pub(super) fn collect(&mut self) {
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

    /// The host enums' items, after every script item: ids above the script's, in the root, in no namespace.
    pub(super) fn collect_host_enums(&mut self, items: &'a [ast::Item]) {
        for item in items {
            let ast::ItemKind::Enum(name, _) = &item.kind else {
                continue;
            };
            let id = DefId(index_u32(self.metas.len()));
            self.metas.push(Meta {
                name: name.clone(),
                module: ROOT,
                public: true,
                span: item.span,
                tag: Tag::Enum,
                item: Some(item),
                impl_index: None,
                parent: None,
            });
            self.host.enums.entry(name.clone()).or_insert(id);
            self.host.enum_defs.push(id);
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
            ast::ItemKind::Dialogue(d) | ast::ItemKind::Scene(d) => {
                Some(def(&d.name, Tag::Dialogue))
            }
            ast::ItemKind::Quest(q) => Some(def(&q.name, Tag::Quest)),
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
                        public: item.public,
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
            // The grammar takes these only inside a quest.
            ast::ItemKind::Requires(_) | ast::ItemKind::Transition(..) | ast::ItemKind::Part(_) => {
                self.diags.push(error(
                    "quest-only",
                    "this belongs in a `quest`".to_string(),
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
            name: name.to_string(),
            module: m,
            public: item.public,
            span: item.span,
            tag,
            item: Some(item),
            impl_index,
            parent: None,
        });
        if let Some(module) = self.modules.get_mut(m.0 as usize) {
            module.defs.push(id);
        }
        id
    }

    /// A quest's member or generated item, or a synthesised fn: an item of module `m` (listed in its
    /// `defs`) in no namespace, whose kind is set by its maker.
    pub(super) fn add_member(
        &mut self,
        m: ModId,
        name: String,
        span: ast::Span,
        tag: Tag,
        item: Option<&'a ast::Item>,
        parent: Option<DefId>,
    ) -> DefId {
        let id = DefId(index_u32(self.metas.len()));
        self.metas.push(Meta {
            name,
            module: m,
            public: false,
            span,
            tag,
            item,
            impl_index: None,
            parent,
        });
        if let Some(module) = self.modules.get_mut(m.0 as usize) {
            module.defs.push(id);
        }
        id
    }

    /// The compiler's own items (`hir::Builtins`), after the host enums: in the root, in no module's
    /// namespace, found after a module's own items.
    pub(super) fn collect_builtins(&mut self) {
        use hir::{Builtin, Field, Type, TypeKind, TypeName, Variant};
        let ty = |kind: TypeKind| Type {
            kind,
            span: ast::Span::default(),
        };
        let named = |n: TypeName| ty(TypeKind::Named(n, Vec::new()));
        let host_struct = |name: &str| match self.host.structs.get(name) {
            Some(i) => named(TypeName::HostStruct(*i)),
            None => ty(TypeKind::Error),
        };
        let field = |name: &str, ty: Type| Field {
            name: name.to_string(),
            ty,
            span: ast::Span::default(),
        };
        let variants = |names: &[&str]| {
            names
                .iter()
                .map(|n| Variant {
                    name: n.to_string(),
                    fields: Vec::new(),
                    span: ast::Span::default(),
                })
                .collect::<Vec<_>>()
        };
        let (player, person) = (host_struct("Player"), host_struct("Person"));
        let int = named(TypeName::Builtin(Builtin::Int));
        let objective = self.add_builtin(
            "Objective",
            Tag::Struct,
            hir::DefKind::Struct(vec![field("state", int.clone()), field("count", int)]),
        );
        let end_reason = self.add_builtin(
            "EndReason",
            Tag::Enum,
            hir::DefKind::Enum(variants(&[
                "Finished",
                "Left",
                "Timeout",
                "TooDeep",
                "Edited",
                "OutOfRange",
            ])),
        );
        let scene_end = self.add_builtin(
            "SceneEnd",
            Tag::Enum,
            hir::DefKind::Enum(variants(&["Finished", "Interrupted", "ActorLost"])),
        );
        let dialogue = named(TypeName::Builtin(Builtin::Dialogue));
        let talked = self.add_builtin(
            "Talked",
            Tag::Event,
            hir::DefKind::Event(vec![
                field("player", player.clone()),
                field("npc", person.clone()),
            ]),
        );
        let dialogue_ended = self.add_builtin(
            "DialogueEnded",
            Tag::Event,
            hir::DefKind::Event(vec![
                field("player", player),
                field("npc", person),
                field("dialogue", dialogue.clone()),
                field("reason", named(TypeName::Def(end_reason))),
                field("exit", named(TypeName::Builtin(Builtin::Exit))),
            ]),
        );
        let scene_ended = self.add_builtin(
            "SceneEnded",
            Tag::Event,
            hir::DefKind::Event(vec![
                field("scene", dialogue),
                field("reason", named(TypeName::Def(scene_end))),
            ]),
        );
        self.builtins = hir::Builtins {
            objective: Some(objective),
            talked: Some(talked),
            dialogue_ended: Some(dialogue_ended),
            scene_ended: Some(scene_ended),
            end_reason: Some(end_reason),
            scene_end: Some(scene_end),
        };
    }

    fn add_builtin(&mut self, name: &'static str, tag: Tag, kind: hir::DefKind) -> DefId {
        let id = DefId(index_u32(self.metas.len()));
        self.metas.push(Meta {
            name: name.to_string(),
            module: ROOT,
            public: true,
            span: ast::Span::default(),
            tag,
            item: None,
            impl_index: None,
            parent: None,
        });
        self.set_kind(id, kind);
        self.builtin_names.insert(name, id);
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

    pub(super) fn resolve_imports(&mut self) {
        loop {
            let mut progress = false;
            for i in 0..self.imports.len() {
                let Some(import) = self.imports.get(i) else {
                    continue;
                };
                if import.done.is_some() {
                    continue;
                }
                let (m, path, span, public) =
                    (import.module, import.path, import.span, import.public);
                if let Walk::Found(res, mut diags) = self.walk(m, path, span, Mode::Use) {
                    self.diags.append(&mut diags);
                    if public && !self.is_public(res) {
                        self.diags.push(error(
                            "private",
                            format!(
                                "`pub use` re-exports `{}`, which is private",
                                path.join("::")
                            ),
                            self.file_of(m),
                            span,
                        ));
                    }
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

    /// Whether the item `res` names was declared `pub` (a module: its `pub mod`).
    fn is_public(&self, res: Res) -> bool {
        match res {
            Res::Def(d) => self.metas.get(d.0 as usize).is_some_and(|m| m.public),
            Res::Mod(m) => {
                let name = self
                    .modules
                    .get(m.0 as usize)
                    .and_then(|module| module.path.last());
                match (self.parent_of(m), name) {
                    (Some(parent), Some(name)) => self
                        .scopes
                        .get(parent.0 as usize)
                        .and_then(|s| s.get(name.as_str()))
                        .is_some_and(|e| e.public),
                    // The crate root is everyone's.
                    _ => true,
                }
            }
            _ => true,
        }
    }

    pub(super) fn slot_res(&self, slot: Slot) -> Option<Res> {
        match slot {
            Slot::Res(res) => Some(res),
            Slot::Import(i) => self.imports.get(i).and_then(|imp| imp.done),
        }
    }

    /// Module `m`'s entry for `name`: what it means (`Some(None)` while its import is pending).
    pub(super) fn entry(&self, m: ModId, name: &str) -> Option<(Option<Res>, Entry)> {
        let entry = *self.scopes.get(m.0 as usize)?.get(name)?;
        Some((self.slot_res(entry.slot), entry))
    }

    /// Walks a path of two or more segments, or a `use` path, from module `from`.
    pub(super) fn walk(&self, from: ModId, path: &[String], span: ast::Span, mode: Mode) -> Walk {
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
                // A compiler builtin (`EndReason::Left`) or a host enum (`Occupation::Smith`).
                None if mode == Mode::Item && self.builtin_names.contains_key(name) => self
                    .builtin_names
                    .get(name)
                    .map_or(Res::Error, |d| Res::Def(*d)),
                None if mode == Mode::Item && self.host.enums.contains_key(name) => self
                    .host
                    .enums
                    .get(name)
                    .map_or(Res::Error, |d| Res::Def(*d)),
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

    /// A struct's or enum's variant or method `name`, or a dialogue's exit.
    pub(super) fn member(&self, d: DefId, name: &str) -> Option<Res> {
        let meta = self.metas.get(d.0 as usize)?;
        let variant = match meta.item.map(|i| &i.kind) {
            Some(ast::ItemKind::Enum(_, variants)) => variants.iter().position(|v| v.name == name),
            Some(_) => None,
            // A generated enum: its kind was set when it was made.
            None => match self.kinds.get(d.0 as usize) {
                Some(Some(hir::DefKind::Enum(variants))) => {
                    variants.iter().position(|v| v.name == name)
                }
                _ => None,
            },
        };
        if let Some(i) = variant {
            return Some(Res::Variant(d, index_u32(i)));
        }
        if meta.tag == Tag::Dialogue {
            return self.exit_index(d, name).map(|i| Res::Exit(d, i));
        }
        if !matches!(meta.tag, Tag::Struct | Tag::Enum) {
            return None;
        }
        self.methods
            .get(&(d, name.to_string()))
            .map(|m| Res::Def(*m))
    }

    // Pass 3: impls.

    pub(super) fn resolve_impls(&mut self) {
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
                let (mname, mspan) = (meta.name.clone(), meta.span);
                let file = self.file_of(meta.module);
                match self.methods.entry((owner, mname.clone())) {
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
}
