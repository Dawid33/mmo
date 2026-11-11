//! The portable region format (`game::portable`, world store R0+R1 Task 3):
//! what a region keeps in the store — its meta maps, the stud chunks changed
//! since the last save, the ground recipes once, and its owned non-player,
//! non-person entities as bundles — and the region rebuilt from it. A
//! restored region is not expected to hash-match its pre-save self (its
//! physics handles are rebuilt), so equivalence is the portable snapshot
//! compared field by field.
use crate::common;

use common::water::{floor_with_water, WaterBox};
use game::portable::{classify, PortableRegion, RegionMeta, SaveTerms};
use game::rapier3d::math::Vector;
use game::{
    restore_portable, state_hash, ColliderSpec, Container, DesignationKind, EntityBundle, EntityKey, GameEventKind,
    Intent, ItemId, ItemStack, OpId, Order, Pose, Region, RegionCoords, Rollback, SentIntent, SettlePolicy,
    StoredRegion, StudChunkRecord, Target, TileBox, WorldBlockPos, REGION_BLOCKS,
};

/// One metre (a tile's width), sim units.
const B: f32 = common::TILE_W;
/// The level of the ground's top (solid) tile; things rest in `G + 1`.
const G: i32 = common::STAND - 1;

fn rc() -> RegionCoords {
    common::r0()
}

