//! Elastic 4: the hosts the world wants — the coordinator decides how many exist. The table is
//! `desired_hosts` (`world_store` `fleet.rs`), written only by the store decisions
//! [`super::AddHost`], [`super::DrainHost`] and [`super::RemoveHost`] inside the coordinator's
//! transition; the reconciler reads it and makes one host Deployment per row.
//!
//! **What the ticker decides** (plan `2026-11-19-elastic-4-reconciler-scaling.md`, decision 4):
//! [`Coordinator::maintain_fleet`], from the upkeep ([`super::Input::Maintain`]), on the ticker
//! only, only with fleet dials set ([`Coordinator::set_fleet`]; none = no fleet decision at
//! all: `dev-r3`, role `all`). At most one decision per [`SCALE_DECISION_EVERY_MS`], none while
//! one is unanswered (`fleet_pending`), and after an answer none before a `hosts` read ISSUED
//! after it was applied (`fleet_read_after`) — the desired rows come with that read
//! ([`super::Input::HostsRead`], never logged), so a rule never acts on a table its own last
//! decision may already have changed. The core keeps no durable fleet state: the decisions carry
//! what they expect ([`super::AddHost::expect_wanted`]), so a replay, a duplicate or a second
//! replica changes nothing in the store.
//!
//! Terms: `wanted` / `draining` = the desired rows by state; the **live host of a name** = its
//! live dirty `hosts` row (the newest that still beats: a registration lapses its name's older
//! rows, which stay live and dirty until ended). The first rule that applies, in order:
//! 1. a draining name whose live host carries no drain mark (the pod restarted, a stop/start, a
//!    drain leave that did not commit) → [`super::DrainHost`] of that host;
//! 2. a wanted name whose live host carries a mark (the operator's `world-drain`) →
//!    [`super::DrainHost`] of the row only: the row follows the mark;
//! 2b. a wanted name with no live host whose NEWEST `hosts` row (the highest id of that name) is
//!    clean and carries a mark (the operator's drain completed before the ticker saw the marked
//!    host live: `worldctl drain … --in 0`; the host left and stays up drained) →
//!    [`super::DrainHost`] of the row only — else nothing would ever remove that pod. A newest
//!    row that is clean WITHOUT a mark (a SIGTERM leave: `world-stop`, a release) changes
//!    nothing: its pod comes back as a new host of the same name. Residual: a marked row pruned
//!    before the ticker read it looks like a pod that has not started yet (the name stays
//!    wanted, its drained pod stays up until an operator removes it);
//! 3. a draining name none of whose `hosts` rows is live-and-dirty (pruned rows count as none) →
//!    [`super::RemoveHost`] — also when its host left still holding land or in-flight rows: its
//!    `Leave` succession needs only the clean `hosts` row (the store moves the land to an heir;
//!    the drained process takes no part and writes nothing, even at its SIGTERM), not the desired
//!    row or the pod — and the slot must be free for that heir to be added at all;
//! 4. fewer wanted rows than the minimum → [`super::AddHost`];
//! 5. **scale up**: the scale gates hold (below), fewer wanted rows than the maximum, and for
//!    `up_after_ms` without a break every read showed the least-loaded serving host's
//!    `tick_permille` ≥ [`SCALE_UP_LOAD_PERMILLE`] → [`super::AddHost`];
//! 6. **scale down**: the scale gates hold, more wanted rows than `max(min, 1)`, every serving
//!    host known for ≥ `min_lifetime_ms`, and for `down_after_ms` without a break every read
//!    showed `Σ tick_permille ≤ (n − 1) · (1000 − SCALE_DOWN_SPARE_PERMILLE)` over the `n`
//!    serving hosts → [`super::DrainHost`] of the victim: fewest players, then fewest running
//!    regions, then the highest host id (the youngest); notice 0 with no players, else
//!    [`DRAIN_WARNING_SECS`];
//! 7. **to zero** (only with `min == 0`): the scale gates hold, ONE wanted name is left, its
//!    serving host known for ≥ `min_lifetime_ms`, and for `down_after_ms` without a break every
//!    read showed it with 0 players and 0 running regions → [`super::DrainHost`] of that host
//!    (notice 0). A host added for a login is as old as any: not drained before its lifetime.
//!
//! **The scale gates** (rules 5, 6 and 7): no desired row draining, no succession run open (or being
//! declared, a rollback in flight, or the open runs not known yet), and every wanted name has a
//! **serving** host — its live host, attached, taking logins (not leaving, not
//! marked). **The timers** (`fleet_up_since`, `fleet_down_since`, `fleet_zero_since`, on the
//! lease watch's clock) move
//! only on a NEW `hosts` read (a read is the evidence; an upkeep between two reads moves
//! nothing): a read that shows a rule's condition starts its count if none runs, a read that
//! breaks it (or a failed read) clears it, and so does every fleet decision's answer. A new
//! ticker starts all three at `None` (its watch counts from zero: delayed, never hastened). After a
//! scale-down the remaining `n − 1` hosts sit at ≤ 300 ‰ on average, half the scale-up
//! threshold: the two rules cannot chase each other.
//!
//! Rules 1–3 follow drains, which only `claim` hosts heed (a `static` host ignores a mark): they
//! run only under [`crate::CrashPolicy::Succession`]; rule 4 runs under `static` too (min = max
//! = 1); rules 5, 6 and 7 need `Succession` as well.
//!
//! **A login when no host takes logins** (decision 5; [`Coordinator::login_gate`]): the world's
//! gate host takes the order ([`Gate::Host`], whichever replica it reached — the gate is
//! world-wide); with no serving host ([`Gate::Nobody`]) and fleet dials set,
//! the replica answers `Starting` and applies [`super::Input::StartWanted`]: ANY replica (login
//! orders reach whichever replica the login service dialled), logged and replayed, it issues
//! [`super::AddHost`] `{ max, expect_wanted: 0 }` itself — at most once per
//! [`WORLD_START_ASK_EVERY_MS`] per replica. Two replicas asking at once add ONE row: the store
//! adds only while no row is wanted (`NotNeeded` otherwise), and the two transitions are
//! serialised by the row's compare-and-set. Without dials, `Nobody` is `NoGate`, as before.
//! With zero hosts no cut begins (no expected host), the calendar runs on, free land stays free.

