//! The layout pass: line starts become `Indent` / `Dedent` / `Newline`, newlines inside brackets and on
//! blank lines vanish, and the contextual words of the dialogue and quest syntax are recognised.

use crate::diag::Diagnostic;
use crate::lex::{Tok, Token};

pub struct Layout<'a> {
    file: &'a str,
    out: Vec<Token>,
    /// Indentation widths of the open blocks; the bottom is always 0.
    stack: Vec<u32>,
    /// Open brackets.
    depth: u32,
    /// No token yet on this physical line.
    line_start: bool,
    diags: Vec<Diagnostic>,
}

impl<'a> Layout<'a> {
    pub fn new(file: &'a str) -> Self {
        Layout {
            file,
            out: Vec::new(),
            stack: vec![0],
            depth: 0,
            line_start: true,
            diags: Vec::new(),
        }
    }

    fn emit(&mut self, kind: Tok, line: u32, col: u32, len: u32) {
        self.out.push(Token {
            kind,
            line,
            col,
            len,
        });
    }

    /// Feeds the next raw token. `tab` says whether the indentation before it holds a tab (only read
    /// for the first token of a line).
    pub fn push(&mut self, kind: Tok, line: u32, col: u32, len: u32, tab: bool) {
        if kind == Tok::Newline {
            if self.depth == 0 && !self.line_start {
                self.emit(Tok::Newline, line, col, len);
                self.line_start = true;
            }
            return;
        }
        if self.line_start && self.depth == 0 {
            self.indentation(line, col, tab);
        }
        self.line_start = false;
        match kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace => self.depth = self.depth.saturating_add(1),
            Tok::RParen | Tok::RBracket | Tok::RBrace => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        self.emit(kind, line, col, len);
    }

    fn indentation(&mut self, line: u32, col: u32, tab: bool) {
        let width = col - 1;
        if tab {
            self.diags.push(Diagnostic::error(
                "tab-indent",
                "indent with spaces, not tabs",
                self.file,
                line,
                1,
                width,
            ));
        }
        let top = *self.stack.last().unwrap_or(&0);
        if width > top {
            self.stack.push(width);
            self.emit(Tok::Indent, line, col, 0);
            return;
        }
        while self.stack.last().is_some_and(|&w| w > width) {
            self.stack.pop();
            self.emit(Tok::Dedent, line, col, 0);
        }
        if self.stack.last() != Some(&width) {
            self.diags.push(Diagnostic::error(
                "bad-dedent",
                "this line does not line up with any enclosing block",
                self.file,
                line,
                1,
                width,
            ));
        }
    }

    /// Ends the file at `line`:`col`: closes the last line and every open block.
    pub fn finish(mut self, line: u32, col: u32) -> (Vec<Token>, Vec<Diagnostic>) {
        if !self.line_start {
            self.emit(Tok::Newline, line, col, 0);
        }
        while self.stack.len() > 1 {
            self.stack.pop();
            self.emit(Tok::Dedent, line, col, 0);
        }
        contextual_words(&mut self.out);
        self.emit(Tok::Eof, line, col, 0);
        (self.out, self.diags)
    }
}

fn is_ident(t: Option<&Token>, word: &str) -> bool {
    matches!(t, Some(Token { kind: Tok::Ident(s), .. }) if s == word)
}

fn kind_at(out: &[Token], i: usize) -> Option<&Tok> {
    out.get(i).map(|t| &t.kind)
}

/// The index of the token that ends the logical line `out[from..]` is on (a `Newline`, `Indent` or
/// `Dedent`), or `out.len()`.
fn line_end(out: &[Token], from: usize) -> usize {
    out.get(from..)
        .and_then(|rest| {
            rest.iter()
                .position(|t| matches!(t.kind, Tok::Newline | Tok::Indent | Tok::Dedent))
        })
        .map_or(out.len(), |p| from + p)
}

