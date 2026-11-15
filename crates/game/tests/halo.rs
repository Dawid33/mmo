//! The read-only terrain halo (`game::halo`): neighbours' chunks within
//! `HALO_BLOCKS` of each edge, mirrored into every running region as fixed
//! colliders, kept fresh by stamped `HaloUpdate` transfers.
use game::rapier3d::math::Vector;
use game::{
    Block, BlockId, BlockPos, Chunk, ChunkCoords, GameEventKind, HaloDelta, HaloUpdate, Key, Pose, Region,
    RegionCoords, WorldBlockPos, WorldChunkPos, BLOCK_VOXELS, REGION_BLOCKS, REGION_CHUNKS, SPAWN_REGION,
};

use crate::common::{self, STAND};
use crate::common::rig::{Rig, CLIENT};
use crate::common::seam_translate::uniform_floor;
use crate::common::water::{floor_with_water, WaterBox};

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
    assert!(
        seen_from_spawn(&mut rig, world).is_some_and(|b| b.id != BlockId::AIR),
        "slot {}: the delta is buffered to its end",
        t + 1
    );
    rig.round();
    assert_eq!(seen_from_spawn(&mut rig, world).map(|b| b.id), Some(BlockId::AIR), "tick {}: the break is in the halo", t + 2);
    // The one-block floor has nothing under the broken block: the floor beside it stands.
    let beside = WorldBlockPos::from_local(north(), BlockPos::new(local.x + 1, local.y, local.z));
    assert!(seen_from_spawn(&mut rig, beside).is_some_and(|b| b.id != BlockId::AIR), "the block beside it still stands");
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
    rig.round(); // the T+1 delta is buffered to its slot's end
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
    crate::common::install_real_block_props();
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
    let seed = HaloUpdate {
        from: east(),
        replace: true,
        deltas: column(Chunk::flat_floor(BLOCK_VOXELS as u32)),
        ground: Vec::new(),
        // A pile on the neighbour's edge cell: its mirror rolls back too.
        piles: vec![(WorldBlockPos::from_local(SPAWN_REGION, game::BlockPos::new(REGION_BLOCKS as i32, 1, 3)), Some(pile(3)))],
        // And a chest beside it (Task 6.4).
        containers: vec![(chest_cell(), Some(game::Container::chest()))],
    };
    let before = game::state_hash(region.data());
    region.handle_event(GameEventKind::HaloUpdate(seed.clone())).unwrap();
    assert!(region.data().halo_chunk_at([n, 0, 0]).is_some());
    assert_eq!(region.data().halo_piles().len(), 1, "the seed's pile is mirrored");
    let pile_cell = WorldBlockPos::from_local(SPAWN_REGION, game::BlockPos::new(REGION_BLOCKS as i32, 1, 3));
    {
        use game::WorldView as _;
        assert_eq!(region.data().view().pile(pile_cell), Some(&pile(3)), "WorldView reads the mirror");
        assert_eq!(region.data().view().container(chest_cell()), Some(&game::Container::chest()), "and the chest's");
    }
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
        ground: Vec::new(),
        piles: vec![(WorldBlockPos::from_local(SPAWN_REGION, game::BlockPos::new(REGION_BLOCKS as i32, 1, 3)), Some(pile(5)))],
        containers: vec![(chest_cell(), Some(stocked_chest()))],
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
    assert!(
        seen_from_spawn(&mut rig, world).is_some_and(|b| b.id != BlockId::AIR),
        "the reseed is buffered to its slot's end"
    );
    rig.round();
    assert_eq!(seen_from_spawn(&mut rig, world).map(|b| b.id), Some(BlockId::AIR), "the parked edit is in the halo");
    let beside = WorldBlockPos::from_local(north(), BlockPos::new(local.x + 1, local.y, local.z));
    assert!(seen_from_spawn(&mut rig, beside).is_some_and(|b| b.id != BlockId::AIR), "beside it the ground stands");
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
    // No round since: the spawn region is in the slot north's last stamp opens;
    // the end is buffered to that slot's end.
    assert_eq!(tick_of(&mut rig, SPAWN_REGION), last, "lockstep, nothing ticked since the park");
    assert!(mirror_in(&mut rig, SPAWN_REGION, id).is_some(), "buffered to the end of the slot");
    // One tick (not `round`: its conservation check does not count parked states).
    rig.now_ms += game::TICK_RATE;
    rig.manager.spawner_mut().tick_all();
    rig.settle();
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
    rig.round(); // the spawn region's slot closes: the drop is applied before its Tick
    assert_eq!(mirror_in(&mut rig, SPAWN_REGION, id), None, "the lost owner's mirror is gone");
}

