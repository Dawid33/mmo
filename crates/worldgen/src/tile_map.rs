//! World-map tile summaries: one [`TileSummary`] per engine region tile laid
//! over a forged world, computed from the artifact's rasters, its river list
//! and point samples only — never by generating a region (viewing a tile must
//! not build it). The server computes the map once when its world is
//! installed and sends it; the client draws it.
//!
//! Units: engine world metres in x/z (1 block = 1 m, a tile is
//! `REGION_SIZE / UNITS_PER_METER` = `REGION_BLOCKS` m wide); heights in the
//! summaries are metres relative to sea level, while a [`PointSample`] holds
//! world block Y metres (`SEA_LEVEL + height`).

use game::movement::UNITS_PER_METER;
use game::{RegionCoords, TileSummary, WorldMap, BLOCK_VOXELS, REGION_BLOCKS, REGION_SIZE};

use crate::forged::{ForgedWorld, PointSample, OUTSIDE_HEIGHT_M};
use crate::pipeline::{SEA_LEVEL, SEA_TOP_BLOCK};

/// Percentiles whose spread is a tile's `relief_m`.
pub const RELIEF_PERCENTILES: (f64, f64) = (0.1, 0.9);

/// Topmost solid block for a continuous surface height (world metres) —
/// `generate_region`'s rule: a block is solid when its CENTER (`by + 0.5`) is
/// below the surface.
pub fn surface_block(ground_y: f64) -> i32 {
    (ground_y - 0.5).ceil() as i32 - 1
}

/// Whether a column is dry land: its surface block stands at or above the
/// sea's top block and no lake or river surface lies above its ground. The
/// one definition of "dry" shared by the map's landing search and
/// `examples/world_map.rs`.
pub fn is_dry(sample: &PointSample) -> bool {
    surface_block(sample.ground_y) >= SEA_TOP_BLOCK && sample.water_y.is_none_or(|w| w <= sample.ground_y)
}

/// One tile's edge in engine metres.
fn tile_m() -> f64 {
    f64::from(REGION_SIZE / UNITS_PER_METER)
}

/// Engine metres per block column.
fn block_m() -> f64 {
    BLOCK_VOXELS as f64 / f64::from(UNITS_PER_METER)
}

/// The map of every engine region tile intersecting `world`'s artifact, row-major
/// by z then x. See [`summarize_tile`] for what each tile holds.
pub fn build_world_map(world: &ForgedWorld) -> WorldMap {
    let (min, max) = world.artifact_bounds_engine_m();
    let t = tile_m();
    let first = |v: f64| (v / t).floor() as i32;
    // The last tile whose start lies before the (exclusive) max edge.
    let last = |v: f64| (v / t).ceil() as i32 - 1;
    let origin = RegionCoords::new(first(min[0]), first(min[1]));
    let (width, height) = ((last(max[0]) - origin.x + 1) as u32, (last(max[1]) - origin.z + 1) as u32);
    let mut tiles = Vec::with_capacity((width * height) as usize);
    for dz in 0..height as i32 {
        for dx in 0..width as i32 {
            tiles.push(summarize_tile(world, RegionCoords::new(origin.x + dx, origin.z + dz)));
        }
    }
    WorldMap { origin, width, height, tiles, rock_names: world.artifact().rock_names.clone() }
}

/// One raster cell of the artifact lattice: inside the artifact, or a
/// virtual open-ocean cell beyond its edge.
struct Cell {
    ix: i64,
    iz: i64,
    /// Engine metres of the cell centre.
    x: f64,
    z: f64,
    inside: bool,
}

