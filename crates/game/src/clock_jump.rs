//! Region clock jumps (a region starting at the world tick).
use crate::{ItemPile, Rollback, Tick};

impl Rollback {
    /// Called by `Region::jump_clock_to` after the tick counter moved forward
    /// by `delta`. Shifts deadlines that count LOADED time: each pile's
    /// `expire_tick` and `pickup_after_tick` and the `pile_expiry` index (via
    /// the pile write funnel). Absolute deadlines that mean "time passed"
    /// (slime timers, cooldowns) are left to expire.
    pub(crate) fn on_clock_jump(&mut self, delta: Tick) {
        let shifted: Vec<_> = self
            .data
            .item_piles
            .iter()
            .map(|(&p, pile)| {
                let pile = ItemPile {
                    expire_tick: pile.expire_tick + delta,
                    pickup_after_tick: pile.pickup_after_tick + delta,
                    ..pile.clone()
                };
                (p, Some(pile))
            })
            .collect();
        self.write_piles_safe(shifted);
    }
}
