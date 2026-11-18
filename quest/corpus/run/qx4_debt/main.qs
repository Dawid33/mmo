#! role=lender=Person(3) role=debtor=Person(4)
# Quest examples §4: A Debt to Settle — a reusable dialogue with exits, `detour` arms, `DialogueEnded.exit`.
# Adapted: the library package `errantry/trade` is the module `trade` here (`trade/mod.qs` declares
# `pub mod haggle`; `trade/haggle.qs` is the example's `src/haggle.qs`), so the `use` below is the example's.

mod trade
# Package errantry/old-debts — src/main.qs   (depends on errantry/trade)
use trade::haggle::haggle          # `trade`: the dependency's crate name, as in Rust

quest OldDebt:
    role lender: Person where lender.occupation == Occupation::Merchant
    role debtor: Person where debtor.site == lender.site and debtor != lender and debtor.alive

    event Collected:
        player: Player

    stage Collect:
        objective collect f"Collect {lender.name}'s money from {debtor.name}" at debtor

        dialogue confront(p: Player, npc: Person) for debtor topic f"{lender.name} sent me" exits paid, stalled:
            npc: "Oh. Them."
            detour haggle:
                agreed:
                    npc: "Here. Tell them we're square."
                    return paid
                refused:
                    npc: "Come back when you're less... you."
                    return stalled

        dialogue thanks(p: Player, npc: Person) for lender topic "About the debt" if collect.done:
            npc: "You got it? Then here's your cut."
            emit Collected(player=p)

        @on(DialogueEnded)
        fn after_talk(e):
            if e.dialogue == confront and e.exit == confront::paid:
                join(e.player)
                collect.finish()

        @on(Collected)
        fn reward(e):
            give(e.player, Item(kind="silver", count=10))
            complete()
