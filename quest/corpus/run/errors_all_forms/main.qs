# Spec §5.7: errors raised without declaration, error sets, a pinned set, `?`, the three `else` forms, `match`
# on a fallible call (exhaustive against the inferred set), `?` / `else` on Option, an error escaping a handler.
# Adapted: the spec's `c` is a `Chest` struct declared here; its `give(p, gold)` gives `Item(kind="gold",
# count=gold)` (the stub's `give` takes an `Item`); `log` is the stub command (it prints `log: …`).
error ChestError = ChestEmpty | NotEnough(have: int)
error QuestError = ChestError | QuestClosed | NotYourQuest

struct Chest:
    gold: int
    closed: bool

let paid = 0

fn take_gold(c: Chest, n: int) -> int:
    if c.closed:
        raise QuestClosed
    if c.gold == 0:
        raise ChestEmpty
    if c.gold < n:
        raise NotEnough(have=c.gold)
    return n

fn pay(c: Chest, n: int) -> Item raises QuestError:
    let gold = take_gold(c, n)?
    return Item(kind="gold", count=gold)

fn give_or_return(p: Player, c: Chest):
    let gold = take_gold(c, 20) else return
    give(p, Item(kind="gold", count=gold))

fn take_or_log(c: Chest) -> int:
    let gold = take_gold(c, 20) else err:
        log(f"failed: {err}")
        return -1
    return gold

fn settle(p: Player, c: Chest):
    match take_gold(c, 20):
        case Ok(gold):
            give(p, Item(kind="gold", count=gold))
        case Err(NotEnough(have)):
            say(p, f"Only {have} left.")
        case Err(ChestEmpty | QuestClosed):
            pass

fn first_big(xs: list[int]) -> Option[int]:
    for x in xs:
        if x > 10:
            return Some(x)
    return None

fn double_first_big(xs: list[int]) -> Option[int]:
    let x = first_big(xs)?
    return Some(x * 2)

@on(PlayerClicked)
fn on_click(e):
    paid = paid + 1
    print("never printed")
    let item = pay(Chest(gold=0, closed=false), 5)?
    give(e.player, item)

@on(PlayerEntered)
fn on_enter(e):
    print(f"paid {paid}")

fn main():
    let ana = Player(id=1, name="ana")
    let full = Chest(gold=30, closed=false)
    let poor = Chest(gold=3, closed=false)
    let gold = take_gold(full, 20)?
    print(f"took {gold}")
    let gold = take_gold(poor, 20) else 0
    print(f"fallback {gold}")
    give_or_return(ana, poor)
    give_or_return(ana, full)
    print(str(take_or_log(poor)))
    settle(ana, full)
    settle(ana, poor)
    settle(ana, Chest(gold=0, closed=true))
    let item = pay(full, 5) else Item(kind="nothing", count=0)
    print(f"{item}")
    let item = pay(poor, 5) else Item(kind="nothing", count=0)
    print(f"{item}")
    print(str(double_first_big([1, 20]) else 0))
    print(str(double_first_big([1, 2]) else 0))
    emit(PlayerClicked(player=ana))
    emit(PlayerEntered(player=ana))
