# Spec §5.11, the second example: a greeting node group (`when p.has_tag(...)`, `when once per p`, none) and a
# line group with a `once per p` member. Scores: each condition 1, none 0; ties go to the least picked, then
# source position.
# Adapted: the snippet's `innkeeper` is a top-level `let` (`Person(id=5)`).

let innkeeper = Person(id=5)

dialogue greeting(p: Player, npc: Person) for innkeeper:
    node hello when p.has_tag("outlaw"):
        npc: "Keep walking."                                           @greet.outlaw
    node hello when once per p:
        npc: "A new face! Welcome."                                    @greet.first
    node hello:
        => npc: "Morning."                                             @greet.a
        => npc: "Weather's turning."                                   @greet.b
        => npc: "You again?"                                           @greet.c when once per p
