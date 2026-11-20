//! Deterministic world generation. `generate_region` is a pure function of
//! region coordinates: same coords → identical output on every machine,
//! which is what makes "cycle out = park, cycle in = restore-or-regenerate"
//! safe for the multi-region world.

pub mod cache;
mod density;
pub mod forge;
pub mod forged;
pub mod forged_tiles;
pub mod habitat;
pub mod islands;
pub mod material;
mod noise;
pub mod pipeline;
pub mod sites;
mod slopes;
mod spline;
pub mod structures;
pub mod surface;
pub mod tile_map;
pub mod trees;
pub mod world_store;

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
    BLOCK_VOXELS, BRICK_VOXEL_COUNT, BRICK_VOXELS, Biome, Block, BlockId, BlockIndex,
    CHUNK_BLOCK_COUNT, CHUNK_BLOCKS, ChiselData, Chunk, ChunkBlocks, Column, ColumnCoords,
    REGION_CHUNKS, RegionCoords, SubChunk,
};

use crate::forged::ColumnClimate;
use crate::material::{GroundCover, Materials};
use crate::pipeline::{PROFILE_LATTICE_BLOCKS, ProfileGrid, SEA_TOP_BLOCK, TerrainPipeline};

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
    c.s >= if c.unbounded {
        FORGED_SCAN_BOTTOM
    } else {
        SCAN_BOTTOM
    }
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
    debug_assert!(
        s >= scan_band(&prof).1,
        "surface below the scan at ({x}, {z})"
    );
    let unbounded = prof.is_unbounded();
    let plain = ColSample {
        s,
        water_top,
        top: s,
        ice_from: water_top + 1,
        cover: None,
        trees: true,
        unbounded,
    };
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
        ice_from: if ice > 0 {
            (water_top - ice + 1).max(s + 1)
        } else {
            water_top + 1
        },
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
    c.cover
        .map_or(MAX_SOIL_DEPTH, |cover| cover.soil_depth.max(MAX_SOIL_DEPTH))
}

/// The surface a tree may stand on, from a `column_sample` and whether it
/// carries a slope: dry, no slope (a trunk would hang over the slope's open
/// front) and a cover that grows trees. `None` = no tree.
fn tree_ground(c: ColSample, slope: bool) -> Option<i32> {
    (!slope && c.s >= c.water_top && c.trees).then_some(c.s)
}

/// Where trees come from: the habitat, the placement seed and the lattice's
/// wrap (`None`: the world does not wrap east–west).
#[derive(Clone, Copy)]
struct TreeSource<'a> {
    habitat: &'a dyn habitat::HabitatSource,
    seed: u64,
    wrap: Option<trees::LatticeWrap>,
}

impl<'a> TreeSource<'a> {
    /// The trees of forged world `world`.
    fn forged(world: &'a forged::ForgedWorld) -> Self {
        TreeSource {
            habitat: world,
            seed: world.seed(),
            wrap: tree_lattice_wrap(world),
        }
    }
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
    let memo = MemoHabitat {
        inner: trees_from.habitat,
        memo: Default::default(),
    };
    trees::trees_in(
        (x0 - TREE_REACH_BLOCKS).max(rx),
        (z0 - TREE_REACH_BLOCKS).max(rz),
        (x1 + TREE_REACH_BLOCKS).min(rx + REGION_BLOCKS),
        (z1 + TREE_REACH_BLOCKS).min(rz + REGION_BLOCKS),
        trees_from.seed,
        trees_from.wrap,
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
    strata
        .iter()
        .rev()
        .find(|(top, _)| i32::from(*top) > s)
        .or(strata.first())
        .map_or(BlockId::AIR, |&(_, id)| id)
}

/// The formations of `strata` (top-down) that `s` in `lo_s..=hi_s` can hit
/// under `ColumnGround::block_at`'s rule, plus one more above and below.
fn footprint_strata(strata: &[(i16, BlockId)], lo_s: i32, hi_s: i32) -> Vec<(i16, BlockId)> {
    let holding = |s: i32| {
        strata
            .partition_point(|&(top, _)| i32::from(top) > s)
            .saturating_sub(1)
    };
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
pub fn generate_region(
    coords: RegionCoords,
    seed: u64,
    mats: Materials,
) -> Vec<(ColumnCoords, Column)> {
    generate_columns(coords, 0..REGION_CHUNKS, 0..REGION_CHUNKS, seed, mats)
}

/// `generate_region` with an explicit structure source and no forged world (procedural terrain from
/// `seed`).
pub fn generate_region_with(
    coords: RegionCoords,
    seed: u64,
    mats: Materials,
    structures: &dyn structures::StructureSource,
) -> Vec<(ColumnCoords, Column)> {
    generate_columns_with(
        coords,
        0..REGION_CHUNKS,
        0..REGION_CHUNKS,
        seed,
        mats,
        structures,
    )
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
    if let Some(world) = forged::active() {
        let coords = canonical_region(world, coords);
        let cb = CHUNK_BLOCKS as i32;
        let (x, z) = (
            coords.x * REGION_BLOCKS + xs.start as i32 * cb,
            coords.z * REGION_BLOCKS + zs.start as i32 * cb,
        );
        let (ox, oz, span) = patch_window(x, z, xs.len().max(zs.len()) as i32 * cb, &mats);
        let trees_from = TreeSource::forged(world);
        let patch = world.patch(ox, oz, span);
        let structures = wrapped_structures(world, structures::active());
        return columns_from_patch(&patch, coords, xs, zs, mats, trees_from, &structures);
    }
    generate_columns_with(coords, xs, zs, seed, mats, structures::active())
}

/// `generate_columns` with an explicit structure source and no forged world (procedural terrain from
/// `seed`). `generate_columns` is this with `structures::active()` when no forged world is installed.
pub fn generate_columns_with(
    coords: RegionCoords,
    xs: std::ops::Range<usize>,
    zs: std::ops::Range<usize>,
    seed: u64,
    mats: Materials,
    structures: &dyn structures::StructureSource,
) -> Vec<(ColumnCoords, Column)> {
    let cb = CHUNK_BLOCKS as i32;
    let (x, z) = (
        coords.x * REGION_BLOCKS + xs.start as i32 * cb,
        coords.z * REGION_BLOCKS + zs.start as i32 * cb,
    );
    let (ox, oz, span) = patch_window(x, z, xs.len().max(zs.len()) as i32 * cb, &mats);
    let grid = TerrainPipeline::new(seed).profile_grid(ox, oz, span);
    let habitat = habitat::ProceduralHabitat::new(seed);
    let trees_from = TreeSource {
        habitat: &habitat,
        seed,
        wrap: None,
    };
    columns_from_patch(
        &ProceduralPatch(grid),
        coords,
        xs,
        zs,
        mats,
        trees_from,
        structures,
    )
}

/// The y of the top ground block of world column (x, z) — the same value generation uses as the
/// column's surface before any structure pad. Uses the installed forged world, else `seed`. The patch
/// is the chunk-aligned one-chunk window holding the column, as `generate_columns` builds it.
pub fn ground_y(x: i32, z: i32, seed: u64, mats: Materials) -> i32 {
    if let Some(world) = forged::active() {
        return ground_y_forged(world, x, z, mats);
    }
    let (ox, oz, span) = chunk_window(x, z, &mats);
    let grid = TerrainPipeline::new(seed).profile_grid(ox, oz, span);
    column_sample(&ProceduralPatch(grid), &mats, x, z).s
}

/// `ground_y` for an explicit forged world.
pub fn ground_y_forged(world: &forged::ForgedWorld, x: i32, z: i32, mats: Materials) -> i32 {
    let x = x - wrap_shift_blocks(world, x);
    let (ox, oz, span) = chunk_window(x, z, &mats);
    column_sample(&world.patch(ox, oz, span), &mats, x, z).s
}

/// Whether world column (x, z) is under water in natural terrain (the same sample generation uses: its
/// water top lies above its ground). Uses the installed forged world, else `seed`; the patch is the one
/// [`ground_y`] reads.
pub fn is_wet(x: i32, z: i32, seed: u64, mats: Materials) -> bool {
    let c = match forged::active() {
        Some(world) => {
            let x = x - wrap_shift_blocks(world, x);
            let (ox, oz, span) = chunk_window(x, z, &mats);
            column_sample(&world.patch(ox, oz, span), &mats, x, z)
        }
        None => {
            let (ox, oz, span) = chunk_window(x, z, &mats);
            column_sample(
                &ProceduralPatch(TerrainPipeline::new(seed).profile_grid(ox, oz, span)),
                &mats,
                x,
                z,
            )
        }
    };
    c.water_top > c.top
}

/// The multiple of `world`'s width (blocks) that takes world block column
/// `x` onto the map's canonical regions (`tile_map::map_extent`'s
/// `[origin.x, origin.x + width)`): 0 on a world that does not wrap
/// east–west. Generation reads `x - shift`, so every image of a region or LOD
/// key yields the terrain (trees and structures included) of its canonical
/// image, laid out where the caller asked.
fn wrap_shift_blocks(world: &forged::ForgedWorld, x: i32) -> i32 {
    wrap_frame(world).map_or(0, |(origin, width)| (x - origin).div_euclid(width) * width)
}

/// The canonical block columns `[origin, origin + width)` of a world that
/// wraps east–west, as `(origin, width)`; `None` for a world that does not.
/// A width that is not whole regions cannot shift a region onto a region
/// (the forge refuses such a wrapping world): `None` too, and the sampler
/// still folds every point it reads.
fn wrap_frame(world: &forged::ForgedWorld) -> Option<(i32, i32)> {
    let g = world.globals();
    if g.edges[1] != forged::EdgeKind::Wrap || g.width_m % f64::from(REGION_BLOCKS) != 0.0 {
        return None;
    }
    let (origin, _, _) = tile_map::map_extent(world);
    Some((origin.x * REGION_BLOCKS, g.width_m as i32))
}

/// `structures` as `world` sees it: on a world that wraps east–west a window
/// near the seam also gets the other side's placements and paints, moved to
/// its frame (`structures::WrappedStructures`); otherwise exactly `structures`.
fn wrapped_structures<'a>(
    world: &forged::ForgedWorld,
    structures: &'a dyn structures::StructureSource,
) -> structures::WrappedStructures<'a> {
    structures::WrappedStructures {
        inner: structures,
        frame: wrap_frame(world),
    }
}