use serde::{Deserialize, Serialize};

use super::{
    AddHost, Coordinator, DecisionAnswer, DrainHost, HostAdded, HostDrained, HostRemoved, HostSeen,
    RemoveHost, StoreDecision,
};
use crate::{CrashPolicy, DRAIN_WARNING_SECS, HOST_LIVE, HostId};

/// One `desired_hosts` row: a host the coordinator wants to exist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredHost {
    /// `h<n>`, never issued twice (`world.next_desired_host`): the host's Deployment name.
    pub name: String,
    /// The lowest slot free at its add; reused once its row is removed.
    pub slot: u32,
    /// Marked draining ([`super::DrainHost`]): it leaves, and still holds its slot until removed.
    pub draining: bool,
    /// The store's clock (ms since the epoch) at its add.
    pub created_ms: u64,
}

/// How long (ms) the least-loaded serving host must stay loaded before a host is added (scale
/// up, rule 5): the default of `VOXEL_SCALE_UP_AFTER_MS`.
pub const SCALE_UP_AFTER_MS: u64 = 60_000;
/// How long (ms) the hosts must stay idle enough before one is drained (scale down, rule 6; also
/// the quiet time before the last host goes): the default of `VOXEL_SCALE_DOWN_AFTER_MS`.
pub const SCALE_DOWN_AFTER_MS: u64 = 300_000;
/// How long (ms) a host must have been known before it may be drained by load (rule 6; the lease
/// watch's [`super::LeaseWatch::known_for`]): the default of `VOXEL_HOST_MIN_LIFETIME_MS`.
pub const HOST_MIN_LIFETIME_MS: u64 = 600_000;
/// At most one fleet decision per this many store ms.
pub const SCALE_DECISION_EVERY_MS: u64 = 10_000;
/// A replica asks for the world's first host ([`super::Input::StartWanted`]: a login found no
/// host taking logins) at most once per this many store ms; the clients
/// retry their join every second meanwhile.
pub const WORLD_START_ASK_EVERY_MS: u64 = 5_000;

