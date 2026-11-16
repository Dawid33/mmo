# Spec §5.8 (and plan decisions 12 and 18): list and dict literals and methods, indexing, slices (clamped),
# `in`, struct keys, insertion-ordered iteration, order-blind dict equality, copies, mutation while iterating,
# the string methods (`len()` counts characters; `to_upper` / `to_lower` are ASCII-only).
struct Pos:
    x: int
    y: int

fn main():
    let xs = [1, 2, 3]
    xs.append(4)
    xs.insert(0, 0)
    print(f"{xs} {xs.len()}")
    let last = xs.pop()
    print(f"{last} {xs}")
    let removed = xs.remove(1)
    print(f"{removed} {xs}")
    print(f"{xs[1:3]} {xs[0:10]} {xs[2:1]} {2 in xs} {7 in xs}")
    let empty: list[int] = []
    print(f"{empty.pop()}")
    let nested = [[1], [2]]
    let copy = nested
    copy[0].append(5)
    print(f"{nested} {copy}")
    let ys = [1, 2, 3]
    for y in ys:
        if y == 1:
            ys.append(10)
        print(str(y))
    let d = {"a": 1}
    d["b"] = 2
    d["a"] = 10
    let a = d["a"]
    print(f"{d} {d.len()} {a}")
    let got = d.get("b")
    let missing = d.get("zz")
    print(f"{got} {missing}")
    print(f"{d.keys()} {d.values()}")
    let gone = d.remove("a")
    let has_b = "b" in d
    print(f"{gone} {d} {has_b}")
    let e1 = {"a": 1, "b": 2}
    let e2 = {"b": 2, "a": 1}
    print(f"{e1 == e2} {e1 != e2}")
    for k, v in e2.items():
        print(f"{k}:{v}")
    let e3 = e1
    e3["c"] = 3
    print(f"{e1.len()} {e3.len()}")
    let grid = {Pos(x=0, y=0): "origin"}
    grid[Pos(x=1, y=2)] = "far"
    let o = grid[Pos(x=0, y=0)]
    print(f"{grid.len()} {o}")
    let s = "  Hello, World  "
    let t = s.trim()
    print(t)
    print(f"{t.len()} {t.to_upper()} {t.to_lower()}")
    let parts = t.split(", ")
    print(str(parts))
    let sw = t.starts_with("Hell")
    let ew = t.ends_with("x")
    let co = t.contains("o, W")
    print(f"{sw} {ew} {co}")
    let word = "straße"
    print(f"{word.len()} {word.to_upper()}")
    let n = parse_int("42")
    let bad = parse_int("4x")
    print(f"{n} {bad}")
    print("ab" + "cd")
