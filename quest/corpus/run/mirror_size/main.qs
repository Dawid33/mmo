# The warning `mirror-size` (spec §5.11 "Mirror size"): a condition reading a global of unbounded size (a
# `list`) puts all of it in the dialogue read set, mirrored whole on every change. The program still runs.

let hal = Person(id=3)
let regulars: list[int] = [1]

dialogue hello(p: Player, npc: Person) for hal:
    node hi when p.id in regulars:
        npc: "The usual?" @mirror.usual
    node hi:
        npc: "What'll it be?" @mirror.new
