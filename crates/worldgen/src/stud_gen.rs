//! The terrain generator: stud ground recipes from the crate's column sampling — the
//! forged world (or the procedural pipeline) read at stud columns, surfaces quantised to
//! plates.
//!
//! Biome covers, soils, snow, glaciers, lake beds and rock strata come from
//! `profile_sample` / `column_material` (1 m block units), fed a material table in stud
//! material ids ([`stud_materials`]). The result for a chunk column does not depend on
//! the window it was generated in.
//!
//! Trees come from `stud_trees.rs` (`generate_stud_region` only: a region's features).

use game::{
    ColumnCoords, StudColumnGround, StudProfile, StudRegionSeed, CHUNK_BLOCKS, MAT_STONE, REGION_CHUNKS, STUD_CHUNK_STUDS,
    STUD_NO_WATER,
};

use super::*;
use crate::forged::ForgedWorld;
use crate::pipeline::TerrainPipeline;

/// Plates per metre (a plate is 0.2 m); a sampled block column is one metre.
pub const PLATES_PER_M: i32 = 5;
/// Stud columns per metre (a stud is 0.5 m).
pub const STUDS_PER_M: i32 = 2;
/// Thickness of a column's cover material: one brick course.
pub const COVER_PLATES: u8 = 3;
/// Plates below this are bedrock: under the deepest forged sea floor.
pub const STUD_BEDROCK_PLATE: i32 = (FORGED_SCAN_BOTTOM - 256) * PLATES_PER_M;
/// Plates the surface scan starts above the analytic surface.
const SCAN_HEADROOM_PLATES: i32 = 2;
const N: usize = STUD_CHUNK_STUDS;
const _: () = assert!(STUD_CHUNK_STUDS as i32 == CHUNK_BLOCKS as i32 * STUDS_PER_M || CHUNK_BLOCKS != 16);

/// The worldgen material table in stud material ids (`BlockId(id)`), resolved by
/// material name.
pub fn stud_materials() -> Materials {
    Materials::from_names(|name| BlockId(game::stud_material_id(name).unwrap_or(0)))
}

/// One past the top solid plate of a profile: the highest plate that is at least half
/// full, plus one. The scan-band bottom when the column has no ground.
fn surface_plate(prof: &crate::pipeline::ColumnProfile) -> i32 {
    let (top, bottom) = scan_band(prof);
    let (top, bottom) = (top * PLATES_PER_M, bottom * PLATES_PER_M);
    let solid = |p: i32| prof.density_at((p as f64 + 0.5) / PLATES_PER_M as f64) > 0.0;
    // `as i32` saturates (NaN -> 0); the clamp covers both.
    let analytic = ((prof.surface_y() * PLATES_PER_M as f64).ceil() as i32).saturating_add(SCAN_HEADROOM_PLATES);
    let mut p = analytic.clamp(bottom, top);
    // Density falls with height, so nothing above a non-solid plate is solid.
    while p < top && solid(p) {
        p += 1;
    }
    while p >= bottom && !solid(p) {
        p -= 1;
    }
    p + 1
}

fn mat(id: BlockId) -> Option<u16> {
    (id != BlockId::AIR).then_some(id.0)
}

fn plates(blocks: i32) -> u8 {
    (blocks.max(0) * PLATES_PER_M).min(u8::MAX as i32) as u8
}

/// The soil stack of sampled column `c` and the plates of glacier ice standing on it.
fn profile_of(mats: &Materials, c: &ColSample) -> (StudProfile, i32) {
    let glacier = (c.top - c.s).max(0) * PLATES_PER_M;
    let surface = mat(column_material(mats, c, c.s));
    let below = mat(column_material(mats, c, c.s - 1));
    let soil_blocks = soil_depth(c).min((1..=soil_depth(c)).take_while(|&d| column_material(mats, c, c.s - d) != BlockId::AIR).count() as i32);
    let (soil, soil_plates) = match below {
        Some(m) => (m, plates(soil_blocks)),
        None => (MAT_STONE, 0),
    };
    let profile = if glacier > 0 {
        // Ice over the ground: the ice is the cover, the ground's own cover joins the soil.
        let ice = mat(mats.biome.glacier_ice).unwrap_or(MAT_STONE);
        StudProfile { cover: ice, cover_plates: glacier.min(u8::MAX as i32) as u8, soil, soil_plates }
    } else {
        match surface {
            Some(cover) => StudProfile { cover, cover_plates: COVER_PLATES, soil, soil_plates },
            None => StudProfile { cover: MAT_STONE, cover_plates: 0, soil, soil_plates },
        }
    };
    (profile, glacier)
}

