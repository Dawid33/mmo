# Dialogue examples §1 (docs/superpowers/specs/2026-11-16-dialogue-examples.md): node groups and line groups,
# Yarn Spinner's BasicSaliency.yarn. Scores by conjuncts (spec §5.11, folded).
# Adapted for the stub: `npc_named("Alice")` is `Person(id=3)` (the stub has no lookup by name; likewise
# Barry 4, Liz 5, George 6), and an `event Clock` with its handler stands in for the host's day and time
# queries, so that the script can move the clock.

enum Day:
    Monday
    Tuesday
    Wednesday

enum TimeOfDay:
    Morning
    Evening

let day = Day::Monday              # the current day (in game: a host query)
let time = TimeOfDay::Morning      # the current time of day

event Clock:
    day: Day
    time: TimeOfDay

@on(Clock)
fn clock(e):
    day = e.day
    time = e.time

dialogue alice_talk(p: Player, npc: Person) for Person(id=3):
    node talk when day == Day::Monday:
        p: "Hey"
        npc: "Hey, it's Monday"
        p: "yep"
        npc: "and I only have a single storylet for Monday regardless of the time of day"
        p: "cool"
        npc: "indeed"
    node talk when day == Day::Tuesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Hello, what a great Tuesday morning it is!"
    node talk when day == Day::Tuesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Ah Tuesday evening, always the best time of Tuesday I say"
    node talk when day == Day::Tuesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Just between you and me I have some plans this evening."
    node talk when day == Day::Wednesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Humpday, am I right?"
    node talk when day == Day::Wednesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Ah Wednesday morning, definitely the most morning part of the day."
    node talk when once if day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "the week is half over, what a relief."
    node talk when day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Probably should go to bed, it is Wednesday evening after all."

dialogue barry_talk(p: Player, npc: Person) for Person(id=4):
    # Three nodes with the same condition: best_least_recent cycles through them.
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "Good morning"
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "How are you this morning?"
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "Top of the morning"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "Good evening"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "What a fine evening it is today"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "Wait, is is it evening or afternoon?"

dialogue liz_talk(p: Player, npc: Person) for Person(id=5):
    node talk when once:           # scores 1, so it wins once; a node with no condition scores 0
        npc: "I have this node which will only happen once, neato right?"
    node talk:
        npc: "Oh hi"
    node talk:
        npc: "What up?"
    node talk:
        npc: "Talk to anyone interesting lately?"

dialogue george_talk(p: Player, npc: Person) for Person(id=6):
    => npc: "oh hi"
    => npc: "another Monday, I hate Mondays" when day == Day::Monday
    => npc: "I suppose I should make breakfast" when time == TimeOfDay::Morning
    => npc: "Weird how tomorrow is Monday, right?" when day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "did you want the devs to have to write out even more sample content?"
        npc: "it would be nice, yes"
        p: "well they aren'that gonna do that"
        npc: "boo"
    => npc: "I wonder if I am in any other samples"
