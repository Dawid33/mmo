# `bad-objective-call`: an objective's commands are `finish()`, `fail()`, `reset()` and `progress(n)` — the
# last only with a `count`; its reads (`done`, …) take no parentheses.

quest Q:
    role keeper: Person
    stage S:
        objective talk "Talk to the keeper" at keeper
        @on(PlayerClicked)
        fn clicked(e):
            talk.progress(1)
            talk.burn()
            if talk.done():
                pass
