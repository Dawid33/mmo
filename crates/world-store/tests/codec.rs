//! Golden blobs of every stored row type. The encoded bytes of a fixed value
//! must equal the checked-in `tests/fixtures/<name>.bin`, and the fixture must
//! decode back to the value. A deliberate layout change re-records them:
//!
//!     MMO_RECORD_GOLDENS=1 just test-one world-store "--test it codec::"
//!
//! (and bumps the type's `FORMAT_*` constant).

use std::fmt::Debug;
use std::path::PathBuf;

use game::rapier3d::math::Vector;
use game::{
    Block, BlockId, ChiselData, Carried, Chunk, Client, ColliderSpec, ColumnGround, EntityBundle, EntityId,
    EntityKind, IntentMsg, ItemId, ItemPile, ItemStack, PlayerRecord, Pose, RegionCoords, RegionMeta, SentIntent, SentReply,
    Target, WaterBody, WaterBodyId, WorldBlockPos, BodyState,
};
use serde::{Serialize, de::DeserializeOwned};
use world_store::*;

fn check<T: Serialize + DeserializeOwned + PartialEq + Debug>(name: &str, format: u8, value: &T) {
    let encoded = encode(format, value);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(format!("{name}.bin"));
    if std::env::var_os("MMO_RECORD_GOLDENS").is_some() {
        std::fs::write(&path, &encoded).unwrap();
    }
    let golden = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (record with MMO_RECORD_GOLDENS=1)", path.display()));
    assert_eq!(encoded, golden, "{name}: encoding changed; re-record on purpose and bump FORMAT_*");
    let back: T = decode(format, &golden).unwrap();
    assert_eq!(&back, value, "{name}: golden does not decode to the fixture value");
}

/// A 2 x 1 world map: one land tile with every optional field set, one sea tile.
pub(crate) fn world_map() -> game::WorldMap {
    let land = game::TileSummary {
        ground_min_m: 12.5,
        ground_mean_m: 40.25,
        ground_max_m: 96.0,
        relief_m: 83.5,
        slope: [0.125, -0.25],
        sea_fraction: 0.0,
        lake_fraction: 0.0625,
        lake_level_m: Some(30.0),
        rivers: 2,
        river_max_width_m: 14.0,
        river_max_discharge_m2: 3.5,
        surface_rock: Some(3),
        landing: Some([17, 201]),
        biome: game::Biome::BroadleafForest,
        region: 1,
        temperature_c: 11.5,
        precip_mm: 900.0,
        savagery: 33,
        alignment: -1,
        volcano: true,
    };
    let sea = game::TileSummary {
        lake_level_m: None,
        surface_rock: None,
        landing: None,
        biome: game::Biome::Ocean,
        region: 0,
        sea_fraction: 1.0,
        volcano: false,
        ..land.clone()
    };
    game::WorldMap {
        origin: RegionCoords::new(-1, 4),
        width: 2,
        height: 1,
        tiles: vec![land, sea],
        rock_names: vec!["granite".into(), "basalt".into(), "limestone".into(), "slate".into()],
        region_types: vec![game::RegionType::Ocean, game::RegionType::Forest],
    }
}

pub(crate) fn eid(serial: u64) -> EntityId {
    EntityId { birth_region: RegionCoords::new(3, -2), serial }
}

pub(crate) fn pos(x: i64, y: i32, z: i64) -> WorldBlockPos {
    WorldBlockPos { x, y, z }
}

pub(crate) fn chunk() -> Chunk {
    let mut c = Chunk::default();
    c.set_block(1, 2, 3, Block::new(BlockId(1)));
    c.set_block(1, 3, 3, Block::new(BlockId(2)));
    c.set_block(5, 0, 7, Block::new(BlockId(3)));
    c.set_block(6, 6, 6, Block::new(BlockId(2)));
    c.set_block(6, 7, 6, Block::new(BlockId(1)));
    c.set_block(0, 0, 0, Block { id: BlockId(4), data: [1, 2, 3] });
    c.set_block(2, 2, 2, Block::new(BlockId(1)));
    c.chisel.insert(game::BlockIndex(0), ChiselData::cuboid(BlockId(4), [0, 0, 0], [4, 8, 4]));
    c
}

