# `dialogue-signature`: a conversation takes `(p: Player, npc: Person)`, a bark `(npc: Person)`; nothing else.

dialogue odd(npc: Person, p: Player):
    npc: "Backwards." @odd.a

dialogue lone(p: Player):
    p: "Nobody to talk to." @lone.a
