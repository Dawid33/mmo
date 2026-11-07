//! The portable region format (`game::portable`, world store R0+R1 Task 3):
//! what a region keeps in the store — its meta maps, the chunks changed
//! since the last save, the ground recipes once, and its owned non-player,
//! non-person entities as bundles — and the region rebuilt from it. A
//! restored region is not expected to hash-match its pre-save self (its
//! physics handles are rebuilt), so equivalence is the portable snapshot
//! compared field by field.
use crate::common;

use common::water::{floor_with_water, WaterBox};
use game::portable::{classify, ChunkTerms, PortableRegion, RegionMeta};
use game::rapier3d::math::Vector;
use game::{
    chest_block, restore_portable, state_hash, Block, BlockId, ChunkCoords, ColliderSpec, Container, DesignationKind,
    EntityBundle, EntityKey, GameEventKind, Intent, ItemId, ItemStack, OpId, Order, Pose, Region, RegionCoords,
    Rollback, SentIntent, SettlePolicy, StoredRegion, Target, TileBox, WorldBlockPos, REGION_BLOCKS,
};

/// Block units per block.
const B: f32 = 16.0;

fn rc() -> RegionCoords {
    common::r0()
}

/// A one-block floor (block y 0) over the whole region with ground recipes,
/// and a pond of recipe water (block levels 1..=3) over world blocks
/// 40..48 × 40..48 — one labelled water body.
fn pond_region() -> Region {
    common::install_real_block_props();
    let n = REGION_BLOCKS as i64;
    let x0 = rc().x as i64 * n;
    let z0 = rc().z as i64 * n;
    let pond = WaterBox { xs: x0 + 40..x0 + 48, zs: z0 + 40..z0 + 48, surface: 0, water_top: 3 };
    Region::from_columns(rc(), floor_with_water(rc(), &[pond]))
}

fn wc(p: [i32; 3]) -> WorldBlockPos {
    WorldBlockPos::from_local(rc(), game::BlockPos::new(p[0], p[1], p[2]))
}

fn cube() -> ColliderSpec {
    ColliderSpec::Cuboid { half_extents: Vector::splat(B / 2.0) }
}

fn spawn_body(region: &mut Region, at: [f32; 3]) -> EntityKey {
    let mut e = None;
    region.with_data(|r| {
        e = Some(r.spawn_body_safe(
            Pose::translation(at[0], at[1], at[2]),
            cube(),
            Vector::new(1.0, 0.0, 0.0),
            Vector::new(0.0, 0.5, 0.0),
        ))
    });
    e.unwrap()
}

/// Ticks `n` times, each committed like the server's.
fn ticks(region: &mut Region, n: usize) {
    for _ in 0..n {
        region.handle_event(GameEventKind::Tick).unwrap();
        region.commit_server_event();
    }
}

fn snapshot(region: &Region, saved: &ChunkTerms) -> (PortableRegion, ChunkTerms) {
    region.data().portable_snapshot(saved)
}

