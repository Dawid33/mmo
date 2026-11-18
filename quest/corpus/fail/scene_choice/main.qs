# Quest examples §6, the fourth fragment: a scene has no listener, so nobody to choose.

scene quarrel(a: Person, b: Person):
    a: "Well?" @quarrel.well
    choice:                                            # scene-choice: a scene has nobody to choose
        "Yes" @quarrel.yes
