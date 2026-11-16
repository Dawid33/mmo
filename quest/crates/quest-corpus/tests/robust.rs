//! The compiler on arbitrary input never panics, overflows its stack or hangs (Review Focus 2), and the
//! program generator is sound: what it yields compiles with no diagnostic (plan Task 33).
#![cfg(not(target_arch = "wasm32"))]

use proptest::prelude::*;
use quest_corpus::generate;
use quest_lang::{SourceTree, compile, render};

/// The most bytes of one random source.
const SOURCE_BYTES_MAX: usize = 2048;

fn tree(text: &str) -> SourceTree {
    SourceTree {
        files: [("main.qs".to_string(), text.to_string())].into(),
    }
}

/// The token alphabet: every keyword and contextual word, punctuation, names, literals, newlines and runs
/// of spaces (indentation is significant, so these make most of the interesting inputs).
const TOKENS: &[&str] = &[
    "fn",
    "let",
    "if",
    "elif",
    "else",
    "while",
    "for",
    "in",
    "not",
    "and",
    "or",
    "return",
    "break",
    "continue",
    "pass",
    "match",
    "case",
    "struct",
    "impl",
    "enum",
    "error",
    "mod",
    "use",
    "pub",
    "as",
    "raise",
    "await",
    "self",
    "crate",
    "super",
    "true",
    "false",
    "dialogue",
    "choice",
    "once",
    "when",
    "where",
    "node",
    "stage",
    "role",
    "objective",
    "event",
    "emit",
    "detour",
    "default",
    "per",
    "at",
    "topic",
    "priority",
    "quest",
    "main",
    "print",
    "str",
    "int",
    "float",
    "list",
    "dict",
    "Option",
    "Some",
    "None",
    "Ok",
    "Err",
    "Promise",
    "world",
    "get",
    "add",
    "range",
    "len",
    "append",
    "Player",
    "PlayerClicked",
    "on",
    "x",
    "y",
    "e",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ",",
    ":",
    "::",
    ".",
    "@",
    "=",
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "+",
    "-",
    "*",
    "/",
    "%",
    "&",
    "|",
    "?",
    "->",
    "=>",
    "\"",
    "'",
    "f\"",
    "f'",
    "\"a\"",
    "f\"{x}\"",
    "f\"{",
    "}\"",
    "0",
    "1",
    "42",
    "9223372036854775807",
    "99999999999999999999",
    "1.5",
    "1e3",
    "0xff",
    "1_000",
    "#",
    "# c",
    "\\",
    "\t",
    "\n",
    "\n",
    "\n    ",
    "\n        ",
    " ",
    "  ",
    "    ",
    "        ",
    "\r\n",
    "ß",
    "\u{0}",
];

fn token_soup() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(TOKENS), 0..600).prop_map(|toks| {
        let mut s = String::new();
        for t in toks {
            if s.len() + t.len() > SOURCE_BYTES_MAX {
                break;
            }
            // Mostly space-separated, sometimes glued.
            if !s.is_empty() && !s.ends_with(['\n', ' ']) && t.len() % 3 != 0 {
                s.push(' ');
            }
            s.push_str(t);
        }
        s
    })
}

fn lossy_bytes() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<u8>(), 0..SOURCE_BYTES_MAX)
        .prop_map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// A generated program (or a corpus case) with a few word-level edits: a word replaced by one of the
/// alphabet, a line dropped or doubled. Most are near misses, which reach the checker and code generator
/// rather than stopping in the lexer or parser.
fn near_miss() -> impl Strategy<Value = String> {
    let base = prop_oneof![
        generate::program(),
        proptest::sample::select(quest_corpus::RUN).prop_map(|c| {
            c.files
                .iter()
                .find(|(p, _)| *p == "main.qs")
                .map_or(String::new(), |(_, t)| t.to_string())
        }),
    ];
    let edit = (
        any::<u16>(),
        any::<u16>(),
        proptest::sample::select(TOKENS),
        0u8..4,
    );
    (base, proptest::collection::vec(edit, 1..4)).prop_map(|(text, edits)| {
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        for (at, word, tok, how) in edits {
            if lines.is_empty() {
                break;
            }
            let i = usize::from(at) % lines.len();
            match how {
                0 => {
                    lines.remove(i);
                }
                1 => {
                    let l = lines[i].clone();
                    lines.insert(i, l);
                }
                _ => {
                    let mut words: Vec<&str> = lines[i].split(' ').collect();
                    let w = usize::from(word) % words.len();
                    words[w] = tok;
                    lines[i] = words.join(" ");
                }
            }
        }
        let mut out = lines.join("\n");
        out.truncate(out.floor_char_boundary(SOURCE_BYTES_MAX * 8));
        out
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(generate::cases()))]

    #[test]
    fn compiler_never_panics(text in prop_oneof![3 => token_soup(), 1 => lossy_bytes(), 3 => near_miss()]) {
        // Returning at all is the property: a panic, a stack overflow or a hang fails the test.
        let _ = compile(&tree(&text), &quest_stub::stub_api());
    }

    #[test]
    fn generated_programs_compile(text in generate::program()) {
        let t = tree(&text);
        match compile(&t, &quest_stub::stub_api()) {
            Ok(c) => {
                let warnings: Vec<String> = c.warnings.iter().map(|d| render(d, &t)).collect();
                prop_assert!(warnings.is_empty(), "warnings:\n{}\n--- source\n{text}", warnings.join("\n"));
            }
            Err(diags) => {
                let rendered: Vec<String> = diags.iter().map(|d| render(d, &t)).collect();
                prop_assert!(false, "diagnostics:\n{}\n--- source\n{text}", rendered.join("\n"));
            }
        }
    }
}
