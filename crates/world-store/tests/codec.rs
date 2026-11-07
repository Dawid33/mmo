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
    EntityKind, IntentMsg, ItemId, ItemPile, ItemStack, PlayerRecord, Pose, RegionCoords, RegionMeta, SentIntent,
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

fn eid(serial: u64) -> EntityId {
    EntityId { birth_region: RegionCoords::new(3, -2), serial }
}

fn pos(x: i64, y: i32, z: i64) -> WorldBlockPos {
    WorldBlockPos { x, y, z }
}

fn chunk() -> Chunk {
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

fn ground() -> ColumnGround {
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

fn meta() -> RegionMeta {
    let mut m = RegionMeta { tick: 123_456, next_game_event_id: 99, next_entity_serial: 12, ..Default::default() };
    m.item_piles.insert(
        pos(10, 20, 30),
        ItemPile {
            stacks: vec![ItemStack { item: ItemId::block(BlockId(1)), count: 5 }],
            expire_tick: 5000,
            pickup_after_tick: 100,
        },
    );
    m.pile_takes.insert((eid(2), pos(10, 20, 30)), 120);
    m.water_bodies.insert(WaterBodyId(7), WaterBody { level: 4096 * 44, volume: 4096 * 4 });
    m.water_bodies.insert(WaterBodyId(8), WaterBody { level: 4096 * 12, volume: 4096 });
    m
}

fn bundle() -> EntityBundle {
    let pose = Pose::from_translation(Vector::new(100.5, 26.0, -40.25));
    let slime = game::slime::Slime { yaw: 0.5, target_yaw: 1.0, next_turn_tick: 900, next_hop_tick: 880, hopping: false };
    EntityBundle {
        id: eid(5),
        kind: EntityKind::Slime { size: 2 },
        body: BodyState::kinematic(pose),
        collider: ColliderSpec::CapsuleY { half_height: 8.0, radius: 6.0 },
        carried: Carried { slime: Some(slime), drops: vec![ItemStack { item: ItemId::block(BlockId(2)), count: 3 }], ..Default::default() },
        client: None,
        pending_inputs: Vec::new(),
    }
}

fn player() -> PlayerRecord {
    PlayerRecord {
        client: Client::default(),
        pose: Pose::from_translation(Vector::new(10.0, 30.0, 12.0)),
        region: RegionCoords::new(-1, 4),
        fortress: None,
        adventure: None,
    }
}

fn intent() -> IntentMsg {
    IntentMsg::Intent(SentIntent {
        target: Target::Block(pos(-5, 12, 99)),
        intent: game::Intent::BreakBlock { by: eid(1), survival: true },
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
