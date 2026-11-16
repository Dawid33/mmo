//! Code generation for references (`&place`, `*r`, auto-dereference) and `&self` methods, run end to end
//! through the stub host (plan Task 28).

mod common;

use common::run;
use quest_vm::FaultKind;
use quest_vm::host::{AbortCause, Report};

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

const LEDGER: &str = "struct Ledger:
    owner: str
    entries: list[int]

impl Ledger:
    fn total(self) -> int:
        let t = 0
        for e in self.entries:
            t = t + e
        return t

    fn add(&self, n):
        self.entries.append(n)

    fn bump(self) -> int:
        self.entries.append(100)
        return self.entries.len()
";

const QUEST: &str = "struct Reward:
    item: str
    count: int

struct Quest:
    name: str
    rewards: list[Reward]
";

#[test]
fn spec_5_3_reference() {
    let main = r#"fn main():
    let a = [1, 2]
    let b = a
    let r = &a
    r.append(9)
    assert_eq(a, [1, 2, 9])
    print(f"{a} {b} {r.len()}")
"#;
    assert_eq!(printed(main), lines(&["[1, 2, 9] [1, 2] 3"]));
}

#[test]
fn spec_5_5_add() {
    let main = format!(
        r#"{LEDGER}
fn main():
    let l = Ledger(owner="ana", entries=[])
    l.add(5)
    assert_eq(l.total(), 5)
    print(f"{{l.total()}} {{l.bump()}} {{l}}")
"#
    );
    assert_eq!(
        printed(&main),
        lines(&[r#"5 2 Ledger(owner="ana", entries=[5])"#])
    );
}

#[test]
fn reference_into_a_place() {
    let main = format!(
        r#"{QUEST}
fn main():
    let qu = Quest(name="q", rewards=[Reward(item="a", count=1), Reward(item="b", count=2), Reward(item="c", count=3)])
    let before = qu
    let r = &qu.rewards[2]
    r.count = r.count + 4
    r.item = r.item + "!"
    print(f"{{qu}}")
    print(f"{{before.rewards[2]}} {{r.count}}")
"#
    );
    assert_eq!(
        printed(&main),
        lines(&[
            r#"Quest(name="q", rewards=[Reward(item="a", count=1), Reward(item="b", count=2), Reward(item="c!", count=7)])"#,
            r#"Reward(item="c", count=3) 7"#,
        ])
    );
}

#[test]
fn reference_passed_down_two_calls() {
    let main = format!(
        r#"{QUEST}
fn inner(xs: &list[Reward], n: int):
    xs.append(Reward(item=f"r{{n}}", count=n))
    xs[0].count = xs[0].count + n

fn outer(xs: &list[Reward]):
    inner(xs, 1)
    inner(xs, xs.len())

fn main():
    let q = Quest(name="q", rewards=[Reward(item="a", count=0)])
    outer(&q.rewards)
    print(f"{{q.rewards}}")
"#
    );
    assert_eq!(
        printed(&main),
        lines(&[
            r#"[Reward(item="a", count=3), Reward(item="r1", count=1), Reward(item="r2", count=2)]"#
        ])
    );
}

#[test]
fn deref_assign() {
    let main = r#"fn set(r: &int, v: int):
    *r = v

fn main():
    let n = 1
    let r = &n
    *r = 7
    print(f"{n} {*r}")
    let xs = [1, 2, 3]
    let e = &xs[1]
    *e = *e * 10
    set(&xs[2], 8)
    set(r, n + 1)
    print(f"{xs} {n}")
"#;
    assert_eq!(printed(main), lines(&["7 7", "[1, 20, 8] 8"]));
}

#[test]
fn reference_to_a_top_level_variable_from_a_function() {
    let main = r#"let log = ["start"]

fn note(s: str):
    let r = &log
    r.append(s)

fn push_to(l: &list[str], s: str):
    l.append(s)

fn main():
    note("a")
    push_to(&log, "b")
    print(f"{log}")
"#;
    let out = run(&[("main.qs", main)]);
    assert!(out.reports.is_empty(), "{:?}", out.reports);
    assert_eq!(out.lines, lines(&[r#"["start", "a", "b"]"#]));
}

#[test]
fn bad_ref_fault_line() {
    let main = r#"fn main():
    let xs = [1, 2, 3]
    let r = &xs[2]
    xs.pop()
    print(f"{*r}")
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
            assert_eq!(*kind, FaultKind::BadRef);
            assert_eq!(stack[0].line, 5);
            assert_eq!(stack[0].file, "main.qs");
        }
        other => panic!("expected one fault report, got {other:?}"),
    }
}

#[test]
fn no_leaks_with_references() {
    let main = format!(
        r#"{LEDGER}
{QUEST}
enum Slot:
    Empty
    Held(item: str)

fn grow(l: &Ledger, n: int):
    l.add(n)
    l.add(l.total())

fn fill(s: &Slot, item: str):
    *s = Slot::Held(item=item)

fn main():
    let books = {{"ana": Ledger(owner="ana", entries=[1])}}
    let r = &books["ana"]
    grow(r, 2)
    r.entries[0] = 10
    books["ana"].add(4)
    let d = {{"k": ["x"]}}
    let v = &d["k"]
    v.append("y")
    *v = [v[1], "z"]
    d["j"] = ["new"]
    let q = Quest(name="q", rewards=[Reward(item="a", count=1)])
    let rw = &q.rewards
    rw.append(Reward(item="b", count=2))
    let names = []
    for w in rw:
        names.append(w.item)
    let s = Slot::Empty
    fill(&s, "sword")
    let rs = &s
    match rs:
        case Slot::Held(item):
            names.append(item)
        case Slot::Empty:
            pass
    let has = "y" in v
    print(f"{{books}} {{d}} {{names}} {{has}} {{rw[1:]}}")
"#
    );
    let out = run(&[("main.qs", &main)]);
    assert!(out.reports.is_empty(), "{:?}", out.reports);
    assert_eq!(
        out.lines,
        lines(&[
            r#"{"ana": Ledger(owner="ana", entries=[10, 2, 3, 4])} {"k": ["y", "z"], "j": ["new"]} ["a", "b", "sword"] true [Reward(item="b", count=2)]"#
        ])
    );
    assert_eq!(out.leaked_words, 0);
}
