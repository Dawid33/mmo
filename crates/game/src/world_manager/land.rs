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
//! land as its own without the fence: the server's directory keeper fences a lost `Assigned`
//! before it hands the read over (never while a succession is open: such a row may be
//! INHERITED, which comes only by its order and adoption) and withholds every other row of this
//! host its fence lacks.
//!
//! **Idle land is released** (plan "Decisions settled in planning" 2, Elastic 3 Task 6). Every
//! [`LAND_IDLE_SCAN_MS`] the upkeep stamps each owned super-region nothing holds
//! ([`WorldManager::busy_land`], the busy table) with the time it was first seen so
//! (`land_idle_since`); a busy one loses its stamp. At a world cut's `BeginCut`
//! ([`WorldManager::begin_release`]) the owned super-regions idle for at least the dial
//! (`land_idle_ms`, [`LAND_IDLE_MS`]) and not busy now are MASKED (`releasing`):
//! [`WorldManager::owner_of`] answers `Unknown` for them as for land being inherited — nothing
//! starts there, a need is recorded without an ask, every arrival takes the unowned path — and
//! this host's part carries them in `WorldCut::releases`; the store frees those rows with the
//! part's commit. Each is masked WITH the epoch it is released at. Committed
//! ([`WorldManager::land_released`], after the server shrank its fence — a row only at its
//! released epoch): each row is free at that epoch + 1 in the directory, the parked blobs of
//! that land are dropped (the store holds its state now; land assigned back here later starts
//! from the store), every peer is told ([`crate::Reliable::Released`]; a peer that links
//! later, at its hello), the mask lifts and what waited asks for an owner. A row the directory
//! (or the fence) already knows at a NEWER epoch was assigned again between the commit and this
//! host's settle — to this host or another: it is kept as it is and nobody is told it is free.
//! Records parked here that the committed cut wrote and that lie on land not this host's are
//! dropped ([`WorldManager::drop_stored_parked`]): the store is their only holder. Aborted
//! ([`WorldManager::release_aborted`]): the mask lifts, the land is this host's, what waited is
//! delivered here. Nothing is released under `static` / all-in-one, or while any succession run
//! is open on this host.
//!
//! **A clean leave releases all its land** (Elastic 3 Task 8, plan "### 2. Release"): a host
//! leaving in a cut releases nothing at its `BeginCut`; once its part is complete
//! ([`WorldManager::leave_release`], from `try_complete_cut`: only there is the part's in-flight
//! list known) every super-region its directory gives it is masked for that cut and the part
//! carries them all — unless the part carries an in-flight row or a succession run is open on
//! this host: then it releases nothing and the land waits for its `Leave` succession (the rows
//! are keyed to this host id; only a succession's adopting part moves them; a leaver that owns
//! no land and still carries rows gets that succession too, declared without an ask). The leaver parked
//! everything at its `BeginCut`, so the mask only gates `owner_of`; committed or aborted, the
//! mask lifts as for an idle release (an aborted leave keeps the land, its retry's part decides
//! again). Peers learn it from a directory read (`end_leaves` asks for one: the leaver's
//! `Released` stays in its held sessions), and what they held for the leaver goes on by the
//! land's next owner ([`WorldManager::reroute_held_for_landless_leavers`]); their `departed` /
//! `redirected` pointers at it are dropped there too (the store places those players).

use std::collections::BTreeSet;

use super::peers::reliable_target;
use super::{RegionSpawner, WorldManager};
use crate::{CutId, HostId, HostMsg, Owner, RegionCoords, SuperRegion};

/// Most often a host asks the coordinator for one super-region ([`HostMsg::LandNeeded`]): once
/// per this many ms while the need lasts (an ask is volatile: a lost one is asked again).
pub const LAND_NEEDED_EVERY_MS: u64 = 1_000;

/// How long an owned super-region nothing holds stays this host's before a world cut releases
/// it: a player who steps away for a few minutes comes back to the same host. The server's dial
/// is `VOXEL_LAND_IDLE_MS` ([`WorldManager::set_land_idle_ms`]).
pub const LAND_IDLE_MS: u64 = 300_000;

