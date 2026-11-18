# `choice-defaults`: a timeout takes one default, so a choice has at most one (`default "…"` or `default:`).
dialogue talk(p: Player, npc: Person):
    npc: "Well?" @talk.well
    choice(timeout=100):
        default "Yes." @talk.yes
        "No." @talk.no
        default:
            npc: "Too slow." @talk.slow
