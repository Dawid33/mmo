use game::{RegionCoords, StudChunkCoords, StudRegionSeed, REGION_CHUNKS, STUD_CHUNK_STUDS};
use worldgen::stud_world::*;

const N: usize = STUD_CHUNK_STUDS;

fn seed(x: i32, z: i32) -> StudRegionSeed {
    region_seed(RegionCoords::new(x, z))
}

#[test]
fn outside_radius_is_empty() {
    assert_eq!(seed(2, 0), StudRegionSeed::default());
    assert_eq!(seed(0, -2), StudRegionSeed::default());
}

#[test]
fn inside_radius_has_a_recipe_for_every_chunk_column() {
    let s = seed(1, -1);
    assert_eq!(s.ground.len(), REGION_CHUNKS * REGION_CHUNKS);
    let mut cols: Vec<_> = s.ground.iter().map(|(c, _)| (c.x, c.z)).collect();
    cols.sort();
    cols.dedup();
    assert_eq!(cols.len(), REGION_CHUNKS * REGION_CHUNKS);
    assert!(s.features.is_empty(), "only region (0, 0) has the spawn feature");
}

#[test]
fn deterministic() {
    assert!(seed(0, 0) == seed(0, 0));
}

#[test]
fn surface_matches_height_field_and_bounds() {
    let rc = RegionCoords::new(-1, 1);
    let region = (REGION_CHUNKS * N) as i64;
    for (c, g) in region_seed(rc).ground {
        // The whole toy world sits in layer 0.
        assert_eq!(g.surface_layers(), 0..=0);
        for sx in 0..N {
            for sz in 0..N {
                let (wx, wz) = (rc.x as i64 * region + (c.x * N + sx) as i64, rc.z as i64 * region + (c.z * N + sz) as i64);
                let want = surface_plates(wx, wz);
                assert!((MIN_SURFACE_PLATES..=MAX_SURFACE_PLATES).contains(&want));
                assert_eq!(g.surface(sx, sz), want as i32);
                assert_eq!(g.material_at(sx, want as i32 - 1, sz), Some(GRASS));
                assert_eq!(g.material_at(sx, want as i32 - 4, sz), Some(DIRT));
                assert_eq!(g.material_at(sx, want as i32 - 10, sz), Some(STONE));
                assert_eq!(g.water_top(sx, sz), None);
            }
        }
    }
}

#[test]
fn heights_are_continuous_across_region_seam() {
    // The height field is a function of world stud coordinates: the last column of region 0
    // and the first of region 1 differ by no more than neighbours inside a region do.
    let region = (REGION_CHUNKS * N) as i64;
    let mut worst_inside = 0i32;
    let mut worst_seam = 0i32;
    for z in 0..region {
        let h = |x: i64| surface_plates(x, z) as i32;
        worst_seam = worst_seam.max((h(region - 1) - h(region)).abs());
        worst_inside = worst_inside.max((h(region / 2) - h(region / 2 + 1)).abs());
    }
    assert!(worst_seam <= worst_inside.max(3), "seam step {worst_seam} vs inside {worst_inside}");
}

#[test]
fn spawn_feature_stands_on_the_ground_in_the_seeded_region() {
    let s = seed(0, 0);
    let c = StudChunkCoords { x: 0, y: 0, z: 0 };
    assert_eq!(s.features.len(), 1);
    assert_eq!(s.features[0].0, c);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = game::Region::new(game::Rollback::new(None), Some(send), RegionCoords::new(0, 0), None);
    let features: usize = s.features[0].1.len();
    region.seed_stud_world(s);
    let chunk = region.data().stud_chunk_at(c).unwrap();
    // Every feature brick was placed (none overlapped the ground): the wall and tower colours
    // are there, and each column of the wall rests on something solid.
    let pristine = region_seed(RegionCoords::new(0, 0)).ground.iter().find(|(col, _)| (col.x, col.z) == (0, 0)).unwrap().1.pristine_chunk(0).unwrap();
    assert_eq!(chunk.len(), pristine.len() + features);
    let occ = chunk.occupancy();
    for x in 4..28u8 {
        let top = chunk.top_plate(x, 24).unwrap();
        assert_eq!(chunk.brick_at([x, top - 1, 24]).unwrap().material, 4, "wall top at x {x}");
        assert!((0..top as i32).all(|y| occ.get([x as i32, y, 24])), "no gap under the wall at x {x}");
    }
    assert_eq!(chunk.brick_at([27, chunk.top_plate(27, 27).unwrap() - 1, 27]).unwrap().material, 1);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn region_timing() {
    let t = std::time::Instant::now();
    let s = seed(0, 0);
    let recipe = t.elapsed();
    let t = std::time::Instant::now();
    let chunks: Vec<_> = s.ground.iter().flat_map(|(_, g)| g.surface_layers().filter_map(|y| g.pristine_chunk(y))).collect();
    let bricks: usize = chunks.iter().map(|c| c.len()).sum();
    eprintln!("recipe: {recipe:?}; pristine: {:?} for {} chunks, {bricks} bricks", t.elapsed(), chunks.len());
}
