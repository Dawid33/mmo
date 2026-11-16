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

    // Through method calls `x.m()`, which the call graph cannot see (the receiver's type decides the callee).
    let main = r#"struct A:
    x: int

impl A:
    fn f(self) -> int:
        return self.f()
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![d("recursion", "main.qs", 6)]);
    assert_eq!(
        messages(&files, "recursion"),
        vec!["recursive call cycle: A::f -> A::f"]
    );

    let main = r#"struct A:
    x: int

struct B:
    x: int

impl A:
    fn f(self) -> int:
        let b = B(x=1)
        return b.g()

impl B:
    fn g(self) -> int:
        let a = A(x=1)
        return a.f()
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![d("recursion", "main.qs", 10)]);
    assert_eq!(
        messages(&files, "recursion"),
        vec!["recursive call cycle: A::f -> B::g -> A::f"]
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

    // A parameter used at two incompatible types asks for an annotation at the parameter, with a note
    // pointing at the conflicting use.
    let main = "fn twice(x):\n    let a = x + 1\n    let b = x + \"s\"\n";
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![d("needs-annotation", "main.qs", 1)]);
    let all = diagnostics(&files);
    assert!(all[0].message.contains("`x`"), "{all:#?}");
    assert!(
        all[0].notes.iter().any(|n| n.contains("line 3")),
        "{all:#?}"
    );
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
    // Reported where the end is reached: after the body's last statement.
    let main = "fn f(x: int):\n    if x > 0:\n        return 1\n\nfn g() -> int:\n    pass\n";
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![
            d("missing-return", "main.qs", 2),
            d("missing-return", "main.qs", 6)
        ]
    );

    // The fall-off does not type the returned parameter as `()`: it stays unknown.
    let main = "fn f(c: bool, x):\n    if c:\n        return x\n";
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![
            d("missing-return", "main.qs", 2),
            d("needs-annotation", "main.qs", 1)
        ]
    );

    // A call to a function that never returns ends the block, as `raise` does (it can fail, so it is
    // handled: Task 22).
    let main = "fn fail():\n    raise Oops\n\nfn g() -> int:\n    fail()?\n";
    assert_eq!(diags(&[("main.qs", main)]), vec![]);
}

/// Regression: every type in `Typed` is final — a local bound to `list[_]` and then assigned `[1]` is
/// `list[int]`, not a type variable.
#[test]
fn typed_holds_final_types() {
    let main = "fn f():\n    let xs = []\n    xs = [1]\n";
    let host = quest_stub::stub_api();
    let (krate, _) = quest_lang::load_modules(&tree(&[("main.qs", main)]));
    let (program, _) = quest_lang::resolve(&krate, &host);
    let (typed, found) = quest_lang::infer(&program, &host);
    assert!(found.is_empty(), "{found:#?}");
    let printer = quest_lang::ty::Printer {
        types: &typed.types,
        krate: &program,
        host: &host,
    };
    let xs = program
        .locals
        .iter()
        .position(|l| l.name == "xs")
        .expect("a local `xs`");
    assert_eq!(printer.print(typed.locals[xs]), "list[int]");
    let all = typed.locals.iter().chain(&typed.exprs);
    for t in all {
        assert!(
            !matches!(typed.types.get(*t), quest_lang::ty::Ty::Var(_)),
            "{}",
            printer.print(*t)
        );
    }
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
/// every step — quadratic, 17 s for 4000 steps. Counts the type nodes inference visits (not time, which
/// a loaded machine inflates): doubling the steps must at most about double the visits (quadratic: 4x).
#[test]
fn deep_inferred_types_are_linear() {
    let visits = |first: &str, steps: usize| -> u64 {
        let mut main = format!("fn f():\n    let x0 = {first}\n");
        for i in 1..steps {
            main.push_str(&format!("    let x{i} = [x{}]\n", i - 1));
        }
        quest_lang::work::reset();
        let found = diags(&[("main.qs", &main)]);
        assert_eq!(found.len(), usize::from(first == "[]"), "{found:?}");
        quest_lang::work::type_visits()
    };
    // Ground element type: every step's type is final at once, so the work is linear.
    let (small, large) = (visits("[1]", 1000), visits("[1]", 2000));
    // 2x plus slack for fixed costs.
    assert!(large <= 2 * small + 1000, "[1]: {small} -> {large}");
    // An unresolved element type (a variable until the end): each step's occurs check still walks the
    // type built so far, 2n^2 nodes in all and cheap. Known; the budget pins that constant, so a return
    // to re-walking with the old cost (or worse) fails.
    let n = 1000;
    let work = visits("[]", n as usize);
    assert!(work <= 3 * n * n, "[]: {work} visits for {n} steps");
}
