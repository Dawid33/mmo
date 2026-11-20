//! The coordinator assigns land (Elastic 3, plan `2026-11-18-elastic-3-land-assignment.md`,
//! "Decisions settled in planning" 1). A host that needs a region of a super-region it knows no
//! reachable owner of asks ([`crate::HostMsg::LandNeeded`]); the ask is queued with its asker in
//! the projection ([`Coordinator::land_asked`], any replica), the ticker's runtime reads the
//! asked rows ([`Coordinator::take_land_lookups`] → [`super::Input::LandOwners`]) and
//! [`Coordinator::on_land_read`] answers, per asked super-region:
//! - **absent or free**: [`StoreDecision::AssignLand`] to the host [`Coordinator::pick_assignee`]
//!   chooses (none: nothing, the hosts ask again); at most [`ASSIGN_PER_PASS_MAX`] per read, the
//!   rest queued again for the next;
//! - **owned by an attached live host**: [`CoordMsg::Assigned`] to it if it asked itself (its
//!   `Assigned` was lost), [`CoordMsg::LandIs`] to every other asker;
//! - **owned by anyone else**: [`Coordinator::on_land_owner`], as before (a `Leave` succession for
//!   a departed owner).
//!
//! The store's answer ([`super::DecisionAnswer::Assigned`], a logged input) is applied by
//! [`Coordinator::assign_answered`]: `Done` tells the assignee its row and the other askers the
//! owner; any refusal tells nobody (the hosts ask again within a second). Assignments carry no
//! mark: the store's `WHERE owner = NO_OWNER` refuses a duplicate, and an answer applied twice
//! (a replay) only sends its messages again. Under `Static` (and all-in-one) nothing is
//! assigned: the paths run only where successions do.
//!
//! **Who is chosen.** The attached live hosts that take logins (neither draining nor leaving) and
//! have a live dirty `hosts` row in the ticker's latest read; then, each only if it leaves one,
//! those this ticker has known for [`ASSIGN_MIN_ATTACHED_MS`] (the crash-loop guard), then those
//! with [`ASSIGN_HEADROOM_MIN_PERMILLE`] of tick headroom. Among the rest the fewest players, then
//! the fewest regions — each plus the host's provisional load ([`ASSIGN_PROVISIONAL_MS`]: the
//! assignments decided since its beat could show them) — then the rendezvous weight.

use std::collections::BTreeMap;

use super::{AssignLand, Assigned, CoordMsg, Coordinator, HostSeen, StoreDecision, Target};
use crate::{HOST_LIVE, HostId, SuperRegion};

/// The tick headroom a host keeps before it takes new land while another can (permille of the
/// tick period): a host whose worst tick uses over 80 % of the period is skipped.
pub const ASSIGN_HEADROOM_MIN_PERMILLE: u32 = 200;
/// How long the ticker must have known a host's process (its lease watch) before it takes new
/// land while an older one can: three leases — a pod that crash-loops is usually gone by then.
pub const ASSIGN_MIN_ATTACHED_MS: u64 = 30_000;
/// How long (store ms) an assignment counts toward its host's load: one lease — by then a beat
/// shows the real load.
pub const ASSIGN_PROVISIONAL_MS: u64 = 10_000;
/// The most assignments one owners read decides: bounds how long one transition holds the
/// coordinator row (and the `super_regions` rows it locks).
pub const ASSIGN_PER_PASS_MAX: usize = 64;
/// The ticker logs that no host can take free land at most once per this many store ms.
pub const ASSIGN_WARN_EVERY_MS: u64 = 60_000;

