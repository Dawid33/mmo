# `duplicate-line-id`: line ids are unique per package.

let hal = Person(id=3)

dialogue hello(p: Player, npc: Person) for hal:
    npc: "Hello." @greet.hello
    npc: "Hello again." @greet.hello
