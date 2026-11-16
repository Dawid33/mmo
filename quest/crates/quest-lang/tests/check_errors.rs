//! Errors: names, inferred and pinned error sets, the must-handle rule and its handling forms (plan
//! Task 22; spec §5.7).

mod common;

use common::{diagnostics, diags, tree};
use quest_lang::Severity;
use std::collections::BTreeMap;

/// One expected diagnostic: code, file, line.
fn d(code: &str, file: &str, line: u32) -> (String, String, u32) {
    (code.to_string(), file.to_string(), line)
}

/// Every function's printed signature.
fn sigs(files: &[(&str, &str)]) -> BTreeMap<String, String> {
    quest_lang::signatures(&tree(files), &quest_stub::stub_api())
}

/// The diagnostics of a single `main.qs`.
fn main_diags(main: &str) -> Vec<(String, String, u32)> {
    diags(&[("main.qs", main)])
}

const TAKE_GOLD: &str = r#"fn take_gold(c: int, n: int) -> int:
    if c == 0:
        raise ChestEmpty
    if c < n:
        raise NotEnough(have=c)
    return n
"#;

#[test]
fn inferred_sets() {
    let main = format!(
        "{TAKE_GOLD}
fn pay(c: int) -> int:
    let g = take_gold(c, 20)?
    if g > 100:
        raise TooRich
    return g

fn relay(c: int) -> int:
    return pay(c)? + take_gold(c, 1)?
"
    );
    let files = [("main.qs", main.as_str())];
    assert_eq!(diags(&files), vec![]);
    let sigs = sigs(&files);
    assert_eq!(
        sigs["take_gold"],
        "fn(int, int) -> int raises ChestEmpty | NotEnough"
    );
    assert_eq!(
        sigs["pay"],
        "fn(int) -> int raises ChestEmpty | NotEnough | TooRich"
    );
    assert_eq!(
        sigs["relay"],
        "fn(int) -> int raises ChestEmpty | NotEnough | TooRich"
    );
}

#[test]
fn must_handle() {
    let main = format!(
        "{TAKE_GOLD}
fn main():
    take_gold(1, 20)
    let g = take_gold(1, 20)
    let _ = take_gold(1, 20)
    print(str(take_gold(1, 20)))
"
    );
    assert_eq!(
        main_diags(&main),
        vec![
            d("unhandled-fallible", "main.qs", 9),
            d("unhandled-fallible", "main.qs", 10),
            d("unhandled-fallible", "main.qs", 11),
            d("unhandled-fallible", "main.qs", 12),
        ]
    );
}

#[test]
fn handling_forms() {
    let main = format!(
        r#"{TAKE_GOLD}
fn propagate(c: int) -> int:
    return take_gold(c, 20)?

fn value(c: int) -> int:
    return take_gold(c, 20) else 0

fn diverge(c: int):
    let g = take_gold(c, 20) else return
    print(str(g))

fn reraise(c: int) -> int:
    return take_gold(c, 20) else raise Other

fn block_diverges(c: int) -> int:
    let g = take_gold(c, 20) else err:
        log(f"failed: {{err}}")
        return 0
    return g

fn block_value(c: int) -> int:
    let g = take_gold(c, 20) else err:
        log(f"failed: {{err}}")
        5
    return g

fn block_reraises(c: int) -> int:
    let g = take_gold(c, 20) else err:
        raise err
    return g

fn block_wrong(c: int) -> int:
    let g = take_gold(c, 20) else err:
        log("failed")
    return g

fn matched(p: Player, c: int):
    match take_gold(c, 20):
        case Ok(gold):
            say(p, str(gold))
        case Err(NotEnough(have)):
            say(p, f"Only {{have}} left.")
        case Err(ChestEmpty):
            pass
"#
    );
    let files = [("main.qs", main.as_str())];
    assert_eq!(diags(&files), vec![d("type-mismatch", "main.qs", 40)]);
    let sigs = sigs(&files);
    assert_eq!(
        sigs["propagate"],
        "fn(int) -> int raises ChestEmpty | NotEnough"
    );
    assert_eq!(sigs["value"], "fn(int) -> int");
    assert_eq!(sigs["diverge"], "fn(int) -> ()");
    assert_eq!(sigs["reraise"], "fn(int) -> int raises Other");
    assert_eq!(sigs["block_diverges"], "fn(int) -> int");
    assert_eq!(sigs["block_value"], "fn(int) -> int");
    assert_eq!(
        sigs["block_reraises"],
        "fn(int) -> int raises ChestEmpty | NotEnough"
    );
    assert_eq!(sigs["matched"], "fn(Player, int) -> ()");
}

