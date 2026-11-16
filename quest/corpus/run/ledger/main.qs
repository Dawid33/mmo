# Spec §5.5: struct + impl, `self` by value (a copy), `&self` mutates.
# Adapted: the spec's top-level `l.add(5)` is a statement, and the top level holds only items and `let`,
# so the calls are in `main`; `l` stays a top-level (persistent) `let`.
struct Ledger:
    owner: str
    entries: list[int]

impl Ledger:
    fn total(self) -> int:          # self by value (a COW copy — cheap)
        let t = 0
        for e in self.entries:
            t = t + e
        return t

    fn add(&self, n):               # &self: may mutate
        self.entries.append(n)

    fn with_extra(self) -> int:
        self.entries.append(100)
        return self.total()

let l = Ledger(owner="ana", entries=[])

fn main():
    l.add(5)
    l.add(7)
    print(str(l.total()))
    print(str(l.with_extra()))
    print(f"{l}")
