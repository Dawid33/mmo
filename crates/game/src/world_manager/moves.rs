//! Elastic 5: occupied land moved in place (plan `docs/superpowers/plans/2026-11-20-elastic-5-move-occupied-land.md`,
//! decisions 4 and 5) — the host's side.
//!
//! **The giver G** (`moving_out`): `CoordMsg::MoveLand` warns the home sessions whose body is in
//! super-region S (`ServerPacket::LandMoving` with a standby ticket to the taker T; one that walks
//! off S is told the move is off for it). At the `BeginCut` that carries the move, G refuses its
//! part (`CutFailure::MoveRefused`) unless it owns S, T takes part and no spawn order is held or
//! pinned on S and no session in S is between states; otherwise it masks S (`owner_of_sr` answers
//! `Unknown`), parks every player in S KEEPING its session (the record lands in `parked_players`,
//! the session waits with no home) and stops S's regions (their `Stopped` counts as parked at the
//! cut's tick, as a leave's). Its part carries the move (`WorldCut::moves`). Committed
//! ([`WorldManager::land_moved`], before `drop_stored_parked`): the directory names T at the next
//! epoch (no `Released`), S's caches go, and each kept player's record is TAKEN from
//! `parked_players` and sent to T as a `Reliable::PlayerMove` placed at its pose
//! (`MovePlace::Pose`), the session turning into an observer after its subscriptions in S are sent
//! to T. Aborted ([`WorldManager::move_aborted`]): the mask lifts and each kept player is restored
//! here at its record's pose. A peer's hand-off into S during the mask takes the release's path
//! (parked here, written by the part, placed by T at the client's rejoin): a reconnect, never a loss.
//!
//! **The taker T** (`expecting`): `CoordMsg::ExpectLand`: T does not ask for S. Committed: T asks
//! for a directory read; the server's keeper fences S only once a read shows the row as T's
//! ([`WorldManager::expected_committed`]), then `claimed` → [`WorldManager::moved_in`]. A
//! `PlayerMove` (or any hand-off) into S that arrives first is HELD (`MoveIn::held`, the record in
//! `cut_transit` so a cut writes it) and delivered at `moved_in`; held past [`MOVE_HOP_WAIT_MS`] it
//! takes the ordinary path (parked here: a reconnect).
//!
//! **Every other participant** (`peer_moving`): S is masked from the `BeginCut` until the cut's
//! abort, or until the directory shows S at a newer epoch than it knew (T's `Owns`, a read).

use std::collections::BTreeSet;

use super::{Parking, RegionSpawner, SessionKind, WorldManager};
use crate::{
    ClientId, CutFailure, CutId, GameEventKind, HostId, LandMove, MoveId, PlayerId, PlayerRecord, RegionCoords,
    RegionInput, Reliable, ServerPacket, SuperRegion, Tick,
};

/// How long (ms) the taker holds a player handed into land it expects before it takes the
/// ordinary path (parked: the client rejoins).
pub const MOVE_HOP_WAIT_MS: u64 = 30_000;

/// The giver's state of one move ([`WorldManager::move_land`]).
#[derive(Clone, Debug)]
pub(super) struct MoveOut {
    id: MoveId,
    taker: HostId,
    at_tick: Tick,
    /// Masked for cut `.0` at epoch `.1` (the row's epoch the part moves).
    masked: Option<(CutId, u64)>,
    /// The sessions parked for the move, keeping their sessions.
    kept: Vec<(ClientId, PlayerId)>,
    /// The home sessions told of the move.
    notified: BTreeSet<ClientId>,
}

/// The taker's state of one move ([`WorldManager::expect_land`]).
#[derive(Debug)]
pub(super) struct MoveIn {
    id: MoveId,
    giver: HostId,
    /// The cut that carries it, once begun here.
    cut: Option<CutId>,
    /// That cut committed: S is fenced at the first read that shows it as this host's.
    committed: bool,
    /// Hand-offs into S held until it is claimed: `(from, handoff, record, target, place, since ms)`.
    held: Vec<(HostId, crate::PlayerHandoff, PlayerRecord, RegionCoords, crate::MovePlace, u64)>,
}

impl MoveIn {
    pub(super) fn cut_is(&self, cut: CutId) -> bool {
        self.cut == Some(cut) && !self.committed
    }

    pub(super) fn uncut(&mut self) {
        self.cut = None;
    }
}

