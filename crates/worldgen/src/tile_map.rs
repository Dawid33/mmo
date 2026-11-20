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
use game::{Biome, RegionCoords, RegionType, TileSummary, WorldMap, BLOCK_VOXELS, REGION_BLOCKS, REGION_SIZE};

use crate::forged::{ForgedWorld, PointSample};
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
/// one definition of "dry" shared by the map's landing search (which also
/// wants [`LANDING_MIN_BLOCKS_ABOVE_SEA`], see [`is_landing`]) and
/// `examples/world_map.rs`.
pub fn is_dry(sample: &PointSample) -> bool {
    surface_block(sample.ground_y) >= SEA_TOP_BLOCK && sample.water_y.is_none_or(|w| w <= sample.ground_y)
}

/// Blocks a landing column's surface block must stand above the sea's top
/// block, on top of [`is_dry`]: a column level with the sea is dry but sits
/// right on the waterline, too marginal to put a body down on.
pub const LANDING_MIN_BLOCKS_ABOVE_SEA: i32 = 1;

/// Whether a column may be landed on: [`is_dry`] and its surface block at
/// least [`LANDING_MIN_BLOCKS_ABOVE_SEA`] above the sea's top block.
pub fn is_landing(sample: &PointSample) -> bool {
    is_dry(sample) && surface_block(sample.ground_y) >= SEA_TOP_BLOCK + LANDING_MIN_BLOCKS_ABOVE_SEA
}

/// One tile's edge in engine metres.
fn tile_m() -> f64 {
    f64::from(REGION_SIZE / UNITS_PER_METER)
}

/// Engine metres per block column.
fn block_m() -> f64 {
    BLOCK_VOXELS as f64 / f64::from(UNITS_PER_METER)
}

/// Version of what [`build_world_map`] produces. The world store keeps the
/// map it built when the world was written (never rebuilt at start) with this
/// number beside it, and a host refuses — the server re-tiles from the
/// archived forge file — a stored map of another version.
///
/// **Bump rule:** bump it on ANY change to what `build_world_map` returns for
/// the same world — a `WorldMap` / `TileSummary` layout change, but also pure
/// logic (summaries, landings, biome or rock picks, rounding), since a stale
/// stored map is otherwise served silently.
///
/// v2: width x height worlds (the tiles span `width_m` by `height_m`).
/// v3: `WorldMap::wrap_x` (east–west wrap).
pub const WORLD_MAP_VERSION: u32 = 3;

/// Whether `world` wraps east–west.
fn wraps(world: &ForgedWorld) -> bool {
    world.globals().edges[1] == crate::forged::EdgeKind::Wrap
}

/// The map's first tile and its size in tiles: every tile intersecting the
/// artifact — on a world that wraps east–west exactly `width_m / tile`
/// columns from the first (the forge makes a wrapping world a whole number
/// of tiles wide), the canonical regions `[origin.x, origin.x + width)`.
pub(crate) fn map_extent(world: &ForgedWorld) -> (RegionCoords, u32, u32) {
    let (min, max) = world.artifact_bounds_engine_m();
    let t = tile_m();
    let first = |v: f64| (v / t).floor() as i32;
    // The last tile whose start lies before the (exclusive) max edge.
    let last = |v: f64| (v / t).ceil() as i32 - 1;
    let origin = RegionCoords::new(first(min[0]), first(min[1]));
    let width =
        if wraps(world) { (world.globals().width_m / t).round() as u32 } else { (last(max[0]) - origin.x + 1) as u32 };
    (origin, width, (last(max[1]) - origin.z + 1) as u32)
}

