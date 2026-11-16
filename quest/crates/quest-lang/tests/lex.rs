use quest_lang::{Diagnostic, FPart, SourceTree, Tok, Token, lex, render};
use std::collections::BTreeMap;

fn toks(text: &str) -> Vec<Tok> {
    let (tokens, diags) = lex("main.qs", text);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    tokens.into_iter().map(|t| t.kind).collect()
}

fn ident(s: &str) -> Tok {
    Tok::Ident(s.to_string())
}

fn codes(text: &str) -> Vec<(&'static str, u32, u32)> {
    lex("main.qs", text)
        .1
        .iter()
        .map(|d| (d.code, d.line, d.col))
        .collect()
}

#[test]
fn indent_dedent_basic() {
    use Tok::*;
    assert_eq!(
        toks("if x:\n    y\nz\n"),
        vec![
            If,
            ident("x"),
            Colon,
            Newline,
            Indent,
            ident("y"),
            Newline,
            Dedent,
            ident("z"),
            Newline,
            Eof
        ]
    );
}

#[test]
fn blank_and_comment_lines_are_invisible() {
    use Tok::*;
    let plain = toks("if x:\n    y\n");
    assert_eq!(
        toks("\n# top\nif x:\n\n    # inner\n   \n    y  # tail\n\n"),
        plain
    );
    assert_eq!(
        plain,
        vec![
            If,
            ident("x"),
            Colon,
            Newline,
            Indent,
            ident("y"),
            Newline,
            Dedent,
            Eof
        ]
    );
}

#[test]
fn brackets_suppress_newlines() {
    use Tok::*;
    assert_eq!(
        toks("f(1,\n      2)\n[\n3,\n]\n"),
        vec![
            ident("f"),
            LParen,
            Int(1),
            Comma,
            Int(2),
            RParen,
            Newline,
            LBracket,
            Int(3),
            Comma,
            RBracket,
            Newline,
            Eof
        ]
    );
}

#[test]
fn dedents_close_at_eof() {
    use Tok::*;
    assert_eq!(
        toks("a:\n  b:\n    c"),
        vec![
            ident("a"),
            Colon,
            Newline,
            Indent,
            ident("b"),
            Colon,
            Newline,
            Indent,
            ident("c"),
            Newline,
            Dedent,
            Dedent,
            Eof
        ]
    );
}

#[test]
fn edge_inputs() {
    use Tok::*;
    assert_eq!(toks(""), vec![Eof]);
    assert_eq!(toks("x"), vec![ident("x"), Newline, Eof]);
    let lf = "if x:\n    y\nz\n";
    assert_eq!(lex("m", &lf.replace('\n', "\r\n")).0, lex("m", lf).0);
    assert_eq!(lex("m", "\u{feff}x = 1\n"), lex("m", "x = 1\n"));
    let (tokens, _) = lex("m", "\u{feff}x");
    assert_eq!((tokens[0].line, tokens[0].col), (1, 1));
}

#[test]
fn errors() {
    assert_eq!(codes("if x:\n\ty\n"), vec![("tab-indent", 2, 1)]);
    assert_eq!(codes("if x:\n    a\n  b\n"), vec![("bad-dedent", 3, 1)]);
    assert_eq!(codes("a $ b\n"), vec![("bad-char", 1, 3)]);
    assert_eq!(codes("\"abc\n"), vec![("unterminated-string", 1, 1)]);
    assert_eq!(codes("\"abc"), vec![("unterminated-string", 1, 1)]);
    assert_eq!(codes("x = \"\\q\"\n"), vec![("bad-escape", 1, 6)]);
    assert_eq!(codes("1.\n"), vec![("bad-number", 1, 1)]);
    assert_eq!(codes("0x\n"), vec![("bad-number", 1, 1)]);
    assert_eq!(codes("9223372036854775808\n"), vec![("bad-number", 1, 1)]);
    assert_eq!(codes("żółw = 1\n"), vec![("non-ascii-ident", 1, 1)]);
    let (_, d) = lex("m", "żółw = 1\n");
    assert_eq!(d[0].len, 4);
}

#[test]
fn columns_count_characters() {
    assert_eq!(codes("let s = \"żż\" $\n"), vec![("bad-char", 1, 14)]);
}

#[test]
fn numbers() {
    use Tok::*;
    assert_eq!(
        toks("1_000 0xff 1.5 1e3"),
        vec![Int(1000), Int(255), Float(1.5), Float(1000.0), Newline, Eof]
    );
}