impl<S: RegionSpawner> WorldManager<S> {
    /// `CoordMsg::MoveLand`: this host gives `sr` to `taker` with a cut at or after `at_tick`.
    pub(super) fn move_land(&mut self, id: MoveId, sr: SuperRegion, taker: HostId, at_tick: Tick) {
        match self.moving_out.get_mut(&sr) {
            Some(m) if m.id == id => m.at_tick = at_tick,
            _ => {
                log::info!("[server] move {}: super-region {sr:?} moves to host {taker} at tick {at_tick}: warning its players", id.0);
                self.moving_out
                    .insert(sr, MoveOut { id, taker, at_tick, masked: None, kept: Vec::new(), notified: BTreeSet::new() });
            }
        }
        let now = self.clock_ms;
        self.notify_moving(now);
    }

    /// `CoordMsg::ExpectLand`: `sr` comes here from `giver`.
    pub(super) fn expect_land(&mut self, id: MoveId, sr: SuperRegion, giver: HostId, _at_tick: Tick) {
        if self.expecting.get(&sr).is_some_and(|m| m.id == id) {
            return;
        }
        log::info!("[server] move {}: super-region {sr:?} is to come from host {giver}", id.0);
        self.expecting.insert(sr, MoveIn { id, giver, cut: None, committed: false, held: Vec::new() });
    }

    /// `CoordMsg::MoveOff`: the move is called off. The giver's warned sessions are told; the
    /// taker releases what it held (the ordinary path).
    pub(super) fn move_off(&mut self, id: MoveId, sr: SuperRegion) {
        if let Some(m) = self.moving_out.get(&sr).filter(|m| m.id == id && m.masked.is_none()) {
            log::info!("[server] move {} of super-region {sr:?} is off", id.0);
            let told: Vec<ClientId> = m.notified.iter().copied().collect();
            self.moving_out.remove(&sr);
            for c in told {
                let _ = self.out.send((Some(c), ServerPacket::LandMoving { super_region: sr, at_tick: None, standby: None }));
            }
        }
        if self.expecting.get(&sr).is_some_and(|m| m.id == id && !m.committed) {
            if let Some(m) = self.expecting.remove(&sr) {
                self.release_held(m);
            }
        }
    }

    /// `CoordMsg::Attached` (a re-attach): orders not begun in a cut are dropped — the coordinator
    /// sends them again in the same transition while the move runs.
    pub(super) fn moves_reattached(&mut self) {
        let warned: Vec<SuperRegion> = self.moving_out.iter().filter(|(_, m)| m.masked.is_none()).map(|(sr, _)| *sr).collect();
        for sr in warned {
            if let Some(m) = self.moving_out.remove(&sr) {
                for c in m.notified {
                    let _ = self.out.send((Some(c), ServerPacket::LandMoving { super_region: sr, at_tick: None, standby: None }));
                }
            }
        }
        let uncut: Vec<SuperRegion> = self.expecting.iter().filter(|(_, m)| m.cut.is_none() && !m.committed).map(|(sr, _)| *sr).collect();
        for sr in uncut {
            if let Some(m) = self.expecting.remove(&sr) {
                self.release_held(m);
            }
        }
    }

    /// Whether `sr` is masked by a move here (the giver's part moves it, or a peer's does).
    pub(super) fn move_masked(&self, sr: SuperRegion) -> bool {
        self.moving_out.get(&sr).is_some_and(|m| m.masked.is_some()) || self.peer_moving.contains_key(&sr)
    }

    /// Whether this host waits for `sr` to be moved here (it does not ask for it).
    pub(super) fn expects_land(&self, sr: SuperRegion) -> bool {
        self.expecting.contains_key(&sr)
    }

    /// The super-regions moved here by a committed cut and not claimed yet: the server's keeper
    /// fences each once a directory read shows it as this host's.
    pub fn expected_committed(&self) -> Vec<SuperRegion> {
        self.expecting.iter().filter(|(_, m)| m.committed).map(|(sr, _)| *sr).collect()
    }

    /// The moved rows of cut `cut`'s part, each with the epoch it was masked at (the server
    /// shrinks its fence by them when the part commits; [`Self::release_epochs_of`] includes them).
    pub(super) fn move_epochs_of(&self, cut: CutId) -> Vec<(SuperRegion, u64)> {
        self.moving_out.iter().filter_map(|(sr, m)| m.masked.filter(|(c, _)| *c == cut).map(|(_, e)| (*sr, e))).collect()
    }

