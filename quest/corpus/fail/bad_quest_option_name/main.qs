# `bad-quest-option`: an unknown option of a `repeatable quest`.

repeatable quest Again(cooldown=10, forever=true):
    role keeper: Person
    stage S:
        @on(Entered)
        fn s(e):
            pass
