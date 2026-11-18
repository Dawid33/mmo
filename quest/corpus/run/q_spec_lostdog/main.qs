#! role=farmer=Person(3) role=dog=Creature(7)
# Spec §5.12, the example: requires, roles (one optional, unbound here), stages, a topic, named objectives, a
# transition, `Entered`, a stage-scoped timer, a part on the dog, `complete()`.
# Adapted: the snippet handles `DogDelivered` without declaring it, and nothing in it finishes `feed_dog`;
# both events are declared here, with a `fed` handler in `Search`.

quest LostDog:
    requires act() >= 1
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    optional role miller: Person where miller.occupation == Occupation::Miller

    let attempts = 0

    event Accepted:
        player: Player
    event DogFound:
        player: Player
    event Nightfall:
        attempt: int
    event DogFed:
        player: Player
    event DogDelivered:
        player: Player

    stage Open:
        dialogue plea(p: Player, npc: Person) for farmer topic f"About {dog.name}" @lostdog.topic:
            npc: f"{dog.name} ran off toward the hills."                @lostdog.plea
            choice:
                "I'll find him." @lostdog.accept:
                    emit Accepted(player=p)
                "Not now." @lostdog.not_now
        @on(Accepted)
        fn accepted(e):
            join(e.player)
            -> Search

    stage Search:
        objective find_dog f"Find {dog.name}" @lostdog.find at dog
        objective feed_dog f"Coax {dog.name} with food" @lostdog.feed at dog
        optional objective ask_miller "Ask at the mill" @lostdog.mill at miller
        -> Returning when find_dog.done and feed_dog.done
        @on(Entered)
        fn count_attempt(e):
            attempts = attempts + 1
        @on(DogFound)
        fn found(e):
            if not find_dog.done:
                find_dog.finish()
                after(9000, Nightfall(attempt=attempts))
        @on(DogFed)
        fn fed(e):
            feed_dog.finish()
        @on(Nightfall)
        fn bolted(e):
            -> Search                       # a failed attempt: re-entering re-opens the objectives

    stage Returning:
        objective bring_home f"Bring {dog.name} home" @lostdog.return at farmer
        @on(DogDelivered)
        fn delivered(e):
            bring_home.finish()
            complete()                      # for every participant

    part dog_watch on dog:
        @on(PlayerNear)
        fn spotted(e):
            if stage == Stage::Search and e.player in participants:
                emit DogFound(player=e.player)
