//! A world map tile's surface from worldgen alone (plan 2026-11-20-block-level-world-map, Task 3): per
//! sample, the top block's top, the top block, the water over it and whether that water is ice — the
//! terrain where no stored chunk says otherwise (`map-tiles` overlays those). Each sample is read at its
//! own centre: a level is sampled directly, never averaged from the level below.
//!
//! Levels up to [`SURFACE_PATCH_MAX_LEVEL`] read one patch over the tile, the profile lattice region
//! generation reads, so level 0 is generation's own top bit for bit (trees aside: pristine canopies are
//! not stamped). Above it a tile's patch would hold millions of lattice corners, so each sample is read
//! exactly at its point ([`crate::forged::PointPatch`]; procedural: a one-cell lattice around it).

use game::map_tile::{sample_flags, MapSample, MapTile, MapTileKey, MAP_TILE_SAMPLES};
use game::{BlockId, REGION_BLOCKS};

use crate::forged::{ForgedWorld, PointPatch};
use crate::material::Materials;
use crate::pipeline::{TerrainPipeline, PROFILE_LATTICE_BLOCKS};
use crate::{column_material, patch_margin, point_sample, wrap_frame, wrap_shift_blocks, ColSample, Patch, ProceduralPatch};

/// The regions `world` covers (canonical; x wraps when the world does), as the world map's extent.
pub fn map_extent(world: &ForgedWorld) -> game::MapExtent {
    let (origin, width, height) = crate::tile_map::map_extent(world);
    game::MapExtent { origin, width, height, wrap_x: world.globals().edges[1] == crate::forged::EdgeKind::Wrap }
}

/// The highest level read through one lattice patch per tile (1024 m at 4 m: 257² corners).
pub const SURFACE_PATCH_MAX_LEVEL: u8 = 2;

const EDGE: usize = MAP_TILE_SAMPLES as usize;

/// Engine metres of sample `i`'s centre along an axis from the tile's min block `o`.
fn centre_m(o: i64, i: usize, sample_m: i64) -> f64 {
    (o + i as i64 * sample_m) as f64 + sample_m as f64 * 0.5
}

/// The map sample of a generated column.
fn map_sample(mats: &Materials, c: &ColSample) -> MapSample {
    let block = if c.top > c.s {
        mats.biome.glacier_ice
    } else {
        match column_material(mats, c, c.s) {
            BlockId::AIR => mats.stone, // bare rock: its recipe rock, shown as stone
            id => id,
        }
    };
    let water = (c.water_top - c.top).clamp(0, u8::MAX as i32) as u8;
    let mut flags = 0;
    if water > 0 && c.ice_from <= c.water_top {
        flags |= sample_flags::ICE;
    }
    MapSample { height: (c.top + 1).clamp(i16::MIN as i32, i16::MAX as i32) as i16, block, water, flags }
}

/// Every sample of `key` from the forged `world` alone (no stored terrain): height = the top block's top
/// (ground, or glacier ice over it), block = that block, water = its depth over the top when positive,
/// `ICE` when that water's top is frozen, `OUTSIDE` past the world's edge. Sampled at each sample's centre.
pub fn surface_tile_forged(world: &ForgedWorld, key: MapTileKey, mats: &Materials) -> MapTile {
    let mut tile = MapTile::blank(key);
    let (ox, oz) = key.origin_block();
    let sm = key.sample_m();
    let (origin, w, h) = crate::tile_map::map_extent(world);
    let rb = REGION_BLOCKS as i64;
    let (x0, z0) = (origin.x as i64 * rb, origin.z as i64 * rb);
    let (x1, z1) = (x0 + w as i64 * rb, z0 + h as i64 * rb);
    let wraps = wrap_frame(world).is_some();
    let block_of = |o: i64, i: usize| o + i as i64 * sm + sm / 2;
    let inside = |sx: usize, sz: usize| {
        let (bx, bz) = (block_of(ox, sx), block_of(oz, sz));
        (z0..z1).contains(&bz) && (wraps || (x0..x1).contains(&bx))
    };
    let mut fill = |patch: &dyn Patch, shift: i64| {
        for sz in 0..EDGE {
            for sx in 0..EDGE {
                let s = &mut tile.samples[sx + EDGE * sz];
                if !inside(sx, sz) {
                    s.flags = sample_flags::OUTSIDE;
                    continue;
                }
                let c = point_sample(patch, mats, centre_m(ox, sx, sm) - shift as f64, centre_m(oz, sz, sm));
                *s = map_sample(mats, &c);
            }
        }
    };
    if key.level <= SURFACE_PATCH_MAX_LEVEL {
        // One window in the frame of the tile's west edge (a wrapping world's patch runs on east).
        let shift = wrap_shift_blocks(world, ox.clamp(i32::MIN as i64, i32::MAX as i64) as i32) as i64;
        let m = patch_margin(mats) as i64;
        let span = key.span_blocks();
        let patch = world.patch((ox - shift - m) as f64, (oz - m) as f64, (span + 2 * m) as f64);
        fill(&patch, shift);
    } else {
        fill(&PointPatch { world }, 0);
    }
    tile
}

/// [`surface_tile_forged`] for the procedural world of `seed` (no edge: nothing is `OUTSIDE`).
pub fn surface_tile_procedural(seed: u64, key: MapTileKey, mats: &Materials) -> MapTile {
    let mut tile = MapTile::blank(key);
    let (ox, oz) = key.origin_block();
    let sm = key.sample_m();
    let pipeline = TerrainPipeline::new(seed);
    if key.level <= SURFACE_PATCH_MAX_LEVEL {
        let m = patch_margin(mats) as i64;
        let span = key.span_blocks();
        let patch = ProceduralPatch(pipeline.profile_grid((ox - m) as f64, (oz - m) as f64, (span + 2 * m) as f64));
        for sz in 0..EDGE {
            for sx in 0..EDGE {
                let c = point_sample(&patch, mats, centre_m(ox, sx, sm), centre_m(oz, sz, sm));
                tile.samples[sx + EDGE * sz] = map_sample(mats, &c);
            }
        }
    } else {
        let step = PROFILE_LATTICE_BLOCKS as f64;
        for sz in 0..EDGE {
            for sx in 0..EDGE {
                let (x, z) = (centre_m(ox, sx, sm), centre_m(oz, sz, sm));
                // The lattice cell holding the point: the same corners any window would read there.
                let patch = ProceduralPatch(pipeline.profile_grid((x / step).floor() * step, (z / step).floor() * step, step));
                tile.samples[sx + EDGE * sz] = map_sample(mats, &point_sample(&patch, mats, x, z));
            }
        }
    }
    tile
}