/// The map of every engine region tile intersecting `world`'s artifact, row-major
/// by z then x (on a world that wraps east–west, its canonical regions). See
/// [`summarize_tile`] for what each tile holds.
pub fn build_world_map(world: &ForgedWorld) -> WorldMap {
    let a = world.artifact().expect("whole world");
    let (origin, width, height) = map_extent(world);
    let reaches = reaches_by_tile(world);
    let mut tiles = Vec::with_capacity((width * height) as usize);
    for dz in 0..height as i32 {
        for dx in 0..width as i32 {
            let rc = RegionCoords::new(origin.x + dx, origin.z + dz);
            let rivers = reaches.get(&(rc.x, rc.z)).map_or(&[][..], Vec::as_slice);
            tiles.push(summarize_tile_with(world, rc, rivers.iter().map(|&k| &a.rivers[k]).collect()));
        }
    }
    WorldMap {
        origin,
        width,
        height,
        tiles,
        rock_names: a.rock_names.clone(),
        region_types: a.regions.iter().map(|r| r.kind).collect(),
        wrap_x: wraps(world),
    }
}

/// Whether artifact point `(x, z)` lies in tile `rc`'s footprint — the one
/// test for which tile a river point or volcano belongs to. On a world that
/// wraps east–west, x is taken round the world to the footprint's image.
fn in_footprint(world: &ForgedWorld, rc: RegionCoords, x: f32, z: f32) -> bool {
    let (min, _) = world.artifact_bounds_engine_m();
    let t = tile_m();
    let (fx0, fz0) = (f64::from(rc.x) * t - min[0], f64::from(rc.z) * t - min[1]);
    let (x, z) = (f64::from(x), f64::from(z));
    let x = if wraps(world) { fx0 + (x - fx0).rem_euclid(world.globals().width_m) } else { x };
    x >= fx0 && x < fx0 + t && z >= fz0 && z < fz0 + t
}

/// Every tile's river reaches (indices into the artifact's rivers, each
/// once, ascending): one pass over the river points instead of one per tile
/// (a 100 km world has ~38 k tiles and millions of river points).
fn reaches_by_tile(world: &ForgedWorld) -> std::collections::HashMap<(i32, i32), Vec<usize>> {
    let (min, _) = world.artifact_bounds_engine_m();
    let t = tile_m();
    // On a world that wraps east–west a point's tile is folded onto the
    // map's canonical columns (a point past the last column's end lies in
    // the first's image).
    let (origin, width, _) = map_extent(world);
    let fold = |x: i32| if wraps(world) { origin.x + (x - origin.x).rem_euclid(width as i32) } else { x };
    let mut by_tile: std::collections::HashMap<(i32, i32), Vec<usize>> = Default::default();
    for (k, reach) in world.artifact().expect("whole world").rivers.iter().enumerate() {
        for p in &reach.points {
            let guess = (
                ((min[0] + f64::from(p.x)) / t).floor() as i32,
                ((min[1] + f64::from(p.z)) / t).floor() as i32,
            );
            // The guess, or a neighbour when it rounds across a tile edge.
            for (dx, dz) in [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (-1, 1), (1, -1), (1, 1)] {
                let rc = RegionCoords::new(fold(guess.0 + dx), guess.1 + dz);
                if in_footprint(world, rc, p.x, p.z) {
                    let list = by_tile.entry((rc.x, rc.z)).or_default();
                    if list.last() != Some(&k) {
                        list.push(k);
                    }
                    break;
                }
            }
        }
    }
    by_tile
}

/// One raster cell of the artifact lattice: inside the artifact, or a
/// virtual cell beyond its edge (ocean past the Planet's north and south sea
/// edges; east–west the lattice wraps round).
struct Cell {
    ix: i64,
    iz: i64,
    /// Engine metres of the cell centre.
    x: f64,
    z: f64,
    inside: bool,
}

/// The summary of region tile `rc`: statistics over the artifact cells whose
/// centres fall in the tile (cells beyond the artifact count as the sampler
/// extends the map, `ForgedWorld::extended_cell_height`: ocean at
/// [`crate::forged::OUTSIDE_HEIGHT_M`] past a sea edge, x wrapping round east–west), rivers by any reach point inside the footprint, the
/// most common surface rock, and the landing column — the tile centre's
/// column or else the in-artifact cell centre nearest it (ties by z, then x)
/// whose block column [`is_landing`]. Biome and region are the modes of the
/// stored rasters over the in-artifact cells (lowest value wins a tie),
/// climate and savagery their means, `alignment` the mode region's; a tile
/// with no cell centre inside the artifact reads the nearest edge cell.
pub fn summarize_tile(world: &ForgedWorld, rc: RegionCoords) -> TileSummary {
    let rivers = &world.artifact().expect("whole world").rivers;
    let reaches = rivers.iter().filter(|r| r.points.iter().any(|p| in_footprint(world, rc, p.x, p.z))).collect();
    summarize_tile_with(world, rc, reaches)
}