/// The index after the bracket group that opens at `open` (a `(`) and before `end`, or `None` when no
/// `(` stands there or it is not closed on the line.
fn after_group(out: &[Token], open: usize, end: usize) -> Option<usize> {
    if kind_at(out, open) != Some(&Tok::LParen) {
        return None;
    }
    let mut depth = 0u32;
    for (j, t) in out.get(open..end)?.iter().enumerate() {
        match t.kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth = depth.saturating_add(1),
            Tok::RParen | Tok::RBracket | Tok::RBrace => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open + j + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `t` can begin the expression after `requires` / `pause` without also continuing a name (so
/// `pause(x)`, `pause = 1`, `requires.x` and `pause - 1` stay ordinary code).
fn begins_operand(t: Option<&Tok>) -> bool {
    matches!(
        t,
        Some(
            Tok::Ident(_)
                | Tok::Int(_)
                | Tok::Float(_)
                | Tok::Str(_)
                | Tok::FString(_)
                | Tok::True
                | Tok::False
                | Tok::Not
                | Tok::Await
                | Tok::SelfKw
                | Tok::Crate
                | Tok::Super
        )
    )
}

/// Whether `t` can end an expression (what may stand before a trailing `uninterruptible`).
fn ends_operand(t: Option<&Tok>) -> bool {
    matches!(
        t,
        Some(
            Tok::Ident(_)
                | Tok::Int(_)
                | Tok::Float(_)
                | Tok::Str(_)
                | Tok::FString(_)
                | Tok::True
                | Tok::False
                | Tok::SelfKw
                | Tok::RParen
                | Tok::RBracket
                | Tok::RBrace
                | Tok::Question
        )
    )
}

/// Turns the contextual words into their own tokens where the grammar allows them (spec 5.13).
fn contextual_words(out: &mut [Token]) {
    for i in 0..out.len() {
        if out[i].kind == Tok::Once && is_ident(out.get(i + 1), "per") {
            out[i + 1].kind = Tok::Per;
        }
        let at_line_start =
            i == 0 || matches!(out[i - 1].kind, Tok::Newline | Tok::Indent | Tok::Dedent);
        if !at_line_start {
            continue;
        }
        if out[i].kind == Tok::Pub {
            // `pub` may stand before `dialogue`, `scene`, `quest` and `repeatable quest`.
            headers(out, i + 1);
        } else {
            headers(out, i);
            line_word(out, i);
        }
    }
}

/// The headers that may follow `pub`, at token `h`: `dialogue`, `scene`, `[repeatable] quest`.
fn headers(out: &mut [Token], h: usize) {
    match kind_at(out, h) {
        Some(Tok::Dialogue) => dialogue_header(out, h),
        Some(Tok::Ident(w))
            if w == "scene" && matches!(kind_at(out, h + 1), Some(Tok::Ident(_))) =>
        {
            out[h].kind = Tok::Scene;
            scene_header(out, h);
        }
        Some(Tok::Ident(w))
            if w == "repeatable"
                && is_ident(out.get(h + 1), "quest")
                && matches!(kind_at(out, h + 2), Some(Tok::Ident(_))) =>
        {
            out[h].kind = Tok::Repeatable;
            quest_header(out, h + 1);
        }
        Some(Tok::Ident(w))
            if w == "quest" && matches!(kind_at(out, h + 1), Some(Tok::Ident(_))) =>
        {
            quest_header(out, h);
        }
        _ => {}
    }
}

/// The words that begin a line of their own, at token `i` (first on its line, no `pub`).
fn line_word(out: &mut [Token], i: usize) {
    let Some(Tok::Ident(word)) = kind_at(out, i) else {
        return;
    };
    let next = kind_at(out, i + 1);
    let operand = matches!(next, Some(Tok::Ident(_) | Tok::Str(_) | Tok::FString(_)));
    let kind = match word.as_str() {
        "node" if operand => Tok::Node,
        "stage" if operand => Tok::Stage,
        "role" if operand => Tok::Role,
        "objective" if operand => Tok::Objective,
        "event" if operand => Tok::Event,
        "emit" if operand => Tok::Emit,
        "detour" if operand => Tok::Detour,
        "part" if operand => Tok::Part,
        "requires" if begins_operand(next) => Tok::Requires,
        "pause" if begins_operand(next) => Tok::Pause,
        "optional"
            if (is_ident(out.get(i + 1), "role") || is_ident(out.get(i + 1), "objective"))
                && matches!(
                    kind_at(out, i + 2),
                    Some(Tok::Ident(_) | Tok::Str(_) | Tok::FString(_))
                ) =>
        {
            Tok::Optional
        }
        "default"
            if matches!(next, Some(Tok::Str(_) | Tok::FString(_)))
                || (matches!(next, Some(Tok::Colon))
                    && matches!(kind_at(out, i + 2), Some(Tok::Newline))) =>
        {
            Tok::Default
        }
        _ => return,
    };
    out[i].kind = kind;
    match out[i].kind {
        Tok::Optional => {
            let role = is_ident(out.get(i + 1), "role");
            out[i + 1].kind = if role { Tok::Role } else { Tok::Objective };
            if !role {
                objective_line(out, i + 1);
            }
        }
        Tok::Objective => objective_line(out, i),
        Tok::Part => {
            // `part name on role` / `part name at role`.
            if is_ident(out.get(i + 2), "on") {
                out[i + 2].kind = Tok::On;
            } else if is_ident(out.get(i + 2), "at") {
                out[i + 2].kind = Tok::AtKw;
            }
        }
        _ => {}
    }
}

/// On a line begun by `objective` (token `o`): the first `at` and the first `count` outside brackets
/// that are not a field or path segment (`@q.at` is a line id) become their own tokens.
fn objective_line(out: &mut [Token], o: usize) {
    let end = line_end(out, o + 1);
    let (mut at, mut count) = (false, false);
    let mut depth = 0u32;
    for j in o + 1..end {
        match out[j].kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth = depth.saturating_add(1),
            Tok::RParen | Tok::RBracket | Tok::RBrace => depth = depth.saturating_sub(1),
            _ => {}
        }
        let segment = matches!(
            kind_at(out, j - 1),
            Some(Tok::Dot | Tok::ColonColon | Tok::At)
        );
        if depth > 0 || segment {
            continue;
        }
        if !at && is_ident(out.get(j), "at") {
            out[j].kind = Tok::AtKw;
            at = true;
        } else if !count && is_ident(out.get(j), "count") {
            out[j].kind = Tok::Count;
            count = true;
        }
    }
}

/// `quest Name[(options)] [at role]:` (token `q` is the word `quest`).
fn quest_header(out: &mut [Token], q: usize) {
    out[q].kind = Tok::Quest;
    let end = line_end(out, q);
    let after = after_group(out, q + 2, end).unwrap_or(q + 2);
    if is_ident(out.get(after), "at") {
        out[after].kind = Tok::AtKw;
    }
}

/// `scene name(cast) [where pred] [uninterruptible]:` (token `s` is `scene`): `uninterruptible` right
/// before the header's closing `:`.
fn scene_header(out: &mut [Token], s: usize) {
    let end = line_end(out, s);
    let Some(after) = after_group(out, s + 2, end) else {
        return;
    };
    if end >= 2
        && end - 2 >= after
        && kind_at(out, end - 1) == Some(&Tok::Colon)
        && is_ident(out.get(end - 2), "uninterruptible")
        && (end - 2 == after || ends_operand(kind_at(out, end - 3)))
    {
        out[end - 2].kind = Tok::Uninterruptible;
    }
}

/// On a line begun by `dialogue` (token `d`), after the parameter list: `topic` before a string or an
/// f-string, `priority` before a number and `exits` before a name become their own tokens (spec 5.13).
fn dialogue_header(out: &mut [Token], d: usize) {
    let end = line_end(out, d);
    let Some(params) = (d..end).find(|&j| out[j].kind == Tok::LParen) else {
        return;
    };
    let after = after_group(out, params, end).unwrap_or(end);
    for j in after..end {
        let next = kind_at(out, j + 1);
        if is_ident(out.get(j), "topic") && matches!(next, Some(Tok::Str(_) | Tok::FString(_))) {
            out[j].kind = Tok::Topic;
        } else if is_ident(out.get(j), "priority") && matches!(next, Some(Tok::Int(_))) {
            out[j].kind = Tok::Priority;
        } else if is_ident(out.get(j), "exits") && matches!(next, Some(Tok::Ident(_))) {
            out[j].kind = Tok::Exits;
        }
    }
}