/// Flat stud ground over the whole region, and a pond of recipe water (tile
/// levels `G + 1 ..= G + 3`) over world tiles 40..48 × 40..48.
fn pond_region() -> Region {
    common::install_real_block_props();
    let n = REGION_BLOCKS as i64;
    let x0 = rc().x as i64 * n;
    let z0 = rc().z as i64 * n;
    let pond = WaterBox { xs: x0 + 40..x0 + 48, zs: z0 + 40..z0 + 48, surface: G, water_top: G + 3 };
    common::region_of(rc(), floor_with_water(rc(), &[pond]))
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

fn snapshot(region: &Region, saved: &SaveTerms) -> (PortableRegion, SaveTerms) {
    region.data().portable_snapshot(saved)
}

/// A 2 x 4 brick placed in the air over chunk column (3, 1) (min cell
/// `BRICK`) and detached into a falling stud body.
fn detach_a_brick(region: &mut Region) -> EntityKey {
    use game::stud_catalog::STUD_PARTS;
    const BRICK: [u8; 3] = [4, 60, 4];
    let part = STUD_PARTS.iter().position(|p| p.number == "3001").expect("the 2 x 4 brick") as u16;
    let at = common::sc(3, 0, 1);
    let mut body = None;
    region.with_data(|r| {
        assert!(r.place_stud_safe(at, game::StudBrick::part_at(BRICK, part, 0, 4)), "a brick in the air");
        let picks = std::collections::BTreeMap::from([(at, [BRICK].into())]);
        body = r.detach_studs_safe(&picks, SettlePolicy::Despawn, Vector::ZERO, None);
    });
    body.expect("a stud body")
}

/// A body-centre height a few metres over the floor: where the fixture's
/// slimes and body start falling from.
fn over_floor(metres: f32) -> f32 {
    common::floor_y(common::FLOOR_PLATES) + metres * B
}

/// The region the snapshot test checks: a dug floor tile, a chest with
/// items, a pile, a designation, two slimes, a dynamic body, a stud body,
/// a queued intent and the pond's recipe water.
fn furnished_region() -> Region {
    let mut region = pond_region();
    region.with_data(|r| {
        assert!(common::carve_tile(r, [10, G, 10]), "dig a floor tile");
        assert!(r.place_stud_chest_safe([20, G + 1, 20]), "place a chest");
        let mut slots = vec![None; game::CHEST_SLOTS];
        slots[0] = Some(ItemStack { item: ItemId(1), count: 5 });
        slots[3] = Some(ItemStack { item: ItemId(2), count: 7 });
        r.write_container_safe(wc([20, G + 1, 20]), Some(Container { slots }));
        let tick = *r.tick;
        assert_eq!(r.deposit_items_safe([30, G + 1, 30], ItemStack { item: ItemId(1), count: 3 }, tick, 0), 0);
        let op = OpId { client: 0, seq: 1 };
        let area = TileBox { min: [12, G, 12], max: [13, G, 12] };
        assert!(r.apply_order_safe(0, Order::Designate { op, kind: DesignationKind::Dig, priority: 4, area }));
    });
    common::spawn_slime(&mut region, [200.0, over_floor(1.5), 200.0], 2, 0.0, 1);
    common::spawn_slime(&mut region, [400.0, over_floor(1.5), 300.0], 1, 1.0, 2);
    spawn_body(&mut region, [300.0, over_floor(4.0), 500.0]);
    detach_a_brick(&mut region);
    ticks(&mut region, 3);
    // An intent queued for the next tick: a break of a floor tile, sent by
    // the first slime.
    let by = {
        let d = region.data();
        *d.id_index.keys().next().expect("an owned entity")
    };
    let due = region.current_tick() + 1;
    region.with_data(|r| {
        let sent = SentIntent {
            target: Target::Block(wc([5, G, 5])),
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
    let slime = common::spawn_slime(&mut region, [200.0, over_floor(1.5), 200.0], 2, 0.0, 1);
    let body = spawn_body(&mut region, [300.0, over_floor(4.0), 500.0]);
    let bricks = detach_a_brick(&mut region);
    ticks(&mut region, 2);
    for key in [slime, body, bricks] {
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
    let (original, terms) = snapshot(&region, &SaveTerms::default());
    // The fixture has what the test is about.
    assert!(original.stud_ground.as_ref().is_some_and(|g| !g.is_empty()), "a first snapshot carries the recipes");
    assert_eq!(original.meta.containers.len(), 1);
    assert_eq!(original.meta.item_piles.len(), 1);
    assert_eq!(original.meta.designations.len(), 2);
    assert_eq!(original.meta.intents.len(), 1);
    assert_eq!(original.entities.len(), 4, "two slimes, a body and a stud body");
    assert!(original.entities.iter().any(|b| b.carried.stud_body.is_some()));
    assert!(original.players.is_empty());
    // Empty terms: every chunk has a term; the rows are the chunks that differ
    // from their recipe (the dug tile's, the chest's).
    assert_eq!(terms.studs.len(), region.data().stud_index.len());
    let edited = original.stud_chunks.values().filter(|r| matches!(r, Some(StudChunkRecord::Edited(_)))).count();
    assert_eq!(edited, 2, "the dug chunk and the chest's: {:?}", original.stud_chunks.keys().collect::<Vec<_>>());

    let (restored_region, restored_terms) = restore_portable(rc(), stored(original.clone()));
    assert_eq!(restored_terms, terms, "the restored chunks have the saved terms");
    let (restored, _) = snapshot(&restored_region, &SaveTerms::default());

    // Exhaustive on purpose: a new field must be compared (or named as not
    // stored).
    let PortableRegion { meta, entities, players, stud_chunks, stud_ground } = restored;
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
    assert_eq!(stud_chunks, original.stud_chunks);
    assert_eq!(stud_ground, original.stud_ground);
    assert_eq!(derived_velocities_zeroed(entities), derived_velocities_zeroed(original.entities.clone()));
    assert_eq!(players, original.players);
    assert_eq!(
        restored_region.data().pile_expiry().len(),
        region.data().pile_expiry().len(),
        "pile_expiry rebuilt from item_piles"
    );

    let (again, _) = snapshot(&restored_region, &restored_terms);
    assert!(again.stud_ground.is_none(), "the recipes go only into a first snapshot");
    assert!(again.stud_chunks.is_empty(), "nothing changed since the restore");
}

#[test]
fn players_and_people_are_not_entities() {
    let mut region = pond_region();
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.commit_server_event();
    region.with_data(|r| {
        r.spawn_person_safe(game::history::PersonId(4), (300.0, 300.0)).expect("a person");
    });
    let (p, _) = snapshot(&region, &SaveTerms::default());
    assert_eq!(p.entities.len(), 0);
    assert_eq!(p.players.len(), 1);
    assert_eq!(p.players[0].0, 0);
}

/// A snapshot holding the stud rows `entries`: a slab `plates` thick, or a delete.
fn chunk_map(entries: &[(game::StudChunkCoords, Option<u8>)]) -> PortableRegion {
    let mut p = PortableRegion::default();
    for &(c, plates) in entries {
        p.stud_chunks.insert(c, plates.map(|n| StudChunkRecord::Edited(common::slab_chunk(n))));
    }
    p
}

#[test]
fn merge_keeps_uncommitted_chunks() {
    let a = common::sc(0, 0, 0);
    let b = common::sc(1, 0, 0);
    let mut older = chunk_map(&[(a, Some(1))]);
    older.merge(chunk_map(&[(b, Some(2))]));
    assert_eq!(older.stud_chunks.keys().copied().collect::<Vec<_>>(), vec![a, b]);

    let mut older = chunk_map(&[(a, Some(1))]);
    let newer = chunk_map(&[(a, Some(2))]);
    let a2 = newer.stud_chunks[&a].clone();
    older.merge(newer);
    assert_eq!(older.stud_chunks.len(), 1);
    assert_eq!(older.stud_chunks[&a], a2, "the newer chunk wins");
    // A newer delete replaces an older write.
    older.merge(chunk_map(&[(a, None)]));
    assert_eq!(older.stud_chunks[&a], None);

    // The recipes: the first Some.
    let mut first = PortableRegion { stud_ground: Some(Vec::new()), ..Default::default() };
    first.merge(PortableRegion::default());
    assert_eq!(first.stud_ground, Some(Vec::new()));
}

#[test]
fn classify_compiles() {
    let rb = Rollback::new(None);
    classify(&rb);
}
