//! The syntax tree the grammar builds.
//!
//! Every expression, type, pattern, statement, dialogue step and item carries its [`Span`] and its `depth`:
//! 1 for a leaf, otherwise 1 + its deepest child, except that a parenthesised expression is one deeper than
//! its contents (parentheses make no node, but they count as a level of source nesting). The parts that
//! have no depth of their own (a `match` case, a dialogue node, a choice option, a line-group member) count
//! toward their parent's. The grammar refuses to build a node deeper than `PARSE_DEPTH_MAX` (`too-deep`),
//! so every later pass may recurse over the tree, and dropping it recurses at most that deep.

use std::fmt::Write;

/// Where a node is: the 1-based line and column of its first token and its length in characters. A node
/// that spans several lines has the length of its first token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub len: u32,
}

/// A path: `x`, `Stage::Accepted`, `crate::rewards::give`. Segments are names or the words `self`,
/// `crate`, `super`.
pub type Path = Vec<String>;

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    pub depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Ref,
    Deref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// A call argument: positional (`f(x)`) or by name (`Ledger(owner="ana")`).
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
    /// `()`.
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    FStr(Vec<FStrPart>),
    Path(Path),
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `await e`.
    Await(Box<Expr>),
    /// `e?`.
    Try(Box<Expr>),
    /// A call or a construction: `f(x)`, `Ledger(owner="ana")`, `Stage::Accepted(5)`.
    Call(Box<Expr>, Vec<Arg>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    /// `e[a:b]`; either bound may be missing.
    Slice(Box<Expr>, Option<Box<Expr>>, Option<Box<Expr>>),
    /// The error fallback `e else fallback`.
    Else(Box<Expr>, Box<Expr>),
    /// The diverging fallbacks, which appear only as the right side of an `else`.
    Return(Option<Box<Expr>>),
    Break,
    Continue,
    Raise(Box<Expr>),
    /// The block fallback `e else err:` + block, the last link of an `else` chain that ends a `let`, an
    /// assignment or an expression statement: the error's binding and the block.
    FallbackBlock(String, Vec<Stmt>),
}

impl ExprKind {
    /// The depth of the deepest direct child (0 for a leaf).
    pub fn child_depth(&self) -> u32 {
        let d = |e: &Expr| e.depth;
        let od = |e: &Option<Box<Expr>>| e.as_ref().map_or(0, |e| e.depth);
        match self {
            ExprKind::Unit
            | ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Path(_)
            | ExprKind::Break
            | ExprKind::Continue => 0,
            ExprKind::FStr(parts) => parts
                .iter()
                .map(|p| match p {
                    FStrPart::Text(_) => 0,
                    FStrPart::Expr(e) => e.depth,
                })
                .max()
                .unwrap_or(0),
            ExprKind::List(items) => items.iter().map(d).max().unwrap_or(0),
            ExprKind::Dict(entries) => entries
                .iter()
                .map(|(k, v)| k.depth.max(v.depth))
                .max()
                .unwrap_or(0),
            ExprKind::Unary(_, e)
            | ExprKind::Await(e)
            | ExprKind::Try(e)
            | ExprKind::Field(e, _)
            | ExprKind::Raise(e) => e.depth,
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Else(a, b) => {
                a.depth.max(b.depth)
            }
            ExprKind::Call(callee, args) => args
                .iter()
                .map(|a| a.value.depth)
                .fold(callee.depth, u32::max),
            ExprKind::Slice(e, a, b) => e.depth.max(od(a)).max(od(b)),
            ExprKind::Return(e) => od(e),
            ExprKind::FallbackBlock(_, body) => block_depth(body),
        }
    }
}

/// A type annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    /// `()`.
    Unit,
    /// `int`, `npc::dialogue::Line`, or a generic one with arguments: `list[int]`, `dict[str, int]`.
    Named(Path, Vec<Type>),
    /// A reference, as a parameter takes one: `&list[int]`.
    Ref(Box<Type>),
}

impl TypeKind {
    pub fn child_depth(&self) -> u32 {
        match self {
            TypeKind::Unit => 0,
            TypeKind::Named(_, args) => args.iter().map(|t| t.depth).max().unwrap_or(0),
            TypeKind::Ref(t) => t.depth,
        }
    }
}

/// A `match` pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
    pub depth: u32,
}

/// A literal in a pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum PatLit {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

