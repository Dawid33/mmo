//! A host marked to drain (Elastic 1 Task 8; `worldctl drain <url> <host id> [--in <secs>]`):
//! the server's half. `game::WorldManager` holds the deadline and tells the players
//! (`world_manager/drain.rs`); this module turns the heartbeat's mark into that deadline and,
//! after the leave, keeps the process up, idle and unready until it is removed.
//!
//! **The drain, as the main loop runs it** (`server::run`, `claim` hosts only):
//! - *not draining* → *warned*: a beat answers a drain mark (`Heartbeat::drain_in_ms`). Every
//!   pass after the lease step: [`note_drain`] — `drain_at(now_tick + max(d, 0) / TICK_RATE)`;
//!   the manager tells every home session the deadline (`ServerPacket::HostDraining`). The host
//!   simulates as before: joins, hand-offs, cuts, its own land; it claims nothing new (Task 7).
//!   A later `worldctl drain` may bring the deadline forward (re-read every beat).
//! - *warned* → *due*: `WorldManager::drain_due` — the world tick reached the deadline AND the
//!   coordinator is attached. With the coordinator unreachable nothing is parked: the host runs
//!   on, its players keep the notice, until the link is back.
//! - *due* → *leaving*: the loop ends and `cuts::exit_world` runs the existing leave
//!   (`leave_world`: the players park, a leave cut, `LEAVING_REFUSAL` once it is decided). A
//!   world cut under way is not disturbed: the leave cut begins after it (the coordinator
//!   never runs two), as for a SIGTERM during one.
//! - *leaving* → *drained*: the leave committed ([`stays_drained`]). The heartbeat stops (the
//!   clean row may be pruned while the process lives: no store call is made any more), every
//!   region, peer and coordinator link and the store go with the manager, and [`idle_drained`]
//!   runs: `/readyz` 503 `drained`, `/livez` 200 (its loop beats), a connecting client is
//!   refused with `LEAVING_REFUSAL`; a stop signal ends it, exit 0. Another host takes the land
//!   through a `Leave` succession when it needs it.
//! - *leaving* → *ended*: a leave that did NOT commit (aborted past the leave timeout, or never
//!   decided) ends the process as today: exit 0 with its `hosts` row dirty — a crash to the
//!   others (a succession takes its land; its players rejoin and lose up to one save interval).
//!   The Deployment then starts a new host in its place. No retry loop beyond `leave_world`'s.
//!
//! A stop signal (SIGTERM) always means "leave now": during the warning it is today's exit
//! (the leave at once, then exit 0 — never the drained idle); while drained, exit 0 at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender};
use game::{ClientId, RegionSpawner, ServerEvent, ServerPacket, Tick, WorldManager, TICK_RATE};

use crate::claims::{ServerExit, LEAVING_REFUSAL};
use crate::health::Health;

/// How long the drained idle loop waits for a connection event before it looks at the stop
/// flag again (and beats `/livez`).
pub const DRAINED_POLL: Duration = Duration::from_millis(200);

/// The drain deadline (a world tick) of a mark `drain_in_ms` from now (database-computed:
/// negative = past due) at world tick `now_tick`.
pub fn drain_deadline(now_tick: Tick, drain_in_ms: i64) -> Tick {
    now_tick + u64::try_from(drain_in_ms.max(0)).unwrap_or(0) / TICK_RATE
}

/// The main loop's drain step, after the lease step: the last beat's mark (`None`: not
/// draining, or no beat answered yet) becomes the manager's deadline. Idempotent every pass:
/// the manager keeps the earliest deadline (a stale mark only ever computes a later one).
pub fn note_drain<S: RegionSpawner>(manager: &mut WorldManager<S>, drain_in_ms: Option<i64>, now_tick: Tick) {
    if let Some(d) = drain_in_ms {
        manager.drain_at(drain_deadline(now_tick, d));
    }
}

/// A `static` host (the world's only one) never acts on a drain mark; the first time its beat
/// shows one (`drain_in_ms`), it says so — once (`warned` remembers). Whether to warn now.
pub fn static_mark_first_seen(warned: &mut bool, drain_in_ms: Option<i64>) -> bool {
    if *warned || drain_in_ms.is_none() {
        return false;
    }
    *warned = true;
    true
}

/// What a `static` host logs (once) when it is marked draining.
pub const STATIC_HOST_IGNORES_DRAIN: &str = "[server] this host is marked draining (`worldctl drain`), but a static host \
     is its world's only one and is never drained: the mark is ignored (stop the world instead, `just world-stop`)";

/// Whether the process stays up drained after its exit: the loop ended for the drain's
/// deadline (not a stop signal, a fence or a rollback), the exit was clean and the leave cut
/// committed (`WorldManager::leave_outcome`). Otherwise the process ends with `outcome`.
pub fn stays_drained(for_the_drain: bool, outcome: ServerExit, leave_outcome: Option<bool>) -> bool {
    for_the_drain && outcome == ServerExit::Clean && leave_outcome == Some(true)
}

/// The drained host, after its leave committed: `health` set drained, then — until `stop` —
/// its loop beats (`/livez`), and every session the edge admits (`events`: a connect of any
/// kind) is refused with [`LEAVING_REFUSAL`] on `out` (the edge's writer). No store call, no
/// region, no peer, no coordinator. Returns [`ServerExit::Clean`] at the stop signal (exit 0).
pub fn idle_drained(
    stop: &AtomicBool,
    events: &Receiver<ServerEvent>,
    out: &Sender<(Option<ClientId>, ServerPacket)>,
    health: &Health,
) -> ServerExit {
    // The beat first (the leave may have outlasted `LIVEZ_STALL_MS`; `set_drained` beats too).
    health.loop_beat();
    health.set_drained();
    log::info!("[server] drained: this host left the world; idle and unready until it is stopped");
    loop {
        health.loop_beat();
        if stop.load(Ordering::SeqCst) {
            log::info!("[server] stop signal while drained: exiting");
            return ServerExit::Clean;
        }
        match events.recv_timeout(DRAINED_POLL) {
            Ok(ev) => {
                if let Some(client) = connecting(&ev) {
                    let _ = out.send((Some(client), ServerPacket::Refused(LEAVING_REFUSAL.to_string())));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The edge is gone: nothing more can connect; wait for the stop signal.
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(DRAINED_POLL),
        }
    }
}

/// The session a connection event starts, if it is one.
fn connecting(ev: &ServerEvent) -> Option<ClientId> {
    match ev {
        ServerEvent::ClientConnected { client, .. }
        | ServerEvent::LoginConnected { client, .. }
        | ServerEvent::ObserverConnected { client, .. } => Some(*client),
        _ => None,
    }
}

