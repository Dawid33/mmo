//! Boundary handoff under scheduling jitter. `InlineSpawner`'s default is one
//! fixed schedule (every region ticks once per round, every output lands
//! before the neighbour's next tick); real region threads each run their own
//! timer. `InlineSchedule` replays that jitter from a seed — permuted tick
//! order, inputs that arrive a tick late, missed ticks — over a straight seam
//! crossing and a diagonal one through a region corner, so a schedule that
//! breaks the handoff is a seed to replay rather than a flaky thread test
//! (`crates/server/tests/threaded_world.rs` keeps the real-thread plumbing
//! check). Run via `just test-threads`.
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, GameEvent, GameEventKind,
    InlineLodBuilder, InlineSchedule, InlineSpawner, InputEvent, Key, LodBuilder, RegionCoords,
    RegionOutput, ServerEvent, ServerPacket, SubChunk, WorldManager, SPAWN_REGION, TICK_RATE,
};

/// Seeds swept per schedule mode.
const SEEDS: u64 = 12;
/// Round budget for one leg of the walk. Lockstep needs ~40 (130 units at
/// walk speed); missed ticks and late inputs stretch it.
const LEG_ROUNDS: usize = 160;
/// Rounds walked past the seam before turning back, so the return leg starts
/// clear of the flip hysteresis.
const PAST_SEAM_ROUNDS: usize = 6;

const CLIENT: ClientId = 0;

fn north() -> RegionCoords {
    RegionCoords::new(0, -1)
}

fn north_west() -> RegionCoords {
    RegionCoords::new(-1, -1)
}

/// One scripted walk out of the spawn region and back.
#[derive(Clone, Copy)]
struct Route {
    name: &'static str,
    target: RegionCoords,
    out: &'static [Key],
    back: &'static [Key],
}

/// Straight across one seam.
const NORTH: Route =
    Route { name: "north", target: RegionCoords { x: 0, z: -1 }, out: &[Key::KeyW], back: &[Key::KeyS] };
/// Diagonally through the corner. The spawn point is equidistant from both
/// seams, so the walk reaches them together: whether the player hops through
/// an edge neighbour first or lands in the diagonal one directly is up to the
/// schedule — it must end up owned by the diagonal region either way.
const CORNER: Route = Route {
    name: "corner",
    target: RegionCoords { x: -1, z: -1 },
    out: &[Key::KeyW, Key::KeyA],
    back: &[Key::KeyS, Key::KeyD],
};

fn floor_column(x: usize, z: usize) -> (ColumnCoords, Column) {
    let air = Block::new(BlockId::AIR);
    let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
    (ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub]))
}

/// Floor under the whole square between the spawn point (x=128,z=128) and the
/// region's -x/-z corner, continuing two columns deep into the three
/// neighbours around that corner — so every route is walked grounded, with
/// colliders on both sides of each handoff.
fn seam_floor_columns(rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let (near, far) = (0..=4, 30..=31);
    let (xs, zs) = if rc == SPAWN_REGION {
        (near.clone(), near)
    } else if rc == north() {
        (near, far)
    } else if rc == RegionCoords::new(-1, 0) {
        (far, near)
    } else if rc == north_west() {
        (far.clone(), far)
    } else {
        return vec![floor_column(4, 4)];
    };
    xs.flat_map(|x| zs.clone().map(move |z| floor_column(x, z))).collect()
}

struct Rig {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    now_ms: u64,
    /// Latest authoritative `PlayerRegion` push — what a client would believe.
    pushed_home: Option<RegionCoords>,
}