/// The pile and chest mirrors the spawn region holds of `of`, in `of`'s frame.
fn mirrors_from_spawn(rig: &mut Rig, of: RegionCoords) -> (Vec<game::BlockPos>, Vec<game::BlockPos>) {
    let mut out = (Vec::new(), Vec::new());
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let d = r.data();
        out.0 = d.halo_piles().keys().filter(|p| p.to_local().0 == of).map(|p| p.to_local().1).collect();
        out.1 = d.halo_containers().keys().filter(|p| p.to_local().0 == of).map(|p| p.to_local().1).collect();
    });
    out
}

/// A region lost and respawned at once (the next message for it found its
/// thread dead) re-seeds its running neighbours: the new incarnation's seed
/// lands after the end of the lost one's mirrors (`HaloSourceGone`, stamped
/// with the world tick), so the neighbour mirrors the pile and chest the new
/// incarnation holds.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn respawned_region_reseeds_after_its_loss() {
    let mut rig = joined();
    rig.kill(north());
    // The client's repeated subscribe: its snapshot request finds north dead.
    let ev = game::ServerEvent::ClientPacket(game::ClientPacket::RequestRegionConnection(north()), CLIENT);
    assert!(rig.manager.handle_server_event(ev, rig.now_ms));
    assert!(rig.manager.running_regions().contains(&north()), "north respawned");
    // Before the new incarnation reads its first input: a pile and a chest on
    // its edge facing the spawn region, as a host's respawn restores them from
    // its store (this rig's respawn regenerates) — no delta of them pending.
    let world = game::RegionSpawner::world_tick(rig.manager.spawner_mut());
    let n = REGION_BLOCKS as i32;
    rig.manager.spawner_mut().with_region(north(), |r| {
        let stack = game::ItemStack { item: game::ItemId(1), count: 3 };
        r.with_data(|d| {
            assert_eq!(d.deposit_items_safe([5, STAND, n - 1], stack, world, game::PICKUP_DELAY_TICKS), 0);
            let chest = WorldBlockPos::from_local(north(), game::BlockPos::new(9, STAND, n - 1));
            d.write_container_safe(chest, Some(stocked_chest()));
        });
        r.take_halo_updates();
    });
    rig.settle();
    for _ in 0..3 {
        rig.round();
    }
    let (piles, chests) = mirrors_from_spawn(&mut rig, north());
    assert_eq!(piles, vec![game::BlockPos::new(5, STAND, n - 1)], "the respawned north's pile is mirrored");
    assert_eq!(chests, vec![game::BlockPos::new(9, STAND, n - 1)], "and its chest");
}


/// The chest cell [`halo_updates_roll_back_bit_exact`] mirrors: east's edge
/// column, next to its pile.
fn chest_cell() -> WorldBlockPos {
    WorldBlockPos::from_local(SPAWN_REGION, game::BlockPos::new(REGION_BLOCKS as i32 + 1, 1, 4))
}

/// A chest holding one stack.
fn stocked_chest() -> game::Container {
    let mut c = game::Container::chest();
    c.slots[2] = Some(game::ItemStack { item: game::ItemId(1), count: 7 });
    c
}

