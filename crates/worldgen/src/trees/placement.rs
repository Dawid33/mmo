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

/// Blocks a tree's leaves may reach past its crown radius (raster's leaf
/// dither); the seam rule keeps those inside the region too.
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
    /// `ID_CELL_SPAN / 2`-cell neighbourhood. On a world that wraps (a
    /// `LatticeWrap` of `period` cells, a whole number of 512 m) an id also
    /// repeats across the seam, `period mod ID_CELL_SPAN` cells apart at the
    /// nearest (`ID_CELL_SPAN` when that is 0): never under 64 cells, 512 m.
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

/// The tree lattice of a world that wraps east–west: lattice column `lx` and
/// `lx + period` are one cell. A cell's hashes and id are those of its column
/// folded into `[origin, origin + period)`; its position stays in the
/// caller's frame (the folded cell's, moved by whole periods).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatticeWrap {
    /// The map's first lattice column.
    pub origin: i32,
    /// Lattice columns around the world.
    pub period: i32,
}

impl LatticeWrap {
    /// `lx` folded into `[origin, origin + period)`.
    fn fold(self, lx: i32) -> i32 {
        self.origin + (lx - self.origin).rem_euclid(self.period)
    }
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
    wrap: Option<LatticeWrap>,
    habitat: &dyn HabitatSource,
    surface: &dyn Fn(i32, i32) -> Option<i32>,
) -> Option<Candidate> {
    // Hashes and the id read the folded column `hx`; the position stays at
    // `lx` (the folded cell's, moved by whole periods), in the caller's frame.
    let hx = wrap.map_or(lx, |w| w.fold(lx));
    let l = TREE_LATTICE_BLOCKS;
    let x = lx * l + (hash01(seed, hx, lz, SALT_X) * l as f64) as i32;
    let z = lz * l + (hash01(seed, hx, lz, SALT_Z) * l as f64) as i32;
    let sy = surface(x, z)?;
    let h = habitat.habitat(x as f64 + 0.5, z as f64 + 0.5, sy);
    let mut pick = hash01(seed, hx, lz, SALT_EXISTS) as f32;
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
    // Leaf dither reaches one block past the crown radius (`raster.rs`).
    let radius = SPECIES[species as usize].crown_radius_blocks + LEAF_REACH_PAST_CROWN_BLOCKS;
    let (rx, rz) = (x.rem_euclid(REGION_BLOCKS), z.rem_euclid(REGION_BLOCKS));
    if rx - radius < 0 || rx + radius >= REGION_BLOCKS || rz - radius < 0 || rz + radius >= REGION_BLOCKS {
        return None;
    }
    let age = ((hash01(seed, hx, lz, SALT_AGE) * (MAX_AGE as f64 + 1.0)) as u16).min(MAX_AGE);
    Some(Candidate {
        pos: [x, z],
        species,
        priority: hash01(seed, hx, lz, SALT_PRIORITY),
        seed: TreeSeed {
            base: [x, sy + 1, z],
            species,
            seed: hash64(seed, hx as i64 as u64, (lz as i64 as u64) ^ SALT_SEED),
            age,
            id: tree_id(hx, lz),
        },
    })
}

/// The species of the tree lattice cell `(lx, lz)` would hold standing at surface block `sy`, without
/// the spacing between neighbours `trees_in` settles: the world map's coarse levels, where a sample is
/// as wide as a cell or wider, colour by it (`surface::surface_tile_forged`).
pub(crate) fn cell_tree(lx: i32, lz: i32, seed: u64, wrap: Option<LatticeWrap>, habitat: &dyn HabitatSource, sy: i32) -> Option<u8> {
    candidate(lx, lz, seed, wrap, habitat, &|_, _| Some(sy)).map(|c| c.species)
}

fn too_close(a: &Candidate, b: &Candidate) -> bool {
    let r = SPECIES[a.species as usize].crown_radius_blocks.max(SPECIES[b.species as usize].crown_radius_blocks);
    let (dx, dz) = (a.pos[0] - b.pos[0], a.pos[1] - b.pos[1]);
    dx * dx + dz * dz < r * r
}

/// Every tree whose base lies in the half-open world block window
/// `[x0, x1) x [z0, z1)`, sorted by id. `surface` gives the surface block y of
/// a world column, `None` for water. `wrap`: the lattice of a world that
/// wraps east–west (`None`: it does not); the window may then run past the
/// seam and gets each cell's folded tree at the window's position.
#[allow(clippy::too_many_arguments)]
pub fn trees_in(
    x0: i32,
    z0: i32,
    x1: i32,
    z1: i32,
    seed: u64,
    wrap: Option<LatticeWrap>,
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
            let Some(c) = candidate(lx, lz, seed, wrap, habitat, surface) else { continue };
            if c.pos[0] < x0 || c.pos[0] >= x1 || c.pos[1] < z0 || c.pos[1] >= z1 {
                continue;
            }
            let beaten = (-1..=1).any(|dz| {
                (-1..=1).any(|dx| {
                    (dx, dz) != (0, 0)
                        && candidate(lx + dx, lz + dz, seed, wrap, habitat, surface)
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