/// The summary of region tile `rc`: statistics over the artifact cells whose
/// centres fall in the tile (cells beyond the artifact count as ocean at
/// [`OUTSIDE_HEIGHT_M`]), rivers by any reach point inside the footprint, the
/// most common surface rock, and the landing column — the tile centre's
/// column or else the in-artifact cell centre nearest it (ties by z, then x)
/// whose block column [`is_dry`].
pub fn summarize_tile(world: &ForgedWorld, rc: RegionCoords) -> TileSummary {
    let a = world.artifact();
    let n = i64::from(a.n);
    let cell_m = a.cell_m;
    let (min, _) = world.artifact_bounds_engine_m();
    let t = tile_m();
    let tile_min = [f64::from(rc.x) * t, f64::from(rc.z) * t];

    // Lattice indices whose centres `min + (i + 0.5) * cell_m` lie in [lo, lo + t).
    let range = |lo: f64, origin: f64| {
        let first = ((lo - origin) / cell_m - 0.5).ceil() as i64;
        let end = ((lo + t - origin) / cell_m - 0.5).ceil() as i64;
        // A tile narrower than a cell still gets the cell holding its centre.
        if end > first { first..end } else { let i = ((lo + 0.5 * t - origin) / cell_m).floor() as i64; i..i + 1 }
    };
    let (xs, zs) = (range(tile_min[0], min[0]), range(tile_min[1], min[1]));
    let inside = |ix: i64, iz: i64| (0..n).contains(&ix) && (0..n).contains(&iz);
    let height = |ix: i64, iz: i64| {
        if inside(ix, iz) { f64::from(a.height[(iz * n + ix) as usize]) } else { OUTSIDE_HEIGHT_M }
    };
    let cells: Vec<Cell> = zs
        .flat_map(|iz| xs.clone().map(move |ix| (ix, iz)))
        .map(|(ix, iz)| Cell {
            ix,
            iz,
            x: min[0] + (ix as f64 + 0.5) * cell_m,
            z: min[1] + (iz as f64 + 0.5) * cell_m,
            inside: inside(ix, iz),
        })
        .collect();
    let count = cells.len() as f64;

    // Ground, sea and slope over every cell.
    let mut grounds: Vec<f64> = cells.iter().map(|c| height(c.ix, c.iz)).collect();
    let sea = grounds.iter().filter(|&&g| surface_block(SEA_LEVEL + g) < SEA_TOP_BLOCK).count();
    let mean = grounds.iter().sum::<f64>() / count;
    let mut slope = [0.0; 2];
    for c in &cells {
        slope[0] += (height(c.ix + 1, c.iz) - height(c.ix - 1, c.iz)) / (2.0 * cell_m);
        slope[1] += (height(c.ix, c.iz + 1) - height(c.ix, c.iz - 1)) / (2.0 * cell_m);
    }
    grounds.sort_by(f64::total_cmp);
    let percentile = |q: f64| grounds[((grounds.len() - 1) as f64 * q).round() as usize];

    // Lakes: cells with a lake surface.
    let lake_levels: Vec<f32> = cells
        .iter()
        .filter(|c| c.inside)
        .map(|c| a.lake_level[(c.iz * n + c.ix) as usize])
        .filter(|l| !l.is_nan())
        .collect();

    // Rivers: reaches with a point inside the footprint (artifact metres).
    let (fx0, fz0) = (tile_min[0] - min[0], tile_min[1] - min[1]);
    let in_footprint = |x: f32, z: f32| {
        let (x, z) = (f64::from(x), f64::from(z));
        x >= fx0 && x < fx0 + t && z >= fz0 && z < fz0 + t
    };
    let reaches: Vec<_> =
        a.rivers.iter().filter(|r| r.points.iter().any(|p| in_footprint(p.x, p.z))).collect();

    // Surface rock: the mode over the in-artifact cell centres (lowest index wins a tie).
    let mut rock_counts = vec![0u32; a.rock_names.len()];
    for c in cells.iter().filter(|c| c.inside) {
        if let Some(rock) = world.surface_rock(c.x, c.z) {
            rock_counts[rock as usize] += 1;
        }
    }
    let surface_rock = rock_counts
        .iter()
        .enumerate()
        .filter(|&(_, &k)| k > 0)
        .max_by(|(i, x), (j, y)| x.cmp(y).then(j.cmp(i)))
        .map(|(i, _)| i as u8);

    TileSummary {
        ground_min_m: grounds[0] as f32,
        ground_mean_m: mean as f32,
        ground_max_m: grounds[grounds.len() - 1] as f32,
        relief_m: (percentile(RELIEF_PERCENTILES.1) - percentile(RELIEF_PERCENTILES.0)) as f32,
        slope: [(slope[0] / count) as f32, (slope[1] / count) as f32],
        sea_fraction: (sea as f64 / count) as f32,
        lake_fraction: (lake_levels.len() as f64 / count) as f32,
        lake_level_m: lake_levels.iter().copied().reduce(f32::max),
        rivers: reaches.len().min(u16::MAX as usize) as u16,
        river_max_discharge_m2: reaches.iter().map(|r| r.discharge_m2).fold(0.0, f64::max) as f32,
        river_max_width_m: reaches.iter().map(|r| r.width_m).fold(0.0, f32::max),
        surface_rock,
        landing: landing(world, &cells, tile_min),
    }
}

/// The landing column: the tile centre's own block column, then the
/// in-artifact cell centres by distance to the tile centre (ties by z, then
/// x); the first whose block column is dry, as a region-local block column.
/// (Cell centres alone can sit up to half a cell off the centre; the centre
/// column first keeps a dry tile's landing on its middle.)
fn landing(world: &ForgedWorld, cells: &[Cell], tile_min: [f64; 2]) -> Option<[u32; 2]> {
    let centre = [tile_min[0] + 0.5 * tile_m(), tile_min[1] + 0.5 * tile_m()];
    let mut order: Vec<&Cell> = cells.iter().filter(|c| c.inside).collect();
    let dist2 = |c: &Cell| (c.x - centre[0]).powi(2) + (c.z - centre[1]).powi(2);
    order.sort_by(|p, q| dist2(p).total_cmp(&dist2(q)).then(p.iz.cmp(&q.iz)).then(p.ix.cmp(&q.ix)));
    let (bounds_min, bounds_max) = world.artifact_bounds_engine_m();
    let in_artifact =
        |x: f64, z: f64| x >= bounds_min[0] && z >= bounds_min[1] && x < bounds_max[0] && z < bounds_max[1];
    let column = |v: f64, lo: f64| (((v - lo) / block_m()).floor().max(0.0) as u32).min(REGION_BLOCKS as u32 - 1);
    let at = |b: u32, lo: f64| lo + (f64::from(b) + 0.5) * block_m();
    std::iter::once(centre)
        .filter(|c| in_artifact(c[0], c[1]))
        .chain(order.into_iter().map(|c| [c.x, c.z]))
        .find_map(|[x, z]| {
            let col = [column(x, tile_min[0]), column(z, tile_min[1])];
            is_dry(&world.sample(at(col[0], tile_min[0]), at(col[1], tile_min[1]))).then_some(col)
        })
}
