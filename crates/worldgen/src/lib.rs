//! Deterministic world generation. A region's stud terrain
//! (`stud_gen::generate_stud_region`) is a pure function of region
//! coordinates (and the installed forged world): same coords → identical
//! output on every machine, which is what makes "cycle out = park, cycle in
//! = restore-or-regenerate" safe for the multi-region world.
//!
//! This file holds the column sampling every generator shares: a `Patch`
//! (the noise pipeline or a forged world) read at a point gives a
//! `ColSample` — surface, water, biome cover — in 1 m block units; `stud_gen`
//! quantises it to plates and studs.

mod noise;
mod spline;
mod density;
pub mod forge;
pub mod forged;
pub mod forged_tiles;
pub mod habitat;
pub mod islands;
pub mod pipeline;
pub mod material;
pub mod tile_map;
pub mod world_store;
pub mod stud_gen;
pub mod stud_trees;
pub mod stud_world;
pub mod trees;
mod slopes;

/// The world seed shared by every generator caller (server + offline
/// client) so they produce the SAME world. Kept in one place so the server and
/// client can never silently drift to different seeds.
// Picked by examples/seed_scan.rs (re-picked for FEATURE_SCALE = 0.5): dry
// flat spawn (y=77, sea=64) on a coastal plain, open sea ~0.5 km north and a
// mountain belt (peak y=261) ~1 km south — both inside the LOD horizon.
// Re-run the scan if terrain shaping changes.
pub const WORLD_SEED: u64 = 0x00D5_C0DE_007B;

use game::{Biome, BlockId, RegionCoords};

use crate::forged::ColumnClimate;
use crate::material::{GroundCover, Materials};
use crate::pipeline::{ProfileGrid, PROFILE_LATTICE_BLOCKS, SEA_TOP_BLOCK};

// Region span in blocks (the game's dial, as i32 for coordinate math).
const REGION_BLOCKS: i32 = game::REGION_BLOCKS as i32; // 64 at the test default
// Vertical scan bounds (blocks) of a procedural column. Terrain lives well
// inside this band (the density ramp clamps outside it). They bound surfaces
// only: the ground below a surface is the column's ground recipe, which has
// no lower bound.
const SCAN_TOP: i32 = 320;
const SCAN_BOTTOM: i32 = -64;
/// Scan bounds (blocks) of a forged (unbounded) column: the sea plus the
/// forge's tallest peaks (~3,500 m) with headroom, and the deepest sea floor.
const FORGED_SCAN_TOP: i32 = 4096;
const FORGED_SCAN_BOTTOM: i32 = -1024;
/// Blocks above the analytic surface (`ColumnProfile::surface_y`) where the
/// surface scan starts.
const SURFACE_SCAN_HEADROOM: i32 = 2;
/// Margin (blocks) a region or cell patch covers past its window on every
/// side, so the cliff rule can read the surfaces just outside it. One whole
/// lattice step keeps the window on the world-aligned profile lattice, so the
/// surfaces inside are bit-identical to a window without the margin.
const PATCH_MARGIN: i32 = PROFILE_LATTICE_BLOCKS as i32;
/// How far (blocks) a tree's blocks reach past its base column: the widest
/// crown plus the one block of leaf dither past it (`trees::raster`).
const TREE_REACH_BLOCKS: i32 = trees::MAX_CROWN_RADIUS_BLOCKS + trees::LEAF_REACH_PAST_CROWN_BLOCKS;
/// Surfaces (blocks) a tree query reads past its window: the trees whose
/// crowns reach in, and their placement neighbours — a candidate sits
/// anywhere in its lattice cell, one cell out from a query cell that may
/// itself start up to a cell before the query.
const TREE_RING_BLOCKS: i32 = TREE_REACH_BLOCKS + 2 * trees::TREE_LATTICE_BLOCKS;
/// Patch margin (blocks) with trees on: the tree ring, rounded up to the
/// profile lattice (so surfaces stay bit-identical), plus `PATCH_MARGIN`.
const TREE_PATCH_MARGIN: i32 = PATCH_MARGIN
    + (TREE_RING_BLOCKS + PROFILE_LATTICE_BLOCKS as i32 - 1) / PROFILE_LATTICE_BLOCKS as i32
        * PROFILE_LATTICE_BLOCKS as i32;
