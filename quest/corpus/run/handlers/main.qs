# Spec §7: `@on(Event)` handlers (the event type inferred from the decorator), several on one event, run
# one at a time; events emitted by `main` go through the real queue; a handler is also a plain function.
# The two `PlayerEntered` handlers are named so that declaration order and name order agree (the order is
# not guaranteed to authors).
let entered = 0

@on(PlayerClicked)
fn greet(e):                     # e: PlayerClicked, inferred from the decorator
    say(e.player, "Well met.")

@on(PlayerEntered)
fn warn(e):
    if e.player.has_tag("outlaw"):
        say(e.player, "Begone.")

@on(PlayerEntered)
fn watch(e):
    entered = entered + 1
    print(f"entered {entered}")

fn main():
    let ana = Player(id=1, name="ana")
    let bo = Player(id=2, name="bo")
    bo.add_tag("outlaw")
    greet(PlayerClicked(player=bo))
    emit(PlayerClicked(player=ana))
    emit(PlayerEntered(player=ana))
    emit(PlayerEntered(player=bo))
