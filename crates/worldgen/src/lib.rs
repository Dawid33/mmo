//! Deterministic world generation. `generate_region` is a pure function of
//! region coordinates: same coords → identical output on every machine,
//! which is what makes "cycle out = park, cycle in = restore-or-regenerate"
//! safe for the multi-region world.

mod noise;
mod spline;
mod density;
pub mod cache;
pub mod forge;
pub mod forged;
pub mod habitat;
pub mod islands;
pub mod pipeline;
pub mod material;
pub mod tile_map;
pub mod world_store;
pub mod trees;
mod slopes;

/// The world seed shared by every `generate_region` caller (server + offline
/// client) so they produce the SAME world. Kept in one place so the server and
/// client can never silently drift to different seeds.
// Picked by examples/seed_scan.rs (re-picked for FEATURE_SCALE = 0.5): dry
// flat spawn (y=77, sea=64) on a coastal plain, open sea ~0.5 km north and a
// mountain belt (peak y=261) ~1 km south — both inside the LOD horizon.
// Re-run the scan if terrain shaping changes.
pub const WORLD_SEED: u64 = 0x00D5_C0DE_007B;

use std::collections::BTreeMap;

use game::{
    Biome, Block, BlockId, BlockIndex, ChiselData, Chunk, ChunkBlocks, Column, ColumnCoords, RegionCoords, SubChunk,
    BLOCK_VOXELS, CHUNK_BLOCKS, CHUNK_BLOCK_COUNT, BRICK_VOXELS, BRICK_VOXEL_COUNT, REGION_CHUNKS,
};

use crate::forged::ColumnClimate;
use crate::material::{GroundCover, Materials};
use crate::pipeline::{ProfileGrid, TerrainPipeline, PROFILE_LATTICE_BLOCKS, SEA_TOP_BLOCK};

// Region span in blocks (the game's dial, as i32 for coordinate math).
const REGION_BLOCKS: i32 = game::REGION_BLOCKS as i32; // 64 at the test default
// Vertical scan bounds (blocks) of a procedural column. Terrain lives well
// inside this band (the density ramp clamps outside it). They bound surfaces
// only: the ground below a surface is the column's `ColumnGround` recipe,
// which has no lower bound.
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

/// Worldgen terrain is whole blocks and slopes only — never finer voxel
/// detail. The continuous surface is rounded to whole blocks at the block's
/// midpoint: the surface block is the highest whose density at `by + 0.5` is
/// positive. Density falls monotonically with height, so every block under it
/// is solid. A one-block rise then carves the higher column's top block into a
/// slope (`slopes::slope_for`) — an ordinary chisel, editable at voxel scale.
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
/// Shared by full-res and coarse generation, so any-solid LOD cells agree
/// with the generated blocks.
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
    /// Lowest water block that is ice; above `water_top` when none is.
    ice_from: i32,
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

