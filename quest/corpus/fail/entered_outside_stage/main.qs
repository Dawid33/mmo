# `entered-outside-stage`: `Entered` is raised when a stage becomes current, so only a stage's handler may take
# it (quest examples §6).

quest Q:
    role keeper: Person
    stage Open:
        @on(Entered)
        fn opened(e):
            pass
    @on(Entered)
    fn entered(e):
        pass