/// The rock formations of `patch` as a recipe's `strata`: top-down `(top plate, material)`.
fn stud_strata(patch: &dyn Patch, mats: &Materials) -> Vec<(i32, u16)> {
    let mut out: Vec<(i32, u16)> = Vec::new();
    for (top, id) in patch.strata(mats) {
        let top = if top == i16::MAX { i32::MAX } else { i32::from(top) * PLATES_PER_M };
        // Tops must fall strictly; a formation that does not start lower is hidden.
        if out.last().is_none_or(|&(prev, _)| top < prev) {
            out.push((top, mat(id).unwrap_or(MAT_STONE)));
        }
    }
    if out.is_empty() {
        out.push((i32::MAX, MAT_STONE));
    }
    out
}

/// One stud column of a recipe, sampled at world metres `(x, z)` (the column's centre).
struct StudCol {
    /// One past the top solid plate.
    surface: i32,
    /// One past the top water plate where water stands over the ground.
    water: Option<i32>,
    profile: StudProfile,
    /// Stratigraphic offset, plates.
    geo: i32,
}

/// The recipe values of the stud column centred on world metres `(x, z)`: what both the
/// ground recipes and the far-terrain samples ([`stud_lod_tile`]) are made of.
fn stud_column(patch: &dyn Patch, mats: &Materials, x: f64, z: f64) -> StudCol {
    let (c, prof) = profile_sample(patch, mats, x, z);
    let (profile, glacier) = profile_of(mats, &c);
    let surface = surface_plate(&prof) + glacier;
    let water_top = (c.water_top + 1) * PLATES_PER_M;
    StudCol {
        surface,
        water: (surface < water_top).then_some(water_top),
        profile,
        geo: i32::from(patch.geo(x, z)) * PLATES_PER_M,
    }
}

/// Centre (world metres) of world stud column `stud` along one axis.
fn stud_centre_m(stud: i64) -> f64 {
    (stud as f64 + 0.5) / STUDS_PER_M as f64
}

fn recipes_from_patch(
    patch: &dyn Patch,
    coords: RegionCoords,
    xs: std::ops::Range<usize>,
    zs: std::ops::Range<usize>,
    mats: &Materials,
) -> Vec<(ColumnCoords, StudColumnGround)> {
    let strata = stud_strata(patch, mats);
    let region_studs = REGION_BLOCKS as i64 * STUDS_PER_M as i64;
    let mut out = Vec::with_capacity(xs.len() * zs.len());
    for cx in xs {
        for cz in zs.clone() {
            let mut surface = vec![0i32; N * N];
            let mut water = vec![STUD_NO_WATER; N * N];
            let mut profile = vec![0u8; N * N];
            let mut geo = vec![0i32; N * N];
            let mut profiles: Vec<StudProfile> = Vec::new();
            for sz in 0..N {
                for sx in 0..N {
                    let i = sz * N + sx;
                    let stud = |region: i32, c: usize, s: usize| region as i64 * region_studs + (c * N + s) as i64;
                    let (x, z) = (stud_centre_m(stud(coords.x, cx, sx)), stud_centre_m(stud(coords.z, cz, sz)));
                    let col = stud_column(patch, mats, x, z);
                    let p = col.profile;
                    surface[i] = col.surface;
                    if let Some(water_top) = col.water {
                        water[i] = water_top;
                    }
                    geo[i] = col.geo;
                    profile[i] = match profiles.iter().position(|q| *q == p) {
                        Some(k) => k as u8,
                        // A palette of 256 stacks per chunk column; more reuse the last.
                        None if profiles.len() > u8::MAX as usize => u8::MAX,
                        None => {
                            profiles.push(p);
                            (profiles.len() - 1) as u8
                        }
                    };
                }
            }
            let ground = StudColumnGround::new(
                surface.into(),
                water.into(),
                profile.into(),
                profiles,
                geo.into(),
                strata.clone(),
                STUD_BEDROCK_PLATE,
            )
            .expect("generated stud recipes are valid by construction");
            out.push((ColumnCoords::new(cx, cz), ground));
        }
    }
    out
}

/// The stud ground recipes of chunk columns `xs × zs` of region `coords`: the same
/// recipes whatever window they are generated in. With a forged world installed they
/// come from it and `seed` is ignored.
pub fn generate_stud_columns(
    coords: RegionCoords,
    xs: std::ops::Range<usize>,
    zs: std::ops::Range<usize>,
    seed: u64,
) -> Vec<(ColumnCoords, StudColumnGround)> {
    let mats = stud_materials();
    let cb = CHUNK_BLOCKS as i32;
    let (x, z) = (coords.x * REGION_BLOCKS + xs.start as i32 * cb, coords.z * REGION_BLOCKS + zs.start as i32 * cb);
    let (ox, oz, span) = patch_window(x, z, xs.len().max(zs.len()) as i32 * cb, PATCH_MARGIN);
    if let Some(world) = forged::active() {
        return recipes_from_patch(&world.patch(ox, oz, span), coords, xs, zs, &mats);
    }
    let grid = TerrainPipeline::new(seed).profile_grid(ox, oz, span);
    recipes_from_patch(&ProceduralPatch(grid), coords, xs, zs, &mats)
}

