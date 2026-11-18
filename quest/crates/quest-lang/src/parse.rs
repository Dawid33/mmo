//! The parser: the LALRPOP grammar (`grammar.lalrpop`, generated into `OUT_DIR` by `build.rs`) run over
//! the lexer's tokens, the helpers its actions call, and the mapping of its errors to diagnostics.
//!
//! The grammar's location type is the token's index in the slice it parses; diagnostics map it back to
//! the token's line, column and length.

use crate::ast::{
    Arg, BinOp, Cond, Decorator, DialogueEntry, Expr, ExprKind, FStrPart, File, Item, ItemKind,
    Line, LineId, PatLit, Path, Pattern, PatternKind, Span, Step, StepKind, Stmt, StmtKind, Type,
    TypeKind, UnOp,
};
use crate::diag::Diagnostic;
use crate::lex::{FPart, Tok, Token, lex};
use lalrpop_util::{ErrorRecovery, ParseError};
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
}

pub(crate) type PErr = ParseError<usize, Tok, ParseFail>;

/// A syntax error the grammar recovered from with its error token (`!`).
pub(crate) type Recovery = ErrorRecovery<usize, Tok, ParseFail>;

/// Where the grammar recovered from an error: what the message may say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctx {
    /// The top level of a file, where only items and `let` stand.
    Top,
    Stmt,
    /// A dialogue body or step block, which holds steps, not statements.
    Step,
    /// Any other block: fields, variants, cases, options, members.
    Other,
}

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

/// The position (line, column, length) of token `i`, or of the end of the input after the last token.
fn token_at(toks: &[Token], i: usize) -> (u32, u32, u32) {
    match toks.get(i) {
        Some(t) => (t.line, t.col, t.len),
        None => toks.last().map_or((1, 1, 0), |t| (t.line, t.col, 0)),
    }
}

/// Records a diagnostic the parse goes on after, at most one per line: a second error on the line of the
/// last one is a consequence of the first.
fn record(errs: &mut Vec<Diagnostic>, d: Diagnostic) {
    if errs
        .last()
        .is_some_and(|last| last.line == d.line && last.file == d.file)
    {
        return;
    }
    errs.push(d);
}

/// A comparison operator after a comparison (token `at`): `chained-comparison`, recorded; the parse goes
/// on with the comparison so far.
pub(crate) fn chained(
    errs: &mut Vec<Diagnostic>,
    toks: &[Token],
    file: &str,
    at: usize,
    e: Expr,
) -> Expr {
    let (line, col, len) = token_at(toks, at);
    record(
        errs,
        Diagnostic::error(
            "chained-comparison",
            "comparisons do not chain: write `a < b and b < c`",
            file,
            line,
            col,
            len,
        ),
    );
    e
}

/// The `raises` clause of a signature: `word` (token `at`) must be the word `raises`.
pub(crate) fn raises(
    errs: &mut Vec<Diagnostic>,
    toks: &[Token],
    file: &str,
    at: usize,
    word: &str,
    set: Path,
) -> Path {
    if word != "raises" {
        let (line, col, len) = token_at(toks, at);
        record(
            errs,
            Diagnostic::error(
                "syntax",
                format!("expected `raises` or `:`, found the name `{word}`"),
                file,
                line,
                col,
                len,
            ),
        );
    }
    set
}

/// The `ticks=n` of a line (token `at` is the word): `word` must be `ticks`.
pub(crate) fn ticks(
    errs: &mut Vec<Diagnostic>,
    toks: &[Token],
    file: &str,
    at: usize,
    word: &str,
    e: Expr,
) -> Expr {
    if word != "ticks" {
        let (line, col, len) = token_at(toks, at);
        record(
            errs,
            Diagnostic::error(
                "syntax",
                format!("expected `ticks=…`, found the name `{word}`"),
                file,
                line,
                col,
                len,
            ),
        );
    }
    e
}

