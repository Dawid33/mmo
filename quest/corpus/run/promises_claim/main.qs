#! latency=2 chest=gold=50
# Spec §8.2 (`claim`): a compare-and-set race; the winner is paid from the chest.
# Adapted: `chest.take` is a promise (plan decision 10): `give(e.player, await chest.take("gold", 20)?)`;
# the match gains `case Err(_)` — a direct host call raises its whole list (decision 21: `NotFound`,
# `Timeout`, `Lost`, `NamespaceGone` besides `Changed`); the new value is `str(e.player.id)`, not the id:
# `compare_and_set` takes `new` of the type of `expected` (one key holds one type, so `Changed(current)` has
# exactly that type) — the spec's `e.player.id` is refused, see `fail/cas_mixed_types`.
@on(ItemDelivered)
fn claim(e):
    match await world.compare_and_set("wyrm_quest", "open", str(e.player.id)):
        case Ok(_):
            give(e.player, await chest.take("gold", 20)?)
        case Err(Changed(winner)):
            say(e.player, "Another claimed it first.")
        case Err(_):
            say(e.player, "The ledgers are silent.")

fn main():
    await world.set("wyrm_quest", "open") else ()
    let wyrm = Item(kind="wyrm head", count=1)
    emit(ItemDelivered(player=Player(id=1, name="ana"), item=wyrm))
    emit(ItemDelivered(player=Player(id=2, name="bo"), item=wyrm))