/// Scale up (rule 5) when the LEAST-loaded serving host's worst tick stays at or above this, in
/// permille of the tick period: every host is busy, not one crowded super-region. Assignment
/// already avoids hosts above `1000 − ASSIGN_HEADROOM_MIN_PERMILLE` (800): a host is added
/// before none qualifies.
pub const SCALE_UP_LOAD_PERMILLE: u32 = 600;
/// Scale down (rule 6) when the serving hosts' summed load fits in one host fewer with this much
/// spare per host, in permille: `Σ ≤ (n − 1) · (1000 − SCALE_DOWN_SPARE_PERMILLE)` — after the
/// removal the others average ≤ 300 ‰.
pub const SCALE_DOWN_SPARE_PERMILLE: u32 = 700;

// A host is never drained by load before assignment would prefer it, nor before its idle land
// could have been released.
const _: () = assert!(HOST_MIN_LIFETIME_MS > super::ASSIGN_MIN_ATTACHED_MS);
const _: () = assert!(HOST_MIN_LIFETIME_MS > crate::LAND_IDLE_MS);
// A host is added before assignment refuses every host for its load.
const _: () = assert!(SCALE_UP_LOAD_PERMILLE < 1000 - super::ASSIGN_HEADROOM_MIN_PERMILLE);
// After a scale-down the remaining hosts average below the scale-up threshold: the two rules
// cannot chase each other (`(n − 1) · 300` over `n − 1` hosts is ≤ 300 < 600).
const _: () = assert!(SCALE_DOWN_SPARE_PERMILLE < 1000);
const _: () = assert!(1000 - SCALE_DOWN_SPARE_PERMILLE < SCALE_UP_LOAD_PERMILLE);

/// Which rule a fleet decision came from (the module docs; the window line counts the load
/// rules, `scale=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetWhy {
    /// Rule 1: a draining name's live host carries no mark.
    MarkHost,
    /// Rule 2: a wanted name's live host carries a mark.
    FollowMark,
    /// Rule 2b: a wanted name's newest row left clean with a mark.
    LeftMarked,
    /// Rule 3: a draining name nothing runs for (a leaver that kept land included).
    Remove,
    /// Rule 4: below the minimum.
    Minimum,
    /// Rule 5: scale up under load.
    ScaleUp,
    /// Rule 6: scale down, the load fits in fewer.
    ScaleDown,
    /// Rule 7: the last host goes, nobody online (`min == 0`).
    ToZero,
    /// Elastic 5, rule 6b: an evacuated scale-down victim drains (at once when its land holds
    /// nobody, else with the warning: its moves gave up).
    Evacuated,
}

impl FleetWhy {
    /// The word the window line's `scale=` uses for a load rule; `None` for the others.
    pub fn scale_word(self) -> Option<&'static str> {
        match self {
            FleetWhy::ScaleUp => Some("up"),
            FleetWhy::ScaleDown | FleetWhy::Evacuated => Some("down"),
            FleetWhy::ToZero => Some("zero"),
            _ => None,
        }
    }
}

/// The scale gates' view of one `hosts` read (rules 5, 6 and 7): the serving host of every wanted
/// name, or `None` when a gate fails (the module docs).
struct Serving<'a> {
    /// One per wanted name, its live host (attached, taking logins).
    hosts: Vec<&'a HostSeen>,
}

impl Serving<'_> {
    fn least(&self) -> u32 {
        self.hosts
            .iter()
            .map(|h| h.load.tick_permille)
            .min()
            .unwrap_or(0)
    }
    fn sum(&self) -> u64 {
        self.hosts
            .iter()
            .map(|h| u64::from(h.load.tick_permille))
            .sum()
    }
    /// `(n − 1) · (1000 − SCALE_DOWN_SPARE_PERMILLE)`: the summed load that fits in one host fewer.
    fn fits_fewer(&self) -> u64 {
        (self.hosts.len() as u64).saturating_sub(1) * u64::from(1000 - SCALE_DOWN_SPARE_PERMILLE)
    }
    /// The evidence of a log line: each serving host's name, id, load, players and regions.
    fn describe(&self) -> String {
        let parts: Vec<String> = self
            .hosts
            .iter()
            .map(|h| {
                format!(
                    "{} (host {}) {}‰ {}p {}r",
                    h.name, h.ordinal, h.load.tick_permille, h.load.players, h.load.regions
                )
            })
            .collect();
        parts.join(", ")
    }
}