#[test]
fn strings_and_fstrings() {
    use Tok::*;
    assert_eq!(
        toks(r#""a\nb" 'c\td' "\\ \" \' \{ \}" '"'"#),
        vec![
            Str("a\nb".into()),
            Str("c\td".into()),
            Str("\\ \" ' { }".into()),
            Str("\"".into()),
            Newline,
            Eof
        ]
    );
    let t = toks(r#"f"a{x + 1}b{{c}}""#);
    assert_eq!(
        t[0],
        FString(vec![
            FPart::Text("a".into()),
            FPart::Expr(vec![
                Token {
                    kind: ident("x"),
                    line: 1,
                    col: 5,
                    len: 1
                },
                Token {
                    kind: Plus,
                    line: 1,
                    col: 7,
                    len: 1
                },
                Token {
                    kind: Int(1),
                    line: 1,
                    col: 9,
                    len: 1
                },
            ]),
            FPart::Text("b{c}".into()),
        ])
    );
    // A diagnostic inside an f-string expression points at its own column.
    assert_eq!(
        codes("let s = \"żż\" + f\"é{a $ b}\"\n"),
        vec![("bad-char", 1, 22)]
    );
}

#[test]
fn a_megabyte_line_lexes() {
    let text = "1 + ".repeat(1 << 18);
    let (tokens, diags) = lex("m", &text);
    assert!(diags.is_empty());
    assert_eq!(tokens.len(), (1 << 19) + 2);
}

fn tree(text: &str) -> SourceTree {
    SourceTree {
        files: BTreeMap::from([("main.qs".to_string(), text.to_string())]),
    }
}

#[test]
fn render_format() {
    let src = tree("fn f(n):\n    return f(n)\n");
    let mut d = Diagnostic::error(
        "recursion",
        "recursive call cycle: f -> f",
        "main.qs",
        2,
        12,
        4,
    );
    assert_eq!(
        render(&d, &src),
        "error[recursion]: recursive call cycle: f -> f\n --> main.qs:2:12\n  |\n2 |     return f(n)\n  |            ^^^^"
    );
    d.notes.push("calls cannot loop".into());
    assert!(render(&d, &src).ends_with("^^^^\n  = note: calls cannot loop"));
    // A wider gutter for a line number with more digits.
    let long = tree(&"\n".repeat(11).replace('\n', "x\n"));
    let d = Diagnostic::error("syntax", "m", "main.qs", 10, 1, 1);
    assert_eq!(
        render(&d, &long),
        "error[syntax]: m\n  --> main.qs:10:1\n   |\n10 | x\n   | ^"
    );
    // Past the end of the file: no source line.
    let d = Diagnostic::error("syntax", "m", "main.qs", 99, 1, 1);
    assert_eq!(render(&d, &src), "error[syntax]: m\n  --> main.qs:99:1");
}

#[test]
fn dialogue_tokens() {
    use Tok::*;
    assert_eq!(
        toks("=> dialogue quest choice once when where"),
        vec![
            FatArrow, Dialogue, Quest, Choice, Once, When, Where, Newline, Eof
        ]
    );
    assert_eq!(
        toks("@farmer.start"),
        vec![At, ident("farmer"), Dot, ident("start"), Newline, Eof]
    );
    assert_eq!(toks("# a comment @x"), vec![Eof]);
}

#[test]
fn contextual_words() {
    use Tok::*;
    assert_eq!(
        toks("node start:"),
        vec![Node, ident("start"), Colon, Newline, Eof]
    );
    assert_eq!(
        toks("node = 1"),
        vec![ident("node"), Assign, Int(1), Newline, Eof]
    );
    assert_eq!(
        toks("match node:"),
        vec![Match, ident("node"), Colon, Newline, Eof]
    );
    assert_eq!(
        toks("stage Open:"),
        vec![Stage, ident("Open"), Colon, Newline, Eof]
    );
    assert_eq!(
        toks("role dog: Creature"),
        vec![Role, ident("dog"), Colon, ident("Creature"), Newline, Eof]
    );
    assert_eq!(
        toks("objective \"Find\" at dog"),
        vec![
            Objective,
            Str("Find".into()),
            AtKw,
            ident("dog"),
            Newline,
            Eof
        ]
    );
    assert_eq!(
        toks("once per p:"),
        vec![Once, Per, ident("p"), Colon, Newline, Eof]
    );
    assert_eq!(
        toks("let per = 1"),
        vec![Let, ident("per"), Assign, Int(1), Newline, Eof]
    );
    assert_eq!(
        toks("default \"Option 1?\":"),
        vec![Default, Str("Option 1?".into()), Colon, Newline, Eof]
    );
    assert_eq!(toks("default:\n"), vec![Default, Colon, Newline, Eof]);
    assert_eq!(
        toks("default = 3"),
        vec![ident("default"), Assign, Int(3), Newline, Eof]
    );
    assert_eq!(
        toks("emit Accepted(player=p)"),
        vec![
            Emit,
            ident("Accepted"),
            LParen,
            ident("player"),
            Assign,
            ident("p"),
            RParen,
            Newline,
            Eof
        ]
    );
    assert_eq!(
        toks("detour farmer_secret"),
        vec![Detour, ident("farmer_secret"), Newline, Eof]
    );
    assert_eq!(
        toks("event Accepted:"),
        vec![Event, ident("Accepted"), Colon, Newline, Eof]
    );
    assert_eq!(
        toks("let event = 1"),
        vec![Let, ident("event"), Assign, Int(1), Newline, Eof]
    );
    assert_eq!(
        toks("emit(x)"),
        vec![ident("emit"), LParen, ident("x"), RParen, Newline, Eof]
    );
    // First on a line after a dedent counts, too.
    assert_eq!(
        toks("a:\n  b\nnode x:\n"),
        vec![
            ident("a"),
            Colon,
            Newline,
            Indent,
            ident("b"),
            Newline,
            Dedent,
            Node,
            ident("x"),
            Colon,
            Newline,
            Eof
        ]
    );
}
