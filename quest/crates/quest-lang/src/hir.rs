//! The resolved program (HIR): the syntax tree with every name replaced by what it means.
//!
//! - An item is a [`DefId`]: an index into [`Crate::defs`], assigned in module order (modules sorted by
//!   path, the root first), then declaration order; a method takes the next id where its `impl` stands.
//!   The host's enums come last ([`Crate::host_enums`]).
//! - A binding is a [`LocalId`]: an index into [`Crate::locals`], fresh for every parameter, `let`, `for`
//!   variable, pattern binding and `else err:` binding, so shadowing is explicit.
//! - A host function, struct or event is its index into the [`HostApi`](quest_vm::host::HostApi) tables
//!   ([`HostItem`]); a prelude name is a [`Prelude`].
//! - `use` is resolved away: an imported name is the item it names.
//! - Every node keeps its source position ([`Span`]; the file is its module's).
//!
//! An expression, type or pattern whose resolution failed is `Error`. Its diagnostic was reported, as for
//! the parser's `ast::ExprKind::Error`: later passes skip it silently and treat its type as unknown.
//!
//! Dialogues, scenes and quests (spec §5.11–5.12) are [`DefKind::Dialogue`] / [`DefKind::Quest`] items whose
//! expressions are synthesised pure functions, plus the items a quest generates: see `hir/dialogue.rs`.

mod dialogue;

pub use dialogue::*;

use crate::ast::{BinOp, PatLit, Receiver, Span, UnOp};
use std::collections::BTreeMap;

/// A module: an index into [`Crate::modules`]. The root is 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModId(pub u32);

/// An item: an index into [`Crate::defs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DefId(pub u32);

/// An expression: unique in the crate, assigned by resolution in construction order (children before their
/// parent), below [`Crate::expr_count`]. Inference records each expression's type by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExprId(pub u32);

/// A pattern node: unique in the crate, assigned by resolution in construction order (children before their
/// parent), below [`Crate::pat_count`]. Inference records what each one matches by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PatId(pub u32);

/// A binding: an index into [`Crate::locals`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalId(pub u32);

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Crate {
    /// Sorted by path, the root first.
    pub modules: Vec<Module>,
    pub defs: Vec<Def>,
    pub locals: Vec<Local>,
    /// The methods of each struct or enum, by (type, method name).
    pub methods: BTreeMap<(DefId, String), DefId>,
    /// The item of each host enum, by its index into `HostApi::enums`: an `enum` of the root with no
    /// source, after every script item (resolution's `collect_host_enums`).
    pub host_enums: Vec<DefId>,
    /// The compiler's own items every module sees.
    pub builtins: Builtins,
    /// Every [`ExprId`] is below this (a dialogue's constants — `pause`, `count` — leave unused ids).
    pub expr_count: u32,
    /// Every [`PatId`] is below this.
    pub pat_count: u32,
}

impl Crate {
    pub fn module(&self, id: ModId) -> Option<&Module> {
        self.modules.get(id.0 as usize)
    }

    pub fn def(&self, id: DefId) -> Option<&Def> {
        self.defs.get(id.0 as usize)
    }

    pub fn local(&self, id: LocalId) -> Option<&Local> {
        self.locals.get(id.0 as usize)
    }

