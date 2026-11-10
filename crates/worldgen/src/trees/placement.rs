//! Deterministic tree placement. The world is a lattice of `TREE_LATTICE_BLOCKS`
//! cells; each cell has at most one candidate tree, a pure function of
//! `(seed, cell)` and the ground there. Candidates compete with their 3x3
//! neighbours, so the result never depends on the window asked for.
//!
//! Only integer math, + - * / and floor (the `noise.rs` determinism policy).

use super::species::SPECIES;
use crate::habitat::HabitatSource;
use crate::REGION_BLOCKS;
use game::Biome;

/// Side of one placement cell, blocks.
pub const TREE_LATTICE_BLOCKS: i32 = 8;
/// Oldest tree age (the age is a growth stage in `0..=MAX_AGE`).
pub const MAX_AGE: u16 = 100;
/// Widest crown over all species (checked against the table in a test).
pub const MAX_CROWN_RADIUS_BLOCKS: i32 = 8;
/// Tallest tree over all species (checked against the table in a test).
pub const MAX_TREE_HEIGHT_BLOCKS: i32 = 32;

/// Tree density per biome (forged worlds): multiplies every species'
/// placement chance on a column of that biome. Forests 1, jungle denser,
/// open land sparse, deserts a cactus here and there; biomes whose ground
/// carries no trees (`lib.rs` `treeless`) 0. Every biome is listed once.
pub const BIOME_TREE_DENSITY: [(Biome, f32); 20] = [
    (Biome::Ocean, 0.0),
    (Biome::FrozenOcean, 0.0),
    (Biome::Lake, 0.0),
    (Biome::Glacier, 0.0),
    (Biome::Tundra, 0.0),
    (Biome::Taiga, 1.0),
    (Biome::ConiferForest, 1.0),
    (Biome::BroadleafForest, 1.0),
    (Biome::TropicalConiferForest, 1.0),
    (Biome::TropicalBroadleafForest, 1.4),
    (Biome::Grassland, 0.05),
    (Biome::Savanna, 0.15),
    (Biome::Shrubland, 0.1),
    (Biome::SandDesert, 0.02),
    (Biome::RockyWasteland, 0.02),
    (Biome::Badlands, 0.02),
    (Biome::Marsh, 0.0),
    (Biome::Swamp, 0.8),
    (Biome::MangroveSwamp, 1.0),
    (Biome::Mountain, 0.6),
];

/// Biomes that grow only the species listing them (`Species::biomes`), not
/// the any-biome ones (oak, pine, birch): deserts grow cactus, jungle and
/// mangrove their tropical trees.
pub const NATIVE_ONLY_BIOMES: [Biome; 5] =
    [Biome::SandDesert, Biome::RockyWasteland, Biome::Badlands, Biome::TropicalBroadleafForest, Biome::MangroveSwamp];

/// `BIOME_TREE_DENSITY` of `biome`; 1 with no biome (the procedural world).
pub fn biome_tree_density(biome: Option<Biome>) -> f32 {
    biome.map_or(1.0, |b| BIOME_TREE_DENSITY.iter().find(|(x, _)| *x == b).map_or(0.0, |&(_, d)| d))
}

/// Whether species `s` may grow on a column of `biome`.
fn eligible(s: &super::Species, biome: Option<Biome>) -> bool {
    match biome {
        Some(b) if NATIVE_ONLY_BIOMES.contains(&b) => s.biomes.contains(&b),
        _ => s.grows_in(biome),
    }
}

/// Blocks a tree's leaves may reach past its crown radius (the leaf cells
/// laid around a cluster); the seam rule keeps those inside the region too.
pub const LEAF_REACH_PAST_CROWN_BLOCKS: i32 = 1;