// Deepest non-stone material_at depth (dirt/sand band). Keep in sync with
// material.rs's `depth_below_surface <= 3` bands.
const MAX_SOIL_DEPTH: i32 = 3;

/// Meter coordinate of a block center at world block index `b`.
fn block_center_m(b: i32) -> f64 {
    b as f64 + 0.5
}

/// Where a block column's surface is read: the surface block is the highest
/// whose density at `by + 0.5` is positive. Density falls monotonically with
/// height, so every block under it is solid. (Stud recipes quantise the same
/// profile to plates, `stud_gen::surface_plate`; this block surface is what
/// covers, snow lines and tree placement are decided on.)
const SURFACE_TEST_M: f64 = 0.5;

/// The scan band `(top, bottom)` (blocks) of a profile.
fn scan_band(prof: &crate::pipeline::ColumnProfile) -> (i32, i32) {
    if prof.is_unbounded() {
        (FORGED_SCAN_TOP, FORGED_SCAN_BOTTOM)
    } else {
        (SCAN_TOP, SCAN_BOTTOM)
    }
}

/// Highest block that is at least half full (`SURFACE_TEST_M`) for a precomputed profile — the
/// surface block — or the band's bottom − 1 if the whole column is air.
///
/// The scan starts just above the analytic surface rather than at the band
/// top, so its cost does not grow with the band. Density falls monotonically
/// with height (every profile's `factor` is positive), so nothing above a
/// non-solid start block is solid and the result is the full top-down scan's
/// — bit-identical for procedural profiles. Should the start block be solid
/// anyway (the analytic surface is only exact inside the ramp), the scan
/// falls back to the band top.
fn surface_from_profile(prof: &crate::pipeline::ColumnProfile) -> i32 {
    let (top, bottom) = scan_band(prof);
    let solid = |by: i32| prof.density_at(by as f64 + SURFACE_TEST_M) > 0.0;
    // `as i32` saturates (NaN -> 0); the guard below covers both.
    let analytic = (prof.surface_y().ceil() as i32).saturating_add(SURFACE_SCAN_HEADROOM);
    let mut by = top.min(analytic);
    if by < top && solid(by) {
        by = top;
    }
    while by >= bottom {
        if solid(by) {
            return by;
        }
        by -= 1;
    }
    bottom - 1
}

/// Whether column `c` has ground at all (a surface inside its scan band).
fn has_ground(c: &ColSample) -> bool {
    c.s >= if c.unbounded { FORGED_SCAN_BOTTOM } else { SCAN_BOTTOM }
}

/// One block column as generation builds it (`column_sample`).
#[derive(Clone, Copy)]
struct ColSample {
    /// The ground's surface block (soil over rock).
    s: i32,
    /// Topmost water block (`SEA_TOP_BLOCK` where the sea is all there is).
    water_top: i32,
    /// Topmost solid block: `s`, or the top of the glacier ice over it.
    top: i32,
    /// Biome ground cover (forged worlds with biome blocks); `None` keeps
    /// `solid_material` (the procedural world, bit-identical).
    cover: Option<GroundCover>,
    /// Whether the cover lets a tree stand on it.
    trees: bool,
    /// Whether the profile is a forged (unbounded) one.
    unbounded: bool,
}

/// World block column `(bx, bz)` of `patch`.
fn column_sample(patch: &dyn Patch, mats: &Materials, bx: i32, bz: i32) -> ColSample {
    point_sample(patch, mats, block_center_m(bx), block_center_m(bz))
}