/// The callee of the statement `emit Ev(…)`: a name no script can write, which resolution takes for the
/// host's `emit` whatever a script names `emit`.
pub const EMIT_CALLEE: &str = "$emit";

/// `emit Event(args)` as a statement (the `emit` at token `l`, the `(` at `at`, the `)` before `r`):
/// the call `$emit(Event(args))` of the host's `emit` ([`EMIT_CALLEE`]).
pub(crate) fn emit_stmt(
    toks: &[Token],
    l: usize,
    at: usize,
    r: usize,
    event: Path,
    args: Vec<Arg>,
) -> Result<StmtKind, PErr> {
    let start = l.saturating_add(1);
    let callee = expr(
        toks,
        l,
        start,
        ExprKind::Path(vec![EMIT_CALLEE.to_string()]),
    )?;
    let event = expr(toks, start, at, ExprKind::Path(event))?;
    let value = expr(toks, start, r, ExprKind::Call(Box::new(event), args))?;
    let call = expr(
        toks,
        l,
        r,
        ExprKind::Call(Box::new(callee), vec![Arg { name: None, value }]),
    )?;
    Ok(StmtKind::Expr(call))
}

/// Builds a statement over the tokens `l..r`, refusing one deeper than `PARSE_DEPTH_MAX`.
pub(crate) fn stmt(toks: &[Token], l: usize, r: usize, kind: StmtKind) -> Result<Stmt, PErr> {
    let depth = check_depth(kind.child_depth().saturating_add(1), l)?;
    Ok(Stmt {
        kind,
        span: span(toks, l, r),
        depth,
    })
}

/// Builds a dialogue step over the tokens `l..r`, refusing one deeper than `PARSE_DEPTH_MAX`.
pub(crate) fn step(toks: &[Token], l: usize, r: usize, kind: StepKind) -> Result<Step, PErr> {
    let depth = check_depth(kind.child_depth().saturating_add(1), l)?;
    Ok(Step {
        kind,
        span: span(toks, l, r),
        depth,
    })
}

/// Builds an item over the tokens `l..r` (from its keyword), refusing one deeper than `PARSE_DEPTH_MAX`.
pub(crate) fn item(
    toks: &[Token],
    l: usize,
    r: usize,
    decorators: Vec<Decorator>,
    public: bool,
    kind: ItemKind,
) -> Result<Item, PErr> {
    let child = decorators
        .iter()
        .flat_map(|d| d.args.iter().map(|a| a.value.depth))
        .fold(kind.child_depth(), u32::max);
    let depth = check_depth(child.saturating_add(1), l)?;
    Ok(Item {
        kind,
        public,
        decorators,
        span: span(toks, l, r),
        depth,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn line(
    toks: &[Token],
    l: usize,
    r: usize,
    speaker: Option<String>,
    text: Expr,
    id: Option<LineId>,
    ticks: Option<Expr>,
    cond: Option<Cond>,
) -> Line {
    Line {
        speaker,
        text,
        id,
        ticks,
        cond,
        span: span(toks, l, r),
    }
}

/// Joins consecutive line-group members (each parsed as a one-member group) into one group.
pub(crate) fn group_steps(steps: Vec<Step>) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::with_capacity(steps.len());
    for s in steps {
        join_or_push(&mut out, s, step_itself);
    }
    out
}

/// [`group_steps`] for the entries of a dialogue body.
pub(crate) fn group_entries(entries: Vec<DialogueEntry>) -> Vec<DialogueEntry> {
    let mut out: Vec<DialogueEntry> = Vec::with_capacity(entries.len());
    for e in entries {
        join_or_push(&mut out, e, entry_step);
    }
    out
}

fn step_itself(s: &mut Step) -> Option<&mut Step> {
    Some(s)
}

fn entry_step(e: &mut DialogueEntry) -> Option<&mut Step> {
    match e {
        DialogueEntry::Step(s) => Some(s),
        DialogueEntry::Node(_) => None,
    }
}

/// Pushes `x`, or adds its members to the previous entry when both are line groups. A joined group is as
/// deep as the deeper of the two (each is 1 + its deepest member).
fn join_or_push<T>(out: &mut Vec<T>, mut x: T, step_of: fn(&mut T) -> Option<&mut Step>) {
    if let (Some(prev), Some(next)) = (out.last_mut().and_then(step_of), step_of(&mut x))
        && let (StepKind::Group(members), StepKind::Group(more)) = (&mut prev.kind, &mut next.kind)
    {
        members.append(more);
        prev.depth = prev.depth.max(next.depth);
        return;
    }
    out.push(x);
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

/// Parses the `{expr}` parts of an f-string token (token `at`). A part that fails to parse records its
/// diagnostics and stands as an [`ExprKind::Error`]; the parse goes on.
pub(crate) fn fstring(
    errs: &mut Vec<Diagnostic>,
    toks: &[Token],
    file: &str,
    at: usize,
    parts: Vec<FPart>,
) -> ExprKind {
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
                        for d in diags {
                            record(errs, d);
                        }
                        out.push(FStrPart::Expr(Expr {
                            kind: ExprKind::Error,
                            span: span(toks, at, at.saturating_add(1)),
                            depth: 1,
                        }));
                    }
                }
            }
        }
    }
    ExprKind::FStr(out)
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

