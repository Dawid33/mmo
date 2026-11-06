//! Region clock jumps (a region starting at the world tick).
use crate::{Rollback, Tick};

impl Rollback {
    /// Called by `Region::jump_clock_to` after the tick counter moved forward
    /// by `delta`. Filled by Task 1.2: shifts loaded-time deadlines.
    pub(crate) fn on_clock_jump(&mut self, _delta: Tick) {}
}
