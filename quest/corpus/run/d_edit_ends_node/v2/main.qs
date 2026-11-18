# The edited program of `d_edit_ends_node`: the node `second` is gone; `stay`'s farewell is new.

let hal = Person(id=3)
let mara = Person(id=5)

dialogue walk(p: Player, npc: Person) for hal:
    node first:
        npc: "First." @walk.first
        choice:
            "Stay." @walk.stay
        npc: "Still first." @walk.still

dialogue stay(p: Player, npc: Person) for mara:
    node first:
        npc: "Hello." @stay.hello
        choice:
            "Go on." @stay.go_on
        npc: "Farewell, then." @stay.farewell

@on(DialogueEnded)
fn ended(e):
    print(f"DialogueEnded {e.player.name} {e.reason}")