/// Where a login order of the login service goes ([`Coordinator::login_gate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// The world's gate host ([`Coordinator::gate_host`]): the order is relayed to it.
    Host(HostId),
    /// No attached live host takes logins (none attached, every one leaving or draining): with
    /// fleet dials, `Starting` and [`super::Input::StartWanted`]; without, `NoGate`.
    Nobody,
}

/// The fleet's dials (decision 8; parsed by the server from `VOXEL_FLEET_MIN`,
/// `VOXEL_FLEET_MAX`, `VOXEL_SCALE_UP_AFTER_MS`, `VOXEL_SCALE_DOWN_AFTER_MS`,
/// `VOXEL_HOST_MIN_LIFETIME_MS` — the core reads no environment): the fewest and the most
/// desired rows, and the paces of the load rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetDials {
    pub min: u32,
    pub max: u32,
    pub up_after_ms: u64,
    pub down_after_ms: u64,
    pub min_lifetime_ms: u64,
}

impl Coordinator {
    /// The fleet's dials (the runtime's configuration, a process field: a rebase keeps it);
    /// `None` (the default): no fleet decision at all.
    pub fn set_fleet(&mut self, dials: Option<FleetDials>) {
        self.fleet_dials = dials;
    }

    /// The fleet's dials as set ([`Self::set_fleet`]).
    pub fn fleet_dials(&self) -> Option<FleetDials> {
        self.fleet_dials
    }

    /// The desired rows of the ticker's latest successful `hosts` read (`None`: none since this
    /// replica became the ticker, or the last read failed).
    pub fn fleet_desired(&self) -> Option<&[DesiredHost]> {
        self.fleet_desired.as_deref()
    }

    /// The upkeep's fleet rules (the module docs): at most one decision handed out, with the
    /// next transition.
    pub(super) fn maintain_fleet(&mut self, now_ms: u64) {
        let Some(dials) = self.fleet_dials else {
            return;
        };
        if self.fleet_pending || !self.ticking_now() {
            return;
        }
        // Cloned: the evacuation step below changes the core (a handful of rows).
        let Some(desired) = self.fleet_desired.clone() else {
            return;
        };
        if self
            .fleet_read_after
            .is_some_and(|n| self.leases.reads() <= n)
        {
            return;
        }
        if self
            .fleet_last_ms
            .is_some_and(|at| now_ms.saturating_sub(at) < SCALE_DECISION_EVERY_MS)
        {
            return;
        }
        // Elastic 5, rule 6b (ahead of rules 4–7, no scale gates): an evacuating host's next step.
        if let Some(drain) = self.evacuation_step(now_ms) {
            log::info!("coordinator: fleet: {:?}: {drain:?}", FleetWhy::Evacuated);
            self.fleet_pending = true;
            self.fleet_why = Some(FleetWhy::Evacuated);
            self.push_decision(StoreDecision::DrainHost(drain));
            return;
        }
        if self.move_open().is_some() {
            // One move at a time, and no other fleet decision under it but the marks' rules.
            return;
        }
        let Some((why, decision)) = self.fleet_rule(dials, &desired) else {
            return;
        };
        // Elastic 5, rule 6 with players: the victim is emptied by moves first, drained after.
        if let (
            FleetWhy::ScaleDown,
            StoreDecision::DrainHost(DrainHost {
                name,
                host: Some((victim, boot)),
                ..
            }),
        ) = (&why, &decision)
        {
            if self.leases.row(*victim).is_some_and(|r| r.load.players > 0) {
                log::info!(
                    "coordinator: fleet: scale down: host {victim} ({name}) holds players: its occupied land moves first"
                );
                self.start_evacuation(*victim, *boot, name.clone(), now_ms);
                return;
            }
        }
        // Rules 2b, 5, 6 and 7 logged their evidence already, with the decision.
        if !matches!(
            why,
            FleetWhy::LeftMarked | FleetWhy::ScaleUp | FleetWhy::ScaleDown | FleetWhy::ToZero
        ) {
            log::info!("coordinator: fleet: {why:?}: {decision:?}");
        }
        self.fleet_pending = true;
        self.fleet_why = Some(why);
        self.push_decision(decision);
    }

