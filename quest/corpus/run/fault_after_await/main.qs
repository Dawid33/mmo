# Spec §6.6 / §11 "Segments": a fault after an `await` undoes only the segment since it; the aborted
# segment's output, commands and promise requests never reach the host.
let g = 0

@on(PlayerClicked)
fn h(e):
    g = 1
    print("before")
    await world.set("k", 1) else ()
    g = 2
    print("after")
    say(e.player, "never said")
    let _ = world.set("never", 1)
    let xs = [1]
    print(str(xs[5]))

@on(PlayerEntered)
fn show(e):
    let never: int = await world.get("never") else -1
    print(f"g={g} never={never}")

fn main():
    let ana = Player(id=1, name="ana")
    emit(PlayerClicked(player=ana))
    await world.set("m", 1) else ()
    await world.set("m", 2) else ()
    emit(PlayerEntered(player=ana))
