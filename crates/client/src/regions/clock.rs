//! The client's one world clock. Every region counts the server's world tick
//! numbers, so the client keeps ONE predicted world tick — its estimate of
//! the server's world tick plus a lead ([`CLIENT_LEAD_MS`] beyond the round
//! trip) — and every predicted region runs to it: one behind (a fresh
//! snapshot) catches up in bounded bursts, one ahead is held. Regions in
//! step close each slot together, so a transfer one predicts into another
//! lands in the same slot as on the server. [`decide`] steers the predicted
//! tick from `ServerPacket::SyncClock` samples: a rate slew, or a step when
//! it is far off.
use game::TEST_INDUCED_LATENCY_MS;

/// How far ahead of the server the client's timeline is paced, in ms beyond
/// the round trip: the margin an input has to reach the server before the
/// tick it was predicted after. One and a half ticks.
pub(crate) const CLIENT_LEAD_MS: isize = game::TICK_RATE as isize * 3 / 2;
/// Half-width of the band around [`CLIENT_LEAD_MS`] in which the tick rate is
/// left alone. The lead is measured in whole ticks, so anything narrower than
/// a tick flaps.
pub(crate) const CLIENT_LEAD_BAND_MS: isize = game::TICK_RATE as isize;
/// How far past the band the lead has to be before the predicted tick is
/// stepped rather than slewed. A step shows: the player jumps a few ticks of
/// movement, or stands still for them.
pub(crate) const HOME_STEP_ALIGN_MS: isize = game::TICK_RATE as isize * 3;
/// How far ahead of the band the clock must run before it is stepped back
/// (every region held) rather than slewed: the server holds an input stamped
/// up to this far ahead (`MAX_INPUT_LEAD_TICKS`), so any lead inside it
/// still lands in its slot.
pub(crate) const HOME_STALL_MIN_MS: isize = (game::MAX_INPUT_LEAD_TICKS * game::TICK_RATE) as isize;
/// Most ticks one alignment step adds or sits out (two seconds).
pub(crate) const MAX_ALIGN_TICKS: usize = 2000 / game::TICK_RATE as usize;

/// One `ServerPacket::SyncClock` sample, from any region: every region
/// reports the one world tick numbering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockInput {
    pub server_tick: game::Tick,
    pub server_rate_ms: u64,
    pub rtt_ms: u64,
}

/// What to do about one sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockDecision {
    /// The client's lead over the server in ms, net of the round trip (the
    /// `drift` metric).
    pub drift_ms: isize,
    /// `Some((error_ms, ticks))` when the clock is far out of its band: move
    /// the predicted tick `ticks` forward (`error_ms < 0`; every region
    /// catches up) or back (every region is held for them). Capped at
    /// [`MAX_ALIGN_TICKS`].
    pub step: Option<(isize, usize)>,
    /// The client tick period to run at and whether the clock counts as
    /// synced (`ready`).
    pub rate: (u64, bool),
}

/// The clock decision for one sample, with the predicted world tick at
/// `predicted`.
pub fn decide(predicted: game::Tick, input: &ClockInput) -> ClockDecision {
    let diff: isize = predicted as isize - input.server_tick as isize;
    // How far ahead of the server the client is, net of the round trip.
    let drift_ms = diff * input.server_rate_ms as isize - input.rtt_ms as isize;

    // Hold a lead over the server, within a band: an input that reaches the
    // server before the tick it was predicted after is applied in exactly
    // that slot, one that arrives late is a rollback. The band keeps the
    // rate from flapping on the measurement's one-tick grain.
    let lead = drift_ms - TEST_INDUCED_LATENCY_MS;
    let error = lead - CLIENT_LEAD_MS;
    let rate = (input.server_rate_ms as isize).max(1);

    // Slewed, and stepped only when far out (the join, a long hitch): at
    // the 10% slew a second of error would take ten to close.
    let far = CLIENT_LEAD_BAND_MS + HOME_STEP_ALIGN_MS;
    // Never stepped back for running AHEAD within the server's input-hold
    // horizon: a sample goes stale behind the region snapshots a window
    // shift queues on the same path, so right after a crossing it read
    // ~350 ms ahead and sat the player out for 6-10 ticks — the crossing
    // pause (`just fly-probe`, 2026-09-23). Ahead only means the server
    // holds each input a little longer; the slew below closes it.
    let ahead_ok = error > 0 && error <= HOME_STALL_MIN_MS;
    let step = (error.abs() > far && !ahead_ok)
        .then(|| (error, (((error.abs() + rate / 2) / rate) as usize).min(MAX_ALIGN_TICKS)));
    let server = input.server_rate_ms;
    let rate = if error < -CLIENT_LEAD_BAND_MS {
        ((server as f32 * 0.9) as u64, false)
    } else if error > CLIENT_LEAD_BAND_MS {
        ((server as f32 * 1.1) as u64, true)
    } else {
        (server, true)
    };
    ClockDecision { drift_ms, step, rate }
}

/// The client's predicted world tick, steered by every region's samples.
///
/// Each running server region reports its tick every 10 world ticks, all at
/// the same tick numbers, so a round of samples says the same thing nine
/// times; and a server region still catching up after its start reports an
/// older tick. Only a sample NEWER than every one seen so far is decided on:
/// once per round, and on the freshest news of the server's world tick —
/// whichever region it comes from, so a crossing changes nothing here.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PredictedClock {
    tick: Option<game::Tick>,
    newest_sample: Option<game::Tick>,
}

