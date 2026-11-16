use crate::Ledger

pub fn give_gold(p: Player, n: int):
    give(p, Item(kind="gold", count=n))

pub fn total(l: Ledger) -> int:
    let t = 0
    for e in l.entries:
        t = t + e
    return t + bonus()

fn bonus() -> int:
    return 100
