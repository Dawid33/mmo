//! The parser: the LALRPOP grammar (`grammar.lalrpop`, generated into `OUT_DIR` by `build.rs`) run over
//! the lexer's tokens, the helpers its actions call, and the mapping of its errors to diagnostics.
//!
//! The grammar's location type is the token's index in the slice it parses; diagnostics map it back to
//! the token's line, column and length.

use crate::ast::{
    BinOp, Expr, ExprKind, FStrPart, PatLit, Path, Pattern, PatternKind, Span, Type, TypeKind, UnOp,
};
use crate::diag::Diagnostic;
use crate::lex::{FPart, Tok, Token, lex};
use lalrpop_util::ParseError;
use quest_vm::consts::PARSE_DEPTH_MAX;

lalrpop_util::lalrpop_mod!(
    #[allow(clippy::all, clippy::pedantic, unused)]
    grammar
);

/// Why a grammar action refused to build a node.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseFail {
    /// The node at this token would be deeper than `PARSE_DEPTH_MAX`.
    TooDeep(usize),
    /// A second comparison operator at this token: `a < b < c`.
    Chained(usize),
    /// An f-string's `{expr}` part failed to parse; its diagnostics, already positioned.
    Nested(Vec<Diagnostic>),
}

pub(crate) type PErr = ParseError<usize, Tok, ParseFail>;

/// Where an input ends (line, column): the position of an error at its end.
pub(crate) type End = (u32, u32);

/// The span of the tokens `l..r` (see [`Span`]).
pub(crate) fn span(toks: &[Token], l: usize, r: usize) -> Span {
    let Some(first) = toks.get(l) else {
        return Span::default();
    };
    let len = match r.checked_sub(1).and_then(|last| toks.get(last)) {
        Some(last) if r > l && last.line == first.line => {
            last.col.saturating_add(last.len).saturating_sub(first.col)
        }
        _ => first.len,
    };
    Span {
        line: first.line,
        col: first.col,
        len,
    }
}

fn check_depth(depth: u32, at: usize) -> Result<u32, PErr> {
    if usize::try_from(depth).map_or(true, |d| d > PARSE_DEPTH_MAX) {
        return Err(ParseError::User {
            error: ParseFail::TooDeep(at),
        });
    }
    Ok(depth)
}

/// Builds an expression node over the tokens `l..r`, refusing one deeper than `PARSE_DEPTH_MAX`.
pub(crate) fn expr(toks: &[Token], l: usize, r: usize, kind: ExprKind) -> Result<Expr, PErr> {
    let depth = check_depth(kind.child_depth().saturating_add(1), l)?;
    Ok(Expr {
        kind,
        span: span(toks, l, r),
        depth,
    })
}

pub(crate) fn unary(toks: &[Token], l: usize, r: usize, op: UnOp, e: Expr) -> Result<Expr, PErr> {
    expr(toks, l, r, ExprKind::Unary(op, Box::new(e)))
}

pub(crate) fn binary(
    toks: &[Token],
    l: usize,
    r: usize,
    op: BinOp,
    a: Expr,
    b: Expr,
) -> Result<Expr, PErr> {
    expr(toks, l, r, ExprKind::Binary(op, Box::new(a), Box::new(b)))
}

/// A parenthesised expression (the `(` at token `l`): no node, but one level deeper.
pub(crate) fn paren(mut e: Expr, l: usize) -> Result<Expr, PErr> {
    e.depth = check_depth(e.depth.saturating_add(1), l)?;
    Ok(e)
}

/// The error for a second comparison operator at token `at`.
pub(crate) fn chained<T>(at: usize) -> Result<T, PErr> {
    Err(ParseError::User {
        error: ParseFail::Chained(at),
    })
}

/// What follows an expression in a postfix chain.
pub(crate) enum Suffix {
    Call(Vec<crate::ast::Arg>),
    Field(String),
    Index(Expr),
    Slice(Option<Expr>, Option<Expr>),
}

pub(crate) fn suffix(toks: &[Token], l: usize, r: usize, e: Expr, s: Suffix) -> Result<Expr, PErr> {
    let e = Box::new(e);
    let kind = match s {
        Suffix::Call(args) => ExprKind::Call(e, args),
        Suffix::Field(name) => ExprKind::Field(e, name),
        Suffix::Index(i) => ExprKind::Index(e, Box::new(i)),
        Suffix::Slice(a, b) => ExprKind::Slice(e, a.map(Box::new), b.map(Box::new)),
    };
    expr(toks, l, r, kind)
}

