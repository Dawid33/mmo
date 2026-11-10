//! Stud ground recipes from the procedural pipeline (`stud_gen.rs`).
use game::{stud_material, RegionCoords, StudColumnGround, REGION_CHUNKS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS};
use worldgen::stud_gen::*;

const N: usize = STUD_CHUNK_STUDS;
const SEED: u64 = 7;

fn region(x: i32, z: i32) -> Vec<(game::ColumnCoords, StudColumnGround)> {
    generate_stud_columns(RegionCoords::new(x, z), 0..REGION_CHUNKS, 0..REGION_CHUNKS, SEED)
}

fn column(all: &[(game::ColumnCoords, StudColumnGround)], x: usize, z: usize) -> &StudColumnGround {
    &all.iter().find(|(c, _)| (c.x, c.z) == (x, z)).unwrap().1
}

#[test]
fn every_worldgen_material_is_a_stud_material() {
    let m = stud_materials();
    let ids = [m.stone, m.dirt, m.grass, m.sand, m.bedrock, m.biome.snow, m.biome.ice, m.biome.glacier_ice, m.biome.mud];
    let more = [m.biome.peat, m.biome.gravel, m.biome.red_sand, m.biome.dry_grass, m.biome.podzol, m.biome.frozen_dirt, m.biome.salt];
    for id in ids.iter().chain(&more).chain(&m.rocks) {
        assert!(id.0 >= game::NATURAL_BASE && stud_material(id.0).is_some(), "{id:?}");
    }
}

#[test]
fn recipes_do_not_depend_on_the_window() {
    let rc = RegionCoords::new(0, 0);
    let all = region(0, 0);
    assert_eq!(all.len(), REGION_CHUNKS * REGION_CHUNKS);
    let part = generate_stud_columns(rc, 1..3, 2..3, SEED);
    assert_eq!(part.len(), 2);
    for (c, g) in &part {
        assert!(column(&all, c.x, c.z) == g, "column {c:?} differs between windows");
    }
    assert!(region(0, 0) == all, "deterministic");
}

#[test]
fn surfaces_are_continuous_across_chunk_and_region_edges() {
    let (a, b) = (region(0, 0), region(1, 0));
    let last = REGION_CHUNKS - 1;
    let mut worst = 0;
    for cz in 0..REGION_CHUNKS {
        for sz in 0..N {
            // Across the region seam, and across a chunk edge inside the region.
            let seam = (column(&a, last, cz).surface(N - 1, sz) - column(&b, 0, cz).surface(0, sz)).abs();
            let edge = (column(&a, 0, cz).surface(N - 1, sz) - column(&a, 1, cz).surface(0, sz)).abs();
            worst = worst.max(seam).max(edge);
        }
    }
    // Half a metre apart: a cliff could be more, a seam error would be metres.
    assert!(worst <= 2 * PLATES_PER_M, "worst step across an edge: {worst} plates");
}

#[test]
fn ground_has_cover_soil_rock_and_sea() {
    let sea = (64 * PLATES_PER_M, "sea surface plate (SEA_LEVEL 64 m)");
    let (mut dry, mut wet) = (0, 0);
    for rx in -1..=1 {
        for (_, g) in region(rx, 0) {
            for sz in (0..N).step_by(5) {
                for sx in (0..N).step_by(5) {
                    let s = g.surface(sx, sz);
                    assert!((-64 * PLATES_PER_M..=320 * PLATES_PER_M).contains(&s), "surface {s} outside the procedural band");
                    let name = |plate| stud_material(g.material_at(sx, plate, sz).unwrap()).unwrap().name;
                    assert!(g.material_at(sx, s, sz).is_none());
                    assert_eq!(name(s - 60), "stone");
                    assert_eq!(name(STUD_BEDROCK_PLATE - 1), "bedrock");
                    match g.water_top(sx, sz) {
                        Some(w) => {
                            wet += 1;
                            assert_eq!(w, sea.0, "{}", sea.1);
                            assert!(s < w);
                            assert_eq!(name(s - 1), "sand");
                        }
                        None => {
                            dry += 1;
                            assert!(s >= sea.0);
                            assert!(["grass", "sand"].contains(&name(s - 1)), "cover {}", name(s - 1));
                            assert!(["dirt", "sand"].contains(&name(s - 1 - COVER_PLATES as i32)));
                        }
                    }
                }
            }
        }
    }
    assert!(dry > 0, "no dry land in three regions (dry {dry}, wet {wet})");
}