/// [`summarize_tile`] with the tile's river reaches already found.
fn summarize_tile_with(world: &ForgedWorld, rc: RegionCoords, reaches: Vec<&crate::forged::RiverReach>) -> TileSummary {
    let a = world.artifact().expect("whole world");
    let (nx, nz) = (i64::from(a.nx), i64::from(a.nz));
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
    // On a world that wraps east–west a column index is taken round the
    // world (the last tile reads the first columns again).
    let wrap = wraps(world);
    let fold = move |ix: i64| if wrap { ix.rem_euclid(nx) } else { ix };
    let inside = |ix: i64, iz: i64| (0..nx).contains(&fold(ix)) && (0..nz).contains(&iz);
    let height = |ix: i64, iz: i64| {
        if inside(ix, iz) {
            f64::from(a.height.get((iz * nx + fold(ix)) as usize))
        } else {
            world.extended_cell_height(ix, iz)
        }
    };
    let cells: Vec<Cell> = zs
        .flat_map(|iz| xs.clone().map(move |ix| (ix, iz)))
        .map(|(ix, iz)| Cell {
            ix: fold(ix),
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
        .filter_map(|c| world.lake_level_of((c.iz * nx + c.ix) as usize))
        .collect();


    // Surface rock: the mode over the in-artifact cell centres (lowest index wins a tie).
    let mut rock_counts = vec![0u32; a.rock_names.len()];
    for c in cells.iter().filter(|c| c.inside) {
        if let Some(rock) = world.surface_rock(c.x, c.z) {
            if let Some(k) = rock_counts.get_mut(rock as usize) {
                *k += 1;
            }
        }
    }
    let surface_rock = rock_counts
        .iter()
        .enumerate()
        .filter(|&(_, &k)| k > 0)
        .max_by(|(i, x), (j, y)| x.cmp(y).then(j.cmp(i)))
        .map(|(i, _)| i as u8);

    let climate = climate(world, &cells);
    let volcano = a.volcanoes.iter().any(|v| in_footprint(world, rc, v[0], v[1]));

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
        biome: climate.biome,
        region: climate.region,
        temperature_c: climate.temperature_c,
        precip_mm: climate.precip_mm,
        savagery: climate.savagery,
        alignment: climate.alignment,
        volcano,
    }
}

/// A tile's biome, region and climate fields of [`TileSummary`].
struct TileClimate {
    biome: Biome,
    region: u16,
    temperature_c: f32,
    precip_mm: f32,
    savagery: u8,
    alignment: i8,
}

/// The stored per-cell fields over `cells` (see [`summarize_tile`]).
fn climate(world: &ForgedWorld, cells: &[Cell]) -> TileClimate {
    let a = world.artifact().expect("whole world");
    let (nx, nz) = (i64::from(a.nx), i64::from(a.nz));
    let mut index: Vec<usize> =
        cells.iter().filter(|c| c.inside).map(|c| (c.iz * nx + c.ix) as usize).collect();
    if index.is_empty() {
        // No cell centre inside: the nearest edge cell stands in.
        let c = &cells[cells.len() / 2];
        index.push((c.iz.clamp(0, nz - 1) * nx + c.ix.clamp(0, nx - 1)) as usize);
    }
    let mut biomes = [0u32; Biome::ALL.len()];
    for &i in &index {
        biomes[usize::from(a.biome[i]).min(Biome::ALL.len() - 1)] += 1;
    }
    let biome = Biome::ALL[mode_index(biomes.iter().copied().enumerate())];
    let mut regions: Vec<u16> = index.iter().map(|&i| a.region[i]).collect();
    regions.sort_unstable();
    let runs = regions.chunk_by(|p, q| p == q).map(|run| (usize::from(run[0]), run.len() as u32));
    let region = mode_index(runs) as u16;
    let mean = |f: &dyn Fn(usize) -> f64| index.iter().map(|&i| f(i)).sum::<f64>() / index.len() as f64;
    TileClimate {
        biome,
        region,
        temperature_c: mean(&|i| f64::from(a.temperature_c.get(i))) as f32,
        precip_mm: mean(&|i| f64::from(a.precip_mm.get(i))) as f32,
        savagery: mean(&|i| f64::from(a.savagery[i])).round() as u8,
        alignment: a.regions.get(usize::from(region)).map_or(0, |r| r.alignment),
    }
}

/// The value with the highest count in `(value, count)` pairs; the lowest
/// value wins a tie (0 for none).
fn mode_index(counts: impl Iterator<Item = (usize, u32)>) -> usize {
    counts
        .filter(|&(_, k)| k > 0)
        .max_by(|(i, x), (j, y)| x.cmp(y).then(j.cmp(i)))
        .map_or(0, |(i, _)| i)
}

/// The landing column: the tile centre's own block column, then the
/// in-artifact cell centres by distance to the tile centre (ties by z, then
/// x); the first whose block column [`is_landing`], as a region-local block
/// column.
/// (Cell centres alone can sit up to half a cell off the centre; the centre
/// column first keeps a dry tile's landing on its middle.)
fn landing(world: &ForgedWorld, cells: &[Cell], tile_min: [f64; 2]) -> Option<[u32; 2]> {
    let centre = [tile_min[0] + 0.5 * tile_m(), tile_min[1] + 0.5 * tile_m()];
    let mut order: Vec<&Cell> = cells.iter().filter(|c| c.inside).collect();
    let dist2 = |c: &Cell| (c.x - centre[0]).powi(2) + (c.z - centre[1]).powi(2);
    order.sort_by(|p, q| dist2(p).total_cmp(&dist2(q)).then(p.iz.cmp(&q.iz)).then(p.ix.cmp(&q.ix)));
    let (bounds_min, bounds_max) = world.artifact_bounds_engine_m();
    let wrap = wraps(world);
    let in_artifact = |x: f64, z: f64| {
        (wrap || (x >= bounds_min[0] && x < bounds_max[0])) && z >= bounds_min[1] && z < bounds_max[1]
    };
    let column = |v: f64, lo: f64| (((v - lo) / block_m()).floor().max(0.0) as u32).min(REGION_BLOCKS as u32 - 1);
    let at = |b: u32, lo: f64| lo + (f64::from(b) + 0.5) * block_m();
    std::iter::once(centre)
        .filter(|c| in_artifact(c[0], c[1]))
        .chain(order.into_iter().map(|c| [c.x, c.z]))
        .find_map(|[x, z]| {
            let col = [column(x, tile_min[0]), column(z, tile_min[1])];
            is_landing(&world.sample(at(col[0], tile_min[0]), at(col[1], tile_min[1]))).then_some(col)
        })
}

/// Share of a tile under sea at or above which it reads as open ocean.
pub const OCEAN_SEA_FRACTION: f32 = 0.95;
/// Share of a tile under sea above which a (non-ocean) tile is coast.
pub const COAST_SEA_FRACTION: f32 = 0.05;
/// `relief_m` from which a land tile is hills.
pub const HILLS_RELIEF_M: f32 = 60.0;
/// `relief_m` from which a land tile is mountains.
pub const MOUNTAINS_RELIEF_M: f32 = 250.0;
/// `ground_max_m` from which a land tile is mountains whatever its relief.
pub const MOUNTAINS_MIN_M: f32 = 1200.0;

/// What a tile looks like at a glance; provisional, from thresholds on the summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainClass {
    Ocean,
    Coast,
    Lowland,
    Hills,
    Mountains,
}

