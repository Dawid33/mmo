#! fail=Timeout@1:world.add fail=Timeout@1:world.get
# Spec §8.2: every request times out after a fixed number of ticks (200) with `Timeout`, an ordinary error:
# matched in `h`, escaping `g` through `?` (the handler is aborted and reported).
@on(PlayerClicked)
fn h(e):
    print("asking")
    match await world.add("k", 1):
        case Ok(n):
            print(f"ok {n}")
        case Err(Timeout):
            print("timed out")
        case Err(_):
            print("other")

@on(PlayerEntered)
fn g(e):
    print("fetching")
    let v: int = await world.get("k")?
    print(f"got {v}")

fn main():
    emit(PlayerClicked(player=Player(id=1, name="ana")))
    emit(PlayerEntered(player=Player(id=1, name="ana")))