/// The column at world metres `(x, z)` of `patch`.
fn point_sample(patch: &dyn Patch, mats: &Materials, x: f64, z: f64) -> ColSample {
    profile_sample(patch, mats, x, z).0
}

/// [`point_sample`] plus the profile it was read from.
fn profile_sample(patch: &dyn Patch, mats: &Materials, x: f64, z: f64) -> (ColSample, crate::pipeline::ColumnProfile) {
    // The lake flag is what frozen water tops were decided on (ice on cold lakes
    // and seas, `ColumnClimate::ice_blocks`); stud recipes hold liquid water only.
    let (prof, water_top, _lake) = patch.sample(x, z);
    let s = surface_from_profile(&prof);
    debug_assert!(s >= scan_band(&prof).1, "surface below the scan at ({x}, {z})");
    let unbounded = prof.is_unbounded();
    let plain = ColSample { s, water_top, top: s, cover: None, trees: true, unbounded };
    if !mats.biome.enabled() {
        return (plain, prof);
    }
    let sample = match patch.climate(x, z) {
        None => plain,
        Some(c) => biome_column(mats, &c, plain),
    };
    (sample, prof)
}

/// Biomes that never carry trees: frozen tundra, glacier, or an open mud
/// flat (marsh). Swamps and mangrove swamps are forested wetland (DF) and
/// keep their trees on mud. Deserts, wastelands and badlands carry only
/// their sparse cacti (`trees::NATIVE_ONLY_BIOMES`, `BIOME_TREE_DENSITY`).
fn treeless(b: Biome) -> bool {
    matches!(b, Biome::Marsh | Biome::Tundra | Biome::Glacier)
}

/// Forest biomes: trees stand on their snow (a boreal forest floor), where
/// snow elsewhere — alpine meadows, snowy grassland — is treeless.
fn forest(b: Biome) -> bool {
    matches!(
        b,
        Biome::Taiga
            | Biome::ConiferForest
            | Biome::BroadleafForest
            | Biome::TropicalConiferForest
            | Biome::TropicalBroadleafForest
            | Biome::Swamp
            | Biome::MangroveSwamp
    )
}

/// `plain` (a forged column's ground and water) dressed by its climate:
/// ground cover per biome, snow above the snow line (patches in tundra),
/// salt flats, glacier ice over dry glacier ground.
fn biome_column(mats: &Materials, c: &ColumnClimate, plain: ColSample) -> ColSample {
    let ColSample { s, water_top, .. } = plain;
    let dry = s >= water_top;
    let ground_m = (s - SEA_TOP_BLOCK) as f32; // top of block `s`, m above sea
    // Snow lies on dry ground only (a cold sea's snow line is far below
    // its floor).
    let snowy = dry
        && match c.biome {
            Biome::Tundra => c.snow_patch,
            Biome::Glacier => false,
            _ => ground_m > c.snow_line_m,
        };
    let salt_flat = dry && c.salt_flat && !snowy;
    let cover = if salt_flat {
        mats.salt_flat()
    } else if !dry {
        // Under water the floor is the sea's (or a lake's / river's) floor,
        // whatever biome the dither lands there: `material_at`'s sand.
        mats.ground_cover(Biome::Ocean, false, s)
    } else {
        mats.ground_cover(c.biome, snowy, s)
    };
    let glacier = if dry { c.glacier_blocks() } else { 0 };
    ColSample {
        top: s + glacier,
        cover: Some(cover),
        trees: glacier == 0 && !salt_flat && !treeless(c.biome) && (!snowy || forest(c.biome)),
        ..plain
    }
}