/// The tree lattice's wrap on `world` (`wrap_frame` in lattice columns; a
/// region is a whole number of them): `None` when it does not wrap.
///
/// Tree ids then repeat with the period too. Two cells share an id only when
/// their folded columns are equal mod 512 (`placement::tree_id`), so across
/// the seam their unwrapped distance is ≡ period (mod 512): with the period a
/// multiple of 64 cells (every wrapping width is whole 512 m) that is at least
/// 64 cells = 512 blocks. `game::trees::fell`'s same-id flood fill reads ids
/// one halo band (16 blocks) past its region and a tree spans at most 9 blocks
/// from its trunk, so it never reaches a second tree of the same id.
pub fn tree_lattice_wrap(world: &forged::ForgedWorld) -> Option<trees::LatticeWrap> {
    let l = trees::TREE_LATTICE_BLOCKS;
    wrap_frame(world).map(|(origin, width)| {
        let period = width / l;
        debug_assert_eq!(
            period % 64,
            0,
            "tree id spacing across the seam needs a period of whole 64 cells, got {period}"
        );
        trees::LatticeWrap {
            origin: origin / l,
            period,
        }
    })
}

/// The trees region `coords` of `world` stands, as its generation places
/// them with no structure source (a forged world has none outside the dev
/// tools), sorted by id, positioned in the frame `coords` is in. Empty when
/// `mats` has trees off. For tools (the forge report).
pub fn region_trees_forged(
    world: &forged::ForgedWorld,
    coords: RegionCoords,
    mats: Materials,
) -> Vec<trees::TreeSeed> {
    if !mats.trees.enabled() {
        return Vec::new();
    }
    let shift = wrap_shift_blocks(world, coords.x * REGION_BLOCKS);
    let coords = canonical_region(world, coords);
    let (rx, rz) = (coords.x * REGION_BLOCKS, coords.z * REGION_BLOCKS);
    let (ox, oz, span) = patch_window(rx, rz, REGION_BLOCKS, &mats);
    let patch = world.patch(ox, oz, span);
    // `columns_from_patch`'s tree surface, without structures.
    let sample = |x: i32, z: i32| column_sample(&patch, &mats, x, z);
    let surface = |x: i32, z: i32| {
        let c = sample(x, z);
        tree_ground(
            c,
            slopes::slope_for(&c, |dx, dz| sample(x + dx, z + dz)).is_some(),
        )
    };
    let window = [rx, rz, rx + REGION_BLOCKS, rz + REGION_BLOCKS];
    let mut out = trees_for_window(coords, window, TreeSource::forged(world), &surface);
    for t in &mut out {
        t.base[0] += shift;
    }
    out
}

/// Region `coords` folded onto `world`'s canonical regions (itself on a
/// world that does not wrap east–west).
fn canonical_region(world: &forged::ForgedWorld, coords: RegionCoords) -> RegionCoords {
    let shift = wrap_shift_blocks(world, coords.x * REGION_BLOCKS);
    RegionCoords::new(coords.x - shift / REGION_BLOCKS, coords.z)
}

/// `patch_window` of the one-chunk window holding world block column `(x, z)`.
fn chunk_window(x: i32, z: i32, mats: &Materials) -> (f64, f64, f64) {
    let cb = CHUNK_BLOCKS as i32;
    patch_window(x.div_euclid(cb) * cb, z.div_euclid(cb) * cb, cb, mats)
}

/// Patch margin (blocks) past a window: wider when trees are on, which read
/// surfaces further out (`TREE_RING_BLOCKS`).
fn patch_margin(mats: &Materials) -> i32 {
    if mats.trees.enabled() {
        TREE_PATCH_MARGIN
    } else {
        PATCH_MARGIN
    }
}