    /// The first of rules 1–7 that applies to the latest read's `desired` rows and `hosts` rows
    /// (the lease watch's), if any, with the rule it came from.
    fn fleet_rule(
        &self,
        dials: FleetDials,
        desired: &[DesiredHost],
    ) -> Option<(FleetWhy, StoreDecision)> {
        if self.crash_policy() == CrashPolicy::Succession {
            // 1. A draining name whose live host carries no mark: that host is marked.
            for d in desired.iter().filter(|d| d.draining) {
                if let Some(live) = self.live_host_of(&d.name).filter(|r| !r.draining) {
                    let drain = DrainHost {
                        name: d.name.clone(),
                        host: Some((live.ordinal, live.boot)),
                        in_ms: drain_notice_ms(live),
                    };
                    return Some((FleetWhy::MarkHost, StoreDecision::DrainHost(drain)));
                }
            }
            // 2. A wanted name whose live host carries a mark: the row follows it.
            for d in desired.iter().filter(|d| !d.draining) {
                if self.live_host_of(&d.name).is_some_and(|r| r.draining) {
                    return Some((
                        FleetWhy::FollowMark,
                        StoreDecision::DrainHost(DrainHost {
                            name: d.name.clone(),
                            host: None,
                            in_ms: 0,
                        }),
                    ));
                }
            }
            // 2b. A wanted name with no live host whose newest row left clean WITH a mark (a
            // drain that completed before the ticker saw the marked host live): the row follows
            // the mark too. A clean newest row without one (a SIGTERM leave) changes nothing.
            // A clean row keeps `drain_at_ms` (nothing in the store clears it), so the read
            // shows the mark. Residual: a row pruned before this read looks like a pod that has
            // not started yet — the name stays wanted.
            for d in desired.iter().filter(|d| !d.draining) {
                if self.live_host_of(&d.name).is_some() {
                    continue;
                }
                let newest = self
                    .leases
                    .rows()
                    .filter(|r| r.name == d.name)
                    .max_by_key(|r| r.ordinal);
                if let Some(r) = newest.filter(|r| r.clean && r.draining) {
                    log::info!(
                        "[coordinator] fleet: host {} has no live host and its newest host {} left drained: its row drains too",
                        d.name,
                        r.ordinal
                    );
                    return Some((
                        FleetWhy::LeftMarked,
                        StoreDecision::DrainHost(DrainHost {
                            name: d.name.clone(),
                            host: None,
                            in_ms: 0,
                        }),
                    ));
                }
            }
            // 3. A draining name nothing of which runs any more: removed — whether or not its
            // leaver kept land. The land waits in the store for its `Leave` succession, which
            // needs the clean `hosts` row only (never this row or the drained pod), and its heir
            // can exist only once this slot is free (final review, A-I1).
            for d in desired.iter().filter(|d| d.draining) {
                let runs = self
                    .leases
                    .rows()
                    .any(|r| r.name == d.name && !r.clean && r.state == HOST_LIVE);
                if !runs {
                    return Some((
                        FleetWhy::Remove,
                        StoreDecision::RemoveHost(RemoveHost {
                            name: d.name.clone(),
                        }),
                    ));
                }
            }
        }
        // 4. Below the minimum.
        let wanted =
            u32::try_from(desired.iter().filter(|d| !d.draining).count()).unwrap_or(u32::MAX);
        if wanted < dials.min {
            return Some((
                FleetWhy::Minimum,
                StoreDecision::AddHost(AddHost {
                    max: dials.max,
                    expect_wanted: wanted,
                }),
            ));
        }
        // 5, 6 and 7 need the scale gates (and `Succession`, checked by `fleet_serving`), checked
        // again here: the attachments and the succession runs may have moved since the read.
        let serving = self.fleet_serving(desired)?;
        let watch_now = self.leases.latest_received_ms();
        // 5. Scale up: the least-loaded serving host stayed loaded for `up_after_ms`.
        if let Some(since) = self.fleet_up_since {
            let held = watch_now.saturating_sub(since);
            if wanted < dials.max
                && serving.least() >= SCALE_UP_LOAD_PERMILLE
                && held >= dials.up_after_ms
            {
                log::info!(
                    "[coordinator] fleet: scale up: the least-loaded serving host stayed at ≥ {SCALE_UP_LOAD_PERMILLE}‰ for {held} ms \
                     (now {}‰; wanted {wanted} < max {}): {}",
                    serving.least(),
                    dials.max,
                    serving.describe()
                );
                return Some((
                    FleetWhy::ScaleUp,
                    StoreDecision::AddHost(AddHost {
                        max: dials.max,
                        expect_wanted: wanted,
                    }),
                ));
            }
        }
        // 6. Scale down: the load fit in one host fewer for `down_after_ms`, every serving host
        // older than the minimum lifetime.
        if let Some(since) = self.fleet_down_since {
            let held = watch_now.saturating_sub(since);
            let floor = dials.min.max(1);
            let old_enough = serving
                .hosts
                .iter()
                .all(|h| self.leases.known_for(h.ordinal, watch_now) >= dials.min_lifetime_ms);
            if wanted > floor
                && serving.sum() <= serving.fits_fewer()
                && held >= dials.down_after_ms
                && old_enough
            {
                // The victim: fewest players, then fewest running regions, then the youngest.
                let victim = serving
                    .hosts
                    .iter()
                    .min_by_key(|h| (h.load.players, h.load.regions, std::cmp::Reverse(h.ordinal)))
                    .copied()?;
                let in_ms = drain_notice_ms(victim);
                log::info!(
                    "[coordinator] fleet: scale down: Σ {}‰ ≤ {}‰ over {} serving hosts for {held} ms (wanted {wanted} > {floor}); \
                     victim {} (host {}): fewest players {}, then regions {}, then the youngest; notice {in_ms} ms: {}",
                    serving.sum(),
                    serving.fits_fewer(),
                    serving.hosts.len(),
                    victim.name,
                    victim.ordinal,
                    victim.load.players,
                    victim.load.regions,
                    serving.describe()
                );
                let drain = DrainHost {
                    name: victim.name.clone(),
                    host: Some((victim.ordinal, victim.boot)),
                    in_ms,
                };
                return Some((FleetWhy::ScaleDown, StoreDecision::DrainHost(drain)));
            }
        }
        // 7. To zero (`min == 0` only): the one wanted name's serving host had nobody and ran
        // nothing for `down_after_ms`, and is older than the minimum lifetime.
        if let Some(since) = self.fleet_zero_since {
            let held = watch_now.saturating_sub(since);
            if let [last] = serving.hosts[..] {
                let known = self.leases.known_for(last.ordinal, watch_now);
                if dials.min == 0
                    && wanted == 1
                    && idle(last)
                    && held >= dials.down_after_ms
                    && known >= dials.min_lifetime_ms
                {
                    log::info!(
                        "[coordinator] fleet: to zero: the last host {} (host {}) had 0 players and 0 running regions for {held} ms \
                         (known for {known} ms, min 0): it drains",
                        last.name,
                        last.ordinal
                    );
                    let drain = DrainHost {
                        name: last.name.clone(),
                        host: Some((last.ordinal, last.boot)),
                        in_ms: drain_notice_ms(last),
                    };
                    return Some((FleetWhy::ToZero, StoreDecision::DrainHost(drain)));
                }
            }
        }
        None
    }