    /// What cut `cut`'s part moves: `(super-region, taker)`.
    pub(super) fn moves_of(&self, cut: CutId) -> Vec<(SuperRegion, HostId)> {
        self.moving_out.iter().filter(|(_, m)| m.masked.is_some_and(|(c, _)| c == cut)).map(|(sr, m)| (*sr, m.taker)).collect()
    }

    /// Whether this host masks land for cut `cut`'s move (the server settles such a cut by the
    /// store's decision when no part of it is tracked, as for a release).
    pub fn gives_land_in(&self, cut: CutId) -> bool {
        self.moving_out.values().any(|m| m.masked.is_some_and(|(c, _)| c == cut))
    }

    /// The `BeginCut`'s moves, BEFORE the part begins: refuses the part if this host cannot give
    /// a move it is the giver of; notes the taker's and the other participants' sides.
    pub(super) fn check_cut_moves(&mut self, cut: CutId, participants: &BTreeSet<HostId>, moves: &[LandMove]) -> Result<(), CutFailure> {
        let me = self.ordinal();
        let s = self.ownership.1;
        for mv in moves.iter().filter(|m| m.giver == me) {
            let Some(m) = self.moving_out.get(&mv.sr).filter(|m| m.id == mv.id) else {
                log::warn!("world cut {}: move {} of {:?}: no such move here; refused", cut.0, mv.id.0, mv.sr);
                return Err(CutFailure::MoveRefused);
            };
            let epoch = self.ownership.0.directory().and_then(|d| d.epoch(mv.sr));
            if self.owner_of_sr(mv.sr) != crate::Owner::Me || epoch.is_none() || m.taker != mv.taker || !participants.contains(&mv.taker) {
                log::warn!("world cut {}: move {} of {:?}: not this host's to give to host {}; refused", cut.0, mv.id.0, mv.sr, mv.taker);
                return Err(CutFailure::MoveRefused);
            }
            let in_sr = |rc: &RegionCoords| crate::super_region_of(*rc, s) == mv.sr;
            let held = self.spawn_held.iter().map(|(_, r)| r).chain(&self.spawn_here).any(|r| in_sr(&self.spawn_region(r.record_region)));
            let pinned = self.pinned_regions().any(|rc| in_sr(&rc));
            let between = self.moving.keys().chain(self.creating.keys()).chain(self.parking.keys()).any(|c| self.homes.get(c).is_some_and(in_sr))
                || self.moving.values().any(|mv| in_sr(&mv.from) || in_sr(&mv.target))
                || self.creating.values().any(|(rc, _, _)| in_sr(rc))
                || self.parking.values().any(|p| in_sr(&p.region))
                || self.awaiting_session.keys().any(|c| self.homes.get(c).is_some_and(in_sr));
            if held || pinned || between {
                log::warn!(
                    "world cut {}: move {} of {:?}: a spawn order held or pinned ({}) or a player between states ({between}) on it; refused",
                    cut.0,
                    mv.id.0,
                    mv.sr,
                    held || pinned
                );
                return Err(CutFailure::MoveRefused);
            }
        }
        for mv in moves {
            if mv.taker == me {
                if let Some(m) = self.expecting.get_mut(&mv.sr).filter(|m| m.id == mv.id) {
                    m.cut = Some(cut);
                }
            } else if mv.giver != me {
                let known = self.ownership.0.directory().and_then(|d| d.epoch(mv.sr)).unwrap_or(0);
                self.peer_moving.insert(mv.sr, (cut, known));
            }
        }
        Ok(())
    }