/// A field of a constructor pattern: positional (`Accepted(tick)`) or by name (`Accepted(tick=t)`).
#[derive(Debug, Clone, PartialEq)]
pub struct PatField {
    pub name: Option<String>,
    pub pattern: Pattern,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    /// `_`.
    Wild,
    /// A single name: a binding, or a unit variant or constant in scope (`None`) — resolution decides.
    Name(String),
    /// A path of two or more segments without fields: `Stage::Done`.
    Path(Path),
    /// A constructor: `Some(x)`, `Stage::Accepted(tick=t)`.
    Ctor(Path, Vec<PatField>),
    Lit(PatLit),
    /// Alternatives: `ChestEmpty | QuestClosed`.
    Or(Vec<Pattern>),
}

impl PatternKind {
    pub fn child_depth(&self) -> u32 {
        match self {
            PatternKind::Wild
            | PatternKind::Name(_)
            | PatternKind::Path(_)
            | PatternKind::Lit(_) => 0,
            PatternKind::Ctor(_, fields) => {
                fields.iter().map(|f| f.pattern.depth).max().unwrap_or(0)
            }
            PatternKind::Or(alts) => alts.iter().map(|p| p.depth).max().unwrap_or(0),
        }
    }
}

// Statements.

/// A statement.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
    pub depth: u32,
}

/// `let name[: ty] = value`, as a statement or a top-level item.
#[derive(Debug, Clone, PartialEq)]
pub struct Let {
    pub name: String,
    pub ty: Option<Type>,
    pub value: Expr,
}

/// One `case` of a `match`.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Let(Let),
    /// `place = value`: `x = 2`, `*r = v`, `a.b[0] = 1`.
    Assign(Expr, Expr),
    Expr(Expr),
    /// `if` and each `elif`: a condition and its block; then the `else` block.
    If(Vec<(Expr, Vec<Stmt>)>, Option<Vec<Stmt>>),
    While(Expr, Vec<Stmt>),
    /// `for v in e:` or `for k, v in e:` — one or two names.
    For(Vec<String>, Expr, Vec<Stmt>),
    Match(Expr, Vec<Case>),
    Pass,
    Break,
    Continue,
    Return(Option<Expr>),
    Raise(Expr),
    /// `-> Stage`: a stage change in a quest handler.
    Jump(String),
}

/// The deepest statement of a block (0 for none).
pub fn block_depth(body: &[Stmt]) -> u32 {
    body.iter().map(|s| s.depth).max().unwrap_or(0)
}

fn opt_depth(e: Option<&Expr>) -> u32 {
    e.map_or(0, |e| e.depth)
}

impl StmtKind {
    pub fn child_depth(&self) -> u32 {
        match self {
            StmtKind::Let(l) => l.depth(),
            StmtKind::Assign(a, b) => a.depth.max(b.depth),
            StmtKind::Expr(e) | StmtKind::Raise(e) => e.depth,
            StmtKind::If(branches, other) => branches
                .iter()
                .map(|(c, b)| c.depth.max(block_depth(b)))
                .fold(other.as_deref().map_or(0, block_depth), u32::max),
            StmtKind::While(c, b) | StmtKind::For(_, c, b) => c.depth.max(block_depth(b)),
            StmtKind::Match(e, cases) => cases
                .iter()
                .map(|c| {
                    c.pattern
                        .depth
                        .max(opt_depth(c.guard.as_ref()))
                        .max(block_depth(&c.body))
                })
                .fold(e.depth, u32::max),
            StmtKind::Return(e) => opt_depth(e.as_ref()),
            StmtKind::Pass | StmtKind::Break | StmtKind::Continue | StmtKind::Jump(_) => 0,
        }
    }
}

impl Let {
    fn depth(&self) -> u32 {
        self.value
            .depth
            .max(self.ty.as_ref().map_or(0, |t| t.depth))
    }
}

// Items.

/// A parsed file: its items in source order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct File {
    pub items: Vec<Item>,
}

/// An item: a top-level definition, a member of an `impl`, a quest or a stage. `span` starts at the item's
/// keyword (after its decorators).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    pub public: bool,
    pub decorators: Vec<Decorator>,
    pub span: Span,
    pub depth: u32,
}

/// `@on(PlayerClicked)`, `@pick(best)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Decorator {
    pub name: String,
    pub args: Vec<Arg>,
    pub span: Span,
}

/// How a method takes its receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver {
    /// `self`: a copy.
    Value,
    /// `&self`: a reference; the method may mutate.
    Ref,
}

