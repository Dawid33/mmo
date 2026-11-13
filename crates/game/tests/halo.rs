//! The read-only halo (`game::halo`, `game::stud_halo`): neighbours' stud
//! chunks, piles, chests and bodies within `HALO_BLOCKS` of each edge,
//! mirrored into every running region, kept fresh by stamped `HaloUpdate`
//! transfers. This file drives it through the manager (the seam rig); the
//! hand-driven stud halo cases are `stud_halo::`, the walk and the edits
//! across a seam `handoff_schedule::stud_*`.
use game::{
    BlockPos, GameEventKind, Key, Pose, Region, RegionCoords, StudHaloDelta, WorldBlockPos, WorldChunkPos, MAT_STONE,
    REGION_BLOCKS, REGION_CHUNKS, SPAWN_REGION, STUD_CHUNK_STUDS, TILE_PLATES,
};

use crate::common::{self, FLOOR_PLATES, STAND};
use crate::common::rig::{Rig, CLIENT};
use crate::common::seam_translate::uniform_floor;
use crate::common::water::{floor_with_water, WaterBox};

/// Stud columns along a region's edge.
const REGION_STUDS: i32 = (REGION_CHUNKS * STUD_CHUNK_STUDS) as i32;

/// A pile of `n` dirt, pickable at once.
fn pile(n: u32) -> game::ItemPile {
    game::ItemPile { stacks: vec![game::ItemStack { item: game::ItemId(1), count: n }], expire_tick: 6000, pickup_after_tick: 0 }
}

fn north() -> RegionCoords {
    RegionCoords::new(SPAWN_REGION.x, SPAWN_REGION.z - 1)
}

fn east() -> RegionCoords {
    RegionCoords::new(SPAWN_REGION.x + 1, SPAWN_REGION.z)
}

/// A rig over flat stud ground everywhere, with the client's 3x3 window
/// running and the spawn region owning the player.
fn joined() -> Rig {
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    rig
}

fn tick_of(rig: &mut Rig, rc: RegionCoords) -> u64 {
    let mut t = None;
    rig.manager.spawner_mut().with_region(rc, |r| t = Some(r.current_tick()));
    t.expect("region is running")
}

/// The spawn region's frame for stud cell `cell` of region `of` (north or east).
fn from_spawn(of: RegionCoords, cell: [i32; 3]) -> [i64; 3] {
    let (dx, dz) = (of.x - SPAWN_REGION.x, of.z - SPAWN_REGION.z);
    [(cell[0] + dx * REGION_STUDS) as i64, cell[1] as i64, (cell[2] + dz * REGION_STUDS) as i64]
}

/// The material at stud cell `cell` of region `of` as the spawn region sees it
/// (own ∪ halo): `None` is air, or outside own ∪ halo.
fn seen_from_spawn(rig: &mut Rig, of: RegionCoords, cell: [i32; 3]) -> Option<u16> {
    let mut m = None;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| m = r.data().stud_material_seen(from_spawn(of, cell)));
    m
}

/// The box of the top ground plate over the 2 x 2 studs at `(x, z)` of a region.
fn top_plate(x: i32, z: i32) -> ([i32; 3], [i32; 3]) {
    ([x, FLOOR_PLATES - 1, z], [x + 2, FLOOR_PLATES, z + 2])
}

/// Carves `lo .. hi` out of `rc` between ticks, as an event of the slot that
/// opens at its current tick would.
fn carve(rig: &mut Rig, rc: RegionCoords, (lo, hi): ([i32; 3], [i32; 3])) {
    rig.manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| assert!(!d.carve_stud_safe(lo, hi).is_empty(), "{rc:?} {lo:?} carved"));
    });
}

/// Fills `lo .. hi` of `rc` with stone between ticks.
fn fill(rig: &mut Rig, rc: RegionCoords, (lo, hi): ([i32; 3], [i32; 3])) {
    rig.manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| assert!(d.fill_stud_box_safe(lo, hi, MAT_STONE) > 0, "{rc:?} {lo:?} filled"));
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn neighbour_edit_reaches_halo_next_tick() {
    let mut rig = joined();
    // 2 m inside north, on its edge facing the spawn region: the ground's top plate.
    let at = top_plate(20, REGION_STUDS - 4);
    assert!(seen_from_spawn(&mut rig, north(), at.0).is_some(), "the spawn region's halo holds north's ground");
    let t = tick_of(&mut rig, north());
    assert_eq!(tick_of(&mut rig, SPAWN_REGION), t, "lockstep");
    carve(&mut rig, north(), at);
    assert!(seen_from_spawn(&mut rig, north(), at.0).is_some(), "slot {t}: not yet");
    rig.round();
    assert_eq!(tick_of(&mut rig, SPAWN_REGION), t + 1);
    assert_eq!(seen_from_spawn(&mut rig, north(), at.0), None, "slot {}: the carve is in the halo", t + 1);
    let below = [at.0[0], at.0[1] - 1, at.0[2]];
    assert!(seen_from_spawn(&mut rig, north(), below).is_some(), "the plate under it still stands");
    // Outside own ∪ halo: unknown.
    let far = [20, FLOOR_PLATES - 1, REGION_STUDS / 2];
    assert_eq!(seen_from_spawn(&mut rig, north(), far), None);
}

