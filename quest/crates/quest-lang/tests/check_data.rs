//! Typing of structs, enums, collections, the built-in API and host calls (plan Task 21).

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

/// The diagnostics of a single `main.qs`.
fn main_diags(main: &str) -> Vec<(String, String, u32)> {
    diags(&[("main.qs", main)])
}

#[test]
fn struct_construction() {
    let main = r#"struct P:
    name: str
    age: int

fn by_name():
    return P(name="a", age=1)

fn by_position():
    return P("b", 2)

fn missing():
    let p = P(name="a")

fn unknown():
    let p = P(name="a", age=1, x=3)

fn wrong():
    let p = P(name=1, age=1)
"#;
    let files = [("main.qs", main)];
    assert_eq!(
        diags(&files),
        vec![
            d("missing-field", "main.qs", 12),
            d("type-mismatch", "main.qs", 18),
            d("unknown-field", "main.qs", 15),
        ]
    );
    let sigs = sigs(&files);
    assert_eq!(sigs["by_name"], "fn() -> P");
    assert_eq!(sigs["by_position"], "fn() -> P");

    let mut big = String::from("struct Big:\n");
    for i in 0..33 {
        big.push_str(&format!("    f{i}: int\n"));
    }
    assert_eq!(main_diags(&big), vec![d("too-many-fields", "main.qs", 1)]);
}

#[test]
fn methods() {
    // Spec §5.5.
    let main = r#"struct Ledger:
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

fn make() -> Ledger:
    return Ledger(owner="ana", entries=[])

fn main():
    let l = Ledger(owner="ana", entries=[])
    l.add(5)
    let t: int = l.total()
    let u: int = make().total()
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![]);
    let found = sigs(&files);
    assert_eq!(found["Ledger::add"], "fn(&self, int) -> ()");
    assert_eq!(found["Ledger::total"], "fn(self) -> int");

    let bad = format!("{main}    l.nope()\n    make().add(1)\n");
    assert_eq!(
        main_diags(&bad),
        vec![
            d("ref-not-place", "main.qs", 24),
            d("unknown-method", "main.qs", 23)
        ]
    );

    // A caller ordered before the method it calls (`a` < `z::S::get`) waits for it.
    let main = "mod z\n\nfn a():\n    let s = z::S(n=1)\n    return s.get()\n";
    let z = "pub struct S:\n    n: int\n\nimpl S:\n    pub fn get(self):\n        return self.n\n";
    let files = [("main.qs", main), ("z.qs", z)];
    assert_eq!(diags(&files), vec![]);
    assert_eq!(sigs(&files)["a"], "fn() -> int");
}

#[test]
fn recursive_types() {
    let main = r#"struct Node:
    v: int
    next: Option[Node]

enum Tree:
    Leaf
    Branch(kids: list[Tree])

struct A:
    b: Option[B]

struct B:
    a: Option[A]

fn main():
    let n = Node(v=1, next=Some(Node(v=2, next=None)))
    let t = Tree::Branch(kids=[Tree::Leaf])
    let a = A(b=Some(B(a=None)))
"#;
    assert_eq!(main_diags(main), vec![]);

    let files = [("main.qs", "struct Bad:\n    next: Bad\n")];
    assert_eq!(diags(&files), vec![d("infinite-type", "main.qs", 1)]);
    let found = messages(&files, "infinite-type");
    assert!(found[0].contains("Bad -> Bad"), "{found:#?}");

    let files = [("main.qs", "struct Y:\n    x: X\n\nstruct X:\n    y: Y\n")];
    assert_eq!(diags(&files), vec![d("infinite-type", "main.qs", 4)]);
    let found = messages(&files, "infinite-type");
    assert!(found[0].contains("X -> Y -> X"), "{found:#?}");
}

#[test]
fn enums() {
    let main = r#"enum Stage:
    Asked
    Accepted(tick: int)
    Done

fn by_name():
    return Stage::Accepted(tick=5)

fn by_position():
    return Stage::Accepted(5)

fn no_data():
    return Stage::Asked

fn none():
    let x: Option[int] = None
    return x

fn some():
    return Some(1)

fn wrong():
    let s = Stage::Accepted(tick="x")
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![d("type-mismatch", "main.qs", 23)]);
    let sigs = sigs(&files);
    assert_eq!(sigs["by_name"], "fn() -> Stage");
    assert_eq!(sigs["by_position"], "fn() -> Stage");
    assert_eq!(sigs["no_data"], "fn() -> Stage");
    assert_eq!(sigs["none"], "fn() -> Option[int]");
    assert_eq!(sigs["some"], "fn() -> Option[int]");
}

