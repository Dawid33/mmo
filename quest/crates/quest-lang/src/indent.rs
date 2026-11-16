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

/// Turns the contextual words into their own tokens where the grammar allows them (spec 5.13).
fn contextual_words(out: &mut [Token]) {
    for i in 0..out.len() {
        let at_line_start =
            i == 0 || matches!(out[i - 1].kind, Tok::Newline | Tok::Indent | Tok::Dedent);
        if out[i].kind == Tok::Once && is_ident(out.get(i + 1), "per") {
            out[i + 1].kind = Tok::Per;
        }
        // A dialogue header: `dialogue …` or `pub dialogue …` first on its line.
        if at_line_start
            && (out[i].kind == Tok::Dialogue
                || (out[i].kind == Tok::Pub
                    && out.get(i + 1).is_some_and(|t| t.kind == Tok::Dialogue)))
        {
            dialogue_header(&mut out[i..]);
        }
        // A quest header: `quest Name:` or `pub quest Name:`; anywhere else `quest` is a plain name.
        let header_at = if at_line_start && out[i].kind == Tok::Pub {
            i + 1
        } else {
            i
        };
        if (at_line_start || header_at != i)
            && is_ident(out.get(header_at), "quest")
            && matches!(out.get(header_at + 1).map(|t| &t.kind), Some(Tok::Ident(_)))
            && matches!(out.get(header_at + 2).map(|t| &t.kind), Some(Tok::Colon))
        {
            out[header_at].kind = Tok::Quest;
        }
        if at_line_start {
            let Tok::Ident(word) = &out[i].kind else {
                continue;
            };
            let next = out.get(i + 1).map(|t| &t.kind);
            let operand = matches!(next, Some(Tok::Ident(_) | Tok::Str(_) | Tok::FString(_)));
            let kind = match word.as_str() {
                "node" if operand => Tok::Node,
                "stage" if operand => Tok::Stage,
                "role" if operand => Tok::Role,
                "objective" if operand => Tok::Objective,
                "event" if operand => Tok::Event,
                "emit" if operand => Tok::Emit,
                "detour" if operand => Tok::Detour,
                "default"
                    if matches!(next, Some(Tok::Str(_) | Tok::FString(_)))
                        || (matches!(next, Some(Tok::Colon))
                            && matches!(out.get(i + 2).map(|t| &t.kind), Some(Tok::Newline))) =>
                {
                    Tok::Default
                }
                _ => continue,
            };
            out[i].kind = kind;
            if out[i].kind == Tok::Objective {
                let end = out[i + 1..]
                    .iter()
                    .position(|t| matches!(t.kind, Tok::Newline | Tok::Indent | Tok::Dedent))
                    .map_or(out.len(), |p| i + 1 + p);
                if let Some(at) = (i + 1..end).find(|&j| is_ident(out.get(j), "at")) {
                    out[at].kind = Tok::AtKw;
                }
            }
        }
    }
}

/// On a line begun by `dialogue` (or `pub dialogue`), after the parameter list: `topic` before a string or an f-string and
/// `priority` before a number become their own tokens (spec 5.13).
fn dialogue_header(line: &mut [Token]) {
    let end = line
        .iter()
        .position(|t| matches!(t.kind, Tok::Newline | Tok::Indent | Tok::Dedent))
        .unwrap_or(line.len());
    let Some(params) = line[..end].iter().position(|t| t.kind == Tok::LParen) else {
        return;
    };
    let mut depth = 0u32;
    let mut after = end;
    for (j, t) in line[..end].iter().enumerate().skip(params) {
        match t.kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth = depth.saturating_add(1),
            Tok::RParen | Tok::RBracket | Tok::RBrace => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    after = j + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    for j in after..end {
        let next = line.get(j + 1).map(|t| &t.kind);
        if is_ident(line.get(j), "topic") && matches!(next, Some(Tok::Str(_) | Tok::FString(_))) {
            line[j].kind = Tok::Topic;
        } else if is_ident(line.get(j), "priority") && matches!(next, Some(Tok::Int(_))) {
            line[j].kind = Tok::Priority;
        }
    }
}
