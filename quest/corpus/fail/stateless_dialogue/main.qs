# Quest examples §6, the second fragment: package-level content (no quest) is stateless — it may not read a
# script global nor `emit`.

let rumour = "wolves"                                  # a global in a package with no quest

event Gossiped:
    player: Player

dialogue gossip(p: Player, npc: Person) for npc where npc.has_tag("gossip"):
    npc: f"They say {rumour}."                         # stateless-dialogue: reads a script global
    emit Gossiped(player=p)                            # stateless-dialogue: no instance to emit to
