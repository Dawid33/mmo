# A fallible host query (Task 37b): `Person.distance_to(other)` raises `OutOfReach` when either has no
# position. Handled with `else` in a dialogue condition (a pure function may handle an error, never let it
# escape) and with `match` in a handler.

let hal = Person(id=3)

event Measure:
    other: Person

@on(Measure)
fn measure(e):
    match hal.distance_to(e.other):
        case Ok(d):
            print(f"{d} blocks")
        case Err(OutOfReach):
            print("out of reach")

dialogue hello(p: Player, npc: Person) for hal:
    node hi when (npc.distance_to(p) else 99) <= 4:
        npc: "You're close enough to hear me." @dist.close
    node hi:
        npc: "Come closer!" @dist.far
