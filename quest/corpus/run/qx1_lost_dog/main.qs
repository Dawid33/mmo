#! role=farmer=Person(3) role=dog=Creature(7) role=miller=Person(4)
# Quest examples §1: The Lost Dog — one-off, a home and an entity part (`dog_watch` on the dog), two players,
# a bolt at nightfall and a retry; a double sighting in one tick sets one timer (one more bolt, not two). Its opening is quest examples §7's table (batch 1: the greeting and the
# menu; batch 2: the plea; batch 3: the hint and the stale menu; the stale pick refused — see script.txt).
# Adapted: `NIGHT_TICKS` is 300, not 9000 (the golden holds a line per tick).

# Package errantry/lost-dog — src/main.qs

let NIGHT_TICKS = 300                   # from finding the dog to it bolting, unless fed

quest LostDog:
    requires act() >= 1
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    optional role miller: Person where miller.occupation == Occupation::Miller and near(miller, farmer, regions=1)

    let attempts = 0                    # quest state: how often the dog was found and lost again

    event Accepted:
        player: Player
    event AskedMiller:
        player: Player
    event DogFound:
        player: Player
    event DogFed:
        player: Player
    event Nightfall:
        attempt: int
    event Reunited:
        player: Player

    # ── Open: the farmer asks anyone who talks to him ─────────────────────────
    stage Open:
        dialogue worry(p: Player, npc: Person) for farmer:          # a greeting: no topic
            node hi when once per p:
                npc: "Stranger! Have you seen a brown dog on the road?"         @lostdog.worry.first
            node hi:
                => npc: "Still no sign of him."                                 @lostdog.worry.a
                => npc: "I left the gate open. My own fault."                   @lostdog.worry.b

        dialogue plea(p: Player, npc: Person) for farmer topic f"About {dog.name}" @lostdog.topic:
            npc: f"{dog.name} ran off toward the hills two days ago."           @lostdog.plea
            npc: "He's never been gone a night before."                         @lostdog.plea2
            choice:
                "I'll find him."                                                @lostdog.accept:
                    emit Accepted(player=p)
                    npc: "Bless you. Take something to eat. He's skittish."     @lostdog.hint
                "What does he look like?"                                       @lostdog.ask_look:
                    npc: "Brown, one ear up. Answers to his name, mostly."      @lostdog.look
                "Not now."                                                      @lostdog.not_now

        @on(Accepted)
        fn accepted(e):
            join(e.player)
            -> Search

    # ── Search: two objectives in parallel, one optional ─────────────────────
    stage Search:
        objective find_dog f"Find {dog.name}"                    @lostdog.find     at dog
        objective feed_dog f"Coax {dog.name} with food"          @lostdog.feed     at dog
        optional objective ask_miller "Ask at the mill"          @lostdog.ask_mill at miller
        -> Returning when find_dog.done and feed_dog.done

        dialogue join_in(p: Player, npc: Person) for farmer topic "Can I help look?" @lostdog.join if not (p in participants):
            npc: "Every pair of eyes helps."                                    @lostdog.join_ok
            emit Accepted(player=p)

        dialogue lost_again(p: Player, npc: Person) for farmer:
            node hi when attempts >= 2:
                npc: f"Found and lost {attempts} times now. He's frightened."   @lostdog.again
            node hi:
                npc: "Any luck?"                                                @lostdog.any_luck

        dialogue tip(p: Player, npc: Person) for miller topic f"Seen {dog.name}?" @lostdog.mill_topic if p in participants:
            npc: "Something's been at my grain sacks down by the weir."         @lostdog.mill_tip
            emit AskedMiller(player=p)

        @on(Entered)
        fn count_attempt(e):
            attempts = attempts + 1

        @on(Accepted)
        fn joined_late(e):
            join(e.player)

        @on(AskedMiller)
        fn asked(e):
            ask_miller.finish()

        @on(DogFound)
        fn found(e):
            if not find_dog.done:       # two players may spot him in one tick
                find_dog.finish()
                after(NIGHT_TICKS, Nightfall(attempt=attempts))

        @on(DogFed)
        fn fed(e):
            if find_dog.done:
                feed_dog.finish()
                follow(dog, e.player)

        @on(Nightfall)
        fn bolted(e):
            -> Search                   # a failed attempt: re-entering re-opens both objectives

    # ── Returning: bring him home ────────────────────────────────────────────
    stage Returning:
        objective bring_home f"Bring {dog.name} home to {farmer.name}"   @lostdog.return at farmer

        dialogue reunion(p: Player, npc: Person) for farmer topic f"{dog.name} is back" @lostdog.back_topic if near(dog, farmer, blocks=8):
            npc: f"{dog.name}! You daft mutt."                                  @lostdog.reunion
            npc: "Take this. My father carved it. It's all I have of worth."    @lostdog.reward
            emit Reunited(player=p)

        @on(Reunited)
        fn reunited(e):
            bring_home.finish()
            for q in participants:
                give(q, Item(kind="carved_whistle", count=1))
            complete()                  # for every participant

    # ── In any stage ─────────────────────────────────────────────────────────
    @on(RoleLost)
    fn lost(e):
        match e.role:
            case Role::miller:
                pass                    # optional: the tip is simply gone
            case _:
                fail()                  # the farmer or the dog: the quest cannot be done

    # ── The part that rides with the dog ─────────────────────────────────────
    part dog_watch on dog:
        @on(PlayerNear)
        fn spotted(e):
            if stage == Stage::Search and e.player in participants and not find_dog.done:
                emit DogFound(player=e.player)

        @on(ItemGiven)
        fn offered(e):
            if stage == Stage::Search and e.item.kind == "meat":
                emit DogFed(player=e.player)
