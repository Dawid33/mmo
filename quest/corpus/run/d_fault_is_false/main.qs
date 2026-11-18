# Spec §5.11 "Purity": a condition that faults, or runs out of `COND_FUEL`, counts as false, and a
# substitution that faults is empty; each is reported to the owner. The conversation goes on.

let hal = Person(id=3)
let zero = 0

fn spins() -> bool:
    let i = 0
    while true:
        i = i + 1
    return i > 0

dialogue hello(p: Player, npc: Person) for hal:
    node hi when 10 / zero == 1:
        npc: "Never said: the condition divides by zero." @fault.div
    node hi when spins():
        npc: "Never said: the condition runs out of fuel." @fault.fuel
    node hi:
        npc: f"Hello. Ten in {zero} parts is {10 / zero}." @fault.subst