/// Patch origin and span (meters) for the window of `span` blocks at world
/// block `(x, z)`, margin included.
fn patch_window(x: i32, z: i32, span: i32, mats: &Materials) -> (f64, f64, f64) {
    let m = patch_margin(mats);
    ((x - m) as f64, (z - m) as f64, (span + 2 * m) as f64)
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
/// through [`generate_columns`] / [`generate_region`]. Hold the pins while
/// generating: the reads then never load, and a tile that cannot be loaded
/// is this `Err` (naming the tile), never terrain. Empty without a world or
/// for a whole one.
pub fn prefetch_region(rc: RegionCoords) -> Result<forged_tiles::TilePins, String> {
    forged::active().map_or_else(
        || Ok(Default::default()),
        |world| prefetch_region_forged(world, rc),
    )
}

/// [`prefetch_region`] for an explicit forged world.
pub fn prefetch_region_forged(
    world: &forged::ForgedWorld,
    rc: RegionCoords,
) -> Result<forged_tiles::TilePins, String> {
    let rc = canonical_region(world, rc);
    // The wider (trees on) margin, whatever the materials.
    let (x, z) = (
        f64::from(rc.x * REGION_BLOCKS),
        f64::from(rc.z * REGION_BLOCKS),
    );
    let (min, max) = read_rect(x, z, f64::from(REGION_BLOCKS), TREE_PATCH_MARGIN);
    world.prefetch_rect(min, max)
}

/// Loads and pins the tiles of the installed forged world that building LOD
/// key `key` reads ([`coarse_tile`] and [`coarse_water`]); see
/// [`prefetch_region`].
pub fn prefetch_lod(key: &game::lod::LodKey) -> Result<forged_tiles::TilePins, String> {
    forged::active().map_or_else(
        || Ok(Default::default()),
        |world| prefetch_lod_forged(world, key),
    )
}

/// [`prefetch_lod`] for an explicit forged world.
pub fn prefetch_lod_forged(
    world: &forged::ForgedWorld,
    key: &game::lod::LodKey,
) -> Result<forged_tiles::TilePins, String> {
    // `coarse_tile_forged`'s window with the trees' margin (it contains
    // `coarse_water_forged`'s).
    let (ox, oz, span) = coarse_window(key, coarse_shift(world, key));
    let margin = TREE_PATCH_MARGIN + coarse_ring(key);
    let (min, max) = read_rect(f64::from(ox), f64::from(oz), f64::from(span), margin);
    world.prefetch_rect(min, max)
}

/// `generate_region` for an explicit forged world (tests, tools), with the installed structure source.
pub fn generate_region_forged(
    world: &forged::ForgedWorld,
    coords: RegionCoords,
    mats: Materials,
) -> Vec<(ColumnCoords, Column)> {
    generate_region_forged_with(world, coords, mats, structures::active())
}

/// `generate_region_forged` with an explicit structure source.
pub fn generate_region_forged_with(
    world: &forged::ForgedWorld,
    coords: RegionCoords,
    mats: Materials,
    structures: &dyn structures::StructureSource,
) -> Vec<(ColumnCoords, Column)> {
    let coords = canonical_region(world, coords);
    let (ox, oz, span) = patch_window(
        coords.x * REGION_BLOCKS,
        coords.z * REGION_BLOCKS,
        REGION_BLOCKS,
        &mats,
    );
    let trees_from = TreeSource::forged(world);
    let patch = world.patch(ox, oz, span);
    let structures = wrapped_structures(world, structures);
    columns_from_patch(
        &patch,
        coords,
        0..REGION_CHUNKS,
        0..REGION_CHUNKS,
        mats,
        trees_from,
        &structures,
    )
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

/// One generated tree block (always a whole block) and whether it is wood.
struct PlacedTreeBlock {
    block: Block,
    wood: bool,
    root: bool,
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
) -> (BTreeMap<[i32; 3], PlacedTreeBlock>, Vec<game::TreeLifeSeed>) {
    let [x0, z0, x1, z1] = window;
    let mut out: BTreeMap<[i32; 3], PlacedTreeBlock> = BTreeMap::new();
    let mut seeds = Vec::new();
    for tree in trees_for_window(coords, window, trees_from, surface) {
        let blocks = mats.trees.species[tree.species as usize];
        let sk = trees::skeleton(&trees::SPECIES[tree.species as usize], tree.seed, tree.age);
        for b in trees::rasterize(&sk) {
            let p = [
                tree.base[0] + b.offset[0],
                tree.base[1] + b.offset[1],
                tree.base[2] + b.offset[2],
            ];
            if p[0] < x0 || p[0] >= x1 || p[2] < z0 || p[2] >= z1 || !in_region(coords, p[0], p[2])
            {
                continue;
            }
            let (id, wood) = match b.part {
                trees::TreePart::Log => (blocks.log, true),
                trees::TreePart::Branch => (blocks.branch, true),
                trees::TreePart::Leaves => (blocks.leaves, false),
            };
            let placed = PlacedTreeBlock {
                block: Block::with_tree(id, tree.id, b.limb),
                wood,
                root: false,
            };
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
        let mut roots = vec![[tree.base[0], tree.base[1] - 1, tree.base[2]]];
        roots.push([tree.base[0], tree.base[1] - 2, tree.base[2]]);
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let p = [tree.base[0] + dx, tree.base[1] - 2, tree.base[2] + dz];
            if surface(p[0], p[2]).is_some_and(|sy| p[1] <= sy) && in_region(coords, p[0], p[2]) {
                roots.push(p);
            }
        }
        roots.sort_unstable();
        roots.dedup();
        for &p in &roots {
            if p[0] < x0 || p[0] >= x1 || p[2] < z0 || p[2] >= z1 {
                continue;
            }
            out.insert(
                p,
                PlacedTreeBlock {
                    block: Block::with_tree_id(blocks.log, tree.id),
                    wood: true,
                    root: true,
                },
            );
        }
        if tree.base[0] >= x0 && tree.base[0] < x1 && tree.base[2] >= z0 && tree.base[2] < z1 {
            let origin = [coords.x * REGION_BLOCKS, coords.z * REGION_BLOCKS];
            let local = |p: [i32; 3]| [p[0] - origin[0], p[1], p[2] - origin[1]];
            let roots: Vec<[i32; 3]> = roots.into_iter().map(local).collect();
            seeds.push(game::TreeLifeSeed {
                life: game::TreeLife {
                    genome: game::TreeGenome {
                        id: tree.id,
                        species: tree.species,
                        seed: tree.seed,
                        base: local(tree.base),
                        initial_age: tree.age,
                        log: blocks.log,
                        branch: blocks.branch,
                        leaves: blocks.leaves,
                    },
                    original_roots: roots.clone(),
                    surviving_roots: roots,
                    state: game::TreeState::Standing,
                },
            });
        }
    }
    (out, seeds)
}

/// Levels the samples (`samples[i * nz + j]` is world column `(x0 + i, z0 + j)`) under the structures'
/// pads, from each column's natural surface, and keeps trees off their keep-out. A padded column loses its
/// glacier ice; its water and cover stay. Returns, indexed like `samples`, whether each column lies in a
/// pad's box (see [`pad_sample`]).
fn pad_samples(
    samples: &mut [ColSample],
    near: &[&structures::StructurePlacement],
    (x0, z0): (i32, i32),
    nz: usize,
) -> Vec<bool> {
    samples
        .iter_mut()
        .enumerate()
        .map(|(k, c)| pad_sample(c, near, x0 + (k / nz) as i32, z0 + (k % nz) as i32))
        .collect()
}

/// Levels one sample, of world block column `(x, z)`, under the structures' pads and keeps trees off
/// their keep-out (see [`pad_samples`]). Every place generation samples a ground column pads it with this.
/// Returns whether the column lies in a pad's box: such a column never carries a slope (its top block is
/// the ground a structure stands on), wherever generation decides slopes.
fn pad_sample(c: &mut ColSample, near: &[&structures::StructurePlacement], x: i32, z: i32) -> bool {
    let mut boxed = false;
    if let structures::PadEffect::Level { surface, in_box } = structures::pad_at(near, x, z, c.s) {
        c.s = surface;
        c.top = surface;
        c.ice_from = c.water_top + 1;
        boxed = in_box;
    }
    if structures::blocks_trees(near, x, z, TREE_REACH_BLOCKS) {
        c.trees = false;
    }
    boxed
}

/// The paint of one (padded) sample, of world block column `(x, z)`: keeps trees off a painted column,
/// and returns the block its top terrain block takes — `None` when no paint holds it, or it lies in a
/// pad's box (`in_box`), or it has no ground, or water stands over it, or glacier ice tops it. Every
/// place generation samples a ground column paints it with this, after [`pad_sample`].
fn paint_sample(
    c: &mut ColSample,
    paints: &[structures::Paint],
    in_box: bool,
    x: i32,
    z: i32,
) -> Option<BlockId> {
    let block = structures::paint_at(paints, x, z)?;
    c.trees = false;
    (!in_box && has_ground(c) && c.water_top <= c.top && c.top == c.s).then_some(block)
}

/// The band `(lo, hi)` of the chunk column at world block `(x, z)` grown to the structures whose box
/// meets its footprint grown by one block (a carve beside it needs this column's wall stored): up to
/// the box's top block, down to one block under its bottom (a carve's floor faces stored ground), never
/// below `floor`.
fn structure_band(
    near: &[&structures::StructurePlacement],
    [x, z]: [i32; 2],
    (mut lo, mut hi): (i32, i32),
    floor: i32,
) -> (i32, i32) {
    let cb = CHUNK_BLOCKS as i32;
    for p in near {
        let (min, max) = p.bounds();
        if min[0] < x + cb + 1 && max[0] > x - 1 && min[2] < z + cb + 1 && max[2] > z - 1 {
            hi = hi.max(max[1] - 1);
            lo = lo.min((min[1] - 1).max(floor));
        }
    }
    (lo, hi)
}

/// Writes the structures' cells of world chunk `chunk` into its block grid and chisel map: a carve is
/// air, a write its block and chisel (a cell's old chisel — a slope's — goes unless the write has one).
/// Nothing at or below `bedrock(lx, lz)`, the bedrock y of that block column.
fn stamp_structures(
    near: &[&structures::StructurePlacement],
    chunk: [i32; 3],
    blocks: &mut [Block],
    chisel: &mut BTreeMap<BlockIndex, ChiselData>,
    bedrock: &dyn Fn(usize, usize) -> i32,
) {
    let cb = CHUNK_BLOCKS as i32;
    let lo = chunk.map(|c| c * cb);
    let above_bedrock = |bi: BlockIndex| {
        let (lx, ly, lz) = bi.xyz();
        lo[1] + ly as i32 > bedrock(lx, lz)
    };
    for p in near {
        let (min, max) = p.bounds();
        if (0..3).any(|a| min[a] >= lo[a] + cb || max[a] <= lo[a]) {
            continue;
        }
        let Some(stamp) = game::prefab::layout_chunk(&p.prefab, &p.at, chunk) else {
            continue;
        };
        for bi in stamp.carve.into_iter().filter(|&bi| above_bedrock(bi)) {
            blocks[bi.linear()] = Block::new(BlockId::AIR);
            chisel.remove(&bi);
        }
        for (bi, block, c) in stamp.writes.into_iter().filter(|w| above_bedrock(w.0)) {
            blocks[bi.linear()] = block;
            match c {
                Some(c) => {
                    chisel.insert(bi, c);
                }
                None => {
                    chisel.remove(&bi);
                }
            }
        }
    }
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
///
/// Structures from `structures` level the ground under their pads, keep trees off their keep-out, grow
/// the band over their boxes and are written after terrain and trees. Its paints recolour the top
/// terrain block of their columns (`paint_sample`) and keep trees off them; heights, slopes, water and
/// the `ColumnGround` stay. With no placement and no paint in reach of the window the output is exactly
/// the structure-free one.
fn columns_from_patch(
    patch: &dyn Patch,
    coords: RegionCoords,
    xs: std::ops::Range<usize>,
    zs: std::ops::Range<usize>,
    mats: Materials,
    trees_from: TreeSource,
    structures: &dyn structures::StructureSource,
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
    let (wx, wz) = (
        coords.x * REGION_BLOCKS + xs.start as i32 * cb,
        coords.z * REGION_BLOCKS + zs.start as i32 * cb,
    );
    let (ww, wd) = (xs.len() as i32 * cb, zs.len() as i32 * cb);
    let (nx, nz) = ((ww + 2 * ring) as usize, (wd + 2 * ring) as usize);
    let mut samples = Vec::with_capacity(nx * nz);
    for i in 0..nx {
        for j in 0..nz {
            samples.push(column_sample(
                patch,
                &mats,
                wx + i as i32 - ring,
                wz + j as i32 - ring,
            ));
        }
    }
    // Structures reaching the sampled ring (their pads) or its trees (their keep-out). None: the terrain
    // is exactly what it is without a structure source.
    let near_owned = structures.placements_in(wx, wz, wx + ww, wz + wd, ring + TREE_REACH_BLOCKS);
    let near: Vec<&structures::StructurePlacement> =
        near_owned.iter().map(|p| p.as_ref()).collect();
    // Paints over the sampled ring (the same query). None: the terrain is exactly what it is without.
    let paints = structures.paints_in(wx, wz, wx + ww, wz + wd, ring + TREE_REACH_BLOCKS);
    // Whether each sampled column lies in a pad's box, indexed like `samples` (empty: none does).
    let mut in_box = Vec::new();
    if !near.is_empty() {
        // The whole ring, before slopes: slopes, the cliff rule and tree surfaces read it.
        in_box = pad_samples(&mut samples, &near, (wx - ring, wz - ring), nz);
    }
    // Each sampled column's top-block paint, indexed like `samples` (empty: none is painted). The whole
    // ring, after the pads (a box beats a paint, a skirt is painted at its padded height): tree surfaces
    // read it.
    let mut paint_cells: Vec<Option<BlockId>> = Vec::new();
    if !paints.is_empty() {
        paint_cells = samples
            .iter_mut()
            .enumerate()
            .map(|(k, c)| {
                let boxed = in_box.get(k).copied().unwrap_or(false);
                paint_sample(
                    c,
                    &paints,
                    boxed,
                    wx - ring + (k / nz) as i32,
                    wz - ring + (k % nz) as i32,
                )
            })
            .collect();
    }
    let paint_of = |bx: i32, bz: i32| {
        paint_cells
            .get((bx + ring) as usize * nz + (bz + ring) as usize)
            .copied()
            .flatten()
    };
    let at = |bx: i32, bz: i32| samples[(bx + ring) as usize * nz + (bz + ring) as usize];
    // Each column's slope, over the ring minus its outer block (`None`
    // there, and in a pad's box), indexed like `samples`.
    let mut slope_cells = vec![None; nx * nz];
    for bx in 1 - ring..ww + ring - 1 {
        for bz in 1 - ring..wd + ring - 1 {
            let i = (bx + ring) as usize * nz + (bz + ring) as usize;
            if in_box.get(i).copied().unwrap_or(false) {
                continue;
            }
            slope_cells[i] = slopes::slope_for(&at(bx, bz), |dx, dz| at(bx + dx, bz + dz));
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
    let mut tree_life_cols: Vec<Vec<game::TreeLifeSeed>> =
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
        let (stamped, life) = tree_blocks(
            coords,
            [wx, wz, wx + ww, wz + wd],
            &mats,
            trees_from,
            &surface,
        );
        for ([x, y, z], placed) in stamped {
            let (bx, bz) = (x - wx, z - wz);
            let col = (bx / cb) as usize * zs.len() + (bz / cb) as usize;
            tree_cols[col].push(((bx % cb) as usize, (bz % cb) as usize, y, placed));
        }
        for seed in life {
            let bx = seed.life.genome.base[0];
            let bz = seed.life.genome.base[2];
            let col = (bx / cb - xs.start as i32) as usize * zs.len()
                + (bz / cb - zs.start as i32) as usize;
            tree_life_cols[col].push(seed);
        }
    }

    let mut columns = Vec::with_capacity(xs.len() * zs.len());
    for cx in xs.clone() {
        for cz in zs.clone() {
            // This Column covers CHUNK_BLOCKS x CHUNK_BLOCKS blocks, from
            // window-local block (fx, fz).
            let fx = (cx - xs.start) as i32 * cb;
            let fz = (cz - zs.start) as i32 * cb;
            let col_trees =
                std::mem::take(&mut tree_cols[(cx - xs.start) * zs.len() + (cz - zs.start)]);
            let col_life =
                std::mem::take(&mut tree_life_cols[(cx - xs.start) * zs.len() + (cz - zs.start)]);

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
                        open_lo = open_lo.min(if slope_at(nbx, nbz).is_some() {
                            n.top
                        } else {
                            n.top + 1
                        });
                    }
                }
            }
            // The band reaches the highest tree block too.
            hi = col_trees.iter().fold(hi, |hi, t| hi.max(t.2));
            lo = col_trees.iter().fold(lo, |lo, t| lo.min(t.2));
            // Extend the band bottom past the shallowest surface in the footprint
            // so the deepest soil (dirt/sand) of the lowest column is still
            // materialized, and down to every open neighbour block.
            lo = (lo - soil_max).min(open_lo);
            if !near.is_empty() {
                let floor = (0..cb * cb)
                    .map(|i| at(fx + i / cb, fz + i % cb).top - game::UNDERGROUND_DEPTH + 1)
                    .min()
                    .unwrap();
                (lo, hi) = structure_band(&near, [wx + fx, wz + fz], (lo, hi), floor);
            }

            // Materialize whole sub-chunks [base_sub_y ..= top_sub_y]; below is
            // the ground recipe, above air. Whole blocks, and a slope chisel
            // on the top block of a slope column.
            let base_sub_y = lo.div_euclid(cb);
            let top_sub_y = hi.div_euclid(cb);
            let n_sub = (top_sub_y - base_sub_y + 1) as usize;
            // Linear block grids + chisel maps per sub-chunk, made canonical
            // `Chunk`s once filled.
            let mut chunks: Vec<(Vec<Block>, BTreeMap<BlockIndex, ChiselData>)> = (0..n_sub)
                .map(|_| (vec![Block::default(); CHUNK_BLOCK_COUNT], BTreeMap::new()))
                .collect();

            let mut surface = vec![0i16; game::COLUMN_FOOTPRINT].into_boxed_slice();
            let mut geo = vec![0i16; game::COLUMN_FOOTPRINT].into_boxed_slice();
            let mut water_top = vec![game::NO_WATER; game::COLUMN_FOOTPRINT].into_boxed_slice();
            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let (bx, bz) = (fx + lx as i32, fz + lz as i32);
                    let c = at(bx, bz);
                    // The visible top: a glacier's ice counts as ground
                    // (digging, implied ground and pathing see the ice top).
                    surface[lx * CHUNK_BLOCKS + lz] = c.top as i16;
                    // Recipe water: the column's water (or ice) above its
                    // ground — the only water there is; the chunks below
                    // hold air there (ice at a frozen top).
                    if c.water_top > c.top {
                        water_top[lx * CHUNK_BLOCKS + lz] = c.water_top as i16;
                    }
                    geo[lx * CHUNK_BLOCKS + lz] =
                        patch.geo(block_center_m(wx + bx), block_center_m(wz + bz));
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
                water_top,
                water_body: vec![game::NO_BODY; game::COLUMN_FOOTPRINT].into_boxed_slice(),
            };

            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let c = at(fx + lx as i32, fz + lz as i32);
                    let slope = slope_at(fx + lx as i32, fz + lz as i32);
                    let paint = paint_of(fx + lx as i32, fz + lz as i32);
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
                            // slope column (a painted top in the paint's
                            // block); bare rock is the recipe's.
                            let block = match paint.filter(|_| by == c.top) {
                                Some(p) => Block::new(p),
                                None => match column_material(&mats, &c, by) {
                                    BlockId::AIR => ground.block_at(lx, lz, by),
                                    mat => Block::new(mat),
                                },
                            };
                            blocks[bi.linear()] = block;
                            if let Some(shape) = slope.filter(|_| by == c.top) {
                                chisel.insert(bi, game::slope_chisel(block.id, shape));
                            }
                        } else if by <= c.water_top && by >= c.ice_from {
                            // The frozen top of the water: ice, a solid block.
                            blocks[bi.linear()] = Block::new(mats.biome.ice);
                        }
                        // else: air (the grid starts all air) — the water
                        // under the surface included: it is the recipe's
                        // (`ColumnGround::water_top`), never a block.
                    }
                }
            }
            // Trees, into the air above the ground and any water only.
            for (lx, lz, by, placed) in col_trees {
                let c = at(fx + lx as i32, fz + lz as i32);
                if !placed.root && (by <= c.top || by <= c.water_top) {
                    continue;
                }
                let bi = BlockIndex::from_xyz(lx, by.rem_euclid(cb) as usize, lz);
                let (blocks, chisel) = &mut chunks[(by.div_euclid(cb) - base_sub_y) as usize];
                blocks[bi.linear()] = placed.block;
                // A tree block is whole: drop any chisel already in the cell (a slope's).
                chisel.remove(&bi);
            }
            // Structures, after terrain and trees, in the source's order (a later one wins).
            if !near.is_empty() {
                let rc = REGION_CHUNKS as i32;
                let chunk_xz = [coords.x * rc + cx as i32, coords.z * rc + cz as i32];
                let bedrock = |lx: usize, lz: usize| {
                    at(fx + lx as i32, fz + lz as i32).top - game::UNDERGROUND_DEPTH
                };
                for (sub_i, (blocks, chisel)) in chunks.iter_mut().enumerate() {
                    let sub_y = base_sub_y + sub_i as i32;
                    stamp_structures(
                        &near,
                        [chunk_xz[0], sub_y, chunk_xz[1]],
                        blocks,
                        chisel,
                        &bedrock,
                    );
                }
            }

            let subs: Vec<SubChunk> = chunks
                .into_iter()
                .map(|(blocks, chisel)| {
                    SubChunk::Dense(Box::new(Chunk {
                        blocks: ChunkBlocks::from_vec(blocks),
                        chisel,
                    }))
                })
                .collect();
            // `above = air` trims air tops; the bottom is kept (`grounded`).
            let column =
                Column::grounded(base_sub_y, stone, air, subs, ground).with_tree_life(col_life);
            columns.push((ColumnCoords::new(cx, cz), column));
        }
    }
    columns
}

