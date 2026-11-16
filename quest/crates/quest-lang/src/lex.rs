//! The lexer: a `logos` token enum for everything that needs no context, a scanner that turns it into
//! positioned [`Token`]s (numbers and string escapes decoded, f-strings split), and the layout pass
//! ([`crate::indent`]) that adds `Indent` / `Dedent` / `Newline`.

use crate::diag::Diagnostic;
use crate::indent::Layout;
use logos::{Lexer, Logos};

/// A token kind. The contextual words (`Node` … `Priority`) are produced only by the layout pass, where the
/// grammar allows them; elsewhere those words stay `Ident`.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    // Layout.
    Indent,
    Dedent,
    Newline,
    Eof,
    // Keywords.
    Fn,
    Let,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Not,
    And,
    Or,
    Return,
    Break,
    Continue,
    Pass,
    Match,
    Case,
    Struct,
    Impl,
    Enum,
    Error,
    Mod,
    Use,
    Pub,
    As,
    Raise,
    Await,
    SelfKw,
    Crate,
    Super,
    True,
    False,
    Dialogue,
    Quest,
    Choice,
    Once,
    When,
    Where,
    // Contextual words (set by the layout pass).
    Node,
    Stage,
    Role,
    Objective,
    Event,
    Emit,
    Detour,
    Default,
    Per,
    /// The word `at` on an `objective` line (`At` is the `@` token).
    AtKw,
    Topic,
    Priority,
    // Punctuation.
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    ColonColon,
    Dot,
    At,
    Assign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Question,
    Arrow,
    FatArrow,
    // Data.
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    FString(Vec<FPart>),
}

/// A piece of an f-string: literal text (escapes and `{{` / `}}` decoded) or the tokens of a `{expr}`
/// part, positioned in the enclosing file.
#[derive(Debug, Clone, PartialEq)]
pub enum FPart {
    Text(String),
    Expr(Vec<Token>),
}

/// A token and where it is: 1-based line and column, `len` characters.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: Tok,
    pub line: u32,
    pub col: u32,
    pub len: u32,
}

/// How far a string-like literal got.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Lit {
    Closed,
    Open,
}

/// A scanned number.
#[derive(Debug, Clone, PartialEq)]
enum Num {
    Int(i64),
    Float(f64),
    Bad(&'static str),
}

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t]+")]
#[logos(skip(r"#[^\n]*", allow_greedy = true))]
enum Raw {
    #[regex(r"\r?\n")]
    Newline,
    #[token("fn", |_| Tok::Fn)]
    #[token("let", |_| Tok::Let)]
    #[token("if", |_| Tok::If)]
    #[token("elif", |_| Tok::Elif)]
    #[token("else", |_| Tok::Else)]
    #[token("while", |_| Tok::While)]
    #[token("for", |_| Tok::For)]
    #[token("in", |_| Tok::In)]
    #[token("not", |_| Tok::Not)]
    #[token("and", |_| Tok::And)]
    #[token("or", |_| Tok::Or)]
    #[token("return", |_| Tok::Return)]
    #[token("break", |_| Tok::Break)]
    #[token("continue", |_| Tok::Continue)]
    #[token("pass", |_| Tok::Pass)]
    #[token("match", |_| Tok::Match)]
    #[token("case", |_| Tok::Case)]
    #[token("struct", |_| Tok::Struct)]
    #[token("impl", |_| Tok::Impl)]
    #[token("enum", |_| Tok::Enum)]
    #[token("error", |_| Tok::Error)]
    #[token("mod", |_| Tok::Mod)]
    #[token("use", |_| Tok::Use)]
    #[token("pub", |_| Tok::Pub)]
    #[token("as", |_| Tok::As)]
    #[token("raise", |_| Tok::Raise)]
    #[token("await", |_| Tok::Await)]
    #[token("self", |_| Tok::SelfKw)]
    #[token("crate", |_| Tok::Crate)]
    #[token("super", |_| Tok::Super)]
    #[token("true", |_| Tok::True)]
    #[token("false", |_| Tok::False)]
    #[token("dialogue", |_| Tok::Dialogue)]
    #[token("choice", |_| Tok::Choice)]
    #[token("once", |_| Tok::Once)]
    #[token("when", |_| Tok::When)]
    #[token("where", |_| Tok::Where)]
    #[token("(", |_| Tok::LParen)]
    #[token(")", |_| Tok::RParen)]
    #[token("[", |_| Tok::LBracket)]
    #[token("]", |_| Tok::RBracket)]
    #[token("{", |_| Tok::LBrace)]
    #[token("}", |_| Tok::RBrace)]
    #[token(",", |_| Tok::Comma)]
    #[token(":", |_| Tok::Colon)]
    #[token("::", |_| Tok::ColonColon)]
    #[token(".", |_| Tok::Dot)]
    #[token("@", |_| Tok::At)]
    #[token("=", |_| Tok::Assign)]
    #[token("==", |_| Tok::EqEq)]
    #[token("!=", |_| Tok::NotEq)]
    #[token("<", |_| Tok::Lt)]
    #[token("<=", |_| Tok::Le)]
    #[token(">", |_| Tok::Gt)]
    #[token(">=", |_| Tok::Ge)]
    #[token("+", |_| Tok::Plus)]
    #[token("-", |_| Tok::Minus)]
    #[token("*", |_| Tok::Star)]
    #[token("/", |_| Tok::Slash)]
    #[token("%", |_| Tok::Percent)]
    #[token("&", |_| Tok::Amp)]
    #[token("|", |_| Tok::Pipe)]
    #[token("?", |_| Tok::Question)]
    #[token("->", |_| Tok::Arrow)]
    #[token("=>", |_| Tok::FatArrow)]
    Fixed(Tok),
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,
    #[regex(r"[0-9]", number)]
    Number(Num),
    #[token("\"", |lex| string_extent(lex, '"'))]
    #[token("'", |lex| string_extent(lex, '\''))]
    Str(Lit),
    #[token("f\"", |lex| string_extent(lex, '"'))]
    #[token("f'", |lex| string_extent(lex, '\''))]
    FStr(Lit),
}