#[test]
fn collections() {
    let mismatches = [
        "fn main():\n    let xs = [1, \"a\"]\n",
        "fn main():\n    let x = {\"a\": 1}[0]\n",
        "fn main():\n    for x in 5:\n        pass\n",
        "fn main():\n    let b = \"a\" in [1]\n",
    ];
    for main in mismatches {
        assert_eq!(
            main_diags(main),
            vec![d("type-mismatch", "main.qs", 2)],
            "{main}"
        );
    }
    let bad_keys = [
        "fn main():\n    let d = {1.5: 1}\n",
        "fn main():\n    let d: dict[list[int], int] = {}\n",
    ];
    for main in bad_keys {
        assert_eq!(
            main_diags(main),
            vec![d("bad-dict-key", "main.qs", 2)],
            "{main}"
        );
    }

    let main = r#"struct K:
    a: int
    b: str

fn struct_key():
    return {K(a=1, b="x"): 1}

fn slice(xs: list[int]):
    return xs[0:1]

fn get(d: dict[str, int], k: str):
    return d.get(k)

fn len_str(s: str):
    return s.len()

fn len_list(xs: list[int]):
    return xs.len()

fn len_dict(d: dict[str, int]):
    return d.len()

fn in_list(xs: list[int]):
    return 1 in xs

fn not_in_dict(d: dict[str, int]):
    return "a" not in d

fn ranges():
    return range(1, 3)

fn items(d: dict[str, int]):
    let n = 0
    for k, v in d.items():
        let s: str = k
        n = n + v
    return n
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![]);
    let sigs = sigs(&files);
    assert_eq!(sigs["struct_key"], "fn() -> dict[K, int]");
    assert_eq!(sigs["slice"], "fn(list[int]) -> list[int]");
    assert_eq!(sigs["get"], "fn(dict[str, int], str) -> Option[int]");
    assert_eq!(sigs["len_str"], "fn(str) -> int");
    assert_eq!(sigs["len_list"], "fn(list[int]) -> int");
    assert_eq!(sigs["len_dict"], "fn(dict[str, int]) -> int");
    assert_eq!(sigs["in_list"], "fn(list[int]) -> bool");
    assert_eq!(sigs["not_in_dict"], "fn(dict[str, int]) -> bool");
    assert_eq!(sigs["ranges"], "fn() -> list[int]");
    assert_eq!(sigs["items"], "fn(dict[str, int]) -> int");

    // There is no free `len`.
    let files = [("main.qs", "fn f(xs: list[int]):\n    return len(xs)\n")];
    assert_eq!(diags(&files), vec![d("unresolved", "main.qs", 2)]);
    let all = diagnostics(&files);
    assert!(
        all[0].notes.iter().any(|n| n.contains(".len()")),
        "{all:#?}"
    );
}