/// Cheap coarse LOD generation for one block-scale-or-coarser key
/// (`game::lod::COARSE_MIN_FACTOR` and up): one profile sample per cell
/// column, surface scan, then solid-below-surface (+ the ice on frozen
/// water; liquid water is not LOD terrain — see [`coarse_water`]). No
/// block materialization, no chiseling. Pure and deterministic: same (key,
/// seed, mats) → identical cells everywhere. A 64/256 tile is exactly one 32³
/// grid per vertical band; a factor-16 region is 2×2 of them, grouped like
/// `game::lod::block_scale_cells`.
/// Point-sampling is NOT bit-consistent with downsampled full generation
/// (accepted, sub-pixel at these distances — see the design spec §3);
/// `coarse_tile_tracks_downsampled_full_generation` bounds the divergence.
pub fn coarse_tile(key: &game::lod::LodKey, seed: u64, mats: Materials) -> Vec<game::lod::LodCell> {
    if let Some(world) = forged::active() {
        return coarse_tile_forged(world, key, mats);
    }
    coarse_tile_with(key, seed, mats, structures::active())
}

/// `coarse_tile` with an explicit structure source and no forged world (procedural terrain from
/// `seed`). `coarse_tile` is this with `structures::active()` when no forged world is installed.
pub fn coarse_tile_with(
    key: &game::lod::LodKey,
    seed: u64,
    mats: Materials,
    structures: &dyn structures::StructureSource,
) -> Vec<game::lod::LodCell> {
    let (ox, oz, span) = coarse_patch_window(key, &mats, 0);
    let grid = TerrainPipeline::new(seed).profile_grid(ox, oz, span);
    let habitat = habitat::ProceduralHabitat::new(seed);
    coarse_from_patch(
        &ProceduralPatch(grid),
        key,
        mats,
        TreeSource {
            habitat: &habitat,
            seed,
            wrap: None,
        },
        structures,
        0,
    )
}