/// The outcome of one entry parse: the value when nothing was reported, else every diagnostic (those
/// recorded along the way, then the error that stopped it).
fn finish<T>(
    file: &str,
    toks: &[Token],
    end: End,
    errs: Vec<Diagnostic>,
    result: Result<T, PErr>,
) -> Result<T, Vec<Diagnostic>> {
    match result {
        Ok(v) if errs.is_empty() => Ok(v),
        Ok(_) => Err(errs),
        Err(e) => {
            let mut errs = errs;
            errs.extend(diagnose(file, toks, end, e));
            Err(errs)
        }
    }
}

/// Parses `toks` (a whole expression; `end` is where the input ends, for an error there).
pub(crate) fn expr_tokens(file: &str, toks: &[Token], end: End) -> Result<Expr, Vec<Diagnostic>> {
    let mut errs = Vec::new();
    let result = grammar::ExprEntryParser::new().parse(toks, file, &mut errs, triples(toks));
    finish(file, toks, end, errs, result)
}

pub(crate) fn type_tokens(file: &str, toks: &[Token], end: End) -> Result<Type, Vec<Diagnostic>> {
    let mut errs = Vec::new();
    let result = grammar::TypeEntryParser::new().parse(toks, file, &mut errs, triples(toks));
    finish(file, toks, end, errs, result)
}

pub(crate) fn pattern_tokens(
    file: &str,
    toks: &[Token],
    end: End,
) -> Result<Pattern, Vec<Diagnostic>> {
    let mut errs = Vec::new();
    let result = grammar::PatternEntryParser::new().parse(toks, file, &mut errs, triples(toks));
    finish(file, toks, end, errs, result)
}

