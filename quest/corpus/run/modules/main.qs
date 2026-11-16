# Spec §5.10: the module tree, `mod`, `use` (lists, `as`), `pub`, `crate::`, `super::`, `self::`, a `use`
# cycle between modules (main uses rewards, rewards uses crate::Ledger).
# Adapted: `npc/mod.qs` declares `pub mod dialogue` (a child module lives in its parent's directory and must
# be `pub` for the crate root to reach into it). `dialogue` is a reserved word (spec §5.13) but stays valid as
# a module name and path segment.
mod rewards
mod npc
use rewards::give_gold
use npc::dialogue::{greet, farewell}
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
