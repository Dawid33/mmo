#[test]
fn chunk_edge_is_a_whole_number_of_bricks() {
    assert_eq!(game::CHUNK_UNITS % game::BRICK_VOXELS, 0);
    assert_eq!(game::BRICKS_PER_CHUNK_EDGE * game::BRICK_VOXELS, game::CHUNK_UNITS);
    assert_eq!(game::BRICK_BLOCKS * game::BLOCK_VOXELS, game::BRICK_VOXELS);
}

#[test]
fn chunk_coords_round_trip_through_body_translation() {
    for c in [game::ChunkCoords::new(0, 0, 0), game::ChunkCoords::new(3, -5, 7)] {
        let u = game::CHUNK_UNITS as f32;
        let t = game::rapier3d::math::Vector::new(c.x as f32 * u, c.y as f32 * u, c.z as f32 * u);
        assert_eq!(game::chunk_coords_from_translation(t), c);
    }
}
