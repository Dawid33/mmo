# Quest examples §5: Town life — package content with no quest: a stateless filter dialogue for every smith,
# an ambient scene cast from idle people with a follow-up, an interrupted scene, a one-actor scene (the
# crier's bark) picking its act-specific line. As written.

# Package errantry/town-life — src/main.qs   (no quest: package-level content)

dialogue smith_hello(p: Player, npc: Person) for npc where npc.occupation == Occupation::Smith:
    node hello when once per p:
        npc: "Mind the sparks. New faces always stand too close."
    node hello when p.inventory_count("broken_tool") > 0:
        npc: "That's a sorry tool you're carrying. Leave it with me."
    node hello:
        => npc: "Hot work, this."
        => npc: "Need something mended?"
        => npc: "Iron's dear since the mine closed." when act() >= 3

scene market_haggle(a: Person, b: Person) where a.occupation == Occupation::Merchant and b.site == a.site and a != b:
    b: "How much for the turnips?"
    a: "Two coppers."
    b: "Two? They're half rot."
    pause 20
    a: "Then it's one copper of turnip and one of rot."                     (ticks=90)
    -> market_haggle_after

scene market_haggle_after(a: Person, b: Person):
    pause 40
    b: "...Fine. Two."

scene crier(npc: Person) where npc.has_tag("town_crier"):                 # a one-actor scene: a bark
    => npc: "Hear ye, hear ye!"
    => npc: "The hollow road is closed by order of the reeve!" when act() == 1
    => npc: "The old mine is sealed again! Praise the stranger!" when completed("errantry/act-two::SealedMine")