/// The column at world metres `(x, z)` of `patch` (a block centre, or a
/// coarse LOD cell's).
fn point_sample(patch: &dyn Patch, mats: &Materials, x: f64, z: f64) -> ColSample {
    let (prof, water_top, lake) = patch.sample(x, z);
    let s = surface_from_profile(&prof);
    debug_assert!(s >= scan_band(&prof).1, "surface below the scan at ({x}, {z})");
    let unbounded = prof.is_unbounded();
    let plain = ColSample { s, water_top, top: s, ice_from: water_top + 1, cover: None, trees: true, unbounded };
    if !mats.biome.enabled() {
        return plain;
    }
    match patch.climate(x, z) {
        None => plain,
        Some(c) => biome_column(mats, &c, plain, lake),
    }
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
/// salt flats, glacier ice over dry glacier ground, ice on cold lake and sea
/// water (`lake` = the water is a lake's; above the sea and not a lake it is
/// a river, which stays liquid).
fn biome_column(mats: &Materials, c: &ColumnClimate, plain: ColSample, lake: bool) -> ColSample {
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
    let frozen = lake || water_top == SEA_TOP_BLOCK;
    let ice = if !dry && frozen { c.ice_blocks() } else { 0 };
    ColSample {
        top: s + glacier,
        ice_from: if ice > 0 { (water_top - ice + 1).max(s + 1) } else { water_top + 1 },
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

/// The water block at `by` (at or under `c.water_top`): ice from `ice_from` up.
fn water_material(mats: &Materials, c: &ColSample, by: i32) -> BlockId {
    if by >= c.ice_from { mats.biome.ice } else { mats.water }
}

/// The surface a tree may stand on, from a `column_sample` and whether it
/// carries a slope: dry, no slope (a trunk would hang over the slope's open
/// front) and a cover that grows trees. `None` = no tree.
fn tree_ground(c: ColSample, slope: bool) -> Option<i32> {
    (!slope && c.s >= c.water_top && c.trees).then_some(c.s)
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

/// Whether world block `(x, z)` lies in region `coords`.
fn in_region(coords: RegionCoords, x: i32, z: i32) -> bool {
    x.div_euclid(REGION_BLOCKS) == coords.x && z.div_euclid(REGION_BLOCKS) == coords.z
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

/// The rock of `strata` (top-down) at block Y `y` of a column with
/// stratigraphic offset `geo`: `game::ColumnGround::block_at`'s rule without
/// bedrock (coarse LOD shows bare rock with it).
fn strata_rock(strata: &[(i16, BlockId)], geo: i16, y: i32) -> BlockId {
    let s = y - i32::from(geo);
    strata.iter().rev().find(|(top, _)| i32::from(*top) > s).or(strata.first()).map_or(BlockId::AIR, |&(_, id)| id)
}

/// The formations of `strata` (top-down) that `s` in `lo_s..=hi_s` can hit
/// under `ColumnGround::block_at`'s rule, plus one more above and below.
fn footprint_strata(strata: &[(i16, BlockId)], lo_s: i32, hi_s: i32) -> Vec<(i16, BlockId)> {
    let holding = |s: i32| strata.partition_point(|&(top, _)| i32::from(top) > s).saturating_sub(1);
    let first = holding(hi_s).saturating_sub(1);
    let last = (holding(lo_s) + 1).min(strata.len() - 1);
    strata[first..=last].to_vec()
}

struct ProceduralPatch(ProfileGrid);

impl Patch for ProceduralPatch {
    fn sample(&self, x: f64, z: f64) -> (crate::pipeline::ColumnProfile, i32, bool) {
        (self.0.profile_at(x, z), SEA_TOP_BLOCK, false)
    }
}

/// The full REGION_CHUNKS x REGION_CHUNKS column grid for one region. Pure and
/// deterministic. With `VOXEL_WORLD_FILE` set the region comes from that
/// forged world and `seed` is ignored (see `forged.rs`).
pub fn generate_region(coords: RegionCoords, seed: u64, mats: Materials) -> Vec<(ColumnCoords, Column)> {
    generate_columns(coords, 0..REGION_CHUNKS, 0..REGION_CHUNKS, seed, mats)
}

/// The columns `xs × zs` (region-local chunk columns) of region `coords`:
/// exactly the same columns `generate_region` makes, however the region is
/// split into windows.
pub fn generate_columns(
    coords: RegionCoords,
    xs: std::ops::Range<usize>,
    zs: std::ops::Range<usize>,
    seed: u64,
    mats: Materials,
) -> Vec<(ColumnCoords, Column)> {
    let cb = CHUNK_BLOCKS as i32;
    let (x, z) = (coords.x * REGION_BLOCKS + xs.start as i32 * cb, coords.z * REGION_BLOCKS + zs.start as i32 * cb);
    let (ox, oz, span) = patch_window(x, z, xs.len().max(zs.len()) as i32 * cb, &mats);
    if let Some(world) = forged::active() {
        let trees_from = TreeSource { habitat: world, seed: world.artifact().seed };
        return columns_from_patch(&world.patch(ox, oz, span), coords, xs, zs, mats, trees_from);
    }
    let grid = TerrainPipeline::new(seed).profile_grid(ox, oz, span);
    let habitat = habitat::ProceduralHabitat::new(seed);
    columns_from_patch(&ProceduralPatch(grid), coords, xs, zs, mats, TreeSource { habitat: &habitat, seed })
}

/// Patch margin (blocks) past a window: wider when trees are on, which read
/// surfaces further out (`TREE_RING_BLOCKS`).
fn patch_margin(mats: &Materials) -> i32 {
    if mats.trees.enabled() { TREE_PATCH_MARGIN } else { PATCH_MARGIN }
}

/// Patch origin and span (meters) for the window of `span` blocks at world
/// block `(x, z)`, margin included.
fn patch_window(x: i32, z: i32, span: i32, mats: &Materials) -> (f64, f64, f64) {
    let m = patch_margin(mats);
    ((x - m) as f64, (z - m) as f64, (span + 2 * m) as f64)
}

/// `generate_region` for an explicit forged world (tests, tools).
pub fn generate_region_forged(
    world: &forged::ForgedWorld,
    coords: RegionCoords,
    mats: Materials,
) -> Vec<(ColumnCoords, Column)> {
    let (ox, oz, span) = patch_window(coords.x * REGION_BLOCKS, coords.z * REGION_BLOCKS, REGION_BLOCKS, &mats);
    let trees_from = TreeSource { habitat: world, seed: world.artifact().seed };
    columns_from_patch(&world.patch(ox, oz, span), coords, 0..REGION_CHUNKS, 0..REGION_CHUNKS, mats, trees_from)
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

/// One generated tree block: its block, and its voxel mask if partial.
struct PlacedTreeBlock {
    block: Block,
    chisel: Option<ChiselData>,
    wood: bool,
}

/// The tree blocks of the window `[x0, x1) x [z0, z1)` (world blocks), keyed
/// by world `(x, y, z)`. Where trees overlap, wood wins over leaves, else the
/// lower tree id: a block's covering trees are the same in every window, so
/// the result is too.
fn tree_blocks(
    coords: RegionCoords,
    window: [i32; 4],
    mats: &Materials,
    trees_from: TreeSource,
    surface: &dyn Fn(i32, i32) -> Option<i32>,
) -> BTreeMap<[i32; 3], PlacedTreeBlock> {
    let [x0, z0, x1, z1] = window;
    let mut out: BTreeMap<[i32; 3], PlacedTreeBlock> = BTreeMap::new();
    for tree in trees_for_window(coords, window, trees_from, surface) {
        let blocks = mats.trees.species[tree.species as usize];
        let sk = trees::skeleton(&trees::SPECIES[tree.species as usize], tree.seed, tree.age);
        for b in trees::rasterize(&sk) {
            let p = [tree.base[0] + b.offset[0], tree.base[1] + b.offset[1], tree.base[2] + b.offset[2]];
            if p[0] < x0 || p[0] >= x1 || p[2] < z0 || p[2] >= z1 || !in_region(coords, p[0], p[2]) {
                continue;
            }
            let (id, wood) = match b.part {
                trees::TreePart::Log => (blocks.log, true),
                trees::TreePart::Branch => (blocks.branch, true),
                trees::TreePart::Leaves => (blocks.leaves, false),
            };
            let chisel = b.chisel.map(|mask| {
                ChiselData::from_fn(vec![id], |vx, vy, vz| {
                    mask[vx + BLOCK_VOXELS * (vy + BLOCK_VOXELS * vz)].then_some(0)
                })
            });
            let placed = PlacedTreeBlock { block: Block::with_tree(id, tree.id, b.limb), chisel, wood };
            match out.entry(p) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(placed);
                }
                std::collections::btree_map::Entry::Occupied(mut e) => {
                    if wood && !e.get().wood {
                        e.insert(placed);
                    }
                }
            }
        }
    }
    out
}

/// The columns `xs × zs` (region-local chunk columns) of region `coords`,
/// sampled from `patch` — which must cover them plus `patch_margin(&mats)`.
/// Trees (when `mats.trees` is on) come from `trees_from`.
///
/// Every column carries a `ColumnGround` recipe for the ground below its band
/// (the sim reads missing chunks under the band from it), and the band's
/// solids deeper than soil are that recipe's blocks, so band and implied
/// ground never disagree. The band reaches down far enough that no air or
/// water block faces implied ground: past the footprint's deepest soil, and
/// to the lowest open block of every face-neighbouring block column (the
/// cliff rule — a cliff's face is materialised, not implied).
fn columns_from_patch(
    patch: &dyn Patch,
    coords: RegionCoords,
    xs: std::ops::Range<usize>,
    zs: std::ops::Range<usize>,
    mats: Materials,
    trees_from: TreeSource,
) -> Vec<(ColumnCoords, Column)> {
    let cb = CHUNK_BLOCKS as i32;
    let air = Block::new(BlockId::AIR);
    let stone = Block::new(mats.stone);
    let strata = patch.strata(&mats);
    let with_trees = mats.trees.enabled();

    // Per-block-column `column_sample` over the window plus a ring on every
    // side: the columns whose slope is read — one block out (the cliff rule's
    // neighbours), or `TREE_RING_BLOCKS` with trees on (placement's
    // surfaces) — plus one more block, their slope's neighbours. Index
    // `[bx + ring][bz + ring]`, window-local blocks.
    let ring = if with_trees { TREE_RING_BLOCKS + 1 } else { 2 };
    let (wx, wz) = (coords.x * REGION_BLOCKS + xs.start as i32 * cb, coords.z * REGION_BLOCKS + zs.start as i32 * cb);
    let (ww, wd) = (xs.len() as i32 * cb, zs.len() as i32 * cb);
    let (nx, nz) = ((ww + 2 * ring) as usize, (wd + 2 * ring) as usize);
    let mut samples = Vec::with_capacity(nx * nz);
    for i in 0..nx {
        for j in 0..nz {
            samples.push(column_sample(patch, &mats, wx + i as i32 - ring, wz + j as i32 - ring));
        }
    }
    let at = |bx: i32, bz: i32| samples[(bx + ring) as usize * nz + (bz + ring) as usize];
    // Each column's slope, over the ring minus its outer block (`None`
    // there), indexed like `samples`.
    let mut slope_cells = vec![None; nx * nz];
    for bx in 1 - ring..ww + ring - 1 {
        for bz in 1 - ring..wd + ring - 1 {
            slope_cells[(bx + ring) as usize * nz + (bz + ring) as usize] =
                slopes::slope_for(&at(bx, bz), |dx, dz| at(bx + dx, bz + dz));
        }
    }
    let slope_at = |bx: i32, bz: i32| {
        debug_assert!(
            (1 - ring..ww + ring - 1).contains(&bx) && (1 - ring..wd + ring - 1).contains(&bz),
            "slope read at window block ({bx}, {bz}), outside the decided ring"
        );
        slope_cells[(bx + ring) as usize * nz + (bz + ring) as usize]
    };

    // Tree blocks, bucketed per chunk column of the window.
    // `(lx, lz, block y, block)`, footprint-local.
    let mut tree_cols: Vec<Vec<(usize, usize, i32, PlacedTreeBlock)>> =
        (0..xs.len() * zs.len()).map(|_| Vec::new()).collect();
    if with_trees {
        let surface = |x: i32, z: i32| {
            let (bx, bz) = (x - wx, z - wz);
            assert!(
                (1 - ring..ww + ring - 1).contains(&bx) && (1 - ring..wd + ring - 1).contains(&bz),
                "tree placement read the surface at ({x}, {z}), outside the sampled ring"
            );
            tree_ground(at(bx, bz), slope_at(bx, bz).is_some())
        };
        for ([x, y, z], placed) in tree_blocks(coords, [wx, wz, wx + ww, wz + wd], &mats, trees_from, &surface) {
            let (bx, bz) = (x - wx, z - wz);
            let col = (bx / cb) as usize * zs.len() + (bz / cb) as usize;
            tree_cols[col].push(((bx % cb) as usize, (bz % cb) as usize, y, placed));
        }
    }

    let mut columns = Vec::with_capacity(xs.len() * zs.len());
    for cx in xs.clone() {
        for cz in zs.clone() {
            // This Column covers CHUNK_BLOCKS x CHUNK_BLOCKS blocks, from
            // window-local block (fx, fz).
            let fx = (cx - xs.start) as i32 * cb;
            let fz = (cz - zs.start) as i32 * cb;
            let col_trees = std::mem::take(&mut tree_cols[(cx - xs.start) * zs.len() + (cz - zs.start)]);

            // Every footprint has samples, so these only seed the min/max
            // (procedural results equal the old SCAN_TOP / SCAN_BOTTOM - 1 seeds:
            // `lo - MAX_SOIL_DEPTH` is always under SCAN_TOP).
            let mut lo = i32::MAX;
            let mut hi = i32::MIN;
            let mut open_lo = i32::MAX;
            let mut soil_max = MAX_SOIL_DEPTH;
            for lx in 0..cb {
                for lz in 0..cb {
                    let c = at(fx + lx, fz + lz);
                    lo = lo.min(c.s);
                    hi = hi.max(c.top.max(c.water_top)); // include the water column up to its surface
                    soil_max = soil_max.max(soil_depth(&c));
                    // Cliff rule: the lowest block a face neighbour leaves open
                    // (its slope's open quarters included).
                    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                        let (nbx, nbz) = (fx + lx + dx, fz + lz + dz);
                        let n = at(nbx, nbz);
                        open_lo = open_lo.min(if slope_at(nbx, nbz).is_some() { n.top } else { n.top + 1 });
                    }
                }
            }
            // The band reaches the highest tree block too.
            hi = col_trees.iter().fold(hi, |hi, t| hi.max(t.2));
            // Extend the band bottom past the shallowest surface in the footprint
            // so the deepest soil (dirt/sand) of the lowest column is still
            // materialized, and down to every open neighbour block.
            lo = (lo - soil_max).min(open_lo);

            // Materialize whole sub-chunks [base_sub_y ..= top_sub_y]; below is
            // the ground recipe, above air. Whole blocks, and a slope chisel
            // on the top block of a slope column.
            let base_sub_y = lo.div_euclid(cb);
            let top_sub_y = hi.div_euclid(cb);
            let n_sub = (top_sub_y - base_sub_y + 1) as usize;
            // Linear block grids + chisel maps per sub-chunk, made canonical
            // `Chunk`s once filled.
            let mut chunks: Vec<(Vec<Block>, BTreeMap<BlockIndex, ChiselData>)> =
                (0..n_sub).map(|_| (vec![Block::default(); CHUNK_BLOCK_COUNT], BTreeMap::new())).collect();

            let mut surface = vec![0i16; game::COLUMN_FOOTPRINT].into_boxed_slice();
            let mut geo = vec![0i16; game::COLUMN_FOOTPRINT].into_boxed_slice();
            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let (bx, bz) = (fx + lx as i32, fz + lz as i32);
                    // The visible top: a glacier's ice counts as ground
                    // (digging, implied ground and pathing see the ice top).
                    surface[lx * CHUNK_BLOCKS + lz] = at(bx, bz).top as i16;
                    geo[lx * CHUNK_BLOCKS + lz] = patch.geo(block_center_m(wx + bx), block_center_m(wz + bz));
                }
            }
            // Only the formations this footprint's ground can reach, from its
            // highest surface down to bedrock under its lowest.
            let (s_lo, s_hi) = (surface.iter().min().unwrap(), surface.iter().max().unwrap());
            let (g_lo, g_hi) = (geo.iter().min().unwrap(), geo.iter().max().unwrap());
            let lo_s = i32::from(*s_lo) - game::UNDERGROUND_DEPTH - i32::from(*g_hi);
            let hi_s = i32::from(*s_hi) - i32::from(*g_lo);
            let ground = game::ColumnGround {
                surface,
                geo,
                strata: footprint_strata(&strata, lo_s, hi_s),
                bedrock: mats.bedrock,
                band_base_sub_y: base_sub_y,
            };

            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let c = at(fx + lx as i32, fz + lz as i32);
                    let slope = slope_at(fx + lx as i32, fz + lz as i32);
                    let soil = soil_depth(&c);
                    for by in base_sub_y * cb..=hi {
                        let sub_i = (by.div_euclid(cb) - base_sub_y) as usize;
                        let local_by = by.rem_euclid(cb) as usize;
                        let bi = BlockIndex::from_xyz(lx, local_by, lz);
                        let (blocks, chisel) = &mut chunks[sub_i];
                        let depth = c.s - by;
                        if depth > soil {
                            // Below the soil: the recipe's rock (or bedrock).
                            blocks[bi.linear()] = ground.block_at(lx, lz, by);
                        } else if by <= c.top {
                            // Solid: whole blocks, the top one a slope on a
                            // slope column; bare rock is the recipe's.
                            let block = match column_material(&mats, &c, by) {
                                BlockId::AIR => ground.block_at(lx, lz, by),
                                mat => Block::new(mat),
                            };
                            blocks[bi.linear()] = block;
                            if let Some(shape) = slope.filter(|_| by == c.top) {
                                chisel.insert(bi, game::slope_chisel(block.id, shape));
                            }
                        } else if by <= c.water_top {
                            // Above ground, below the water surface -> water
                            // (its top frozen where it is cold).
                            blocks[bi.linear()] = Block::new(water_material(&mats, &c, by));
                        }
                        // else: air (the grid starts all air).
                    }
                }
            }
            // Trees, into the air above the ground and any water only.
            for (lx, lz, by, placed) in col_trees {
                let c = at(fx + lx as i32, fz + lz as i32);
                if by <= c.top || by <= c.water_top {
                    continue;
                }
                let bi = BlockIndex::from_xyz(lx, by.rem_euclid(cb) as usize, lz);
                let (blocks, chisel) = &mut chunks[(by.div_euclid(cb) - base_sub_y) as usize];
                blocks[bi.linear()] = placed.block;
                if let Some(c) = placed.chisel {
                    chisel.insert(bi, c);
                }
            }

            let subs: Vec<SubChunk> = chunks
                .into_iter()
                .map(|(blocks, chisel)| {
                    SubChunk::Dense(Box::new(Chunk { blocks: ChunkBlocks::from_vec(blocks), chisel }))
                })
                .collect();
            // `above = air` trims air tops; the bottom is kept (`grounded`).
            let column = Column::grounded(base_sub_y, stone, air, subs, ground);
            columns.push((ColumnCoords::new(cx, cz), column));
        }
    }
    columns
}