    /// The `BeginCut`'s moves this host gives, once the part began (`begin_cut_at` asked the
    /// regions for their snapshots): S is masked, its players parked keeping their sessions,
    /// its regions stopped (each `Stopped` counts as parked at the cut's tick).
    pub(super) fn begin_moves_out(&mut self, cut: CutId, moves: &[LandMove]) {
        let me = self.ordinal();
        let s = self.ownership.1;
        let now_ms = self.clock_ms;
        for mv in moves.iter().filter(|m| m.giver == me) {
            let epoch = self.ownership.0.directory().and_then(|d| d.epoch(mv.sr)).unwrap_or(0);
            let in_sr = |rc: &RegionCoords| crate::super_region_of(*rc, s) == mv.sr;
            let homes: Vec<(ClientId, RegionCoords)> = self.homes.iter().filter(|(_, rc)| in_sr(rc)).map(|(c, rc)| (*c, *rc)).collect();
            let mut kept = Vec::new();
            for (c, home) in homes {
                let Some(&player) = self.player_of.get(&c) else { continue };
                let adventure = self.adventurer_of.get(&c).copied();
                self.homes.remove(&c);
                self.parking.insert(c, Parking { player, region: home, adventure });
                self.send_play_event(home, GameEventKind::ParkPlayer(c));
                kept.push((c, player));
            }
            let regions: Vec<RegionCoords> = self.regions.iter().filter(|(rc, l)| in_sr(rc) && !l.stopping).map(|(rc, _)| *rc).collect();
            let mut stopping = Vec::new();
            for rc in regions {
                if let Some(link) = self.regions.get_mut(&rc) {
                    link.stopping = true;
                    if link.input.send(RegionInput::Shutdown).is_ok() {
                        stopping.push(rc);
                    } else {
                        log::error!("region {rc:?} died before the move's shutdown");
                    }
                }
            }
            for rc in &stopping {
                self.world_region_stopping(*rc);
            }
            log::info!(
                "[server] world cut {}: move {} masks {:?} for host {}: {} player(s) parked keeping their sessions, {} region(s) stopping",
                cut.0,
                mv.id.0,
                mv.sr,
                mv.taker,
                kept.len(),
                stopping.len()
            );
            if let Some(m) = self.moving_out.get_mut(&mv.sr) {
                m.masked = Some((cut, epoch));
                m.kept = kept;
            }
            for rc in stopping {
                self.refresh_keepalive(rc, now_ms);
            }
        }
    }

    /// Cut `cut` committed with this host's part (the server shrank its fence by the moved rows
    /// first): each super-region it moved is the taker's at the next epoch; its caches go; the
    /// players parked for it are sent to the taker, placed at their pose; the mask lifts. Runs
    /// before `drop_stored_parked` (which would drop those records as parked on foreign land).
    pub(super) fn land_moved(&mut self, cut: CutId) {
        let moved: Vec<(SuperRegion, MoveOut)> = {
            let srs: Vec<SuperRegion> = self.moving_out.iter().filter(|(_, m)| m.masked.is_some_and(|(c, _)| c == cut)).map(|(sr, _)| *sr).collect();
            srs.into_iter().filter_map(|sr| self.moving_out.remove(&sr).map(|m| (sr, m))).collect()
        };
        if moved.is_empty() {
            return;
        }
        let s = self.ownership.1;
        let now_ms = self.clock_ms;
        for (sr, m) in moved {
            let (_, was) = m.masked.expect("filtered above");
            if let Some(directory) = self.ownership.0.directory_mut() {
                if directory.epoch(sr).is_none_or(|known| known <= was + 1) {
                    directory.set(sr, Some(m.taker), was + 1);
                }
            }
            let of_sr = |rc: &RegionCoords| crate::super_region_of(*rc, s) == sr;
            self.parked.retain(|rc, _| !of_sr(rc));
            self.terrain_cache.retain(|rc, _| !of_sr(rc));
            self.pristine_terrain.retain(|rc, _| !of_sr(rc));
            self.unavailable.retain(|rc, _| !of_sr(rc));
            self.land_idle_since.remove(&sr);
            log::info!("[server] world cut {} committed: move {}: {sr:?} is host {}'s now; {} player(s) go there", cut.0, m.id.0, m.taker, m.kept.len());
            for (c, player) in m.kept {
                self.hand_over_moved(c, player, m.taker, now_ms);
            }
        }
        self.flush_unowned();
    }

