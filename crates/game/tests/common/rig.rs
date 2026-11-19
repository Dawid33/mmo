//! The scheduling rig the seam tests share: a [`WorldManager`] on the
//! [`InlineSpawner`] (lockstep, or jittered by an [`InlineSchedule`]) with one
//! scripted client, its region outputs and packets pumped by hand. Moved out
//! of `handoff_schedule.rs` so the seam-translation harness
//! ([`super::seam_translate`]) drives the same rig.
use crossbeam::channel::{unbounded, Receiver};
use game::{
    ClientId, Column, ColumnCoords, EntityId, GameEventKind, InlineLodBuilder, InlineSpawner, InputEvent, LodBuilder, RegionCoords, RegionOutput, ServerEvent,
    ServerPacket, WorldManager,
};
use std::collections::BTreeMap;

// --- The seam floor (moved from `handoff_schedule.rs`, shared) ---------

pub const CLIENT: ClientId = 0;

pub struct Rig {
    pub manager: WorldManager<InlineSpawner>,
    pub region_out: Receiver<(RegionCoords, RegionOutput)>,
    pub packets: Receiver<(Option<ClientId>, ServerPacket)>,
    pub now_ms: u64,
    /// Latest authoritative `PlayerRegion` push — what a client would believe.
    pub pushed_home: Option<RegionCoords>,
    /// [`Rig::start_trace`]: each traced region's tick, and what was seen.
    pub trace: Option<(std::collections::BTreeMap<RegionCoords, u64>, Vec<(RegionCoords, Seen)>)>,
    /// Every non-person entity [`assert_conserved`] has seen, and where it
    /// was last (`None` = in flight): it must never vanish.
    pub tracked: std::collections::BTreeMap<EntityId, Option<RegionCoords>>,
    /// The voxel bodies [`assert_conserved`] has seen: they may end by
    /// settling in the region that owns them.
    pub settling: std::collections::BTreeSet<EntityId>,
    /// Latest region snapshot sent to the client, per region: whether it
    /// held the client's player.
    pub snapshot_has_player: std::collections::BTreeMap<RegionCoords, bool>,
    /// Every `CutSnapshot` the regions sent: `(region, cut, its tick, the
    /// snapshot's meta tick)`.
    pub cut_snapshots: Vec<(RegionCoords, game::CutId, u64, u64)>,
    /// Every `CutMissed`: `(region, cut, the region's tick when seen)`.
    pub cut_missed: Vec<(RegionCoords, game::CutId, u64)>,
    /// Every intent and reply the regions sent (`RegionOutput::Intents`).
    pub intents_seen: Vec<(RegionCoords, game::IntentMsg)>,
    /// `Some`: every event the regions processed since it was set, in output
    /// order — each region's authoritative stream, as a client replays it.
    pub events_seen: Option<Vec<(RegionCoords, GameEventKind)>>,
    /// `Some`: each region's authoritative stream since it was set — every
    /// event it processed, with its id, in output order.
    pub event_streams: Option<Streams>,
    /// `Some`: every packet the manager sent since it was set, in order (region
    /// snapshots aside).
    pub packets_seen: Option<Vec<(Option<ClientId>, ServerPacket)>>,
}

impl Rig {