/// Extends a string literal past its opening quote to its closing one. A string ends at the closing quote;
/// a newline or the end of the text before it leaves the literal open (the newline is not consumed).
fn string_extent(lex: &mut Lexer<Raw>, quote: char) -> Lit {
    let rest = lex.remainder();
    let mut chars = rest.char_indices();
    while let Some((at, c)) = chars.next() {
        if c == quote {
            lex.bump(at + c.len_utf8());
            return Lit::Closed;
        }
        if c == '\n' || (c == '\r' && rest[at + 1..].starts_with('\n')) {
            lex.bump(at);
            return Lit::Open;
        }
        if c == '\\' {
            match chars.next() {
                Some((_, '\n')) | None => {
                    lex.bump(at + 1);
                    return Lit::Open;
                }
                Some(_) => {}
            }
        }
    }
    lex.bump(rest.len());
    Lit::Open
}

/// Scans a number whose first digit was just matched.
fn number(lex: &mut Lexer<Raw>) -> Num {
    let (len, value) = scan_number(&lex.source()[lex.span().start..]);
    lex.bump(len - 1);
    value
}

fn scan_number(full: &str) -> (usize, Num) {
    let b = full.as_bytes();
    let mut i = 0;
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let hex = b.len() >= 2 && b[0] == b'0' && (b[1] == b'x' || b[1] == b'X');
    let mut bad: Option<&'static str> = None;
    let mut float = false;
    if hex {
        i = 2;
        while i < b.len() && (b[i].is_ascii_hexdigit() || b[i] == b'_') {
            i += 1;
        }
        if !b[2..i].iter().any(|c| c.is_ascii_hexdigit()) {
            bad = Some("a hexadecimal literal needs digits after `0x`");
        }
    } else {
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
        if i < b.len() && b[i] == b'.' {
            if b.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
                float = true;
                i += 1;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                    i += 1;
                }
            } else {
                i += 1;
                bad = Some("a number needs digits after the `.`");
            }
        }
        if bad.is_none() && i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            float = true;
            i += 1;
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                i += 1;
            }
            let digits = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
            if i == digits {
                bad = Some("an exponent needs digits");
            }
        }
    }
    if i < b.len() && is_word(b[i]) {
        while i < b.len() && is_word(b[i]) {
            i += 1;
        }
        bad.get_or_insert("a number cannot be followed by letters");
    }
    // `i` only ever stops on ASCII bytes, so this slice is on a char boundary.
    let text = &full[..i];
    if let Some(message) = bad {
        return (i, Num::Bad(message));
    }
    let clean: String = text.chars().filter(|&c| c != '_').collect();
    if text.ends_with('_') || text.contains("__") {
        return (i, Num::Bad("misplaced `_` in a number"));
    }
    let value = if hex {
        i64::from_str_radix(&clean[2..], 16)
            .map(Num::Int)
            .unwrap_or(Num::Bad("the number does not fit in an int (i64)"))
    } else if float {
        match clean.parse::<f64>() {
            Ok(f) if f.is_finite() => Num::Float(f),
            _ => Num::Bad("the number does not fit in a float"),
        }
    } else {
        clean
            .parse::<i64>()
            .map(Num::Int)
            .unwrap_or(Num::Bad("the number does not fit in an int (i64)"))
    };
    (i, value)
}

