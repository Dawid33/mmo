//! Far terrain of stud worlds (`stud_gen::stud_lod_tile`): the recipe point-sampled on
//! each LOD key's cell-corner lattice.
use game::lod::LodKey;
use game::{
    stud_material, RegionCoords, StudColumnGround, StudLodSample, StudLodTile, REGION_BLOCKS, REGION_CHUNKS,
    STUD_CHUNK_STUDS, STUD_LOD_MIN_FACTOR,
};
use worldgen::stud_gen::*;

const N: usize = STUD_CHUNK_STUDS;
const SEED: u64 = 7;

type Region = Vec<(game::ColumnCoords, StudColumnGround)>;

fn region(x: i32, z: i32) -> Region {
    generate_stud_columns(RegionCoords::new(x, z), 0..REGION_CHUNKS, 0..REGION_CHUNKS, SEED)
}

/// What the recipes of `r` hold at region-local stud column `(sx, sz)`.
fn recipe_sample(r: &Region, sx: usize, sz: usize) -> StudLodSample {
    let g = &r.iter().find(|(c, _)| (c.x, c.z) == (sx / N, sz / N)).unwrap().1;
    let (x, z) = (sx % N, sz % N);
    let surface = g.surface(x, z);
    StudLodSample::of_column(surface, g.material_at(x, surface - 1, z).unwrap(), g.water_top(x, z))
}

fn samples(key: &LodKey) -> (usize, Vec<StudLodSample>) {
    let tile = stud_lod_tile(key, SEED);
    assert_eq!(tile.side, StudLodTile::side_for(key));
    (usize::from(tile.side) + 1, tile.decode().expect("a generated tile decodes"))
}

/// A factor whose key is one region.
fn region_factor() -> u32 {
    let f = game::stud_ring_table(game::lod::LOD_RING_TABLE)[0].1;
    assert_eq!(LodKey { factor: f, coords: [0, 0] }.region_span(), 1);
    f
}

/// A region with sea in it: the one holding the first wet sample of a coarse tile
/// east of the origin (seed 7 has a shore there).
fn shore_region() -> RegionCoords {
    let key = LodKey::for_region(128, RegionCoords::new(4, 0));
    let (n, coarse) = samples(&key);
    let first = coarse.iter().position(|s| s.has_water()).expect("sea in the coarse tile east of the origin");
    let (cx, cz) = (first % n, first / n);
    let cells_per_region = (n - 1) / key.region_span() as usize;
    let within = |c: usize| (c / cells_per_region).min(key.region_span() as usize - 1) as i32;
    let o = key.origin_region();
    RegionCoords::new(o.x + within(cx), o.z + within(cz))
}

/// Every lattice point of a tile is the recipe of its stud column — top, top material,
/// standing water — over an inland region and one on the shore; the last row and column
/// are the neighbouring regions' first.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_tile_is_the_recipe_at_its_lattice_points() {
    let factor = region_factor();
    let cell = (factor / STUD_LOD_MIN_FACTOR) as usize;
    let (mut wet, mut dry) = (0, 0);
    for rc in [RegionCoords::new(0, 0), shore_region()] {
        let (rx, rz) = (rc.x, rc.z);
        let (here, east, south, corner) = (region(rx, rz), region(rx + 1, rz), region(rx, rz + 1), region(rx + 1, rz + 1));
        let (n, tile) = samples(&LodKey::for_region(factor, rc));
        let side = n - 1;
        assert_eq!(side * cell, REGION_BLOCKS as usize * STUDS_PER_M as usize);
        for cz in 0..n {
            for cx in 0..n {
                let from = match (cx == side, cz == side) {
                    (false, false) => &here,
                    (true, false) => &east,
                    (false, true) => &south,
                    (true, true) => &corner,
                };
                let want = recipe_sample(from, (cx % side) * cell, (cz % side) * cell);
                let got = tile[cz * n + cx];
                assert_eq!(got, want, "region {rc:?}, lattice ({cx}, {cz})");
                assert!(stud_material(got.material).is_some());
                if got.has_water() {
                    wet += 1;
                    assert_eq!(i32::from(got.water), 64 * PLATES_PER_M, "the sea surface plate");
                    assert!(got.top < got.water);
                    assert_eq!(stud_material(got.material).unwrap().name, "sand", "the sea floor");
                } else {
                    dry += 1;
                }
            }
        }
    }
    assert!(wet > 0 && dry > 0, "the regions hold sea ({wet}) and land ({dry})");
}

/// The same tile every time, and the same sample whichever key or factor reads a
/// lattice point: neighbours share their border, coarser lattices are subsets.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tiles_are_deterministic_and_agree_across_keys_and_factors() {
    let factor = region_factor();
    let key = LodKey::for_region(factor, RegionCoords::new(0, 0));
    assert_eq!(stud_lod_tile(&key, SEED), stud_lod_tile(&key, SEED));
    assert_ne!(stud_lod_tile(&key, SEED), stud_lod_tile(&key, SEED + 1), "the seed matters");
    let (n, here) = samples(&key);
    let (_, east) = samples(&LodKey::for_region(factor, RegionCoords::new(1, 0)));
    let (_, north) = samples(&LodKey::for_region(factor, RegionCoords::new(0, -1)));
    for i in 0..n {
        assert_eq!(here[i * n + n - 1], east[i * n], "east border, row {i}");
        assert_eq!(here[i], north[(n - 1) * n + i], "north border, column {i}");
    }
    // Twice and four times coarser, whatever tile they are keyed by.
    for coarser in [factor * 2, factor * 4] {
        let ckey = LodKey::for_region(coarser, RegionCoords::new(0, 0));
        let (cn, coarse) = samples(&ckey);
        let step = (coarser / factor) as usize;
        // Lattice offset of region (0, 0) inside the coarse key, in fine cells.
        let origin = ckey.origin_region();
        let off = |o: i32| (-o) as usize * (n - 1);
        let mut compared = 0;
        for cz in 0..cn {
            for cx in 0..cn {
                let (fx, fz) = ((cx * step).checked_sub(off(origin.x)), (cz * step).checked_sub(off(origin.z)));
                let (Some(fx), Some(fz)) = (fx, fz) else { continue };
                if fx < n && fz < n {
                    assert_eq!(coarse[cz * cn + cx], here[fz * n + fx], "factor {coarser}, lattice ({cx}, {cz})");
                    compared += 1;
                }
            }
        }
        assert!(compared >= 4, "factor {coarser} shares lattice points with the region ({compared})");
    }
}

