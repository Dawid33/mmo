#! role=farmer=Person(3) role=dog=Creature(7) role=inn=Person(5)
# Dialogue examples §7: two scripts, one innkeeper — attached dialogues combine. The innkeeper of §6 belongs to
# the site's script; the lost-dog quest adds a topic to the same NPC while its stage is `Open`.
# Adapted for the stub: the stub runs one program, so the site's script is the module `site` of the quest's
# program (`site.qs`: §6 as in `dx6_innkeeper`); an added stage `Searching`, entered from `asked`, shows the
# topic leaving the menu when the stage moves on. The quest has no objectives to name.

mod site

# Package `errantry/lost-dog`, quest program
quest LostDog:
    role farmer: Person
    role dog: Creature where dog.has_tag("pet")
    role inn: Person where inn.has_tag("innkeeper")

    let pointed_to_mill: list[Player] = []

    event AskedAround:
        player: Player

    stage Open:
        dialogue dog_rumour(p: Player, npc: Person) for inn topic f"Seen a dog called {dog.name}?" priority 1:
            npc: "Muddy little thing? Ran past at dawn, toward the mill."
            emit AskedAround(player=p)
        @on(AskedAround)
        fn asked(e):
            pointed_to_mill.append(e.player)     # the mill scene can check this later
            -> Searching

    stage Searching:
        @on(Entered)
        fn searching(e):
            print(f"pointed to the mill: {pointed_to_mill.len()}")