/// The region the snapshot test checks: a dug floor block, a chest with
/// items, a pile, a designation, two slimes, a dynamic body, a voxel body,
/// a queued intent and the pond's water body.
fn furnished_region() -> Region {
    let mut region = pond_region();
    let chest = chest_block().expect("the manifest has a chest");
    region.with_data(|r| {
        assert!(r.set_block_safe([10, 0, 10], Block::new(BlockId::AIR)), "dig a floor block");
        assert!(r.set_block_safe([20, 1, 20], Block::new(chest)), "place a chest");
        let mut slots = vec![None; game::CHEST_SLOTS];
        slots[0] = Some(ItemStack { item: ItemId(1), count: 5 });
        slots[3] = Some(ItemStack { item: ItemId(2), count: 7 });
        r.write_container_safe(wc([20, 1, 20]), Some(Container { slots }));
        let tick = *r.tick;
        assert_eq!(r.deposit_items_safe([30, 1, 30], ItemStack { item: ItemId(1), count: 3 }, tick, 0), 0);
        let op = OpId { client: 0, seq: 1 };
        let area = TileBox { min: [12, 0, 12], max: [13, 0, 12] };
        assert!(r.apply_order_safe(0, Order::Designate { op, kind: DesignationKind::Dig, priority: 4, area }));
    });
    common::spawn_slime(&mut region, [200.0, 40.0, 200.0], 2, 0.0, 1);
    common::spawn_slime(&mut region, [400.0, 40.0, 300.0], 1, 1.0, 2);
    spawn_body(&mut region, [300.0, 80.0, 500.0]);
    region.with_data(|r| {
        r.detach_blocks_safe(&[[50, 0, 20], [51, 0, 20]], SettlePolicy::Despawn, Vector::ZERO).expect("a voxel body");
    });
    ticks(&mut region, 3);
    // An intent queued for the next tick: a break of a floor block, sent by
    // the first slime.
    let by = {
        let d = region.data();
        *d.id_index.keys().next().expect("an owned entity")
    };
    let due = region.current_tick() + 1;
    region.with_data(|r| {
        let sent = SentIntent {
            target: Target::Block(wc([5, 0, 5])),
            intent: Intent::BreakBlock { by, survival: false },
            from: rc(),
            due,
        };
        // Queued raw, as `restore_portable` puts back stored intents.
        r.data.intents.insert_raw((due, by, 0), sent);
    });
    region
}

/// The bundles with a position-based kinematic body's velocities zeroed:
/// rapier derives those from the body's motion each step, and a body built
/// from a bundle (an arrival, a restore) starts at rest — `BodyState`
/// applies velocities to dynamic bodies only.
fn derived_velocities_zeroed(mut bundles: Vec<EntityBundle>) -> Vec<EntityBundle> {
    for b in &mut bundles {
        if b.body.kind == game::BodyKind::KinematicPosition {
            b.body.linvel = Vector::ZERO;
            b.body.angvel = Vector::ZERO;
        }
    }
    bundles
}

fn stored(p: PortableRegion) -> StoredRegion {
    StoredRegion::from(p)
}

#[test]
fn bundle_of_matches_extract() {
    let mut region = pond_region();
    let slime = common::spawn_slime(&mut region, [200.0, 40.0, 200.0], 2, 0.0, 1);
    let body = spawn_body(&mut region, [300.0, 80.0, 500.0]);
    let mut voxel = None;
    region.with_data(|r| {
        voxel = r.detach_blocks_safe(&[[30, 0, 30], [31, 0, 30]], SettlePolicy::Despawn, Vector::ZERO);
    });
    let voxel = voxel.expect("a voxel body");
    ticks(&mut region, 2);
    for key in [slime, body, voxel] {
        let before = state_hash(region.data());
        let bundle: EntityBundle = region.data().bundle_of(key);
        assert_eq!(before, state_hash(region.data()), "bundle_of is read-only");
        let mut extracted = None;
        region.with_data(|r| extracted = Some(r.extract_entity_safe(key)));
        assert_eq!(Some(bundle), extracted, "bundle_of == extract_entity_safe for {key:?}");
    }
}