const SALT_X: u64 = 1;
const SALT_Z: u64 = 2;
const SALT_EXISTS: u64 = 3;
const SALT_PRIORITY: u64 = 4;
const SALT_AGE: u64 = 5;
const SALT_SEED: u64 = 6;
/// Lattice bits per axis of a tree id (`game::TREE_ID_BITS` = 2 × 9).
const ID_AXIS_BITS: u32 = game::TREE_ID_BITS / 2;
/// Cells a tree id repeats after.
const ID_CELL_SPAN: i32 = 1 << ID_AXIS_BITS;
const ZERO_CELL_ID: u32 = (ID_CELL_SPAN as u32 / 2) | (ID_CELL_SPAN as u32 / 2) << ID_AXIS_BITS;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreeSeed {
    /// World block of the lowest trunk block, standing on the surface.
    pub base: [i32; 3],
    /// Index into `SPECIES`.
    pub species: u8,
    /// Seeds this tree's shape.
    pub seed: u64,
    pub age: u16,
    /// Non-zero, fits `game::TREE_ID_BITS`; unique within any
    /// `ID_CELL_SPAN / 2`-cell neighbourhood.
    pub id: u32,
}

/// Splitmix-style mix of `(seed, a, b)`; also seeds tree shapes (`skeleton.rs`).
pub(super) fn hash64(seed: u64, a: u64, b: u64) -> u64 {
    let mut z = seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform in [0, 1) of (seed, cell, salt).
fn hash01(seed: u64, lx: i32, lz: i32, salt: u64) -> f64 {
    (hash64(seed, lx as i64 as u64, (lz as i64 as u64) ^ salt.wrapping_mul(0xD6E8_FEB8_6659_FD93)) >> 11) as f64
        / (1u64 << 53) as f64
}

/// Tree id of lattice cell `(lx, lz)`: `lx mod 512 | (lz mod 512) << 9`. Cell
/// (0, 0) would give 0, which means "not a tree", so it takes `ZERO_CELL_ID`,
/// the id of the antipodal cell (256, 256): the id is shared by two cells 256
/// cells apart, however the 512 wrap is crossed (the all-ones id would be
/// adjacent to (0, 0) across the wrap).
pub fn tree_id(lx: i32, lz: i32) -> u32 {
    let id = (lx.rem_euclid(ID_CELL_SPAN) as u32) | (lz.rem_euclid(ID_CELL_SPAN) as u32) << ID_AXIS_BITS;
    if id == 0 { ZERO_CELL_ID } else { id }
}

struct Candidate {
    pos: [i32; 2],
    species: u8,
    priority: f64,
    seed: TreeSeed,
}

fn candidate(
    lx: i32,
    lz: i32,
    seed: u64,
    habitat: &dyn HabitatSource,
    surface: &dyn Fn(i32, i32) -> Option<i32>,
) -> Option<Candidate> {
    let l = TREE_LATTICE_BLOCKS;
    let x = lx * l + (hash01(seed, lx, lz, SALT_X) * l as f64) as i32;
    let z = lz * l + (hash01(seed, lx, lz, SALT_Z) * l as f64) as i32;
    let sy = surface(x, z)?;
    let h = habitat.habitat(x as f64 + 0.5, z as f64 + 0.5, sy);
    let mut pick = hash01(seed, lx, lz, SALT_EXISTS) as f32;
    // Each species' chance; 0 where it may not grow.
    let density = biome_tree_density(h.biome);
    let chance = SPECIES.map(|s| {
        if !eligible(&s, h.biome) {
            return 0.0;
        }
        let p = s.max_density
            * s.temperature_c.response(h.temperature_c)
            * s.moisture.response(h.moisture)
            * s.slope.response(h.slope)
            * s.soil_depth.response(h.soil_depth)
            * s.salinity.response(h.salinity);
        if h.biome.is_some() { p * density } else { p }
    });
    // Forged worlds: where the chances sum past 1 the cell always holds a
    // tree, and the species is picked in proportion to them (so a species
    // late in the table is not crowded out by earlier ones). The procedural
    // world keeps first-come order, bit for bit.
    if h.biome.is_some() {
        let total: f32 = chance.iter().sum();
        if total > 1.0 {
            pick *= total;
        }
    }
    let mut chosen = None;
    for (i, &p) in chance.iter().enumerate() {
        if pick < p {
            chosen = Some(i as u8);
            break;
        }
        pick -= p;
    }
    let species = chosen?;
    // A crown must not cross a region seam: regions generate independently.
    // Leaves reach one block past the crown radius.
    let radius = SPECIES[species as usize].crown_radius_blocks + LEAF_REACH_PAST_CROWN_BLOCKS;
    let (rx, rz) = (x.rem_euclid(REGION_BLOCKS), z.rem_euclid(REGION_BLOCKS));
    if rx - radius < 0 || rx + radius >= REGION_BLOCKS || rz - radius < 0 || rz + radius >= REGION_BLOCKS {
        return None;
    }
    let age = ((hash01(seed, lx, lz, SALT_AGE) * (MAX_AGE as f64 + 1.0)) as u16).min(MAX_AGE);
    Some(Candidate {
        pos: [x, z],
        species,
        priority: hash01(seed, lx, lz, SALT_PRIORITY),
        seed: TreeSeed {
            base: [x, sy + 1, z],
            species,
            seed: hash64(seed, lx as i64 as u64, (lz as i64 as u64) ^ SALT_SEED),
            age,
            id: tree_id(lx, lz),
        },
    })
}

fn too_close(a: &Candidate, b: &Candidate) -> bool {
    let r = SPECIES[a.species as usize].crown_radius_blocks.max(SPECIES[b.species as usize].crown_radius_blocks);
    let (dx, dz) = (a.pos[0] - b.pos[0], a.pos[1] - b.pos[1]);
    dx * dx + dz * dz < r * r
}

/// Every tree whose base lies in the half-open world block window
/// `[x0, x1) x [z0, z1)`, sorted by id. `surface` gives the surface block y of
/// a world column, `None` for water.
pub fn trees_in(
    x0: i32,
    z0: i32,
    x1: i32,
    z1: i32,
    seed: u64,
    habitat: &dyn HabitatSource,
    surface: &dyn Fn(i32, i32) -> Option<i32>,
) -> Vec<TreeSeed> {
    let mut out = Vec::new();
    if x1 <= x0 || z1 <= z0 {
        return out;
    }
    let l = TREE_LATTICE_BLOCKS;
    for lz in z0.div_euclid(l)..=(z1 - 1).div_euclid(l) {
        for lx in x0.div_euclid(l)..=(x1 - 1).div_euclid(l) {
            let Some(c) = candidate(lx, lz, seed, habitat, surface) else { continue };
            if c.pos[0] < x0 || c.pos[0] >= x1 || c.pos[1] < z0 || c.pos[1] >= z1 {
                continue;
            }
            let beaten = (-1..=1).any(|dz| {
                (-1..=1).any(|dx| {
                    (dx, dz) != (0, 0)
                        && candidate(lx + dx, lz + dz, seed, habitat, surface)
                            .is_some_and(|n| too_close(&c, &n) && n.priority > c.priority)
                })
            });
            if !beaten {
                out.push(c.seed);
            }
        }
    }
    out.sort_by_key(|t| t.id);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::habitat::Habitat;
    use game::Biome;

    struct Stub(Habitat);
    impl HabitatSource for Stub {
        fn habitat(&self, _: f64, _: f64, _: i32) -> Habitat {
            self.0
        }
    }
    const GOOD: Habitat = Habitat {
        rain: 1.5,
        temperature_c: 8.0,
        moisture: 1.5,
        slope: 0.1,
        soil_depth: 3.0,
        salinity: 0.0,
        biome: None,
    };
    fn land(_: i32, _: i32) -> Option<i32> {
        Some(100)
    }
    fn sorted(mut v: Vec<TreeSeed>) -> Vec<TreeSeed> {
        v.sort_by_key(|t| (t.base[0], t.base[2]));
        v
    }

    #[test]
    fn max_constants_match_the_table() {
        assert_eq!(MAX_CROWN_RADIUS_BLOCKS, SPECIES.iter().map(|s| s.crown_radius_blocks).max().unwrap());
        assert_eq!(MAX_TREE_HEIGHT_BLOCKS, SPECIES.iter().map(|s| s.max_height_blocks).max().unwrap());
    }

    fn species(name: &str) -> u8 {
        SPECIES.iter().position(|s| s.name == name).unwrap_or_else(|| panic!("no species {name}")) as u8
    }
    /// Trees per species of `t`, by name.
    fn census(t: &[TreeSeed]) -> std::collections::BTreeMap<&'static str, usize> {
        let mut out = std::collections::BTreeMap::new();
        for tr in t {
            *out.entry(SPECIES[tr.species as usize].name).or_insert(0) += 1;
        }
        out
    }
    /// A window of several regions, so seams don't dominate.
    const W: i32 = 4 * REGION_BLOCKS;
    /// Lattice cells in the `W` × `W` window.
    const CELLS: usize = ((W / TREE_LATTICE_BLOCKS) * (W / TREE_LATTICE_BLOCKS)) as usize;

    #[test]
    fn biome_density_table_covers_every_biome_once() {
        for b in Biome::ALL {
            assert_eq!(BIOME_TREE_DENSITY.iter().filter(|(x, _)| *x == b).count(), 1, "{b:?}");
        }
        assert_eq!(biome_tree_density(None), 1.0, "procedural: unscaled");
        assert_eq!(biome_tree_density(Some(Biome::BroadleafForest)), 1.0);
        assert_eq!(biome_tree_density(Some(Biome::Taiga)), 1.0);
        assert_eq!(biome_tree_density(Some(Biome::TropicalBroadleafForest)), 1.4);
        assert_eq!(biome_tree_density(Some(Biome::Grassland)), 0.05);
        assert_eq!(biome_tree_density(Some(Biome::SandDesert)), 0.02);
        // Every biome some species may grow in has a density, and the
        // biomes only listed species grow in all have one listing them.
        for b in NATIVE_ONLY_BIOMES {
            assert!(SPECIES.iter().any(|s| s.biomes.contains(&b)), "{b:?}");
        }
    }

    /// The procedural world (no biome) only ever grows the first three.
    #[test]
    fn procedural_world_grows_only_oak_pine_birch() {
        for (t, rain) in [(8.0, 1.5), (-1.0, 1.0), (14.0, 0.4)] {
            let h = Stub(Habitat { temperature_c: t, rain, moisture: rain, ..GOOD });
            let trees = trees_in(-W, -W, W, W, 4, &h, &land);
            assert!(!trees.is_empty());
            assert!(trees.iter().all(|t| t.species < 3), "{:?}", census(&trees));
        }
    }

    #[test]
    fn taiga_grows_spruce_not_oak() {
        let taiga = Habitat { temperature_c: -1.0, rain: 1.0, moisture: 1.4, biome: Some(Biome::Taiga), ..GOOD };
        let trees = trees_in(0, 0, W, W, 11, &Stub(taiga), &land);
        let c = census(&trees);
        assert_eq!(c.get("oak"), None, "{c:?}");
        let spruce = c.get("spruce").copied().unwrap_or(0);
        assert!(spruce > 0 && spruce >= c.get("pine").copied().unwrap_or(0), "spruce leads: {c:?}");
        assert!(trees.iter().all(|t| ["spruce", "pine", "birch"].contains(&SPECIES[t.species as usize].name)), "{c:?}");
        // The same climate with no biome (procedural) grows no spruce.
        let procedural = Habitat { biome: None, ..taiga };
        assert!(trees_in(0, 0, W, W, 11, &Stub(procedural), &land).iter().all(|t| t.species != species("spruce")));
    }

    #[test]
    fn savanna_grows_scattered_acacia() {
        let savanna = Habitat { temperature_c: 26.0, rain: 0.6, moisture: 0.8, biome: Some(Biome::Savanna), ..GOOD };
        let trees = trees_in(0, 0, W, W, 12, &Stub(savanna), &land);
        let c = census(&trees);
        assert!(trees.iter().all(|t| t.species == species("acacia")), "{c:?}");
        // Scattered: a few % of the lattice cells, far below a forest's.
        assert!(!trees.is_empty() && trees.len() * 10 < CELLS, "{} trees in {CELLS} cells", trees.len());
        let forest = Habitat { biome: Some(Biome::BroadleafForest), ..GOOD };
        let n_forest = trees_in(0, 0, W, W, 12, &Stub(forest), &land).len();
        assert!(trees.len() * 4 < n_forest, "savanna {} vs forest {n_forest}", trees.len());
    }

    #[test]
    fn hot_desert_grows_sparse_cactus_only() {
        let desert = Habitat { temperature_c: 28.0, rain: 0.1, moisture: 0.15, biome: Some(Biome::SandDesert), ..GOOD };
        let trees = trees_in(-W, -W, W, W, 13, &Stub(desert), &land);
        let c = census(&trees);
        assert!(trees.iter().all(|t| t.species == species("cactus")), "{c:?}");
        assert!(!trees.is_empty() && trees.len() * 25 < 4 * CELLS, "{} cacti in {} cells", trees.len(), 4 * CELLS);
        // A temperate tree's climate in the desert still grows only cactus.
        let lush = Habitat { biome: Some(Biome::SandDesert), ..GOOD };
        assert!(trees_in(-W, -W, W, W, 13, &Stub(lush), &land).iter().all(|t| t.species == species("cactus")));
    }

    #[test]
    fn jungle_is_denser_than_grassland() {
        let hot = Habitat { temperature_c: 26.0, rain: 2.0, moisture: 2.5, ..GOOD };
        let n = |b: Biome| trees_in(0, 0, W, W, 14, &Stub(Habitat { biome: Some(b), ..hot }), &land);
        let jungle = n(Biome::TropicalBroadleafForest);
        assert!(jungle.iter().any(|t| t.species == species("jungle")), "{:?}", census(&jungle));
        assert!(jungle.iter().all(|t| t.species != species("oak")), "{:?}", census(&jungle));
        assert!(jungle.len() > 10 * n(Biome::Grassland).len().max(1), "{:?}", census(&jungle));
    }

    #[test]
    fn palms_grow_on_the_tropical_coast_only() {
        let coast = Habitat {
            temperature_c: 27.0,
            rain: 1.4,
            moisture: 1.8,
            salinity: 0.8,
            biome: Some(Biome::TropicalBroadleafForest),
            ..GOOD
        };
        let trees = trees_in(0, 0, W, W, 15, &Stub(coast), &land);
        assert!(trees.iter().all(|t| t.species == species("palm")), "{:?}", census(&trees));
        assert!(!trees.is_empty());
        let inland = Habitat { salinity: 0.0, ..coast };
        assert!(trees_in(0, 0, W, W, 15, &Stub(inland), &land).iter().all(|t| t.species != species("palm")));
    }

    #[test]
    fn niche_is_a_trapezoid() {
        let n = SPECIES[0].temperature_c; // 4, 10, 20, 24
        assert_eq!(n.response(3.9), 0.0);
        assert_eq!(n.response(7.0), 0.5);
        assert_eq!(n.response(15.0), 1.0);
        assert_eq!(n.response(22.0), 0.5);
        assert_eq!(n.response(24.1), 0.0);
        assert_eq!(SPECIES[0].slope.response(0.0), 1.0);
    }

    #[test]
    fn placement_is_deterministic() {
        let h = Stub(GOOD);
        let a = trees_in(-40, -40, 120, 120, 7, &h, &land);
        assert!(a.len() > 20, "got {}", a.len());
        assert_eq!(a, trees_in(-40, -40, 120, 120, 7, &h, &land));
        assert_ne!(a, trees_in(-40, -40, 120, 120, 8, &h, &land));
    }

    #[test]
    fn placement_is_order_independent() {
        let h = Stub(GOOD);
        let whole = sorted(trees_in(-32, -32, 32, 32, 3, &h, &land));
        let mut parts = Vec::new();
        for (x, z) in [(-32, -32), (0, -32), (-32, 0), (0, 0)] {
            parts.extend(trees_in(x, z, x + 32, z + 32, 3, &h, &land));
        }
        assert_eq!(whole, sorted(parts));
    }

    #[test]
    fn no_tree_in_water_or_on_salt_beach() {
        let salty = Stub(Habitat { salinity: 1.0, ..GOOD });
        assert!(trees_in(0, 0, 128, 128, 1, &salty, &land).is_empty());
        let water = |_: i32, _: i32| None;
        assert!(trees_in(0, 0, 128, 128, 1, &Stub(GOOD), &water).is_empty());
    }

    #[test]
    fn trees_stand_on_the_surface() {
        let t = trees_in(0, 0, 64, 64, 1, &Stub(GOOD), &land);
        assert!(!t.is_empty() && t.iter().all(|t| t.base[1] == 101 && t.id != 0 && t.age <= MAX_AGE));
    }

    #[test]
    fn neighbours_respect_crown_spacing() {
        let t = trees_in(-64, -64, 192, 192, 5, &Stub(GOOD), &land);
        for (i, a) in t.iter().enumerate() {
            for b in &t[i + 1..] {
                let r = SPECIES[a.species as usize].crown_radius_blocks.max(SPECIES[b.species as usize].crown_radius_blocks);
                let (dx, dz) = (a.base[0] - b.base[0], a.base[2] - b.base[2]);
                assert!(dx * dx + dz * dz >= r * r, "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn no_crown_crosses_a_region_seam() {
        let t = trees_in(-REGION_BLOCKS, -REGION_BLOCKS, 2 * REGION_BLOCKS, 2 * REGION_BLOCKS, 9, &Stub(GOOD), &land);
        assert!(!t.is_empty());
        for tr in t {
            let r = SPECIES[tr.species as usize].crown_radius_blocks + LEAF_REACH_PAST_CROWN_BLOCKS;
            for c in [tr.base[0], tr.base[2]] {
                assert_eq!((c - r).div_euclid(REGION_BLOCKS), c.div_euclid(REGION_BLOCKS));
                assert_eq!((c + r).div_euclid(REGION_BLOCKS), c.div_euclid(REGION_BLOCKS));
            }
        }
    }

    #[test]
    fn tree_ids_are_distinct_around_the_origin_and_the_wrap_point() {
        // Any window narrower than ID_CELL_SPAN / 2 cells holds distinct ids.
        let h = ID_CELL_SPAN / 4 - 1;
        for (cx, cz) in [(0, 0), (ID_CELL_SPAN / 2, ID_CELL_SPAN / 2), (ID_CELL_SPAN - 1, ID_CELL_SPAN - 1), (-1, -1), (h, h)] {
            let mut seen = std::collections::HashSet::new();
            for lz in cz - h..=cz + h {
                for lx in cx - h..=cx + h {
                    assert!(seen.insert(tree_id(lx, lz)), "({lx}, {lz})");
                }
            }
        }
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn tree_ids_are_unique_over_a_full_period() {
        // Every id is held by one cell per period, except ZERO_CELL_ID, shared by
        // (0, 0) and (256, 256).
        let mut seen = std::collections::HashMap::new();
        for lz in 0..ID_CELL_SPAN {
            for lx in 0..ID_CELL_SPAN {
                *seen.entry(tree_id(lx, lz)).or_insert(0u32) += 1;
            }
        }
        assert!(!seen.contains_key(&0));
        assert!(seen.iter().all(|(&id, &n)| n == if id == ZERO_CELL_ID { 2 } else { 1 }));
    }

    #[test]
    fn tree_id_rule() {
        assert_eq!(tree_id(0, 0), ZERO_CELL_ID);
        assert_eq!(tree_id(ID_CELL_SPAN / 2, ID_CELL_SPAN / 2), ZERO_CELL_ID);
        assert_eq!(tree_id(1, 0), 1);
        assert_eq!(tree_id(3, 0), 3);
        assert_eq!(tree_id(0, 1), 512);
        assert_eq!(tree_id(-1, 0), 511);
        assert_eq!(tree_id(512, 512), ZERO_CELL_ID);
        assert_eq!(ZERO_CELL_ID, 256 | 256 << 9);
        assert!((0..50).all(|i| tree_id(i * 977, i * 1313) != 0 && tree_id(i, -i) <= game::TREE_ID_MASK));
    }
}