impl Coordinator {
    /// The ticker's owners read ([`super::Input::LandOwners`]): `asked` as handed out by
    /// [`Self::take_land_lookups`], `rows` the `super_regions` rows read of them (an asked
    /// super-region with no row is free). The module docs say what each row decides. Ticker
    /// only, outside the log: a rebase drops what it decided, and the hosts ask again.
    pub(super) fn on_land_read(
        &mut self,
        asked: Vec<(SuperRegion, Vec<HostId>)>,
        rows: &[(SuperRegion, i64, u64)],
        now_ms: u64,
    ) {
        if self.crash_policy() != crate::CrashPolicy::Succession {
            log::debug!("coordinator: an owners read under Static: nothing is assigned");
            return;
        }
        self.clock_ms = self.clock_ms.max(now_ms);
        self.provisional
            .retain(|&(_, at)| now_ms.saturating_sub(at) < ASSIGN_PROVISIONAL_MS);
        let rows: BTreeMap<SuperRegion, (i64, u64)> = rows
            .iter()
            .map(|&(sr, owner, epoch)| (sr, (owner, epoch)))
            .collect();
        let live = self.attached_live();
        let mut decided = 0usize;
        for (sr, askers) in asked {
            let owned = rows
                .get(&sr)
                .and_then(|&(owner, epoch)| u64::try_from(owner).ok().map(|o| (HostId(o), epoch)));
            if let Some((owner, epoch)) = owned {
                // Owned: an assignment of it answered or lost is no longer awaited here.
                self.assign_askers.remove(&sr);
                if !live.contains(&owner) {
                    self.on_land_owner(owner, now_ms);
                    continue;
                }
                for asker in askers {
                    if asker == owner {
                        // Never land a succession moved to it (or may be moving: one is being
                        // declared, or the open ones are not known here): INHERITED land comes
                        // by its order and is used only behind the heir's mask, after its
                        // adoption — an `Assigned` would make it plain land (final review,
                        // A-F1). The heir asks again; after the settle the heal runs.
                        if self.inherits_or_may(owner, sr) {
                            log::debug!(
                                "coordinator: host {owner} asks for {sr:?}, which a succession gives it: its order carries it"
                            );
                            continue;
                        }
                        self.push(
                            Target::Host(owner),
                            CoordMsg::Assigned {
                                rows: vec![(sr, epoch)],
                            },
                        );
                    } else {
                        self.push(Target::Host(asker), CoordMsg::LandIs { sr, owner, epoch });
                    }
                }
                continue;
            }
            if decided >= ASSIGN_PER_PASS_MAX {
                // The rest wait for the next read.
                self.successions
                    .land_lookups
                    .entry(sr)
                    .or_default()
                    .extend(askers);
                continue;
            }
            let Some((host, boot)) = self.pick_assignee(sr, now_ms) else {
                if self
                    .assign_warned_ms
                    .is_none_or(|at| now_ms.saturating_sub(at) >= ASSIGN_WARN_EVERY_MS)
                {
                    self.assign_warned_ms = Some(now_ms);
                    log::warn!(
                        "coordinator: super-region {sr:?} is free and asked for by {askers:?}, but no attached host takes logins: nothing assigned (the hosts ask again)"
                    );
                }
                continue;
            };
            log::debug!(
                "coordinator: super-region {sr:?} is free: assigning it to host {host} (boot {boot})"
            );
            self.push_decision(StoreDecision::AssignLand(AssignLand { sr, host, boot }));
            self.provisional.push((host, now_ms));
            let waiting = self.assign_askers.entry(sr).or_default();
            for asker in askers {
                if !waiting.contains(&asker) {
                    waiting.push(asker);
                }
            }
            decided += 1;
        }
    }

    /// Whether super-region `sr`, `host`'s in the store, is (or may be) land a succession not
    /// settled yet moved to it: an open run names `host` heir of it, a declaration is in flight,
    /// or this replica does not know the open successions yet.
    fn inherits_or_may(&self, host: HostId, sr: SuperRegion) -> bool {
        !self.successions_known()
            || self.successions_declaring().is_some()
            || self.succession_moved_to(host, sr)
    }