/// Material of the solid block at `by` (at or under `c.top`) of column `c`:
/// glacier ice over the ground, else the cover (or `solid_material` without
/// one). `BlockId::AIR` = bare rock: the column's ground-recipe rock.
fn column_material(mats: &Materials, c: &ColSample, by: i32) -> BlockId {
    if by > c.s {
        return mats.biome.glacier_ice;
    }
    let depth = c.s - by;
    let Some(cover) = c.cover else {
        return solid_material(mats, depth, by, c.s, c.water_top);
    };
    if depth <= MAX_SOIL_DEPTH && c.s < c.water_top && c.water_top > SEA_TOP_BLOCK {
        mats.sand // lake and river beds
    } else if depth == 0 {
        cover.surface
    } else if depth <= cover.soil_depth {
        cover.soil
    } else {
        BlockId::AIR
    }
}

/// Deepest soil (blocks) of column `c`: the band reaches below it.
fn soil_depth(c: &ColSample) -> i32 {
    c.cover.map_or(MAX_SOIL_DEPTH, |cover| cover.soil_depth.max(MAX_SOIL_DEPTH))
}

/// The surface a tree may stand on, from a `column_sample` and whether it
/// is a step edge (`slopes::step_edge`): dry, no step edge (where voxel
/// terrain carved a slope — tree placement is unchanged from those worlds)
/// front) and a cover that grows trees. `None` = no tree.
fn tree_ground(c: ColSample, step_edge: bool) -> Option<i32> {
    (!step_edge && c.s >= c.water_top && c.trees).then_some(c.s)
}

/// Where trees come from: the habitat and the placement seed.
#[derive(Clone, Copy)]
struct TreeSource<'a> {
    habitat: &'a dyn habitat::HabitatSource,
    seed: u64,
}

/// A `HabitatSource` that remembers its answers: placement asks again for
/// every candidate's neighbours.
struct MemoHabitat<'a> {
    inner: &'a dyn habitat::HabitatSource,
    memo: std::cell::RefCell<std::collections::HashMap<(u64, u64, i32), habitat::Habitat>>,
}

impl habitat::HabitatSource for MemoHabitat<'_> {
    fn habitat(&self, x_m: f64, z_m: f64, surface_block: i32) -> habitat::Habitat {
        let key = (x_m.to_bits(), z_m.to_bits(), surface_block);
        if let Some(h) = self.memo.borrow().get(&key) {
            return *h;
        }
        let h = self.inner.habitat(x_m, z_m, surface_block);
        self.memo.borrow_mut().insert(key, h);
        h
    }
}

/// Trees of region `coords` that can put a block into the world block window
/// `[x0, x1) x [z0, z1)`, sorted by id. A tree belongs to the region of its
/// base and never writes outside it, so the query stays inside the region.
fn trees_for_window(
    coords: RegionCoords,
    [x0, z0, x1, z1]: [i32; 4],
    trees_from: TreeSource,
    surface: &dyn Fn(i32, i32) -> Option<i32>,
) -> Vec<trees::TreeSeed> {
    let (rx, rz) = (coords.x * REGION_BLOCKS, coords.z * REGION_BLOCKS);
    let memo = MemoHabitat { inner: trees_from.habitat, memo: Default::default() };
    trees::trees_in(
        (x0 - TREE_REACH_BLOCKS).max(rx),
        (z0 - TREE_REACH_BLOCKS).max(rz),
        (x1 + TREE_REACH_BLOCKS).min(rx + REGION_BLOCKS),
        (z1 + TREE_REACH_BLOCKS).min(rz + REGION_BLOCKS),
        trees_from.seed,
        &memo,
        surface,
    )
}

/// The terrain of one region- or LOD-tile-sized window, whatever it comes
/// from: the noise pipeline, or a forged world file (`forged.rs`).
pub(crate) trait Patch {
    /// Column profile at world meters (x, z), the topmost WATER block of
    /// that column — `SEA_TOP_BLOCK` everywhere in a procedural world, higher
    /// over a forged world's lakes and rivers — and whether that water is a
    /// lake's.
    fn sample(&self, x: f64, z: f64) -> (crate::pipeline::ColumnProfile, i32, bool);

