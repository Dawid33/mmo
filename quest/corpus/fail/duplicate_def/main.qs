# Quest examples §6, the fifth fragment: an objective and a role share the quest's names (`duplicate-def`), and
# `Entered` outside a stage has no stage to have been entered (`entered-outside-stage`).

quest Q:
    role dog: Creature
    stage Search:
        objective dog "Find the dog" at dog            # duplicate-def: the objective `dog` shadows the role
    @on(Entered)                                       # entered-outside-stage: whose stage was entered?
    fn entered(e):
        pass
