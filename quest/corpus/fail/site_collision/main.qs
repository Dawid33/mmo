# `site-collision`: `once` records are keyed by a site anchored on line ids (they survive edits); `once: once:`
# with the same first line anchors both on one id.

let hal = Person(id=3)

dialogue hello(p: Player, npc: Person) for hal:
    once:
        once:
            npc: "Only ever once." @hello.once