    /// The climate behind the ground cover at world meters (x, z): `None` in
    /// the procedural world, which keeps `material_at`.
    fn climate(&self, _x: f64, _z: f64) -> Option<ColumnClimate> {
        None
    }

    /// Stratigraphic offset (blocks) at world meters (x, z): the ground
    /// recipe reads rock at block Y `y` from `strata` at `s = y - geo`.
    fn geo(&self, _x: f64, _z: f64) -> i16 {
        0
    }

    /// Rock formations top-down by top `s` (blocks): plain stone unless the
    /// patch has real geology (a forged world).
    fn strata(&self, mats: &Materials) -> Vec<(i16, BlockId)> {
        vec![(i16::MAX, mats.stone)]
    }
}

struct ProceduralPatch(ProfileGrid);

impl Patch for ProceduralPatch {
    fn sample(&self, x: f64, z: f64) -> (crate::pipeline::ColumnProfile, i32, bool) {
        (self.0.profile_at(x, z), SEA_TOP_BLOCK, false)
    }
}

/// Patch origin and span (meters) for the window of `span` blocks at world
/// block `(x, z)` with `margin` blocks of patch margin on every side.
fn patch_window(x: i32, z: i32, span: i32, margin: i32) -> (f64, f64, f64) {
    ((x - margin) as f64, (z - margin) as f64, (span + 2 * margin) as f64)
}

/// Engine-metre rect `[min, max]` that every patch a job builds lies in,
/// for a block window at `(x, z)` of `span` blocks with `margin` blocks of
/// patch margin: the patch's lattice runs up to one lattice step past its
/// span (`ProfileGrid::from_fn`).
fn read_rect(x: f64, z: f64, span: f64, margin: i32) -> ([f64; 2], [f64; 2]) {
    let m = f64::from(margin);
    let far = span + m + PROFILE_LATTICE_BLOCKS as f64;
    ([x - m, z - m], [x + far, z + far])
}

/// Loads and pins the tiles of the installed forged world
/// ([`forged::active`]) that generating region `rc` reads — any window of it
/// through [`stud_gen::generate_stud_columns`] /
/// [`stud_gen::generate_stud_region`]. Hold the pins while
/// generating: the reads then never load, and a tile that cannot be loaded
/// is this `Err` (naming the tile), never terrain. Empty without a world or
/// for a whole one.
pub fn prefetch_region(rc: RegionCoords) -> Result<forged_tiles::TilePins, String> {
    forged::active().map_or_else(|| Ok(Default::default()), |world| prefetch_region_forged(world, rc))
}

/// [`prefetch_region`] for an explicit forged world.
pub fn prefetch_region_forged(world: &forged::ForgedWorld, rc: RegionCoords) -> Result<forged_tiles::TilePins, String> {
    // The region generator's margin (tree placement reads that far out); the
    // column windows' is inside it.
    let (x, z) = (f64::from(rc.x * REGION_BLOCKS), f64::from(rc.z * REGION_BLOCKS));
    let (min, max) = read_rect(x, z, f64::from(REGION_BLOCKS), TREE_PATCH_MARGIN);
    world.prefetch_rect(min, max)
}

/// Loads and pins the tiles of the installed forged world that building LOD
/// key `key` reads ([`stud_gen::stud_lod_tile`]); see [`prefetch_region`].
pub fn prefetch_lod(key: &game::lod::LodKey) -> Result<forged_tiles::TilePins, String> {
    forged::active().map_or_else(|| Ok(Default::default()), |world| prefetch_lod_forged(world, key))
}

/// [`prefetch_lod`] for an explicit forged world.
pub fn prefetch_lod_forged(world: &forged::ForgedWorld, key: &game::lod::LodKey) -> Result<forged_tiles::TilePins, String> {
    // `stud_lod_tile_forged`'s patch.
    let (ox, oz, span) = lod_window(key);
    let (min, max) = read_rect(f64::from(ox), f64::from(oz), f64::from(span), PATCH_MARGIN);
    world.prefetch_rect(min, max)
}

