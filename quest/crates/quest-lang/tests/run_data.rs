//! Code generation for structs, enums, collections, places and `match`, run end to end through the stub
//! host (plan Task 26).

mod common;

use common::run;

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
";

const STAGE: &str = "enum Stage:
    Asked
    Accepted(tick: int)
    Done
";

#[test]
fn spec_5_3_copy() {
    let main = r#"fn main():
    let a = [1, 2]
    let b = a
    b.append(3)
    print(f"{a} {b}")
"#;
    assert_eq!(printed(main), lines(&["[1, 2] [1, 2, 3]"]));
}

#[test]
fn spec_5_5_total() {
    let main = format!(
        r#"{LEDGER}
fn main():
    assert_eq(Ledger(owner="ana", entries=[2, 3]).total(), 5)
    let l = Ledger(owner="ana", entries=[2, 3])
    print(f"{{l.total()}} {{l}}")
"#
    );
    assert_eq!(
        printed(&main),
        lines(&[r#"5 Ledger(owner="ana", entries=[2, 3])"#])
    );
}

#[test]
fn nested_place_assignment() {
    let main = r#"struct Reward:
    item: str
    count: int

struct Quest:
    name: str
    rewards: list[Reward]

fn main():
    let q = Quest(name="q", rewards=[Reward(item="a", count=1), Reward(item="b", count=2), Reward(item="c", count=3)])
    let before = q
    q.rewards[2].count = 5
    print(f"{q.rewards[2].count} {before.rewards[2].count} {q == before}")
    q.rewards[0].item = q.rewards[1].item + q.rewards[0].item
    q.name = "renamed"
    print(f"{q.rewards[0]} {before.rewards[0]} {q.name} {before.name}")
"#;
    assert_eq!(
        printed(main),
        lines(&[
            "5 3 false",
            r#"Reward(item="ba", count=1) Reward(item="a", count=1) renamed q"#,
        ])
    );
}

#[test]
fn spec_5_6_match() {
    let main = format!(
        r#"{STAGE}
fn talk(stage: Stage):
    match stage:
        case Stage::Asked:
            print("Will you help?")
        case Stage::Accepted(tick) if tick > 100:
            print(f"Still waiting on you since {{tick}}.")
        case Stage::Accepted(_):
            print("Accepted.")
        case Stage::Done:
            print("Thanks again.")

fn main():
    talk(Stage::Asked)
    talk(Stage::Accepted(tick=150))
    talk(Stage::Accepted(5))
    talk(Stage::Done)
"#
    );
    assert_eq!(
        printed(&main),
        lines(&[
            "Will you help?",
            "Still waiting on you since 150.",
            "Accepted.",
            "Thanks again.",
        ])
    );
}

#[test]
fn dict_order() {
    let main = r#"fn main():
    let d = {"b": 1}
    d["a"] = 2
    d["c"] = 3
    d.remove("a")
    d["a"] = 4
    for k in d:
        print(k)
    for k, v in d.items():
        print(f"{k}={v}")
    print(f"{d.keys()} {d.values()} {d.len()}")
"#;
    assert_eq!(
        printed(main),
        lines(&[
            "b",
            "c",
            "a",
            "b=1",
            "c=3",
            "a=4",
            r#"["b", "c", "a"] [1, 3, 4] 3"#
        ])
    );
}

#[test]
fn mutation_during_iteration() {
    let main = r#"fn main():
    let xs = [1, 2, 3]
    for x in xs:
        if x == 1:
            xs.append(4)
        print(f"{x}")
"#;
    assert_eq!(printed(main), lines(&["1", "2", "3", "4"]));
}

#[test]
fn slices_and_in() {
    let main = r#"fn main():
    let xs = [10, 20, 30, 40]
    print(f"{xs[1:3]} {xs[:2]} {xs[2:]} {xs[:]} {xs[3:100]} {xs[3:1]}")
    let d = {"a": 1}
    let a = "a"
    let z = "z"
    print(f"{20 in xs} {25 in xs} {25 not in xs} {a in d} {z in d} {z not in d}")
"#;
    assert_eq!(
        printed(main),
        lines(&[
            "[20, 30] [10, 20] [30, 40] [10, 20, 30, 40] [40] []",
            "true false true true false true",
        ])
    );
}

#[test]
fn fstrings_of_every_kind() {
    let main = format!(
        r#"{LEDGER}
{STAGE}
fn main():
    let zero = 0.0
    print(f"{{42}} {{1.5}} {{2.0}} {{zero / zero}} {{1.0 / zero}} {{true}} {{()}}")
    let s = "raw"
    let names = ["a", "b"]
    let d = {{"a": 1}}
    print(f"{{s}} {{names}} {{[1, 2]}} {{d}}")
    let l = Ledger(owner="ana", entries=[])
    let n: Option[int] = None
    print(f"{{l}} {{Stage::Accepted(tick=5)}} {{Stage::Asked}} {{Some(3)}} {{n}}")
"#
    );
    assert_eq!(
        printed(&main),
        lines(&[
            "42 1.5 2.0 NaN inf true ()",
            r#"raw ["a", "b"] [1, 2] {"a": 1}"#,
            r#"Ledger(owner="ana", entries=[]) Stage::Accepted(tick=5) Stage::Asked Some(3) None"#,
        ])
    );
}

#[test]
fn structural_equality() {
    let main = r#"struct P:
    x: int
    name: str

fn main():
    let a = P(x=1, name="n")
    let b = P(x=1, name="n")
    let c = P(x=2, name="n")
    print(f"{a == b} {a != b} {a == c} {a != c}")
    print(f"{[a, b] == [b, a]} {[a] == [c]} {[1, 2] != [1, 2]} {[1, 2] != [2, 1]}")
    let e1 = {"a": 1, "b": 2}
    let e2 = {"b": 2, "a": 1}
    print(f"{e1 == e2}")
    let d = {a: "first"}
    d[b] = "second"
    print(f"{d.len()} {d[a]} {c in d} {b in d}")
"#;
    assert_eq!(
        printed(main),
        lines(&[
            "true false false true",
            "true false false true",
            "true",
            "1 second false true",
        ])
    );
}

#[test]
fn option_match() {
    let main = r#"fn describe(o: Option[int]) -> str:
    match o:
        case Some(n) if n > 10:
            return "big"
        case Some(n):
            return f"some {n}"
        case None:
            return "none"

fn main():
    print(describe(Some(50)))
    print(describe(Some(3)))
    print(describe(None))
    let d = {"a": 1}
    let a = "a"
    let z = "z"
    print(f"{d.get(a) else 0} {d.get(z) else 0}")
    let xs = ["x"]
    match xs.pop():
        case Some(s):
            print(f"popped {s}")
        case None:
            print("empty")
    match xs.pop():
        case Some(s):
            print(f"popped {s}")
        case None:
            print("empty")
"#;
    assert_eq!(
        printed(main),
        lines(&["big", "some 3", "none", "1 0", "popped x", "empty"])
    );
}

#[test]
fn no_leaks() {
    let main = r#"struct Item:
    name: str
    tags: list[str]

enum Slot:
    Empty
    Full(item: Item)

fn build(n: int) -> list[Item]:
    let out: list[Item] = []
    for i in range(n):
        let t = f"t{i}"
        out.append(Item(name=f"item{i}", tags=[t, t + "!"]))
        if i == 3:
            return out
    return out

fn label(s: Slot) -> str:
    match s:
        case Slot::Full(item) if item.tags.len() > 5:
            return "many"
        case Slot::Full(item):
            for t in item.tags:
                if t == "never":
                    return t
            return item.name
        case Slot::Empty:
            return "empty"

fn count(d: dict[str, list[Item]]) -> int:
    let n = 0
    for k, v in d.items():
        n = n + v.len()
    return n

fn main():
    let total = 0
    for it in build(3):
        let name = label(Slot::Full(item=it))
        total = total + name.len()
    for round in range(3):
        let d: dict[str, list[Item]] = {}
        let i = 0
        while i < 6:
            i = i + 1
            let items = build(i)
            d[f"k{i}"] = items
            if i == 2:
                continue
            for it in items:
                let s = Slot::Full(item=it)
                match s:
                    case Slot::Full(item) if item.name == "item1":
                        total = total + 1
                        break
                    case Slot::Full(item):
                        let text = item.tags[0] + item.name
                        if text.len() > 100:
                            return
                        if text == "t0item0":
                            continue
                        total = total + 10
                    case Slot::Empty:
                        pass
            if i == 5:
                break
        for k, v in d.items():
            let copy = v
            copy.append(Item(name=k, tags=[]))
            for it in copy:
                if it.name == k:
                    continue
                total = total + 100
        total = total + count(d)
        let first = d.get("k1") else []
        d["k3"][0].tags[1] = "changed"
        let gone = d.remove("k4")
        if round == 2:
            for k in d:
                if k == "k3":
                    print(f"{total}")
                    return
"#;
    let out = run(&[("main.qs", main)]);
    assert!(out.reports.is_empty(), "{:?}", out.reports);
    assert_eq!(out.lines.len(), 1, "{:?}", out.lines);
    assert_eq!(out.leaked_words, 0);
}