/// `coarse_tile` for an explicit forged world (tests, tools). A coarse cell is
/// point-sampled at its center, so rivers narrower than a cell come and go
/// with distance; lakes and valleys survive every ring.
pub fn coarse_tile_forged(
    world: &forged::ForgedWorld,
    key: &game::lod::LodKey,
    mats: Materials,
) -> Vec<game::lod::LodCell> {
    coarse_tile_forged_with(world, key, mats, structures::active())
}

/// `coarse_tile_forged` with an explicit structure source.
pub fn coarse_tile_forged_with(
    world: &forged::ForgedWorld,
    key: &game::lod::LodKey,
    mats: Materials,
    structures: &dyn structures::StructureSource,
) -> Vec<game::lod::LodCell> {
    // One cell column of margin past the key, for the steep-face fill's
    // neighbours (rounded to the profile lattice, so surfaces stay
    // bit-identical to any other window's).
    let ring = coarse_ring(key);
    let shift = coarse_shift(world, key);
    let (ox, oz, span) = coarse_patch_window(key, &mats, shift);
    let (ox, oz, span) = (ox - ring as f64, oz - ring as f64, span + 2.0 * ring as f64);
    let trees_from = TreeSource::forged(world);
    let structures = wrapped_structures(world, structures);
    coarse_from_patch(
        &world.patch(ox, oz, span),
        key,
        mats,
        trees_from,
        &structures,
        shift,
    )
}