impl TerrainClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ocean => "Ocean",
            Self::Coast => "Coast",
            Self::Lowland => "Lowland",
            Self::Hills => "Hills",
            Self::Mountains => "Mountains",
        }
    }
}

/// The terrain class of a tile (first matching rule wins, in the enum's
/// ocean-to-mountain order of the constants above).
pub fn terrain_class(t: &TileSummary) -> TerrainClass {
    if t.landing.is_none() || t.sea_fraction >= OCEAN_SEA_FRACTION {
        TerrainClass::Ocean
    } else if t.sea_fraction > COAST_SEA_FRACTION {
        TerrainClass::Coast
    } else if t.relief_m >= MOUNTAINS_RELIEF_M || t.ground_max_m >= MOUNTAINS_MIN_M {
        TerrainClass::Mountains
    } else if t.relief_m >= HILLS_RELIEF_M {
        TerrainClass::Hills
    } else {
        TerrainClass::Lowland
    }
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Piecewise-linear colour ramp over ascending `(value, rgb)` stops.
fn ramp(stops: &[(f64, [f64; 3])], v: f64) -> [f64; 3] {
    let mut prev = stops[0];
    for &stop in stops {
        if v <= stop.0 {
            let span = (stop.0 - prev.0).max(f64::EPSILON);
            return lerp3(prev.1, stop.1, (v - prev.0) / span);
        }
        prev = stop;
    }
    prev.1
}

/// Sea colour (RGB 0..255) at `depth_m` below the sea surface; darker with depth.
pub fn sea_color(depth_m: f64) -> [f64; 3] {
    ramp(
        &[
            (0.0, [110.0, 190.0, 215.0]),
            (8.0, [60.0, 140.0, 200.0]),
            (40.0, [30.0, 80.0, 160.0]),
            (128.0, [12.0, 30.0, 90.0]),
        ],
        depth_m,
    )
}

/// Fresh-water colour at `depth_m` below the lake or river surface.
pub fn lake_color(depth_m: f64) -> [f64; 3] {
    ramp(&[(0.0, [120.0, 185.0, 225.0]), (3.0, [60.0, 130.0, 205.0])], depth_m)
}

/// Land colour (RGB 0..255) at `height_m` above sea level. Follows
/// `Materials::material_at`: sand through one block above the sea top, then
/// the elevation ramp `height_color`.
pub fn land_color(height_m: f64) -> [f64; 3] {
    if surface_block(SEA_LEVEL + height_m) <= SEA_TOP_BLOCK + 1 {
        return [222.0, 205.0, 150.0]; // sand
    }
    height_color(height_m)
}

/// Height (m above sea) from which ground is drawn snow-capped on maps.
pub const SNOW_LINE_M: f64 = 2_200.0;
/// Height over which the colour blends from bare rock to snow above `SNOW_LINE_M`.
pub const SNOW_BLEND_M: f64 = 150.0;
/// Top of the elevation ramp (m above sea): the forge's highest `max_peak_m`.
pub const HEIGHT_RAMP_TOP_M: f64 = 3_500.0;

/// The one elevation ramp of every height map — the minimap, the world map and
/// the forge previews (RGB 0..255 at `height_m` above sea level, no sand: the
/// callers draw their shore their own way). Lowland greens through the first
/// few hundred metres (where most land and all the detail is), olive and brown
/// foothills, grey rock towards `SNOW_LINE_M`, then a snow cap up to
/// `HEIGHT_RAMP_TOP_M`.
pub fn height_color(height_m: f64) -> [f64; 3] {
    ramp(
        &[
            (0.0, [96.0, 168.0, 84.0]),
            (60.0, [70.0, 140.0, 66.0]),
            (250.0, [104.0, 136.0, 72.0]),
            (600.0, [128.0, 126.0, 84.0]),
            (1_100.0, [140.0, 116.0, 98.0]),
            (1_700.0, [138.0, 130.0, 124.0]),
            (SNOW_LINE_M, [168.0, 164.0, 160.0]),
            (SNOW_LINE_M + SNOW_BLEND_M, [236.0, 238.0, 244.0]),
            (HEIGHT_RAMP_TOP_M, [250.0, 250.0, 255.0]),
        ],
        height_m,
    )
}

/// Unit vector towards the sun for hillshading: north-west (top-left of a
/// map), 45 degrees up.
pub fn sun_dir() -> [f64; 3] {
    let v = [-1.0f64, 1.4142, -1.0];
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

/// Brightness factor for ground sloping `(dhdx, dhdz)` (m per m); flat ground is 1.
fn hillshade(slope: [f64; 2]) -> f64 {
    let sun = sun_dir();
    let nrm = [-slope[0], 1.0, -slope[1]];
    let len = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
    let lambert = (nrm[0] * sun[0] + nrm[1] * sun[1] + nrm[2] * sun[2]) / len;
    (0.35 + 0.65 * lambert.max(0.0) / sun[1]).min(1.35)
}

/// Lake blue a tile blends toward by its `lake_fraction`.
pub const LAKE_RGB: [f64; 3] = [60.0, 130.0, 205.0];
/// River mark colour on the map.
pub const RIVER_RGB: [u8; 3] = [70, 140, 230];
/// Multiplier for the grid-line pixels of a tile.
pub const GRID_DARKEN: f64 = 0.75;
/// Map pixels per tile edge.
pub const MAP_TILE_PX: u32 = 8;
/// Grid-line width in map pixels: the last rows/columns of each tile. The map
/// window frames the cell inside them (Slint `WorldMapConfig.grid-px`).
pub const MAP_GRID_PX: u32 = 1;

/// The colour of a tile on the map: sea by depth for ocean, else land by mean
/// height, blended toward lake blue by lake share, shaded by slope (sea is flat).
pub fn tile_color(t: &TileSummary) -> [u8; 3] {
    let mean = f64::from(t.ground_mean_m);
    let (base, shade) = if terrain_class(t) == TerrainClass::Ocean {
        (sea_color(-mean), 1.0)
    } else {
        let base = lerp3(land_color(mean), LAKE_RGB, f64::from(t.lake_fraction));
        (base, hillshade([f64::from(t.slope[0]), f64::from(t.slope[1])]))
    };
    let px = |v: f64| (v * shade).clamp(0.0, 255.0) as u8;
    [px(base[0]), px(base[1]), px(base[2])]
}

/// What the world map shows: one colour per tile, by a different field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapLayer {
    /// The tile's dominant biome, in the shared palette ([`Biome::map_rgb`]).
    Biome,
    /// Sea by depth, land by mean height, hillshaded ([`tile_color`]).
    Relief,
    /// Mean annual temperature ([`TEMPERATURE_RAMP`]).
    Temperature,
    /// Annual precipitation ([`PRECIP_RAMP`]).
    Precipitation,
    /// One colour per forge region ([`region_color`]).
    Regions,
}

impl MapLayer {
    /// Every layer in the map window's button order (the first is the default).
    pub const ALL: [MapLayer; 5] =
        [MapLayer::Biome, MapLayer::Relief, MapLayer::Temperature, MapLayer::Precipitation, MapLayer::Regions];

    /// The layer button's caption.
    pub fn label(self) -> &'static str {
        match self {
            MapLayer::Biome => "Biomes",
            MapLayer::Relief => "Relief",
            MapLayer::Temperature => "Temperature",
            MapLayer::Precipitation => "Rain",
            MapLayer::Regions => "Regions",
        }
    }
}