/// A parameter: a name and an optional annotation.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<Type>,
    pub span: Span,
}

/// A field of a struct, an event, an enum variant or an error: `name: ty`.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnDef {
    pub name: String,
    pub receiver: Option<Receiver>,
    pub params: Vec<Param>,
    pub ret: Option<Type>,
    /// The pinned error set: `raises QuestError`.
    pub raises: Option<Path>,
    pub body: Vec<Stmt>,
}

/// An enum variant or a member of an error set: a name, with fields when it carries data.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
    pub span: Span,
}

/// One name a `use` brings in: `a::b` or `a::b as c`.
#[derive(Debug, Clone, PartialEq)]
pub struct UseImport {
    pub path: Path,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    Fn(FnDef),
    Struct(String, Vec<Field>),
    /// `impl Name:` and its methods (each an `ItemKind::Fn`).
    Impl(String, Vec<Item>),
    Enum(String, Vec<Variant>),
    /// `error Name = A | B(x: int)`.
    ErrorSet(String, Vec<Variant>),
    Mod(String),
    /// `use a::b`, `use a::b as c`, `use a::{b, c}`: the imports it makes.
    Use(Vec<UseImport>),
    /// A top-level (or quest) `let`.
    Let(Let),
    Event(String, Vec<Field>),
    Dialogue(Dialogue),
    /// `quest Name:` and its members.
    Quest(String, Vec<Item>),
    /// `role name: Type [where requirement]`.
    Role(String, Type, Option<Expr>),
    /// `stage Name:` and its members.
    Stage(String, Vec<Item>),
    /// `objective "text" [@id] [at target]`.
    Objective(Expr, Option<LineId>, Option<Expr>),
}

fn fields_depth(fields: &[Field]) -> u32 {
    fields.iter().map(|f| f.ty.depth).max().unwrap_or(0)
}

fn items_depth(items: &[Item]) -> u32 {
    items.iter().map(|i| i.depth).max().unwrap_or(0)
}

impl ItemKind {
    pub fn child_depth(&self) -> u32 {
        match self {
            ItemKind::Fn(f) => f
                .params
                .iter()
                .filter_map(|p| p.ty.as_ref().map(|t| t.depth))
                .chain(f.ret.as_ref().map(|t| t.depth))
                .fold(block_depth(&f.body), u32::max),
            ItemKind::Struct(_, fields) | ItemKind::Event(_, fields) => fields_depth(fields),
            ItemKind::Impl(_, items) | ItemKind::Quest(_, items) | ItemKind::Stage(_, items) => {
                items_depth(items)
            }
            ItemKind::Enum(_, variants) | ItemKind::ErrorSet(_, variants) => variants
                .iter()
                .map(|v| fields_depth(&v.fields))
                .max()
                .unwrap_or(0),
            ItemKind::Mod(_) | ItemKind::Use(_) => 0,
            ItemKind::Let(l) => l.depth(),
            ItemKind::Dialogue(d) => d.depth(),
            ItemKind::Role(_, ty, req) => ty.depth.max(opt_depth(req.as_ref())),
            ItemKind::Objective(text, _, at) => text.depth.max(opt_depth(at.as_ref())),
        }
    }
}

// Dialogue (spec 5.11).

/// `dialogue name(params) [for target] [topic …]:` + body.
#[derive(Debug, Clone, PartialEq)]
pub struct Dialogue {
    pub name: String,
    pub params: Vec<Param>,
    pub target: Option<Expr>,
    pub topic: Option<Topic>,
    pub body: Vec<DialogueEntry>,
}

/// `topic "text" [@id] [if cond] [priority n]`: the dialogue is a topic of the NPC's menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Topic {
    pub text: Expr,
    pub id: Option<LineId>,
    pub cond: Option<Expr>,
    pub priority: Option<i64>,
    pub span: Span,
}

/// An entry of a dialogue body: a node or a step (resolution refuses a body that mixes them).
#[derive(Debug, Clone, PartialEq)]
pub enum DialogueEntry {
    Node(Node),
    Step(Step),
}

/// `node name [when cond]:` + steps.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    pub cond: Option<Cond>,
    pub steps: Vec<Step>,
    pub span: Span,
}

/// A line id: `@farmer.start`.
#[derive(Debug, Clone, PartialEq)]
pub struct LineId {
    pub path: Vec<String>,
    pub span: Span,
}

