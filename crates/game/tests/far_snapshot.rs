use game::{
    Chunk, ChunkCoords, FarSnapshot, Region, RegionCoords, FAR_SNAPSHOT_MIN_INTERVAL_TICKS,
    FAR_VIEW_RADIUS,
};

fn flat_chunks() -> Vec<(ChunkCoords, Chunk)> {
    (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect()
}

#[test]
fn constants_have_expected_defaults() {
    // Far ring at radius 2: one visual-only ring of far regions outside the
    // near 3×3 window (5×5 footprint, 16 far regions), keeping the join-time
    // worldgen/meshing burst small — LOD is still future work.
    assert_eq!(FAR_VIEW_RADIUS, 2);
    assert_eq!(FAR_SNAPSHOT_MIN_INTERVAL_TICKS, 25);
}

#[test]
fn region_far_snapshot_returns_all_terrain_chunks() {
    let region = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks());
    let snap = region.far_snapshot();
    // Four terrain chunks in, four out — no entities/players contributed.
    assert_eq!(snap.chunks.len(), 4);
    let mut coords: Vec<(usize, i32, usize)> =
        snap.chunks.iter().map(|(c, _)| (c.x, c.y, c.z)).collect();
    coords.sort();
    assert_eq!(coords, vec![(0, 0, 0), (0, 0, 1), (1, 0, 0), (1, 0, 1)]);
}

#[test]
fn terrain_hash_is_stable_and_content_sensitive() {
    let a = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).far_snapshot();
    let b = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).far_snapshot();
    assert_eq!(a.terrain_hash(), b.terrain_hash());

    let taller: Vec<(ChunkCoords, Chunk)> = (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(12))))
        .collect();
    let c = Region::from_chunks(RegionCoords::new(0, 0), taller).far_snapshot();
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
    let from_cols = FarSnapshot::from_columns(columns);
    let from_region = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).far_snapshot();
    assert_eq!(from_cols.terrain_hash(), from_region.terrain_hash());
}

#[test]
fn far_wire_packets_roundtrip_bincode() {
    use game::{ClientPacket, RegionCoords, ServerPacket};

    let rc = RegionCoords::new(3, -1);
    let req = ClientPacket::RequestFarRegion(rc);
    let bytes = bincode::serialize(&req).unwrap();
    assert!(matches!(
        bincode::deserialize::<ClientPacket>(&bytes).unwrap(),
        ClientPacket::RequestFarRegion(r) if r == rc
    ));

    let rel = ClientPacket::ReleaseFarRegion(rc);
    let bytes = bincode::serialize(&rel).unwrap();
    assert!(matches!(
        bincode::deserialize::<ClientPacket>(&bytes).unwrap(),
        ClientPacket::ReleaseFarRegion(r) if r == rc
    ));

    let snap = Region::from_chunks(rc, flat_chunks()).far_snapshot();
    let pkt = ServerPacket::FarRegion(rc, snap);
    let bytes = bincode::serialize(&pkt).unwrap();
    assert!(matches!(
        bincode::deserialize::<ServerPacket>(&bytes).unwrap(),
        ServerPacket::FarRegion(r, s) if r == rc && s.chunks.len() == 4
    ));
}
