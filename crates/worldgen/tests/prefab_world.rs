use game::{RegionCoords, StudRegionSeed, REGION_BLOCKS, REGION_CHUNKS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, TILE_STUDS};
use worldgen::prefab_world::*;

#[test]
fn editor_ground_is_flat_everywhere_inside() {
    for rc in [RegionCoords::new(0, 0), RegionCoords::new(-1, 1)] {
        let s = region_seed(rc);
        assert!(s.features.is_empty() && s.stamps.is_empty());
        assert_eq!(s.ground.len(), REGION_CHUNKS * REGION_CHUNKS);
        for (_, g) in &s.ground {
            for (x, z) in [(0, 0), (31, 31), (7, 20)] {
                assert_eq!(g.surface(x, z), EDITOR_SURFACE_PLATE);
            }
        }
    }
}

#[test]
fn the_baseplate_is_one_plain_material_with_nothing_above() {
    let s = region_seed(RegionCoords::new(0, 0));
    let (_, g) = &s.ground[0];
    let top = EDITOR_SURFACE_PLATE.div_euclid(STUD_CHUNK_PLATES as i32) - 1;
    let chunk = g.pristine_chunk(top).expect("ground under the surface");
    let surface = chunk.brick_at([5, (STUD_CHUNK_PLATES - 1) as u8, 5]).expect("the top plate");
    assert_eq!(surface.material, BASEPLATE);
    assert!(g.pristine_chunk(top + 1).is_none(), "nothing stands on the baseplate");
    assert_eq!(g.water_top(3, 3), None);
}

#[test]
fn outside_the_radius_is_empty() {
    assert_eq!(region_seed(RegionCoords::new(EDITOR_WORLD_RADIUS + 1, 0)), StudRegionSeed::default());
    assert_eq!(region_seed(RegionCoords::new(0, -EDITOR_WORLD_RADIUS - 1)), StudRegionSeed::default());
}

#[test]
fn anchor_leaves_the_extent_cap_inside_region_zero() {
    let a = editor_anchor();
    let studs = (REGION_BLOCKS * TILE_STUDS) as i64;
    let half = game::PREFAB_HALF_STUDS as i64;
    assert!(a[0] - half >= 0 && a[0] + half <= studs && a[2] - half >= 0 && a[2] + half <= studs);
    assert_eq!(a[1], EDITOR_SURFACE_PLATE as i64);
    // Mid chunk, so the middle of a prefab is clear of chunk edges.
    assert_eq!(a[0].rem_euclid(STUD_CHUNK_STUDS as i64), STUD_CHUNK_STUDS as i64 / 2);
    // The whole height range of a prefab is in legal layers.
    assert!(game::stud_layer_ok(((a[1] + game::PREFAB_MAX_PLATE as i64) / STUD_CHUNK_PLATES as i64) as i32));
}

#[test]
fn deterministic() {
    assert!(region_seed(RegionCoords::new(1, 1)) == region_seed(RegionCoords::new(1, 1)));
}
