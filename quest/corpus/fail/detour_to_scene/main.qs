# `detour-target`: a detour runs a conversation and comes back to the menu; a scene or a bark is not one.
scene argue(a: Person, b: Person):
    a: "No." @argue.no

dialogue talk(p: Player, npc: Person):
    npc: "Watch this." @talk.watch
    detour argue
