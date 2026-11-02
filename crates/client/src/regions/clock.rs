//! The client's per-region clock decision: how far a region's predicted
//! timeline leads its server counterpart, and whether to step it (predict
//! extra ticks now, or sit ticks out) or slew the home clock.
use game::TEST_INDUCED_LATENCY_MS;

/// How far ahead of the server the client's timeline is paced, in ms beyond
/// the round trip: the margin an input has to reach the server before the
/// tick it was predicted after. One and a half ticks.
pub(crate) const CLIENT_LEAD_MS: isize = game::TICK_RATE as isize * 3 / 2;
/// Half-width of the band around [`CLIENT_LEAD_MS`] in which the tick rate is
/// left alone. The lead is measured in whole ticks, so anything narrower than
/// a tick flaps.
pub(crate) const CLIENT_LEAD_BAND_MS: isize = game::TICK_RATE as isize;
/// How far past the band the home region's lead has to be before it is
/// stepped rather than left to the clock. A step shows: the player jumps a
/// few ticks of movement, or stands still for them.
pub(crate) const HOME_STEP_ALIGN_MS: isize = game::TICK_RATE as isize * 3;
/// How far ahead of the band home must run before it is sat out rather than
/// slewed: the server holds an input stamped up to this far ahead
/// (`MAX_INPUT_LEAD_TICKS`), so any lead inside it still lands in its slot.
pub(crate) const HOME_STALL_MIN_MS: isize = (game::MAX_INPUT_LEAD_TICKS * game::TICK_RATE) as isize;
/// Most ticks one alignment step adds or sits out (two seconds).
pub(crate) const MAX_ALIGN_TICKS: usize = 2000 / game::TICK_RATE as usize;

/// One `ServerPacket::SyncClock` sample for a region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockInput {
    pub server_tick: usize,
    pub server_rate_ms: u64,
    pub rtt_ms: u64,
    pub is_home: bool,
}

/// What to do about one sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockDecision {
    /// The client's lead over the server in ms, net of the round trip (the
    /// home region's is the `drift` metric).
    pub drift_ms: isize,
    /// `Some((error_ms, ticks))` when the region is out of its band: predict
    /// `ticks` more now (`error_ms < 0`) or sit the next `ticks` out. Capped
    /// at [`MAX_ALIGN_TICKS`].
    pub step: Option<(isize, usize)>,
    /// Home only: the client tick period to run at and whether the clock
    /// counts as synced (`ready`).
    pub home_rate: Option<(u64, bool)>,
}

/// The clock decision for one region whose timeline stands at `client_tick`.
pub fn decide(client_tick: usize, input: &ClockInput) -> ClockDecision {
    let diff: isize = client_tick as isize - input.server_tick as isize;
    // How far ahead of the server the client is, net of the round trip.
    let drift_ms = diff * input.server_rate_ms as isize - input.rtt_ms as isize;

    // Hold a lead over the server, within a band: an input that reaches the
    // server before the tick it was predicted after is applied in exactly
    // that slot, one that arrives late is a rollback. The band keeps the
    // rate from flapping on the measurement's one-tick grain.
    let lead = drift_ms - TEST_INDUCED_LATENCY_MS;
    let error = lead - CLIENT_LEAD_MS;
    let rate = (input.server_rate_ms as isize).max(1);

    // One clock ticks every region, but each region's offset to its server
    // counterpart dates from when its snapshot loaded, and a fresh one is
    // always behind. Were every region to steer the clock, the stream of
    // fresh regions would keep speeding it up and run the lead of all the
    // older ones away — home included, a second ahead within a minute of
    // roaming. So the clock follows home alone, and any other region is
    // stepped into the band on its own. Home is stepped too when it is far
    // out (the join, a flip into a region not yet aligned): at the clock's
    // 10% slew a second of error would take ten to close.
    let far = if input.is_home { CLIENT_LEAD_BAND_MS + HOME_STEP_ALIGN_MS } else { CLIENT_LEAD_BAND_MS };
    // Home is never frozen for running AHEAD within the server's input-hold
    // horizon: a sample goes stale behind the region snapshots a window shift
    // queues on the same path, so right after a crossing it read ~350 ms
    // ahead and sat the player out for 6-10 ticks — the crossing pause
    // (`just fly-probe`, 2026-09-23). Ahead only means the server holds each
    // input a little longer; the slew below closes it.
    let home_ahead_ok = input.is_home && error > 0 && error <= HOME_STALL_MIN_MS;
    let step = (error.abs() > far && !home_ahead_ok)
        .then(|| (error, (((error.abs() + rate / 2) / rate) as usize).min(MAX_ALIGN_TICKS)));
    let home_rate = input.is_home.then(|| {
        let server = input.server_rate_ms;
        if error < -CLIENT_LEAD_BAND_MS {
            ((server as f32 * 0.9) as u64, false)
        } else if error > CLIENT_LEAD_BAND_MS {
            ((server as f32 * 1.1) as u64, true)
        } else {
            (server, true)
        }
    });
    ClockDecision { drift_ms, step, home_rate }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(server_tick: usize, rtt_ms: u64, is_home: bool) -> ClockInput {
        ClockInput { server_tick, server_rate_ms: 50, rtt_ms, is_home }
    }

    #[test]
    fn home_in_band_runs_at_server_rate() {
        let d = decide(102, &sample(100, 25, true));
        assert_eq!(d, ClockDecision { drift_ms: 75, step: None, home_rate: Some((50, true)) });
    }

    #[test]
    fn home_far_behind_steps_and_slows() {
        let d = decide(100, &sample(110, 0, true));
        assert_eq!(d, ClockDecision { drift_ms: -500, step: Some((-575, 12)), home_rate: Some((45, false)) });
    }

    #[test]
    fn home_slightly_ahead_is_not_stalled() {
        let d = decide(120, &sample(100, 0, true));
        assert_eq!(d, ClockDecision { drift_ms: 1000, step: None, home_rate: Some((55, true)) });
    }

    #[test]
    fn neighbour_ahead_is_stalled() {
        let d = decide(120, &sample(100, 0, false));
        assert_eq!(d, ClockDecision { drift_ms: 1000, step: Some((925, 19)), home_rate: None });
    }

    #[test]
    fn step_is_capped() {
        let d = decide(0, &sample(1000, 0, true));
        assert_eq!(d.step.map(|(_, ticks)| ticks), Some(MAX_ALIGN_TICKS));
        assert_eq!(MAX_ALIGN_TICKS, 40);
    }
}
