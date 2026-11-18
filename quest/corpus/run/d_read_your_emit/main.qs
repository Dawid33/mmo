# Spec §5.11 "Read-your-emit": an `emit`ted event is handled after the step runs, at the earliest the next
# time the instance runs; a condition or substitution later in the same batch still sees the old state. A menu
# built before the handler ran is stale: a pick of a topic that is no longer eligible is refused and the menu
# re-sent (quest examples §7, tick 1304).

let hal = Person(id=3)
let count = 0

event Bump:
    n: int

@on(Bump)
fn bump(e):
    count = count + e.n

dialogue chat(p: Player, npc: Person) for hal:
    npc: f"Count is {count}." @rye.count
    choice:
        "Bump it." @rye.bump:
            emit Bump(n=1)
            npc: f"Still {count}, in this batch." @rye.still
    npc: f"{count} after the choice, too." @rye.after

dialogue before(p: Player, npc: Person) for hal topic "While it is nought" @rye.before_topic if count == 0:
    npc: "It is nought." @rye.nought

dialogue always(p: Player, npc: Person) for hal topic "And now?" @rye.always_topic:
    npc: f"Now it is {count}." @rye.now
