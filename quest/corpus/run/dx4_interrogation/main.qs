# Dialogue examples §4: speakers in scope, scenario state (Yarn Spinner's Room-Interrogation.yarn).
# Adapted for the stub: `npc_named("Alice")` is `Person(id=3)`, `npc_named("Bob")` `Person(id=4)`; the
# comment on the first node counts its score by conjuncts (the fold), not by operators.

enum ScenarioState:
    NotStarted
    Started
    Complete

enum Room:
    Mansion
    Pub
    Office
    Church

let state = ScenarioState::NotStarted
let room = Room::Mansion
let primary = Person(id=3)
let secondary = Person(id=4)

event Advance:
    to: ScenarioState

@on(Advance)
fn advance(e):
    state = e.to

dialogue primary_talk(p: Player, npc: Person) for primary:
    # Alice's opening scores once 1 + two conjuncts 2 = 3 and wins over the generic opening (1).
    node talk when once if npc.name == "Alice" and state == ScenarioState::NotStarted:
        npc: f"So it's time to break {secondary.name}."
        p: "oh?"
        npc: "yeah they are real twisted and I will need your help"
        p: "oh cool"
        npc: "agreed, anyways, talk to me again please and we'll discuss strategy."
    node talk when state == ScenarioState::NotStarted:
        npc: "ok you be good cop, I'll be bad"
        p: "yep"
        emit Advance(to=ScenarioState::Started)
    node talk when state == ScenarioState::Started:
        npc: "you are in real trouble now"
        secondary: "Oh am I now!?"
        => p: "it's weird we're in a mansion for this right?" when room == Room::Mansion
        => p: "anyone fancy a pint?" when room == Room::Pub
        => p: "this feels like the right place for this" when room == Room::Office
        => p: "I reckon the symbolism here is confusing" when room == Room::Church
        npc: "quiet for a second please"
        p: "want to go see Les Mis?"
        secondary: "huh?"
        npc: "Yeah, what?"
        p: "anyone fancy an icecream?, I could go pick us some up."
        secondary: "I confess, just stop being so weird."
        emit Advance(to=ScenarioState::Complete)
    node talk when state == ScenarioState::Complete:
        npc: "look I am not sure what you were doing but wow did it ever work."
        p: "thanks"

dialogue secondary_talk(p: Player, npc: Person) for secondary:
    node talk when state == ScenarioState::Complete:
        => npc: "I promise I'll be good, please don't Henry the Train me."
        => npc: "I'm done hiding the corpses of my masters victims." when room == Room::Mansion
        => npc: "I'm all Jesused up now, I swear." when room == Room::Church
        => npc: f"I promise I'll never eat {primary.name}'s lunch again." when room == Room::Office
        => npc: "I promise I'll no more cheating at darts." when room == Room::Pub
    node talk:
        npc: "Why are you talking to me?"
