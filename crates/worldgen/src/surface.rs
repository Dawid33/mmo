//! A world map tile's surface from worldgen alone (plan 2026-11-20-block-level-world-map, Task 3): per
//! sample, the top block's top, the top block, the water over it and whether that water is ice — the
//! terrain where no stored chunk says otherwise (`map-tiles` overlays those). Each sample is read at its
//! own centre: a level is sampled directly, never averaged from the level below.
//!
//! Levels up to [`SURFACE_PATCH_MAX_LEVEL`] read one patch over the tile, the profile lattice region
//! generation reads, so level 0 is generation's own top bit for bit; there the trees' real crowns are
//! stamped from generation's own placement ([`CANOPY_STAMP_MAX_LEVEL`]), coarser levels show leaves
//! where a tree lattice cell holds a tree. Above it a tile's patch would hold millions of lattice corners, so each sample is read
//! exactly at its point ([`crate::forged::PointPatch`]; procedural: a one-cell lattice around it).

use game::map_tile::{MAP_TILE_SAMPLES, MapSample, MapTile, MapTileKey, sample_flags};
use game::{BlockId, REGION_BLOCKS};

use crate::forged::{ForgedWorld, PointPatch};
use crate::material::Materials;
use crate::pipeline::{PROFILE_LATTICE_BLOCKS, TerrainPipeline};
use crate::{
    ColSample, MemoHabitat, Patch, ProceduralPatch, TREE_REACH_BLOCKS, TreeSource, column_material,
    column_sample, crown_extent, patch_margin, point_sample, slopes, tree_ground, trees,
    wrap_frame, wrap_shift_blocks,
};

/// The regions `world` covers (canonical; x wraps when the world does), as the world map's extent.
pub fn map_extent(world: &ForgedWorld) -> game::MapExtent {
    let (origin, width, height) = crate::tile_map::map_extent(world);
    game::MapExtent {
        origin,
        width,
        height,
        wrap_x: world.globals().edges[1] == crate::forged::EdgeKind::Wrap,
    }
}

/// The highest level read through one lattice patch per tile (1024 m at 4 m: 257² corners).
pub const SURFACE_PATCH_MAX_LEVEL: u8 = 2;
/// The highest level whose tiles carry the trees' real crowns (placed as generation places them; a
/// level-1 tile holds four times the trees and cost ~150 ms); a coarser sample shows leaves where the
/// tree lattice cell under it holds a tree ([`cell_canopy`]).
pub const CANOPY_STAMP_MAX_LEVEL: u8 = 0;

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
    MapSample {
        height: (c.top + 1).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        block,
        water,
        flags,
    }
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
    // Each sample's column, kept for the canopy pass (whether a tree may stand there, and on what).
    let mut cols: Vec<Option<(i32, bool)>> = vec![None; EDGE * EDGE];
    let mut fill = |patch: &dyn Patch, shift: i64| {
        for sz in 0..EDGE {
            for sx in 0..EDGE {
                let s = &mut tile.samples[sx + EDGE * sz];
                if !inside(sx, sz) {
                    s.flags = sample_flags::OUTSIDE;
                    continue;
                }
                let c = point_sample(
                    patch,
                    mats,
                    centre_m(ox, sx, sm) - shift as f64,
                    centre_m(oz, sz, sm),
                );
                *s = map_sample(mats, &c);
                cols[sx + EDGE * sz] = Some((c.s, c.trees && c.s >= c.water_top && c.top == c.s));
            }
        }
    };
    let trees_from = TreeSource::forged(world);
    if key.level <= SURFACE_PATCH_MAX_LEVEL {
        // One window in the frame of the tile's west edge (a wrapping world's patch runs on east).
        let shift =
            wrap_shift_blocks(world, ox.clamp(i32::MIN as i64, i32::MAX as i64) as i32) as i64;
        let m = patch_margin(mats) as i64;
        let span = key.span_blocks();
        let patch = world.patch(
            (ox - shift - m) as f64,
            (oz - m) as f64,
            (span + 2 * m) as f64,
        );
        fill(&patch, shift);
        if mats.trees.enabled() {
            if key.level <= CANOPY_STAMP_MAX_LEVEL {
                stamp_crowns(&mut tile, &patch, mats, trees_from, shift);
            } else {
                cell_canopy(&mut tile, &cols, mats, trees_from, shift);
            }
        }
    } else {
        let patch = PointPatch { world };
        fill(&patch, 0);
        if mats.trees.enabled() {
            cell_canopy(&mut tile, &cols, mats, trees_from, 0);
        }
    }
    tile
}

