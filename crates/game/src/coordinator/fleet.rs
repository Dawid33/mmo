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
//! 3. a draining name none of whose `hosts` rows is live-and-dirty or `landed` (pruned rows count
//!    as none) → [`super::RemoveHost`] — a leaver that kept land (in-flight rows) holds its row
//!    until its `Leave` succession moved it;
//! 4. fewer wanted rows than the minimum → [`super::AddHost`].
//!
//! Rules 1–3 follow drains, which only `claim` hosts heed (a `static` host ignores a mark): they
//! run only under [`crate::CrashPolicy::Succession`]; rule 4 runs under `static` too (min = max
//! = 1). Scaling by load (rules 5–7) is a later task.

use serde::{Deserialize, Serialize};

use super::{AddHost, Coordinator, DecisionAnswer, DrainHost, HostAdded, HostDrained, HostRemoved, HostSeen, RemoveHost, StoreDecision};
use crate::{CrashPolicy, DRAIN_WARNING_SECS, HOST_LIVE};

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
/// up, a later task): the default of `VOXEL_SCALE_UP_AFTER_MS`.
pub const SCALE_UP_AFTER_MS: u64 = 60_000;
/// How long (ms) the hosts must stay idle enough before one is drained (scale down, a later
/// task; also the quiet time before the last host goes): the default of
/// `VOXEL_SCALE_DOWN_AFTER_MS`.
pub const SCALE_DOWN_AFTER_MS: u64 = 300_000;
/// How long (ms) a host must have been known before it may be drained by load (a later task):
/// the default of `VOXEL_HOST_MIN_LIFETIME_MS`.
pub const HOST_MIN_LIFETIME_MS: u64 = 600_000;
/// At most one fleet decision per this many store ms.
pub const SCALE_DECISION_EVERY_MS: u64 = 10_000;

// A host is never drained by load before assignment would prefer it, nor before its idle land
// could have been released.
const _: () = assert!(HOST_MIN_LIFETIME_MS > super::ASSIGN_MIN_ATTACHED_MS);
const _: () = assert!(HOST_MIN_LIFETIME_MS > crate::LAND_IDLE_MS);

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
        let Some(dials) = self.fleet_dials else { return };
        if self.fleet_pending || !self.ticking_now() {
            return;
        }
        let Some(desired) = self.fleet_desired.as_deref() else { return };
        if self.fleet_read_after.is_some_and(|n| self.leases.reads() <= n) {
            return;
        }
        if self.fleet_last_ms.is_some_and(|at| now_ms.saturating_sub(at) < SCALE_DECISION_EVERY_MS) {
            return;
        }
        let Some(decision) = self.fleet_rule(dials, desired) else { return };
        log::info!("coordinator: fleet: {decision:?}");
        self.fleet_pending = true;
        self.push_decision(decision);
    }

    /// The first of rules 1–4 that applies to the latest read's `desired` rows and `hosts` rows
    /// (the lease watch's), if any.
    fn fleet_rule(&self, dials: FleetDials, desired: &[DesiredHost]) -> Option<StoreDecision> {
        if self.crash_policy() == CrashPolicy::Succession {
            // 1. A draining name whose live host carries no mark: that host is marked.
            for d in desired.iter().filter(|d| d.draining) {
                if let Some(live) = self.live_host_of(&d.name).filter(|r| !r.draining) {
                    let in_ms = if live.load.players == 0 { 0 } else { DRAIN_WARNING_SECS * 1000 };
                    return Some(StoreDecision::DrainHost(DrainHost {
                        name: d.name.clone(),
                        host: Some((live.ordinal, live.boot)),
                        in_ms,
                    }));
                }
            }
            // 2. A wanted name whose live host carries a mark: the row follows it.
            for d in desired.iter().filter(|d| !d.draining) {
                if self.live_host_of(&d.name).is_some_and(|r| r.draining) {
                    return Some(StoreDecision::DrainHost(DrainHost { name: d.name.clone(), host: None, in_ms: 0 }));
                }
            }
            // 3. A draining name nothing of which runs or holds land any more: removed.
            for d in desired.iter().filter(|d| d.draining) {
                let holds = self.leases.rows().any(|r| r.name == d.name && ((!r.clean && r.state == HOST_LIVE) || r.landed));
                if !holds {
                    return Some(StoreDecision::RemoveHost(RemoveHost { name: d.name.clone() }));
                }
            }
        }
        // 4. Below the minimum.
        let wanted = u32::try_from(desired.iter().filter(|d| !d.draining).count()).unwrap_or(u32::MAX);
        (wanted < dials.min).then_some(StoreDecision::AddHost(AddHost { max: dials.max, expect_wanted: wanted }))
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
                Ok(HostAdded::Done { name, slot }) => log::info!("coordinator: fleet: host {name} added at slot {slot}"),
                Ok(HostAdded::NotNeeded) => log::info!("coordinator: fleet: no host added ({add:?}): the wanted rows moved"),
                Ok(HostAdded::Full) => {
                    log::warn!("coordinator: fleet: no host added: all {} slots are taken (a draining host holds one until removed)", add.max)
                }
                Err(e) => log::warn!("coordinator: fleet: adding a host failed ({e}); decided again after the next read"),
            },
            DecisionAnswer::HostDrained { drain, outcome, .. } => match outcome {
                Ok(HostDrained::Marked) => log::info!("coordinator: fleet: host {} drains (its host {:?} marked, in {} ms)", drain.name, drain.host, drain.in_ms),
                Ok(HostDrained::RowOnly) => log::info!("coordinator: fleet: host {} drains (its row only)", drain.name),
                Ok(HostDrained::Unknown) => log::info!("coordinator: fleet: host {} is not a desired row: nothing drained", drain.name),
                Err(e) => log::warn!("coordinator: fleet: draining host {} failed ({e}); decided again after the next read", drain.name),
            },
            DecisionAnswer::HostRemoved { remove, outcome, .. } => match outcome {
                Ok(HostRemoved::Done) => log::info!("coordinator: fleet: host {} removed", remove.name),
                Ok(HostRemoved::Gone) => log::info!("coordinator: fleet: host {} was removed already", remove.name),
                Err(e) => log::warn!("coordinator: fleet: removing host {} failed ({e}); decided again after the next read", remove.name),
            },
            _ => return,
        }
        self.fleet_settled(now_ms);
    }

    /// A fleet decision handed out is over (answered, or lost with a write that may have
    /// committed): see [`Self::fleet_answered`].
    pub(super) fn fleet_settled(&mut self, now_ms: u64) {
        self.fleet_pending = false;
        self.fleet_last_ms = Some(self.fleet_last_ms.map_or(now_ms, |at| at.max(now_ms)));
        self.fleet_read_after = Some(self.leases.reads().saturating_add(1));
    }

    /// The `hosts` read this replica's rules stood on is gone (a failed read, a change of the
    /// ticker's role): no rule until the next good one.
    pub(super) fn fleet_unread(&mut self) {
        self.fleet_desired = None;
    }
}
