# `bark-choice`: a bark has no listener, so no choice.

dialogue shout(npc: Person):
    npc: "Hey!" @shout.hey
    choice:
        "What?" @shout.what
