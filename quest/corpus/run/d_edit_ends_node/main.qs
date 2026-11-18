# Spec §5.11 "Edits": the dialogue data is replaced; a conversation whose current node still exists continues
# there, one whose node is gone ends (`Edited`). The script's `edit v2` replaces this program with `v2/main.qs`,
# which drops the node `second` and changes a line of `first`.

let hal = Person(id=3)
let mara = Person(id=5)

dialogue walk(p: Player, npc: Person) for hal:
    node first:
        npc: "First." @walk.first
        choice:
            "On." @walk.on:
                -> second
            "Stay." @walk.stay
        npc: "Still first." @walk.still
    node second:
        npc: "Second." @walk.second
        choice:
            "Bye." @walk.bye

dialogue stay(p: Player, npc: Person) for mara:
    node first:
        npc: "Hello." @stay.hello
        choice:
            "Go on." @stay.go_on
        npc: "Goodbye." @stay.goodbye

@on(DialogueEnded)
fn ended(e):
    print(f"DialogueEnded {e.player.name} {e.reason}")