/// Converts increasing byte offsets of `text` to line and column, counting characters.
struct Cursor<'a> {
    text: &'a str,
    byte: usize,
    line: u32,
    col: u32,
}

impl Cursor<'_> {
    fn advance_to(&mut self, byte: usize) -> (u32, u32) {
        for c in self.text[self.byte..byte].chars() {
            if c == '\n' {
                self.line = self.line.saturating_add(1);
                self.col = 1;
            } else {
                self.col = self.col.saturating_add(1);
            }
        }
        self.byte = byte;
        (self.line, self.col)
    }
}

fn chars_u32(text: &str) -> u32 {
    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
}

/// What the scanner hands on: kind, line, column, length, and (for the first token of a line) whether the
/// indentation before it holds a tab.
type Sink<'a> = dyn FnMut(Tok, u32, u32, u32, bool) + 'a;

/// Scans `text`, which starts at `line`:`col` of `file`, into tokens. Returns the position just past it.
fn scan(
    file: &str,
    text: &str,
    line: u32,
    col: u32,
    diags: &mut Vec<Diagnostic>,
    sink: &mut Sink<'_>,
) -> (u32, u32) {
    let mut cur = Cursor {
        text,
        byte: 0,
        line,
        col,
    };
    let mut lexer = Raw::lexer(text);
    let mut line_start = 0;
    let mut fresh = true;
    while let Some(raw) = lexer.next() {
        let span = lexer.span();
        let (l, c) = cur.advance_to(span.start);
        let slice = &text[span.clone()];
        let kind = match raw {
            Err(()) => {
                let first = slice.chars().next().unwrap_or('?');
                if !first.is_ascii() && first.is_alphabetic() {
                    let tail = &text[span.start..];
                    let run = tail
                        .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                        .unwrap_or(tail.len());
                    lexer.bump(run.saturating_sub(span.len()));
                    let word = &tail[..run];
                    diags.push(Diagnostic::error(
                        "non-ascii-ident",
                        "identifiers are ASCII: letters, digits and `_`",
                        file,
                        l,
                        c,
                        chars_u32(word),
                    ));
                    Tok::Ident(word.to_string())
                } else {
                    diags.push(Diagnostic::error(
                        "bad-char",
                        format!("unexpected character `{}`", first.escape_debug()),
                        file,
                        l,
                        c,
                        1,
                    ));
                    continue;
                }
            }
            Ok(Raw::Newline) => {
                line_start = span.end;
                fresh = true;
                sink(Tok::Newline, l, c, 1, false);
                continue;
            }
            Ok(Raw::Fixed(tok)) => tok,
            Ok(Raw::Ident) => Tok::Ident(slice.to_string()),
            Ok(Raw::Number(num)) => match num {
                Num::Int(i) => Tok::Int(i),
                Num::Float(f) => Tok::Float(f),
                Num::Bad(message) => {
                    diags.push(Diagnostic::error(
                        "bad-number",
                        message,
                        file,
                        l,
                        c,
                        chars_u32(slice),
                    ));
                    Tok::Int(0)
                }
            },
            Ok(Raw::Str(lit)) => {
                let (body, closed) = literal_body(slice, 1, lit);
                if !closed {
                    unterminated(diags, file, l, c, slice);
                }
                Tok::Str(decode(file, body, l, c + 1, diags))
            }
            Ok(Raw::FStr(lit)) => {
                let (body, closed) = literal_body(slice, 2, lit);
                if !closed {
                    unterminated(diags, file, l, c, slice);
                }
                Tok::FString(fstring(file, body, l, c + 2, diags))
            }
        };
        let tab = fresh && text[line_start..span.start].contains('\t');
        fresh = false;
        sink(kind, l, c, chars_u32(slice), tab);
    }
    cur.advance_to(text.len())
}

fn unterminated(diags: &mut Vec<Diagnostic>, file: &str, l: u32, c: u32, slice: &str) {
    diags.push(Diagnostic::error(
        "unterminated-string",
        "this string is never closed",
        file,
        l,
        c,
        chars_u32(slice),
    ));
}