/// Parses the `{expr}` parts of an f-string token (token `at`).
pub(crate) fn fstring(
    toks: &[Token],
    file: &str,
    at: usize,
    parts: Vec<FPart>,
) -> Result<ExprKind, PErr> {
    let fallback = toks.get(at).map_or((1, 1), |t| (t.line, t.col));
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            FPart::Text(t) => out.push(FStrPart::Text(t)),
            FPart::Expr(inner) => {
                let end = inner
                    .last()
                    .map_or(fallback, |t| (t.line, t.col.saturating_add(t.len)));
                match expr_tokens(file, &inner, end) {
                    Ok(e) => out.push(FStrPart::Expr(e)),
                    Err(diags) => {
                        return Err(ParseError::User {
                            error: ParseFail::Nested(diags),
                        });
                    }
                }
            }
        }
    }
    Ok(ExprKind::FStr(out))
}

pub(crate) fn ty(toks: &[Token], l: usize, r: usize, kind: TypeKind) -> Result<Type, PErr> {
    let depth = check_depth(kind.child_depth().saturating_add(1), l)?;
    Ok(Type {
        kind,
        span: span(toks, l, r),
        depth,
    })
}

pub(crate) fn pat(toks: &[Token], l: usize, r: usize, kind: PatternKind) -> Result<Pattern, PErr> {
    let depth = check_depth(kind.child_depth().saturating_add(1), l)?;
    Ok(Pattern {
        kind,
        span: span(toks, l, r),
        depth,
    })
}

/// A path pattern without fields: `_`, a single name, or a longer path.
pub(crate) fn path_pat(toks: &[Token], l: usize, r: usize, mut p: Path) -> Result<Pattern, PErr> {
    let kind = if p.len() == 1 {
        match p.pop() {
            Some(name) if name == "_" => PatternKind::Wild,
            Some(name) => PatternKind::Name(name),
            None => PatternKind::Wild,
        }
    } else {
        PatternKind::Path(p)
    };
    pat(toks, l, r, kind)
}

pub(crate) fn lit_pat(toks: &[Token], l: usize, r: usize, lit: PatLit) -> Result<Pattern, PErr> {
    pat(toks, l, r, PatternKind::Lit(lit))
}

// Running the parser.

type Triple = Result<(usize, Tok, usize), ParseFail>;

fn triples(toks: &[Token]) -> impl Iterator<Item = Triple> + '_ {
    toks.iter()
        .enumerate()
        .map(|(i, t)| Ok((i, t.kind.clone(), i.saturating_add(1))))
}

/// Parses `toks` (a whole expression; `end` is where the input ends, for an error there).
pub(crate) fn expr_tokens(file: &str, toks: &[Token], end: End) -> Result<Expr, Vec<Diagnostic>> {
    grammar::ExprEntryParser::new()
        .parse(toks, file, triples(toks))
        .map_err(|e| diagnose(file, toks, end, e))
}

pub(crate) fn type_tokens(file: &str, toks: &[Token], end: End) -> Result<Type, Vec<Diagnostic>> {
    grammar::TypeEntryParser::new()
        .parse(toks, file, triples(toks))
        .map_err(|e| diagnose(file, toks, end, e))
}

pub(crate) fn pattern_tokens(
    file: &str,
    toks: &[Token],
    end: End,
) -> Result<Pattern, Vec<Diagnostic>> {
    grammar::PatternEntryParser::new()
        .parse(toks, file, triples(toks))
        .map_err(|e| diagnose(file, toks, end, e))
}

const TEST_FILE: &str = "main.qs";

/// Lexes one line of test input: the tokens without the layout tokens that close it, and where it
/// ends.
fn lex_line(text: &str) -> Result<(Vec<Token>, End), Vec<Diagnostic>> {
    let (mut toks, diags) = lex(TEST_FILE, text);
    if !diags.is_empty() {
        return Err(diags);
    }
    let mut end = (1, 1);
    while let Some(last) = toks.last() {
        if !matches!(last.kind, Tok::Eof | Tok::Newline | Tok::Dedent) {
            break;
        }
        end = (last.line, last.col);
        toks.pop();
    }
    Ok((toks, end))
}

