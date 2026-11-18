# `bad-part-item`: a part holds `let`s, `event`s and handlers; stages, objectives, dialogues and the rest belong
# in the quest.

quest Q:
    role pet: Creature
    stage S:
        @on(Entered)
        fn s(e):
            pass
    part watch on pet:
        let seen = 0
        stage Watching:
            @on(Entered)
            fn w(e):
                pass
