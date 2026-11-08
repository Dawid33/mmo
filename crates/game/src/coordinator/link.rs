//! The host's end of its link to the world coordinator, and the inline link that runs a
//! [`Coordinator`] in the host's own thread (single-player on wasm, tests).

use std::collections::VecDeque;

use super::{CoordMsg, Coordinator, HostMsg};

/// The host's end of the link to the world coordinator: messages out, messages in, in
/// order (FIFO each way while the link is up).
pub trait WorldLink {
    fn send(&mut self, msg: HostMsg);
    fn recv(&mut self) -> Option<CoordMsg>;
    fn is_up(&self) -> bool;
    /// The link's connection generation: it changes on every (re)connect. A host that
    /// sees it change while [`WorldLink::is_up`] stayed true (the link dropped and came
    /// back between two looks) re-attaches as after a down link. A link that never
    /// reconnects (inline, in-process) keeps the default 0.
    fn generation(&self) -> u64 {
        0
    }
    /// Drive a coordinator that runs in the caller's thread ([`InlineWorldLink`]); the
    /// host calls it before draining [`WorldLink::recv`]. A no-op for a real link.
    fn pump(&mut self, _now_ms: u64) {}
    /// The inline link behind a boxed link, for tests and tools that reach the
    /// coordinator core (`InlineWorldLink::coordinator_mut`). `None` for any other link.
    fn as_inline_mut(&mut self) -> Option<&mut InlineWorldLink> {
        None
    }
    /// [`WorldLink::as_inline_mut`], read-only.
    fn as_inline(&self) -> Option<&InlineWorldLink> {
        None
    }
}

/// splitmix64 of `seed` and `i`: the stateless roll behind the inline link's delays.
fn mix(seed: u64, i: u64) -> u64 {
    let mut z = seed ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A [`Coordinator`] driven in the host's thread. [`WorldLink::pump`] hands it the host's
/// messages, runs its `maintain`, commits its state at once (no store: nothing to be
/// durable to, see [`Coordinator::commit_unstored`]) and releases its outputs. With no
/// delay every output is readable right after the pump that made it — today's
/// synchronous ordering; with a delay ([`InlineWorldLink::with_delay`],
/// `InlineSchedule::coordinator_delay`) each one becomes readable 0..=`max_steps` pumps
/// later, chosen by a stateless roll of (seed, its index), order kept.
pub struct InlineWorldLink {
    coord: Coordinator,
    in_q: VecDeque<HostMsg>,
    /// Outputs with the pump step from which they may be read.
    out_q: VecDeque<(u64, CoordMsg)>,
    /// `(seed, max_steps)`.
    delay: Option<(u64, u32)>,
    /// Pumps so far.
    step: u64,
    /// Outputs released so far (the roll's index).
    released: u64,
    /// Test hook ([`InlineWorldLink::set_up`]): `false` simulates a link that dropped.
    up: bool,
}

impl InlineWorldLink {
    pub fn new(coord: Coordinator) -> Self {
        InlineWorldLink { coord, in_q: VecDeque::new(), out_q: VecDeque::new(), delay: None, step: 0, released: 0, up: true }
    }

    /// Each output readable 0..=`max_steps` pumps after the one that made it.
    pub fn with_delay(mut self, seed: u64, max_steps: u32) -> Self {
        self.delay = (max_steps > 0).then_some((seed, max_steps));
        self
    }

    /// The delay `schedule` asks for (`InlineSchedule::coordinator_delay`), if any.
    pub fn with_schedule(self, schedule: &crate::InlineSchedule) -> Self {
        match schedule.coordinator_link_delay() {
            Some((seed, max)) => self.with_delay(seed, max),
            None => self,
        }
    }

    pub fn coordinator(&self) -> &Coordinator {
        &self.coord
    }

    pub fn coordinator_mut(&mut self) -> &mut Coordinator {
        &mut self.coord
    }

    /// Outputs made but not read yet (delayed or not drained).
    pub fn in_transit(&self) -> usize {
        self.out_q.len()
    }

    /// Test hook: take the link down (`false`) or bring it back (`true`). While down,
    /// [`WorldLink::is_up`] is false, whatever the host sends is lost (as on a dropped
    /// connection) and nothing is readable; the coordinator keeps running (its calendar
    /// moves on each pump), and what it sent meanwhile — or had on the link before —
    /// is read once the link is up again: stale messages arriving after a reconnect.
    pub fn set_up(&mut self, up: bool) {
        self.up = up;
    }

    /// Test hook: lose every output not read yet, as a dropped connection loses what was
    /// on it (the coordinator still made them: a request it answered is answered again
    /// from its memo when the host replays it).
    pub fn lose_in_transit(&mut self) {
        self.out_q.clear();
    }
}

impl WorldLink for InlineWorldLink {
    fn send(&mut self, msg: HostMsg) {
        if self.up {
            self.in_q.push_back(msg);
        } else {
            log::debug!("inline world link down: {msg:?} lost");
        }
    }

    fn recv(&mut self) -> Option<CoordMsg> {
        if !self.up {
            return None;
        }
        if self.out_q.front().is_some_and(|(due, _)| *due <= self.step) {
            self.out_q.pop_front().map(|(_, m)| m)
        } else {
            None
        }
    }

    fn is_up(&self) -> bool {
        self.up
    }

    fn pump(&mut self, now_ms: u64) {
        self.step += 1;
        while let Some(msg) = self.in_q.pop_front() {
            self.coord.handle(msg, now_ms);
        }
        self.coord.maintain(now_ms);
        self.coord.commit_unstored();
        // Never before the output ahead of it: the order is kept.
        let mut floor = self.out_q.back().map_or(self.step, |(due, _)| (*due).max(self.step));
        for msg in self.coord.take_out() {
            let lag = self.delay.map_or(0, |(seed, max)| mix(seed, self.released) % (u64::from(max) + 1));
            self.released += 1;
            floor = floor.max(self.step + lag);
            self.out_q.push_back((floor, msg));
        }
    }

    fn as_inline_mut(&mut self) -> Option<&mut InlineWorldLink> {
        Some(self)
    }

    fn as_inline(&self) -> Option<&InlineWorldLink> {
        Some(self)
    }
}
