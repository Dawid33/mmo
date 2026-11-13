//! The scheduling rig the seam tests share: a [`WorldManager`] on the
//! [`InlineSpawner`] (lockstep, or jittered by an [`InlineSchedule`]) with one
//! scripted client, its region outputs and packets pumped by hand. Moved out
//! of `handoff_schedule.rs` so the seam-translation harness
//! ([`super::seam_translate`]) drives the same rig.
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, SubChunk, REGION_SIZE,
    ClientId, ClientPacket, Column, ColumnCoords, EntityId, GameEvent, GameEventKind, InlineLodBuilder,
    InlineSchedule, InlineSpawner, InputEvent, Key, LodBuilder, Order, RegionCoords, RegionOutput, ServerEvent,
    ServerPacket, TilePos, Walker, WorldManager, SPAWN_REGION, TICK_RATE,
};
use std::collections::BTreeMap;

/// The conservation invariant of a crossing: every `EntityId` is owned by
/// exactly one running region or sits exactly once in the manager's
/// in-flight table or arrival mailbox — never both, never twice, and (people aside, whom the
/// history may absorb or end, arrivals a full mailbox despawned, and voxel
/// bodies that settled in the region owning them) never lost. Called every
/// round.
pub fn assert_conserved(rig: &mut Rig) {
    let mut places: BTreeMap<EntityId, Vec<Option<RegionCoords>>> = BTreeMap::new();
    let mut people = std::collections::BTreeSet::new();
    let mut settling = std::collections::BTreeSet::new();
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            for (id, &key) in data.id_index.iter() {
                places.entry(*id).or_default().push(Some(rc));
                if matches!(data.ecs.kind.try_get(key), Some(game::EntityKind::Person { .. })) {
                    people.insert(*id);
                }
                if data.ecs.voxel_body.try_get(key).is_some() {
                    settling.insert(*id);
                }
            }
        });
    }
    rig.settling.extend(settling);
    for (id, _target) in rig.manager.in_flight_ids() {
        places.entry(id).or_default().push(None);
    }
    // Waiting in the manager's mailbox for a region that is not running
    // counts as in flight too.
    for (id, _target) in rig.manager.pending_arrival_ids() {
        places.entry(id).or_default().push(None);
    }
    for (id, at) in &places {
        assert_eq!(at.len(), 1, "{id:?} is in several places at once (None = in flight): {at:?}");
    }
    // A waiting arrival the full mailbox despawned is an allowed exit.
    for id in rig.manager.recently_despawned_arrivals() {
        rig.tracked.remove(&id);
    }
    // A voxel body settles (its policy) where it is owned: an allowed exit
    // from a region, never from flight.
    let settled: Vec<EntityId> = rig
        .tracked
        .iter()
        .filter(|(id, last)| !places.contains_key(id) && last.is_some() && rig.settling.contains(id))
        .map(|(id, _)| *id)
        .collect();
    for id in settled {
        rig.tracked.remove(&id);
        rig.settling.remove(&id);
    }
    for (id, last) in &rig.tracked {
        assert!(places.contains_key(id), "{id:?} vanished (last seen {last:?}, None = in flight)");
    }
    rig.tracked.retain(|id, _| !people.contains(id));
    for (id, at) in places {
        if !people.contains(&id) {
            rig.tracked.insert(id, at[0]);
        }
    }
}

// --- The seam floor (moved from `handoff_schedule.rs`, shared) ---------

pub fn north() -> RegionCoords {
    RegionCoords::new(0, -1)
}

pub fn north_west() -> RegionCoords {
    RegionCoords::new(-1, -1)
}

fn floor_column(x: usize, z: usize) -> (ColumnCoords, Column) {
    let air = Block::new(BlockId::AIR);
    let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
    (ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub]))
}

/// Floor columns over the world-unit rectangle `xs` x `zs`: every chunk
/// column the rectangle overlaps.
pub fn floor_columns(xs: std::ops::Range<f32>, zs: std::ops::Range<f32>) -> Vec<(ColumnCoords, Column)> {
    super::chunks_over(xs, zs).into_iter().map(|(x, z)| floor_column(x, z)).collect()
}

/// Floor under the whole square between the spawn point (x=128,z=128) and the
/// region's -x/-z corner, continuing 128 units deep into
/// the three neighbours around that corner — so every route is walked
/// grounded, with colliders on both sides of each handoff. Stated in world
/// units, so it holds at any chunk size.
pub fn seam_floor_columns(rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let near = 0.0..5.0 * 64.0;
    let far = REGION_SIZE - 128.0..REGION_SIZE;
    let (xs, zs) = if rc == SPAWN_REGION {
        (near.clone(), near)
    } else if rc == north() {
        (near, far)
    } else if rc == RegionCoords::new(-1, 0) {
        (far, near)
    } else if rc == north_west() {
        (far.clone(), far)
    } else {
        // Far regions: just the chunk under the spawn column.
        return floor_columns(128.0..160.0, 128.0..160.0);
    };
    floor_columns(xs, zs)
}

