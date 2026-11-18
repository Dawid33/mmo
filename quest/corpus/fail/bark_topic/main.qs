# `conversation-only`: a bark is said to nobody in particular, so it takes no `for`, filter or topic.
let smith = Person(id=3)

dialogue shout(npc: Person) for smith:
    npc: "Hey!" @shout.hey