/// How often the upkeep stamps idle land ([`WorldManager::scan_idle_land`]): no finer grain is
/// needed than the ask period's.
pub const LAND_IDLE_SCAN_MS: u64 = 1_000;

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
        // Land masked for a release is still this host's: its needs wait for the decision
        // (asked once it is free, `land_released`; dropped as owned at an abort).
        let masked: BTreeSet<SuperRegion> = needs.iter().copied().filter(|sr| self.releasing.contains_key(sr)).collect();
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
        needs.extend(masked);
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

    // --- idle land is released (the module docs) ---

    /// The server's dial (`VOXEL_LAND_IDLE_MS`) and the tests'. Default [`LAND_IDLE_MS`].
    pub fn set_land_idle_ms(&mut self, ms: u64) {
        self.land_idle_ms = ms;
    }

    /// The super-regions something here holds or aims at (the plan's busy table): a region
    /// running or stopping; a waiting arrival or held intent mail; a crossing in flight to it;
    /// an intent in flight to or from it; the home of a connected (or awaited) client; a move
    /// from or to it; a held spawn order's region; a region pinned for an adoption; land being
    /// inherited. `uncommitted` is not a holder (the releasing part carries every snapshot of
    /// land with no running region); nor is `parked_players` (keyed by player, moved to the
    /// land's next owner when the player returns).
    pub(super) fn busy_land(&self) -> BTreeSet<SuperRegion> {
        let s = self.ownership.1;
        let sr = |rc: &RegionCoords| crate::super_region_of(*rc, s);
        let mut busy: BTreeSet<SuperRegion> = BTreeSet::new();
        busy.extend(self.regions.keys().map(sr));
        busy.extend(self.pending_arrivals.iter().filter(|(_, l)| !l.is_empty()).map(|(rc, _)| sr(rc)));
        busy.extend(self.intent_mailbox.iter().filter(|(_, l)| !l.is_empty()).map(|(rc, _)| sr(rc)));
        busy.extend(self.in_flight.values().map(|f| sr(&f.target)));
        for f in &self.in_flight_intents {
            busy.insert(sr(&f.target));
            if let crate::IntentMsg::Intent(i) = &f.msg {
                busy.insert(sr(&i.from));
            }
        }
        let clients = self.sessions.keys().chain(self.awaiting_session.keys());
        busy.extend(clients.filter_map(|c| self.homes.get(c)).map(sr));
        for m in self.moving.values() {
            busy.insert(sr(&m.from));
            busy.insert(sr(&m.target));
        }
        let held = self.spawn_held.iter().map(|(_, r)| r).chain(&self.spawn_here);
        busy.extend(held.map(|r| sr(&self.spawn_region(r.record_region))));
        // An order already placed here pins its region until the session comes.
        busy.extend(self.pinned_regions().map(|rc| sr(&rc)));
        busy.extend(self.succession_pinned.iter().map(sr));
        busy.extend(self.inheriting.keys().copied());
        busy
    }

    /// The upkeep's idle stamps, at most once per [`LAND_IDLE_SCAN_MS`]: every owned
    /// super-region nothing holds keeps (or gets) the time it was first seen idle; a busy one,
    /// or one no longer owned, loses its stamp. Never under `static` / all-in-one. A busy spell
    /// shorter than one scan between two scans leaves the stamp as it was (bounded early release).
    pub(super) fn scan_idle_land(&mut self, now_ms: u64) {
        if self.idle_scanned_ms != 0 && now_ms < self.idle_scanned_ms.saturating_add(LAND_IDLE_SCAN_MS) {
            return;
        }
        self.idle_scanned_ms = now_ms;
        self.prune_recently_released();
        let me = self.ordinal();
        let Some(directory) = self.ownership.0.directory() else {
            self.land_idle_since.clear();
            return;
        };
        let owned: BTreeSet<SuperRegion> = directory.owned_by(me).into_iter().collect();
        let busy = self.busy_land();
        self.land_idle_since.retain(|sr, _| owned.contains(sr) && !busy.contains(sr));
        for sr in owned.difference(&busy) {
            self.land_idle_since.entry(*sr).or_insert(now_ms);
        }
    }

    /// At world cut `cut`'s `BeginCut` (after its participation checks, before any region is
    /// asked): the owned super-regions idle for at least the dial and not busy now are masked
    /// until the cut is decided here, and this host's part releases them. Nothing under `static`
    /// / all-in-one, while this host leaves in the cut ([`Self::leave_release`] decides a
    /// leaver's), or while any succession run is open on this host (heir, survivor or adopting).
    pub(super) fn begin_release(&mut self, cut: CutId, now_ms: u64) {
        let me = self.ordinal();
        let Some(directory) = self.ownership.0.directory() else { return };
        if self.leave_parked || self.leaving_cut == Some(cut) {
            return;
        }
        if self.succession_open_here() {
            return;
        }
        let busy = self.busy_land();
        // Each with the epoch it is released at (the row's now: what the fence holds and the
        // store's release compares).
        let idle: Vec<(SuperRegion, u64)> = self
            .land_idle_since
            .iter()
            .filter(|(sr, since)| now_ms.saturating_sub(**since) >= self.land_idle_ms && !busy.contains(*sr))
            .filter(|(sr, _)| directory.get(**sr, me) == Owner::Me && !self.releasing.contains_key(*sr))
            .map(|(sr, _)| (*sr, directory.epoch(*sr).unwrap_or(0)))
            .collect();
        if idle.is_empty() {
            return;
        }
        log::info!("[server] world cut {}: {} idle super-region(s) released in this host's part (masked until it is decided)", cut.0, idle.len());
        log::debug!("[server] world cut {}: releasing {idle:?}", cut.0);
        for (sr, epoch) in idle {
            self.releasing.insert(sr, (cut, epoch));
        }
    }

    /// Whether any succession run is open on this host (heir, survivor or adopting): no part of
    /// it releases land until every one settled.
    fn succession_open_here(&self) -> bool {
        !self.successions.is_empty() || !self.inheriting.is_empty() || !self.adoptions.is_empty()
    }

    /// This host leaves in world cut `cut` and its part is complete (`try_complete_cut`, before
    /// the part is built): every super-region its directory gives it is masked for `cut` and
    /// the part releases them all (the module docs) — unless the part carries an in-flight row
    /// (`in_flight_rows`) or a succession run is open on this host: then nothing, and its land
    /// waits for a `Leave` succession. Nothing under `static` / all-in-one. Land assigned to
    /// this host and not yet in its directory (`assigned_rows`, its fence not grown yet) is not
    /// released: it is this host's in the store, and goes by that `Leave` succession.
    ///
    /// A leaver that owns NO land and still carries in-flight rows (idle land released earlier,
    /// then something arrived for free land and waits here) leaves them keyed to its host id:
    /// the coordinator declares a `Leave` succession for a clean row that holds only in-flight
    /// rows without waiting for an ask (`coordinator/succession.rs`), and its heir adopts them.
    pub(super) fn leave_release(&mut self, cut: CutId, in_flight_rows: usize) {
        let me = self.ordinal();
        let Some(directory) = self.ownership.0.directory() else { return };
        let land: Vec<(SuperRegion, u64)> = directory.owned_by(me).into_iter().map(|sr| (sr, directory.epoch(sr).unwrap_or(0))).collect();
        if land.is_empty() {
            if in_flight_rows > 0 {
                log::warn!(
                    "[server] world cut {}: this host leaves owning no land with {in_flight_rows} in-flight row(s): they stay \
                     keyed to this host until a Leave succession's heir adopts them (the coordinator declares it)",
                    cut.0
                );
            }
            return;
        }
        if in_flight_rows > 0 {
            log::info!(
                "[server] world cut {}: this host leaves with {in_flight_rows} in-flight row(s): its {} super-region(s) are \
                 kept for a Leave succession",
                cut.0,
                land.len()
            );
            return;
        }
        if self.succession_open_here() {
            log::info!(
                "[server] world cut {}: this host leaves with a succession open on it: its {} super-region(s) are kept for a \
                 Leave succession",
                cut.0,
                land.len()
            );
            return;
        }
        log::info!("[server] world cut {}: this host leaves: its {} super-region(s) are released in its part", cut.0, land.len());
        for (sr, epoch) in land {
            self.releasing.insert(sr, (cut, epoch));
        }
    }

    /// The super-regions masked for cut `cut`'s release (what its part carries).
    pub fn releases_of(&self, cut: CutId) -> Vec<SuperRegion> {
        self.releasing.iter().filter(|(_, (c, _))| *c == cut).map(|(sr, _)| *sr).collect()
    }

    /// [`Self::releases_of`], each with the epoch it was masked — and is released — at (the
    /// server shrinks its fence by them when that part commits, a row only at that epoch).
    pub fn release_epochs_of(&self, cut: CutId) -> Vec<(SuperRegion, u64)> {
        self.releasing.iter().filter(|(_, (c, _))| *c == cut).map(|(sr, (_, epoch))| (*sr, *epoch)).collect()
    }

    /// Cut `cut` committed — with this host's part, the server's fence already shrunk by its
    /// releases. Per released super-region, RELEASED AT the epoch it was masked at
    /// (`releasing`): the directory row is free at that epoch + 1 — unless the directory
    /// already knows a newer row (final review, A-F2 / B-I5: the land was assigned again, to
    /// this host or another, after the release committed and before this host settled the cut;
    /// its `Assigned` / `Owns` / a read came first): that row is kept, nobody is told it is
    /// free, and it is not remembered as released. For every released super-region the parked
    /// blobs of its regions and their cached terrain are dropped (the store holds that land
    /// now: land assigned back here starts from the store), so are their load-retry marks and
    /// its idle stamp; it is counted. The rows that are free now are remembered in
    /// `recently_released` and every peer is told ([`crate::Reliable::Released`]). Then the mask
    /// lifts and what waited goes on (it asks for an owner, or is delivered to the new one).
    pub(super) fn land_released(&mut self, cut: CutId) {
        let released = self.release_epochs_of(cut);
        if released.is_empty() {
            return;
        }
        let s = self.ownership.1;
        let Some(directory) = self.ownership.0.directory_mut() else {
            log::error!("[server] world cut {}: {} super-region(s) released, but this host keeps no directory", cut.0, released.len());
            for (sr, _) in &released {
                self.releasing.remove(sr);
            }
            return;
        };
        let mut rows = Vec::with_capacity(released.len());
        for &(sr, was) in &released {
            let free = was + 1;
            if let Some(known) = directory.epoch(sr).filter(|known| *known > free) {
                log::info!(
                    "[server] world cut {}: super-region {sr:?} was released at epoch {was} and has an owner again at epoch \
                     {known}: that row is kept",
                    cut.0
                );
                continue;
            }
            directory.set(sr, None, free);
            self.recently_released.insert(sr, free);
            rows.push((sr, free));
        }
        let of_released = |rc: &RegionCoords| released.iter().any(|(sr, _)| *sr == crate::super_region_of(*rc, s));
        self.parked.retain(|rc, _| !of_released(rc));
        self.terrain_cache.retain(|rc, _| !of_released(rc));
        self.pristine_terrain.retain(|rc, _| !of_released(rc));
        self.unavailable.retain(|rc, _| !of_released(rc));
        for (sr, _) in &released {
            self.land_idle_since.remove(sr);
            self.releasing.remove(sr);
        }
        self.land_released_total += released.len() as u64;
        log::info!("[server] world cut {} committed: {} super-region(s) released ({} free now)", cut.0, released.len(), rows.len());
        // Every peer learns it now (a peer that links later, at its hello): nothing it sends
        // for that land comes here any more — a need asks the coordinator, an arrival waits
        // on the peer for the land's next owner. Not a host that left (its session is held).
        if !rows.is_empty() {
            let peers: Vec<HostId> = self.peers.keys().copied().filter(|p| !self.left_peers.contains_key(p)).collect();
            for p in peers {
                self.queue_to_peer(p, crate::Reliable::Released { rows: rows.clone() });
            }
        }
        self.flush_unowned();
    }

    /// Cut `cut` committed with this host's part (after [`Self::land_released`]: the masks of
    /// its releases are lifted): every record noted for it (`parked_written` — parked here on
    /// land that was not plainly this host's when the part was built and written by the part
    /// as it is, or handed here wrapped in the cut, its sender's part writing it) that is
    /// still parked here unchanged, on land that is not this host's now, is DROPPED: the store
    /// holds exactly that record, and whoever places the player next reads it there (final
    /// review, B-I2). Kept here it would be written by every later part of this host while
    /// the land's next owner places the player from the store — the player twice, two parts
    /// writing one `players` row. Not dropped: a record changed or taken since (a login, a
    /// newer park), one a spawn order placed here waits on (its session is coming), and one
    /// whose land is this host's (again). Under `static` / all-in-one nothing is noted.
    pub(super) fn drop_stored_parked(&mut self, cut: CutId) {
        let written = self.parked_written.remove(&cut).unwrap_or_default();
        // Notes of older cuts were settled with them (a note is made only for a pending cut).
        self.parked_written.retain(|c, _| *c > cut);
        let mut dropped = 0usize;
        for (player, record) in written {
            if self.parked_only(player) != Some(&record) {
                continue;
            }
            if self.spawn_awaited(player) {
                continue;
            }
            if !self.parks_on_foreign_land(&record) {
                continue;
            }
            self.parked_players.remove(&player);
            self.parked_bodies.retain(|_, p| *p != player);
            dropped += 1;
        }
        if dropped > 0 {
            log::info!(
                "[server] world cut {} committed: {dropped} record(s) parked here on land that is not this host's are the \
                 store's now (placed by their land's owner)",
                cut.0
            );
        }
    }

    /// Whether `record`, parked here, lies on land that is not this host's: the region a login
    /// of it is placed in (`spawn_region`) has another owner, none, or is masked for a release.
    /// Land this host inherits and has not adopted yet is its own already.
    pub(super) fn parks_on_foreign_land(&self, record: &crate::PlayerRecord) -> bool {
        let region = self.spawn_region(Some(record.region));
        if self.inheriting.contains_key(&crate::super_region_of(region, self.ownership.1)) {
            return false;
        }
        self.owner_of(region) != Owner::Me
    }

    /// Cut `cut` was decided aborted (or its part never staged): the mask of its releases lifts,
    /// the land stays this host's, and what waited for it is delivered here.
    pub(super) fn release_aborted(&mut self, cut: CutId) {
        // Nothing of it is in the store: the records noted for it stay parked here.
        self.parked_written.remove(&cut);
        let kept = self.releases_of(cut);
        if kept.is_empty() {
            return;
        }
        for sr in &kept {
            self.releasing.remove(sr);
        }
        log::info!("[server] world cut {} aborted: {} super-region(s) it would have released stay this host's", cut.0, kept.len());
        self.flush_unowned();
    }

    /// A row of `recently_released` leaves once the directory shows it owned again.
    pub(super) fn prune_recently_released(&mut self) {
        let me = self.ordinal();
        let Some(directory) = self.ownership.0.directory() else {
            self.recently_released.clear();
            return;
        };
        self.recently_released.retain(|sr, _| directory.get(*sr, me) == Owner::Free);
    }

    /// Whether `rc`'s land is masked here (inherited and not adopted, or released in a pending
    /// part) or was released here and no read showed it owned since: a peer's state message for
    /// it is expected (a peer not told yet), not a disagreement — logged below error.
    pub(super) fn masked_or_released(&self, rc: RegionCoords) -> bool {
        let sr = crate::super_region_of(rc, self.ownership.1);
        self.inheriting.contains_key(&sr) || self.releasing.contains_key(&sr) || self.recently_released.contains_key(&sr)
    }

    /// Test support and the `[server]` line: the super-regions masked for a release now.
    pub fn land_releasing(&self) -> Vec<SuperRegion> {
        self.releasing.keys().copied().collect()
    }

    /// Test support and the `[server]` line: super-regions released since start.
    pub fn land_released_total(&self) -> u64 {
        self.land_released_total
    }
}