/// `[speaker:] "text" [@id] [when cond]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub speaker: Option<String>,
    /// A string or an f-string literal.
    pub text: Expr,
    pub id: Option<LineId>,
    pub cond: Option<Cond>,
    pub span: Span,
}

/// The condition of a `when`: `once [per e] [if e]`, or an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    Once {
        per: Option<Expr>,
        guard: Option<Expr>,
    },
    Expr(Expr),
}

/// A dialogue step.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub kind: StepKind,
    pub span: Span,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepKind {
    Line(Line),
    /// `choice[(args)]:` + options.
    Choice(Vec<Arg>, Vec<ChoiceOption>),
    /// Consecutive `=>` members: a line group.
    Group(Vec<GroupMember>),
    /// `when cond:` + steps.
    When(Cond, Vec<Step>),
    /// `once [per e]:` + steps.
    Once(Option<Expr>, Vec<Step>),
    /// `-> node`.
    Jump(String),
    Detour(String),
    /// `emit Event(args)`.
    Emit(Path, Vec<Arg>),
    Return,
}

/// `=> line` and the steps that follow it when it is picked.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupMember {
    pub line: Line,
    pub steps: Vec<Step>,
    pub span: Span,
}

/// How an option is marked: `once` or `default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionMark {
    Once,
    Default,
}

/// An option of a `choice`. The hidden fallback `default:` has no text.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceOption {
    pub mark: Option<OptionMark>,
    pub text: Option<Expr>,
    pub id: Option<LineId>,
    pub guard: Option<Expr>,
    /// Empty for an option without a block.
    pub steps: Vec<Step>,
    pub span: Span,
}

/// The deepest step (0 for none).
pub fn steps_depth(steps: &[Step]) -> u32 {
    steps.iter().map(|s| s.depth).max().unwrap_or(0)
}

fn args_depth(args: &[Arg]) -> u32 {
    args.iter().map(|a| a.value.depth).max().unwrap_or(0)
}

impl Cond {
    pub fn depth(&self) -> u32 {
        match self {
            Cond::Once { per, guard } => opt_depth(per.as_ref()).max(opt_depth(guard.as_ref())),
            Cond::Expr(e) => e.depth,
        }
    }
}

impl Line {
    pub fn depth(&self) -> u32 {
        self.text
            .depth
            .max(self.cond.as_ref().map_or(0, Cond::depth))
    }
}

impl Dialogue {
    fn depth(&self) -> u32 {
        let topic = self
            .topic
            .as_ref()
            .map_or(0, |t| t.text.depth.max(opt_depth(t.cond.as_ref())));
        self.body
            .iter()
            .map(|e| match e {
                DialogueEntry::Node(n) => {
                    steps_depth(&n.steps).max(n.cond.as_ref().map_or(0, Cond::depth))
                }
                DialogueEntry::Step(s) => s.depth,
            })
            .chain(
                self.params
                    .iter()
                    .filter_map(|p| p.ty.as_ref().map(|t| t.depth)),
            )
            .fold(opt_depth(self.target.as_ref()).max(topic), u32::max)
    }
}

impl StepKind {
    pub fn child_depth(&self) -> u32 {
        match self {
            StepKind::Line(line) => line.depth(),
            StepKind::Choice(args, options) => options
                .iter()
                .map(|o| {
                    opt_depth(o.text.as_ref())
                        .max(opt_depth(o.guard.as_ref()))
                        .max(steps_depth(&o.steps))
                })
                .fold(args_depth(args), u32::max),
            StepKind::Group(members) => members
                .iter()
                .map(|m| m.line.depth().max(steps_depth(&m.steps)))
                .max()
                .unwrap_or(0),
            StepKind::When(cond, steps) => cond.depth().max(steps_depth(steps)),
            StepKind::Once(per, steps) => opt_depth(per.as_ref()).max(steps_depth(steps)),
            StepKind::Emit(_, args) => args_depth(args),
            StepKind::Jump(_) | StepKind::Detour(_) | StepKind::Return => 0,
        }
    }
}

// The s-expression dump: a test aid.

fn path_text(path: &Path) -> String {
    path.join("::")
}

