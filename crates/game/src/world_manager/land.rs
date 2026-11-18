//! Land a host needs and does not have (Elastic 3, plan `2026-11-18-elastic-3-land-assignment.md`,
//! "Decisions settled in planning" 1): the host asks the COORDINATOR, which assigns free land to
//! the host it picks (the least loaded) or names the owner. No host claims land of its own any
//! more, and no host asks another.
//!
//! **The ask** ([`WorldManager::ask_for_land`], from every [`WorldManager::pump_peers`]): every
//! super-region something here needs — `land_needed` (a subscribe, a player clamped at its seam,
//! a spawn placement, a move: `note_unowned`, `note_land_needed`), what waits for an owner
//! (`unowned_waiting`), and what is held for a host that left cleanly (its session's unreceived
//! messages, `peer_waiting`, a subscribe it was asked elsewhere) — whose owner this host knows to
//! be nobody reachable: `Free`, `Unknown`, or a host in `left_peers`. Land this host owns, or a
//! reachable host owns, leaves the set; so does land it inherits (`inheriting`: its own already).
//! Each is asked ([`HostMsg::LandNeeded`]) at most once per [`LAND_NEEDED_EVERY_MS`], only while
//! the coordinator link is attached (an ask is volatile: it is asked again, never queued). A
//! draining host asks too: it is assigned nothing, but what waits on it needs an owner somewhere.
//! Land of a departed host is asked only at the scan of what is held for departed hosts (once per
//! period, as `LandWanted` was before): an ask the moment a need appears would have the `Leave`
//! succession declared before the next world cut, and a host that registered during the leave
//! cut — outside it, its journal still counting from the cut before — could be its heir, waiting
//! for a base cut it never takes part in (until the deadline rolls the world back).
//!
//! **The answers.** [`CoordMsg::LandIs`] names the owner ([`WorldManager::land_is`]: the
//! directory is set, what waited goes on). [`CoordMsg::Assigned`] gives this host rows
//! ([`WorldManager::assigned`]): they are only QUEUED here — the server takes them
//! ([`WorldManager::take_assigned`]), grows its fence, and only then tells the manager
//! ([`WorldManager::claimed`]): fence first. A directory read is never what makes this host treat
//! land as its own without the fence (the server's directory keeper fences such a row before it
//! hands the read over: a lost `Assigned`, healed).

use std::collections::BTreeSet;

use super::peers::reliable_target;
use super::{RegionSpawner, WorldManager};
use crate::{HostId, HostMsg, Owner, SuperRegion};

/// Most often a host asks the coordinator for one super-region ([`HostMsg::LandNeeded`]): once
/// per this many ms while the need lasts (an ask is volatile: a lost one is asked again).
pub const LAND_NEEDED_EVERY_MS: u64 = 1_000;

impl<S: RegionSpawner> WorldManager<S> {
    /// One pass of the ask (the module docs): every super-region needed here with no reachable
    /// owner known is asked of the coordinator, each at most once per [`LAND_NEEDED_EVERY_MS`],
    /// while its link is attached. What is held for departed hosts is looked through at most
    /// once per that period (its needs stay in `land_needed` meanwhile). Never under `Static`
    /// or all-in-one (no directory): their land is a table.
    pub(super) fn ask_for_land(&mut self, now_ms: u64) {
        if self.ownership.0.directory().is_none() {
            self.land_needed.clear();
            self.land_needed_sent.clear();
            return;
        }
        let s = self.ownership.1;
        let mut needs: BTreeSet<SuperRegion> = std::mem::take(&mut self.land_needed);
        needs.extend(self.unowned_waiting.keys().copied());
        let scan = !self.left_peers.is_empty() && !self.land_scanned_ms.is_some_and(|at| now_ms < at + LAND_NEEDED_EVERY_MS);
        if scan {
            self.land_scanned_ms = Some(now_ms);
            for &o in self.left_peers.keys() {
                let session = self.peers.get(&o).into_iter().flat_map(|p| p.session.unreceived());
                let waiting = self.peer_waiting.get(&o).into_iter().flatten();
                needs.extend(session.chain(waiting).filter_map(reliable_target).map(|rc| crate::super_region_of(rc, s)));
            }
            // A subscribe waiting for that land (`Session::asked_elsewhere`, R4 Task 16).
            needs.extend(self.sessions.values().flat_map(|x| x.asked_elsewhere.iter()).map(|rc| crate::super_region_of(*rc, s)));
        }
        needs.retain(|sr| self.asks_for(*sr));
        // Kept a second period: a need still there at the next ask is not logged again.
        self.land_needed_sent.retain(|sr, at| needs.contains(sr) && now_ms < *at + 2 * LAND_NEEDED_EVERY_MS);
        if self.world_attached() {
            for &sr in &needs {
                if self.land_needed_sent.get(&sr).is_some_and(|&at| now_ms < at + LAND_NEEDED_EVERY_MS) {
                    continue;
                }
                let owner = self.owner_of_sr(sr);
                if matches!(owner, Owner::Host(_)) && !scan {
                    // A departed host's land is asked only at the paced scan (as before Elastic
                    // 3: its `Leave` succession is declared from the ask; see the module docs).
                    continue;
                }
                if !self.land_needed_sent.contains_key(&sr) {
                    log::debug!("[server] super-region {sr:?} is needed here and is {owner:?}: asking the coordinator for it");
                }
                self.land_needed_sent.insert(sr, now_ms);
                self.world_send(HostMsg::LandNeeded { sr });
                if matches!(owner, Owner::Host(_)) {
                    // A departed host's land goes by a `Leave` succession, whose order reaches
                    // only its recipients (a landless asker is none): the heir is found by a
                    // directory read, asked with each ask (paced as the asks are).
                    self.directory_wanted = true;
                }
            }
        }
        self.land_needed = needs;
    }