/// Parses `text` as one expression and dumps it as an s-expression (a test aid).
#[doc(hidden)]
pub fn parse_expr_str(text: &str) -> Result<String, Vec<Diagnostic>> {
    let (toks, end) = lex_line(text)?;
    expr_tokens(TEST_FILE, &toks, end).map(|e| e.dump())
}

/// Parses `text` as one type annotation and dumps it (a test aid).
#[doc(hidden)]
pub fn parse_type_str(text: &str) -> Result<String, Vec<Diagnostic>> {
    let (toks, end) = lex_line(text)?;
    type_tokens(TEST_FILE, &toks, end).map(|t| t.dump())
}

/// Parses `text` as one `match` pattern and dumps it (a test aid).
#[doc(hidden)]
pub fn parse_pattern_str(text: &str) -> Result<String, Vec<Diagnostic>> {
    let (toks, end) = lex_line(text)?;
    pattern_tokens(TEST_FILE, &toks, end).map(|p| p.dump())
}

// Errors.

/// The diagnostics for a parse error. `end` is where the input ends.
fn diagnose(file: &str, toks: &[Token], end: End, err: PErr) -> Vec<Diagnostic> {
    let at = |i: usize| {
        toks.get(i)
            .map_or((end.0, end.1, 0), |t| (t.line, t.col, t.len))
    };
    let (code, message, (line, col, len)) = match err {
        ParseError::User {
            error: ParseFail::Nested(diags),
        } => return diags,
        ParseError::User {
            error: ParseFail::TooDeep(i),
        } => (
            "too-deep",
            format!("this is nested more than {PARSE_DEPTH_MAX} levels deep"),
            at(i),
        ),
        ParseError::User {
            error: ParseFail::Chained(i),
        } => (
            "chained-comparison",
            "comparisons do not chain: write `a < b and b < c`".to_string(),
            at(i),
        ),
        ParseError::UnrecognizedToken {
            token: (i, tok, _),
            expected,
        } => (
            "syntax",
            format!(
                "expected {}, found {}",
                expected_text(&expected),
                describe(&tok)
            ),
            at(i),
        ),
        ParseError::UnrecognizedEof { expected, .. } => (
            "syntax",
            format!(
                "expected {}, found the end of the input",
                expected_text(&expected)
            ),
            (end.0, end.1, 0),
        ),
        ParseError::ExtraToken { token: (i, tok, _) } => {
            ("syntax", format!("unexpected {}", describe(&tok)), at(i))
        }
        ParseError::InvalidToken { location } => {
            ("syntax", "unexpected input".to_string(), at(location))
        }
    };
    vec![Diagnostic::error(code, message, file, line, col, len)]
}

/// The terminals that start an expression; when the parser expects one of them it expects an
/// expression, and the message says so once.
const EXPR_START: &[&str] = &[
    "NAME", "INT", "FLOAT", "STRING", "FSTRING", "true", "false", "(", "[", "{", "-", "&", "*",
    "not", "await", "self", "crate", "super",
];

/// The terminals that continue an expression (binary operators and postfix forms).
const OPERATORS: &[&str] = &[
    "+", "-", "*", "/", "%", "==", "!=", "<", "<=", ">", ">=", "in", "not", "and", "or", "else",
    "?", "(", ".", "[", "::",
];

/// A friendly name for one terminal.
fn terminal_name(t: &str) -> String {
    match t {
        "NAME" => "a name".to_string(),
        "INT" | "FLOAT" => "a number".to_string(),
        "STRING" => "a string".to_string(),
        "FSTRING" => "an f-string".to_string(),
        "INDENT" => "an indented block".to_string(),
        "DEDENT" => "the end of the block".to_string(),
        "NEWLINE" => "the end of the line".to_string(),
        "EOF" => "the end of the file".to_string(),
        other => format!("`{other}`"),
    }
}

