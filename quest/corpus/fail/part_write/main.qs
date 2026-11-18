# `part-write`: the home owns the quest's state; a part reads it through its mirror and changes it only by
# `emit`ting to the home.

quest Q:
    role pet: Creature
    let sightings = 0
    stage S:
        @on(Entered)
        fn s(e):
            pass
    part watch on pet:
        @on(PlayerNear)
        fn near(e):
            sightings = sightings + 1
