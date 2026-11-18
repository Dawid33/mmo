# Quest examples §6, the first fragment: a role's `where` may call only fact queries (design §7.1), never a
# script `fn` — the coordinator indexes it without the VM.
# Adapted: the stub's `Person` has no `skill` (a `Player` query), so `wealthy` reads a tag.

fn wealthy(x: Person) -> bool:
    return x.has_tag("wealthy")

quest Q:
    role buyer: Person where wealthy(buyer)            # bad-where: a script fn is not a fact query
    stage S:
        @on(Entered)
        fn s(e):
            pass