/// Cheap coarse LOD generation for one block-scale-or-coarser key
/// (`game::lod::COARSE_MIN_FACTOR` and up): one profile sample per cell
/// column, surface scan, then solid-below-surface + water to sea level. No
/// block materialization, no chiseling. Pure and deterministic: same (key,
/// seed, mats) → identical cells everywhere. A 64/256 tile is exactly one 32³
/// grid per vertical band; a factor-16 region is 2×2 of them, grouped like
/// `game::lod::block_scale_cells`.
/// Point-sampling is NOT bit-consistent with downsampled full generation
/// (accepted, sub-pixel at these distances — see the design spec §3);
/// `coarse_tile_tracks_downsampled_full_generation` bounds the divergence.
pub fn coarse_tile(
    key: &game::lod::LodKey,
    seed: u64,
    mats: Materials,
) -> Vec<game::lod::LodCell> {
    if let Some(world) = forged::active() {
        return coarse_tile_forged(world, key, mats);
    }
    let (ox, oz, span) = coarse_patch_window(key, &mats);
    let grid = TerrainPipeline::new(seed).profile_grid(ox, oz, span);
    let habitat = habitat::ProceduralHabitat::new(seed);
    coarse_from_patch(&ProceduralPatch(grid), key, mats, TreeSource { habitat: &habitat, seed })
}