pub(crate) fn ground() -> ColumnGround {
    let mut g = ColumnGround::plain(
        (0..game::COLUMN_FOOTPRINT).map(|i| 40 + (i % 5) as i16).collect(),
        -3,
        BlockId(1),
        BlockId(9),
    );
    g.strata = vec![(0, BlockId(1)), (-8, BlockId(2)), (-20, BlockId(3))];
    for i in [0usize, 1, 2, 17] {
        g.water_top[i] = g.surface[i] + 3;
        g.water_body[i] = 7;
    }
    g
}

pub(crate) fn meta() -> RegionMeta {
    let mut m = RegionMeta { tick: 123_456, next_game_event_id: 99, next_entity_serial: 12, ..Default::default() };
    let cell = pos(10, 20, 30);
    m.designations.insert(
        [3, 4, 5],
        game::Designation { kind: game::DesignationKind::Chop, op: game::OpId { client: 2, seq: 7 }, priority: 6 },
    );
    m.designations.insert(
        [-1, 9, 2],
        game::Designation { kind: game::DesignationKind::Build(BlockId(3)), op: game::OpId { client: 1, seq: 8 }, priority: 2 },
    );
    m.constructed.insert([6, 7, 8], ());
    m.item_piles.insert(
        cell,
        ItemPile {
            stacks: vec![
                ItemStack { item: ItemId::block(BlockId(1)), count: 5 },
                ItemStack { item: ItemId::block(BlockId(3)), count: 64 },
            ],
            expire_tick: 5000,
            pickup_after_tick: 100,
        },
    );
    m.containers.insert(
        pos(-4, 11, 8),
        game::Container { slots: vec![None, Some(ItemStack { item: ItemId::block(BlockId(2)), count: 17 }), None] },
    );
    m.pile_takes.insert((eid(2), cell), 120);
    let sent = SentIntent {
        target: Target::Container(cell),
        intent: game::Intent::TakeFromContainer {
            cell,
            slot: 4,
            item: ItemId::block(BlockId(2)),
            count: 9,
            to_cursor: true,
            by: eid(2),
        },
        from: RegionCoords::new(1, 0),
        due: 777,
    };
    m.intents.insert((777, eid(2), 3), sent);
    m.intent_replies.insert((778, eid(2), 4), reply());
    m.intent_inbox.insert(
        (eid(2), 5),
        game::IntentReply::Refused(sent.intent, game::IntentRefusal::ContainerFull),
    );
    m.water_bodies.insert(WaterBodyId(7), WaterBody { level: 4096 * 44, volume: 4096 * 4 });
    m.water_bodies.insert(WaterBodyId(8), WaterBody { level: 4096 * 12, volume: 4096 });
    m
}

pub(crate) fn reply() -> SentReply {
    SentReply {
        reply: game::IntentReply::Refused(
            game::Intent::PutInContainer {
                cell: pos(1, 2, 3),
                stack: ItemStack { item: ItemId::block(BlockId(2)), count: 4 },
                slot: Some(2),
                by: eid(6),
            },
            game::IntentRefusal::ContainerFull,
        ),
        hint: RegionCoords::new(-2, 5),
        due: 901,
        hops: 2,
    }
}

pub(crate) fn bundle() -> EntityBundle {
    let pose = Pose::from_translation(Vector::new(100.5, 26.0, -40.25));
    let slime = game::slime::Slime { yaw: 0.5, target_yaw: 1.0, next_turn_tick: 900, next_hop_tick: 880, hopping: false };
    EntityBundle {
        id: eid(5),
        kind: EntityKind::Slime { size: 2 },
        body: BodyState::kinematic(pose),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.0 },
        carried: Carried {
            slime: Some(slime),
            motion: Some(game::Motion { vel_y: -3.5, grounded: true, last_jump_tap: Some(40), ..Default::default() }), drops: vec![ItemStack { item: ItemId::block(BlockId(2)), count: 3 }], ..Default::default() },
        client: None,
        pending_inputs: Vec::new(),
    }
}

pub(crate) fn player() -> PlayerRecord {
    let mut client = Client::default();
    client.inventory.slots[0] = Some(ItemStack { item: ItemId::block(BlockId(1)), count: 12 });
    client.inventory.slots[10] = Some(ItemStack { item: ItemId::block(BlockId(3)), count: 64 });
    PlayerRecord {
        client,
        pose: Pose::from_translation(Vector::new(10.0, 30.0, 12.0)),
        region: RegionCoords::new(-1, 4),
        fortress: Some(game::history::SiteId(4)),
        adventure: Some(game::history::SiteId(9)),
    }
}