fn east_water_floor(rc: RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> {
    if rc != east() {
        return uniform_floor(rc);
    }
    let air = Block::new(BlockId::AIR);
    let water = Block::new(BlockId(5));
    let mut out = Vec::new();
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            let sub = game::SubChunk::Dense(Box::new(Chunk::uniform(water)));
            out.push((game::ColumnCoords::new(x, z), game::Column::new(0, air, air, vec![sub])));
        }
    }
    out
}

#[test]
fn halo_water_chunk_has_no_collider() {
    let mut rig = Rig::with_floor(None, east_water_floor);
    rig.join_spawn();
    rig.round();
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let data = r.data();
        let mut water_chunks = 0;
        for halo in data.halo.values() {
            let Some(chunk) = halo.chunk.as_ref() else { continue };
            if chunk.blocks.get(game::BlockIndex::from_xyz(0, 0, 0)).id != BlockId(5) {
                continue;
            }
            water_chunks += 1;
            let body = data.physics.bodies.get(halo.body).expect("halo body");
            assert!(body.colliders().is_empty(), "an all-water halo chunk carries no collider");
        }
        assert!(water_chunks > 0, "the east neighbour's water chunks are mirrored in the halo band");
    });
}

/// Recipe water 3 blocks deep (block y 1..=3 over the one-block floor)
/// over world block columns `x` −3..3 (straddling the spawn region's east
/// seam), `z` 6..10.
fn seam_pond(rc: RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> {
    let n = REGION_BLOCKS as i64;
    let seam = east().x as i64 * n;
    let pond = WaterBox { xs: seam - 3..seam + 3, zs: SPAWN_REGION.z as i64 * n + 6..SPAWN_REGION.z as i64 * n + 10, surface: 0, water_top: 3 };
    floor_with_water(rc, &[pond])
}

/// Water units at world block `p` as region `rc` sees it.
fn units_from(rig: &mut Rig, rc: RegionCoords, p: WorldBlockPos) -> Option<u16> {
    let mut u = None;
    rig.manager.spawner_mut().with_region(rc, |r| u = Some(r.data().water_units_at(p)));
    u
}

#[test]
fn water_units_at_reads_the_halo() {
    common::install_real_block_props();
    let mut rig = Rig::with_floor(None, seam_pond);
    rig.join_spawn();
    rig.round();
    // 2 blocks inside the east neighbour, in its recipe water.
    let at = WorldBlockPos::from_local(SPAWN_REGION, BlockPos::new(REGION_BLOCKS as i32 + 1, 3, 8));
    assert_eq!(units_from(&mut rig, SPAWN_REGION, at), Some(game::WATER_FULL));
}

/// A pond whose recipe straddles the east seam reads full on both sides of
/// it from either region (own cells and halo cells alike); a block placed
/// into a water cell by its owner reads dry in the neighbour's halo after
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
            for y in 1..=3 {
                let p = WorldBlockPos::new(x, y, z);
                assert_eq!(units_from(&mut rig, rc, p), Some(game::WATER_FULL), "{rc:?} reads {p:?}");
            }
            assert_eq!(units_from(&mut rig, rc, WorldBlockPos::new(x, 0, z)), Some(0), "{rc:?}: the floor");
            assert_eq!(units_from(&mut rig, rc, WorldBlockPos::new(x, 4, z)), Some(0), "{rc:?}: above the pond");
        }
        assert_eq!(units_from(&mut rig, rc, WorldBlockPos::new(seam + 3, 2, z)), Some(0), "{rc:?}: past the pond");
    }
    // Bodies are per region: each side labels its own half of the pond, the
    // halo's copy of the neighbour's recipe carries none.
    for rc in [SPAWN_REGION, east()] {
        rig.manager.spawner_mut().with_region(rc, |r| {
            let d = r.data();
            assert_eq!(d.water_bodies().len(), 1, "{rc:?}: its half of the pond");
            assert!(d.halo_grounds().values().all(|g| g.water_body.iter().all(|&b| b == game::NO_BODY)), "{rc:?}");
            let own = WorldBlockPos::new(if rc == east() { seam } else { seam - 1 }, 2, z).to_local().1;
            assert!(d.water_body_at(own).is_some(), "{rc:?}: own water cell");
        });
    }
    // East places stone into its own water cell 1 block past the seam.
    let cell = WorldBlockPos::new(seam + 1, 2, z);
    let (_, local) = cell.to_local();
    set_block(&mut rig, east(), local, Block::new(BlockId(2)));
    assert_eq!(units_from(&mut rig, east(), cell), Some(0), "the owner reads its block at once");
    assert_eq!(units_from(&mut rig, SPAWN_REGION, cell), Some(game::WATER_FULL), "the halo: not yet");
    rig.round();
    assert_eq!(units_from(&mut rig, SPAWN_REGION, cell), Some(game::WATER_FULL), "the T+1 delta: buffered to its slot's end");
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

