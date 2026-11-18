# Spec §5.11, the first example: a dialogue with a choice, an option guard, a `once` option, a jump back to
# the node, a detour, and an `emit` whose handler changes the state the guard reads.
# Adapted: the snippet's `farmer` is a top-level `let` (`Person(id=3)`), and the detour's target
# `farmer_secret`, which the snippet does not show, is a short dialogue here.
# The snippet warns `mirror-size` as written: the substitution reads the `str` global `dog_name`, which
# counts as unbounded (spec §5.11 "Mirror size").

let accepted = false
let dog_name = "Rufus"
let farmer = Person(id=3)

event Accepted:
    player: Player

@pick(best)
dialogue farmer_talk(p: Player, npc: Person) for farmer:
    node start:
        npc: "Have you seen my dog? He ran off toward the hills."     @farmer.start
        choice:
            "I'll find him." @farmer.accept if not accepted:
                emit Accepted(player=p)
                npc: "Bless you. Bring him back before dark."          @farmer.thanks
            "What does he look like?" @farmer.ask_dog:
                npc: f"Brown, one ear up. He answers to {dog_name}."   @farmer.dog_desc
                -> start
            once "Is that blood on your sleeve?" @farmer.blood:
                detour farmer_secret
            "Goodbye." @farmer.goodbye
        npc: "Safe roads."                                             @farmer.bye

dialogue farmer_secret(p: Player, npc: Person):
    npc: "Chicken. Only chicken."                                      @farmer.secret

@on(Accepted)
fn accept(e):
    accepted = true
    give(e.player, Item(kind="dog whistle", count=1))