pub(crate) fn intent() -> IntentMsg {
    IntentMsg::Intent(SentIntent {
        target: Target::Pile(pos(-5, 12, 99)),
        intent: game::Intent::DepositItems {
            cell: pos(-5, 12, 99),
            stack: ItemStack { item: ItemId::block(BlockId(2)), count: 7 },
            expire: 9000,
            pickup: 4300,
            by: eid(1),
        },
        from: RegionCoords::new(0, 1),
        due: 4242,
    })
}

#[test]
fn golden_blobs_decode() {
    check("region_meta", FORMAT_REGION_META, &meta());
    check("chunk", FORMAT_CHUNK, &chunk());
    check("column_ground", FORMAT_COLUMN_GROUND, &ground());
    check("entity_bundle", FORMAT_ENTITY_BUNDLE, &bundle());
    check("player_record", FORMAT_PLAYER_RECORD, &player());
    check("intent_msg", FORMAT_INTENT_MSG, &intent());
    check("intent_msg_reply", FORMAT_INTENT_MSG, &IntentMsg::Reply(reply()));
    check("region_ground", FORMAT_REGION_GROUND, &region_ground());
    check_world_map();
}

/// The world-map row has its own framing (`encode_world_map`: format byte,
/// `WORLD_MAP_VERSION`, crc32, zstd body), so it is pinned on its own; a
/// `WORLD_MAP_VERSION` bump re-records it too.
fn check_world_map() {
    let encoded = encode_world_map(&world_map());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/world_map.bin");
    if std::env::var_os("MMO_RECORD_GOLDENS").is_some() {
        std::fs::write(&path, &encoded).unwrap();
    }
    let golden = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (record with MMO_RECORD_GOLDENS=1)", path.display()));
    assert_eq!(encoded, golden, "world_map: encoding changed; re-record on purpose and bump FORMAT_WORLD_MAP / WORLD_MAP_VERSION");
    assert_eq!(decode_world_map(&golden).unwrap(), world_map(), "world_map: golden does not decode to the fixture value");
}

/// A region's ground recipes as `regions.ground` stores them.
pub(crate) fn region_ground() -> Vec<(game::ColumnCoords, ColumnGround)> {
    let mut other = ground();
    other.surface[3] = -7;
    vec![(game::ColumnCoords::new(0, 0), ground()), (game::ColumnCoords::new(1, 0), other)]
}

#[test]
fn wrong_format_byte_is_corrupt() {
    let blob = encode(FORMAT_CHUNK, &chunk());
    let err = decode::<Chunk>(FORMAT_CHUNK + 1, &blob).unwrap_err();
    assert!(matches!(&err, StoreError::Corrupt(m) if m.contains("Chunk")), "{err}");
    let mut bad = blob.clone();
    bad.truncate(blob.len() / 2);
    assert!(matches!(decode::<Chunk>(FORMAT_CHUNK, &bad), Err(StoreError::Corrupt(_))));
    assert!(matches!(decode::<Chunk>(FORMAT_CHUNK, &[]), Err(StoreError::Corrupt(_))));
}

#[test]
fn world_map_row_errors_name_their_cause() {
    let good = encode_world_map(&world_map());
    let old = encode(1, &world_map());
    assert_eq!(decode_world_map(&old), Err(WorldMapRowError::Layout { format: 1 }));
    let mut other = good.clone();
    other[1..5].copy_from_slice(&7u32.to_le_bytes());
    assert_eq!(decode_world_map(&other), Err(WorldMapRowError::Version { found: 7 }));
    let mut flipped = good.clone();
    let mid = good.len() / 2 + 5;
    flipped[mid] ^= 1;
    assert!(matches!(decode_world_map(&flipped), Err(WorldMapRowError::Damaged(m)) if m.contains("crc32")));
    assert!(matches!(decode_world_map(&good[..4]), Err(WorldMapRowError::Damaged(_))));
    assert!(matches!(decode_world_map(&[]), Err(WorldMapRowError::Damaged(_))));
}