/// `wrap_shift_blocks` of a coarse key's window: LOD keys arrive in the
/// client's frame, unwrapped past the seam; the key's terrain is its
/// canonical image's.
fn coarse_shift(world: &forged::ForgedWorld, key: &game::lod::LodKey) -> i32 {
    wrap_shift_blocks(world, key.origin_region().x * REGION_BLOCKS)
}

/// The cell-column margin (blocks) `coarse_tile_forged` adds past a key,
/// for the steep-face fill's neighbours, rounded up to the profile lattice.
fn coarse_ring(key: &game::lod::LodKey) -> i32 {
    let cb = (key.factor / BLOCK_VOXELS as u32) as i32;
    let lattice = PROFILE_LATTICE_BLOCKS as i32;
    (cb + lattice - 1) / lattice * lattice
}

/// The water surface layer of LOD key `key` (water plan Task 8), from the
/// recipe: one `point_sample` per LOD column, at the point
/// [`coarse_tile`] samples it (a block-scale-or-coarser cell's centre); a
/// sub-block key samples each block column once, at its centre as region
/// generation does, and repeats it over the block's LOD columns (water is
/// whole blocks). A column holds water iff its water top is above its
/// ground (`water_top > top`), with depth `water_top − top` blocks
/// (clamped to [`game::lod::LOD_WATER_MAX_DEPTH_BLOCKS`]); a
/// column whose top water block is ice is dry here (the ice is LOD terrain).
/// Pure: the same water on every build path, whatever the region's chunks.
pub fn coarse_water(key: &game::lod::LodKey, seed: u64, mats: Materials) -> game::lod::LodWater {
    if let Some(world) = forged::active() {
        return coarse_water_forged(world, key, mats);
    }
    coarse_water_with(key, seed, mats, structures::active())
}

/// `coarse_water` with an explicit structure source and no forged world (procedural terrain from
/// `seed`): a pad that raises ground to or above the water top removes the water there, as it does in
/// full generation.
pub fn coarse_water_with(
    key: &game::lod::LodKey,
    seed: u64,
    mats: Materials,
    structures: &dyn structures::StructureSource,
) -> game::lod::LodWater {
    let (ox, oz, span) = coarse_window(key, 0);
    let grid = TerrainPipeline::new(seed).profile_grid(ox as f64, oz as f64, span as f64);
    water_from_patch(&ProceduralPatch(grid), key, &mats, structures, 0)
}

