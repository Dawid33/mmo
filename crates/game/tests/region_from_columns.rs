use game::{Block, BlockId, Chunk, Column, ColumnCoords, RegionCoords, SubChunk};

fn stone() -> Block { Block::new(BlockId(2)) }
fn air() -> Block { Block::new(BlockId::AIR) }

fn dense_floor() -> SubChunk {
    SubChunk::Dense(Box::new(Chunk::flat_floor_with(8, BlockId(2))))
}

#[test]
fn materialized_sub_chunks_skip_air() {
    // base_sub_y = 0, one dense floor, then (trimmed) air above.
    let col = Column::new(0, stone(), air(), vec![dense_floor(), SubChunk::Air]);
    let mats = col.materialized_sub_chunks();
    assert_eq!(mats.len(), 1);
    assert_eq!(mats[0].0, 0); // sub_y
}

#[test]
fn from_columns_builds_a_region() {
    let id = RegionCoords::new(0, 0);
    let columns = vec![
        (ColumnCoords::new(0, 0), Column::new(0, stone(), air(), vec![dense_floor()])),
        (ColumnCoords::new(1, 0), Column::new(0, stone(), air(), vec![dense_floor()])),
    ];
    // Must not panic; produces a region with the two flattened sub-chunks.
    let _region = game::Region::from_columns(id, columns);
}

fn ground_region() -> game::Region {
    let ground = game::ColumnGround::plain(vec![10; game::COLUMN_FOOTPRINT].into_boxed_slice(), 2, BlockId(2), BlockId(3));
    let col = Column::new(2, stone(), air(), vec![dense_floor()]).with_ground(ground);
    game::Region::from_columns(RegionCoords::new(0, 0), vec![(ColumnCoords::new(0, 0), col)])
}

#[test]
fn implied_ground_below_the_band() {
    let region = ground_region();
    let d = region.data();
    assert_eq!(d.implied_block(game::ChunkCoords::new(0, 1, 0), (0, 0, 0)), Some(stone()));
    assert_eq!(
        d.implied_block(game::ChunkCoords::new(0, -30, 0), (0, 0, 0)),
        Some(Block::new(BlockId(3)))
    );
    // Materialised (a chunk entity exists) and above the band: not implied.
    assert_eq!(d.implied_block(game::ChunkCoords::new(0, 2, 0), (0, 0, 0)), None);
    assert_eq!(d.implied_block(game::ChunkCoords::new(0, 9, 0), (0, 0, 0)), None);
    // A column without ground contributes nothing.
    assert_eq!(d.implied_block(game::ChunkCoords::new(5, 0, 5), (0, 0, 0)), None);
}

#[test]
fn ground_survives_park_and_restore() {
    let region = ground_region();
    let parked = game::SerializedRegion::from_rollback(region.data());
    let back = parked.to_rollback().expect("restores");
    for c in [(0, 1, 0), (0, -30, 0), (0, 2, 0)] {
        let c = game::ChunkCoords::new(c.0, c.1, c.2);
        assert_eq!(region.data().implied_block(c, (1, 1, 1)), back.implied_block(c, (1, 1, 1)));
    }
    assert_eq!(game::state_hash(region.data()), game::state_hash(&back));
}

// --- Recipe water and bodies at genesis (water plan Task 5) ------------------------

/// Ground top of the pond fixture's footprint, block y.
const POND_SURFACE: i16 = 67;
/// The pond's recipe water top, block y: water at 68..=70.
const POND_TOP: i16 = 70;
/// The fixture's lowest sub-chunk: the one holding `POND_SURFACE`.
const POND_SUB_Y: i32 = POND_SURFACE as i32 / game::CHUNK_BLOCKS as i32;
/// The fixture's highest sub-chunk: the one holding `POND_TOP`.
const POND_TOP_SUB_Y: i32 = POND_TOP as i32 / game::CHUNK_BLOCKS as i32;
const ICE: BlockId = BlockId(23);

/// Chunk column (0, 0) of a region: solid up to `POND_SURFACE`, air above
/// (sub-chunks `POND_SUB_Y ..= POND_TOP_SUB_Y` materialised), with a plain
/// ground recipe and recipe water topped at `top` over each listed block
/// column `(lx, lz, top)`; `ice` cells (`(lx, y, lz)`) hold ice.
fn pond_region(water: &[(usize, usize, i16)], ice: &[(usize, i32, usize)]) -> game::Region {
    crate::common::install_real_block_props();
    let cb = game::CHUNK_BLOCKS as i32;
    let floor_voxels = ((i32::from(POND_SURFACE) + 1 - POND_SUB_Y * cb) as usize * game::BLOCK_VOXELS) as u32;
    let mut chunks: Vec<Chunk> = (POND_SUB_Y..=POND_TOP_SUB_Y)
        .map(|sub_y| match sub_y {
            POND_SUB_Y => Chunk::flat_floor_with(floor_voxels, BlockId(2)),
            _ => Chunk::uniform(air()),
        })
        .collect();
    for &(lx, y, lz) in ice {
        chunks[(y.div_euclid(cb) - POND_SUB_Y) as usize].set_block(lx, y.rem_euclid(cb) as usize, lz, Block::new(ICE));
    }
    let mut ground = game::ColumnGround::plain(
        vec![POND_SURFACE; game::COLUMN_FOOTPRINT].into_boxed_slice(),
        POND_SUB_Y,
        BlockId(2),
        BlockId(6),
    );
    for &(lx, lz, top) in water {
        ground.water_top[lx * game::CHUNK_BLOCKS + lz] = top;
    }
    let subs = chunks.into_iter().map(|c| SubChunk::Dense(Box::new(c))).collect();
    let col = Column::new(POND_SUB_Y, stone(), air(), subs).with_ground(ground);
    game::Region::from_columns(RegionCoords::new(0, 0), vec![(ColumnCoords::new(0, 0), col)])
}

