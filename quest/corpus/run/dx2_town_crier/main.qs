# Dialogue examples §2: barks, a hub and a detour (Yarn Spinner's TownCrier.yarn), with line ids.
# Adapted for the stub: `npc_named("Town Crier")` is `Person(id=8)`; the host's `Ambient` event is declared
# here as a script event (the stub has none), an `event DragonSlain` with its handler lets the script
# change `dragon_dead`, and an `ended` handler prints the `DialogueEnded` the runner raises.

let dragon_dead = false
let king_new_advisor = false
let crier = Person(id=8)

event Ambient:
    npc: Person

event DragonSlain:
    by: Player

@on(DragonSlain)
fn slain(e):
    dragon_dead = true

dialogue crier_bark(npc: Person):                   # a bark: no player, no choices
    => npc: "Hear ye, hear ye!"                     @crier.bark.hear_ye
    => npc: "The king has a new advisor!"           @crier.bark.advisor when king_new_advisor
    => npc: "A dragon stalks the village!"          @crier.bark.dragon when not dragon_dead
    => npc: "The fearsome dragon is slain!"         @crier.bark.slain when dragon_dead

dialogue crier_talk(p: Player, npc: Person) for crier:
    node start:
        p: "Hello."                                 @crier.hello
        npc: "Hello, citizen!"                      @crier.hello_back
        -> hub
    node hub:
        choice:
            "What's the latest news?"               @crier.ask_news:
                detour crier_news
            "Goodbye."                              @crier.goodbye:
                npc: "Farewell!"                    @crier.farewell
                return                              # Yarn's <<stop>>
        -> hub

dialogue crier_news(p: Player, npc: Person):
    node news when dragon_dead:
        npc: "The beast that stalked the land of late is slain by a valiant adventurer!"  @crier.news.slain
    node news when not dragon_dead:
        npc: "A fearsome dragon stalks the village from the mountains!"                   @crier.news.dragon
        npc: "Will a noble hero save us from this fell beast?"                            @crier.news.hero

@on(Ambient)
fn call_out(e):
    if e.npc == crier:
        bark(e.npc, crier_bark)                     # a dialogue's name is a Dialogue value

@on(DialogueEnded)
fn ended(e):
    print(f"DialogueEnded {e.reason}")