#[test]
fn a_generated_region_seeds_a_walkable_sim() {
    let seed = generate_stud_region(RegionCoords::new(0, 0), SEED);
    let layers: Vec<_> = seed.ground.iter().map(|(_, g)| g.surface_layers()).collect();
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = game::Region::new(game::Rollback::new(None), Some(send), RegionCoords::new(0, 0), None);
    let ground = seed.ground.clone();
    region.seed_stud_world(seed);
    for ((c, g), layers) in ground.iter().zip(layers) {
        // The chunk holding each column's top plate exists and tops out at the recipe's surface.
        for (sx, sz) in [(0, 0), (17, 9), (31, 31)] {
            let s = g.surface(sx, sz);
            let y = (s - 1).div_euclid(STUD_CHUNK_PLATES as i32);
            assert!(layers.contains(&y));
            let chunk = region.data().stud_chunk_at(game::StudChunkCoords { x: c.x, y, z: c.z }).expect("surface chunk seeded");
            // The ground's top: a tree may stand over the column (bricks with a tree id).
            let top = chunk
                .bricks()
                .filter(|b| b.data == 0)
                .filter(|b| {
                    let size = b.size();
                    (b.min[0]..b.min[0] + size[0]).contains(&(sx as u8)) && (b.min[2]..b.min[2] + size[2]).contains(&(sz as u8))
                })
                .map(|b| (b.min[1] + b.size()[1]) as i32)
                .max()
                .expect("column has bricks");
            assert_eq!(y * STUD_CHUNK_PLATES as i32 + top, s, "column {sx},{sz} of {c:?}");
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn region_cost() {
    let t = std::time::Instant::now();
    let seed = generate_stud_region(RegionCoords::new(0, 0), SEED);
    let recipe = t.elapsed();
    let t = std::time::Instant::now();
    let chunks: Vec<_> = seed.ground.iter().flat_map(|(_, g)| g.surface_layers().filter_map(|y| g.pristine_chunk(y))).collect();
    let bricks: usize = chunks.iter().map(|c| c.len()).sum();
    eprintln!("recipes: {recipe:?}; pristine: {:?} for {} chunks, {bricks} bricks", t.elapsed(), chunks.len());
}

#[test]
#[ignore = "diagnostic: prints the brick mix of a generated region"]
fn brick_mix() {
    let seed = generate_stud_region(RegionCoords::new(0, 0), SEED);
    let mut mix: std::collections::BTreeMap<(bool, [u8; 3]), usize> = Default::default();
    let mut spans = Vec::new();
    for (_, g) in &seed.ground {
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for z in 0..N {
            for x in 0..N {
                lo = lo.min(g.surface(x, z));
                hi = hi.max(g.surface(x, z));
            }
        }
        spans.push(hi - lo);
        for y in g.surface_layers() {
            for b in g.pristine_chunk(y).iter().flat_map(|c| c.bricks()) {
                let mut s = b.size();
                if s[0] < s[2] {
                    s.swap(0, 2);
                }
                let filler = matches!(b.shape, game::StudShape::Filler { .. });
                *mix.entry((filler, if filler { [0, 0, 0] } else { s })).or_default() += 1;
            }
        }
    }
    eprintln!("surface span per chunk column (plates): {spans:?}");
    for ((filler, s), n) in mix {
        eprintln!("{} {s:?}: {n}", if filler { "filler" } else { "part  " });
    }
}