/// A halo seed and a later patch of one chunk roll back bit-exact (the
/// rollback bar: `hash(before) == hash(after undo)`), colliders, pile and
/// chest mirrors included.
#[test]
fn halo_updates_roll_back_bit_exact() {
    crate::common::install_real_block_props();
    let n = REGION_CHUNKS as i32;
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(game::Rollback::new(Some(send.clone())), Some(send), SPAWN_REGION, None);
    region.seed_stud_world(common::uniform_seed());
    let pile_cell = WorldBlockPos::from_local(SPAWN_REGION, game::BlockPos::new(REGION_BLOCKS as i32, STAND, 3));
    // East's west columns, as east would seed them.
    let mut seed = common::stud_halo_seed(east(), SPAWN_REGION, &common::uniform_seed()).expect("neighbours");
    // A pile on the neighbour's edge cell: its mirror rolls back too.
    seed.piles = vec![(pile_cell, Some(pile(3)))];
    // And a chest beside it (Task 6.4).
    seed.containers = vec![(chest_cell(), Some(game::Container::chest()))];
    let before = game::state_hash(region.data());
    region.handle_event(GameEventKind::HaloUpdate(seed.clone())).unwrap();
    assert!(region.data().stud_halo_chunk_at([n, 0, 0]).is_some());
    assert_eq!(region.data().halo_piles().len(), 1, "the seed's pile is mirrored");
    {
        use game::WorldView as _;
        assert_eq!(region.data().view().pile(pile_cell), Some(&pile(3)), "WorldView reads the mirror");
        assert_eq!(region.data().view().container(chest_cell()), Some(&game::Container::chest()), "and the chest's");
    }
    let bodies = region.data().physics.bodies.len();
    region.rollback_last_event();
    assert_eq!(game::state_hash(region.data()), before, "seed undone exactly");
    // Seed for good, then a patch: one chunk loses a box of ground.
    region.handle_event(GameEventKind::HaloUpdate(seed.clone())).unwrap();
    region.forget_last_event();
    assert_eq!(region.data().physics.bodies.len(), bodies);
    let seeded = game::state_hash(region.data());
    let mut dug = region.data().stud_halo_chunk_at([n, 0, 0]).expect("the seeded edge chunk").clone();
    assert!(!dug.carve([6, FLOOR_PLATES as u8 - 6, 6], [8, FLOOR_PLATES as u8, 8]).is_empty(), "the patch digs a tile");
    let patch = game::HaloUpdate {
        replace: false,
        studs: vec![StudHaloDelta { chunk: WorldChunkPos::from_local(SPAWN_REGION, [n, 0, 0]), data: Some(dug) }],
        stud_ground: Vec::new(),
        piles: vec![(pile_cell, Some(pile(5)))],
        containers: vec![(chest_cell(), Some(stocked_chest()))],
        ..seed
    };
    region.handle_event(GameEventKind::HaloUpdate(patch)).unwrap();
    assert_ne!(game::state_hash(region.data()), seeded);
    assert_eq!(region.data().halo_containers().get(&chest_cell()), Some(&stocked_chest()), "the chest delta");
    region.rollback_last_event();
    assert_eq!(game::state_hash(region.data()), seeded, "patch undone exactly");
    // The source gone: its pile mirrors go (nobody answers takes of them).
    region.handle_event(GameEventKind::HaloSourceGone(east())).unwrap();
    assert!(region.data().halo_piles().is_empty(), "no mirror of a gone source's pile");
    assert!(region.data().halo_containers().is_empty(), "nor of its chest");
    region.rollback_last_event();
    assert_eq!(game::state_hash(region.data()), seeded, "the drop undoes exactly");
}

