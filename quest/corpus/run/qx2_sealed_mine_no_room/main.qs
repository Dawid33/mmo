#! role=elder=Person(3) role=foreman=Person(4) role=mine=Site(9)
# Quest examples §2: The Sealed Mine — `quest … at elder`, the part `depths at mine` with its own `Setup` (the
# nest placed), the uninterruptible staged scene (`pause 30`, `(ticks=60)`), `SceneEnded`, a counter objective
# counting only `ours` kills by a player. As written.
# Here the mine has no open ground: `place` fails with `NoRoom`, the part `decline()`s, and the whole quest is
# not placed.

# Package errantry/act-two — src/sealed_mine.qs

quest SealedMine at elder:
    requires act() == 2
    role elder: Person where elder.occupation == Occupation::Elder and elder.alive
    role foreman: Person where foreman.occupation == Occupation::Miner and foreman.site == elder.site
    role mine: Site where site_kind(mine) == SiteKind::Mine and near(mine, elder, regions=2)

    event HeardOut:
        player: Player
    event LurkerSlain:
        player: Player
    event SealSet:
        player: Player

    scene argument(a: Person, b: Person) uninterruptible:
        a: "The seal held for forty years. You will not break it for silver."
        b: "Silver feeds this village, old man."
        pause 30
        a: "Silver did not feed the eleven we lost."
        b: "..."                                                      (ticks=60)
        a: "Then let the stranger decide what they find down there."

    stage Council:
        objective hear_out "Hear the elder and the foreman out" at elder
        -> Depths when hear_out.done

        dialogue ask(p: Player, npc: Person) for elder topic "The mine?":
            npc: "Brann wants it open. Stand here and listen to him lie."
            emit HeardOut(player=p)

        @on(HeardOut)
        fn convene(e):
            join(e.player)
            play(argument, a=elder, b=foreman)

        @on(SceneEnded)
        fn heard(e):
            if e.scene == argument and e.reason == SceneEnd::Finished:
                hear_out.finish()

    stage Depths:
        objective lurkers "Clear the lurkers from the mine" at mine count 6
        objective seal "Set the seal stone" at mine

        dialogue foreman_sulk(p: Player, npc: Person) for foreman:
            => npc: "Go on then. Bury our living."
            => npc: "Mind the third shaft. It floods."

        @on(LurkerSlain)
        fn slain(e):
            join(e.player)              # anyone who fights down there takes part
            lurkers.progress(1)

        @on(SealSet)
        fn sealed(e):
            if lurkers.done:
                seal.finish()
                complete()

    part depths at mine:
        @on(Setup)
        fn nest(e):
            place(prefab("lurker_nest"), mine) else:
                decline()               # no room at this mine: the whole quest is not placed
                return

        @on(Killed)
        fn killed(e):
            if stage == Stage::Depths and ours(e.victim):
                match e.by:
                    case Some(p):
                        emit LurkerSlain(player=p)
                    case None:
                        pass            # drowned in the third shaft: no one's kill

        @on(BlockUsed)
        fn used(e):
            if stage == Stage::Depths and e.tag == "seal_stone":
                emit SealSet(player=e.player)