impl PredictedClock {
    /// The predicted world tick: every region runs to it. `None` before the
    /// first region snapshot.
    pub fn tick(&self) -> Option<game::Tick> {
        self.tick
    }

    /// A region snapshot at `tick` arrived: the first one starts the clock
    /// there (the samples then steer it into the lead).
    pub fn start_at(&mut self, tick: game::Tick) {
        self.tick.get_or_insert(tick);
    }

    /// One tick of the client's tick timer: the predicted tick moves on.
    pub fn advance(&mut self) -> Option<game::Tick> {
        let t = self.tick.as_mut()?;
        *t += 1;
        Some(*t)
    }

    /// Decide on one sample, applying its step. `None`: the clock has not
    /// started, or the sample is no newer than one already decided on.
    pub fn sample(&mut self, input: &ClockInput) -> Option<ClockDecision> {
        let predicted = self.tick?;
        if self.newest_sample.is_some_and(|t| input.server_tick <= t) {
            return None;
        }
        self.newest_sample = Some(input.server_tick);
        let d = decide(predicted, input);
        if let Some((error, ticks)) = d.step {
            let t = self.tick.as_mut().expect("started");
            if error > 0 {
                *t = t.saturating_sub(ticks as game::Tick);
            } else {
                *t += ticks as game::Tick;
            }
        }
        Some(d)
    }
}

/// Ticks a region at `region_tick` runs on one tick of the clock to reach
/// the predicted world tick: the whole gap, at most
/// [`game::MAX_CATCH_UP_BURST`] (the server's catch-up rule); none for a
/// region already there or ahead (held until the clock reaches it).
pub fn ticks_due(region_tick: game::Tick, predicted: game::Tick) -> usize {
    game::ticks_due(region_tick, predicted) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(server_tick: game::Tick, rtt_ms: u64) -> ClockInput {
        ClockInput { server_tick, server_rate_ms: 50, rtt_ms }
    }

    #[test]
    fn in_band_runs_at_server_rate() {
        let d = decide(102, &sample(100, 25));
        assert_eq!(d, ClockDecision { drift_ms: 75, step: None, rate: (50, true) });
    }

    #[test]
    fn far_behind_steps_and_slows() {
        let d = decide(100, &sample(110, 0));
        assert_eq!(d, ClockDecision { drift_ms: -500, step: Some((-575, 12)), rate: (45, false) });
    }

    /// The crossing-pause regression (`HOME_STALL_MIN_MS`): running ahead
    /// within the server's input-hold horizon is slewed, never held.
    #[test]
    fn slightly_ahead_is_not_stalled() {
        let d = decide(120, &sample(100, 0));
        assert_eq!(d, ClockDecision { drift_ms: 1000, step: None, rate: (55, true) });
    }

    #[test]
    fn far_ahead_is_stepped_back() {
        let d = decide(130, &sample(100, 0));
        assert_eq!(d, ClockDecision { drift_ms: 1500, step: Some((1425, 29)), rate: (55, true) });
    }

    #[test]
    fn step_is_capped() {
        let d = decide(0, &sample(1000, 0));
        assert_eq!(d.step.map(|(_, ticks)| ticks), Some(MAX_ALIGN_TICKS));
        assert_eq!(MAX_ALIGN_TICKS, 40);
    }

    /// Several regions' samples of one server world tick give ONE predicted
    /// tick, and every region — whatever tick its snapshot left it at — runs
    /// to that same tick.
    #[test]
    fn every_region_targets_one_predicted_tick() {
        let mut clock = PredictedClock::default();
        clock.start_at(100);
        clock.start_at(90); // a later snapshot does not move it
        assert_eq!(clock.tick(), Some(100));
        // Three regions report world tick 110: the first decides (12 ticks
        // forward), the rest say nothing new.
        assert!(clock.sample(&sample(110, 0)).is_some());
        assert_eq!(clock.sample(&sample(110, 0)), None);
        assert_eq!(clock.sample(&sample(109, 0)), None, "a region still catching up");
        let target = clock.advance().expect("started");
        assert_eq!(target, 113);
        let mut regions = [100, 108, 113, 114];
        for _ in 0..4 {
            for r in &mut regions {
                *r += ticks_due(*r, target) as game::Tick;
            }
        }
        assert_eq!(regions[..3], [target; 3], "every region at the predicted tick");
        assert_eq!(regions[3], 114, "a region ahead is held, never stepped back");
    }

    /// A region 3 ticks behind runs all 3 at once; one far behind catches
    /// up in bursts of at most `MAX_CATCH_UP_BURST`.
    #[test]
    fn region_behind_gets_a_bounded_burst() {
        assert_eq!(ticks_due(97, 100), 3);
        assert_eq!(ticks_due(80, 100), game::MAX_CATCH_UP_BURST as usize);
        assert_eq!(ticks_due(100, 100), 0);
        assert_eq!(ticks_due(101, 100), 0);
    }

    #[test]
    fn far_ahead_sample_moves_the_clock_back() {
        let mut clock = PredictedClock::default();
        assert_eq!(clock.sample(&sample(100, 0)), None, "not started");
        clock.start_at(130);
        clock.sample(&sample(100, 0)).expect("decided");
        assert_eq!(clock.tick(), Some(101));
    }
}