#[test]
fn builtin_api() {
    let main = r#"fn f_print():
    return print("x")

fn f_range():
    return range(3)

fn f_float():
    return float(1)

fn f_int():
    return int(1.5)

fn f_str():
    return str([1])

fn f_parse_int():
    return parse_int("4")

fn f_abs_int():
    return abs(-1)

fn f_abs_float():
    return abs(-1.5)

fn f_min():
    return min(1, 2)

fn f_max():
    return max(1.5, 2.5)

fn f_sqrt():
    return sqrt(2.0)

fn f_floor():
    return floor(2.5)

fn f_ceil():
    return ceil(2.5)

fn f_round():
    return round(2.5)

fn f_sin():
    return sin(2.5)

fn f_cos():
    return cos(2.5)

fn f_pow():
    return pow(2.0, 3.0)

fn f_assert():
    return assert(true)

fn f_assert_eq():
    return assert_eq([1], [2])

fn f_all(ps: list[Promise[int]]):
    return all(ps)

fn l_len(xs: list[int]):
    return xs.len()

fn l_append(xs: list[int]):
    return xs.append(1)

fn l_pop(xs: list[int]):
    return xs.pop()

fn l_insert(xs: list[int]):
    return xs.insert(0, 1)

fn l_remove(xs: list[int]):
    return xs.remove(0)

fn l_in(xs: list[int]):
    return 1 in xs

fn l_index(xs: list[int]):
    return xs[0]

fn l_slice(xs: list[int]):
    return xs[1:]

fn d_len(d: dict[str, int]):
    return d.len()

fn d_index(d: dict[str, int]):
    return d["a"]

fn d_set(d: dict[str, int]):
    d["a"] = 1

fn d_get(d: dict[str, int]):
    return d.get("a")

fn d_remove(d: dict[str, int]):
    return d.remove("a")

fn d_in(d: dict[str, int]):
    return "a" in d

fn d_keys(d: dict[str, int]):
    return d.keys()

fn d_values(d: dict[str, int]):
    return d.values()

fn s_len(s: str):
    return s.len()

fn s_add(s: str):
    return s + "x"

fn s_split(s: str):
    return s.split(",")

fn s_starts_with(s: str):
    return s.starts_with("a")

fn s_ends_with(s: str):
    return s.ends_with("a")

fn s_contains(s: str):
    return s.contains("a")

fn s_trim(s: str):
    return s.trim()

fn s_to_upper(s: str):
    return s.to_upper()

fn s_to_lower(s: str):
    return s.to_lower()
"#;
    let files = [("main.qs", main)];
    assert_eq!(diags(&files), vec![]);
    let sigs = sigs(&files);
    let expected = [
        ("f_print", "fn() -> ()"),
        ("f_range", "fn() -> list[int]"),
        ("f_float", "fn() -> float"),
        ("f_int", "fn() -> int"),
        ("f_str", "fn() -> str"),
        ("f_parse_int", "fn() -> Option[int]"),
        ("f_abs_int", "fn() -> int"),
        ("f_abs_float", "fn() -> float"),
        ("f_min", "fn() -> int"),
        ("f_max", "fn() -> float"),
        ("f_sqrt", "fn() -> float"),
        ("f_floor", "fn() -> float"),
        ("f_ceil", "fn() -> float"),
        ("f_round", "fn() -> float"),
        ("f_sin", "fn() -> float"),
        ("f_cos", "fn() -> float"),
        ("f_pow", "fn() -> float"),
        ("f_assert", "fn() -> ()"),
        ("f_assert_eq", "fn() -> ()"),
        ("f_all", "fn(list[Promise[int]]) -> Promise[list[int]]"),
        ("l_len", "fn(list[int]) -> int"),
        ("l_append", "fn(list[int]) -> ()"),
        ("l_pop", "fn(list[int]) -> Option[int]"),
        ("l_insert", "fn(list[int]) -> ()"),
        ("l_remove", "fn(list[int]) -> int"),
        ("l_in", "fn(list[int]) -> bool"),
        ("l_index", "fn(list[int]) -> int"),
        ("l_slice", "fn(list[int]) -> list[int]"),
        ("d_len", "fn(dict[str, int]) -> int"),
        ("d_index", "fn(dict[str, int]) -> int"),
        ("d_set", "fn(dict[str, int]) -> ()"),
        ("d_get", "fn(dict[str, int]) -> Option[int]"),
        ("d_remove", "fn(dict[str, int]) -> Option[int]"),
        ("d_in", "fn(dict[str, int]) -> bool"),
        ("d_keys", "fn(dict[str, int]) -> list[str]"),
        ("d_values", "fn(dict[str, int]) -> list[int]"),
        ("s_len", "fn(str) -> int"),
        ("s_add", "fn(str) -> str"),
        ("s_split", "fn(str) -> list[str]"),
        ("s_starts_with", "fn(str) -> bool"),
        ("s_ends_with", "fn(str) -> bool"),
        ("s_contains", "fn(str) -> bool"),
        ("s_trim", "fn(str) -> str"),
        ("s_to_upper", "fn(str) -> str"),
        ("s_to_lower", "fn(str) -> str"),
    ];
    for (name, sig) in expected {
        assert_eq!(sigs[name], sig, "{name}");
    }

    // No character indexing on strings.
    let files = [("main.qs", "fn f(s: str):\n    return s[0]\n")];
    assert_eq!(diags(&files), vec![d("type-mismatch", "main.qs", 2)]);
    let found = messages(&files, "type-mismatch");
    assert!(found[0].contains("indexing"), "{found:#?}");
}

#[test]
fn fstrings_take_any_type() {
    let main = r#"struct P:
    x: int

enum E:
    A
    B(n: int)

fn main():
    let p = P(x=1)
    let xs = [1.5]
    let d = {"a": [1]}
    let o = Some(p)
    let e = E::B(2)
    print(f"{p} {xs} {d} {o} {e} {1} {true} {()} {xs.len()}")
"#;
    assert_eq!(main_diags(main), vec![]);
}

#[test]
fn host_calls() {
    let main = r#"@on(PlayerClicked)
fn h(e):
    say(e.player, 1)
    let n = e.nope

fn tag(p: Player):
    return p.has_tag("x")

fn rolls():
    return roll(1, 6)

fn counts():
    return chest.count("gold")

fn store():
    world.set("k", world.get("k"))

fn takes(p: Promise[int]):
    pass

fn promises():
    takes(world.add("k", 1))
    let p: Promise[int] = world.get("k")
    return [world.get("a"), world.set("b", 1)]
"#;
    let files = [("main.qs", main)];
    assert_eq!(
        diags(&files),
        vec![
            d("not-storable", "main.qs", 16),
            d("type-mismatch", "main.qs", 3),
            d("unknown-field", "main.qs", 4),
        ]
    );
    let sigs = sigs(&files);
    assert_eq!(sigs["tag"], "fn(Player) -> bool");
    assert_eq!(sigs["rolls"], "fn() -> int");
    assert_eq!(sigs["counts"], "fn() -> int");
    // A promise's error set is open: promises that meet share the union of their sets.
    assert_eq!(
        sigs["promises"],
        "fn() -> list[Promise[() raises Lost | NamespaceGone | NotFound | Timeout | WrongType]]"
    );
    assert_eq!(sigs["takes"], "fn(Promise[int]) -> ()");
}