/// Material of a solid block: sand under standing water (lake and river
/// beds, like the sea floor), else the depth/height rule.
fn solid_material(mats: &Materials, depth: i32, by: i32, surface: i32, water_top: i32) -> BlockId {
    if depth <= MAX_SOIL_DEPTH && surface < water_top && water_top > SEA_TOP_BLOCK {
        mats.sand
    } else {
        mats.material_at(depth, by)
    }
}

/// World block origin and span of a LOD key.
fn lod_window(key: &game::lod::LodKey) -> (i32, i32, i32) {
    let origin = key.origin_region();
    (origin.x * REGION_BLOCKS, origin.z * REGION_BLOCKS, key.region_span() * REGION_BLOCKS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::TerrainPipeline;

    /// `treeless` (no trees dressed onto the column) and
    /// `trees::BIOME_TREE_DENSITY` (no trees placed) are two sources of truth:
    /// a land biome is treeless exactly when its density is 0, water biomes
    /// have density 0, and the table lists every biome once.
    #[test]
    fn treeless_biomes_match_zero_tree_density() {
        for b in Biome::ALL {
            let listed = crate::trees::placement::BIOME_TREE_DENSITY.iter().filter(|(x, _)| *x == b).count();
            assert_eq!(listed, 1, "{b:?} listed {listed} times in BIOME_TREE_DENSITY");
            let zero = crate::trees::placement::biome_tree_density(Some(b)) == 0.0;
            if game::region_type(b, 50).is_water() {
                assert!(zero, "water biome {b:?} has trees");
            } else {
                assert_eq!(treeless(b), zero, "{b:?}: treeless() and BIOME_TREE_DENSITY disagree");
            }
        }
    }

    /// The engine's `SINGLE_BLOCK_TREES` (which blocks are trunk wood) names
    /// exactly worldgen's `Species::single_block` species.
    #[test]
    fn single_block_trees_match_species() {
        let mut species: Vec<&str> =
            crate::trees::SPECIES.iter().filter(|s| s.single_block).map(|s| s.name).collect();
        let mut engine = game::SINGLE_BLOCK_TREES.to_vec();
        species.sort_unstable();
        engine.sort_unstable();
        assert_eq!(species, engine);
    }

    #[test]
    fn analytic_surface_scan_matches_the_full_band_scan() {
        // The old scan: every block of the band, top down.
        let full = |prof: &crate::pipeline::ColumnProfile| {
            let (top, bottom) = scan_band(prof);
            (bottom..=top).rev().find(|&by| prof.density_at(by as f64 + SURFACE_TEST_M) > 0.0).unwrap_or(bottom - 1)
        };
        // Procedural columns: sea, lowland and mountains, every 37 m over 30 km.
        let p = TerrainPipeline::new(WORLD_SEED);
        let mut tops = std::collections::BTreeSet::new();
        for i in 0..800 {
            let (x, z) = ((i % 40) as f64 * 37.0 * 20.0, (i / 40) as f64 * 37.0 * 20.0);
            let prof = p.column(x, z);
            let s = surface_from_profile(&prof);
            assert_eq!(s, full(&prof), "procedural column at ({x}, {z})");
            tops.insert(s);
        }
        assert!(tops.len() > 20, "a spread of procedural surfaces: {tops:?}");
        // Forged columns, from the deep sea floor to above the old band top.
        for surface_y in [-1100.0, -900.5, 0.3, 64.25, 64.8, 319.9, 320.6, 2563.0, 4000.4, 4200.0] {
            let prof = crate::pipeline::ColumnProfile::from_surface(surface_y);
            assert_eq!(surface_from_profile(&prof), full(&prof), "forged surface {surface_y}");
        }
    }

}