/// A neighbour that parks hands its final terrain to the running regions
/// that mirror it: an edit it never ticked (so never sent as a delta) still
/// reaches their halo once its `Stopped` is handled.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn parked_neighbour_reseeds_running_halo() {
    let mut rig = joined();
    let at = top_plate(24, REGION_STUDS - 2);
    rig.event(game::ServerEvent::ClientPacket(game::ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    // Edited between ticks, then parked before it ticks again.
    carve(&mut rig, north(), at);
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    rig.settle();
    assert!(rig.manager.parked_regions().contains(&north()), "north parked");
    assert_eq!(seen_from_spawn(&mut rig, north(), at.0), None, "the parked edit is in the halo");
    assert!(seen_from_spawn(&mut rig, north(), [at.0[0] + 2, at.0[1], at.0[2]]).is_some(), "beside it the ground stands");
}

/// The mirror `rc` holds of entity `id`, if any.
fn mirror_in(rig: &mut Rig, rc: RegionCoords, id: game::EntityId) -> Option<game::EntityKey> {
    let mut m = None;
    rig.manager.spawner_mut().with_region(rc, |r| m = r.data().halo_entity(id).map(|h| h.entity));
    m
}

/// A slime in north's edge band facing the spawn region: north owns it, the
/// spawn region mirrors it. Returns its id.
fn slime_in_north_band(rig: &mut Rig) -> game::EntityId {
    let bv = common::TILE_W;
    let size = game::REGION_SIZE;
    // 1.5 tiles short of north's +z edge, mid-region in x, on the floor.
    let pos = [size / 2.0, common::above_floor_y(), size - 1.5 * bv];
    let mut id = None;
    rig.manager.spawner_mut().with_region(north(), |r| {
        let tick = r.current_tick();
        let key = crate::common::spawn_slime(r, pos, 1, 0.0, tick);
        id = r.data().entity_id(key);
    });
    let id = id.expect("the slime has an EntityId");
    rig.round();
    rig.round();
    assert!(mirror_in(rig, SPAWN_REGION, id).is_some(), "the spawn region mirrors north's slime");
    id
}

/// An owner that parks takes its mirrors with it at once: the neighbour's
/// halo entity is gone in the slot after the owner's last, not after a TTL.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn halo_entity_removed_explicitly_on_park() {
    let mut rig = joined();
    let id = slime_in_north_band(&mut rig);
    rig.event(game::ServerEvent::ClientPacket(game::ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    let last = tick_of(&mut rig, north());
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    rig.settle();
    assert!(rig.manager.parked_regions().contains(&north()), "north parked");
    // No round since: the spawn region is in the slot north's last stamp opens.
    assert_eq!(tick_of(&mut rig, SPAWN_REGION), last, "lockstep, nothing ticked since the park");
    assert_eq!(mirror_in(&mut rig, SPAWN_REGION, id), None, "the parked owner's mirror is gone");
}

/// A body that walks across the seam takes over its own mirror: the arrival
/// upgrades the halo entity in place, under the same `EntityKey` (the
/// renderer keeps one entity throughout).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn upgrade_in_place_keeps_key() {
    let mut rig = joined();
    rig.key(Key::KeyE, true);
    rig.key(Key::KeyE, false);
    for _ in 0..10 {
        rig.round();
    }
    let bv = common::TILE_W;
    let mut id = None;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        r.with_data(|d| {
            let e = *d.player_entites.get(&CLIENT).unwrap();
            id = d.entity_id(e);
            let t = d.physics.bodies.get(*d.ecs.rigidbody.get(e)).unwrap().translation();
            d.set_body_pose_safe(e, Pose::translation(REGION_BLOCKS as f32 / 2.0 * bv, t.y, 1.0 * bv));
        });
    });
    let id = id.expect("the player has an EntityId");
    rig.round();
    rig.round();
    let mirror = mirror_in(&mut rig, north(), id).expect("north mirrors the player in the spawn region's band");
    rig.key(Key::KeyW, true);
    rig.walk_until_owned_by(north(), "north");
    rig.key(Key::KeyW, false);
    let mut owned = None;
    rig.manager.spawner_mut().with_region(north(), |r| owned = r.data().player_entites.get(&CLIENT).copied());
    assert_eq!(owned, Some(mirror), "the arrival took over the mirror's key");
    assert_eq!(mirror_in(&mut rig, north(), id), None, "no mirror left beside the owned body");
}