/// `coarse_tile` for an explicit forged world (tests, tools). A coarse cell is
/// point-sampled at its center, so rivers narrower than a cell come and go
/// with distance; lakes and valleys survive every ring.
pub fn coarse_tile_forged(
    world: &forged::ForgedWorld,
    key: &game::lod::LodKey,
    mats: Materials,
) -> Vec<game::lod::LodCell> {
    // One cell column of margin past the key, for the steep-face fill's
    // neighbours (rounded to the profile lattice, so surfaces stay
    // bit-identical to any other window's).
    let cb = (key.factor / BLOCK_VOXELS as u32) as i32;
    let lattice = PROFILE_LATTICE_BLOCKS as i32;
    let ring = (cb + lattice - 1) / lattice * lattice;
    let (ox, oz, span) = coarse_patch_window(key, &mats);
    let (ox, oz, span) = (ox - ring as f64, oz - ring as f64, span + 2.0 * ring as f64);
    let trees_from = TreeSource { habitat: world, seed: world.artifact().seed };
    coarse_from_patch(&world.patch(ox, oz, span), key, mats, trees_from)
}

/// World block origin and span of a coarse key.
fn coarse_window(key: &game::lod::LodKey) -> (i32, i32, i32) {
    let origin = key.origin_region();
    (origin.x * REGION_BLOCKS, origin.z * REGION_BLOCKS, key.region_span() * REGION_BLOCKS)
}

