//! Code generation for errors: `raise`, `?`, the `else` fallbacks, the fallible `match` and error return
//! traces, run end to end through the stub host (plan Task 27; spec §5.7).

mod common;

use common::run;
use quest_vm::host::{AbortCause, Report};
use quest_vm::wire::WireValue;

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Runs `main.qs`: no report, nothing leaked; what it printed.
fn printed(main: &str) -> Vec<String> {
    let out = run(&[("main.qs", main)]);
    assert!(out.reports.is_empty(), "{:?}", out.reports);
    assert_eq!(out.leaked_words, 0, "leaked");
    out.lines
}

/// The chest of the spec's examples, and `take_gold` failing as §5.7 describes.
const CHEST: &str = "struct Chest:
    gold: int

fn take_gold(c: Chest, n: int) -> int:
    if c.gold == 0:
        raise ChestEmpty
    if c.gold < n:
        raise NotEnough(have=c.gold)
    return n
";

#[test]
fn spec_5_7_forms() {
    // `?`: the error passes on, the value goes on.
    let main = format!(
        r#"{CHEST}
fn pay(c: Chest, n: int) -> int:
    let gold = take_gold(c, n)?
    return gold * 10

fn main():
    let c = Chest(gold=3)
    print(str(pay(c, 2) else -1))
    print(str(pay(c, 20) else -1))
"#
    );
    assert_eq!(printed(&main), lines(&["20", "-1"]));

    // `else 0`: a fallback value.
    let main = format!(
        r#"{CHEST}
fn main():
    let c = Chest(gold=3)
    let gold = take_gold(c, 20) else 0
    print(f"got {{gold}}")
    let more = take_gold(c, 2) else 0
    print(f"got {{more}}")
"#
    );
    assert_eq!(printed(&main), lines(&["got 0", "got 2"]));

    // `else return`: the fallback leaves.
    let main = format!(
        r#"{CHEST}
fn give(c: Chest, n: int):
    let gold = take_gold(c, n) else return
    print(f"gave {{gold}}")

fn main():
    let c = Chest(gold=3)
    give(c, 2)
    give(c, 20)
    print("done")
"#
    );
    assert_eq!(printed(&main), lines(&["gave 2", "done"]));

    // `else err:` + block: the error is bound, the block leaves.
    let main = format!(
        r#"{CHEST}
fn give(c: Chest, n: int) -> int:
    let gold = take_gold(c, n) else err:
        print(f"failed: {{err}}")
        return 0
    return gold

fn main():
    let c = Chest(gold=3)
    print(str(give(c, 20)))
    print(str(give(c, 1)))
"#
    );
    assert_eq!(
        printed(&main),
        lines(&["failed: NotEnough(have=3)", "0", "1"])
    );

    // `match`: `Ok`, `Err` with data, `Err` without.
    let main = format!(
        r#"{CHEST}
fn give(c: Chest, n: int):
    match take_gold(c, n):
        case Ok(gold):
            print(f"Here are {{gold}} gold.")
        case Err(NotEnough(have)):
            print(f"Only {{have}} left.")
        case Err(ChestEmpty):
            print("The chest is empty.")

fn main():
    let c = Chest(gold=3)
    give(c, 2)
    give(c, 20)
    give(Chest(gold=0), 1)
"#
    );
    assert_eq!(
        printed(&main),
        lines(&["Here are 2 gold.", "Only 3 left.", "The chest is empty."])
    );
}

#[test]
fn escaping_error_report() {
    let main = format!(
        r#"{CHEST}
fn pay(c: Chest) -> int:
    let gold = take_gold(c, 20)?
    return gold

fn main():
    print("before")
    let c = Chest(gold=3)
    print(str(pay(c)?))
"#
    );
    let out = run(&[("main.qs", main.as_str())]);
    // An aborted task prints nothing.
    assert!(out.lines.is_empty(), "{:?}", out.lines);
    match out.reports.as_slice() {
        [
            Report::HandlerAborted {
                cause:
                    AbortCause::Error {
                        name, data, trace, ..
                    },
                ..
            },
        ] => {
            assert_eq!(name, "NotEnough");
            assert_eq!(
                *data,
                WireValue::Variant {
                    name: "NotEnough".to_string(),
                    fields: vec![("have".to_string(), WireValue::Int(3))],
                }
            );
            let at: Vec<(&str, u32)> = trace.iter().map(|l| (l.file.as_str(), l.line)).collect();
            // The `raise` in `take_gold`, the `?` in `pay`, the `?` in `main`.
            assert_eq!(at, vec![("main.qs", 8), ("main.qs", 12), ("main.qs", 18)]);
        }
        other => panic!("expected one error report, got {other:?}"),
    }

    // A caught error raised again keeps its trace: the first `raise`, the re-raise, the `?`. A script's
    // own `enum Error` does not stand in for the program's errors.
    let main = format!(
        r#"{CHEST}
enum Error:
    A
    B(n: int)

fn pay(c: Chest) -> int:
    let gold = take_gold(c, 20) else err:
        raise err
    return gold

fn main():
    let c = Chest(gold=3)
    let mine = Error::B(n=4)
    print(str(pay(c)?))
"#
    );
    let out = run(&[("main.qs", main.as_str())]);
    match out.reports.as_slice() {
        [
            Report::HandlerAborted {
                cause:
                    AbortCause::Error {
                        name, data, trace, ..
                    },
                ..
            },
        ] => {
            assert_eq!(name, "NotEnough");
            assert_eq!(
                *data,
                WireValue::Variant {
                    name: "NotEnough".to_string(),
                    fields: vec![("have".to_string(), WireValue::Int(3))],
                }
            );
            let at: Vec<u32> = trace.iter().map(|l| l.line).collect();
            assert_eq!(at, vec![8, 17, 23]);
        }
        other => panic!("expected one error report, got {other:?}"),
    }
}

