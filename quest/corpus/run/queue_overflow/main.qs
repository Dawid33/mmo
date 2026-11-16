#! fuel=300
# Spec §7: events that arrive while a handler is still running are queued (a warning each); with the queue
# full (4) the newest is dropped (an error). `burst` emits six events; they arrive while `busy` loops.
@on(PlayerClicked)
fn burst(e):
    for i in range(6):
        emit(ItemDelivered(player=e.player, item=Item(kind="k", count=i)))

@on(PlayerEntered)
fn busy(e):
    let i = 0
    while i < 1000:
        i = i + 1
    print("done looping")

@on(ItemDelivered)
fn got(e):
    print(f"got {e.item.count}")

fn main():
    let ana = Player(id=1, name="ana")
    emit(PlayerClicked(player=ana))
    emit(PlayerEntered(player=ana))
