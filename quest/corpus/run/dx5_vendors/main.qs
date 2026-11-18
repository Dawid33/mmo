# Dialogue examples §5: dialogue that would be code (Yarn Spinner's CustomActions-ItemVendors.yarn): a pure `fn`
# shared by every stall's conditions, one `Sell` handler.
# Adapted for the stub: `npc_named("Apple Seller")` is `Person(id=3)`, `npc_named("Orange Seller")`
# `Person(id=4)`.

event Sell:
    player: Player
    kind: str

fn has_room(p: Player, kind: str) -> bool:      # pure: usable in conditions
    return p.inventory_count(kind) < 10

@on(Sell)
fn sell(e):
    give(e.player, Item(kind=e.kind, count=1))

dialogue apple_stall(p: Player, npc: Person) for Person(id=3):
    when has_room(p, "apple"):
        emit Sell(player=p, kind="apple")
    when not has_room(p, "apple"):
        p: "That's too many apples."

dialogue orange_stall(p: Player, npc: Person) for Person(id=4):
    when has_room(p, "orange"):
        emit Sell(player=p, kind="orange")
    when not has_room(p, "orange"):
        p: "I've already got enough oranges."