    /// Whether this host would ask for `sr`: it knows no reachable owner of it (`Free`,
    /// `Unknown`, or a host that left cleanly), and does not inherit it (its own already).
    fn asks_for(&self, sr: SuperRegion) -> bool {
        if self.inheriting.contains_key(&sr) {
            return false;
        }
        match self.ownership.0.owner_of(sr) {
            Owner::Free | Owner::Unknown => true,
            Owner::Host(h) => self.left_peers.contains_key(&h),
            Owner::Me => false,
        }
    }

    /// A spawn placement here needs `rc`, whose owner left cleanly (`placement.rs`): its land is
    /// asked for ([`Self::ask_for_land`]).
    pub(super) fn note_land_needed(&mut self, rc: crate::RegionCoords) {
        self.land_needed.insert(crate::super_region_of(rc, self.ownership.1));
    }

    /// `sr` has an owner now: nothing here asks for it any more.
    pub(super) fn forget_land_need(&mut self, sr: SuperRegion) {
        self.land_needed.remove(&sr);
        self.land_needed_sent.remove(&sr);
    }

    /// A [`crate::CoordMsg::Assigned`]: these rows (`(super-region, epoch)`) are this host's in
    /// the store. A row the directory already knows at that epoch or newer is dropped (a
    /// repeat, or overtaken by a later move); the rest are queued for the server
    /// ([`Self::take_assigned`]), never applied here — the fence comes first.
    pub(super) fn assigned(&mut self, rows: Vec<(SuperRegion, u64)>) {
        let Some(directory) = self.ownership.0.directory() else {
            log::warn!("[server] {} super-region(s) assigned, but this host keeps no directory: ignored", rows.len());
            return;
        };
        for (sr, epoch) in rows {
            if directory.epoch(sr).is_some_and(|known| known >= epoch) {
                log::debug!("[server] assigned {sr:?} at epoch {epoch}, known at that epoch or newer: dropped");
                continue;
            }
            if self.assigned_rows.iter().any(|&(q, e)| q == sr && e >= epoch) {
                continue;
            }
            self.assigned_rows.retain(|&(q, _)| q != sr);
            self.assigned_rows.push((sr, epoch));
        }
    }

    /// The rows assigned to this host and not taken yet ([`Self::assigned`]): the server grows
    /// its fence by them, then calls [`Self::claimed`] with them.
    pub fn take_assigned(&mut self) -> Vec<(SuperRegion, u64)> {
        std::mem::take(&mut self.assigned_rows)
    }

    /// A [`crate::CoordMsg::LandIs`]: `owner` holds `sr` at `epoch`. The directory says so
    /// (unless it knows a newer epoch) and what waited for `sr` goes on. Never this host's own
    /// row (that comes as an `Assigned`, through the fence): one naming this host is ignored.
    pub(super) fn land_is(&mut self, sr: SuperRegion, owner: HostId, epoch: u64) {
        if owner == self.ordinal() {
            log::warn!("[server] the coordinator named this host the owner of {sr:?} (epoch {epoch}) without an assignment: ignored");
            return;
        }
        let Some(directory) = self.ownership.0.directory_mut() else { return };
        directory.set(sr, Some(owner), epoch);
        log::debug!("[server] super-region {sr:?} is host {owner}'s (epoch {epoch})");
        self.forget_land_need(sr);
        self.flush_unowned();
    }

    /// The epoch this host's directory knows for `sr` (`None`: no row) — what the server's
    /// directory keeper compares a read row against before it fences it as a lost assignment.
    pub fn known_epoch(&self, sr: SuperRegion) -> Option<u64> {
        self.ownership.0.directory().and_then(|d| d.epoch(sr))
    }
}
