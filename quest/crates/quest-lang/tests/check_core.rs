//! The call graph and core type inference (plan Task 20).

mod common;

use common::{diagnostics, diags, tree};
use std::collections::BTreeMap;

/// One expected diagnostic: code, file, line.
fn d(code: &str, file: &str, line: u32) -> (String, String, u32) {
    (code.to_string(), file.to_string(), line)
}

/// Every function's printed signature.
fn sigs(files: &[(&str, &str)]) -> BTreeMap<String, String> {
    quest_lang::signatures(&tree(files), &quest_stub::stub_api())
}

/// The messages of every diagnostic with `code`.
fn messages(files: &[(&str, &str)], code: &str) -> Vec<String> {
    diagnostics(files)
        .into_iter()
        .filter(|d| d.code == code)
        .map(|d| d.message)
        .collect()
}

#[test]
fn spec_5_4_signatures() {
    let main = r#"fn reward_for(stage):              # types inferred
    if stage == 0:
        return 0
    elif stage < 3:
        return 10 * stage
    else:
        return 50

fn main():
    let total = 0
    for s in range(5):
        total = total + reward_for(s)
    let i = 0
    while i < 10:
        i = i + 1
        if i == 7:
            break
    print(f"total {total}, stopped at {i}")
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![]);
    let sigs = sigs(&files);
    assert_eq!(sigs["reward_for"], "fn(int) -> int");
    assert_eq!(sigs["main"], "fn() -> ()");
}

#[test]
fn recursion_direct() {
    let files = [("main.qs", "fn f(n):\n    return f(n)\n")];
    assert_eq!(diags(&files), vec![d("recursion", "main.qs", 2)]);
    let all = diagnostics(&files);
    assert_eq!(all[0].message, "recursive call cycle: f -> f");
    assert_eq!((all[0].col, all[0].len), (12, 4), "at the call `f(n)`");
}

#[test]
fn recursion_mutual() {
    // The cycle starts at its smallest name, whichever is declared first.
    let main = r#"fn b(n: int) -> int:
    return a(n)

fn a(n: int) -> int:
    return b(n)
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![d("recursion", "main.qs", 5)]);
    assert_eq!(
        messages(&files, "recursion"),
        vec!["recursive call cycle: a -> b -> a"]
    );

    // Through a method.
    let main = r#"struct S:
    x: int

impl S:
    fn make() -> int:
        return helper()

fn helper() -> int:
    return S::make()
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![d("recursion", "main.qs", 6)]);
    assert_eq!(
        messages(&files, "recursion"),
        vec!["recursive call cycle: S::make -> helper -> S::make"]
    );
}

#[test]
fn needs_annotation() {
    let main = "fn id(x):\n    return x\n\nfn main():\n    let xs = []\n";
    let files = [("main.qs", main)];
    assert_eq!(
        diags(&files),
        vec![
            d("needs-annotation", "main.qs", 1),
            d("needs-annotation", "main.qs", 5)
        ]
    );
    let found = messages(&files, "needs-annotation");
    assert!(found[0].contains("`x`"), "{found:#?}");
    assert!(found[1].contains("`xs`"), "{found:#?}");
}

#[test]
fn mismatches() {
    let cases: [(&str, u32); 6] = [
        ("fn main():\n    let a = 1 + 1.0\n", 2),
        ("fn main():\n    if 1:\n        pass\n", 2),
        ("fn main():\n    let n: int = \"s\"\n", 2),
        ("fn main():\n    let x = 1\n    x = \"s\"\n", 3),
        (
            "fn f(c: bool):\n    if c:\n        return 1\n    return \"s\"\n",
            4,
        ),
        ("fn main():\n    let s = \"a\" + 1\n", 2),
    ];
    for (main, line) in cases {
        assert_eq!(
            diags(&[("main.qs", main)]),
            vec![d("type-mismatch", "main.qs", line)],
            "{main}"
        );
    }
}

#[test]
fn shadowing_may_change_type() {
    let main = "fn f():\n    let x = 1\n    let x = \"s\"\n    return x\n";
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![]);
    assert_eq!(sigs(&files)["f"], "fn() -> str");
}

#[test]
fn callee_first_ignores_declaration_order() {
    // `a` sorts before `z::later` and is declared first, in another module; it is still checked after it.
    let main =
        "mod z\n\nfn a():\n    return z::later(2)\n\nfn b():\n    let s: str = z::later(1)\n";
    let z = "pub fn later(n):\n    return n * 2\n";
    let files = [("main.qs", main), ("z.qs", z)];
    assert_eq!(diags(&files), vec![d("type-mismatch", "main.qs", 7)]);
    let sigs = sigs(&files);
    assert_eq!(sigs["a"], "fn() -> int");
    assert_eq!(sigs["z::later"], "fn(int) -> int");
}

#[test]
fn missing_return() {
    let main = "fn f(x: int):\n    if x > 0:\n        return 1\n\nfn g() -> int:\n    pass\n";
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![
            d("missing-return", "main.qs", 1),
            d("missing-return", "main.qs", 5)
        ]
    );
}

#[test]
fn arg_count() {
    let main =
        "fn f(a: int):\n    pass\n\nfn main():\n    f(1, 2)\n    f()\n    let x = float(1, 2)\n";
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![
            d("arg-count", "main.qs", 5),
            d("arg-count", "main.qs", 6),
            d("arg-count", "main.qs", 7)
        ]
    );
}

#[test]
fn conversions() {
    let main = "fn a():\n    return float(1)\n\nfn b():\n    return int(2.5)\n\nfn c():\n    let n = int(\"x\")\n";
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![d("type-mismatch", "main.qs", 8)]);
    let sigs = sigs(&files);
    assert_eq!(sigs["a"], "fn() -> float");
    assert_eq!(sigs["b"], "fn() -> int");
}

#[test]
fn annotations_are_checked() {
    let main = "fn f(x: int) -> str:\n    return x\n";
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![d("type-mismatch", "main.qs", 2)]
    );
}

/// Regression: a type built step by step through bindings (`let x1 = [x0]`, …) was re-walked in full at
/// every step — quadratic, 17 s for 4000 steps. It is linear now; the bound is generous.
#[test]
fn deep_inferred_types_are_linear() {
    let steps = 4000;
    for first in ["[1]", "[]"] {
        let mut main = format!("fn f():\n    let x0 = {first}\n");
        for i in 1..steps {
            main.push_str(&format!("    let x{i} = [x{}]\n", i - 1));
        }
        let start = std::time::Instant::now();
        let found = diags(&[("main.qs", &main)]);
        let unknown = usize::from(first == "[]");
        assert_eq!(found.len(), unknown, "{found:?}");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{first}: {:?}",
            start.elapsed()
        );
    }
}