/// East as an earlier process saved it: the floor, with a block on its west
/// edge (facing the spawn region) dug out.
fn east_saved_dug() -> (BlockPos, game::TerrainSnapshot) {
    let dug = BlockPos::new(1, 0, 7);
    let mut snap = game::TerrainSnapshot::from_columns(uniform_floor(east()));
    let cb = game::CHUNK_BLOCKS as i32;
    let at = ChunkCoords::new((dug.x / cb) as usize, 0, (dug.z / cb) as usize);
    let (_, chunk) = snap.chunks.iter_mut().find(|(c, _)| *c == at).expect("the floor chunk");
    chunk.set_block((dug.x % cb) as usize, 0, (dug.z % cb) as usize, Block::new(BlockId::AIR));
    (dug, snap)
}

/// A neighbour the store holds that has not run in this process seeds the
/// halo from its saved terrain (the old known limit: pristine worldgen); one
/// the store cannot serve falls back to worldgen.
#[test]
fn stored_neighbour_seeds_halo_from_store() {
    crate::common::install_real_block_props();
    let (out, _out_recv) = crossbeam::channel::unbounded();
    let region = Region::from_columns(SPAWN_REGION, uniform_floor(SPAWN_REGION));
    let mut runner = game::RegionRunner::new(SPAWN_REGION, region, out);
    runner.set_halo_generator(std::sync::Arc::new(|rc, xs, zs| {
        uniform_floor(rc).into_iter().filter(|(c, _)| xs.contains(&c.x) && zs.contains(&c.z)).collect()
    }));
    let (dug, snap) = east_saved_dug();
    runner.set_stored_terrain(std::sync::Arc::new(OneStored(east(), snap)));
    // North is listed but the store has nothing for it: worldgen.
    runner.handle_input(game::RegionInput::SeedHaloFromStore(vec![east(), north()]));

    let data = runner.region_mut().data();
    let east_block = |p: BlockPos| data.halo_block(WorldBlockPos::from_local(east(), p)).map(|b| b.id);
    assert_eq!(east_block(dug), Some(BlockId::AIR), "the saved dig is in the halo");
    let beside = BlockPos::new(dug.x, 0, dug.z + 1);
    assert!(east_block(beside).is_some_and(|b| b != BlockId::AIR), "the rest of the saved floor too");
    let n = REGION_BLOCKS as i32;
    let north_floor = data.halo_block(WorldBlockPos::from_local(north(), BlockPos::new(10, 0, n - 2)));
    assert!(north_floor.is_some_and(|b| b.id != BlockId::AIR), "an unserved neighbour comes from worldgen");
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
        fn generates_columns(&self) -> bool {
            true
        }
    }
    let spawned = Arc::new(Mutex::new(Vec::new()));
    let (out_send, _packets) = crossbeam::channel::unbounded();
    let (region_out_send, _region_out) = crossbeam::channel::unbounded();
    let (lod_result_send, lod_result_recv) = crossbeam::channel::unbounded();
    let lod_builder: Box<dyn game::LodBuilder> = Box::new(game::InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        water: Box::new(game::lod::LodWater::dry),
        out: lod_result_send,
    });
    let mut m = game::WorldManager::new(
        Recording(spawned.clone()),
        Box::new(|_rc: RegionCoords| Vec::new()),
        out_send,
        region_out_send,
        lod_builder,
        lod_result_recv,
    );
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
    fn generates_columns(&self) -> bool {
        true
    }
}