impl Expr {
    /// `(+ 1 (* 2 3))`, `(call f x)`, … — the form the parser tests compare.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out);
        out
    }

    fn dump_into(&self, out: &mut String) {
        let op = |out: &mut String, name: &str, children: &[&Expr]| {
            out.push('(');
            out.push_str(name);
            for c in children {
                out.push(' ');
                c.dump_into(out);
            }
            out.push(')');
        };
        match &self.kind {
            ExprKind::Unit => out.push_str("()"),
            ExprKind::Int(i) => {
                let _ = write!(out, "{i}");
            }
            ExprKind::Float(f) => {
                let _ = write!(out, "{f:?}");
            }
            ExprKind::Bool(b) => {
                let _ = write!(out, "{b}");
            }
            ExprKind::Str(s) => {
                let _ = write!(out, "{s:?}");
            }
            ExprKind::Path(p) => out.push_str(&path_text(p)),
            ExprKind::FStr(parts) => {
                out.push_str("(fstr");
                for part in parts {
                    out.push(' ');
                    match part {
                        FStrPart::Text(t) => {
                            let _ = write!(out, "{t:?}");
                        }
                        FStrPart::Expr(e) => e.dump_into(out),
                    }
                }
                out.push(')');
            }
            ExprKind::List(items) => op(out, "list", &items.iter().collect::<Vec<_>>()),
            ExprKind::Dict(entries) => {
                out.push_str("(dict");
                for (k, v) in entries {
                    out.push_str(" (");
                    k.dump_into(out);
                    out.push(' ');
                    v.dump_into(out);
                    out.push(')');
                }
                out.push(')');
            }
            ExprKind::Unary(u, e) => {
                let name = match u {
                    UnOp::Neg => "neg",
                    UnOp::Not => "not",
                    UnOp::Ref => "ref",
                    UnOp::Deref => "deref",
                };
                op(out, name, &[e]);
            }
            ExprKind::Binary(b, l, r) => {
                let name = match b {
                    BinOp::Or => "or",
                    BinOp::And => "and",
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    BinOp::In => "in",
                    BinOp::NotIn => "not-in",
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Rem => "%",
                };
                op(out, name, &[l, r]);
            }
            ExprKind::Await(e) => op(out, "await", &[e]),
            ExprKind::Try(e) => op(out, "?", &[e]),
            ExprKind::Call(callee, args) => {
                out.push_str("(call ");
                callee.dump_into(out);
                for a in args {
                    out.push(' ');
                    if let Some(name) = &a.name {
                        out.push_str(name);
                        out.push('=');
                    }
                    a.value.dump_into(out);
                }
                out.push(')');
            }
            ExprKind::Field(e, name) => {
                out.push_str("(field ");
                e.dump_into(out);
                out.push(' ');
                out.push_str(name);
                out.push(')');
            }
            ExprKind::Index(e, i) => op(out, "index", &[e, i]),
            ExprKind::Slice(e, a, b) => {
                out.push_str("(slice ");
                e.dump_into(out);
                for bound in [a, b] {
                    out.push(' ');
                    match bound {
                        Some(x) => x.dump_into(out),
                        None => out.push('_'),
                    }
                }
                out.push(')');
            }
            ExprKind::Else(e, f) => op(out, "else", &[e, f]),
            ExprKind::Return(None) => out.push_str("(return)"),
            ExprKind::Return(Some(e)) => op(out, "return", &[e]),
            ExprKind::Break => out.push_str("(break)"),
            ExprKind::Continue => out.push_str("(continue)"),
            ExprKind::Raise(e) => op(out, "raise", &[e]),
            ExprKind::FallbackBlock(binding, body) => {
                let _ = write!(out, "(fallback {binding} ");
                dump_block(out, body);
                out.push(')');
            }
        }
    }
}

impl Type {
    /// `(list (dict str int))`, `()`, `npc::Line`.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        match &self.kind {
            TypeKind::Unit => "()".to_string(),
            TypeKind::Ref(t) => format!("(& {})", t.dump()),
            TypeKind::Named(path, args) if args.is_empty() => path_text(path),
            TypeKind::Named(path, args) => {
                let mut out = format!("({}", path_text(path));
                for a in args {
                    out.push(' ');
                    out.push_str(&a.dump());
                }
                out.push(')');
                out
            }
        }
    }
}

