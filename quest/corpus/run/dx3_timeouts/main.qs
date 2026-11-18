# Dialogue examples §3: options that time out (Yarn Spinner's Timeouts.yarn): a hidden `default:` fallback and
# a visible `default "text"` option, each picked when `choice(timeout=300)` runs out.
# Adapted for the stub: `npc_named("Alice")` is `Person(id=3)`.

dialogue timeouts(p: Player, npc: Person) for Person(id=3):
    node start:
        npc: "This sample shows off three different ways of having an option be selected automatically after a fixed amount of time."
        npc: "Which would you like to see?"
        choice:
            "Hidden fallback option?":
                -> fallback
            "Default option?":
                -> default_example
    node fallback:
        npc: "In this example if no option is chosen before time runs out we chose the option with the fallback metadata."
        npc: "This differs from the default example as the fallback option is hidden from the user."
        choice(timeout=300):                    # ticks
            "Option 1?":
                npc: "option 1 was selected"
            "Option 2?":
                npc: "option 2 was selected"
            default:                            # hidden: never offered, runs on timeout
                npc: "The hidden fallback option was selected"
    node default_example:
        npc: "In this example if no option is chosen before time runs out we chose the option with the default metadata."
        npc: "this differs from the fallback example as the default is visible to the user."
        choice(timeout=300):
            default "Option 1?":                # offered, and picked on timeout
                npc: "option 1 was selected"
                npc: "this is the default."
            "Option 2?":
                npc: "option 2 was selected"