/// A manager owning the spawn region (one of two hosts, every region its own
/// super-region) with its spawn region running, a neighbour `f` of it the other host owns
/// and a neighbour `l` it owns itself (never run), and the spawn region's input channel.
struct TwoHostManager {
    m: game::WorldManager<RecordingSpawner>,
    mine: game::HostId,
    f: RegionCoords,
    l: RegionCoords,
    spawn_inputs: crossbeam::channel::Receiver<game::RegionInput>,
}

fn two_host_manager() -> TwoHostManager {
    let s = 1;
    let mine = game::rendezvous_owner(game::super_region_of(SPAWN_REGION, s), 2);
    let ownership = game::Static { id: mine, hosts: 2 };
    let f = game::halo::neighbours(SPAWN_REGION).find(|rc| !ownership.owns(*rc, s)).expect("a neighbour of the other host");
    let l = game::halo::neighbours(SPAWN_REGION).find(|rc| ownership.owns(*rc, s)).expect("a neighbour of this host");
    let spawned = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (out_send, _packets) = crossbeam::channel::unbounded();
    let (region_out_send, _region_out) = crossbeam::channel::unbounded();
    // The spawner generates on its region threads: the manager's generator is never asked.
    let mut m = common::floor_manager(RecordingSpawner(spawned.clone()), |_| Vec::new(), out_send, region_out_send);
    m.set_ownership(Box::new(ownership), s);
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
    let other = game::HostId(1 - mine.0);
    let hello = |ordinal| PeerHello {
        id: ordinal,
        boot: 7 + ordinal.0,
        generation: 1,
        endpoint: HostEndpoint { id: ordinal, quic: None, webtransport: None, cert_hash: None },
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
    m.set_ownership(Box::new(game::AllInOne), 1);
    m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&spawn_inputs), Vec::<RegionCoords>::new(), "this host's own region: nothing");
    m.set_ownership(Box::new(game::Static { id: mine, hosts: 2 }), 1);
    m.stored_terrain_changed([(f, 1)].into());
    assert_eq!(store_seeds(&spawn_inputs), Vec::<RegionCoords>::new(), "the stale mark went with the ownership");
}