    /// Where a login order goes (decision 5; the module docs): the world's gate host, or nobody
    /// serving.
    pub fn login_gate(&self) -> Gate {
        match self.gate_host() {
            Some(h) => Gate::Host(h),
            None => Gate::Nobody,
        }
    }

    /// [`super::Input::StartWanted`]: a login found [`Gate::Nobody`] — the world's first host is
    /// asked for, `AddHost { max, expect_wanted: 0 }`, at most once per
    /// [`WORLD_START_ASK_EVERY_MS`] on this replica (any replica; logged and replayed: a replay
    /// whose ask a conflict's rebase dropped asks again — `rebase` clears the pacing when a write
    /// was in flight or decisions queued, a refresh keeps it — and the store answers
    /// `NotNeeded` once a wanted row exists). Nothing without fleet dials, or once a
    /// host serves again (a replay after the first host attached).
    pub(super) fn on_start_wanted(&mut self, now_ms: u64) {
        let Some(dials) = self.fleet_dials else {
            return;
        };
        if self.login_gate() != Gate::Nobody {
            log::debug!(
                "coordinator: a start was wanted, and a host takes logins now: nothing asked"
            );
            return;
        }
        if self
            .start_asked_ms
            .is_some_and(|at| now_ms.saturating_sub(at) < WORLD_START_ASK_EVERY_MS)
        {
            return;
        }
        self.start_asked_ms = Some(now_ms);
        let add = AddHost {
            max: dials.max,
            expect_wanted: 0,
        };
        log::info!(
            "[coordinator] fleet: a login found no host taking logins on any replica: the world starts ({add:?})"
        );
        self.push_decision(StoreDecision::AddHost(add));
    }

