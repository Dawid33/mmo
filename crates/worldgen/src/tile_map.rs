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
/// whose block column [`is_landing`].
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
        if inside(ix, iz) { f64::from(a.height.get((iz * n + ix) as usize)) } else { OUTSIDE_HEIGHT_M }
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
        .filter_map(|c| world.lake_level_of((c.iz * n + c.ix) as usize))
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
    let in_artifact =
        |x: f64, z: f64| x >= bounds_min[0] && z >= bounds_min[1] && x < bounds_max[0] && z < bounds_max[1];
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

/// The map as an RGBA picture: `(width_px, height_px, row-major pixels)`,
/// [`MAP_TILE_PX`] per tile, the last [`MAP_GRID_PX`] rows/columns grid-darkened, a 2x2 river mark
/// at the centre of tiles with rivers. Opaque.
pub fn rasterize(map: &WorldMap) -> (u32, u32, Vec<[u8; 4]>) {
    let (w, h) = (map.width * MAP_TILE_PX, map.height * MAP_TILE_PX);
    let mut out = vec![[0, 0, 0, 255]; (w * h) as usize];
    let edge = MAP_TILE_PX - MAP_GRID_PX;
    let mid = MAP_TILE_PX / 2;
    for (i, t) in map.tiles.iter().enumerate() {
        let (tx, tz) = (i as u32 % map.width * MAP_TILE_PX, i as u32 / map.width * MAP_TILE_PX);
        let c = tile_color(t);
        for dz in 0..MAP_TILE_PX {
            for dx in 0..MAP_TILE_PX {
                let mut rgb = c;
                if dx >= edge || dz >= edge {
                    rgb = rgb.map(|v| (f64::from(v) * GRID_DARKEN) as u8);
                }
                if t.rivers > 0 && (mid - 1..=mid).contains(&dx) && (mid - 1..=mid).contains(&dz) {
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
    let mut lines = vec![
        format!("Region ({}, {})", rc.x, rc.z),
        format!("Terrain: {} (provisional)", terrain_class(t).label()),
        format!(
            "Elevation: {:.0}\u{2013}{:.0} m (mean {:.0} m)",
            t.ground_min_m, t.ground_max_m, t.ground_mean_m
        ),
    ];
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
