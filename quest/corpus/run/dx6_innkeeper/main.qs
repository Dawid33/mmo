# Dialogue examples §6: an Errantry innkeeper (original): quest-aware rumours, an act-gated `once` option,
# `once per p` greetings, a hub, and a dialogue that detours into itself, bounded by `dialogue_depth()`.
# Adapted for the stub: `npc_named("Mara")` is `Person(id=5)`; the stub has no `here()`, so the greeting
# names no site ("Welcome to the inn."); `quest_open(id)` is a pure fn over the fact query `completed` (a
# quest counts as open until it is completed).
# Spec/example discrepancy: the example's prose says asking again "nests up to three tellings inside the first".
# `tale` is reached by a detour from `innkeeper`, so the first telling is at depth 2 and `dialogue_depth() < 4`
# offers the choice at depths 2 and 3 only: three tellings in all, two inside the first (see the golden).

let keeper = Person(id=5)

fn quest_open(id: str) -> bool:
    return not completed(f"errantry/{id}")

dialogue innkeeper(p: Player, npc: Person) for keeper:
    node greet when once per p:
        npc: "New face! Welcome to the inn."
        -> menu
    node greet when p.has_tag("outlaw"):
        npc: "Coin first. Then we talk."
        -> menu
    node greet:
        => npc: "Back again?"
        => npc: "Mind the step, it's loose."
        => npc: "Sit anywhere that isn't wet."
        -> menu
    node menu:
        choice:
            "Heard any rumours?":
                detour rumours
            "Tell me a story.":
                detour tale
            once "Who really runs this town?" if act() >= 2:
                npc: "Not the mayor. Ask about the ledger at the mill."
            "Goodbye.":
                npc: "Safe roads."
                return
        -> menu

dialogue rumours(p: Player, npc: Person):
    => npc: "A farmer up the hill lost his dog. Offering a reward, too." when quest_open("lost_dog")
    => npc: "They sealed the old mine again. Third time this year." when quest_open("sealed_mine")
    => npc: "Quiet week. Too quiet."

dialogue tale(p: Player, npc: Person):
    npc: "Once, a traveller sat in an inn much like this one, and the keeper told a tale..."
    when dialogue_depth() < 4:
        choice:
            "And what was that tale?":
                detour tale                  # the dialogue detours into itself
            "Skip to the end."               # no steps: continue after the choice
    npc: "...and when the tale was done, the traveller went on their way."
