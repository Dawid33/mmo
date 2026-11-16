# Spec §5.10: the module tree, `mod`, `use` (lists, `as`), `pub`, `crate::`, `super::`, `self::`, a `use`
# cycle between modules (main uses rewards, rewards uses crate::Ledger).
# Adapted: the spec's `npc/dialogue.qs` is `npc/talk.qs` — `dialogue` became a reserved word (spec §5.13);
# `npc::talk` is `pub mod` so the crate root may reach into it.
mod rewards
mod npc
use rewards::give_gold
use npc::talk::{greet, farewell}
use rewards::give_gold as pay

pub struct Ledger:
    entries: list[int]

fn main():
    let ana = Player(id=1, name="ana")
    greet(ana)
    give_gold(ana, 5)
    pay(ana, 7)
    print(str(rewards::total(Ledger(entries=[1, 2]))))
    print(npc::title())
    farewell(ana)