    /// One player parked for a committed move goes to `taker` as a record placed at its pose; its
    /// session here (if it is still connected) is sent to the taker for its subscriptions in the
    /// moved land and turns into an observer.
    fn hand_over_moved(&mut self, c: ClientId, player: PlayerId, taker: HostId, now_ms: u64) {
        self.parking.remove(&c);
        let Some(record) = self.parked_players.remove(&player) else {
            log::error!("[server] move: player {} (client {c}) has no parked record here; the store holds it on the taker's land", player.0);
            return;
        };
        self.parked_bodies.retain(|_, p| *p != player);
        let connected = self.sessions.get(&c).is_some_and(|s| s.kind == SessionKind::Home) && self.player_of.get(&c) == Some(&player);
        let claim = super::peers::claim_of(if connected { self.adventurer_of.remove(&c) } else { record.adventure });
        if connected {
            self.player_of.remove(&c);
        }
        self.world_handed(player, taker);
        self.forget_claim_reports(player);
        let hop = self.hop_here(player) + 1;
        let login = self.logins.remove(&c);
        let target = record.region;
        if connected {
            let subscribed: Vec<RegionCoords> = self.sessions.get(&c).map(|s| s.subscribed.iter().copied().collect()).unwrap_or_default();
            self.name_new_home(c, target, taker);
            let theirs: Vec<RegionCoords> = subscribed.into_iter().filter(|rc| self.owner_host(*rc) == Some(taker) && *rc != target).collect();
            for rc in theirs {
                if let super::peers::Redirect::Sent(owner) = self.redirect(c, rc) {
                    self.redirected_to(c, rc, owner);
                }
            }
            if let Some(session) = self.sessions.get_mut(&c) {
                session.kind = SessionKind::Observer;
                session.idle_since_ms = now_ms;
            }
            self.forget_home_answers(c);
        }
        self.awaiting_session.remove(&c);
        self.departed.insert(player, taker);
        log::info!("[server] move: player {} (client {c}) goes to host {taker} at its pose in {target:?}", player.0);
        let handoff = crate::PlayerHandoff { client: c, player, claim, login, park: false, hop };
        self.queue_to_peer(taker, Reliable::PlayerMove { handoff, record: Box::new(record), target, place: crate::MovePlace::Pose });
    }

    /// Cut `cut` was decided aborted (or its part never staged): the giver's mask lifts and its
    /// players are restored here at their pose; the taker's and the peers' notes of the cut go.
    pub(super) fn move_aborted(&mut self, cut: CutId) {
        let now_ms = self.clock_ms;
        let srs: Vec<SuperRegion> = self.moving_out.iter().filter(|(_, m)| m.masked.is_some_and(|(c, _)| c == cut)).map(|(sr, _)| *sr).collect();
        for sr in srs {
            let Some(m) = self.moving_out.get_mut(&sr) else { continue };
            m.masked = None;
            let kept = std::mem::take(&mut m.kept);
            log::info!("[server] world cut {} aborted: move {} of {sr:?} stays here; {} player(s) restored", cut.0, m.id.0, kept.len());
            for (c, player) in kept {
                self.parking.remove(&c);
                if !self.sessions.contains_key(&c) || self.player_of.get(&c) != Some(&player) {
                    // Gone meanwhile: its parked record stays (a login places it).
                    continue;
                }
                match self.take_parked(player) {
                    Some(record) => self.restore_player(c, record, now_ms),
                    None => log::error!("[server] move aborted: player {} (client {c}) has no parked record to restore", player.0),
                }
            }
        }
        for m in self.expecting.values_mut().filter(|m| m.cut == Some(cut) && !m.committed) {
            m.cut = None;
        }
        self.peer_moving.retain(|_, (c, _)| *c != cut);
        self.flush_unowned();
    }

    /// The coordinator's `CutDone` of a committed cut (every participant): the taker asks for a
    /// directory read (its keeper fences the moved row once a read shows it), a peer reads too.
    pub(super) fn move_cut_committed(&mut self, cut: CutId) {
        let mut want = false;
        for m in self.expecting.values_mut().filter(|m| m.cut == Some(cut)) {
            m.committed = true;
            want = true;
        }
        want |= self.peer_moving.values().any(|(c, _)| *c == cut);
        if want {
            self.directory_wanted = true;
        }
    }

    /// The keeper fenced moved `sr` and the manager claimed it ([`Self::claimed`]): the expectation
    /// ends and what was held for it is delivered here.
    pub(super) fn moved_in(&mut self, sr: SuperRegion) {
        if let Some(m) = self.expecting.remove(&sr) {
            log::info!("[server] move {}: super-region {sr:?} is this host's; {} held player(s) placed", m.id.0, m.held.len());
            self.release_held(m);
        }
    }

    /// A player handed into `target`, which this host expects by a move and does not own yet: held
    /// (the record noted for the cuts) — `true` — else `false` (the ordinary path).
    pub(super) fn hold_for_move(
        &mut self,
        from: HostId,
        h: &crate::PlayerHandoff,
        record: &PlayerRecord,
        target: RegionCoords,
        place: crate::MovePlace,
    ) -> bool {
        let sr = crate::super_region_of(target, self.ownership.1);
        if self.owns(target) {
            return false;
        }
        let now_ms = self.clock_ms;
        let Some(m) = self.expecting.get_mut(&sr) else { return false };
        log::info!("[server] player {} handed into {target:?} by host {from}: held until move {} lands here", h.player.0, m.id.0);
        m.held.push((from, h.clone(), record.clone(), target, place, now_ms));
        let player = h.player;
        self.cut_transit(player, record);
        true
    }

