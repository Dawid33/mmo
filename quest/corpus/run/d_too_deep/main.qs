# Spec §5.11: a return stack deeper than `DIALOGUE_STACK_MAX` (16) ends the conversation `TooDeep`. `deeper`
# detours into itself with no bound (example 6's `tale` without its `dialogue_depth()` guard).

let hal = Person(id=3)

dialogue deeper(p: Player, npc: Person) for hal:
    npc: "Down we go." @deep.down
    detour deeper

@on(DialogueEnded)
fn ended(e):
    print(f"DialogueEnded {e.reason} {e.exit}")