/// The patch a coarse key samples: its window, plus the trees' margin.
fn coarse_patch_window(key: &game::lod::LodKey, mats: &Materials) -> (f64, f64, f64) {
    let (ox, oz, span) = coarse_window(key);
    let m = if mats.trees.enabled() { TREE_PATCH_MARGIN } else { 0 };
    ((ox - m) as f64, (oz - m) as f64, (span + 2 * m) as f64)
}

/// A tree's crown as coarse LOD shows it: block offsets from its base of the
/// crown's lowest and highest leaf block, and the crown disc's radius in
/// blocks; `None` for a tree with no leaves.
fn crown_extent(tree: &trees::TreeSeed) -> Option<(i32, i32, i32)> {
    let species = &trees::SPECIES[tree.species as usize];
    let sk = trees::skeleton(species, tree.seed, tree.age);
    let bv = BLOCK_VOXELS as i32;
    let axis = (bv / 2) as f64;
    let (mut lo, mut hi, mut reach) = (i32::MAX, i32::MIN, 0.0f64);
    for &(c, r) in &sk.leaves {
        lo = lo.min((c[1] - r).max(0).div_euclid(bv));
        hi = hi.max((c[1] + r).div_euclid(bv));
        let (dx, dz) = (c[0] as f64 - axis, c[2] as f64 - axis);
        reach = reach.max((dx * dx + dz * dz).sqrt() + r as f64);
    }
    let radius = ((reach / bv as f64).ceil() as i32).min(species.crown_radius_blocks);
    (lo <= hi).then_some((lo, hi, radius))
}

