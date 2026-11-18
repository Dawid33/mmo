//! Types: interned, so a type is a [`TyId`] and two types are equal exactly when their ids are.
//!
//! Ids are handed out in first-use order, which is deterministic because inference visits the program in a
//! fixed order. The simple types have fixed ids ([`TyId::ERROR`] … [`TyId::STR`]).
//!
//! Two types exist only for the checker:
//! - [`Ty::Error`]: the type of something whose error was already reported. It is compatible with every type
//!   and is never itself reported, so one mistake gives one diagnostic.
//! - [`Ty::Never`]: the type of an expression that does not produce a value (`return`, `break`, `raise`).
//!   Compatible with every type, like `Error`, but it is not a mistake.
//!
//! [`Ty::Var`] is an inference variable; no final type holds one (see `infer`).
//!
//! Errors (spec §5.7) add two types. An **error set** is a sorted, deduplicated list of error names, interned
//! as a [`SetId`].
//! - [`Ty::Fallible`]: the result of a fallible expression — a call of a function with a non-empty error set,
//!   an awaited promise (always). It is never a value (plan decision 2): it exists only where it is produced and is
//!   consumed there by `?`, `else` or a `match`.
//! - [`Ty::Caught`]: a caught error (`else err:`), one of the set's names with its data.
//!
//! A **promise's type has no error set**: `Promise[T]` is [`Ty::Promise`]`(T)`. What an `await` of it can fail
//! with is decided by where the promise comes from, not by its type (decision 21, `infer/promises.rs`).

use crate::hir::{Crate, DefId};
use quest_vm::host::HostApi;
use std::collections::BTreeMap;

/// An interned type: an index into [`Types`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TyId(pub u32);

impl TyId {
    pub const ERROR: TyId = TyId(0);
    pub const NEVER: TyId = TyId(1);
    pub const UNIT: TyId = TyId(2);
    pub const INT: TyId = TyId(3);
    pub const FLOAT: TyId = TyId(4);
    pub const BOOL: TyId = TyId(5);
    pub const STR: TyId = TyId(6);
}

/// An interned error set: an index into [`Types`]' sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SetId(pub u32);

/// The simple types, in the order of their fixed ids.
const FIXED: [Ty; 7] = [
    Ty::Error,
    Ty::Never,
    Ty::Unit,
    Ty::Int,
    Ty::Float,
    Ty::Bool,
    Ty::Str,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ty {
    Error,
    Never,
    /// `()`.
    Unit,
    Int,
    Float,
    Bool,
    Str,
    List(TyId),
    Dict(TyId, TyId),
    Option(TyId),
    /// The promised value.
    Promise(TyId),
    /// `&T`.
    Ref(TyId),
    /// A script struct, enum or event.
    Adt(DefId),
    /// `HostApi::structs`.
    HostStruct(u32),
    /// `HostApi::events`.
    HostEvent(u32),
    /// An inference variable.
    Var(u32),
    /// A fallible result: a `T`, or one of the set's errors.
    Fallible(TyId, SetId),
    /// A caught error: one of the set's errors.
    Caught(SetId),
    /// A dialogue's id (`farmer_talk`, `DialogueEnded.dialogue`): opaque, compared with `==` / `!=`.
    Dialogue,
    /// A dialogue's exit (`confront::paid`, `DialogueEnded.exit`): opaque, compared with `==` / `!=`.
    Exit,
}

impl Ty {
    /// The types this one is made of, in order.
    pub fn children(self) -> Vec<TyId> {
        match self {
            Ty::List(t) | Ty::Option(t) | Ty::Promise(t) | Ty::Ref(t) | Ty::Fallible(t, _) => {
                vec![t]
            }
            Ty::Dict(k, v) => vec![k, v],
            _ => Vec::new(),
        }
    }

    /// This type with its children replaced, in [`Ty::children`]'s order (missing ones are the error type).
    pub fn with_children(self, children: &[TyId]) -> Ty {
        let at = |i: usize| children.get(i).copied().unwrap_or(TyId::ERROR);
        match self {
            Ty::List(_) => Ty::List(at(0)),
            Ty::Option(_) => Ty::Option(at(0)),
            Ty::Promise(_) => Ty::Promise(at(0)),
            Ty::Ref(_) => Ty::Ref(at(0)),
            Ty::Dict(_, _) => Ty::Dict(at(0), at(1)),
            Ty::Fallible(_, set) => Ty::Fallible(at(0), set),
            other => other,
        }
    }
}

/// The type interner.
#[derive(Debug, Clone)]
pub struct Types {
    tys: Vec<Ty>,
    ids: BTreeMap<Ty, TyId>,
    /// By id: whether the type holds no variable, so walks over types stop there.
    ground: Vec<bool>,
    /// The error sets, by [`SetId`].
    sets: Vec<Vec<String>>,
    set_ids: BTreeMap<Vec<String>, SetId>,
}

impl Default for Types {
    fn default() -> Types {
        Types::new()
    }
}

impl Types {
    pub fn new() -> Types {
        let mut types = Types {
            tys: Vec::new(),
            ids: BTreeMap::new(),
            ground: Vec::new(),
            sets: Vec::new(),
            set_ids: BTreeMap::new(),
        };
        for ty in FIXED {
            types.intern(ty);
        }
        types
    }

    pub fn intern(&mut self, ty: Ty) -> TyId {
        if let Some(id) = self.ids.get(&ty) {
            return *id;
        }
        let id = TyId(u32::try_from(self.tys.len()).unwrap_or(u32::MAX));
        let ground = !matches!(ty, Ty::Var(_)) && ty.children().iter().all(|c| self.is_ground(*c));
        self.tys.push(ty);
        self.ids.insert(ty, id);
        self.ground.push(ground);
        id
    }

    /// The id of `ty` when it was interned.
    pub fn lookup(&self, ty: Ty) -> Option<TyId> {
        self.ids.get(&ty).copied()
    }

    /// Whether the type holds no variable (bound or not).
    pub fn is_ground(&self, id: TyId) -> bool {
        self.ground.get(id.0 as usize).copied().unwrap_or(true)
    }

    /// Interns an error set (any order, duplicates allowed).
    pub fn set(&mut self, names: impl IntoIterator<Item = String>) -> SetId {
        let names: Vec<String> = names
            .into_iter()
            .collect::<std::collections::BTreeSet<String>>()
            .into_iter()
            .collect();
        if let Some(id) = self.set_ids.get(&names) {
            return *id;
        }
        let id = SetId(u32::try_from(self.sets.len()).unwrap_or(u32::MAX));
        self.sets.push(names.clone());
        self.set_ids.insert(names, id);
        id
    }

    /// An error set's names, sorted; an unknown id is empty.
    pub fn names(&self, id: SetId) -> &[String] {
        self.sets.get(id.0 as usize).map_or(&[], Vec::as_slice)
    }

    /// What `id` is; an unknown id is the error type.
    pub fn get(&self, id: TyId) -> Ty {
        self.tys.get(id.0 as usize).copied().unwrap_or(Ty::Error)
    }
}

/// Printing deeper than this shows `…`: messages stay short, and printing never recurses far.
const PRINT_DEPTH_MAX: u32 = 16;

/// Prints types as source writes them (`int`, `list[int]`, `dict[str, int]`, `Option[int]`, `&Ledger`,
/// `npc::Ledger`); the checker's own types print as `{error}`, `!` (never) and `_` (unknown); a fallible result
/// as `int raises A | B`, a caught error as `error[A | B]`.
pub struct Printer<'a> {
    pub types: &'a Types,
    pub krate: &'a Crate,
    pub host: &'a HostApi,
}

