//! The read-only terrain halo (`game::halo`): neighbours' chunks within
//! `HALO_BLOCKS` of each edge, mirrored into every running region as fixed
//! colliders, kept fresh by stamped `HaloUpdate` transfers.
use game::rapier3d::math::Vector;
use game::{
    Block, BlockId, BlockPos, Chunk, ChunkCoords, GameEventKind, HaloDelta, HaloUpdate, Key, Pose, Region,
    RegionCoords, WorldBlockPos, WorldChunkPos, BLOCK_VOXELS, REGION_BLOCKS, REGION_CHUNKS, SPAWN_REGION,
};

use crate::common::rig::{Rig, CLIENT};
use crate::common::seam_translate::uniform_floor;

fn north() -> RegionCoords {
    RegionCoords::new(SPAWN_REGION.x, SPAWN_REGION.z - 1)
}

fn east() -> RegionCoords {
    RegionCoords::new(SPAWN_REGION.x + 1, SPAWN_REGION.z)
}

/// A rig over a one-block floor everywhere, with the client's 3x3 window
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

/// The block at world `p` as the spawn region sees it (own ∪ halo).
fn seen_from_spawn(rig: &mut Rig, p: WorldBlockPos) -> Option<Block> {
    let mut b = None;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| b = r.data().halo_block(p));
    b
}

/// Sets `block` at `local` of `rc` between ticks, as an event of the slot
/// that opens at its current tick would.
fn set_block(rig: &mut Rig, rc: RegionCoords, local: BlockPos, block: Block) {
    rig.manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| assert!(d.set_block_safe([local.x, local.y, local.z], block), "{rc:?} {local:?} changed"));
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn neighbour_edit_reaches_halo_next_tick() {
    let mut rig = joined();
    let n = REGION_BLOCKS as i32;
    // 2 m inside north, on its edge facing the spawn region: the floor block.
    let local = BlockPos::new(10, 0, n - 2);
    let world = WorldBlockPos::from_local(north(), local);
    assert!(
        seen_from_spawn(&mut rig, world).is_some_and(|b| b.id != BlockId::AIR),
        "the spawn region's halo holds north's floor"
    );
    let t = tick_of(&mut rig, north());
    assert_eq!(tick_of(&mut rig, SPAWN_REGION), t, "lockstep");
    set_block(&mut rig, north(), local, Block::new(BlockId::AIR));
    assert!(
        seen_from_spawn(&mut rig, world).is_some_and(|b| b.id != BlockId::AIR),
        "slot {t}: not yet"
    );
    rig.round();
    assert_eq!(tick_of(&mut rig, SPAWN_REGION), t + 1);
    assert_eq!(seen_from_spawn(&mut rig, world).map(|b| b.id), Some(BlockId::AIR), "slot {}: the break is in the halo", t + 1);
    // Outside own ∪ halo: unknown.
    let far = WorldBlockPos::from_local(north(), BlockPos::new(10, 0, n / 2));
    assert_eq!(seen_from_spawn(&mut rig, far), None);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walk_across_seam_never_dips() {
    let mut rig = joined();
    // Body control on (Fortress: E toggles the first-person body).
    rig.key(Key::KeyE, true);
    rig.key(Key::KeyE, false);
    for _ in 0..10 {
        rig.round();
    }
    // Three blocks short of the north seam (z = 0), mid-region in x.
    let bv = BLOCK_VOXELS as f32;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        r.with_data(|d| {
            let e = *d.player_entites.get(&CLIENT).unwrap();
            let t = d.physics.bodies.get(*d.ecs.rigidbody.get(e)).unwrap().translation();
            d.set_body_pose_safe(e, Pose::translation(REGION_BLOCKS as f32 / 2.0 * bv, t.y, 3.0 * bv));
        });
    });
    for _ in 0..5 {
        rig.round();
    }
    let floor_top = bv; // the floor is block y 0
    let lifts_before = game::halo::depenetration_lifts();
    rig.key(Key::KeyW, true);
    let mut min_feet = f32::INFINITY;
    let mut crossed_at = None;
    for round in 0..80 {
        rig.round();
        let owners = rig.owners();
        if let [rc] = owners[..] {
            rig.manager.spawner_mut().with_region(rc, |r| {
                let d = r.data();
                let e = *d.player_entites.get(&CLIENT).unwrap();
                let t = d.physics.bodies.get(*d.ecs.rigidbody.get(e)).unwrap().translation();
                min_feet = min_feet.min(game::feet_of(t).y);
            });
            if rc == north() && crossed_at.is_none() {
                crossed_at = Some(round);
            }
        }
        if crossed_at.is_some_and(|c| round > c + 10) {
            break;
        }
    }
    rig.key(Key::KeyW, false);
    assert!(crossed_at.is_some(), "the walk never reached north");
    let tol = 0.01 * bv;
    assert!(
        min_feet >= floor_top - tol,
        "feet dipped to {min_feet} units, floor at {floor_top} (tolerance {tol})"
    );
    assert_eq!(game::halo::depenetration_lifts() - lifts_before, 0, "a seam crossing needed a depenetration lift");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn raycast_hits_neighbour_block() {
    let mut rig = joined();
    let n = REGION_BLOCKS as i32;
    let z = 20;
    // A pillar 2 blocks inside east (its blocks x = 0 and 1 are the first two).
    for y in 1..4 {
        set_block(&mut rig, east(), BlockPos::new(1, y, z), Block::new(BlockId(1)));
    }
    rig.round();
    let bv = BLOCK_VOXELS as f32;
    // An eye 3 blocks inside the spawn region, looking +x along the row.
    let origin = Vector::new((n as f32 - 2.5) * bv, 2.5 * bv, (z as f32 + 0.5) * bv);
    let mut hit = None;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        hit = r.data().raycast_block(origin, Vector::X, game::edit::REACH);
    });
    let hit = hit.expect("the ray hits the pillar in the halo");
    assert_eq!(hit.block, [n + 1, 2, z], "east's block x = 1, in the spawn region's frame");
    assert_eq!(hit.normal, [-1, 0, 0]);
    // Tile queries read the halo too.
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        assert_eq!(game::TileSource::block(r.data(), [n + 1, 2, z]), BlockId(1));
        assert_eq!(game::TileSource::block(r.data(), [n + 1, 5, z]), BlockId::AIR);
    });
    // Read-only: the hit cannot be broken from here.
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        r.with_data(|d| assert!(!d.set_block_safe(hit.block, Block::new(BlockId::AIR)), "halo blocks are read-only"));
    });
}

