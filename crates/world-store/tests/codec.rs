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
    BlockId, Carried, Client, ColliderSpec, EntityBundle, EntityId,
    EntityKind, IntentMsg, ItemId, ItemPile, ItemStack, PlayerRecord, Pose, RegionCoords, RegionMeta, SentIntent, SentReply,
    Target, WorldBlockPos, BodyState,
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

pub(crate) fn meta() -> RegionMeta {
    let mut m = RegionMeta { tick: 123_456, next_game_event_id: 99, next_entity_serial: 12, ..Default::default() };
    let cell = pos(10, 20, 30);
    m.designations.insert(
        game::StudBox::of_tile([3, 4, 5]).unwrap(),
        game::Designation { kind: game::DesignationKind::Chop, op: game::OpId { client: 2, seq: 7 }, priority: 6 },
    );
    m.designations.insert(
        game::StudBox::of_tile([-1, 9, 2]).unwrap(),
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

/// A stored stud chunk: a filler and a catalog part.
pub(crate) fn stud_chunk() -> game::StudChunkRecord {
    let chunk = game::StudChunk::new([
        game::StudBrick::filler([0, 0, 0], [4, 6, 4], game::MAT_STONE),
        game::StudBrick::part_at([8, 6, 8], 0, 1, 4),
    ])
    .unwrap();
    game::StudChunkRecord::Edited(chunk)
}

/// Stud ground recipes of two chunk columns: flat ground at plates 30 and 95.
pub(crate) fn stud_ground() -> Vec<(game::ColumnCoords, game::StudColumnGround)> {
    let soil = game::StudProfile { cover: game::MAT_GRASS, cover_plates: 1, soil: game::MAT_DIRT, soil_plates: 5 };
    let mut ground = vec![
        (game::ColumnCoords::new(0, 0), game::StudColumnGround::flat(30, soil, game::MAT_STONE, -2000)),
        (game::ColumnCoords::new(1, 0), game::StudColumnGround::flat(95, soil, game::MAT_STONE, -2000)),
    ];
    // Each one's apron on the side of the other: the cliff between them.
    game::link_stud_aprons(&mut ground);
    ground
}

#[test]
fn golden_blobs_decode() {
    check("stud_chunk", FORMAT_STUD_CHUNK, &stud_chunk());
    check("stud_chunk_pristine", FORMAT_STUD_CHUNK, &game::StudChunkRecord::Pristine);
    let fixture = |name: &str| std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap();
    check("stud_ground", FORMAT_STUD_GROUND, &stud_ground());
    // Format 1 (no apron) is still read: the aprons come from the neighbouring
    // columns of the row, the region's edge stays closed. Recorded 2026-11-11 from
    // the format 1 `stud_ground.bin` of the same two columns.
    let v1 = fixture("stud_ground_v1.bin");
    assert_eq!(v1[0], FORMAT_STUD_GROUND_V1);
    let read = decode_stud_ground(&v1).unwrap();
    assert_eq!(read, stud_ground(), "a format 1 row reads as the linked recipes");
    assert_eq!(read[1].1.apron(game::StudSide::West, 3), 30);
    assert_eq!(read[1].1.apron(game::StudSide::East, 3), game::STUD_APRON_CLOSED);
    assert_eq!(decode_stud_ground(&encode(FORMAT_STUD_GROUND, &stud_ground())).unwrap(), stud_ground());
    check("region_meta", FORMAT_REGION_META, &meta());
    // Format 1 (with the voxel world's water bodies) is still read while they are
    // empty — every stud region's row; the golden recorded then held two voxel water
    // bodies, which no longer decode.
    let m = meta();
    let v1 = game::RegionMetaV1 {
        tick: m.tick,
        next_game_event_id: m.next_game_event_id,
        next_entity_serial: m.next_entity_serial,
        designations: m.designations.iter().map(|(b, d)| (b.tiles().min, *d)).collect(),
        constructed: m.constructed.clone(),
        item_piles: m.item_piles.clone(),
        containers: m.containers.clone(),
        pile_takes: m.pile_takes.clone(),
        intents: m.intents.clone(),
        intent_replies: m.intent_replies.clone(),
        intent_inbox: m.intent_inbox.clone(),
        water_bodies: Default::default(),
    };
    assert_eq!(decode_region_meta(&encode(FORMAT_REGION_META_V1, &v1)).unwrap(), m, "a format 1 stud meta decodes to the same meta");
    assert_eq!(decode_region_meta(&encode(FORMAT_REGION_META, &m)).unwrap(), m);
    // Format 2 (designations by tile) is still read, each tile as its cell box: the
    // golden recorded then (the fixture value's two designations sat on tiles).
    let v2 = fixture("region_meta_v2.bin");
    assert_eq!(v2[0], FORMAT_REGION_META_V2);
    assert_eq!(decode_region_meta(&v2).unwrap(), m, "a format 2 meta decodes to the same meta, its tiles as boxes");
    let voxel = fixture("region_meta_v1_water.bin");
    assert_eq!(voxel[0], FORMAT_REGION_META_V1);
    assert!(matches!(decode_region_meta(&voxel), Err(StoreError::Corrupt(_))), "voxel water bodies are refused, not dropped");
    check("entity_bundle", FORMAT_ENTITY_BUNDLE, &bundle());
    // Format 2 (with the voxel body slot, empty in a stud world) is still read: the
    // golden recorded then.
    let v2 = fixture("entity_bundle_v2.bin");
    assert_eq!(v2[0], FORMAT_ENTITY_BUNDLE_V2);
    assert_eq!(decode_entity_bundle(&v2).unwrap(), bundle(), "a format 2 bundle decodes to the same bundle");
    // Format 1 (before `Carried::stud_body`) is still read: the golden recorded then.
    let v1 = std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/entity_bundle_v1.bin")).unwrap();
    assert_eq!(v1[0], FORMAT_ENTITY_BUNDLE_V1);
    assert_eq!(decode_entity_bundle(&v1).unwrap(), bundle(), "a format 1 bundle decodes to the same bundle, no stud body");
    assert_eq!(decode_entity_bundle(&encode(FORMAT_ENTITY_BUNDLE, &bundle())).unwrap(), bundle());
    assert!(decode_entity_bundle(&[9, 1, 2]).is_err() && decode_entity_bundle(&[]).is_err());
    check("player_record", FORMAT_PLAYER_RECORD, &player());
    check("intent_msg", FORMAT_INTENT_MSG, &intent());
    check("intent_msg_reply", FORMAT_INTENT_MSG, &IntentMsg::Reply(reply()));
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

#[test]
fn wrong_format_byte_is_corrupt() {
    use game::StudChunkRecord;
    let blob = encode(FORMAT_STUD_CHUNK, &stud_chunk());
    let err = decode::<StudChunkRecord>(FORMAT_STUD_CHUNK + 1, &blob).unwrap_err();
    assert!(matches!(&err, StoreError::Corrupt(m) if m.contains("StudChunkRecord")), "{err}");
    let mut bad = blob.clone();
    bad.truncate(blob.len() / 2);
    assert!(matches!(decode::<StudChunkRecord>(FORMAT_STUD_CHUNK, &bad), Err(StoreError::Corrupt(_))));
    assert!(matches!(decode::<StudChunkRecord>(FORMAT_STUD_CHUNK, &[]), Err(StoreError::Corrupt(_))));
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