/// "`)`, `,` or an operator" for LALRPOP's list of expected terminals (`"\")\""`, `"NAME"`, …).
fn expected_text(expected: &[String]) -> String {
    let names: Vec<&str> = expected
        .iter()
        .map(|e| {
            e.strip_prefix('"')
                .and_then(|e| e.strip_suffix('"'))
                .unwrap_or(e)
        })
        .collect();
    let wants_expr = names.contains(&"await");
    let wants_operator = !wants_expr && names.contains(&"+");
    // `self`, `crate` and `super` start a path as a name does.
    let wants_name = names.contains(&"NAME");
    let mut out: Vec<String> = Vec::new();
    let mut groups: Vec<&str> = Vec::new();
    for name in names {
        if wants_name && matches!(name, "self" | "crate" | "super") {
            continue;
        }
        if wants_expr && EXPR_START.contains(&name) {
            if !groups.contains(&"an expression") {
                groups.push("an expression");
            }
            continue;
        }
        if wants_operator && OPERATORS.contains(&name) {
            if !groups.contains(&"an operator") {
                groups.push("an operator");
            }
            continue;
        }
        let friendly = terminal_name(name);
        if !out.contains(&friendly) {
            out.push(friendly);
        }
    }
    out.extend(groups.into_iter().map(str::to_string));
    match out.split_last() {
        None => "nothing more".to_string(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    }
}

/// How a message names a token it found.
fn describe(tok: &Tok) -> String {
    match tok {
        Tok::Indent => "an indented block".to_string(),
        Tok::Dedent => "the end of the block".to_string(),
        Tok::Newline => "the end of the line".to_string(),
        Tok::Eof => "the end of the file".to_string(),
        Tok::Ident(name) => format!("the name `{name}`"),
        Tok::Int(i) => format!("the number `{i}`"),
        Tok::Float(f) => format!("the number `{f:?}`"),
        Tok::Str(_) => "a string".to_string(),
        Tok::FString(_) => "an f-string".to_string(),
        fixed => format!("`{}`", spelling(fixed)),
    }
}

/// The source spelling of a token without data.
fn spelling(tok: &Tok) -> &'static str {
    match tok {
        Tok::Fn => "fn",
        Tok::Let => "let",
        Tok::If => "if",
        Tok::Elif => "elif",
        Tok::Else => "else",
        Tok::While => "while",
        Tok::For => "for",
        Tok::In => "in",
        Tok::Not => "not",
        Tok::And => "and",
        Tok::Or => "or",
        Tok::Return => "return",
        Tok::Break => "break",
        Tok::Continue => "continue",
        Tok::Pass => "pass",
        Tok::Match => "match",
        Tok::Case => "case",
        Tok::Struct => "struct",
        Tok::Impl => "impl",
        Tok::Enum => "enum",
        Tok::Error => "error",
        Tok::Mod => "mod",
        Tok::Use => "use",
        Tok::Pub => "pub",
        Tok::As => "as",
        Tok::Raise => "raise",
        Tok::Await => "await",
        Tok::SelfKw => "self",
        Tok::Crate => "crate",
        Tok::Super => "super",
        Tok::True => "true",
        Tok::False => "false",
        Tok::Dialogue => "dialogue",
        Tok::Quest => "quest",
        Tok::Choice => "choice",
        Tok::Once => "once",
        Tok::When => "when",
        Tok::Where => "where",
        Tok::Node => "node",
        Tok::Stage => "stage",
        Tok::Role => "role",
        Tok::Objective => "objective",
        Tok::Event => "event",
        Tok::Emit => "emit",
        Tok::Detour => "detour",
        Tok::Default => "default",
        Tok::Per => "per",
        Tok::AtKw => "at",
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::LBracket => "[",
        Tok::RBracket => "]",
        Tok::LBrace => "{",
        Tok::RBrace => "}",
        Tok::Comma => ",",
        Tok::Colon => ":",
        Tok::ColonColon => "::",
        Tok::Dot => ".",
        Tok::At => "@",
        Tok::Assign => "=",
        Tok::EqEq => "==",
        Tok::NotEq => "!=",
        Tok::Lt => "<",
        Tok::Le => "<=",
        Tok::Gt => ">",
        Tok::Ge => ">=",
        Tok::Plus => "+",
        Tok::Minus => "-",
        Tok::Star => "*",
        Tok::Slash => "/",
        Tok::Percent => "%",
        Tok::Amp => "&",
        Tok::Pipe => "|",
        Tok::Question => "?",
        Tok::Arrow => "->",
        Tok::FatArrow => "=>",
        Tok::Indent
        | Tok::Dedent
        | Tok::Newline
        | Tok::Eof
        | Tok::Ident(_)
        | Tok::Int(_)
        | Tok::Float(_)
        | Tok::Str(_)
        | Tok::FString(_) => "?",
    }
}
