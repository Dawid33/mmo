#! role=watch=Person(3) role=camp=Place(9)
# Quest examples §3: The Bandit Camp — repeatable, exclusive, `at camp`. Four completions: the namespace's
# `times_beaten` survives the resets, and the fourth `Setup` places the veteran camp (and `hire` says so). The
# first run also shows a second party hiring on in the same tick, a failed attempt (everyone down: back to
# `Notice`) and the optional banner (35 silver).
# Spec/example discrepancy: the example says a second party's `hire` leads to a refused `join` (`exclusive`).
# But `hired` is a handler of the stage `Notice`, and the first `Hired` moves the quest to `Assault`: a second
# `Hired` (Bo's, emitted in the same tick as Ana's) finds no handler and is dropped — no `join` is tried, so
# nothing is refused and Bo is told nothing.
# Adapted: `cooldown` is 100, not 72000 (the golden holds a line per tick); `await world.add(...)` in `pay_out`
# is unhandled as written (`unhandled-fallible`) and is `let _ = await world.add(...) else 0` here (a failed
# count is not worth aborting the payout).

# Package errantry/bandits — src/main.qs

repeatable quest BanditCamp(cooldown=100, exclusive=true) at camp:
    role watch: Person where watch.occupation == Occupation::Guard and watch.alive
    role camp: Place where camp.open_ground(blocks=20) and near(camp, watch, regions=1)

    let veteran = false                 # re-initialised on every reset; set from the namespace in Setup
    let paid: list[Player] = []

    event Hired:
        player: Player
    event Paid:
        player: Player

    @on(Setup)
    fn pitch_camp(e):
        let beaten = await world.get("times_beaten") else 0     # the namespace survives resets
        veteran = beaten >= 3
        let camp_prefab = prefab("bandit_camp")
        if veteran:
            camp_prefab = prefab("bandit_camp_veteran")
        place(camp_prefab, camp) else:
            decline()
            return

    stage Notice:
        dialogue hire(p: Player, npc: Person) for watch topic "Any work going?":
            when veteran:
                npc: "Them again. Better blades this time, I hear."
            npc: "Bandits in the hollow. Clear them out and the village pays thirty silver."
            choice:
                "Consider it done.":
                    emit Hired(player=p)
                "Not today."

        @on(Hired)
        fn hired(e):
            join(e.player)
            -> Assault

    stage Assault:
        objective bandits "Drive the bandits from the hollow" at camp count 5
        optional objective banner "Take their banner" at camp
        -> Payday when bandits.done

        @on(Killed)
        fn killed(e):
            if ours(e.victim):
                bandits.progress(1)

        @on(PlayerDefeated)
        fn down(e):
            leave(e.player)
            if participants.len() == 0:
                -> Notice               # everyone is down: the attempt failed; the job is open again

        @on(BlockUsed)
        fn banner_taken(e):
            if e.tag == "bandit_banner":
                banner.finish()

    stage Payday:
        dialogue pay(p: Player, npc: Person) for watch topic "The hollow is clear." if p in participants and not (p in paid):
            npc: "Good work. Thirty, as promised."
            when banner.done:
                npc: "And the banner? The reeve will want that. Five more."
            emit Paid(player=p)

        @on(Paid)
        fn pay_out(e):
            let silver = 30
            if banner.done:
                silver = 35
            give(e.player, Item(kind="silver", count=silver))
            paid.append(e.player)
            if paid.len() == participants.len():
                let _ = await world.add("times_beaten", 1) else 0
                complete()
