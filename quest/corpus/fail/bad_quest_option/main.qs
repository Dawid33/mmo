# `bad-quest-option`: options belong to a `repeatable quest`, are named, known and constant.

quest Once(cooldown=10):
    role keeper: Person
    stage S:
        @on(Entered)
        fn s(e):
            pass