impl Printer<'_> {
    pub fn print(&self, id: TyId) -> String {
        let mut out = String::new();
        self.write(id, 0, &mut out);
        out
    }

    fn write(&self, id: TyId, depth: u32, out: &mut String) {
        if depth > PRINT_DEPTH_MAX {
            out.push('…');
            return;
        }
        let inner = |name: &str, args: &[TyId], out: &mut String| {
            out.push_str(name);
            out.push('[');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                self.write(*a, depth.saturating_add(1), out);
            }
            out.push(']');
        };
        match self.types.get(id) {
            Ty::Error => out.push_str("{error}"),
            Ty::Never => out.push('!'),
            Ty::Unit => out.push_str("()"),
            Ty::Int => out.push_str("int"),
            Ty::Float => out.push_str("float"),
            Ty::Bool => out.push_str("bool"),
            Ty::Str => out.push_str("str"),
            Ty::List(t) => inner("list", &[t], out),
            Ty::Dict(k, v) => inner("dict", &[k, v], out),
            Ty::Option(t) => inner("Option", &[t], out),
            Ty::Promise(t) => inner("Promise", &[t], out),
            Ty::Ref(t) => {
                out.push('&');
                self.write(t, depth.saturating_add(1), out);
            }
            Ty::Adt(d) => out.push_str(&self.krate.qualified_name(d)),
            Ty::HostStruct(i) => out.push_str(
                self.host
                    .structs
                    .get(i as usize)
                    .map_or("?", |s| s.name.as_str()),
            ),
            Ty::HostEvent(i) => out.push_str(
                self.host
                    .events
                    .get(i as usize)
                    .map_or("?", |e| e.name.as_str()),
            ),
            Ty::Var(_) => out.push('_'),
            Ty::Fallible(t, set) => {
                self.write(t, depth.saturating_add(1), out);
                if !self.types.names(set).is_empty() {
                    out.push_str(" raises ");
                    out.push_str(&self.types.names(set).join(" | "));
                }
            }
            Ty::Dialogue => out.push_str("Dialogue"),
            Ty::Exit => out.push_str("Exit"),
            Ty::Caught(set) => {
                out.push_str("error[");
                out.push_str(&self.types.names(set).join(" | "));
                out.push(']');
            }
        }
    }
}