/// What a fresh stud region of the forged (or procedural) world starts from: the
/// recipes of [`generate_stud_columns`] for the whole region, plus its trees as feature
/// bricks (`stud_trees.rs`). A pure function of the region (and the installed forged
/// world).
pub fn generate_stud_region(coords: RegionCoords, seed: u64) -> StudRegionSeed {
    if let Some(world) = forged::active() {
        return generate_stud_region_forged(world, coords);
    }
    let (ox, oz, span) = region_patch_window(coords);
    let patch = ProceduralPatch(TerrainPipeline::new(seed).profile_grid(ox, oz, span));
    let habitat = habitat::ProceduralHabitat::new(seed);
    region_from_patch(&patch, coords, TreeSource { habitat: &habitat, seed })
}

/// [`generate_stud_region`] for an explicit forged world (tests, tools).
pub fn generate_stud_region_forged(world: &ForgedWorld, coords: RegionCoords) -> StudRegionSeed {
    let (ox, oz, span) = region_patch_window(coords);
    region_from_patch(&world.patch(ox, oz, span), coords, TreeSource { habitat: world, seed: world.seed() })
}

/// The patch of a whole region: one for recipes and trees, wide enough for tree
/// placement. The wider margin is a whole number of profile lattice steps, so the
/// recipes are bit-identical to `generate_stud_columns`' (pinned by
/// `stud_trees::a_region_is_its_columns_plus_trees`).
fn region_patch_window(coords: RegionCoords) -> (f64, f64, f64) {
    patch_window(coords.x * REGION_BLOCKS, coords.z * REGION_BLOCKS, REGION_BLOCKS, TREE_PATCH_MARGIN)
}

fn region_from_patch(patch: &dyn Patch, coords: RegionCoords, trees_from: TreeSource) -> StudRegionSeed {
    let mats = stud_materials();
    let all = 0..REGION_CHUNKS;
    let ground = recipes_from_patch(patch, coords, all.clone(), all, &mats);
    let features = crate::stud_trees::stud_tree_features(patch, coords, &mats, trees_from, &ground);
    StudRegionSeed { ground, features }
}

/// Far terrain of LOD key `key` in a stud world (`game::stud_lod`): the recipe's own
/// values ([`stud_column`]) at the stud columns whose min corners are the key's
/// cell-corner lattice points — `side + 1` per edge, the last row and column lying in
/// the neighbouring key. So a sample equals what [`generate_stud_columns`] gives that
/// stud column (top, top material, standing water), whatever key or factor it is read
/// through. With a forged world installed it comes from it and `seed` is ignored.
/// Trees are not in it.
pub fn stud_lod_tile(key: &game::lod::LodKey, seed: u64) -> game::StudLodTile {
    if let Some(world) = forged::active() {
        return stud_lod_tile_forged(world, key);
    }
    let (px, pz, pspan) = lod_patch_window(key);
    let grid = TerrainPipeline::new(seed).profile_grid(px, pz, pspan);
    lod_tile_from_patch(&ProceduralPatch(grid), key, &stud_materials())
}

/// [`stud_lod_tile`] for an explicit forged world (tests, tools).
pub fn stud_lod_tile_forged(world: &ForgedWorld, key: &game::lod::LodKey) -> game::StudLodTile {
    let (px, pz, pspan) = lod_patch_window(key);
    lod_tile_from_patch(&world.patch(px, pz, pspan), key, &stud_materials())
}

/// The patch of a LOD key (`prefetch_lod_forged` pins the same rect).
fn lod_patch_window(key: &game::lod::LodKey) -> (f64, f64, f64) {
    let (ox, oz, span) = lod_window(key);
    patch_window(ox, oz, span, PATCH_MARGIN)
}

fn lod_tile_from_patch(patch: &dyn Patch, key: &game::lod::LodKey, mats: &Materials) -> game::StudLodTile {
    assert!(key.factor >= game::STUD_LOD_MIN_FACTOR, "stud far terrain at factor {}: finer than a stud", key.factor);
    let side = game::StudLodTile::side_for(key);
    let cell_studs = i64::from(key.factor / game::STUD_LOD_MIN_FACTOR);
    let (ox, oz, span) = lod_window(key);
    debug_assert_eq!(i64::from(side) * cell_studs, i64::from(span) * i64::from(STUDS_PER_M));
    let strata = stud_strata(patch, mats);
    let n = i64::from(side) + 1;
    let mut samples = Vec::with_capacity((n * n) as usize);
    for cz in 0..n {
        for cx in 0..n {
            let stud = |origin_blocks: i32, c: i64| i64::from(origin_blocks) * i64::from(STUDS_PER_M) + c * cell_studs;
            let col = stud_column(patch, mats, stud_centre_m(stud(ox, cx)), stud_centre_m(stud(oz, cz)));
            let material = game::stud_top_material(col.surface, &col.profile, col.geo, &strata, STUD_BEDROCK_PLATE);
            samples.push(game::StudLodSample::of_column(col.surface, material, col.water));
        }
    }
    game::StudLodTile::encode(side, &samples)
}
