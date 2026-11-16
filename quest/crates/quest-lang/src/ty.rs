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
}

impl Ty {
    /// The types this one is made of, in order.
    pub fn children(self) -> Vec<TyId> {
        match self {
            Ty::List(t) | Ty::Option(t) | Ty::Promise(t) | Ty::Ref(t) => vec![t],
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

    /// Whether the type holds no variable (bound or not).
    pub fn is_ground(&self, id: TyId) -> bool {
        self.ground.get(id.0 as usize).copied().unwrap_or(true)
    }

    /// What `id` is; an unknown id is the error type.
    pub fn get(&self, id: TyId) -> Ty {
        self.tys.get(id.0 as usize).copied().unwrap_or(Ty::Error)
    }
}

/// Printing deeper than this shows `…`: messages stay short, and printing never recurses far.
const PRINT_DEPTH_MAX: u32 = 16;

/// Prints types as source writes them (`int`, `list[int]`, `dict[str, int]`, `Option[int]`, `&Ledger`,
/// `npc::Ledger`); the checker's own types print as `{error}`, `!` (never) and `_` (unknown).
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
            Ty::HostEvent(i) => {
                out.push_str(self.host.events.get(i as usize).map_or("?", String::as_str))
            }
            Ty::Var(_) => out.push('_'),
        }
    }
}
