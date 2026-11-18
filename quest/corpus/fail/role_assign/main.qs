# `role-assign`: a role is bound at placement and never assigned.

quest Q:
    role keeper: Person
    stage Open:
        @on(PlayerClicked)
        fn clicked(e):
            keeper = Person(id=9)
