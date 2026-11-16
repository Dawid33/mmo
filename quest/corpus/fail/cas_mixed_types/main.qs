@on(ItemDelivered)
fn claim(e):
    match await world.compare_and_set("wyrm_quest", "open", e.player.id):
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