    /// The item's path from the crate root: `main`, `rewards::give_gold`, `npc::Ledger::total`; a quest's
    /// item is under the quest's (`LostDog::stage`, `LostDog::plea$cond#0`).
    pub fn qualified_name(&self, id: DefId) -> String {
        let Some(def) = self.def(id) else {
            return String::new();
        };
        // A parent is a quest, which has none: one step.
        if let Some(parent) = def.parent
            && parent != id
            && self.def(parent).is_some_and(|p| p.parent.is_none())
        {
            return format!("{}::{}", self.qualified_name(parent), def.name);
        }
        let mut parts: Vec<&str> = self
            .module(def.module)
            .map(|m| m.path.iter().map(String::as_str).collect())
            .unwrap_or_default();
        if let DefKind::Fn(f) = &def.kind
            && let Some(owner) = f.owner.and_then(|o| self.def(o))
        {
            parts.push(&owner.name);
        }
        parts.push(&def.name);
        parts.join("::")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub path: Vec<String>,
    pub file: String,
    pub parent: Option<ModId>,
    /// The module's items in declaration order, methods included; then, as resolution makes them, a quest's
    /// members and generated names and the synthesised fns.
    pub defs: Vec<DefId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Def {
    pub name: String,
    pub module: ModId,
    pub public: bool,
    /// The item, from its keyword.
    pub span: Span,
    /// The quest a quest's item belongs to: a member (a part's too), a generated name, a synthesised fn of
    /// one of its dialogues. (The quest's own synthesised fns, `Q$where#0`, are items of its module.)
    pub parent: Option<DefId>,
    pub kind: DefKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DefKind {
    /// A function or a method.
    Fn(Fn),
    Struct(Vec<Field>),
    Enum(Vec<Variant>),
    /// `error Name = A | B(x: int) | OtherSet`.
    ErrorSet(Vec<ErrorMember>),
    /// A top-level `let`.
    Global(Global),
    /// A script `event`: a struct that handlers take.
    Event(Vec<Field>),
    /// A conversation, bark or scene.
    Dialogue(Dialogue),
    Quest(Quest),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fn {
    /// The struct or enum of the `impl` a method is in.
    pub owner: Option<DefId>,
    /// `self` / `&self` and its binding.
    pub receiver: Option<(Receiver, LocalId)>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// The pinned error set (`raises …`).
    pub raises: Option<Raises>,
    /// The event of an `@on(…)` handler.
    pub handler: Option<EventRef>,
    pub body: Vec<Stmt>,
    /// Set on a synthesised pure fn: where it stands.
    pub pure: Option<PureSite>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub local: LocalId,
    pub ty: Option<Type>,
    pub span: Span,
}

/// What `raises` names: an error set, or a single error name.
#[derive(Debug, Clone, PartialEq)]
pub enum Raises {
    Set(DefId),
    Error(String),
}

/// An event a handler takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventRef {
    Script(DefId),
    /// An index into `HostApi::events`.
    Host(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
    pub span: Span,
}

/// A member of an error set: another set, or an error name with its data.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorMember {
    Set(DefId, Span),
    Error(Variant),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Global {
    pub ty: Option<Type>,
    pub value: Expr,
}

/// A binding's name and where it was made.
#[derive(Debug, Clone, PartialEq)]
pub struct Local {
    pub name: String,
    pub span: Span,
}

/// A host item: an index into the host API's tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostItem {
    /// `HostApi::fns`: a function without a receiver (`say`, `world.get`). Methods (`p.has_tag`) are
    /// found by the receiver's type, during inference (`Typed::methods`).
    Fn(u32),
    /// `HostApi::structs`.
    Struct(u32),
    /// `HostApi::events`.
    Event(u32),
}

/// The names every module sees without a `use`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prelude {
    Some,
    None,
    Ok,
    Err,
    Option,
    Print,
    Range,
    Float,
    Int,
    Str,
    Assert,
    AssertEq,
    Abs,
    Min,
    Max,
    Sqrt,
    Floor,
    Ceil,
    Round,
    Sin,
    Cos,
    Pow,
    ParseInt,
    All,
}

/// Every prelude item by its name.
pub const PRELUDE: [(&str, Prelude); 24] = [
    ("Some", Prelude::Some),
    ("None", Prelude::None),
    ("Ok", Prelude::Ok),
    ("Err", Prelude::Err),
    ("Option", Prelude::Option),
    ("print", Prelude::Print),
    ("range", Prelude::Range),
    ("float", Prelude::Float),
    ("int", Prelude::Int),
    ("str", Prelude::Str),
    ("assert", Prelude::Assert),
    ("assert_eq", Prelude::AssertEq),
    ("abs", Prelude::Abs),
    ("min", Prelude::Min),
    ("max", Prelude::Max),
    ("sqrt", Prelude::Sqrt),
    ("floor", Prelude::Floor),
    ("ceil", Prelude::Ceil),
    ("round", Prelude::Round),
    ("sin", Prelude::Sin),
    ("cos", Prelude::Cos),
    ("pow", Prelude::Pow),
    ("parse_int", Prelude::ParseInt),
    ("all", Prelude::All),
];

impl Prelude {
    pub fn from_name(name: &str) -> Option<Prelude> {
        PRELUDE.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
    }
}

// Types.

#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Error,
    /// `()`.
    Unit,
    /// A named type and its arguments (`list[int]`; a user type takes none, which inference checks).
    Named(TypeName, Vec<Type>),
    /// `&T`.
    Ref(Box<Type>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeName {
    Builtin(Builtin),
    /// A struct, enum or script event.
    Def(DefId),
    /// `HostApi::structs`.
    HostStruct(u32),
    /// `HostApi::events`.
    HostEvent(u32),
}

/// The built-in type names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Builtin {
    Int,
    Float,
    Bool,
    Str,
    List,
    Dict,
    Option,
    Promise,
    /// A dialogue's id (`DialogueEnded.dialogue`, a dialogue's name as a value): opaque, compared with `==` /
    /// `!=`. Not nameable in a type annotation (`BUILTIN_TYPES` has no `Dialogue`): inferred only.
    Dialogue,
    /// A dialogue's exit (`confront::paid`): opaque, compared with `==` / `!=`. Not nameable in a type
    /// annotation: inferred only.
    Exit,
}

/// Every built-in type by its name.
pub const BUILTIN_TYPES: [(&str, Builtin); 8] = [
    ("int", Builtin::Int),
    ("float", Builtin::Float),
    ("bool", Builtin::Bool),
    ("str", Builtin::Str),
    ("list", Builtin::List),
    ("dict", Builtin::Dict),
    ("Option", Builtin::Option),
    ("Promise", Builtin::Promise),
];

// Expressions.

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub id: ExprId,
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FStrPart {
    Text(String),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A failed parse or resolution, already reported.
    Error,
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    FStr(Vec<FStrPart>),
    Local(LocalId),
    /// An item: a function, a struct (a constructor), an enum, a global, an event, a dialogue.
    Def(DefId),
    /// An enum's variant: the enum and the variant's index.
    Variant(DefId, u32),
    Host(HostItem),
    Prelude(Prelude),
    /// An error name (spec §5.7: any name, global, never declared): the head of a `raise`, alone or
    /// called with its data.
    ErrorName(String),
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Await(Box<Expr>),
    Try(Box<Expr>),
    /// A call or a construction.
    Call(Box<Expr>, Vec<Arg>),
    /// A field, or a method before its call (resolved by the receiver's type during inference:
    /// `Typed::methods`).
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Slice(Box<Expr>, Option<Box<Expr>>, Option<Box<Expr>>),
    /// The error fallback `e else fallback`.
    Else(Box<Expr>, Box<Expr>),
    Return(Option<Box<Expr>>),
    Break,
    Continue,
    Raise(Box<Expr>),
    /// `e else err:` + block: the error's binding and the block.
    FallbackBlock(LocalId, Vec<Stmt>),
    /// A function only the compiler lowers (`join`, `after`, `dialogue_depth`, …).
    Quest(QuestFn),
    /// A dialogue's exit `confront::paid`: the dialogue and the index into its `exits`.
    Exit(DefId, u32),
}

// Statements.

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Let(LocalId, Option<Type>, Expr),
    /// `place = value`; the place is a local, a global, a field, an index or `*r`.
    Assign(Expr, Expr),
    Expr(Expr),
    If(Vec<(Expr, Vec<Stmt>)>, Option<Vec<Stmt>>),
    While(Expr, Vec<Stmt>),
    For(Vec<LocalId>, Expr, Vec<Stmt>),
    Match(Expr, Vec<Case>),
    Pass,
    Break,
    Continue,
    Return(Option<Expr>),
    Raise(Expr),
    /// `-> Stage` in a quest's handler: set the stage and end the handler.
    Stage(StageIdx),
}

// Patterns.

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub id: PatId,
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PatField {
    pub name: Option<String>,
    pub pattern: Pattern,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    Error,
    Wild,
    Bind(LocalId),
    Lit(PatLit),
    /// A constructor, with its fields (none for `None`, `Stage::Done` or `ChestEmpty`).
    Ctor(Ctor, Vec<PatField>),
    Or(Vec<Pattern>),
}

/// What a constructor pattern names.
#[derive(Debug, Clone, PartialEq)]
pub enum Ctor {
    /// `Some`, `None`, `Ok`, `Err`.
    Prelude(Prelude),
    /// An enum's variant: the enum and the variant's index.
    Variant(DefId, u32),
    /// A struct or a script event.
    Def(DefId),
    /// A host struct or event.
    Host(HostItem),
    /// An error name.
    Error(String),
    /// An upper-case name that names nothing, outside `raise` / `Err(…)` (e.g. `match err: case
    /// NotEnough(have):` in an `else err:` block). Resolution does not report it; the type checker does:
    /// against an error or a fallible scrutinee it is that error name, otherwise it reports `unresolved`
    /// (with its "did you mean" note, the second field: the nearest name in scope, computed by resolution) at
    /// the pattern's span. Its sub-patterns are the `Ctor`'s fields.
    ErrorOrUnknown(String, Option<String>),
}
