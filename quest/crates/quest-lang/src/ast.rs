//! The syntax tree the grammar builds.
//!
//! Every node carries its [`Span`] and its `depth`: 1 for a leaf, otherwise 1 + its deepest child, except
//! that a parenthesised expression is one deeper than its contents (parentheses make no node, but they
//! count as a level of source nesting). The grammar refuses to build a node deeper than
//! `PARSE_DEPTH_MAX` (`too-deep`), so every later pass may recurse over the tree, and dropping it recurses
//! at most that deep.

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
}

impl ExprKind {
    /// The depth of the deepest direct child (0 for a leaf).
    pub fn child_depth(&self) -> u32 {
        let d = |e: &Expr| e.depth;
        let od = |e: &Option<Box<Expr>>| e.as_ref().map_or(0, |e| e.depth);
        match self {
            ExprKind::Int(_)
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
}

impl TypeKind {
    pub fn child_depth(&self) -> u32 {
        match self {
            TypeKind::Unit => 0,
            TypeKind::Named(_, args) => args.iter().map(|t| t.depth).max().unwrap_or(0),
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
        }
    }
}

impl Type {
    /// `(list (dict str int))`, `()`, `npc::Line`.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        match &self.kind {
            TypeKind::Unit => "()".to_string(),
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
