use game::{Chunk, ChunkCoords, Region, RegionCoords, TerrainSnapshot, TERRAIN_HASH_INTERVAL_TICKS};

fn flat_chunks() -> Vec<(ChunkCoords, Chunk)> {
    (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect()
}

#[test]
fn constants_have_expected_defaults() {
    // The terrain-hash check cadence doubles as the LOD refresh throttle;
    // repin deliberately if tuned.
    assert_eq!(TERRAIN_HASH_INTERVAL_TICKS, 25);
}

#[test]
fn region_terrain_snapshot_returns_all_terrain_chunks() {
    let region = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks());
    let snap = region.terrain_snapshot();
    // Four terrain chunks in, four out — no entities/players contributed.
    assert_eq!(snap.chunks.len(), 4);
    let mut coords: Vec<(usize, i32, usize)> =
        snap.chunks.iter().map(|(c, _)| (c.x, c.y, c.z)).collect();
    coords.sort();
    assert_eq!(coords, vec![(0, 0, 0), (0, 0, 1), (1, 0, 0), (1, 0, 1)]);
}

#[test]
fn terrain_hash_is_stable_and_content_sensitive() {
    let a = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).terrain_snapshot();
    let b = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).terrain_snapshot();
    assert_eq!(a.terrain_hash(), b.terrain_hash());

    let taller: Vec<(ChunkCoords, Chunk)> = (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(12))))
        .collect();
    let c = Region::from_chunks(RegionCoords::new(0, 0), taller).terrain_snapshot();
    assert_ne!(a.terrain_hash(), c.terrain_hash());
}

#[test]
fn from_columns_materializes_same_chunks_as_region() {
    use game::{Block, BlockId, Column, ColumnCoords, SubChunk};
    let below = Block::new(BlockId::AIR);
    let above = Block::new(BlockId::AIR);
    let columns: Vec<(ColumnCoords, Column)> = (0..2)
        .flat_map(move |x| {
            (0..2).map(move |z| {
                let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
                (ColumnCoords::new(x, z), Column::new(0, below, above, vec![sub]))
            })
        })
        .collect();
    let from_cols = TerrainSnapshot::from_columns(columns);
    let from_region = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).terrain_snapshot();
    assert_eq!(from_cols.terrain_hash(), from_region.terrain_hash());
}

/// The in-place fingerprint is what the region thread polls every
/// `TERRAIN_HASH_INTERVAL_TICKS`; it must detect exactly what
/// `terrain_snapshot().terrain_hash()` detects, without building a snapshot.
#[test]
fn terrain_fingerprint_is_stable_content_sensitive_and_order_insensitive() {
    let id = RegionCoords::new(0, 0);
    let a = Region::from_chunks(id, flat_chunks());
    let b = Region::from_chunks(id, flat_chunks());
    assert_eq!(a.terrain_fingerprint(), b.terrain_fingerprint());

    // ECS insertion order must not matter (the snapshot sorts for the same reason).
    let mut reversed = flat_chunks();
    reversed.reverse();
    let c = Region::from_chunks(id, reversed);
    assert_eq!(a.terrain_fingerprint(), c.terrain_fingerprint());

    let taller: Vec<(ChunkCoords, Chunk)> = (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(12))))
        .collect();
    let d = Region::from_chunks(id, taller);
    assert_ne!(a.terrain_fingerprint(), d.terrain_fingerprint());

    // Same content at a different coordinate is different terrain.
    let mut moved = flat_chunks();
    moved[0].0 = ChunkCoords::new(5, 0, 5);
    let e = Region::from_chunks(id, moved);
    assert_ne!(a.terrain_fingerprint(), e.terrain_fingerprint());
}

#[test]
fn terrain_fingerprint_changes_when_a_chunk_is_added() {
    let mut region = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks());
    let before = region.terrain_fingerprint();
    region.create_basic(ChunkCoords::new(0, 1, 0));
    assert_ne!(before, region.terrain_fingerprint());
}
