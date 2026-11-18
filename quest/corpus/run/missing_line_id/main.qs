# The warning `missing-line-id` (spec §5.11 "Line ids"): a line without `@id` gets a provisional id and the
# program still runs (a package build would refuse it).

let hal = Person(id=3)

dialogue hello(p: Player, npc: Person) for hal:
    npc: "This line has an id." @hello.with_id
    npc: "This one has none."