/// `coarse_water` for an explicit forged world (tests, tools).
pub fn coarse_water_forged(
    world: &forged::ForgedWorld,
    key: &game::lod::LodKey,
    mats: Materials,
) -> game::lod::LodWater {
    coarse_water_forged_with(world, key, mats, structures::active())
}

/// `coarse_water_forged` with an explicit structure source.
pub fn coarse_water_forged_with(
    world: &forged::ForgedWorld,
    key: &game::lod::LodKey,
    mats: Materials,
    structures: &dyn structures::StructureSource,
) -> game::lod::LodWater {
    let shift = coarse_shift(world, key);
    let (ox, oz, span) = coarse_window(key, shift);
    let structures = wrapped_structures(world, structures);
    water_from_patch(
        &world.patch(ox as f64, oz as f64, span as f64),
        key,
        &mats,
        &structures,
        shift,
    )
}

/// One LOD column's `(water_top, depth)` (see [`coarse_water`]).
fn lod_water_column(c: &ColSample) -> (i16, u8) {
    if c.water_top <= c.top || c.ice_from <= c.water_top {
        return (game::NO_WATER, 0);
    }
    let max = i32::from(game::lod::LOD_WATER_MAX_DEPTH_BLOCKS);
    (c.water_top as i16, (c.water_top - c.top).min(max) as u8)
}

fn water_from_patch(
    patch: &dyn Patch,
    key: &game::lod::LodKey,
    mats: &Materials,
    structures: &dyn structures::StructureSource,
    shift: i32,
) -> game::lod::LodWater {
    let side = game::lod::LodWater::side_for(key);
    let n = usize::from(side);
    let (ox, oz, span_blocks) = coarse_window(key, shift);
    // Structures whose pads reach the key: every sample is inside it. None: the water is exactly what it
    // is without a structure source.
    let near_owned = structures.placements_in(ox, oz, ox + span_blocks, oz + span_blocks, 0);
    let near: Vec<&structures::StructurePlacement> =
        near_owned.iter().map(|p| p.as_ref()).collect();
    let padded = |mut c: ColSample, x: i32, z: i32| {
        if !near.is_empty() {
            pad_sample(&mut c, &near, x, z);
        }
        c
    };
    let mut tops = Vec::with_capacity(n * n);
    if key.factor >= BLOCK_VOXELS as u32 {
        // Blocks per cell edge, and the cell centre `coarse_from_patch` samples (in the block column
        // `ox + c * cb + cb / 2`).
        let cb = (key.factor / BLOCK_VOXELS as u32) as i32;
        debug_assert_eq!(span_blocks / cb, i32::from(side));
        let centre = |o: i32, c: i32| (o + c * cb) as f64 + cb as f64 * 0.5;
        for cz in 0..i32::from(side) {
            for cx in 0..i32::from(side) {
                let c = point_sample(patch, mats, centre(ox, cx), centre(oz, cz));
                tops.push(lod_water_column(&padded(
                    c,
                    ox + cx * cb + cb / 2,
                    oz + cz * cb + cb / 2,
                )));
            }
        }
    } else {
        let per = (BLOCK_VOXELS as u32 / key.factor) as usize; // LOD columns per block edge
        let blocks = span_blocks as usize;
        debug_assert_eq!(blocks * per, n);
        let mut row = Vec::with_capacity(blocks);
        for bz in 0..span_blocks {
            row.clear();
            row.extend((0..span_blocks).map(|bx| {
                lod_water_column(&padded(
                    column_sample(patch, mats, ox + bx, oz + bz),
                    ox + bx,
                    oz + bz,
                ))
            }));
            for _ in 0..per {
                for &top in &row {
                    tops.extend(std::iter::repeat_n(top, per));
                }
            }
        }
    }
    game::lod::LodWater::encode(&tops, side)
}

/// World block origin and span of a coarse key, its x moved `shift` blocks
/// west (`coarse_shift`: the canonical image of a key on a world that wraps
/// east–west; 0 otherwise).
fn coarse_window(key: &game::lod::LodKey, shift: i32) -> (i32, i32, i32) {
    let origin = key.origin_region();
    (
        origin.x * REGION_BLOCKS - shift,
        origin.z * REGION_BLOCKS,
        key.region_span() * REGION_BLOCKS,
    )
}