    /// What `m` held, delivered by the ordinary path (placed if the land is this host's now, else
    /// parked here: the client rejoins).
    fn release_held(&mut self, m: MoveIn) {
        for (from, h, record, target, place, _) in m.held {
            self.accept_move(from, h, record, target, place);
        }
    }

    /// The upkeep: the giver warns the home sessions in land it gives (and tells one that left it);
    /// the taker gives up holding past [`MOVE_HOP_WAIT_MS`]; a peer's mask lifts once the directory
    /// shows the moved land at a newer epoch.
    pub(super) fn maintain_moves(&mut self, now_ms: u64) {
        self.notify_moving(now_ms);
        let late: Vec<SuperRegion> = self
            .expecting
            .iter()
            .filter(|(_, m)| m.held.first().is_some_and(|h| now_ms.saturating_sub(h.5) >= MOVE_HOP_WAIT_MS))
            .map(|(sr, _)| *sr)
            .collect();
        for sr in late {
            if let Some(m) = self.expecting.get_mut(&sr) {
                log::warn!("[server] move {}: {sr:?} is not this host's after {MOVE_HOP_WAIT_MS} ms; its held players take the ordinary path", m.id.0);
                let held = std::mem::take(&mut m.held);
                let id = m.id;
                let giver = m.giver;
                self.release_held(MoveIn { id, giver, cut: None, committed: false, held });
            }
        }
        if let Some(directory) = self.ownership.0.directory() {
            let lifted: Vec<SuperRegion> =
                self.peer_moving.iter().filter(|(sr, (_, known))| directory.epoch(**sr).is_some_and(|e| e > *known)).map(|(sr, _)| *sr).collect();
            for sr in lifted {
                self.peer_moving.remove(&sr);
            }
        }
    }

    /// The giver's notices: every home session whose body is in land it gives (not masked yet) is
    /// told once, with a standby ticket to the taker; one told whose body left it is told the move
    /// is off for it.
    fn notify_moving(&mut self, now_ms: u64) {
        let s = self.ownership.1;
        let srs: Vec<SuperRegion> = self.moving_out.iter().filter(|(_, m)| m.masked.is_none()).map(|(sr, _)| *sr).collect();
        for sr in srs {
            let Some(m) = self.moving_out.get(&sr) else { continue };
            let (taker, at_tick) = (m.taker, m.at_tick);
            let inside: BTreeSet<ClientId> = self
                .homes
                .iter()
                .filter(|(c, rc)| crate::super_region_of(**rc, s) == sr && self.sessions.get(c).is_some_and(|s| s.kind == SessionKind::Home))
                .map(|(c, _)| *c)
                .collect();
            let told = m.notified.clone();
            for c in told.difference(&inside) {
                let _ = self.out.send((Some(*c), ServerPacket::LandMoving { super_region: sr, at_tick: None, standby: None }));
            }
            let fresh: Vec<ClientId> = inside.difference(&told).copied().collect();
            for c in fresh {
                let standby = self.standby_for(c, taker, now_ms);
                let _ = self.out.send((Some(c), ServerPacket::LandMoving { super_region: sr, at_tick: Some(at_tick), standby }));
            }
            if let Some(m) = self.moving_out.get_mut(&sr) {
                m.notified = inside;
            }
        }
    }

    /// A standby ticket for `client` to `taker` (its endpoint and an observer ticket), if known.
    fn standby_for(&mut self, client: ClientId, taker: HostId, now_ms: u64) -> Option<(crate::HostEndpoint, crate::ObserverTicket)> {
        let endpoint = self.peer_endpoints.get(&taker).cloned()?;
        self.nonces.as_ref()?;
        self.sessions.get(&client).and_then(|s| s.player)?;
        Some((endpoint, self.observer_ticket(client, taker, now_ms)))
    }

    /// Test support: the moves this host gives / expects, by super-region.
    pub fn moves_here(&self) -> (Vec<SuperRegion>, Vec<SuperRegion>) {
        (self.moving_out.keys().copied().collect(), self.expecting.keys().copied().collect())
    }
}