#[test]
fn snapshot_roundtrip_is_equivalent() {
    let region = furnished_region();
    let (original, terms) = snapshot(&region, &ChunkTerms::new());
    // The fixture has what the test is about.
    assert!(original.ground.is_some(), "a first snapshot carries the ground");
    assert_eq!(original.meta.water_bodies.len(), 1, "the pond is one body");
    assert_eq!(original.meta.containers.len(), 1);
    assert_eq!(original.meta.item_piles.len(), 1);
    assert_eq!(original.meta.designations.len(), 2);
    assert_eq!(original.meta.intents.len(), 1);
    assert_eq!(original.entities.len(), 4, "two slimes, a body and a voxel body");
    assert!(original.entities.iter().any(|b| b.carried.voxel_body.is_some()));
    assert!(original.players.is_empty());
    assert_eq!(original.chunks.len(), terms.len(), "empty terms: every chunk");

    let (restored_region, restored_terms) = restore_portable(rc(), stored(original.clone()));
    assert_eq!(restored_terms, terms, "the restored chunks have the saved terms");
    let (restored, _) = snapshot(&restored_region, &ChunkTerms::new());

    let PortableRegion { meta, chunks, ground, entities, players } = restored;
    let RegionMeta {
        tick: _,
        next_game_event_id,
        next_entity_serial,
        designations,
        constructed,
        item_piles,
        containers,
        pile_takes,
        intents,
        intent_replies,
        intent_inbox,
        water_bodies,
    } = meta;
    let o = &original.meta;
    assert_eq!(next_game_event_id, o.next_game_event_id);
    assert_eq!(next_entity_serial, o.next_entity_serial);
    assert_eq!(designations, o.designations);
    assert_eq!(constructed, o.constructed);
    assert_eq!(item_piles, o.item_piles);
    assert_eq!(containers, o.containers);
    assert_eq!(pile_takes, o.pile_takes);
    assert_eq!(intents, o.intents);
    assert_eq!(intent_replies, o.intent_replies);
    assert_eq!(intent_inbox, o.intent_inbox);
    assert_eq!(water_bodies, o.water_bodies);
    assert_eq!(chunks, original.chunks);
    assert_eq!(ground, original.ground);
    assert_eq!(derived_velocities_zeroed(entities), derived_velocities_zeroed(original.entities.clone()));
    assert_eq!(players, original.players);
    assert_eq!(
        restored_region.data().pile_expiry().len(),
        region.data().pile_expiry().len(),
        "pile_expiry rebuilt from item_piles"
    );

    let (again, _) = snapshot(&restored_region, &restored_terms);
    assert!(again.ground.is_none(), "the ground goes only into a first snapshot");
    assert!(again.chunks.is_empty(), "nothing changed since the restore");
}

#[test]
fn snapshot_sends_only_changed_chunks() {
    let mut region = pond_region();
    let (_, terms) = snapshot(&region, &ChunkTerms::new());
    region.with_data(|r| assert!(r.set_block_safe([3, 1, 3], Block::new(BlockId(1)))));
    // A block placed on the floor: one chunk changes, none appears.
    let (second, _) = snapshot(&region, &terms);
    let changed: Vec<ChunkCoords> = second.chunks.keys().copied().collect();
    assert_eq!(changed, vec![ChunkCoords::new(0, 0, 0)]);
    assert!(second.ground.is_none());
}

#[test]
fn players_and_people_are_not_entities() {
    let mut region = pond_region();
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.commit_server_event();
    region.with_data(|r| {
        r.spawn_person_safe(game::history::PersonId(4), (300.0, 300.0)).expect("a person");
    });
    let (p, _) = snapshot(&region, &ChunkTerms::new());
    assert_eq!(p.entities.len(), 0);
    assert_eq!(p.players.len(), 1);
    assert_eq!(p.players[0].0, 0);
}

fn chunk_map(entries: &[(ChunkCoords, u32)]) -> PortableRegion {
    let mut p = PortableRegion::default();
    for &(c, floor) in entries {
        p.chunks.insert(c, game::Chunk::flat_floor(floor));
    }
    p
}

#[test]
fn merge_keeps_uncommitted_chunks() {
    let a = ChunkCoords::new(0, 0, 0);
    let b = ChunkCoords::new(1, 0, 0);
    let mut older = chunk_map(&[(a, 1)]);
    older.merge(chunk_map(&[(b, 2)]));
    assert_eq!(older.chunks.keys().copied().collect::<Vec<_>>(), vec![a, b]);

    let mut older = chunk_map(&[(a, 1)]);
    let newer = chunk_map(&[(a, 2)]);
    let a2 = newer.chunks[&a].clone();
    older.merge(newer);
    assert_eq!(older.chunks.len(), 1);
    assert_eq!(older.chunks[&a], a2, "the newer chunk wins");

    // Ground: the first Some.
    let mut first = PortableRegion { ground: Some(Vec::new()), ..Default::default() };
    first.merge(PortableRegion::default());
    assert_eq!(first.ground, Some(Vec::new()));
}

#[test]
fn classify_compiles() {
    let rb = Rollback::new(None);
    classify(&rb);
}
