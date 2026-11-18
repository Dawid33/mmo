# Module `b`: a `once per p` greeting (score 1) and a topic.

dialogue first_time(p: Player, npc: Person) for Person(id=5):
    node hi when once per p:
        npc: "First time here?" @b.first

dialogue ale(p: Player, npc: Person) for Person(id=5) topic "An ale." @b.ale_topic:
    npc: "Coming up." @b.ale