/// The trees' crowns over `tile` (levels up to [`CANOPY_STAMP_MAX_LEVEL`]), placed exactly as region
/// generation places them (`trees::trees_in` over the patch's surfaces, every tree whose crown reaches
/// into the tile): each sample under a crown disc (or a trunk) shows the species' leaves at the crown's
/// top, where that is above what the sample shows. `patch` is in the frame `shift` blocks west of the
/// tile's (a wrapping world's).
fn stamp_crowns(
    tile: &mut MapTile,
    patch: &dyn Patch,
    mats: &Materials,
    trees_from: TreeSource,
    shift: i64,
) {
    let key = tile.key;
    let (ox, oz) = key.origin_block();
    let sm = key.sample_m();
    let (x0, z0) = ((ox - shift) as i32, oz as i32);
    let span = key.span_blocks() as i32;
    let memo = std::cell::RefCell::new(std::collections::HashMap::new());
    let sample = |x: i32, z: i32| {
        let cached = memo.borrow().get(&(x, z)).copied();
        cached.unwrap_or_else(|| {
            let c = column_sample(patch, mats, x, z);
            memo.borrow_mut().insert((x, z), c);
            c
        })
    };
    let ground_memo = std::cell::RefCell::new(std::collections::HashMap::new());
    let surface = |x: i32, z: i32| {
        let cached = ground_memo.borrow().get(&(x, z)).copied();
        cached.unwrap_or_else(|| {
            let c = sample(x, z);
            let slope = slopes::slope_for(&c, |dx, dz| sample(x + dx, z + dz)).is_some();
            let g = tree_ground(c, slope);
            ground_memo.borrow_mut().insert((x, z), g);
            g
        })
    };
    let habitat = MemoHabitat {
        inner: trees_from.habitat,
        memo: Default::default(),
    };
    let r = TREE_REACH_BLOCKS;
    let found = trees::trees_in(
        x0 - r,
        z0 - r,
        x0 + span + r,
        z0 + span + r,
        trees_from.seed,
        trees_from.wrap,
        &habitat,
        &surface,
    );
    for tree in found {
        let Some((_, _, radius)) = crown_extent(&tree) else {
            continue;
        };
        let footprint = crown_footprint(&tree, radius);
        let leaves = mats.trees.species[tree.species as usize].leaves;
        let [bx, by, bz] = tree.base;
        // Samples whose centre block lies within the crown's reach.
        let lo_i = |a: i32, o: i32| {
            ((a - radius - o) as i64 - sm / 2 + sm - 1)
                .div_euclid(sm)
                .max(0)
        };
        let hi_i = |a: i32, o: i32| {
            ((a + radius - o) as i64 - sm / 2)
                .div_euclid(sm)
                .min(MAP_TILE_SAMPLES as i64 - 1)
        };
        for sz in lo_i(bz, z0)..=hi_i(bz, z0) {
            for sx in lo_i(bx, x0)..=hi_i(bx, x0) {
                let (cx, cz) = (x0 as i64 + sx * sm + sm / 2, z0 as i64 + sz * sm + sm / 2);
                let (dx, dz) = ((cx - bx as i64) as i32, (cz - bz as i64) as i32);
                let Some(&top_dy) = footprint.get(&(dx, dz)) else {
                    continue;
                };
                let top = (by + top_dy).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                let s = &mut tile.samples[(sx + EDGE as i64 * sz) as usize];
                if s.flags & sample_flags::OUTSIDE == 0 && top > s.height {
                    s.height = top;
                    s.block = leaves;
                    s.water = 0;
                    s.flags &= !sample_flags::ICE;
                }
            }
        }
    }
}