/// A neighbour lagging behind a region that parks: a halo delta the parker
/// sent earlier is still held there when the final-terrain reseed arrives.
/// The reseed is stamped with the parker's last tick, so it lands after the
/// held delta and the final terrain wins.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn parked_reseed_wins_over_held_delta_in_lagging_neighbour() {
    let mut rig = joined();
    let at = top_plate(28, REGION_STUDS - 2);
    rig.event(game::ServerEvent::ClientPacket(game::ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    // The spawn region falls behind for a while.
    rig.manager.spawner_mut().hold(SPAWN_REGION, 6);
    // North carves the plate; its next tick sends the delta, which the
    // lagging spawn region holds for a later slot.
    carve(&mut rig, north(), at);
    rig.now_ms += game::TICK_RATE;
    rig.manager.spawner_mut().tick_all();
    rig.settle();
    // Then puts stone back and parks before ticking again: only the reseed
    // carries the final bricks.
    fill(&mut rig, north(), at);
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    rig.settle();
    assert!(rig.manager.parked_regions().contains(&north()), "north parked");
    // The spawn region catches up past every stamp.
    for _ in 0..12 {
        rig.now_ms += game::TICK_RATE;
        rig.manager.spawner_mut().tick_all();
        rig.settle();
    }
    assert_eq!(seen_from_spawn(&mut rig, north(), at.0), Some(MAT_STONE), "the final terrain won");
}

/// A region lost without `Stopped` (its thread died; the manager finds out
/// when it asks it to shut down) takes its mirrors in the running
/// neighbours with it: nothing else would ever end them.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn lost_region_mirrors_dropped_on_unload() {
    let mut rig = joined();
    let id = slime_in_north_band(&mut rig);
    rig.event(game::ServerEvent::ClientPacket(game::ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    rig.kill(north());
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    rig.settle();
    assert!(!rig.manager.running_regions().contains(&north()), "north is gone");
    assert_eq!(mirror_in(&mut rig, SPAWN_REGION, id), None, "the lost owner's mirror is gone");
}



/// The chest cell [`halo_updates_roll_back_bit_exact`] mirrors: east's edge
/// column, next to its pile.
fn chest_cell() -> WorldBlockPos {
    WorldBlockPos::from_local(SPAWN_REGION, game::BlockPos::new(REGION_BLOCKS as i32 + 1, STAND, 4))
}

/// A chest holding one stack.
fn stocked_chest() -> game::Container {
    let mut c = game::Container::chest();
    c.slots[2] = Some(game::ItemStack { item: game::ItemId(1), count: 7 });
    c
}

/// Tile levels of recipe water over the pond's bed (the flat floor's top).
const POND_DEPTH: i32 = 3;

/// Recipe water `POND_DEPTH` tiles deep (tile levels `STAND ..= STAND + 2` over
/// the flat floor) over world tile columns `x` −3..3 (straddling the spawn
/// region's east seam), `z` 6..10.
fn seam_pond(rc: RegionCoords) -> game::StudRegionSeed {
    let n = REGION_BLOCKS as i64;
    let seam = east().x as i64 * n;
    let pond = WaterBox {
        xs: seam - 3..seam + 3,
        zs: SPAWN_REGION.z as i64 * n + 6..SPAWN_REGION.z as i64 * n + 10,
        surface: STAND - 1,
        water_top: STAND - 1 + POND_DEPTH,
    };
    floor_with_water(rc, &[pond])
}

/// Water units at world tile `p` as region `rc` sees it.
fn units_from(rig: &mut Rig, rc: RegionCoords, p: WorldBlockPos) -> Option<u16> {
    let mut u = None;
    rig.manager.spawner_mut().with_region(rc, |r| u = Some(r.data().water_units_at(p)));
    u
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn water_units_at_reads_the_halo() {
    common::install_real_block_props();
    let mut rig = Rig::with_floor(None, seam_pond);
    rig.join_spawn();
    rig.round();
    // 2 tiles inside the east neighbour, in its recipe water.
    let at = WorldBlockPos::from_local(SPAWN_REGION, BlockPos::new(REGION_BLOCKS as i32 + 1, STAND + POND_DEPTH - 1, 8));
    assert_eq!(units_from(&mut rig, SPAWN_REGION, at), Some(game::WATER_FULL));
}

/// A pond whose recipe straddles the east seam reads full on both sides of
/// it from either region (own tiles and halo tiles alike); bricks filled
/// into a water tile by its owner read dry in the neighbour's halo after
/// the T+1 halo delta.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn halo_carries_recipe_water() {
    common::install_real_block_props();
    let mut rig = Rig::with_floor(None, seam_pond);
    rig.join_spawn();
    rig.round();
    let n = REGION_BLOCKS as i64;
    let seam = east().x as i64 * n;
    let z = SPAWN_REGION.z as i64 * n + 8;
    for rc in [SPAWN_REGION, east()] {
        for x in seam - 3..seam + 3 {
            for y in STAND..STAND + POND_DEPTH {
                let p = WorldBlockPos::new(x, y, z);
                assert_eq!(units_from(&mut rig, rc, p), Some(game::WATER_FULL), "{rc:?} reads {p:?}");
            }
            assert_eq!(units_from(&mut rig, rc, WorldBlockPos::new(x, STAND - 1, z)), Some(0), "{rc:?}: the floor");
            assert_eq!(units_from(&mut rig, rc, WorldBlockPos::new(x, STAND + POND_DEPTH, z)), Some(0), "{rc:?}: above the pond");
        }
        assert_eq!(units_from(&mut rig, rc, WorldBlockPos::new(seam + 3, STAND + 1, z)), Some(0), "{rc:?}: past the pond");
    }
    // East fills its own water tile 1 tile past the seam with stone.
    let cell = WorldBlockPos::new(seam + 1, STAND + 1, z);
    let (_, local) = cell.to_local();
    let (lo, hi) = game::stud_tile_box([local.x, local.y, local.z]).expect("a tile of the region");
    fill(&mut rig, east(), (lo, hi));
    assert_eq!(units_from(&mut rig, east(), cell), Some(0), "the owner reads its bricks at once");
    assert_eq!(units_from(&mut rig, SPAWN_REGION, cell), Some(game::WATER_FULL), "the halo: not yet");
    rig.round();
    assert_eq!(units_from(&mut rig, SPAWN_REGION, cell), Some(0), "after the T+1 halo delta");
}

/// A world store stand-in holding one neighbour's terrain.
struct OneStored(RegionCoords, game::TerrainSnapshot);

impl game::StoredTerrain for OneStored {
    fn load_terrain(&self, rc: RegionCoords) -> Result<Option<game::TerrainSnapshot>, String> {
        Ok((rc == self.0).then(|| self.1.clone()))
    }
}

/// East as an earlier process saved it: the flat ground, with a tile on its
/// west edge (facing the spawn region) dug out. Returns the dug box.
fn east_saved_dug() -> (([i32; 3], [i32; 3]), game::TerrainSnapshot) {
    let dug = ([2, FLOOR_PLATES - TILE_PLATES as i32, 14], [4, FLOOR_PLATES, 16]);
    let mut region = common::region_of(east(), common::uniform_seed());
    region.with_data(|d| assert!(!d.carve_stud_safe(dug.0, dug.1).is_empty()));
    (dug, region.terrain_snapshot())
}

/// A neighbour the store holds that has not run in this process seeds the
/// halo from its saved terrain (not pristine worldgen); one the store cannot
/// serve falls back to worldgen.
#[test]
fn stored_neighbour_seeds_halo_from_store() {
    crate::common::install_real_block_props();
    let (out, _out_recv) = crossbeam::channel::unbounded();
    let region = common::region_of(SPAWN_REGION, common::uniform_seed());
    let mut runner = game::RegionRunner::new(SPAWN_REGION, region, out);
    runner.set_stud_halo_generator(std::sync::Arc::new(|_rc, xs, zs| {
        let mut seed = common::uniform_seed();
        seed.ground.retain(|(c, _)| xs.contains(&c.x) && zs.contains(&c.z));
        seed
    }));
    let (dug, snap) = east_saved_dug();
    runner.set_stored_terrain(std::sync::Arc::new(OneStored(east(), snap)));
    // North is listed but the store has nothing for it: worldgen.
    runner.handle_input(game::RegionInput::SeedHaloFromStore(vec![east(), north()]));

    let data = runner.region_mut().data();
    let seen = |of: RegionCoords, cell: [i32; 3]| data.stud_material_seen(from_spawn(of, cell));
    assert_eq!(seen(east(), dug.0), None, "the saved dig is in the halo");
    assert!(seen(east(), [dug.0[0], dug.0[1], dug.0[2] + 2]).is_some(), "the rest of the saved ground too");
    assert!(seen(east(), [dug.0[0], dug.0[1] - 1, dug.0[2]]).is_some(), "and what lies under the dig");
    let north_floor = seen(north(), [20, FLOOR_PLATES - 1, REGION_STUDS - 4]);
    assert!(north_floor.is_some(), "an unserved neighbour comes from worldgen");
}

/// The manager sends `SeedHaloFromStore` for a starting region's stored,
/// never-run neighbours, and worldgen seeds for the rest, when regions
/// generate on their own threads.
#[test]
fn stored_neighbours_are_seeded_from_the_store_by_their_region() {
    use std::sync::{Arc, Mutex};
    struct Recording(Arc<Mutex<Vec<(RegionCoords, crossbeam::channel::Receiver<game::RegionInput>)>>>);
    impl game::RegionSpawner for Recording {
        fn world_tick(&self) -> game::Tick {
            0
        }
        fn spawn(
            &mut self,
            id: RegionCoords,
            _seed: game::RegionSeed,
            _out: crossbeam::channel::Sender<(RegionCoords, game::RegionOutput)>,
        ) -> crossbeam::channel::Sender<game::RegionInput> {
            let (send, recv) = crossbeam::channel::unbounded();
            self.0.lock().unwrap().push((id, recv));
            send
        }
        fn seeds_halos_on_region_threads(&self) -> bool {
            true
        }
    }
    let spawned = Arc::new(Mutex::new(Vec::new()));
    let (out_send, _packets) = crossbeam::channel::unbounded();
    let (region_out_send, _region_out) = crossbeam::channel::unbounded();
    let mut m = common::stud_manager(Recording(spawned.clone()), out_send, region_out_send);
    m.set_stored_regions([(east(), 3)].into());
    m.handle_server_event(game::ServerEvent::connected(1), 0);

    let spawned = spawned.lock().unwrap();
    let (_, inputs) = spawned.iter().find(|(id, _)| *id == SPAWN_REGION).expect("the spawn region started");
    let (mut from_store, mut from_worldgen) = (Vec::new(), Vec::new());
    for input in inputs.try_iter() {
        match input {
            game::RegionInput::SeedHaloFromStore(n) => from_store.extend(n),
            game::RegionInput::SeedHaloFromWorldgen(n) => from_worldgen.extend(n),
            _ => {}
        }
    }
    assert_eq!(from_store, vec![east()]);
    assert_eq!(from_worldgen.len(), 7, "the other neighbours: {from_worldgen:?}");
    assert!(!from_worldgen.contains(&east()));
}

// ---- stored terrain of other hosts' regions (R3 Task 19) --------------------------------

/// Spawns regions on channels the test reads (`Recording`): what the manager sends them.
struct RecordingSpawner(std::sync::Arc<std::sync::Mutex<Vec<(RegionCoords, crossbeam::channel::Receiver<game::RegionInput>)>>>);

impl game::RegionSpawner for RecordingSpawner {
    fn world_tick(&self) -> game::Tick {
        0
    }
    fn spawn(
        &mut self,
        id: RegionCoords,
        _seed: game::RegionSeed,
        _out: crossbeam::channel::Sender<(RegionCoords, game::RegionOutput)>,
    ) -> crossbeam::channel::Sender<game::RegionInput> {
        let (send, recv) = crossbeam::channel::unbounded();
        self.0.lock().unwrap().push((id, recv));
        send
    }
    fn seeds_halos_on_region_threads(&self) -> bool {
        true
    }
}

/// A manager owning the spawn region (one of two hosts, every region its own
/// super-region) with its spawn region running, a neighbour `f` of it the other host owns
/// and a neighbour `l` it owns itself (never run), and the spawn region's input channel.
struct TwoHostManager {
    m: game::WorldManager<RecordingSpawner>,
    mine: u32,
    f: RegionCoords,
    l: RegionCoords,
    spawn_inputs: crossbeam::channel::Receiver<game::RegionInput>,
}

fn two_host_manager() -> TwoHostManager {
    let s = 1;
    let mine = game::rendezvous_owner(game::super_region_of(SPAWN_REGION, s), 2);
    let ownership = game::Ownership::Static { ordinal: mine, hosts: 2 };
    let f = game::halo::neighbours(SPAWN_REGION).find(|rc| !ownership.owns(*rc, s)).expect("a neighbour of the other host");
    let l = game::halo::neighbours(SPAWN_REGION).find(|rc| ownership.owns(*rc, s)).expect("a neighbour of this host");
    let spawned = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (out_send, _packets) = crossbeam::channel::unbounded();
    let (region_out_send, _region_out) = crossbeam::channel::unbounded();
    let mut m = common::stud_manager(RecordingSpawner(spawned.clone()), out_send, region_out_send);
    m.set_ownership(ownership, s);
    m.set_host_generation(1);
    m.handle_server_event(game::ServerEvent::connected(1), 0);
    let spawn_inputs = {
        let spawned = spawned.lock().unwrap();
        let (_, inputs) = spawned.iter().find(|(id, _)| *id == SPAWN_REGION).expect("the spawn region started");
        inputs.clone()
    };
    assert!(m.running_regions().contains(&SPAWN_REGION));
    assert!(!m.running_regions().contains(&l), "only the spawn region runs");
    for _ in spawn_inputs.try_iter() {}
    TwoHostManager { m, mine, f, l, spawn_inputs }
}

/// The neighbours `SeedHaloFromStore` asked the spawn region for since the last call.
fn store_seeds(inputs: &crossbeam::channel::Receiver<game::RegionInput>) -> Vec<RegionCoords> {
    let mut out = Vec::new();
    for input in inputs.try_iter() {
        if let game::RegionInput::SeedHaloFromStore(n) = input {
            out.extend(n);
        }
    }
    out
}

/// Another host's region next to a running one, not running there (parked): each rise of
/// its stored terrain version re-seeds the running region's halo of it from the store,
/// once; an unchanged version does nothing. A region of this host that does not run here
/// (its parked terrain is the newest there is) and one that runs here are left alone.
#[test]
fn stored_terrain_change_reseeds_a_parked_foreign_halo() {
    let TwoHostManager { mut m, f, l, spawn_inputs, .. } = two_host_manager();
    assert!(!m.foreign_running().contains(&f));
    m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&spawn_inputs), vec![f], "its first saved version re-seeds the halo");
    assert_eq!(m.stored_regions().get(&f), Some(&1));
    m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&spawn_inputs), Vec::<RegionCoords>::new(), "an unchanged version: nothing");
    m.stored_terrain_changed([(f, 2)].into());
    assert_eq!(store_seeds(&spawn_inputs), vec![f], "a newer version: once more");

    m.stored_terrain_changed([(l, 5), (SPAWN_REGION, 9)].into());
    assert_eq!(store_seeds(&spawn_inputs), Vec::<RegionCoords>::new(), "this host's own regions: nothing");
    assert_eq!(m.stored_regions().get(&l), Some(&5), "a region of this host not running here: its version noted");
    assert_eq!(m.stored_regions().get(&SPAWN_REGION), None, "a region running here: untouched");
}

