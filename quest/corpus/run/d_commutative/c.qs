# Module `c`: a plain greeting (score 0, tied with `a`'s), a filter topic for every innkeeper (priority 1), and a
# topic attached by a handler.

event Open:
    npc: Person

@on(Open)
fn open(e):
    attach_dialogue(e.npc, news)

dialogue gossip(p: Player, npc: Person) for npc where npc.has_tag("innkeeper") topic "Gossip?" @c.gossip_topic priority 1:
    npc: "Not from me." @c.gossip

dialogue news(p: Player, npc: Person) topic "News?" @c.news_topic:
    npc: "The mill burned." @c.news

dialogue evening(p: Player, npc: Person) for Person(id=5):
    npc: "Evening, love." @c.evening