    /// The scale gates (rules 5, 6 and 7; the module docs) over the latest read: the serving host
    /// of every wanted name, or `None` — not `Succession`, a desired row draining, a succession
    /// run open (or being declared, a rollback in flight, the open runs not known yet), no wanted
    /// name, or a wanted name without a serving host (its pod starting or crash-looping).
    fn fleet_serving<'a>(&'a self, desired: &[DesiredHost]) -> Option<Serving<'a>> {
        if self.crash_policy() != CrashPolicy::Succession || desired.iter().any(|d| d.draining) {
            return None;
        }
        if !self.successions_known()
            || self.successions_declaring().is_some()
            || self.successions_rolling().is_some()
            || self.successions_open_count() > 0
        {
            return None;
        }
        let attached = self.attached_live();
        let mut hosts = Vec::new();
        for d in desired {
            let live = self.live_host_of(&d.name)?;
            if !attached.contains(&live.ordinal) || !self.takes_logins(live.ordinal) {
                return None;
            }
            hosts.push(live);
        }
        (!hosts.is_empty()).then_some(Serving { hosts })
    }

    /// A new `hosts` read arrived (`on_hosts_read`, after it set `fleet_desired` and the watch's
    /// rows): the load timers move — and only here, a read being the evidence. A read showing a
    /// rule's condition starts its count (on the watch's clock, the read's receive stamp) if none
    /// runs; a read that breaks it clears it.
    pub(super) fn fleet_observe_load(&mut self) {
        let Some(dials) = self.fleet_dials else {
            return;
        };
        let (up, down, zero) = match self
            .fleet_desired
            .as_deref()
            .and_then(|d| self.fleet_serving(d).map(|s| (d, s)))
        {
            Some((desired, serving)) => {
                let wanted = u32::try_from(desired.len()).unwrap_or(u32::MAX);
                (
                    wanted < dials.max && serving.least() >= SCALE_UP_LOAD_PERMILLE,
                    wanted > dials.min.max(1) && serving.sum() <= serving.fits_fewer(),
                    dials.min == 0
                        && wanted == 1
                        && matches!(serving.hosts[..], [last] if idle(last)),
                )
            }
            None => (false, false, false),
        };
        let at = self.leases.latest_received_ms();
        self.fleet_up_since = if up {
            Some(self.fleet_up_since.unwrap_or(at))
        } else {
            None
        };
        self.fleet_down_since = if down {
            Some(self.fleet_down_since.unwrap_or(at))
        } else {
            None
        };
        self.fleet_zero_since = if zero {
            Some(self.fleet_zero_since.unwrap_or(at))
        } else {
            None
        };
    }

    /// The rule of the fleet decision handed out and not answered yet (the runtime reads it
    /// before it applies the answer: the window line's `scale=`).
    pub fn fleet_why(&self) -> Option<FleetWhy> {
        self.fleet_why
    }

    /// The live host of desired row `name`: its live dirty `hosts` row in the latest read — the
    /// newest one still beating (a registration lapses its name's older rows: `seen_ms = 0`,
    /// still live and dirty until ended), else the newest. `None`: none.
    fn live_host_of(&self, name: &str) -> Option<&HostSeen> {
        self.leases
            .rows()
            .filter(|r| r.name == name && !r.clean && r.state == HOST_LIVE)
            .max_by_key(|r| (r.seen_ms != 0, r.ordinal))
    }

    /// A fleet decision's answer ([`super::Input::Decision`], logged: a replay applies it again,
    /// which only paces again): nothing is awaited any more, the next decision waits
    /// [`SCALE_DECISION_EVERY_MS`] and for a `hosts` read issued after now (the reads are issued
    /// one at a time: the one in flight may predate the commit, the one after it cannot).
    /// Nothing durable changes; the table's new state comes with that read.
    pub(super) fn fleet_answered(&mut self, answer: &DecisionAnswer, now_ms: u64) {
        match answer {
            DecisionAnswer::HostAdded { add, outcome, .. } => match outcome {
                Ok(HostAdded::Done { name, slot }) => {
                    log::info!("coordinator: fleet: host {name} added at slot {slot}")
                }
                Ok(HostAdded::NotNeeded) => {
                    log::info!("coordinator: fleet: no host added ({add:?}): the wanted rows moved")
                }
                Ok(HostAdded::Full) => {
                    log::warn!(
                        "coordinator: fleet: no host added: all {} slots are taken (a draining host holds one until removed)",
                        add.max
                    )
                }
                Err(e) => log::warn!(
                    "coordinator: fleet: adding a host failed ({e}); decided again after the next read"
                ),
            },
            DecisionAnswer::HostDrained { drain, outcome, .. } => match outcome {
                Ok(HostDrained::Marked) => log::info!(
                    "coordinator: fleet: host {} drains (its host {:?} marked, in {} ms)",
                    drain.name,
                    drain.host,
                    drain.in_ms
                ),
                Ok(HostDrained::RowOnly) => log::info!(
                    "coordinator: fleet: host {} drains (its row only)",
                    drain.name
                ),
                Ok(HostDrained::Unknown) => log::info!(
                    "coordinator: fleet: host {} is not a desired row: nothing drained",
                    drain.name
                ),
                Err(e) => log::warn!(
                    "coordinator: fleet: draining host {} failed ({e}); decided again after the next read",
                    drain.name
                ),
            },
            DecisionAnswer::HostRemoved {
                remove, outcome, ..
            } => match outcome {
                Ok(HostRemoved::Done) => {
                    log::info!("coordinator: fleet: host {} removed", remove.name)
                }
                Ok(HostRemoved::Gone) => log::info!(
                    "coordinator: fleet: host {} was removed already",
                    remove.name
                ),
                Err(e) => log::warn!(
                    "coordinator: fleet: removing host {} failed ({e}); decided again after the next read",
                    remove.name
                ),
            },
            _ => return,
        }
        self.fleet_settled(now_ms);
    }

    /// A fleet decision handed out is over (answered, or lost with a write that may have
    /// committed): see [`Self::fleet_answered`].
    pub(super) fn fleet_settled(&mut self, now_ms: u64) {
        self.fleet_pending = false;
        self.fleet_why = None;
        // Any fleet decision restarts the load counts: its effect comes with the next reads.
        self.fleet_up_since = None;
        self.fleet_down_since = None;
        self.fleet_zero_since = None;
        self.fleet_last_ms = Some(self.fleet_last_ms.map_or(now_ms, |at| at.max(now_ms)));
        self.fleet_read_after = Some(self.leases.reads().saturating_add(1));
    }

    /// The `hosts` read this replica's rules stood on is gone (a failed read, a change of the
    /// ticker's role): no rule until the next good one, and the load counts start again (a
    /// new ticker counts from zero: delayed, never hastened; a failed read is no evidence).
    pub(super) fn fleet_unread(&mut self) {
        self.fleet_desired = None;
        self.fleet_up_since = None;
        self.fleet_down_since = None;
        self.fleet_zero_since = None;
    }
}

/// Whether `host` reported nobody and nothing running (rule 7).
fn idle(host: &HostSeen) -> bool {
    host.load.players == 0 && host.load.regions == 0
}

/// A drain's notice for `host`: none when it reports no players, else [`DRAIN_WARNING_SECS`].
fn drain_notice_ms(host: &HostSeen) -> u64 {
    if host.load.players == 0 {
        0
    } else {
        DRAIN_WARNING_SECS * 1000
    }
}