/// A halo seed and a later patch of one chunk roll back bit-exact (the
/// rollback bar: `hash(before) == hash(after undo)`), colliders included.
#[test]
fn halo_updates_roll_back_bit_exact() {
    let n = REGION_CHUNKS as i32;
    let floor = |cx: usize, cz: usize| (ChunkCoords::new(cx, 0, cz), Chunk::flat_floor(BLOCK_VOXELS as u32));
    let own: Vec<_> = (0..REGION_CHUNKS).flat_map(|x| (0..REGION_CHUNKS).map(move |z| floor(x, z))).collect();
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(game::Rollback::new(Some(send.clone())), Some(send), SPAWN_REGION, None);
    region.with_data(|r| r.seed_terrain_genesis(own));
    // East's west column, as east would seed it.
    let column = |data: Chunk| -> Vec<HaloDelta> {
        (0..n)
            .map(|z| HaloDelta { chunk: WorldChunkPos::from_local(SPAWN_REGION, [n, 0, z]), data: Some(data.clone()) })
            .collect()
    };
    let seed = HaloUpdate { from: east(), replace: true, deltas: column(Chunk::flat_floor(BLOCK_VOXELS as u32)) };
    let before = game::state_hash(region.data());
    region.handle_event(GameEventKind::HaloUpdate(seed.clone())).unwrap();
    assert!(region.data().halo_chunk_at([n, 0, 0]).is_some());
    let bodies = region.data().physics.bodies.len();
    region.rollback_last_event();
    assert_eq!(game::state_hash(region.data()), before, "seed undone exactly");
    // Seed for good, then a patch: one chunk loses a block.
    region.handle_event(GameEventKind::HaloUpdate(seed)).unwrap();
    region.forget_last_event();
    assert_eq!(region.data().physics.bodies.len(), bodies);
    let seeded = game::state_hash(region.data());
    let mut dug = Chunk::flat_floor(BLOCK_VOXELS as u32);
    dug.set_block(3, 0, 3, Block::new(BlockId::AIR));
    let patch = HaloUpdate {
        from: east(),
        replace: false,
        deltas: vec![HaloDelta { chunk: WorldChunkPos::from_local(SPAWN_REGION, [n, 0, 0]), data: Some(dug) }],
    };
    region.handle_event(GameEventKind::HaloUpdate(patch)).unwrap();
    assert_ne!(game::state_hash(region.data()), seeded);
    region.rollback_last_event();
    assert_eq!(game::state_hash(region.data()), seeded, "patch undone exactly");
}

/// A neighbour that parks hands its final terrain to the running regions
/// that mirror it: an edit it never ticked (so never sent as a delta) still
/// reaches their halo once its `Stopped` is handled.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn parked_neighbour_reseeds_running_halo() {
    let mut rig = joined();
    let n = REGION_BLOCKS as i32;
    let local = BlockPos::new(12, 0, n - 1);
    let world = WorldBlockPos::from_local(north(), local);
    rig.event(game::ServerEvent::ClientPacket(game::ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    // Edited between ticks, then parked before it ticks again.
    set_block(&mut rig, north(), local, Block::new(BlockId::AIR));
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    rig.settle();
    assert!(rig.manager.parked_regions().contains(&north()), "north parked");
    assert_eq!(seen_from_spawn(&mut rig, world).map(|b| b.id), Some(BlockId::AIR), "the parked edit is in the halo");
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
    let bv = BLOCK_VOXELS as f32;
    let size = game::REGION_SIZE;
    // 1.5 blocks short of north's +z edge, mid-region in x, on the floor.
    let pos = [size / 2.0, 2.5 * bv, size - 1.5 * bv];
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
    let bv = BLOCK_VOXELS as f32;
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
    let n = REGION_BLOCKS as i32;
    let local = BlockPos::new(14, 0, n - 1);
    let world = WorldBlockPos::from_local(north(), local);
    rig.event(game::ServerEvent::ClientPacket(game::ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    // The spawn region falls behind for a while.
    rig.manager.spawner_mut().hold(SPAWN_REGION, 6);
    // North breaks the block; its next tick sends the delta, which the
    // lagging spawn region holds for a later slot.
    set_block(&mut rig, north(), local, Block::new(BlockId::AIR));
    rig.now_ms += game::TICK_RATE;
    rig.manager.spawner_mut().tick_all();
    rig.settle();
    // Then puts it back and parks before ticking again: only the reseed
    // carries the final block.
    set_block(&mut rig, north(), local, Block::new(BlockId(1)));
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
    assert_eq!(seen_from_spawn(&mut rig, world).map(|b| b.id), Some(BlockId(1)), "the final terrain won");
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

