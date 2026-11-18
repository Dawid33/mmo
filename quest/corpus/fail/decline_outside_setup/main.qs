# `decline-outside-setup`: `decline()` refuses a placement, so only a `Setup` handler may call it.

quest Q:
    role keeper: Person
    stage S:
        @on(PlayerClicked)
        fn clicked(e):
            decline()