/// The text between a literal's quotes (`prefix` bytes of opening) and whether it was closed.
fn literal_body(slice: &str, prefix: usize, lit: Lit) -> (&str, bool) {
    let inner = &slice[prefix..];
    match lit {
        Lit::Closed => (&inner[..inner.len() - 1], true),
        Lit::Open => (inner, false),
    }
}

/// Decodes the escapes of a plain string body whose first character is at `line`:`col`.
fn decode(file: &str, body: &str, line: u32, col: u32, diags: &mut Vec<Diagnostic>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    let mut at = col;
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            at = at.saturating_add(1);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(e @ ('\\' | '"' | '\'')) => out.push(e),
            Some(e @ ('{' | '}')) => out.push(e),
            Some(other) => diags.push(Diagnostic::error(
                "bad-escape",
                format!("unknown escape `\\{}`", other.escape_debug()),
                file,
                line,
                at,
                2,
            )),
            None => {}
        }
        at = at.saturating_add(2);
    }
    out
}

/// Splits an f-string body (first character at `line`:`col`) into text and expression parts.
fn fstring(file: &str, body: &str, line: u32, col: u32, diags: &mut Vec<Diagnostic>) -> Vec<FPart> {
    let chars: Vec<(usize, char)> = body.char_indices().collect();
    let at = |k: usize| col.saturating_add(u32::try_from(k).unwrap_or(u32::MAX));
    let byte = |k: usize| chars.get(k).map_or(body.len(), |&(b, _)| b);
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k].1;
        let next = chars.get(k + 1).map(|&(_, n)| n);
        match c {
            '\\' => {
                // Decode one escape through the plain-string path.
                let end = (k + 2).min(chars.len());
                text.push_str(&decode(file, &body[byte(k)..byte(end)], line, at(k), diags));
                k = end;
            }
            '{' if next == Some('{') => {
                text.push('{');
                k += 2;
            }
            '}' if next == Some('}') => {
                text.push('}');
                k += 2;
            }
            '}' => {
                diags.push(Diagnostic::error(
                    "syntax",
                    "a single `}` in an f-string; write `}}` for a brace",
                    file,
                    line,
                    at(k),
                    1,
                ));
                k += 1;
            }
            '{' => {
                // The expression runs to the matching `}`; quotes of the other kind hide braces.
                let start = k + 1;
                let mut depth = 1u32;
                let mut j = start;
                while j < chars.len() {
                    match chars[j].1 {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        q @ ('\'' | '"') => {
                            if let Some(close) = chars[j + 1..].iter().position(|&(_, n)| n == q) {
                                j += close + 1;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if depth != 0 {
                    diags.push(Diagnostic::error(
                        "syntax",
                        "this `{` in an f-string is never closed",
                        file,
                        line,
                        at(k),
                        1,
                    ));
                    break;
                }
                if !text.is_empty() {
                    parts.push(FPart::Text(std::mem::take(&mut text)));
                }
                let inner = &body[byte(start)..byte(j)];
                if inner.trim().is_empty() {
                    diags.push(Diagnostic::error(
                        "syntax",
                        "an f-string `{}` needs an expression",
                        file,
                        line,
                        at(k),
                        2,
                    ));
                } else {
                    let mut tokens = Vec::new();
                    scan(
                        file,
                        inner,
                        line,
                        at(start),
                        diags,
                        &mut |kind, l, c, len, _| {
                            tokens.push(Token {
                                kind,
                                line: l,
                                col: c,
                                len,
                            });
                        },
                    );
                    parts.push(FPart::Expr(tokens));
                }
                k = j + 1;
            }
            _ => {
                text.push(c);
                k += 1;
            }
        }
    }
    if !text.is_empty() {
        parts.push(FPart::Text(text));
    }
    parts
}

/// Lexes one source file into tokens and diagnostics. Lines and columns are 1-based, columns count
/// characters. The token list always ends with `Eof`.
pub fn lex(file: &str, text: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut diags = Vec::new();
    let mut layout = Layout::new(file);
    let (line, col) = scan(file, text, 1, 1, &mut diags, &mut |kind, l, c, len, tab| {
        layout.push(kind, l, c, len, tab);
    });
    let (tokens, layout_diags) = layout.finish(line, col);
    diags.extend(layout_diags);
    diags.sort_by_key(|d| (d.line, d.col));
    (tokens, diags)
}
