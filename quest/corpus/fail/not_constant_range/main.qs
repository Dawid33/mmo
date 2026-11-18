# `not-constant`: a choice's timeout, a `pause`, a `(ticks=…)` or an objective's `count` takes an int constant
# (a literal, or a `let` of one) in range — a `count` of 1 or more.

let n = 5

dialogue ask(p: Player, npc: Person) for Person(id=3):
    npc: "Quick!" @ask.quick
    choice(timeout=n + 1):
        "Yes." @ask.yes

quest Q:
    role keeper: Person
    stage S:
        objective coins "Collect coins" @q.coins at keeper count 0