/// Round budget for one leg of the walk. Lockstep needs ~40 (130 units at
/// walk speed); missed ticks and late inputs stretch it.
pub const LEG_ROUNDS: usize = 160;

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
    pub fn with_floor(
        schedule: Option<InlineSchedule>,
        floor: impl FnMut(RegionCoords) -> Vec<(ColumnCoords, Column)> + Send + 'static,
    ) -> Self {
        let spawner = schedule.map_or_else(InlineSpawner::default, InlineSpawner::with_schedule);
        Self::with_spawner(spawner, floor)
    }

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
        Rig {
            manager: WorldManager::new(
                spawner,
                Box::new(floor),
                out_send,
                region_out_send,
                lod_builder,
                lod_result_recv,
            ),
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
        ownership: game::Ownership,
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

    /// One scheduling round, then the ownership invariant: the player is
    /// owned by at most one region (none while a handoff is in flight).
    pub fn round(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.settle();
        let owners = self.owners();
        assert!(owners.len() <= 1, "player owned by several regions at once: {owners:?}");
        assert_conserved(self);
    }

    /// Test support: kill `rc`'s runner (a dead region thread). Its entities
    /// die with it, except what is in flight to it.
    pub fn kill(&mut self, rc: RegionCoords) {
        self.manager.spawner_mut().kill(rc);
        self.tracked.retain(|_, at| *at != Some(rc));
    }

    pub fn key(&mut self, key: Key, pressed: bool) {
        // Stamped with the spawn region on purpose: the manager routes input
        // by the player's home, not by the client's stamp.
        let ev = GameEvent::new(
            GameEventKind::PlayerInput(CLIENT, InputEvent::Key { key, pressed }),
            0,
            SPAWN_REGION,
        );
        self.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), CLIENT));
    }

    /// Any player input, stamped like [`Rig::key`].
    pub fn input(&mut self, input: InputEvent) {
        let ev = GameEvent::new(GameEventKind::PlayerInput(CLIENT, input), 0, SPAWN_REGION);
        self.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), CLIENT));
    }

    pub fn order(&mut self, order: Order) {
        let ev = GameEvent::new(GameEventKind::Order(CLIENT, order), 0, SPAWN_REGION);
        self.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), CLIENT));
    }

    /// The player's walk (if any) and feet tile in `rc`, which owns it.
    pub fn walk_state(&mut self, rc: RegionCoords) -> (Option<Walker>, TilePos) {
        let mut out = None;
        self.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            let e = *data.player_entites.get(&CLIENT).expect("player owned here");
            let handle = *data.ecs.rigidbody.get(e);
            let t = data.physics.bodies.get(handle).unwrap().translation();
            out = Some((data.ecs.walker.try_get(e).clone(), game::feet_tile(game::feet_of(t))));
        });
        out.expect("region is running")
    }

    /// Points the player's look at `p` (world blocks, fractional) from its
    /// eye, in the region owning it (a forgotten state write, as a test
    /// teleport is).
    pub fn aim_at(&mut self, p: [f64; 3]) {
        let rc = self.owners()[0];
        self.manager.spawner_mut().with_region(rc, |region| {
            region.with_data(|d| {
                let e = *d.data.player_entites.get(&CLIENT).expect("player owned here");
                let (eye, _) = d.eye_ray(CLIENT).expect("a body and camera");
                let (bv, n) = (game::BLOCK_VOXELS as f64, game::REGION_BLOCKS as f64);
                let to = [
                    (p[0] - rc.x as f64 * n) * bv - eye.x as f64,
                    p[1] * bv - eye.y as f64,
                    (p[2] - rc.z as f64 * n) * bv - eye.z as f64,
                ];
                let len = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
                let d3 = [to[0] / len, to[1] / len, to[2] / len];
                // look_quat(yaw, pitch) * -Z = (-cos p sin y, sin p, -cos p cos y).
                let mut cam = d.data.ecs.camera.try_get(e).clone().expect("a camera");
                cam.pitch = d3[1].asin() as f32;
                cam.yaw = (-d3[0]).atan2(-d3[2]) as f32;
                d.data.ecs.camera.set_safe(e, Some(cam));
            });
        });
    }

    /// The block under the player's crosshair (world blocks), and the face
    /// normal, as its owning region's sim sees it.
    pub fn eye_hit(&mut self) -> Option<(game::WorldBlockPos, [i32; 3])> {
        let rc = self.owners()[0];
        let mut out = None;
        self.manager.spawner_mut().with_region(rc, |region| {
            out = region.data().eye_hit(CLIENT).map(|h| {
                let w = game::WorldBlockPos::from_local(rc, game::BlockPos::new(h.block[0], h.block[1], h.block[2]));
                (w, h.normal)
            });
        });
        out
    }

    /// Regions that currently own the player entity.
    pub fn owners(&mut self) -> Vec<RegionCoords> {
        let mut owners = Vec::new();
        for rc in self.manager.running_regions() {
            self.manager.spawner_mut().with_region(rc, |region| {
                if region.data().player_entites.contains_key(&CLIENT) {
                    owners.push(rc);
                }
            });
        }
        owners
    }

    /// Regions that currently own an entity of `kind`, once per entity (a
    /// region owning two shows twice).
    pub fn owners_of(&mut self, kind: game::EntityKind) -> Vec<RegionCoords> {
        let mut owners = Vec::new();
        for rc in self.manager.running_regions() {
            self.manager.spawner_mut().with_region(rc, |region| {
                let data = region.data();
                for &key in data.id_index.values() {
                    if *data.ecs.kind.try_get(key) == Some(kind) {
                        owners.push(rc);
                    }
                }
            });
        }
        owners
    }

    pub fn player_xz(&mut self, rc: RegionCoords) -> (f32, f32) {
        let mut xz = None;
        self.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            let e = *data.player_entites.get(&CLIENT).expect("player owned here");
            let handle = *data.ecs.rigidbody.get(e);
            let t = data.physics.bodies.get(handle).unwrap().translation();
            xz = Some((t.x, t.z));
        });
        xz.expect("region is running")
    }

    /// Connect the client, subscribe the spawn region's 3x3 window and run
    /// until the spawn region owns the player.
    pub fn join_spawn(&mut self) {
        self.event(ServerEvent::connected(CLIENT));
        for rc in SPAWN_REGION.window_3x3() {
            self.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
        }
        self.walk_until_owned_by(SPAWN_REGION, "spawn");
    }

    /// Rounds until `rc` owns the player; panics past the leg budget.
    pub fn walk_until_owned_by(&mut self, rc: RegionCoords, what: &str) {
        for _ in 0..LEG_ROUNDS {
            self.round();
            if self.owners() == [rc] {
                return;
            }
        }
        panic!("{what}: player never reached {rc:?}; owners now {:?}", self.owners());
    }

    /// (region, tick, state hash) of every running region, in region order.
    pub fn fingerprint(&mut self) -> Vec<(RegionCoords, u64, u32)> {
        let mut regions = self.manager.running_regions();
        regions.sort();
        let mut out = Vec::new();
        for rc in regions {
            self.manager.spawner_mut().with_region(rc, |region| {
                out.push((rc, region.current_tick(), game::state_hash(region.data())));
            });
        }
        out
    }
}

