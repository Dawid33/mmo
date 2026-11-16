//! Layout and code generation for the core language, run end to end through the stub host (plan Task 25).

mod common;

use common::{run, tree};
use quest_vm::FaultKind;
use quest_vm::host::{AbortCause, Report};

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn spec_5_4() {
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
    let out = run(&[("main.qs", main)]);
    assert_eq!(out.lines, lines(&["total 130, stopped at 7"]));
    assert!(out.reports.is_empty(), "{:?}", out.reports);
    assert_eq!(out.leaked_words, 0);
}

#[test]
fn block_scope_and_shadowing() {
    let main = r#"let x = 1

fn main():
    let x = 2
    if true:
        let x = 3
        print(f"{x}")
    print(f"{x}")
"#;
    let out = run(&[("main.qs", main)]);
    assert_eq!(out.lines, lines(&["3", "2"]));
}

#[test]
fn loops() {
    let main = r#"fn main():
    let i = 0
    while true:
        i = i + 1
        if i > 6:
            break
        if i % 2 == 0:
            continue
        print(f"w {i}")
    for i in range(2, 5):
        print(f"{i}")
"#;
    let out = run(&[("main.qs", main)]);
    assert_eq!(out.lines, lines(&["w 1", "w 3", "w 5", "2", "3", "4"]));
}

#[test]
fn top_level_init_order() {
    let main = r#"mod b
use b::first

fn note(s):
    print(s)
    return 1

let m1 = note("main 1")
let m2 = first + note("main 2")

fn main():
    print(f"{m1} {m2}")
"#;
    let b = r#"fn note(s):
    print(s)
    return 10

pub let first = note("b 1")
let second = note("b 2")
"#;
    let out = run(&[("main.qs", main), ("b.qs", b)]);
    assert_eq!(
        out.lines,
        lines(&["b 1", "b 2", "main 1", "main 2", "1 11"])
    );
}

#[test]
fn calls_three_deep() {
    let main = r#"fn c(x):
    return x * 2

fn b(x):
    return c(x) + 1

fn a(x):
    return b(x) + b(x + 1)

fn greet(name):
    return "hi " + name

fn main():
    print(f"{a(3)}")
    print(greet(greet("ana")))
"#;
    let out = run(&[("main.qs", main)]);
    assert_eq!(out.lines, lines(&["16", "hi hi ana"]));
    assert_eq!(out.leaked_words, 0);
}

#[test]
fn int_and_float_printing() {
    let main = r#"fn main():
    print(f"{7 / 2}")
    print(f"{7.0 / 2.0}")
    print(f"{float(3)}")
    print(f"{7 / 2 == 3} {7.0 / 2.0 == 3.5}")
"#;
    let out = run(&[("main.qs", main)]);
    assert_eq!(out.lines, lines(&["3", "3.5", "3.0", "true true"]));
}

#[test]
fn runtime_fault_reports_the_line() {
    let main = r#"fn main():
    print("before")
    let z = 0
    print(f"{1 / z}")
    print("after")
"#;
    let out = run(&[("main.qs", main)]);
    assert!(out.lines.is_empty(), "{:?}", out.lines);
    match out.reports.as_slice() {
        [
            Report::HandlerAborted {
                cause: AbortCause::Fault { kind, stack, .. },
                ..
            },
        ] => {
            assert_eq!(*kind, FaultKind::DivByZero);
            assert_eq!(stack[0].line, 4);
            assert_eq!(stack[0].file, "main.qs");
        }
        other => panic!("expected one fault report, got {other:?}"),
    }
}

#[test]
fn bytecode_is_deterministic() {
    let files = [
        (
            "main.qs",
            "mod a\nmod b\nuse a::f\nuse b::g\n\nlet top = f(1) + g(2)\n\nfn main():\n    print(f\"{top} {f(3)}\")\n",
        ),
        ("a.qs", "pub fn f(x):\n    return x + 1\n"),
        (
            "b.qs",
            "pub let base = 5\n\npub fn g(x):\n    return x * base\n",
        ),
    ];
    let host = quest_stub::stub_api();
    let once = quest_lang::compile(&tree(&files), &host).expect("compiles");
    let twice = quest_lang::compile(&tree(&files), &host).expect("compiles");
    assert_eq!(once.program.to_bytes(), twice.program.to_bytes());
    // The same files inserted in the other order.
    let mut reversed = quest_lang::SourceTree::default();
    for (path, text) in files.iter().rev() {
        reversed.files.insert(path.to_string(), text.to_string());
    }
    let other = quest_lang::compile(&reversed, &host).expect("compiles");
    assert_eq!(once.program.to_bytes(), other.program.to_bytes());
}

#[test]
fn too_many_slots() {
    let mut main = String::from("fn main():\n");
    for _ in 0..70_000 {
        main.push_str("    let x = 1\n");
    }
    let diags = match quest_lang::compile(&tree(&[("main.qs", &main)]), &quest_stub::stub_api()) {
        Ok(_) => panic!("70 000 locals compiled"),
        Err(d) => d,
    };
    let codes: Vec<&str> = diags.iter().map(|d| d.code).collect();
    assert_eq!(codes, ["too-many-slots"]);
    assert_eq!((diags[0].file.as_str(), diags[0].line), ("main.qs", 1));
}