/// The patch a coarse key samples: its window, plus the trees' margin.
fn coarse_patch_window(key: &game::lod::LodKey, mats: &Materials, shift: i32) -> (f64, f64, f64) {
    let (ox, oz, span) = coarse_window(key, shift);
    let m = if mats.trees.enabled() {
        TREE_PATCH_MARGIN
    } else {
        0
    };
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

/// The coarse tile of `key` from `patch` (see [`coarse_tile`]). Structures from `structures` level the
/// sampled ground under their pads, keep trees off their keep-out, and stand as each prefab column's
/// solid silhouette above the terrain. A painted cell column (its centre block column, `paint_sample`)
/// shows the paint in the cell holding its terrain top and carries no tree. With no placement and no
/// paint in reach of the tile the output is exactly the structure-free one.
fn coarse_from_patch(
    patch: &dyn Patch,
    key: &game::lod::LodKey,
    mats: Materials,
    trees_from: TreeSource,
    structures: &dyn structures::StructureSource,
    shift: i32,
) -> Vec<game::lod::LodCell> {
    use game::Voxel;
    use game::lod::LodCell;

    // A LOD grid's edge in cells: the 32³ render/LOD brick.
    const G: i32 = BRICK_VOXELS as i32;
    /// The voxel of cell `[cx, cy, cz]` (key-local), its 32³ grid made (all air) if missing.
    fn cell_mut(grids: &mut BTreeMap<[i32; 3], Vec<Voxel>>, [cx, cy, cz]: [i32; 3]) -> &mut Voxel {
        let group = [cx.div_euclid(G), cy.div_euclid(G), cz.div_euclid(G)];
        let (lx, ly, lz) = (
            cx.rem_euclid(G) as usize,
            cy.rem_euclid(G) as usize,
            cz.rem_euclid(G) as usize,
        );
        let g = grids
            .entry(group)
            .or_insert_with(|| vec![Voxel::default(); BRICK_VOXEL_COUNT]);
        &mut g[lx + BRICK_VOXELS * ly + BRICK_VOXELS * BRICK_VOXELS * lz]
    }
    // Blocks per cell edge: 1 at factor 16, 2 at 32, 4 at 64, 16 at 256.
    debug_assert!(
        key.factor >= game::lod::COARSE_MIN_FACTOR,
        "sub-block factor {}",
        key.factor
    );
    let cb = (key.factor / BLOCK_VOXELS as u32) as i32;
    let span_blocks = key.region_span() * REGION_BLOCKS;
    let cells = span_blocks / cb; // cell columns per key edge: 64 at factor 16, else 32
    debug_assert_eq!(cells % G, 0, "a key is a whole number of 32³ grids");

    let (ox, oz, _) = coarse_window(key, shift);
    // Tree placement reads surfaces up to `tree_m` past the key; each surface's
    // slope reads one block further (still inside the patch's
    // `TREE_PATCH_MARGIN`).
    let tree_m = TREE_PATCH_MARGIN - PATCH_MARGIN;

    // Structures reaching any column the tile samples (their pads: cell columns
    // one out, tree surfaces and their slopes) or a tree surface's keep-out.
    // None: the tile is exactly what it is without a structure source.
    let margin = (tree_m + TREE_REACH_BLOCKS).max(cb);
    let near_owned = structures.placements_in(ox, oz, ox + span_blocks, oz + span_blocks, margin);
    let near: Vec<&structures::StructurePlacement> =
        near_owned.iter().map(|p| p.as_ref()).collect();
    // Paints over the same columns. None: the tile is exactly what it is without.
    let paints = structures.paints_in(ox, oz, ox + span_blocks, oz + span_blocks, margin);
    // Pads and paints `c`, the sample of world block column (x, z); with whether the column lies in a
    // pad's box and its top block's paint.
    let padded = |mut c: ColSample, x: i32, z: i32| {
        let in_box = !near.is_empty() && pad_sample(&mut c, &near, x, z);
        let paint = if paints.is_empty() {
            None
        } else {
            paint_sample(&mut c, &paints, in_box, x, z)
        };
        (c, in_box, paint)
    };

    // Centre (world metres) of cell column (cx, cz).
    let centre = |cx: i32, cz: i32| {
        (
            (ox + cx * cb) as f64 + cb as f64 * 0.5,
            (oz + cz * cb) as f64 + cb as f64 * 0.5,
        )
    };
    // The block column holding that centre: where structures read the cell column.
    let centre_block = |cx: i32, cz: i32| (ox + cx * cb + cb / 2, oz + cz * cb + cb / 2);
    // The column of cell column (cx, cz), sampled at its centre (with
    // whether it is unbounded); cell columns just outside the key are
    // sampled too.
    let cell_sample = |cx: i32, cz: i32| {
        let (mx, mz) = centre(cx, cz);
        let (x, z) = centre_block(cx, cz);
        let (c, _, paint) = padded(point_sample(patch, &mats, mx, mz), x, z);
        (c, paint)
    };
    // With each cell column's top-block paint.
    let samples: Vec<(ColSample, Option<BlockId>)> = (0..cells)
        .flat_map(|cz| (0..cells).map(move |cx| (cx, cz)))
        .map(|(cx, cz)| cell_sample(cx, cz))
        .collect();
    let surface_of = |cx: i32, cz: i32| {
        if (0..cells).contains(&cx) && (0..cells).contains(&cz) {
            samples[(cz * cells + cx) as usize].0.top
        } else {
            cell_sample(cx, cz).0.top
        }
    };
    let strata = patch.strata(&mats);

    let mut grids: BTreeMap<[i32; 3], Vec<Voxel>> = Default::default();
    for cz in 0..cells {
        for cx in 0..cells {
            let (c, paint) = samples[(cz * cells + cx) as usize];
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
                let mat = if let Some(p) = paint.filter(|_| cy == solid_hi) {
                    // The cell holding a painted terrain top: the paint.
                    p
                } else if is_solid {
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
                    // Likewise the cell's topmost water block — but only ice
                    // is LOD terrain: a liquid cell is air (the bed shows),
                    // its surface rides in the key's `LodWater`
                    // (`coarse_water`).
                    let by = water_top.min(cy * cb + cb - 1);
                    if by < c.ice_from {
                        continue;
                    }
                    mats.biome.ice
                };
                *cell_mut(&mut grids, [cx, cy, cz]) = Voxel::new(mat);
            }
        }
    }

    // Structures: each prefab column's silhouette over its cell column's
    // centre block column, from the first cell above the (padded) terrain top
    // through the cell holding the prefab column's top block, in its top
    // block's material; into air cells only, a later placement winning.
    if !near.is_empty() {
        for cz in 0..cells {
            for cx in 0..cells {
                let (x, z) = centre_block(cx, cz);
                let first = samples[(cz * cells + cx) as usize].0.top.div_euclid(cb) + 1;
                let mut profile: BTreeMap<i32, BlockId> = BTreeMap::new();
                for p in &near {
                    let (min, max) = p.bounds();
                    if x < min[0] || x >= max[0] || z < min[2] || z >= max[2] {
                        continue;
                    }
                    let Some((top, mat)) = game::prefab::column_top_world(&p.prefab, &p.at, x, z)
                    else {
                        continue;
                    };
                    for cy in first..=top.div_euclid(cb) {
                        profile.insert(cy, mat);
                    }
                }
                for (cy, mat) in profile {
                    let v = cell_mut(&mut grids, [cx, cy, cz]);
                    if v.is_air() {
                        *v = Voxel::new(mat);
                    }
                }
            }
        }
    }

    // Trees: each a leaves column from its crown's base to its top over its
    // crown disc (and always over its trunk), into air cells only.
    if mats.trees.enabled() {
        let m = tree_m;
        let sample_memo = std::cell::RefCell::new(std::collections::HashMap::new());
        let sample = |x: i32, z: i32| {
            assert!(
                (ox - m - 1..ox + span_blocks + m + 1).contains(&x)
                    && (oz - m - 1..oz + span_blocks + m + 1).contains(&z),
                "a tree surface's slope read the column ({x}, {z}), outside the coarse patch"
            );
            let cached = sample_memo.borrow().get(&(x, z)).copied();
            cached.unwrap_or_else(|| {
                let c = padded(column_sample(patch, &mats, x, z), x, z);
                sample_memo.borrow_mut().insert((x, z), c);
                c
            })
        };
        let memo = std::cell::RefCell::new(std::collections::HashMap::new());
        let surface = |x: i32, z: i32| {
            assert!(
                (ox - m..ox + span_blocks + m).contains(&x)
                    && (oz - m..oz + span_blocks + m).contains(&z),
                "tree placement read the surface at ({x}, {z}), outside the coarse patch"
            );
            let cached = memo.borrow().get(&(x, z)).copied();
            cached.unwrap_or_else(|| {
                // A column in a pad's box is never a slope, as in full generation.
                let (c, in_box, _) = sample(x, z);
                let slope =
                    !in_box && slopes::slope_for(&c, |dx, dz| sample(x + dx, z + dz).0).is_some();
                let g = tree_ground(c, slope);
                memo.borrow_mut().insert((x, z), g);
                g
            })
        };
        let memo_habitat = MemoHabitat {
            inner: trees_from.habitat,
            memo: Default::default(),
        };
        let (x1, z1) = (ox + span_blocks, oz + span_blocks);
        for tree in trees::trees_in(
            ox,
            oz,
            x1,
            z1,
            trees_from.seed,
            trees_from.wrap,
            &memo_habitat,
            &surface,
        ) {
            let Some((lo, hi, radius)) = crown_extent(&tree) else {
                continue;
            };
            let leaves = Voxel::new(mats.trees.species[tree.species as usize].leaves);
            let [bx, by, bz] = tree.base;
            let (cy0, cy1) = ((by + lo).div_euclid(cb), (by + hi).div_euclid(cb));
            let cells_of = |a: i32, o: i32| {
                ((a - radius).max(o) - o) / cb..=((a + radius).min(o + span_blocks - 1) - o) / cb
            };
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
                        let v = cell_mut(&mut grids, [cx, cy, cz]);
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