impl Pattern {
    /// `_`, `x`, `(Stage::Accepted tick=t)`, `(| A B)`.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        match &self.kind {
            PatternKind::Wild => "_".to_string(),
            PatternKind::Name(n) => n.clone(),
            PatternKind::Path(p) => path_text(p),
            PatternKind::Lit(PatLit::Int(i)) => i.to_string(),
            PatternKind::Lit(PatLit::Float(f)) => format!("{f:?}"),
            PatternKind::Lit(PatLit::Bool(b)) => b.to_string(),
            PatternKind::Lit(PatLit::Str(s)) => format!("{s:?}"),
            PatternKind::Ctor(path, fields) => {
                let mut out = format!("({}", path_text(path));
                for f in fields {
                    out.push(' ');
                    if let Some(name) = &f.name {
                        out.push_str(name);
                        out.push('=');
                    }
                    out.push_str(&f.pattern.dump());
                }
                out.push(')');
                out
            }
            PatternKind::Or(alts) => {
                let mut out = "(|".to_string();
                for a in alts {
                    out.push(' ');
                    out.push_str(&a.dump());
                }
                out.push(')');
                out
            }
        }
    }
}

fn dump_args(out: &mut String, args: &[Arg]) {
    for a in args {
        out.push(' ');
        if let Some(name) = &a.name {
            out.push_str(name);
            out.push('=');
        }
        a.value.dump_into(out);
    }
}

/// `(block s1 s2 …)`.
fn dump_block(out: &mut String, body: &[Stmt]) {
    out.push_str("(block");
    for s in body {
        out.push(' ');
        s.dump_into(out);
    }
    out.push(')');
}

impl Stmt {
    /// `(let x 1)`, `(= x 2)`, `(if c (block …) (else (block …)))`, … — the form the parser tests compare.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out);
        out
    }

    fn dump_into(&self, out: &mut String) {
        match &self.kind {
            StmtKind::Let(l) => l.dump_into(out),
            StmtKind::Assign(a, b) => {
                out.push_str("(= ");
                a.dump_into(out);
                out.push(' ');
                b.dump_into(out);
                out.push(')');
            }
            StmtKind::Expr(e) => e.dump_into(out),
            StmtKind::If(branches, other) => {
                for (i, (cond, body)) in branches.iter().enumerate() {
                    out.push_str(if i == 0 { "(if " } else { " (elif " });
                    cond.dump_into(out);
                    out.push(' ');
                    dump_block(out, body);
                    if i > 0 {
                        out.push(')');
                    }
                }
                if let Some(body) = other {
                    out.push_str(" (else ");
                    dump_block(out, body);
                    out.push(')');
                }
                out.push(')');
            }
            StmtKind::While(cond, body) => {
                out.push_str("(while ");
                cond.dump_into(out);
                out.push(' ');
                dump_block(out, body);
                out.push(')');
            }
            StmtKind::For(vars, iter, body) => {
                let _ = write!(out, "(for ({}) ", vars.join(" "));
                iter.dump_into(out);
                out.push(' ');
                dump_block(out, body);
                out.push(')');
            }
            StmtKind::Match(e, cases) => {
                out.push_str("(match ");
                e.dump_into(out);
                for c in cases {
                    let _ = write!(out, " (case {}", c.pattern.dump());
                    if let Some(g) = &c.guard {
                        out.push_str(" (if ");
                        g.dump_into(out);
                        out.push(')');
                    }
                    out.push(' ');
                    dump_block(out, &c.body);
                    out.push(')');
                }
                out.push(')');
            }
            StmtKind::Pass => out.push_str("(pass)"),
            StmtKind::Break => out.push_str("(break)"),
            StmtKind::Continue => out.push_str("(continue)"),
            StmtKind::Return(None) => out.push_str("(return)"),
            StmtKind::Return(Some(e)) => {
                out.push_str("(return ");
                e.dump_into(out);
                out.push(')');
            }
            StmtKind::Raise(e) => {
                out.push_str("(raise ");
                e.dump_into(out);
                out.push(')');
            }
            StmtKind::Jump(name) => {
                let _ = write!(out, "(jump {name})");
            }
        }
    }
}

impl Let {
    fn dump_into(&self, out: &mut String) {
        let _ = write!(out, "(let {}", self.name);
        if let Some(ty) = &self.ty {
            let _ = write!(out, ": {}", ty.dump());
        }
        out.push(' ');
        self.value.dump_into(out);
        out.push(')');
    }
}

fn dump_fields(out: &mut String, fields: &[Field]) {
    for f in fields {
        let _ = write!(out, " ({} {})", f.name, f.ty.dump());
    }
}

