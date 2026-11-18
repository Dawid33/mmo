# Quest examples §6, the third fragment: a detour arm naming an exit the callee does not declare.

let trader = Person(id=3)

dialogue haggle(p: Player, npc: Person) exits agreed, refused:
    npc: "Well?" @haggle.well
    choice:
        "Yes." @haggle.yes:
            return agreed
        "No." @haggle.no:
            return refused

dialogue offer(p: Player, npc: Person) for trader:
    detour haggle:
        agreed:
            npc: "Done." @offer.done
        cheated:                                       # unknown-exit: haggle exits agreed, refused
            npc: "Hey!" @offer.hey
