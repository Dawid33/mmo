# Spec §5.11 "attached dialogues combine commutatively": what a player gets depends only on the set of
# dialogues attached to the NPC — never on the order they were attached, nor on the module order. Three
# modules attach to Mara: `a` a plain `for` greeting, `b` a `once per p` greeting and a topic, `c` a filter
# topic (priority 1) and a topic a handler attaches. The harness also runs every permutation of the `mod` lines
# below and checks that the golden is the same.

mod c
mod a
mod b