fn dump_variants(out: &mut String, variants: &[Variant]) {
    for v in variants {
        if v.fields.is_empty() {
            let _ = write!(out, " {}", v.name);
        } else {
            let _ = write!(out, " ({}", v.name);
            dump_fields(out, &v.fields);
            out.push(')');
        }
    }
}

fn dump_params(out: &mut String, receiver: Option<Receiver>, params: &[Param]) {
    let mut parts: Vec<String> = Vec::new();
    match receiver {
        Some(Receiver::Value) => parts.push("self".to_string()),
        Some(Receiver::Ref) => parts.push("&self".to_string()),
        None => {}
    }
    for p in params {
        parts.push(match &p.ty {
            Some(ty) => format!("{}: {}", p.name, ty.dump()),
            None => p.name.clone(),
        });
    }
    let _ = write!(out, "({})", parts.join(", "));
}

fn dump_items(out: &mut String, items: &[Item]) {
    for i in items {
        out.push(' ');
        i.dump_into(out);
    }
}

impl File {
    /// One item per line (a test aid).
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        self.items
            .iter()
            .map(Item::dump)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Item {
    /// `(@on PlayerClicked) pub (fn greet (e) …)`: the decorators, `pub`, then the item.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out);
        out
    }

    fn dump_into(&self, out: &mut String) {
        for d in &self.decorators {
            let _ = write!(out, "(@{}", d.name);
            dump_args(out, &d.args);
            out.push_str(") ");
        }
        if self.public {
            out.push_str("pub ");
        }
        match &self.kind {
            ItemKind::Fn(f) => {
                let _ = write!(out, "(fn {} ", f.name);
                dump_params(out, f.receiver, &f.params);
                if let Some(ret) = &f.ret {
                    let _ = write!(out, " -> {}", ret.dump());
                }
                if let Some(raises) = &f.raises {
                    let _ = write!(out, " raises {}", path_text(raises));
                }
                for s in &f.body {
                    out.push(' ');
                    s.dump_into(out);
                }
                out.push(')');
            }
            ItemKind::Struct(name, fields) => {
                let _ = write!(out, "(struct {name}");
                dump_fields(out, fields);
                out.push(')');
            }
            ItemKind::Event(name, fields) => {
                let _ = write!(out, "(event {name}");
                dump_fields(out, fields);
                out.push(')');
            }
            ItemKind::Impl(name, items) => {
                let _ = write!(out, "(impl {name}");
                dump_items(out, items);
                out.push(')');
            }
            ItemKind::Enum(name, variants) => {
                let _ = write!(out, "(enum {name}");
                dump_variants(out, variants);
                out.push(')');
            }
            ItemKind::ErrorSet(name, variants) => {
                let _ = write!(out, "(error {name}");
                dump_variants(out, variants);
                out.push(')');
            }
            ItemKind::Mod(name) => {
                let _ = write!(out, "(mod {name})");
            }
            ItemKind::Use(imports) => {
                out.push_str("(use");
                for u in imports {
                    match &u.alias {
                        None => {
                            let _ = write!(out, " {}", path_text(&u.path));
                        }
                        Some(alias) => {
                            let _ = write!(out, " (as {} {alias})", path_text(&u.path));
                        }
                    }
                }
                out.push(')');
            }
            ItemKind::Let(l) => l.dump_into(out),
            ItemKind::Dialogue(d) => d.dump_into(out),
            ItemKind::Quest(name, items) => {
                let _ = write!(out, "(quest {name}");
                dump_items(out, items);
                out.push(')');
            }
            ItemKind::Role(name, ty, req) => {
                let _ = write!(out, "(role {name} {}", ty.dump());
                if let Some(req) = req {
                    out.push_str(" (where ");
                    req.dump_into(out);
                    out.push(')');
                }
                out.push(')');
            }
            ItemKind::Stage(name, items) => {
                let _ = write!(out, "(stage {name}");
                dump_items(out, items);
                out.push(')');
            }
            ItemKind::Objective(text, id, at) => {
                out.push_str("(objective ");
                text.dump_into(out);
                dump_line_id(out, id.as_ref());
                if let Some(at) = at {
                    out.push_str(" (at ");
                    at.dump_into(out);
                    out.push(')');
                }
                out.push(')');
            }
        }
    }
}

fn dump_line_id(out: &mut String, id: Option<&LineId>) {
    if let Some(id) = id {
        let _ = write!(out, " {}", id.path.join("."));
    }
}