    /// [`Rig::with_floor`] over a given spawner (a restarted world's).
    pub fn with_spawner(
        spawner: InlineSpawner,
        floor: impl FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send + 'static,
    ) -> Self {
        // Before any region exists: the seam tests compare runs (placements,
        // seeds) against each other, and a player's starting kit and a broken
        // block's drops read the process-global block props.
        super::install_real_block_props();
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            water: Box::new(game::lod::LodWater::dry),
            out: lod_result_send,
        });
        let mut manager = WorldManager::new(spawner, Box::new(floor), out_send, region_out_send, lod_builder, lod_result_recv);
        // The peer journal's seeded tests run on `static` hosts too (the mechanism does not
        // depend on the policy); the product journals only under `claim`
        // (`peer_schedule::hosts_under_the_rollback_policy_journal_nothing`).
        manager.journal_under_any_policy(true);
        Rig {
            manager,
            region_out,
            packets,
            now_ms: 0,
            pushed_home: None,
            trace: None,
            tracked: Default::default(),
            settling: Default::default(),
            snapshot_has_player: Default::default(),
            cut_snapshots: Vec::new(),
            cut_missed: Vec::new(),
            intents_seen: Vec::new(),
            events_seen: None,
            event_streams: None,
            packets_seen: None,
        }
    }

    /// [`Rig::with_spawner`] for one host of several: it simulates the regions
    /// `ownership` gives it (`s` regions per super-region side). Its world tick is its
    /// spawner's round count, so every host of one world must be ticked every round
    /// ([`super::multi_rig::MultiRig`]).
    pub fn with_ownership(
        spawner: InlineSpawner,
        floor: impl FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send + 'static,
        ownership: Box<dyn game::Ownership>,
        s: u32,
    ) -> Self {
        let mut rig = Self::with_spawner(spawner, floor);
        rig.manager.set_ownership(ownership, s);
        rig
    }

    pub fn settle(&mut self) {
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.trace_output(rc, &out);
                if let RegionOutput::CutSnapshot(id, tick, p) = &out {
                    self.cut_snapshots.push((rc, *id, *tick, p.meta.tick));
                }
                if let RegionOutput::Intents(list) = &out {
                    self.intents_seen.extend(list.iter().map(|m| (rc, *m)));
                }
                if let (Some(seen), RegionOutput::EventProcessed(e) | RegionOutput::EventProcessedHashed(e, ..)) =
                    (self.events_seen.as_mut(), &out)
                {
                    seen.push((rc, e.kind.clone()));
                }
                if let (Some(streams), RegionOutput::EventProcessed(e) | RegionOutput::EventProcessedHashed(e, ..)) =
                    (self.event_streams.as_mut(), &out)
                {
                    streams.entry(rc).or_default().push((e.id, e.kind.clone()));
                }
                if let RegionOutput::CutMissed(id) = &out {
                    let mut at = 0;
                    self.manager.spawner_mut().with_region(rc, |r| at = r.current_tick());
                    self.cut_missed.push((rc, *id, at));
                }
                self.manager.handle_region_output(rc, out, self.now_ms);
            }
        }
        for (to, packet) in self.packets.try_iter() {
            let packet = match packet {
                ServerPacket::PlayerRegion(home, client) => {
                    if client == CLIENT {
                        self.pushed_home = home;
                    }
                    ServerPacket::PlayerRegion(home, client)
                }
                ServerPacket::Region(rc, state) => {
                    self.snapshot_has_player.insert(rc, state.player_entites.contains_key(&CLIENT));
                    continue;
                }
                other => other,
            };
            if let Some(seen) = self.packets_seen.as_mut() {
                seen.push((to, packet));
            }
        }
    }

    pub fn event(&mut self, ev: ServerEvent) {
        assert!(self.manager.handle_server_event(ev, self.now_ms));
        self.settle();
    }
}

/// Each region's authoritative event stream (`Rig::event_streams`): `(id, kind)`.
pub type Streams = BTreeMap<RegionCoords, Vec<(usize, GameEventKind)>>;

/// One entry of a region's authoritative output stream, as the held-key
/// test reads it: the region's tick when it happened.
#[derive(Debug, Clone, Copy)]
pub enum Seen {
    /// A marker input (`Resized { width: seq }`) was applied.
    Marker { seq: u32, tick: u64 },
    /// The player's body arrived (or was created) here.
    Arrived,
    /// The player's body left here, during the tick that brought the
    /// region to `stamp`.
    Departed { stamp: u64 },
}

impl Rig {

    pub fn trace_output(&mut self, rc: RegionCoords, out: &RegionOutput) {
        let Some((ticks, seen)) = self.trace.as_mut() else { return };
        let Some(tick) = ticks.get_mut(&rc) else { return };
        let events = match out {
            RegionOutput::EventProcessed(e) | RegionOutput::EventProcessedHashed(e, ..) => vec![&e.kind],
            RegionOutput::Departures(list, stamp) => {
                if list.iter().any(|(b, _)| b.client.as_ref().map(|(c, _)| *c) == Some(CLIENT)) {
                    seen.push((rc, Seen::Departed { stamp: *stamp }));
                }
                vec![]
            }
            _ => vec![],
        };
        for kind in events {
            match kind {
                GameEventKind::Tick => *tick += 1,
                GameEventKind::PlayerInput(CLIENT, InputEvent::Resized { width, .. }) => {
                    seen.push((rc, Seen::Marker { seq: *width, tick: *tick }))
                }
                GameEventKind::EntityArrived(b) if b.client.as_ref().map(|(c, _)| *c) == Some(CLIENT) => {
                    seen.push((rc, Seen::Arrived))
                }
                _ => {}
            }
        }
    }
}