/// `(lx, lz, top)` over the block box `xs × zs`.
fn square(xs: std::ops::Range<usize>, zs: std::ops::Range<usize>, top: i16) -> Vec<(usize, usize, i16)> {
    xs.flat_map(|x| zs.clone().map(move |z| (x, z, top))).collect()
}

fn units(region: &game::Region, x: i32, y: i32, z: i32) -> u16 {
    region.data().water_units_at(game::WorldBlockPos::from_local(RegionCoords::new(0, 0), game::BlockPos::new(x, y, z)))
}

fn body_id(x: usize, z: usize) -> game::WaterBodyId {
    game::WaterBodyId((x * game::REGION_BLOCKS as usize + z) as u32)
}

#[test]
fn pond_columns_form_one_body() {
    let region = pond_region(&square(2..5, 2..5, POND_TOP), &[]);
    let bodies = region.data().water_bodies().clone();
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    let id = body_id(2, 2);
    let body = bodies.get(&id).expect("the body's id is its smallest column");
    assert_eq!(body.level, 71 * i64::from(game::WATER_FULL));
    assert_eq!(body.volume, 27 * u64::from(game::WATER_FULL));
    for y in 68..=70 {
        assert_eq!(units(&region, 3, y, 3), game::WATER_FULL, "y {y}");
        assert_eq!(region.data().water_body_at(game::BlockPos::new(3, y, 3)), Some(id));
    }
    assert_eq!(units(&region, 3, 67, 3), 0, "the bed");
    assert_eq!(units(&region, 3, 71, 3), 0, "above the surface");
    assert_eq!(units(&region, 6, 68, 6), 0, "beside the pond");
    assert_eq!(region.data().water_body_at(game::BlockPos::new(3, 71, 3)), None);
    assert_eq!(region.data().water_body_at(game::BlockPos::new(6, 68, 6)), None);
}

#[test]
fn separate_ponds_at_one_height_are_two_bodies() {
    let mut water = square(1..3, 1..3, POND_TOP);
    water.extend(square(6..8, 6..8, POND_TOP));
    let region = pond_region(&water, &[]);
    let bodies = region.data().water_bodies();
    assert_eq!(bodies.keys().copied().collect::<Vec<_>>(), vec![body_id(1, 1), body_id(6, 6)]);
    for b in bodies.values() {
        assert_eq!(b.volume, 12 * u64::from(game::WATER_FULL));
    }
}

#[test]
fn river_steps_are_separate_bodies() {
    let region = pond_region(&[(2, 2, 70), (3, 2, 69)], &[]);
    let bodies = region.data().water_bodies();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert_eq!(bodies[&body_id(2, 2)].level, 71 * i64::from(game::WATER_FULL));
    assert_eq!(bodies[&body_id(3, 2)].level, 70 * i64::from(game::WATER_FULL));
    assert_eq!(bodies[&body_id(3, 2)].volume, 2 * u64::from(game::WATER_FULL));
}

#[test]
fn ice_cell_is_not_water_until_broken() {
    let mut region = pond_region(&square(2..5, 2..5, POND_TOP), &[(3, 70, 3)]);
    assert_eq!(units(&region, 3, 70, 3), 0, "ice");
    assert_eq!(units(&region, 3, 69, 3), game::WATER_FULL, "under the ice");
    assert_eq!(region.data().water_bodies()[&body_id(2, 2)].volume, 26 * u64::from(game::WATER_FULL), "ice excluded");
    region.with_data(|d| assert!(d.set_block_safe([3, 70, 3], air()), "break the ice"));
    assert_eq!(units(&region, 3, 70, 3), game::WATER_FULL, "a broken ice cell is water");
}

#[test]
fn edits_follow_static_water_semantics() {
    let mut region = pond_region(&square(2..5, 2..5, POND_TOP), &[]);
    let bodies = region.data().water_bodies().clone();
    region.with_data(|d| assert!(d.set_block_safe([3, 69, 3], stone()), "place"));
    assert_eq!(units(&region, 3, 69, 3), 0, "a placed block wins");
    assert_eq!(region.data().water_bodies(), &bodies);
    region.with_data(|d| assert!(d.set_block_safe([3, 69, 3], air()), "break it"));
    assert_eq!(units(&region, 3, 69, 3), game::WATER_FULL, "inside the recipe: water again");
    assert_eq!(region.data().water_bodies(), &bodies);
    region.with_data(|d| assert!(d.set_block_safe([3, 67, 3], air()), "break the bed"));
    assert_eq!(units(&region, 3, 67, 3), 0, "below the recipe: dry");
    assert_eq!(region.data().water_bodies(), &bodies);
}
