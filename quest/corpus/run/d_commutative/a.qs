# Module `a`: a plain greeting (score 0) and a topic.

dialogue hello(p: Player, npc: Person) for Person(id=5):
    npc: "Evening." @a.hello

dialogue rooms(p: Player, npc: Person) for Person(id=5) topic "Any rooms?" @a.rooms_topic:
    npc: "One, over the stable." @a.rooms