#[test]
fn option_question_mark() {
    let main = r#"fn first_even(xs: list[int]) -> Option[int]:
    for x in xs:
        if x % 2 == 0:
            return Some(x)
    return None

fn double_first(xs: list[int]) -> Option[int]:
    let x = first_even(xs)?
    return Some(x * 2)

fn shout(names: dict[str, str], key: str) -> Option[str]:
    let name = names.get(key)?
    return Some(name + "!")

fn main():
    print(f"{double_first([1, 4, 6])} {double_first([1, 3])}")
    let names = {"a": "ana"}
    let found = shout(names, "a")
    let missing = shout(names, "b")
    print(f"{found} {missing}")
"#;
    assert_eq!(
        printed(main),
        lines(&["Some(8) None", r#"Some("ana!") None"#])
    );
}

#[test]
fn raise_with_data_bound_in_match() {
    let main = r#"fn check(n: int, label: str) -> int:
    if n > 2:
        raise TooMany(count=n, label=label)
    if n < 0:
        raise Negative
    return n

fn describe(n: int):
    match check(n, "crowd"):
        case Ok(v):
            print(f"ok {v}")
        case TooMany(count, label):
            print(f"{label} of {count}")
        case Negative:
            print("negative")

fn main():
    describe(1)
    describe(5)
    describe(-1)
    match check(7, "pack"):
        case Err(TooMany(label=l, count=c)) if c > 6:
            print(f"big {l} {c}")
        case _:
            print("other")
"#;
    assert_eq!(
        printed(main),
        lines(&["ok 1", "crowd of 5", "negative", "big pack 7"])
    );
}

#[test]
fn error_data_formats() {
    let main = format!(
        r#"{CHEST}
fn main():
    let c = Chest(gold=3)
    let gold = take_gold(c, 20) else err:
        print(f"{{err}}")
        0
    let none = take_gold(Chest(gold=0), 1) else err:
        print(f"{{err}}")
        0
    print(str(gold + none))
"#
    );
    assert_eq!(
        printed(&main),
        lines(&["NotEnough(have=3)", "ChestEmpty", "0"])
    );

    // A script's own `enum Error` is an ordinary enum.
    let main = format!(
        r#"{CHEST}
enum Error:
    A
    B(n: int)

fn main():
    let gold = take_gold(Chest(gold=3), 20) else err:
        print(f"{{err}}")
        0
    print(f"{{Error::B(n=4)}} {{Error::A}} {{gold}}")
"#
    );
    assert_eq!(
        printed(&main),
        lines(&["NotEnough(have=3)", "Error::B(n=4) Error::A 0"])
    );
}

#[test]
fn no_leaks_on_error_paths() {
    let main = r#"fn fail(n: int, tag: str) -> int:
    let s = tag + "!"
    let xs = [s, "b"]
    if n > 0:
        raise Bad(msg=s, parts=xs)
    return n

fn relay(n: int) -> int:
    let s = "held"
    let xs = [[s], ["x"]]
    let parts = [s, str(fail(n, s)?)]
    return parts.len()

fn tolerant(n: int):
    let s = "kept"
    let ys = [s]
    let parts = [s, str(fail(n, "t") else return)]
    print(f"{parts}")

fn caught(n: int) -> str:
    let held = ["a", "b"]
    let m = "none"
    let v = fail(n, "c") else err:
        match err:
            case Bad(text):
                m = text
        0
    return f"{m} {v} {held}"

fn main():
    print(str(relay(1) else -1))
    print(str(relay(0) else -1))
    tolerant(1)
    tolerant(0)
    print(caught(1))
    match fail(2, "m"):
        case Ok(v):
            print(str(v))
        case Err(Bad(msg, parts)):
            print(f"{msg} {parts}")
"#;
    assert_eq!(
        printed(main),
        lines(&[
            "-1",
            "2",
            r#"["kept", "0"]"#,
            r#"c! 0 ["a", "b"]"#,
            r#"m! ["m!", "b"]"#
        ])
    );
}