/// Temperature colour stops (deg C, RGB): purple-blue frost through pale
/// green temperate to red heat.
pub const TEMPERATURE_RAMP: [(f64, [f64; 3]); 6] = [
    (-30.0, [90.0, 60.0, 170.0]),
    (-12.0, [70.0, 130.0, 220.0]),
    (0.0, [150.0, 210.0, 235.0]),
    (12.0, [170.0, 215.0, 120.0]),
    (22.0, [240.0, 200.0, 80.0]),
    (32.0, [215.0, 60.0, 40.0]),
];
/// Precipitation colour stops (mm/yr, RGB): desert tan through green to deep blue.
pub const PRECIP_RAMP: [(f64, [f64; 3]); 5] = [
    (0.0, [225.0, 200.0, 140.0]),
    (300.0, [200.0, 205.0, 110.0]),
    (800.0, [90.0, 170.0, 80.0]),
    (1_600.0, [40.0, 140.0, 170.0]),
    (3_000.0, [30.0, 50.0, 150.0]),
];
/// Drainage colour stops (0..100, RGB): waterlogged blue-green through green
/// to dry tan. The DF thresholds sit at 33 and 66.
pub const DRAINAGE_RAMP: [(f64, [f64; 3]); 4] = [
    (0.0, [40.0, 90.0, 140.0]),
    (33.0, [70.0, 150.0, 130.0]),
    (66.0, [110.0, 170.0, 80.0]),
    (100.0, [225.0, 200.0, 140.0]),
];

