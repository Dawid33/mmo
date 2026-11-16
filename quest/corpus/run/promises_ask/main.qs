#! latency=2
# Spec §8.2 (`ask`): await a namespace read, hold a promise and await it later, an `else` fallback.
# Adapted: `slain` and `r` take the annotation `int` (a `get` receives the type of its binding, and nothing
# else here fixes it); the handler ends by saying what it read. `main` fills the namespace first.
@on(PlayerClicked)
fn ask(e):
    let slain: int = await world.get("dragons_slain")?
    let p = world.get(f"renown/{e.player.id}")   # hold a promise, await later
    say(e.player, "Let me check the ledgers…")
    let r: int = await p else 0
    say(e.player, f"{slain} dragons slain, renown {r}")

fn main():
    await world.set("dragons_slain", 4) else ()
    await world.set("renown/2", 7) else ()
    emit(PlayerClicked(player=Player(id=1, name="ana")))
    emit(PlayerClicked(player=Player(id=2, name="bo")))