/// A PeerLink end the test keeps a hold of (to drop and restore the connection).
struct HeldLink(std::sync::Arc<std::sync::Mutex<game::InlinePeerLink>>);

impl game::PeerLink for HeldLink {
    fn send(&mut self, msg: game::PeerMsg) {
        self.0.lock().unwrap().send(msg)
    }
    fn recv(&mut self) -> Option<game::PeerMsg> {
        self.0.lock().unwrap().recv()
    }
    fn is_up(&self) -> bool {
        self.0.lock().unwrap().is_up()
    }
    fn connection(&self) -> u64 {
        self.0.lock().unwrap().connection()
    }
    fn pump(&mut self, now_ms: u64) {
        self.0.lock().unwrap().pump(now_ms)
    }
}

/// [`two_host_manager`] with a peer session to the other host on which `f` ran (its halo
/// live, version 1 noted while live), then the link dropped: `f`'s halo is no longer live
/// (as if it parked meanwhile, its final halo and `RegionParked` lost with the link).
fn two_host_manager_with_a_dropped_link() -> TwoHostManager {
    use game::{HostEndpoint, PeerHello, PeerLink as _, PeerMsg, PeerSession, Volatile};
    let mut t = two_host_manager();
    let (mine, f) = (t.mine, t.f);
    let other = 1 - mine;
    let hello = |ordinal| PeerHello {
        ordinal,
        boot: 7 + u64::from(ordinal),
        generation: 1,
        endpoint: HostEndpoint { ordinal, quic: None, webtransport: None, cert_hash: None },
        received_through: 0,
        received_from_boot: None,
    };
    let (here, mut there) = game::InlinePeerLink::pair(3, 0);
    let here = std::sync::Arc::new(std::sync::Mutex::new(here));
    t.m.set_peer(other, PeerSession::new(hello(mine), other), Box::new(HeldLink(here.clone())));
    let mut now = 0;
    let mut pump = |m: &mut game::WorldManager<RecordingSpawner>, there: &mut game::InlinePeerLink| {
        now += 1;
        m.pump_peers(now);
        there.pump(now);
        for _ in std::iter::from_fn(|| there.recv()) {}
    };
    pump(&mut t.m, &mut there);
    there.send(PeerMsg::Hello(hello(other)));
    pump(&mut t.m, &mut there);
    there.send(PeerMsg::Volatile(Volatile::RegionUp(f)));
    pump(&mut t.m, &mut there);
    assert!(t.m.foreign_running().contains(&f), "f runs on the other host: its halo is live");
    for _ in t.spawn_inputs.try_iter() {}

    t.m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&t.spawn_inputs), Vec::<RegionCoords>::new(), "a live halo is not re-seeded");
    assert_eq!(t.m.stored_regions().get(&f), Some(&1), "its version is noted");

    here.lock().unwrap().set_up(false);
    pump(&mut t.m, &mut there);
    assert!(!t.m.foreign_running().contains(&f), "a lost link: f's halo is no longer live");
    t
}

