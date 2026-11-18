# Package errantry/trade — src/haggle.qs   (a library: no quest)

pub dialogue haggle(p: Player, npc: Person) exits agreed, refused:
    npc: "Times are hard. You know how it is."
    choice:
        "Half now, half at harvest?" if p.skill("trade") >= 2:
            npc: "...Half now. Fine."
            return agreed
        "Pay, or I take it from your stock." if p.skill("intimidate") >= 3:
            npc: "All right! All right."
            return agreed
        "Then I'll be back.":
            return refused
    # falling off the end would be the implicit exit `done`; every option returns here
