//! The syntax tree the grammar builds.
//!
//! Every expression, type, pattern, statement, dialogue step and item carries its [`Span`] and its `depth`:
//! 1 for a leaf, otherwise 1 + its deepest child, except that a parenthesised expression is one deeper than
//! its contents (parentheses make no node, but they count as a level of source nesting). The parts that
//! have no depth of their own (a `match` case, a dialogue node, a choice option, a line-group member) count
//! toward their parent's. The grammar refuses to build a node deeper than `PARSE_DEPTH_MAX` (`too-deep`),
//! so every later pass may recurse over the tree, and dropping it recurses at most that deep.

mod dump;

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
    /// Stands where an expression failed to parse (an f-string's `{…}` part); the parse that made it
    /// reported a diagnostic. Resolution and every later pass skip it silently: no diagnostic, and its type
    /// is "unknown, already wrong", so no follow-on error cascades from it.
    Error,
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
            ExprKind::Error
            | ExprKind::Unit
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

/// A program's module tree, as `load_modules` reads it: every module reached from the crate root
/// `main.qs` by `mod` items, sorted by path (the root, whose path is empty, first).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Crate {
    pub modules: Vec<Module>,
}

/// One module: its path from the crate root (`["npc", "dialogue"]`), the file it was read from and that
/// file's items.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub path: Vec<String>,
    pub file: String,
    pub ast: File,
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
    /// A conversation or a bark: `dialogue name(…) …:`.
    Dialogue(Dialogue),
    /// A `scene` item; its `Dialogue.kind` is [`DialogueKind::Scene`].
    Scene(Dialogue),
    /// `[repeatable] quest Name[(options)] [at role]:` and its members.
    Quest(Quest),
    /// `[optional] role name: Type [where requirement]`.
    Role(Role),
    /// `stage Name:` and its members.
    Stage(String, Vec<Item>),
    /// `[optional] objective name "text" [@id] [at target] [count n]`.
    Objective(Objective),
    /// `requires cond`, a requirement on the whole quest definition.
    Requires(Expr),
    /// `-> Stage when cond` as a direct member of a stage: a completion transition (the stage it goes
    /// to, the condition).
    Transition(String, Expr),
    /// `part name on|at role:` and its members.
    Part(Part),
}

/// `[repeatable] quest Name[(options)] [at role]:` + body.
#[derive(Debug, Clone, PartialEq)]
pub struct Quest {
    pub name: String,
    pub repeatable: bool,
    /// `cooldown=…`, `exclusive=…`, `once_per_player=…` (checked later).
    pub options: Vec<Arg>,
    /// The role the home is anchored to; `None` = the first role.
    pub at: Option<String>,
    pub body: Vec<Item>,
}

/// `[optional] role name: Type [where cond]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Role {
    pub name: String,
    pub ty: Type,
    pub cond: Option<Expr>,
    pub optional: bool,
}

/// `[optional] objective name "text" [@id] [at target] [count n]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Objective {
    pub name: String,
    /// A string or an f-string literal.
    pub text: Expr,
    pub id: Option<LineId>,
    pub at: Option<Expr>,
    pub count: Option<Expr>,
    pub optional: bool,
}

/// `part name on role:` / `part name at role:` + body.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub name: String,
    pub anchor: PartAnchor,
    pub role: String,
    pub body: Vec<Item>,
}

/// What a part is anchored to: the role's entity (`on`) or the region it was bound in (`at`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartAnchor {
    On,
    At,
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
            ItemKind::Impl(_, items) | ItemKind::Stage(_, items) => items_depth(items),
            ItemKind::Quest(q) => items_depth(&q.body).max(args_depth(&q.options)),
            ItemKind::Part(p) => items_depth(&p.body),
            ItemKind::Enum(_, variants) | ItemKind::ErrorSet(_, variants) => variants
                .iter()
                .map(|v| fields_depth(&v.fields))
                .max()
                .unwrap_or(0),
            ItemKind::Mod(_) | ItemKind::Use(_) => 0,
            ItemKind::Let(l) => l.depth(),
            ItemKind::Dialogue(d) | ItemKind::Scene(d) => d.depth(),
            ItemKind::Role(r) => r.ty.depth.max(opt_depth(r.cond.as_ref())),
            ItemKind::Objective(o) => o
                .text
                .depth
                .max(opt_depth(o.at.as_ref()))
                .max(opt_depth(o.count.as_ref())),
            ItemKind::Requires(e) | ItemKind::Transition(_, e) => e.depth,
        }
    }
}

// Dialogue (spec 5.11).

/// `dialogue name(params) [for target [where filter]] [topic …] [exits a, b]:` + body, or
/// `scene name(cast) [where cast_cond] [uninterruptible]:` + body.
#[derive(Debug, Clone, PartialEq)]
pub struct Dialogue {
    pub kind: DialogueKind,
    pub name: String,
    pub params: Vec<Param>,
    pub target: Option<Expr>,
    /// `for npc where pred`: the predicate.
    pub filter: Option<Expr>,
    pub topic: Option<Topic>,
    /// The declared exits (the implicit `done` is not listed).
    pub exits: Vec<(String, Span)>,
    /// A scene's `where`.
    pub cast_cond: Option<Expr>,
    /// A scene's `uninterruptible`.
    pub uninterruptible: bool,
    pub body: Vec<DialogueEntry>,
}

/// Which item a [`Dialogue`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogueKind {
    /// `dialogue`: a conversation (or, with `(npc: Person)`, a bark).
    Conversation,
    /// `scene`.
    Scene,
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

/// `[speaker:] "text" [@id] [(ticks=n)] [when cond]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub speaker: Option<String>,
    /// A string or an f-string literal.
    pub text: Expr,
    pub id: Option<LineId>,
    /// `(ticks=n)`: overrides the line's derived duration.
    pub ticks: Option<Expr>,
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
    /// `-> node` (in a scene, a last step `-> other_scene` is a follow-up).
    Jump(String),
    /// `detour name`, or `detour name:` + exit arms (empty = a plain detour).
    Detour(String, Vec<ExitArm>),
    /// `emit Event(args)`.
    Emit(Path, Vec<Arg>),
    /// `return [exit]`.
    Return(Option<String>),
    /// `pause n`.
    Pause(Expr),
}

/// An arm of `detour name:`: `exit:` + the steps run when the callee left through it.
#[derive(Debug, Clone, PartialEq)]
pub struct ExitArm {
    pub exit: String,
    pub steps: Vec<Step>,
    pub span: Span,
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
            .max(opt_depth(self.ticks.as_ref()))
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
            .fold(
                opt_depth(self.target.as_ref())
                    .max(opt_depth(self.filter.as_ref()))
                    .max(opt_depth(self.cast_cond.as_ref()))
                    .max(topic),
                u32::max,
            )
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
            StepKind::Detour(_, arms) => arms
                .iter()
                .map(|a| steps_depth(&a.steps))
                .max()
                .unwrap_or(0),
            StepKind::Pause(e) => e.depth,
            StepKind::Jump(_) | StepKind::Return(_) => 0,
        }
    }
}