    /// The host free super-region `sr` goes to, with the boot its `hosts` row is of (what the
    /// store checks), or `None`: no attached live host takes logins (the module docs).
    pub(super) fn pick_assignee(&self, sr: SuperRegion, now_ms: u64) -> Option<(HostId, u64)> {
        let mut candidates: Vec<(HostId, HostSeen)> = self
            .attached_live()
            .into_iter()
            .filter(|h| self.takes_logins(*h))
            .filter_map(|h| {
                self.leases
                    .row(h)
                    .filter(|r| !r.clean && r.state == HOST_LIVE)
                    .map(|r| (h, r.clone()))
            })
            .collect();
        let watch_now = self.leases.latest_received_ms();
        // Each filter applies only if it leaves a candidate.
        narrow(&mut candidates, |(h, _)| {
            self.leases.known_for(*h, watch_now) >= ASSIGN_MIN_ATTACHED_MS
        });
        narrow(&mut candidates, |(_, r)| {
            r.load.tick_permille <= 1000 - ASSIGN_HEADROOM_MIN_PERMILLE
        });
        let key = |(h, r): &(HostId, HostSeen)| {
            let provisional = self.provisional_load(*h, now_ms);
            (
                r.load.players.saturating_add(provisional),
                r.load.regions.saturating_add(provisional),
            )
        };
        let least = candidates.iter().map(key).min()?;
        let tied: Vec<HostId> = candidates
            .iter()
            .filter(|c| key(c) == least)
            .map(|(h, _)| *h)
            .collect();
        let host = crate::rendezvous_among(sr, tied)?;
        candidates
            .iter()
            .find(|(h, _)| *h == host)
            .map(|(h, r)| (*h, r.boot))
    }

    /// The store's answer to an [`AssignLand`] ([`super::DecisionAnswer::Assigned`], a logged input):
    /// `Done` tells the assignee its row ([`CoordMsg::Assigned`]) and the other hosts that asked
    /// for it its owner ([`CoordMsg::LandIs`]); a refusal tells nobody and drops the provisional
    /// load the decision counted (the host took nothing). The askers are dropped with the first
    /// application: a replay (a rebase) tells the assignee again, and the askers ask again.
    /// Nothing durable changes.
    pub(super) fn assign_answered(
        &mut self,
        assign: AssignLand,
        outcome: Result<Assigned, String>,
        now_ms: u64,
    ) {
        self.clock_ms = self.clock_ms.max(now_ms);
        let AssignLand { sr, host, .. } = assign;
        let askers = self.assign_askers.remove(&sr);
        match outcome {
            Ok(Assigned::Done { epoch }) => {
                log::info!("coordinator: super-region {sr:?} is host {host}'s now (epoch {epoch})");
                self.push(
                    Target::Host(host),
                    CoordMsg::Assigned {
                        rows: vec![(sr, epoch)],
                    },
                );
                for asker in askers.into_iter().flatten().filter(|a| *a != host) {
                    self.push(
                        Target::Host(asker),
                        CoordMsg::LandIs {
                            sr,
                            owner: host,
                            epoch,
                        },
                    );
                }
            }
            refused => {
                match &refused {
                    Err(e) => log::warn!(
                        "coordinator: super-region {sr:?} could not be assigned to host {host} ({e}); the hosts ask again"
                    ),
                    Ok(why) => log::info!(
                        "coordinator: super-region {sr:?} was not assigned to host {host}: {why:?}; the hosts ask again"
                    ),
                }
                if askers.is_some() {
                    if let Some(i) = self.provisional.iter().rposition(|(h, _)| *h == host) {
                        self.provisional.remove(i);
                    }
                }
            }
        }
    }

    /// Test support: the assignments decided and not yet expired, per host.
    #[doc(hidden)]
    pub fn provisional_load(&self, host: HostId, now_ms: u64) -> u32 {
        let n = self
            .provisional
            .iter()
            .filter(|&&(h, at)| h == host && now_ms.saturating_sub(at) < ASSIGN_PROVISIONAL_MS)
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    }
}

/// `candidates` narrowed to those `keep` holds for — unless that would leave none.
fn narrow(candidates: &mut Vec<(HostId, HostSeen)>, keep: impl Fn(&(HostId, HostSeen)) -> bool) {
    if candidates.iter().any(&keep) {
        candidates.retain(|c| keep(c));
    }
}
