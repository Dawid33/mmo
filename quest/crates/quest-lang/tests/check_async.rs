//! References, promises, `await` and handlers (plan Task 24; spec §5.3, §7, §8.2).

mod common;

use common::{diags, tree};
use std::collections::BTreeMap;

/// One expected diagnostic: code, file, line.
fn d(code: &str, file: &str, line: u32) -> (String, String, u32) {
    (code.to_string(), file.to_string(), line)
}

/// The diagnostics of a single `main.qs`.
fn main_diags(main: &str) -> Vec<(String, String, u32)> {
    diags(&[("main.qs", main)])
}

/// Every function's printed signature.
fn sigs(files: &[(&str, &str)]) -> BTreeMap<String, String> {
    quest_lang::signatures(&tree(files), &quest_stub::stub_api())
}

#[test]
fn ref_escapes() {
    let main = r#"struct Holder:
    r: &int

let a = 1
let g = &a

fn keep(r: &int) -> int:
    return *r

fn give(r: &int):
    return r

fn main():
    let x = 1
    let r = &x
    let n = keep(r) + keep(&x)
    let xs = [r]
    let d = {"k": r}
    let o = Some(&x)
    raise Leak(r=r)
"#;
    assert_eq!(
        main_diags(main),
        vec![
            d("ref-escapes", "main.qs", 2),
            d("ref-escapes", "main.qs", 5),
            d("ref-escapes", "main.qs", 11),
            d("ref-escapes", "main.qs", 17),
            d("ref-escapes", "main.qs", 18),
            d("ref-escapes", "main.qs", 19),
            d("ref-escapes", "main.qs", 20),
        ]
    );
}

#[test]
fn ref_needs_a_place() {
    let main = r#"struct Box:
    b: list[int]

fn f() -> int:
    return 1

fn main():
    let a = Box(b=[1, 2, 3])
    let n = 2
    let r1 = &f()
    let r2 = &(n + n)
    let r3 = &a.b[2]
    *r3 = 7
"#;
    assert_eq!(
        main_diags(main),
        vec![
            d("ref-not-place", "main.qs", 10),
            d("ref-not-place", "main.qs", 11)
        ]
    );
}

#[test]
fn deref_assign_types() {
    let main = r#"fn main():
    let x = 1
    let r = &x
    *r = "s"
"#;
    assert_eq!(main_diags(main), vec![d("type-mismatch", "main.qs", 4)]);
}

#[test]
fn await_rules() {
    let main = r#"let early = await world.get("k")

fn helper() -> int:
    return await world.add("k", 1) else 0

@on(PlayerClicked)
fn h(e):
    let n = helper()
    let bad = await 5

fn outer() -> int:
    return helper()

let late = outer()
"#;
    assert_eq!(
        main_diags(main),
        vec![
            d("await-in-init", "main.qs", 1),
            d("await-in-init", "main.qs", 14),
            d("await-not-promise", "main.qs", 9),
        ]
    );
}

#[test]
fn unused_promise() {
    let main = r#"fn main():
    world.set("k", 1)
    let _ = world.set("k", 1)
    await world.set("k", 1)
"#;
    assert_eq!(
        main_diags(main),
        vec![
            d("unhandled-fallible", "main.qs", 4),
            d("unused-promise", "main.qs", 2)
        ]
    );
}

#[test]
fn get_needs_a_type() {
    let main = r#"fn a():
    let n = await world.get("k")?

fn b():
    let n: int = await world.get("k")?

fn c() -> int:
    let n = await world.get("k")?
    return n + 1
"#;
    assert_eq!(main_diags(main), vec![d("needs-annotation", "main.qs", 2)]);
}

#[test]
fn changed_has_one_type() {
    let main = r#"fn a():
    let _ = world.compare_and_set("k", 1, 2)

fn b():
    let _ = world.compare_and_set("k", "x", "y")
"#;
    assert_eq!(main_diags(main), vec![d("error-shape", "main.qs", 5)]);
}

#[test]
fn await_all_types() {
    let main = r#"fn f():
    let a = world.add("a", 1)
    let b = world.add("b", 2)
    return await all([a, b])?
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![]);
    assert_eq!(
        sigs(&files)["f"],
        "fn() -> list[int] raises Lost | NamespaceGone | NotFound | Timeout | WrongType"
    );
}

#[test]
fn handlers() {
    let main = r#"@on(Nope)
fn a(e):
    pass

@on(PlayerClicked)
fn two(e, f: int):
    pass

@on(PlayerClicked)
fn value(e) -> int:
    return 1

@on(PlayerClicked)
fn greet(e):
    say(e.player, "hi")

@on(PlayerClicked)
fn greet_again(e):
    let n: int = e.player
"#;
    let files = [("main.qs", main)];
    assert_eq!(
        diags(&files),
        vec![
            d("handler-signature", "main.qs", 6),
            d("handler-signature", "main.qs", 10),
            d("type-mismatch", "main.qs", 19),
            d("unknown-event", "main.qs", 1),
        ]
    );
    assert_eq!(sigs(&files)["greet"], "fn(PlayerClicked) -> ()");
}

// Regressions.

/// Two functions' promises with different (fixed) error sets meet in a list: the list's promises raise
/// the union, not the first one's set.
#[test]
fn fixed_promise_sets_unite() {
    let main = r#"fn a() -> Promise[()]:
    return world.set("k", 1)

fn c() -> Promise[()]:
    return world.compare_and_set("k", 1, 2)

fn both():
    return [a(), c()]
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![]);
    assert_eq!(
        sigs(&files)["both"],
        "fn() -> list[Promise[() raises Changed | Lost | NamespaceGone | NotFound | Timeout]]"
    );
}

/// A promise parameter accepts only the errors its body knows, unless the body handles every error.
#[test]
fn promise_parameters() {
    let main = r#"fn any(p: Promise[int]) -> int:
    let v = await p else 0
    return v

fn known(p: Promise[int]) -> int:
    return await p?

fn main():
    let x = any(world.get("k"))
    let y = known(world.add("k", 1))
"#;
    assert_eq!(main_diags(main), vec![d("type-mismatch", "main.qs", 10)]);
}

/// An awaited promise's errors are closed once a `match` read them: a promise with more errors cannot reach
/// the same variable later.
#[test]
fn awaited_sets_close() {
    let main = r#"fn f():
    let p = world.set("k", 1)
    while true:
        match await p:
            case Ok(_):
                pass
            case Err(Timeout | Lost | NamespaceGone):
                pass
        p = world.delete("k")
"#;
    assert_eq!(main_diags(main), vec![d("type-mismatch", "main.qs", 9)]);
}
