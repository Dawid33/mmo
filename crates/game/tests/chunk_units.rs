#[test]
fn unit_constants_hold_at_every_legal_region_size() {
    use game::{
        max_decompressed_packet_bytes, CHUNK_BLOCKS, CHUNK_UNITS, REGION_BLOCKS,
        REGION_BLOCKS_CHOICES, REGION_CHUNKS, REGION_SIZE, STUD_CHUNK_STUDS, STUD_UNITS, TILE_STUDS,
    };
    for &b in &REGION_BLOCKS_CHOICES {
        assert!(max_decompressed_packet_bytes(b) > 0, "packet cap is 0 at {b} blocks");
        assert!(max_decompressed_packet_bytes(b) >= 256 << 20, "cap below the 64-block cap at {b}");
    }
    // A tile (the unit regions and chunks are counted in) is `TILE_STUDS` studs wide.
    assert_eq!(REGION_SIZE, (REGION_BLOCKS * TILE_STUDS * STUD_UNITS) as f32);
    assert_eq!(REGION_CHUNKS * CHUNK_BLOCKS, REGION_BLOCKS);
    // A stud chunk column is a sim chunk column.
    assert_eq!(STUD_CHUNK_STUDS * STUD_UNITS, CHUNK_UNITS);
    assert_eq!(REGION_CHUNKS * CHUNK_UNITS, REGION_SIZE as usize);

    // Today's values at the 64-block test default.
    assert_eq!(max_decompressed_packet_bytes(64), 256 << 20);
}