#[test]
fn not_fallible() {
    let main = r#"fn one() -> int:
    return 1

fn a() -> int:
    return one()?

fn b() -> int:
    return one() else 0
"#;
    assert_eq!(
        main_diags(main),
        vec![
            d("not-fallible", "main.qs", 5),
            d("not-fallible", "main.qs", 8)
        ]
    );
}

#[test]
fn option_forms() {
    let main = r#"fn first(xs: list[int]) -> Option[int]:
    let x = xs.pop()?
    return Some(x + 1)

fn bad(xs: list[int]) -> int:
    let x = xs.pop()?
    return x

fn fallback(xs: list[int]) -> int:
    return xs.pop() else 0
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![d("type-mismatch", "main.qs", 6)]);
    let sigs = sigs(&files);
    assert_eq!(sigs["first"], "fn(list[int]) -> Option[int]");
    assert_eq!(sigs["fallback"], "fn(list[int]) -> int");
}

#[test]
fn pinned_set() {
    let main = r#"error ChestError = ChestEmpty | NotEnough(have: int)
error QuestError = ChestError | QuestClosed | NotYourQuest

fn pay(p: Player) -> Item raises QuestError:
    if p.has_tag("closed"):
        raise QuestClosed
    raise Other

fn single(p: Player) -> int raises Oops:
    raise Oops
"#;
    let files = [("main.qs", main)];
    assert_eq!(
        diags(&files),
        vec![d("raises-outside-pinned", "main.qs", 7)]
    );
    let sigs = sigs(&files);
    assert_eq!(sigs["single"], "fn(Player) -> int raises Oops");
}

#[test]
fn named_sets_union() {
    let main = r#"error ChestError = ChestEmpty | NotEnough(have: int)
error QuestError = ChestError | QuestClosed | NotYourQuest

fn pay(p: Player) -> int raises QuestError:
    if p.has_tag("empty"):
        raise ChestEmpty
    raise NotEnough(have=1)

fn caller(p: Player) -> int:
    return pay(p)?
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![]);
    let sigs = sigs(&files);
    let all = "fn(Player) -> int raises ChestEmpty | NotEnough | NotYourQuest | QuestClosed";
    assert_eq!(sigs["pay"], all);
    assert_eq!(sigs["caller"], all);
}

#[test]
fn one_shape_per_name() {
    let main = r#"fn a():
    raise NotEnough(have=3)

fn b():
    raise NotEnough

fn c():
    raise NotEnough(have="x")

fn d():
    raise Mine(n=1)

fn e():
    raise Mine(n="s")

fn f():
    let _ = world.compare_and_set("k", 1, 2)

fn g():
    let _ = world.compare_and_set("k", "a", "b")

error Declared = Bare | WithData(n: int)

fn h():
    raise Bare(x=1)

fn i():
    raise Fresh()

fn j():
    raise Fresh()

fn k():
    raise Fresh
"#;
    assert_eq!(
        main_diags(main),
        vec![
            d("error-shape", "main.qs", 5),
            d("error-shape", "main.qs", 8),
            d("error-shape", "main.qs", 14),
            d("error-shape", "main.qs", 20),
            d("error-shape", "main.qs", 25),
        ]
    );
}

#[test]
fn typo_warning() {
    let main = r#"fn take() -> int:
    raise QuestClsed

fn spill() -> int:
    raise Spillt

fn far() -> int:
    raise Elsewhere

fn main():
    match take():
        case Ok(n):
            pass
        case Err(QuestClosed):
            pass
    match spill():
        case Ok(n):
            pass
        case Err(Spillt | Spill):
            pass
    let n = far() else 0
"#;
    let found = diagnostics(&[("main.qs", main)]);
    let typos: Vec<_> = found.iter().filter(|d| d.code == "error-typo").collect();
    assert_eq!(typos.len(), 1, "{found:#?}");
    assert_eq!(typos[0].severity, Severity::Warning);
    assert_eq!(typos[0].line, 2);
    assert!(
        typos[0].message.contains("`QuestClsed`") && typos[0].message.contains("`QuestClosed`"),
        "{found:#?}"
    );
}

#[test]
fn errors_may_escape_main_and_handlers() {
    let main = r#"fn take() -> int:
    raise Oops

fn main():
    let n = take()?
    raise Other

@on(PlayerClicked)
fn h(e):
    let n = take()?
    raise Other
"#;
    assert_eq!(main_diags(main), vec![]);
}