/// Task 6's carry: a FOREIGN neighbour that parks while the peer link is down (its final
/// halo and its `RegionParked` are lost with the link). While it ran, its stored version
/// rose (a cut while live: nothing to do then). Once the link is seen down its halo is no
/// longer live: the next `Resync` re-seeds it from the store even though the version is
/// one already seen, and the cut that saves its park re-seeds it again.
#[test]
fn stored_terrain_change_reseeds_a_foreign_halo_whose_link_dropped() {
    let TwoHostManager { mut m, f, spawn_inputs, .. } = two_host_manager_with_a_dropped_link();
    m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&spawn_inputs), vec![f], "the Resync re-seeds it from the store");
    m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&spawn_inputs), Vec::<RegionCoords>::new(), "once");
    // The cut that saves its park.
    m.stored_terrain_changed([(f, 2)].into());
    assert_eq!(store_seeds(&spawn_inputs), vec![f], "the notice of its saved park re-seeds it");
}

/// A stale mark is dropped when its region is listed while this host owns it (it became
/// this host's): owned by the other host again later, an unchanged version re-seeds
/// nothing.
#[test]
fn stored_terrain_change_drops_the_stale_mark_of_a_region_now_owned_here() {
    let TwoHostManager { mut m, mine, f, spawn_inputs, .. } = two_host_manager_with_a_dropped_link();
    m.set_ownership(game::Ownership::AllInOne, 1);
    m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&spawn_inputs), Vec::<RegionCoords>::new(), "this host's own region: nothing");
    m.set_ownership(game::Ownership::Static { ordinal: mine, hosts: 2 }, 1);
    m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&spawn_inputs), Vec::<RegionCoords>::new(), "the stale mark went with the ownership");
}