/// Lexes and parses one source file. The parser recovers from a syntax error at the next line of the
/// same block (plan D11), so the diagnostics may list several errors; the file holds every item it could
/// parse. An error it cannot recover from (`too-deep`, or one with no line end after it) ends the parse
/// with an empty file.
pub fn parse_file(file: &str, text: &str) -> (File, Vec<Diagnostic>) {
    let (toks, mut diags) = lex(file, text);
    let end = toks.last().map_or((1, 1), |t| (t.line, t.col));
    let mut errs = Vec::new();
    let result = grammar::FileEntryParser::new().parse(&toks, file, &mut errs, triples(&toks));
    diags.append(&mut errs);
    match result {
        Ok(parsed) => (parsed, diags),
        Err(e) => {
            diags.extend(diagnose(file, &toks, end, e));
            (File::default(), diags)
        }
    }
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

/// Records a syntax error the grammar recovered from (its `!` token) in context `ctx`.
pub(crate) fn recovered(
    errs: &mut Vec<Diagnostic>,
    toks: &[Token],
    file: &str,
    rec: Recovery,
    ctx: Ctx,
) {
    if let ParseError::UnrecognizedToken {
        token: (i, tok, _),
        expected,
    } = &rec.error
    {
        let special = match ctx {
            Ctx::Top if expects(expected, "fn") && expects(expected, "struct") => Some((
                "syntax",
                *i,
                format!(
                    "only items (`fn`, `struct`, `impl`, `enum`, `error`, `event`, `mod`, `use`, \
                     `dialogue`, `scene`, `quest`) and `let` may stand at the top level; found {}",
                    describe(tok)
                ),
            )),
            Ctx::Step => statement_at(toks, *i).map(|(start, what)| {
                (
                    "not-a-step",
                    start,
                    format!(
                        "a dialogue holds steps, not statements: found {what}; a step is a line \
                         (`npc: \"…\"`), `choice:`, `=> line`, `when cond:`, `once:`, `-> node`, \
                         `detour name`, `emit Event(…)`, `pause n` or `return`"
                    ),
                )
            }),
            Ctx::Top | Ctx::Stmt | Ctx::Other => None,
        };
        if let Some((code, at, message)) = special {
            let (line, col, len) = token_at(toks, at);
            record(errs, Diagnostic::error(code, message, file, line, col, len));
            return;
        }
    }
    let end = toks.last().map_or((1, 1), |t| (t.line, t.col));
    for d in diagnose(file, toks, end, rec.error) {
        record(errs, d);
    }
}

/// Whether LALRPOP's expected list holds the terminal `word` (it quotes fixed terminals).
fn expects(expected: &[String], word: &str) -> bool {
    expected
        .iter()
        .any(|e| e.strip_prefix('"').and_then(|e| e.strip_suffix('"')) == Some(word))
}

/// Whether token `i`, the one a step block could not take, begins a statement there: `let`, `if`,
/// `while`, `for` or `match` first on its line, or `(` or `=` after a name that is. The statement's first
/// token and how a message names it.
fn statement_at(toks: &[Token], i: usize) -> Option<(usize, String)> {
    let first_on_line = |j: usize| {
        j == 0
            || j.checked_sub(1)
                .and_then(|p| toks.get(p))
                .is_some_and(|t| matches!(t.kind, Tok::Newline | Tok::Indent | Tok::Dedent))
    };
    let tok = &toks.get(i)?.kind;
    match tok {
        Tok::Let | Tok::If | Tok::While | Tok::For | Tok::Match if first_on_line(i) => {
            Some((i, format!("`{}`", spelling(tok))))
        }
        Tok::LParen | Tok::Assign => {
            let name = i.checked_sub(1)?;
            if !matches!(toks.get(name)?.kind, Tok::Ident(_)) || !first_on_line(name) {
                return None;
            }
            let what = if *tok == Tok::LParen {
                "a call"
            } else {
                "an assignment"
            };
            Some((name, what.to_string()))
        }
        _ => None,
    }
}

/// The diagnostics for a parse error. `end` is where the input ends.
fn diagnose(file: &str, toks: &[Token], end: End, err: PErr) -> Vec<Diagnostic> {
    let at = |i: usize| {
        toks.get(i)
            .map_or((end.0, end.1, 0), |t| (t.line, t.col, t.len))
    };
    let (code, message, (line, col, len)) = match err {
        ParseError::User {
            error: ParseFail::TooDeep(i),
        } => (
            "too-deep",
            format!("this is nested more than {PARSE_DEPTH_MAX} levels deep"),
            at(i),
        ),
        ParseError::UnrecognizedToken {
            token: (i, tok, _),
            expected,
        } => {
            // An operator that cannot follow here must not read as "expected an operator".
            let stray_operator = fixed_spelling(&tok)
                .is_some_and(|op| OPERATORS.contains(&op) && !expects(&expected, op));
            // Only a string or an f-string: a line's (or a topic's, an option's, an objective's) text, which
            // is translated by its id and so is a literal (spec §5.11).
            let text_only =
                !expected.is_empty() && expected.iter().all(|e| e == "STRING" || e == "FSTRING");
            if text_only {
                return vec![Diagnostic::error(
                    "bad-line-text",
                    format!(
                        "a line's text is a string or an f-string literal (it is translated by its id), \
                         not {}",
                        describe(&tok)
                    ),
                    file,
                    at(i).0,
                    at(i).1,
                    at(i).2,
                )];
            }
            let message = if stray_operator {
                format!(
                    "{} cannot follow here: expected {}",
                    describe(&tok),
                    expected_text(&expected, "another operator")
                )
            } else {
                format!(
                    "expected {}, found {}",
                    expected_text(&expected, "an operator"),
                    describe(&tok)
                )
            };
            ("syntax", message, at(i))
        }
        ParseError::UnrecognizedEof { expected, .. } => (
            "syntax",
            format!(
                "expected {}, found the end of the input",
                expected_text(&expected, "an operator")
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
    "not", "await", "self", "crate", "super", "dialogue", "quest", "choice", "once", "when",
    "where",
];

/// The terminals that start a statement other than an expression; when the parser expects them all it
/// expects a statement, and the message says so once.
const STMT_START: &[&str] = &[
    "let", "if", "while", "for", "match", "return", "break", "continue", "pass", "raise", "->",
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

/// "`)`, `,` or an operator" for LALRPOP's list of expected terminals (`"\")\""`, `"NAME"`, …);
/// `operators` names the group of expected operators.
fn expected_text(expected: &[String], operators: &'static str) -> String {
    let names: Vec<&str> = expected
        .iter()
        .map(|e| {
            e.strip_prefix('"')
                .and_then(|e| e.strip_suffix('"'))
                .unwrap_or(e)
        })
        .collect();
    let wants_expr = names.contains(&"await");
    let wants_stmt = wants_expr && STMT_START.iter().all(|t| names.contains(t));
    let wants_operator = !wants_expr && names.contains(&"+");
    // `self`, `crate` and `super` start a path as a name does.
    let wants_name = names.contains(&"NAME");
    let mut out: Vec<String> = Vec::new();
    let mut groups: Vec<&str> = Vec::new();
    for name in names {
        if wants_name && matches!(name, "self" | "crate" | "super") {
            continue;
        }
        if wants_stmt && (STMT_START.contains(&name) || EXPR_START.contains(&name)) {
            if !groups.contains(&"a statement") {
                groups.push("a statement");
            }
            continue;
        }
        if wants_expr && EXPR_START.contains(&name) {
            if !groups.contains(&"an expression") {
                groups.push("an expression");
            }
            continue;
        }
        if wants_operator && OPERATORS.contains(&name) {
            if !groups.contains(&operators) {
                groups.push(operators);
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

/// The source spelling of a token without data; `None` for a layout or data token.
fn fixed_spelling(tok: &Tok) -> Option<&'static str> {
    match tok {
        Tok::Indent
        | Tok::Dedent
        | Tok::Newline
        | Tok::Eof
        | Tok::Ident(_)
        | Tok::Int(_)
        | Tok::Float(_)
        | Tok::Str(_)
        | Tok::FString(_) => None,
        fixed => Some(spelling(fixed)),
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
        Tok::Topic => "topic",
        Tok::Priority => "priority",
        Tok::Part => "part",
        Tok::Scene => "scene",
        Tok::Requires => "requires",
        Tok::Pause => "pause",
        Tok::Optional => "optional",
        Tok::Repeatable => "repeatable",
        Tok::Exits => "exits",
        Tok::Count => "count",
        Tok::Uninterruptible => "uninterruptible",
        Tok::On => "on",
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