/// Each region's authoritative event stream (`Rig::event_streams`): `(id, kind)`.
pub type Streams = BTreeMap<RegionCoords, Vec<(usize, GameEventKind)>>;

/// Panics, naming `what` and the first event that differs, unless `a` and `b`
/// are the same streams.
pub fn assert_same_streams(what: &str, a: &Streams, b: &Streams) {
    let regions = |s: &Streams| s.keys().copied().collect::<Vec<_>>();
    assert_eq!(regions(a), regions(b), "{what}: other regions emitted events");
    for (rc, xs) in a {
        let ys = &b[rc];
        let short = |e: Option<&(usize, GameEventKind)>| {
            let mut d = format!("{e:?}");
            d.truncate(240);
            d
        };
        if let Some(i) = (0..xs.len().max(ys.len())).find(|&i| xs.get(i) != ys.get(i)) {
            panic!(
                "{what}: {rc:?}'s streams part at event {i} of {}/{}:\n  {}\n  {}",
                xs.len(),
                ys.len(),
                short(xs.get(i)),
                short(ys.get(i))
            );
        }
    }
}

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
    /// Start recording what the running regions output (see [`Seen`]).
    pub fn start_trace(&mut self) {
        let mut ticks = std::collections::BTreeMap::new();
        for rc in self.manager.running_regions() {
            self.manager.spawner_mut().with_region(rc, |region| {
                ticks.insert(rc, region.current_tick());
            });
        }
        self.trace = Some((ticks, Vec::new()));
    }

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
