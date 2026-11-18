# `impure-condition`: a condition is a pure function — no host command, no assignment to a global, no `await`.

let seen = 0
let hal = Person(id=3)

fn noisy(p: Player) -> bool:
    give(p, Item(kind="coin", count=1))
    return true

fn counting() -> bool:
    seen = seen + 1
    return true

dialogue hello(p: Player, npc: Person) for hal:
    node hi when noisy(p):
        npc: "Have a coin." @impure.coin
    node hi when counting():
        npc: "Counted." @impure.counted