/// A region that has ticked asked to re-seed a neighbour the store cannot serve keeps its
/// mirror of it (worldgen is never older-than-the-store safe); one the store serves is
/// re-seeded from it, the saved dig included.
#[test]
fn stored_terrain_change_on_a_ticking_region_keeps_an_unserved_mirror() {
    crate::common::install_real_block_props();
    let (out, _out_recv) = crossbeam::channel::unbounded();
    let region = common::region_of(SPAWN_REGION, common::uniform_seed());
    let mut runner = game::RegionRunner::new(SPAWN_REGION, region, out);
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let count = generated.clone();
    runner.set_stud_halo_generator(std::sync::Arc::new(move |_rc, xs, zs| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut seed = common::uniform_seed();
        seed.ground.retain(|(c, _)| xs.contains(&c.x) && zs.contains(&c.z));
        seed
    }));
    runner.handle_input(game::RegionInput::SeedHaloFromWorldgen(vec![east(), north()]));
    assert_eq!(generated.load(std::sync::atomic::Ordering::SeqCst), 2);
    let (dug, snap) = east_saved_dug();
    runner.set_stored_terrain(std::sync::Arc::new(OneStored(east(), snap)));
    runner.tick();
    runner.handle_input(game::RegionInput::SeedHaloFromStore(vec![east(), north()]));
    assert_eq!(generated.load(std::sync::atomic::Ordering::SeqCst), 2, "no worldgen for the unserved neighbour");
    let data = runner.region_mut().data();
    let seen = |of: RegionCoords, cell: [i32; 3]| data.stud_material_seen(from_spawn(of, cell));
    assert_eq!(seen(east(), dug.0), None, "the saved dig is in the halo");
    assert!(seen(north(), [20, FLOOR_PLATES - 1, REGION_STUDS - 4]).is_some(), "north's mirror is kept");
}
