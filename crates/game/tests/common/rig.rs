//! The scheduling rig the seam tests share: a [`WorldManager`] on the
//! [`InlineSpawner`] (lockstep, or jittered by an [`InlineSchedule`]) with one
//! scripted client, its region outputs and packets pumped by hand. Moved out
//! of `handoff_schedule.rs` so the seam-translation harness
//! ([`super::seam_translate`]) drives the same rig.
use crossbeam::channel::{unbounded, Receiver};
use game::{
    ClientId, ClientPacket, Column, ColumnCoords, EntityId, GameEvent, GameEventKind, InlineLodBuilder,
    InlineSchedule, InlineSpawner, InputEvent, Key, LodBuilder, Order, RegionCoords, RegionOutput, ServerEvent,
    ServerPacket, TilePos, Walker, WorldManager, SPAWN_REGION, TICK_RATE,
};
use std::collections::BTreeMap;

/// The conservation invariant of a crossing: every `EntityId` is owned by
/// exactly one running region or sits exactly once in the manager's
/// in-flight table — never both, never twice, and (people aside, whom the
/// history may absorb or end) never lost. Called every round.
pub fn assert_conserved(rig: &mut Rig) {
    let mut places: BTreeMap<EntityId, Vec<Option<RegionCoords>>> = BTreeMap::new();
    let mut people = std::collections::BTreeSet::new();
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |region| {
            let data = region.data();
            for (id, &key) in data.id_index.iter() {
                places.entry(*id).or_default().push(Some(rc));
                if matches!(data.ecs.kind.try_get(key), Some(game::EntityKind::Person { .. })) {
                    people.insert(*id);
                }
            }
        });
    }
    for (id, _target) in rig.manager.in_flight_ids() {
        places.entry(id).or_default().push(None);
    }
    for (id, at) in &places {
        assert_eq!(at.len(), 1, "{id:?} is in several places at once (None = in flight): {at:?}");
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
    /// Latest region snapshot sent to the client, per region: whether it
    /// held the client's player.
    pub snapshot_has_player: std::collections::BTreeMap<RegionCoords, bool>,
}

impl Rig {
    pub fn with_floor(schedule: Option<InlineSchedule>, floor: fn(RegionCoords) -> Vec<(ColumnCoords, Column)>) -> Self {
        // Before any region exists: the seam tests compare runs (placements,
        // seeds) against each other, and a player's starting kit and a broken
        // block's drops read the process-global block props.
        super::install_real_block_props();
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            out: lod_result_send,
        });
        let spawner = schedule.map_or_else(InlineSpawner::default, InlineSpawner::with_schedule);
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
            snapshot_has_player: Default::default(),
        }
    }

    pub fn settle(&mut self) {
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.trace_output(rc, &out);
                self.manager.handle_region_output(rc, out, self.now_ms);
            }
        }
        for (_, packet) in self.packets.try_iter() {
            match packet {
                ServerPacket::PlayerRegion(home, CLIENT) => self.pushed_home = home,
                ServerPacket::Region(rc, state) => {
                    self.snapshot_has_player.insert(rc, state.player_entites.contains_key(&CLIENT));
                }
                _ => {}
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