fn dump_steps(out: &mut String, steps: &[Step]) {
    for s in steps {
        out.push(' ');
        s.dump_into(out);
    }
}

impl Cond {
    fn dump_into(&self, out: &mut String) {
        match self {
            Cond::Expr(e) => e.dump_into(out),
            Cond::Once { per, guard } => {
                out.push_str("(once");
                if let Some(per) = per {
                    out.push_str(" per ");
                    per.dump_into(out);
                }
                if let Some(guard) = guard {
                    out.push_str(" if ");
                    guard.dump_into(out);
                }
                out.push(')');
            }
        }
    }
}

impl Line {
    /// `(line npc "text" farmer.start (when …))`.
    fn dump_into(&self, out: &mut String) {
        out.push_str("(line ");
        if let Some(speaker) = &self.speaker {
            out.push_str(speaker);
            out.push(' ');
        }
        self.text.dump_into(out);
        dump_line_id(out, self.id.as_ref());
        if let Some(cond) = &self.cond {
            out.push_str(" (when ");
            cond.dump_into(out);
            out.push(')');
        }
        out.push(')');
    }
}

impl Dialogue {
    fn dump_into(&self, out: &mut String) {
        let _ = write!(out, "(dialogue {} ", self.name);
        dump_params(out, None, &self.params);
        if let Some(target) = &self.target {
            out.push_str(" (for ");
            target.dump_into(out);
            out.push(')');
        }
        if let Some(topic) = &self.topic {
            out.push_str(" (topic ");
            topic.text.dump_into(out);
            dump_line_id(out, topic.id.as_ref());
            if let Some(cond) = &topic.cond {
                out.push_str(" (if ");
                cond.dump_into(out);
                out.push(')');
            }
            if let Some(priority) = topic.priority {
                let _ = write!(out, " (priority {priority})");
            }
            out.push(')');
        }
        for entry in &self.body {
            out.push(' ');
            match entry {
                DialogueEntry::Step(s) => s.dump_into(out),
                DialogueEntry::Node(n) => {
                    let _ = write!(out, "(node {}", n.name);
                    if let Some(cond) = &n.cond {
                        out.push_str(" (when ");
                        cond.dump_into(out);
                        out.push(')');
                    }
                    dump_steps(out, &n.steps);
                    out.push(')');
                }
            }
        }
        out.push(')');
    }
}

impl Step {
    /// `(line …)`, `(choice …)`, `(group (=> …) …)`, `(jump start)`, … (a test aid).
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out);
        out
    }

    fn dump_into(&self, out: &mut String) {
        match &self.kind {
            StepKind::Line(line) => line.dump_into(out),
            StepKind::Choice(args, options) => {
                out.push_str("(choice");
                dump_args(out, args);
                for o in options {
                    out.push_str(" (option");
                    match o.mark {
                        Some(OptionMark::Once) => out.push_str(" once"),
                        Some(OptionMark::Default) => out.push_str(" default"),
                        None => {}
                    }
                    if let Some(text) = &o.text {
                        out.push(' ');
                        text.dump_into(out);
                    }
                    dump_line_id(out, o.id.as_ref());
                    if let Some(guard) = &o.guard {
                        out.push_str(" (if ");
                        guard.dump_into(out);
                        out.push(')');
                    }
                    dump_steps(out, &o.steps);
                    out.push(')');
                }
                out.push(')');
            }
            StepKind::Group(members) => {
                out.push_str("(group");
                for m in members {
                    out.push_str(" (=> ");
                    m.line.dump_into(out);
                    dump_steps(out, &m.steps);
                    out.push(')');
                }
                out.push(')');
            }
            StepKind::When(cond, steps) => {
                out.push_str("(when ");
                cond.dump_into(out);
                dump_steps(out, steps);
                out.push(')');
            }
            StepKind::Once(per, steps) => {
                out.push_str("(once");
                if let Some(per) = per {
                    out.push_str(" per ");
                    per.dump_into(out);
                }
                dump_steps(out, steps);
                out.push(')');
            }
            StepKind::Jump(name) => {
                let _ = write!(out, "(jump {name})");
            }
            StepKind::Detour(name) => {
                let _ = write!(out, "(detour {name})");
            }
            StepKind::Emit(path, args) => {
                let _ = write!(out, "(emit {}", path_text(path));
                dump_args(out, args);
                out.push(')');
            }
            StepKind::Return => out.push_str("(return)"),
        }
    }
}