/// Another host saved one of its regions (not running here) at a newer version: the LOD
/// built here from older terrain is stale. Its cached LOD entries are dropped and the keys
/// a client is subscribed to are rebuilt (`invalidate_region_lod`, as `cut_committed`
/// does for this host's own saves).
#[test]
fn stored_terrain_change_of_a_foreign_region_rebuilds_its_lod() {
    use game::lod::LodKey;
    use game::{ClientPacket, ServerEvent, ServerPacket};
    let s = 1;
    let mine = game::rendezvous_owner(game::super_region_of(SPAWN_REGION, s), 2);
    let ownership = game::Static { id: mine, hosts: 2 };
    let f = (3..64).map(|x| RegionCoords::new(x, 3)).find(|rc| !ownership.owns(*rc, s)).expect("a far region of the other host");
    let (out_send, out) = crossbeam::channel::unbounded();
    let (region_out_send, region_out) = crossbeam::channel::unbounded();
    let mut m = common::floor_manager(game::InlineSpawner::default(), uniform_floor, out_send, region_out_send);
    m.set_ownership(Box::new(ownership), s);
    m.set_host_generation(1);
    let settle = |m: &mut game::WorldManager<game::InlineSpawner>| {
        for _ in 0..3 {
            m.spawner_mut().pump();
            while let Ok((rc, o)) = region_out.try_recv() {
                m.handle_region_output(rc, o, 0);
            }
            m.pump_lod_results();
        }
    };
    m.handle_server_event(ServerEvent::connected(1), 0);
    settle(&mut m);
    assert!(m.is_foreign(f) && !m.running_regions().contains(&f));
    let (subscribed, released) = (LodKey::for_region(2, f), LodKey::for_region(4, f));
    for key in [subscribed, released] {
        m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    }
    settle(&mut m);
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::ReleaseLod(released), 1), 0);
    let lod_data = |out: &crossbeam::channel::Receiver<(Option<game::ClientId>, ServerPacket)>| -> Vec<LodKey> {
        out.try_iter()
            .filter_map(|(_, p)| match p {
                ServerPacket::LodData(k, ..) => Some(k),
                _ => None,
            })
            .collect()
    };
    assert_eq!(lod_data(&out), vec![subscribed, released], "both keys built");
    let cached = m.lod_cache_len();

    m.stored_terrain_changed([(f, 1)].into());
    settle(&mut m);
    assert_eq!(lod_data(&out), vec![subscribed], "the subscribed key is rebuilt and pushed again");
    assert_eq!(m.lod_cache_len(), cached - 1, "the released key's entry is dropped");
    m.stored_terrain_changed([(f, 1)].into());
    settle(&mut m);
    assert_eq!(lod_data(&out), Vec::<LodKey>::new(), "an unchanged version rebuilds nothing");
}

/// A region that has ticked asked to re-seed a neighbour the store cannot serve keeps its
/// mirror of it (worldgen is never older-than-the-store safe); one the store serves is
/// re-seeded from it, the saved dig included.
#[test]
fn stored_terrain_change_on_a_ticking_region_keeps_an_unserved_mirror() {
    crate::common::install_real_block_props();
    let (out, _out_recv) = crossbeam::channel::unbounded();
    let region = Region::from_columns(SPAWN_REGION, uniform_floor(SPAWN_REGION));
    let mut runner = game::RegionRunner::new(SPAWN_REGION, region, out);
    // Every region the halo generator was asked for, once per facing window.
    let generated = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let asked = generated.clone();
    runner.set_halo_generator(std::sync::Arc::new(move |rc, xs, zs| {
        asked.lock().unwrap().push(rc);
        uniform_floor(rc).into_iter().filter(|(c, _)| xs.contains(&c.x) && zs.contains(&c.z)).collect()
    }));
    runner.handle_input(game::RegionInput::SeedHaloFromWorldgen(vec![east(), north()]));
    let seeded = generated.lock().unwrap().clone();
    let mut regions = seeded.clone();
    regions.dedup();
    assert_eq!(regions, vec![east(), north()]);
    let (dug, snap) = east_saved_dug();
    runner.set_stored_terrain(std::sync::Arc::new(OneStored(east(), snap)));
    runner.tick();
    runner.handle_input(game::RegionInput::SeedHaloFromStore(vec![east(), north()]));
    assert_eq!(*generated.lock().unwrap(), seeded, "no worldgen for the unserved neighbour");
    let data = runner.region_mut().data();
    let seen = |of: RegionCoords, p: BlockPos| data.halo_block(WorldBlockPos::from_local(of, p)).map(|b| b.id);
    assert_eq!(seen(east(), dug), Some(BlockId::AIR), "the saved dig is in the halo");
    let n = REGION_BLOCKS as i32;
    assert!(seen(north(), BlockPos::new(10, 0, n - 2)).is_some_and(|b| b != BlockId::AIR), "north's mirror is kept");
}