fn coarse_from_patch(
    patch: &dyn Patch,
    key: &game::lod::LodKey,
    mats: Materials,
    trees_from: TreeSource,
) -> Vec<game::lod::LodCell> {
    use game::lod::LodCell;
    use game::Voxel;

    // A LOD grid's edge in cells: the 32³ render/LOD brick.
    const G: i32 = BRICK_VOXELS as i32;
    // Blocks per cell edge: 1 at factor 16, 2 at 32, 4 at 64, 16 at 256.
    debug_assert!(key.factor >= game::lod::COARSE_MIN_FACTOR, "sub-block factor {}", key.factor);
    let cb = (key.factor / BLOCK_VOXELS as u32) as i32;
    let span_blocks = key.region_span() * REGION_BLOCKS;
    let cells = span_blocks / cb; // cell columns per key edge: 64 at factor 16, else 32
    debug_assert_eq!(cells % G, 0, "a key is a whole number of 32³ grids");

    let (ox, oz, _) = coarse_window(key);

    // Centre (world metres) of cell column (cx, cz).
    let centre = |cx: i32, cz: i32| {
        ((ox + cx * cb) as f64 + cb as f64 * 0.5, (oz + cz * cb) as f64 + cb as f64 * 0.5)
    };
    // The column of cell column (cx, cz), sampled at its centre (with
    // whether it is unbounded); cell columns just outside the key are
    // sampled too.
    let cell_sample = |cx: i32, cz: i32| {
        let (mx, mz) = centre(cx, cz);
        point_sample(patch, &mats, mx, mz)
    };
    let samples: Vec<ColSample> =
        (0..cells).flat_map(|cz| (0..cells).map(move |cx| (cx, cz))).map(|(cx, cz)| cell_sample(cx, cz)).collect();
    let surface_of = |cx: i32, cz: i32| {
        if (0..cells).contains(&cx) && (0..cells).contains(&cz) {
            samples[(cz * cells + cx) as usize].top
        } else {
            cell_sample(cx, cz).top
        }
    };
    let strata = patch.strata(&mats);

    let mut grids: std::collections::BTreeMap<[i32; 3], Vec<Voxel>> = Default::default();
    for cz in 0..cells {
        for cx in 0..cells {
            let c = samples[(cz * cells + cx) as usize];
            let (surface, water_top, unbounded) = (c.s, c.water_top, c.unbounded);

            // Under the soil band; a forged column also reaches down to its
            // lowest face neighbour's surface, so a steep face (forged
            // mountains) has no gaps. Procedural columns keep the soil band
            // only, which keeps their tiles bit-identical.
            let mut solid_lo_block = surface - MAX_SOIL_DEPTH;
            if unbounded {
                for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    solid_lo_block = solid_lo_block.min(surface_of(cx + dx, cz + dz));
                }
            }
            let solid_lo = solid_lo_block.div_euclid(cb);
            let solid_hi = c.top.div_euclid(cb);
            let top_cell = c.top.max(water_top).div_euclid(cb);
            for cy in solid_lo..=top_cell {
                let is_solid = cy <= solid_hi;
                // Water only where the cell's bottom block is at/below the surface.
                if !is_solid && cy * cb > water_top {
                    continue;
                }
                let mat = if is_solid {
                    // The cell's topmost block of terrain — what shows from
                    // above, matching `game::lod::merge_group`.
                    let top = c.top.min(cy * cb + cb - 1);
                    match column_material(&mats, &c, top) {
                        BlockId::AIR => {
                            let (mx, mz) = centre(cx, cz);
                            strata_rock(&strata, patch.geo(mx, mz), top)
                        }
                        mat => mat,
                    }
                } else {
                    // Likewise the cell's topmost water block.
                    water_material(&mats, &c, water_top.min(cy * cb + cb - 1))
                };
                let group = [cx.div_euclid(G), cy.div_euclid(G), cz.div_euclid(G)];
                let (lx, ly, lz) = ((cx % G) as usize, cy.rem_euclid(G) as usize, (cz % G) as usize);
                let g = grids
                    .entry(group)
                    .or_insert_with(|| vec![Voxel::default(); BRICK_VOXEL_COUNT]);
                g[lx + BRICK_VOXELS * ly + BRICK_VOXELS * BRICK_VOXELS * lz] = Voxel::new(mat);
            }
        }
    }

    // Trees: each a leaves column from its crown's base to its top over its
    // crown disc (and always over its trunk), into air cells only.
    if mats.trees.enabled() {
        // Placement reads surfaces up to `m` past the key; each surface's
        // slope reads one block further (still inside the patch's
        // `TREE_PATCH_MARGIN`).
        let m = TREE_PATCH_MARGIN - PATCH_MARGIN;
        let sample_memo = std::cell::RefCell::new(std::collections::HashMap::new());
        let sample = |x: i32, z: i32| {
            assert!(
                (ox - m - 1..ox + span_blocks + m + 1).contains(&x)
                    && (oz - m - 1..oz + span_blocks + m + 1).contains(&z),
                "a tree surface's slope read the column ({x}, {z}), outside the coarse patch"
            );
            let cached = sample_memo.borrow().get(&(x, z)).copied();
            cached.unwrap_or_else(|| {
                let c = column_sample(patch, &mats, x, z);
                sample_memo.borrow_mut().insert((x, z), c);
                c
            })
        };
        let memo = std::cell::RefCell::new(std::collections::HashMap::new());
        let surface = |x: i32, z: i32| {
            assert!(
                (ox - m..ox + span_blocks + m).contains(&x) && (oz - m..oz + span_blocks + m).contains(&z),
                "tree placement read the surface at ({x}, {z}), outside the coarse patch"
            );
            let cached = memo.borrow().get(&(x, z)).copied();
            cached.unwrap_or_else(|| {
                let c = sample(x, z);
                let slope = slopes::slope_for(&c, |dx, dz| sample(x + dx, z + dz)).is_some();
                let g = tree_ground(c, slope);
                memo.borrow_mut().insert((x, z), g);
                g
            })
        };
        let memo_habitat = MemoHabitat { inner: trees_from.habitat, memo: Default::default() };
        let (x1, z1) = (ox + span_blocks, oz + span_blocks);
        for tree in trees::trees_in(ox, oz, x1, z1, trees_from.seed, &memo_habitat, &surface) {
            let Some((lo, hi, radius)) = crown_extent(&tree) else { continue };
            let leaves = Voxel::new(mats.trees.species[tree.species as usize].leaves);
            let [bx, by, bz] = tree.base;
            let (cy0, cy1) = ((by + lo).div_euclid(cb), (by + hi).div_euclid(cb));
            let cells_of = |a: i32, o: i32| ((a - radius).max(o) - o) / cb..=((a + radius).min(o + span_blocks - 1) - o) / cb;
            for cx in cells_of(bx, ox) {
                for cz in cells_of(bz, oz) {
                    // Cell centre to trunk axis, blocks.
                    let (dx, dz) = (
                        (ox + cx * cb) as f64 + cb as f64 * 0.5 - (bx as f64 + 0.5),
                        (oz + cz * cb) as f64 + cb as f64 * 0.5 - (bz as f64 + 0.5),
                    );
                    let trunk = (ox + cx * cb..ox + cx * cb + cb).contains(&bx)
                        && (oz + cz * cb..oz + cz * cb + cb).contains(&bz);
                    if !trunk && dx * dx + dz * dz > (radius as f64 + 0.5) * (radius as f64 + 0.5) {
                        continue;
                    }
                    for cy in cy0..=cy1 {
                        let group = [cx.div_euclid(G), cy.div_euclid(G), cz.div_euclid(G)];
                        let (lx, ly, lz) = ((cx % G) as usize, cy.rem_euclid(G) as usize, (cz % G) as usize);
                        let g = grids.entry(group).or_insert_with(|| vec![Voxel::default(); BRICK_VOXEL_COUNT]);
                        let v = &mut g[lx + BRICK_VOXELS * ly + BRICK_VOXELS * BRICK_VOXELS * lz];
                        if v.is_air() {
                            *v = leaves;
                        }
                    }
                }
            }
        }
    }
    grids
        .into_iter()
        .map(|(group, grid)| LodCell { group, grid })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, Column, ColumnCoords, ReadVol, BlockPos, SubChunk, REGION_CHUNKS};

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

    fn mats() -> crate::material::Materials {
        crate::material::Materials {
            stone: BlockId(2),
            dirt: BlockId(1),
            grass: BlockId(3),
            sand: BlockId(4),
            water: BlockId(5),
            bedrock: game::BlockId(6),
            rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
            trees: crate::material::TreeBlocks::NONE,
            biome: crate::material::BiomeBlocks::NONE,
        }
    }

    fn crc(cols: &[(ColumnCoords, Column)]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&bincode::serialize(cols).unwrap());
        h.finalize()
    }

    #[test]
    fn produces_sixty_four_columns() {
        let cols = generate_region(RegionCoords::new(0, 0), 1, mats());
        assert_eq!(cols.len(), REGION_CHUNKS * REGION_CHUNKS);
    }

    #[test]
    fn deterministic_same_seed() {
        let a = generate_region(RegionCoords::new(-3, 7), 99, mats());
        let b = generate_region(RegionCoords::new(-3, 7), 99, mats());
        assert_eq!(crc(&a), crc(&b));
    }

    #[test]
    fn different_seed_differs() {
        let a = generate_region(RegionCoords::new(0, 0), 1, mats());
        let b = generate_region(RegionCoords::new(0, 0), 2, mats());
        assert_ne!(crc(&a), crc(&b));
    }

    /// Worldgen terrain is whole blocks and slopes only: every non-tree
    /// chisel is one of the slope shapes of its block's own material, there
    /// are some (so placement actually ran and wrote real terrain), none sits
    /// at or under water, where its open quarters would be air pockets, and no
    /// one-cuboid half block is left anywhere.
    #[test]
    fn terrain_is_whole_blocks_and_dry_slopes_only() {
        // Coast north-east of spawn: land, beach and open sea.
        let rc = RegionCoords::from_world(5632.0, -2560.0);
        let cols = generate_region(rc, WORLD_SEED, mats());
        let mut slopes = 0;
        for (_, col) in &cols {
            for (sub_y, sub) in (col.base_sub_y()..).zip(col.sub_chunks()) {
                let SubChunk::Dense(chunk) = sub else { continue };
                for (bi, chisel) in &chunk.chisel {
                    let (bx, by, bz) = bi.xyz();
                    if chunk.block_at(bx, by, bz).tree_id() != 0 {
                        continue; // tree wood, not terrain
                    }
                    let mat = chunk.block_at(bx, by, bz).id;
                    let cuboids: Vec<_> = chisel.cuboids().collect();
                    let half = vec![([0, 0, 0], [BLOCK_VOXELS, game::HALF_BLOCK_VOXELS, BLOCK_VOXELS], mat)];
                    assert_ne!(cuboids, half, "a half block is left");
                    assert!(cuboids.iter().all(|c| c.2 == mat), "a slope in another material");
                    assert!(game::slope::classify(chisel).is_some(), "not a slope: {cuboids:?}");
                    slopes += 1;
                    let y = sub_y * CHUNK_BLOCKS as i32 + by as i32;
                    let above = col.get(BlockPos::new(bx as i32, y + 1, bz as i32));
                    assert_ne!(above.id, mats().water, "slope under water at block y {y}");
                    assert!(y > SEA_TOP_BLOCK, "slope at or under the sea surface at block y {y}");
                }
            }
        }
        assert!(slopes > 0, "expected slopes on the coast");
    }

    #[test]
    fn columns_contain_solid_ground() {
        let cols = generate_region(RegionCoords::new(5, 5), 3, mats());

        // Sanity: `get()` still reads solid ground somewhere too.
        let any_solid = cols.iter().any(|(_, col)| {
            (0..64).any(|y| col.get(BlockPos::new(0, y, 0)).id != BlockId::AIR)
        });
        assert!(any_solid, "expected some solid ground in the region");
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

    #[test]
    fn coarse_tile_is_deterministic_and_key_local() {
        let key = game::lod::LodKey { factor: 64, coords: [0, 0] };
        // One grid per band when the factor-64 grid covers the whole key;
        // several (2x2 at 256 blocks) when a region is bigger than it.
        let grids_per_axis = ((game::REGION_SIZE as i32) / (32 * 64)).max(1);
        let a = coarse_tile(&key, WORLD_SEED, mats());
        let b = coarse_tile(&key, WORLD_SEED, mats());
        assert_eq!(a.len(), b.len());
        for (ca, cb) in a.iter().zip(&b) {
            assert_eq!(ca.group, cb.group);
            assert_eq!(ca.grid, cb.grid);
            assert!(
                (0..grids_per_axis).contains(&ca.group[0]) && (0..grids_per_axis).contains(&ca.group[2]),
                "key-local groups: {:?}",
                ca.group
            );
        }
        assert!(!a.is_empty(), "sea-level world always materializes at least one band");
        // Sea level (block 64) must be covered: some cell is water or solid.
        assert!(a.iter().any(|c| c.grid.iter().any(|v| !v.is_air())));
    }

    /// Coarse point-sampling vs exact generate+downsample: per cell column the
    /// highest solid cell must match within one cell for the vast majority of
    /// columns. This bounds the accepted §3 inexactness; tighten the threshold
    /// only with measurement.
    #[test]
    fn sea_surface_sits_on_every_lod_lattice() {
        let surface_voxels = (SEA_TOP_BLOCK + 1) as u32 * BLOCK_VOXELS as u32;
        for factor in game::lod::LOD_FACTORS {
            assert_eq!(surface_voxels % factor, 0, "factor {factor} would round the sea up");
        }
    }

    /// The sea is one flat plane in every ring: coarse sampling puts its top
    /// cell boundary exactly on the real water surface at every coarse factor.
    #[test]
    fn coarse_sea_surface_is_exact_at_every_factor() {
        let surface_voxels = (SEA_TOP_BLOCK + 1) * BLOCK_VOXELS as i32;
        // Coast north-east of spawn: grass, beach and open sea. Named by WORLD position
        // — region (5, -3) at the default size — so it is the same coast at any
        // `REGION_CHUNKS`.
        let rc = RegionCoords::from_world(5632.0, -2560.0);
        for factor in [16u32, 32, 64, 128, 256, 512] {
            let key = game::lod::LodKey::for_region(factor, rc);
            let mut water_columns = 0;
            for cell in coarse_tile(&key, WORLD_SEED, mats()) {
                for z in 0..32usize {
                    for x in 0..32usize {
                        let top = (0..32usize)
                            .rev()
                            .find(|&y| !cell.grid[x + 32 * y + 32 * 32 * z].is_air());
                        let Some(y) = top else { continue };
                        if cell.grid[x + 32 * y + 32 * 32 * z].block != mats().water {
                            continue;
                        }
                        water_columns += 1;
                        let top_voxels = (cell.group[1] * 32 + y as i32 + 1) * factor as i32;
                        assert_eq!(top_voxels, surface_voxels, "factor {factor}");
                    }
                }
            }
            assert!(water_columns > 0, "factor {factor}: the coast key has open water");
        }
    }

    /// The region-keyed coarse factors replace full generation for pristine
    /// terrain, so they must reproduce it, not merely resemble it: same
    /// surface cell and same visible material in nearly every column.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn region_coarse_factors_match_downsampled_full_generation() {
        let rc = RegionCoords::from_world(5632.0, -2560.0); // coast north-east of spawn (region (5, -3) at the default size): grass, beach and open sea
        let started = std::time::Instant::now();
        let chunks: Vec<(game::ChunkCoords, game::Chunk)> = generate_region(rc, WORLD_SEED, mats())
            .into_iter()
            .flat_map(|(cc, col)| {
                col.materialized_sub_chunks()
                    .into_iter()
                    .map(move |(sub_y, ch)| (game::ChunkCoords::new(cc.x, sub_y, cc.z), ch))
            })
            .collect();
        println!("generate_region: {:?}", started.elapsed());

        // Topmost solid cell (absolute cell y, block id) per cell column.
        let tops = |cells: &[game::lod::LodCell], per: usize| {
            let mut out = std::collections::BTreeMap::new();
            for c in cells {
                for z in 0..32usize {
                    for x in 0..32usize {
                        let col = (c.group[0] as usize * 32 + x, c.group[2] as usize * 32 + z);
                        if col.0 >= per || col.1 >= per {
                            continue;
                        }
                        for y in (0..32usize).rev() {
                            let v = c.grid[x + 32 * y + 32 * 32 * z];
                            if !v.is_air() {
                                let top = (c.group[1] * 32 + y as i32, v.block);
                                let e = out.entry(col).or_insert(top);
                                *e = (*e).max(top);
                                break;
                            }
                        }
                    }
                }
            }
            out
        };

        for factor in [16u32, 32] {
            let key = game::lod::LodKey::for_region(factor, rc);
            let started = std::time::Instant::now();
            let coarse = coarse_tile(&key, WORLD_SEED, mats());
            println!("coarse_tile factor {factor}: {:?}", started.elapsed());
            let exact = game::lod::downsample_chunks(chunks.clone(), factor);

            let per = game::lod::cells_per_region(factor);
            let (a, b) = (tops(&coarse, per), tops(&exact, per));
            assert_eq!(a.len(), per * per, "factor {factor}: every column has terrain or sea");
            let (mut height_hits, mut material_hits) = (0, 0);
            for (col, (ay, amat)) in &a {
                let (by, bmat) = b[col];
                height_hits += (*ay == by) as usize;
                material_hits += (*ay == by && *amat == bmat) as usize;
            }
            println!("factor {factor}: height {height_hits}/{}, material {material_hits}", a.len());
            assert!(height_hits * 100 >= a.len() * 95, "factor {factor}: surface cell matches");
            assert!(material_hits * 100 >= a.len() * 90, "factor {factor}: visible material matches");
        }
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn coarse_tile_tracks_downsampled_full_generation() {
        // A tile spanning several regions: factor 64 (2x2) at the default
        // region size, 256 once a factor-64 grid is one region or less.
        let factor = if game::lod::tile_regions(64) > 1 { 64 } else { 256 };
        if game::lod::tile_regions(factor) <= 1 {
            return; // large cell-mode regions: no tile spans several regions
        }
        let key = game::lod::LodKey { factor, coords: [0, 0] };
        let coarse = coarse_tile(&key, WORLD_SEED, mats());

        // Exact: generate each covered region, downsample to 64, overlay into a tile.
        let mut exact: std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>> = Default::default();
        for rc in key.regions() {
            let columns = generate_region(rc, WORLD_SEED, mats());
            let chunks: Vec<(game::ChunkCoords, game::Chunk)> = columns
                .into_iter()
                .flat_map(|(cc, col)| {
                    col.materialized_sub_chunks()
                        .into_iter()
                        .map(move |(sub_y, ch)| (game::ChunkCoords::new(cc.x, sub_y, cc.z), ch))
                })
                .collect();
            let cells = game::lod::downsample_chunks(chunks, key.factor);
            game::lod::copy_region_cells_into_tile(&mut exact, &cells, rc, &key);
        }

        let top_of = |grids: &std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>>,
                      x: usize,
                      z: usize| {
            grids
                .iter()
                .flat_map(|(g, grid)| {
                    (0..32).rev().find_map(|y| {
                        (!grid[x + 32 * y + 32 * 32 * z].is_air())
                            .then(|| g[1] * 32 + y as i32)
                    })
                })
                .max()
        };
        let coarse_map: std::collections::BTreeMap<[i32; 3], Vec<game::Voxel>> =
            coarse.iter().map(|c| (c.group, c.grid.clone())).collect();

        let (mut hits, mut total) = (0, 0);
        for x in 0..32 {
            for z in 0..32 {
                let (a, b) = (top_of(&coarse_map, x, z), top_of(&exact, x, z));
                if let (Some(a), Some(b)) = (a, b) {
                    total += 1;
                    if (a - b).abs() <= 1 {
                        hits += 1;
                    }
                }
            }
        }
        assert!(total > 200, "most columns have terrain (total={total})");
        assert!(
            hits * 10 >= total * 9,
            "coarse surface within ±1 cell for >=90% of columns ({hits}/{total})"
        );
    }
}