impl Rig {
    fn new(schedule: Option<InlineSchedule>) -> Self {
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
                Box::new(seam_floor_columns),
                out_send,
                region_out_send,
                lod_builder,
                lod_result_recv,
            ),
            region_out,
            packets,
            now_ms: 0,
            pushed_home: None,
        }
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, self.now_ms);
            }
        }
        for (_, packet) in self.packets.try_iter() {
            if let ServerPacket::PlayerRegion(home, CLIENT) = packet {
                self.pushed_home = home;
            }
        }
    }

    fn event(&mut self, ev: ServerEvent) {
        assert!(self.manager.handle_server_event(ev, self.now_ms));
        self.settle();
    }

    /// One scheduling round, then the ownership invariant: the player is
    /// owned by at most one region (none while a handoff is in flight).
    fn round(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.settle();
        let owners = self.owners();
        assert!(owners.len() <= 1, "player owned by several regions at once: {owners:?}");
    }

    fn key(&mut self, key: Key, pressed: bool) {
        // Stamped with the spawn region on purpose: the manager routes input
        // by the player's home, not by the client's stamp.
        let ev = GameEvent::new(
            GameEventKind::PlayerInput(CLIENT, InputEvent::Key { key, pressed }),
            0,
            SPAWN_REGION,
        );
        self.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), CLIENT));
    }

    /// Regions that currently own the player entity.
    fn owners(&mut self) -> Vec<RegionCoords> {
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

    fn player_xz(&mut self, rc: RegionCoords) -> (f32, f32) {
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

    /// Rounds until `rc` owns the player; panics past the leg budget.
    fn walk_until_owned_by(&mut self, rc: RegionCoords, what: &str) {
        for _ in 0..LEG_ROUNDS {
            self.round();
            if self.owners() == [rc] {
                return;
            }
        }
        panic!("{what}: player never reached {rc:?}; owners now {:?}", self.owners());
    }

    /// (region, tick, state hash) of every running region, in region order.
    fn fingerprint(&mut self) -> Vec<(RegionCoords, usize, u32)> {
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

/// Walk `route` out of the spawn region, a little further, then back home.
/// Returns the final fingerprint so callers can compare runs.
fn walk_there_and_back(
    route: Route,
    schedule: Option<InlineSchedule>,
) -> Vec<(RegionCoords, usize, u32)> {
    let mut rig = Rig::new(schedule);
    rig.event(ServerEvent::ClientConnected(CLIENT));
    for rc in SPAWN_REGION.window_3x3() {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
    }
    // A late spawn region creates the player a round or two after connect.
    rig.walk_until_owned_by(SPAWN_REGION, "spawn");

    rig.key(Key::KeyE, true);
    rig.key(Key::KeyE, false);
    for &key in route.out {
        rig.key(key, true);
    }
    rig.walk_until_owned_by(route.target, "outbound");
    assert_eq!(rig.pushed_home, Some(route.target), "client told about its new home");

    // Input follows the player across the flip: the keys are still held, so
    // it keeps walking -z in the new region without any new key event.
    let (_, z_arrived) = rig.player_xz(route.target);
    for _ in 0..PAST_SEAM_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.owners(), [route.target], "no thrash back across the seam");
    let (_, z_walked) = rig.player_xz(route.target);
    assert!(z_walked < z_arrived, "held input died at the handoff: z {z_arrived} -> {z_walked}");

    // Turn around. These key events are still stamped with the OLD home.
    for &key in route.out {
        rig.key(key, false);
    }
    for &key in route.back {
        rig.key(key, true);
    }
    rig.walk_until_owned_by(SPAWN_REGION, "return");
    assert_eq!(rig.pushed_home, Some(SPAWN_REGION), "client told it is home again");
    for &key in route.back {
        rig.key(key, false);
    }

    // Let ghost mirrors and late inputs drain, then the player must still be
    // exactly where ownership says it is.
    for _ in 0..PAST_SEAM_ROUNDS {
        rig.round();
    }
    assert_eq!(rig.owners(), [SPAWN_REGION]);
    rig.fingerprint()
}

/// Sweeps `SEEDS` seeds of one schedule mode over every route; returns how
/// many runs ended in a different state than lockstep, so callers can prove
/// the jitter bit.
fn sweep(mode: &str, schedule: fn(u64) -> InlineSchedule) -> usize {
    let mut perturbed = 0;
    for route in [NORTH, CORNER] {
        let lockstep = walk_there_and_back(route, None);
        for seed in 0..SEEDS {
            let run = || {
                std::panic::catch_unwind(|| walk_there_and_back(route, Some(schedule(seed))))
                    .unwrap_or_else(|_| {
                        panic!("{} handoff broke under schedule `{mode}`, seed {seed}", route.name)
                    })
            };
            let first = run();
            assert_eq!(
                first,
                run(),
                "{} under schedule `{mode}` seed {seed} is not reproducible",
                route.name
            );
            perturbed += usize::from(first != lockstep);
        }
    }
    perturbed
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lockstep_walk_there_and_back() {
    walk_there_and_back(NORTH, None);
    walk_there_and_back(CORNER, None);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_survives_permuted_tick_order() {
    // Order alone may legitimately end bit-identical to lockstep: regions
    // share no state, so only the manager's output order moves.
    sweep("shuffled", InlineSchedule::shuffled);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_survives_late_inputs() {
    let perturbed = sweep("late", InlineSchedule::late);
    assert!(perturbed > 0, "late inputs never changed the outcome — schedule is a no-op");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn handoff_survives_missed_ticks() {
    let perturbed = sweep("jittery", InlineSchedule::jittery);
    assert!(perturbed > 0, "missed ticks never changed the outcome — schedule is a no-op");
}