/// A tree's leaves seen from above: per block column offset from its trunk `(dx, dz)` within `radius`,
/// the top of its highest leaf block there (blocks above the base), from the skeleton's leaf balls
/// sampled at the column's centre — the crown's real outline and height, not its bounding disc.
fn crown_footprint(
    tree: &trees::TreeSeed,
    radius: i32,
) -> std::collections::HashMap<(i32, i32), i32> {
    let species = &trees::SPECIES[tree.species as usize];
    let sk = trees::skeleton(species, tree.seed, tree.age);
    let bv = game::BLOCK_VOXELS as i32;
    let mut out = std::collections::HashMap::new();
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            // The column's centre, in the skeleton's voxels (the base block's min corner at 0).
            let (vx, vz) = ((dx * bv + bv / 2) as f64, (dz * bv + bv / 2) as f64);
            let mut top: Option<f64> = None;
            for &(c, r) in &sk.leaves {
                let (ex, ez) = (vx - c[0] as f64, vz - c[2] as f64);
                let d2 = ex * ex + ez * ez;
                let r = r as f64;
                if d2 <= r * r {
                    let y = c[1] as f64 + (r * r - d2).sqrt();
                    top = Some(top.map_or(y, |t: f64| t.max(y)));
                }
            }
            if let Some(y) = top {
                out.insert((dx, dz), (y.max(0.0) as i32).div_euclid(bv) + 1);
            }
        }
    }
    out
}

/// The coarse canopy's habitat grid (m): one habitat read per cell this wide (species and density
/// change slowly; a map at 2 m per sample or coarser shows no finer).
const CANOPY_HABITAT_CELL_M: f64 = 32.0;

/// A habitat read once per [`CANOPY_HABITAT_CELL_M`] cell (at the first column asked there).
struct GridHabitat<'a> {
    inner: &'a dyn crate::habitat::HabitatSource,
    memo: std::cell::RefCell<std::collections::HashMap<(i64, i64), crate::habitat::Habitat>>,
}

impl crate::habitat::HabitatSource for GridHabitat<'_> {
    fn habitat(&self, x_m: f64, z_m: f64, surface_block: i32) -> crate::habitat::Habitat {
        let cell = (
            (x_m / CANOPY_HABITAT_CELL_M).floor() as i64,
            (z_m / CANOPY_HABITAT_CELL_M).floor() as i64,
        );
        if let Some(h) = self.memo.borrow().get(&cell) {
            return *h;
        }
        let h = self.inner.habitat(x_m, z_m, surface_block);
        self.memo.borrow_mut().insert(cell, h);
        h
    }
}

/// Coarse levels: a dry, tree-bearing sample shows the leaves of the tree the lattice cell under its
/// centre would hold (`trees::placement::cell_tree`; habitat per 32 m cell, each lattice cell asked
/// once, no crowns): forest reads as forest in about the share of its cells that hold trees.
fn cell_canopy(
    tile: &mut MapTile,
    cols: &[Option<(i32, bool)>],
    mats: &Materials,
    trees_from: TreeSource,
    shift: i64,
) {
    let key = tile.key;
    let (ox, oz) = key.origin_block();
    let sm = key.sample_m();
    let l = trees::TREE_LATTICE_BLOCKS as i64;
    let habitat = GridHabitat {
        inner: trees_from.habitat,
        memo: Default::default(),
    };
    let mut cells: std::collections::HashMap<(i32, i32), Option<u8>> =
        std::collections::HashMap::new();
    for sz in 0..EDGE {
        for sx in 0..EDGE {
            let s = tile.samples[sx + EDGE * sz];
            if s.flags & sample_flags::OUTSIDE != 0 || s.water > 0 {
                continue;
            }
            let Some((ground, may_stand)) = cols[sx + EDGE * sz] else {
                continue;
            };
            if !may_stand {
                continue;
            }
            let (x, z) = (centre_m(ox, sx, sm) - shift as f64, centre_m(oz, sz, sm));
            let (bx, bz) = (x.floor() as i64, z.floor() as i64);
            let (lx, lz) = (bx.div_euclid(l) as i32, bz.div_euclid(l) as i32);
            let species = *cells.entry((lx, lz)).or_insert_with(|| {
                trees::placement::cell_tree(
                    lx,
                    lz,
                    trees_from.seed,
                    trees_from.wrap,
                    &habitat,
                    ground,
                )
            });
            if let Some(species) = species {
                tile.samples[sx + EDGE * sz].block = mats.trees.species[species as usize].leaves;
            }
        }
    }
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
        let patch = ProceduralPatch(pipeline.profile_grid(
            (ox - m) as f64,
            (oz - m) as f64,
            (span + 2 * m) as f64,
        ));
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
                let patch = ProceduralPatch(pipeline.profile_grid(
                    (x / step).floor() * step,
                    (z / step).floor() * step,
                    step,
                ));
                tile.samples[sx + EDGE * sz] = map_sample(mats, &point_sample(&patch, mats, x, z));
            }
        }
    }
    tile
}