/// Mean annual temperature (deg C) on [`TEMPERATURE_RAMP`] — the one place
/// the ramp is applied (world map tiles, forge previews, `world-map --layer`).
pub fn temperature_color(c: f64) -> [f64; 3] {
    ramp(&TEMPERATURE_RAMP, c)
}
/// Annual precipitation (mm/yr) on [`PRECIP_RAMP`].
pub fn precipitation_color(mm: f64) -> [f64; 3] {
    ramp(&PRECIP_RAMP, mm)
}
/// Drainage (0..100) on [`DRAINAGE_RAMP`].
pub fn drainage_color(d: f64) -> [f64; 3] {
    ramp(&DRAINAGE_RAMP, d)
}
/// Brightness of open-ocean tiles on the temperature and rain layers, so the
/// coast still reads.
pub const OCEAN_DIM: f64 = 0.6;
/// Brightness range of region colours over land; water regions are drawn
/// blue at [`WATER_REGION_VALUE`].
pub const REGION_SATURATION: f64 = 0.55;
pub const REGION_VALUE: f64 = 0.85;
pub const WATER_REGION_VALUE: f64 = 0.45;

/// HSV (hue in turns) to RGB 0..255.
fn hsv(h: f64, s: f64, v: f64) -> [f64; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let c = v * s;
    let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [(r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0]
}

/// A forge region's colour on the regions layer: a hue hashed from its id
/// (neighbours rarely match), blue for sea and lakes.
pub fn region_color(id: u16, kind: Option<RegionType>) -> [u8; 3] {
    // splitmix64 finaliser: ids next to each other get unrelated hues.
    let mut z = u64::from(id).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let hue = (z >> 11) as f64 / (1u64 << 53) as f64;
    let rgb = if kind.is_some_and(RegionType::is_water) {
        hsv(0.58 + 0.08 * (hue - 0.5), 0.6, WATER_REGION_VALUE)
    } else {
        hsv(hue, REGION_SATURATION, REGION_VALUE)
    };
    rgb.map(|v| v.round().clamp(0.0, 255.0) as u8)
}

/// The colour of tile `t` on `layer`.
pub fn layer_color(map: &WorldMap, t: &TileSummary, layer: MapLayer) -> [u8; 3] {
    let ocean = terrain_class(t) == TerrainClass::Ocean;
    let dim = |rgb: [f64; 3]| {
        let k = if ocean { OCEAN_DIM } else { 1.0 };
        rgb.map(|v| (v * k).round().clamp(0.0, 255.0) as u8)
    };
    match layer {
        MapLayer::Biome => t.biome.map_rgb(),
        MapLayer::Relief => tile_color(t),
        MapLayer::Temperature => dim(temperature_color(f64::from(t.temperature_c))),
        MapLayer::Precipitation => dim(precipitation_color(f64::from(t.precip_mm))),
        MapLayer::Regions => region_color(t.region, map.region_types.get(usize::from(t.region)).copied()),
    }
}

/// `layer` as an RGBA picture, ONE pixel per tile: `(width, height,
/// row-major pixels)`, opaque.
pub fn rasterize_layer(map: &WorldMap, layer: MapLayer) -> (u32, u32, Vec<[u8; 4]>) {
    let px = map
        .tiles
        .iter()
        .map(|t| {
            let [r, g, b] = layer_color(map, t, layer);
            [r, g, b, 255]
        })
        .collect();
    (map.width, map.height, px)
}

/// The map window's picture of `layer`: [`rasterize_layer`] with each tile
/// [`MAP_TILE_PX`] square, the last [`MAP_GRID_PX`] rows/columns
/// grid-darkened, and on the relief layer a 2x2 river mark at the centre of
/// tiles with rivers. `(width_px, height_px, row-major pixels)`, opaque.
pub fn rasterize(map: &WorldMap, layer: MapLayer) -> (u32, u32, Vec<[u8; 4]>) {
    let (_, _, colours) = rasterize_layer(map, layer);
    let (w, h) = (map.width * MAP_TILE_PX, map.height * MAP_TILE_PX);
    let mut out = vec![[0, 0, 0, 255]; (w * h) as usize];
    let edge = MAP_TILE_PX - MAP_GRID_PX;
    let mid = MAP_TILE_PX / 2;
    let rivers = layer == MapLayer::Relief;
    for (i, t) in map.tiles.iter().enumerate() {
        let (tx, tz) = (i as u32 % map.width * MAP_TILE_PX, i as u32 / map.width * MAP_TILE_PX);
        let [r, g, b, _] = colours[i];
        for dz in 0..MAP_TILE_PX {
            for dx in 0..MAP_TILE_PX {
                let mut rgb = [r, g, b];
                if dx >= edge || dz >= edge {
                    rgb = rgb.map(|v| (f64::from(v) * GRID_DARKEN) as u8);
                }
                if rivers && t.rivers > 0 && (mid - 1..=mid).contains(&dx) && (mid - 1..=mid).contains(&dz) {
                    rgb = RIVER_RGB;
                }
                out[((tz + dz) * w + tx + dx) as usize] = [rgb[0], rgb[1], rgb[2], 255];
            }
        }
    }
    (w, h, out)
}

/// The side-panel facts of tile `rc` (empty off the map), absent facts omitted.
pub fn tile_info_lines(map: &WorldMap, rc: RegionCoords) -> Vec<String> {
    let Some(t) = map.tile(rc) else { return Vec::new() };
    let mut lines = vec![format!("Region ({}, {})", rc.x, rc.z), format!("Biome: {}", t.biome.name())];
    if let Some(kind) = map.region_types.get(usize::from(t.region)) {
        let alignment = match t.alignment {
            a if a > 0 => " (good)",
            a if a < 0 => " (evil)",
            _ => "",
        };
        lines.push(format!("Biome region: {} #{}{alignment}", kind.name(), t.region));
    }
    lines.push(format!("Terrain: {} (provisional)", terrain_class(t).label()));
    lines.push(format!(
        "Elevation: {:.0}\u{2013}{:.0} m (mean {:.0} m)",
        t.ground_min_m, t.ground_max_m, t.ground_mean_m
    ));
    lines.push(format!("Climate: {:.0} \u{b0}C, {:.0} mm rain a year", t.temperature_c, t.precip_mm));
    lines.push(format!("Savagery: {}", t.savagery));
    if t.volcano {
        lines.push("Volcano".to_string());
    }
    if t.rivers > 0 {
        lines.push(format!("Rivers: {} (largest {:.0} m wide)", t.rivers, t.river_max_width_m));
    }
    if let Some(level) = t.lake_level_m {
        lines.push(format!("Lake: level {level:.0} m"));
    }
    if let Some(name) = t.surface_rock.and_then(|r| map.rock_names.get(usize::from(r))) {
        lines.push(format!("Rock: {name}"));
    }
    lines
}

/// Why Play is unavailable for tile `rc` (the rule the manager enforces too).
pub fn play_blocker(map: &WorldMap, rc: RegionCoords, home: Option<RegionCoords>) -> Option<&'static str> {
    if home == Some(rc) {
        Some("You are here")
    } else if map.tile(rc).is_none_or(|t| t.landing.is_none()) {
        Some("No dry land")
    } else {
        None
    }
}
