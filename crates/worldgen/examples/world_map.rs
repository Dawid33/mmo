//! Top-down map of the generated world, written as PNGs, plus terrain stats.
//! The way to SEE what the density pipeline produces without running the game:
//! continent layout, coastlines, how much relief there is at a given scale.
//!
//! Run: just world-map            (defaults: 8192 m span around spawn, 1024 px)
//!      just world-map --span 65536 --size 2048 --center 0,0 --out target/world-map/wide
//!      just world-map --world target/world-forge/f1-best.world --span 600
//!          (a FORGED world, sampled exactly as generate_region samples it:
//!           carved river channels, lake and river water above sea level)
//!
//! Writes `<out>-color.png` (hypsometric tint + hillshade, water by depth) and
//! `<out>-height.png` (16-bit grayscale, `HEIGHT_MIN_M..=HEIGHT_MAX_M` mapped
//! linearly onto 0..=65535). +x is right, +z is down; one pixel is
//! `span / size` meters, point-sampled at its center.
//!
//! `--world <file> --layer biome|temperature|precipitation|drainage|regions` also writes
//! `<out>-<layer>.png` in the world map's colours (the forged world's own rasters).
//!
//! `--layer rain|temperature|moisture|salinity|trees` also writes `<out>-<layer>.png`
//! (greyscale ramp over the layer's fixed range, `worldgen::habitat`).
//!
//! `--world <file> --tiles` instead writes the in-game world map: every
//! `tile_map::MapLayer` as `<out>-tiles-<layer>.png` (the map window's
//! picture, `MAP_TILE_PX` per tile) and prints the size of the `WorldMap`
//! packet the server sends on connect.
//!
//! Heights come straight from `TerrainPipeline::column` (the exact field, not
//! the 4 m `ProfileGrid` lattice `generate_region` interpolates — the two agree
//! to well under a block), so a map costs one profile per pixel and never
//! materializes a region.

use std::path::PathBuf;

use image::{ImageBuffer, Luma, Rgb};
use worldgen::forged::PointSample;
use worldgen::habitat::{Habitat, HabitatSource, ProceduralHabitat};
use worldgen::pipeline::{ColumnProfile, SEA_LEVEL, SEA_TOP_BLOCK, TerrainPipeline};
// Topmost solid block / "dry" — the same rules the world map's landing search uses.
use worldgen::tile_map::{is_dry, lake_color, land_color, sea_color, sun_dir, surface_block};

// The height band of the tools (world metres): a forged world's deepest sea
// floor to its tallest peak (`max_peak_m` tops out at 3,500 m above the sea)
// plus headroom. The procedural density ramp (pipeline.rs RAMP_FROM_Y /
// RAMP_TO_Y) sits well inside it. Shared with forge_proto/render.rs and
// seed_scan.rs.
const HEIGHT_MIN_M: f64 = -1_000.0;
const HEIGHT_MAX_M: f64 = 3_600.0;
/// Bisection steps for the surface: 4,600 m / 2^20 is ~4 mm.
const SURFACE_BISECTIONS: usize = 20;

const DEFAULT_SPAN_M: f64 = 8192.0;
const DEFAULT_SIZE_PX: usize = 1024;
// Spawn column: world block (8, 8), the center of region (0, 0).
const DEFAULT_CENTER_M: (f64, f64) = (8.5, 8.5);
const DEFAULT_OUT: &str = "target/world-map/map";
/// Vertical exaggeration of the hillshade (1.0 = true slopes).
const DEFAULT_RELIEF: f64 = 2.0;
/// Share of the logical cores the sampling threads use (the repo's 80% rule).
const CORE_SHARE: f64 = 0.8;

/// Continuous surface height in meters: the zero crossing of the column's
/// density. Density falls monotonically with y (the factor spline is strictly
/// positive), so bisection finds it (`SURFACE_BISECTIONS` halvings of the band).
fn surface_m(prof: &ColumnProfile) -> f64 {
    if prof.density_at(HEIGHT_MIN_M) <= 0.0 {
        return HEIGHT_MIN_M;
    }
    if prof.density_at(HEIGHT_MAX_M) > 0.0 {
        return HEIGHT_MAX_M;
    }
    let (mut lo, mut hi) = (HEIGHT_MIN_M, HEIGHT_MAX_M);
    for _ in 0..SURFACE_BISECTIONS {
        let mid = 0.5 * (lo + hi);
        if prof.density_at(mid) > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

struct Args {
    span_m: f64,
    size: usize,
    center: (f64, f64),
    out: PathBuf,
    seed: u64,
    relief: f64,
    /// Render this forged world file instead of the procedural pipeline.
    world: Option<PathBuf>,
    /// Also write one habitat field as a greyscale PNG.
    layer: Option<Layer>,
    /// A forged world's coloured layer (`biome`, `temperature`, `precipitation`, `drainage`, `regions`; needs `world`).
    forged_layer: Option<ForgedLayer>,
    /// Write the world map's tile layers instead (needs `world`).
    tiles: bool,
}

/// A layer read straight from the forged world's rasters, in the colours of
/// the in-game world map (`worldgen::tile_map` ramps and palette).
#[derive(Clone, Copy)]
enum ForgedLayer {
    Biome,
    Temperature,
    Precipitation,
    Drainage,
    Regions,
}

impl ForgedLayer {
    fn parse(name: &str) -> Option<ForgedLayer> {
        Some(match name {
            "biome" => ForgedLayer::Biome,
            "temperature" => ForgedLayer::Temperature,
            "precipitation" => ForgedLayer::Precipitation,
            "drainage" => ForgedLayer::Drainage,
            "regions" => ForgedLayer::Regions,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            ForgedLayer::Biome => "biome",
            ForgedLayer::Temperature => "temperature",
            ForgedLayer::Precipitation => "precipitation",
            ForgedLayer::Drainage => "drainage",
            ForgedLayer::Regions => "regions",
        }
    }
}

#[derive(Clone, Copy)]
enum Layer {
    Rain,
    Temperature,
    Moisture,
    Salinity,
    /// Tree count per pixel from `worldgen::trees::trees_in` (not a habitat field).
    Trees,
}

impl Layer {
    fn parse(name: &str) -> Layer {
        match name {
            "rain" => Layer::Rain,
            "temperature" => Layer::Temperature,
            "moisture" => Layer::Moisture,
            "salinity" => Layer::Salinity,
            "trees" => Layer::Trees,
            other => panic!(
                "unknown --layer {other}; one of rain, temperature, moisture, salinity, trees, or with --world: biome, \
                 temperature, precipitation, drainage, regions"
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Layer::Rain => "rain",
            Layer::Temperature => "temperature",
            Layer::Moisture => "moisture",
            Layer::Salinity => "salinity",
            Layer::Trees => "trees",
        }
    }

    /// The field's value and the range mapped onto black..white.
    fn value(self, h: &Habitat) -> (f32, f32, f32) {
        match self {
            Layer::Rain => (h.rain, 0.0, 3.0),
            Layer::Temperature => (h.temperature_c, -25.0, 30.0),
            Layer::Moisture => (h.moisture, 0.0, 3.0),
            Layer::Salinity => (h.salinity, 0.0, 1.0),
            Layer::Trees => unreachable!("the trees layer is drawn by write_trees"),
        }
    }
}

fn parse_args() -> Args {
    let mut a = Args {
        span_m: DEFAULT_SPAN_M,
        size: DEFAULT_SIZE_PX,
        center: DEFAULT_CENTER_M,
        out: PathBuf::from(DEFAULT_OUT),
        seed: worldgen::WORLD_SEED,
        relief: DEFAULT_RELIEF,
        world: None,
        layer: None,
        forged_layer: None,
        tiles: false,
    };
    let mut layer_name: Option<String> = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--span" => a.span_m = value().parse().expect("--span <meters>"),
            "--size" => a.size = value().parse().expect("--size <pixels>"),
            "--center" => {
                let v = value();
                let (x, z) = v.split_once(',').expect("--center <x,z> in meters");
                a.center = (x.parse().expect("center x"), z.parse().expect("center z"));
            }
            "--out" => a.out = PathBuf::from(value()),
            "--seed" => {
                let v = value();
                let hex = v.trim_start_matches("0x").replace('_', "");
                a.seed = u64::from_str_radix(&hex, 16).expect("--seed <hex>");
            }
            "--relief" => a.relief = value().parse().expect("--relief <factor>"),
            "--world" => a.world = Some(PathBuf::from(value())),
            "--layer" => layer_name = Some(value()),
            "--tiles" => a.tiles = true,
            other => panic!(
                "unknown argument {other}; flags: --span --size --center --out --seed --relief --world --layer --tiles"
            ),
        }
    }
    // Resolved after parsing so the flag order does not matter: with a forged
    // world "temperature" is its coloured raster, not the habitat grey.
    if let Some(name) = layer_name {
        match ForgedLayer::parse(&name) {
            Some(f) if a.world.is_some() || name != "temperature" => a.forged_layer = Some(f),
            _ => a.layer = Some(Layer::parse(&name)),
        }
    }
    a
}

/// One surface height per pixel, row-major (z rows, x columns), and for a
/// forged world the fresh-water surface over it (NaN = none).
fn sample_heights(a: &Args) -> (Vec<f32>, Vec<f32>) {
    let forged = a.world.as_ref().map(|path| {
        worldgen::forged::ForgedWorld::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    });
    let p = TerrainPipeline::new(a.seed);
    let step = a.span_m / a.size as f64;
    let (x0, z0) = (a.center.0 - a.span_m * 0.5, a.center.1 - a.span_m * 0.5);
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = ((cores as f64 * CORE_SHARE) as usize).max(1);
    let rows_per = a.size.div_ceil(threads);

    let mut heights = vec![0f32; a.size * a.size];
    let mut waters = vec![f32::NAN; a.size * a.size];
    std::thread::scope(|s| {
        let bands = heights.chunks_mut(rows_per * a.size).zip(waters.chunks_mut(rows_per * a.size));
        for (band, (rows, water_rows)) in bands.enumerate() {
            let (p, forged) = (&p, &forged);
            s.spawn(move || {
                let rows = rows.chunks_mut(a.size).zip(water_rows.chunks_mut(a.size));
                for (r, (row, water_row)) in rows.enumerate() {
                    let z = z0 + ((band * rows_per + r) as f64 + 0.5) * step;
                    for (c, (h, water)) in row.iter_mut().zip(water_row.iter_mut()).enumerate() {
                        let x = x0 + (c as f64 + 0.5) * step;
                        match forged {
                            Some(world) => {
                                let sample = world.sample(x, z);
                                *h = sample.ground_y as f32;
                                *water = sample.water_y.map_or(f32::NAN, |w| w as f32);
                            }
                            None => *h = surface_m(&p.column(x, z)) as f32,
                        }
                    }
                }
            });
        }
    });
    (heights, waters)
}

/// Base colour for a column of ground height `h` (world metres): see
/// `worldgen::tile_map::{land_color, sea_color}`.
fn base_color(h: f64) -> [f64; 3] {
    if surface_block(h) < SEA_TOP_BLOCK {
        sea_color(SEA_LEVEL - h)
    } else {
        land_color(h - SEA_LEVEL)
    }
}

fn percentile(sorted: &[f32], q: f64) -> f32 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// `--tiles`: the world map as the game shows it, one PNG per layer.
fn write_tiles(a: &Args) {
    use worldgen::tile_map::{build_world_map, rasterize, MapLayer};
    let path = a.world.as_ref().expect("--tiles needs --world <file>");
    let world = worldgen::forged::ForgedWorld::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let started = std::time::Instant::now();
    let map = build_world_map(&world);
    println!("world map: {}x{} tiles in {:?}", map.width, map.height, started.elapsed());
    let packet = game::encode_packet(&game::ServerPacket::WorldMap(map.clone())).expect("encode");
    println!("WorldMap packet: {} bytes on the wire", packet.len());
    // Relief of the tiles with a landing spot.
    let mut relief: Vec<f32> = map.tiles.iter().filter(|t| t.landing.is_some()).map(|t| t.relief_m).collect();
    relief.sort_by(f32::total_cmp);
    if !relief.is_empty() {
        let p = |q: f64| relief[((relief.len() - 1) as f64 * q).round() as usize];
        println!(
            "landing tiles: {} relief_m p10 {:.0} p50 {:.0} p75 {:.0} p90 {:.0} p99 {:.0} max {:.0}",
            relief.len(),
            p(0.1),
            p(0.5),
            p(0.75),
            p(0.9),
            p(0.99),
            p(1.0),
        );
    }
    if let Some(dir) = a.out.parent() {
        std::fs::create_dir_all(dir).expect("create output directory");
    }
    for layer in MapLayer::ALL {
        let (w, h, px) = rasterize(&map, layer);
        let img = image::RgbaImage::from_raw(w, h, px.into_iter().flatten().collect()).expect("buffer size");
        let out = format!("{}-tiles-{}.png", a.out.display(), layer.label().to_lowercase());
        img.save(&out).expect("write tiles png");
        println!("wrote {out}");
    }
}

fn main() {
    let a = parse_args();
    if a.tiles {
        write_tiles(&a);
        return;
    }
    let n = a.size;
    let step = a.span_m / n as f64;
    println!(
        "world map: seed {:#018X}, center ({}, {}) m, span {} m, {n}x{n} px ({step:.2} m/px)",
        a.seed, a.center.0, a.center.1, a.span_m
    );

    let started = std::time::Instant::now();
    let (heights, waters) = sample_heights(&a);
    println!("sampled {} columns in {:?}", n * n, started.elapsed());

    // Hillshade: central differences over the height field, lit from the
    // north-west (top-left of the image), sun 45 degrees up.
    let at = |x: usize, z: usize| heights[z * n + x] as f64;
    let sun = sun_dir();
    let mut color = ImageBuffer::<Rgb<u8>, Vec<u8>>::new(n as u32, n as u32);
    let mut gray = ImageBuffer::<Luma<u16>, Vec<u16>>::new(n as u32, n as u32);
    let mut slopes = Vec::new(); // land slope, meters of rise per meter
    for z in 0..n {
        for x in 0..n {
            let h = at(x, z);
            let (xl, xr) = (x.saturating_sub(1), (x + 1).min(n - 1));
            let (zu, zd) = (z.saturating_sub(1), (z + 1).min(n - 1));
            let dhdx = (at(xr, z) - at(xl, z)) / ((xr - xl) as f64 * step);
            let dhdz = (at(x, zd) - at(x, zu)) / ((zd - zu) as f64 * step);

            let fresh = waters[z * n + x]; // lake or river surface above this column
            let sample = PointSample { ground_y: h, water_y: (!fresh.is_nan()).then_some(fresh as f64) };
            let water = !is_dry(&sample);
            let shade = if water {
                1.0 // the sea surface is flat; depth already carries the shape
            } else {
                slopes.push((dhdx * dhdx + dhdz * dhdz).sqrt() as f32);
                let nrm = [-dhdx * a.relief, 1.0, -dhdz * a.relief];
                let len = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
                let lambert = (nrm[0] * sun[0] + nrm[1] * sun[1] + nrm[2] * sun[2]) / len;
                // Flat ground (lambert = sun.y) keeps its base colour.
                (0.35 + 0.65 * lambert.max(0.0) / sun[1]).min(1.35)
            };
            let c = if fresh.is_nan() || (fresh as f64) <= h {
                base_color(h)
            } else {
                // Fresh water, darker with depth.
                lake_color(fresh as f64 - h)
            };
            let px = |v: f64| (v * shade).clamp(0.0, 255.0) as u8;
            color.put_pixel(x as u32, z as u32, Rgb([px(c[0]), px(c[1]), px(c[2])]));

            let t = (h - HEIGHT_MIN_M) / (HEIGHT_MAX_M - HEIGHT_MIN_M);
            gray.put_pixel(x as u32, z as u32, Luma([(t.clamp(0.0, 1.0) * 65535.0) as u16]));
        }
    }

    if let Some(dir) = a.out.parent() {
        std::fs::create_dir_all(dir).expect("create output directory");
    }
    let stem = a.out.to_string_lossy();
    let (color_path, height_path) = (format!("{stem}-color.png"), format!("{stem}-height.png"));
    color.save(&color_path).expect("write colour map");
    gray.save(&height_path).expect("write heightmap");
    println!("wrote {color_path}\nwrote {height_path}");

    if let Some(layer) = a.layer {
        let path = format!("{stem}-{}.png", layer.name());
        write_layer(&a, layer, &heights).save(&path).expect("write habitat layer");
        println!("wrote {path}");
    }

    if let Some(layer) = a.forged_layer {
        let path = format!("{stem}-{}.png", layer.name());
        write_forged_layer(&a, layer).save(&path).expect("write forged layer");
        println!("wrote {path}");
    }

    // Terrain stats — the numbers behind "is it flat / is it all ocean".
    let mut sorted = heights.clone();
    sorted.sort_by(f32::total_cmp);
    let land = slopes.len();
    println!(
        "land {:.1}% of columns (sea surface y = {SEA_LEVEL})",
        100.0 * land as f64 / (n * n) as f64
    );
    print!("surface height m:");
    for (label, q) in [("min", 0.0), ("p10", 0.1), ("p50", 0.5), ("p90", 0.9), ("p99", 0.99), ("max", 1.0)] {
        print!(" {label} {:.1}", percentile(&sorted, q));
    }
    println!();
    if land > 0 {
        slopes.sort_by(f32::total_cmp);
        print!("land slope (rise/run, sampled at {step:.2} m):");
        for (label, q) in [("p50", 0.5), ("p90", 0.9), ("p99", 0.99), ("max", 1.0)] {
            print!(" {label} {:.3}", percentile(&slopes, q));
        }
        println!();
    }
}

/// A forged world's raster layer, one colour per pixel: the cell nearest the
/// pixel's centre, in the world map's own colours (`Biome::map_rgb`,
/// `tile_map` ramps and `region_color`); open sea dimmed on the climate layers.
fn write_forged_layer(a: &Args, layer: ForgedLayer) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    use worldgen::tile_map::{drainage_color, precipitation_color, region_color, temperature_color, OCEAN_DIM};
    let path = a.world.as_ref().expect("--layer biome|temperature|precipitation|drainage|regions needs --world <file>");
    let world = worldgen::forged::ForgedWorld::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let n = a.size;
    let step = a.span_m / n as f64;
    let (x0, z0) = (a.center.0 - a.span_m * 0.5, a.center.1 - a.span_m * 0.5);
    let mut img = ImageBuffer::<Rgb<u8>, Vec<u8>>::new(n as u32, n as u32);
    for iz in 0..n {
        for ix in 0..n {
            let (x, z) = (x0 + (ix as f64 + 0.5) * step, z0 + (iz as f64 + 0.5) * step);
            let c = world.cell_climate(x, z);
            let dim = |rgb: [f64; 3]| {
                let k = if c.height_m < 0.0 && !c.is_lake { OCEAN_DIM } else { 1.0 };
                rgb.map(|v| (v * k).round().clamp(0.0, 255.0) as u8)
            };
            let rgb = match layer {
                ForgedLayer::Biome => world.biome_cell(x, z).map_rgb(),
                ForgedLayer::Temperature => dim(temperature_color(f64::from(c.temperature_c))),
                ForgedLayer::Precipitation => dim(precipitation_color(f64::from(c.precip_mm))),
                ForgedLayer::Drainage => dim(drainage_color(f64::from(c.drainage))),
                ForgedLayer::Regions => {
                    let (id, kind) = world.region_cell(x, z);
                    region_color(id, kind)
                }
            };
            img.put_pixel(ix as u32, iz as u32, Rgb(rgb));
        }
    }
    img
}

/// One habitat field per pixel as a greyscale ramp, sampled the way the
/// generator will ask: the pixel's surface block from the map's own heights.
fn write_layer(a: &Args, layer: Layer, heights: &[f32]) -> ImageBuffer<Luma<u8>, Vec<u8>> {
    let forged = a.world.as_ref().map(|path| {
        worldgen::forged::ForgedWorld::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    });
    let procedural = ProceduralHabitat::new(a.seed);
    let source: &(dyn HabitatSource + Sync) = match &forged {
        Some(world) => world,
        None => &procedural,
    };
    if matches!(layer, Layer::Trees) {
        return write_trees(a, source, heights);
    }
    let n = a.size;
    let step = a.span_m / n as f64;
    let (x0, z0) = (a.center.0 - a.span_m * 0.5, a.center.1 - a.span_m * 0.5);
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let rows_per = n.div_ceil(((cores as f64 * CORE_SHARE) as usize).max(1));
    let mut pixels = vec![0u8; n * n];
    std::thread::scope(|s| {
        for (band, rows) in pixels.chunks_mut(rows_per * n).enumerate() {
            s.spawn(move || {
                for (r, row) in rows.chunks_mut(n).enumerate() {
                    let iz = band * rows_per + r;
                    let z = z0 + (iz as f64 + 0.5) * step;
                    for (c, px) in row.iter_mut().enumerate() {
                        let x = x0 + (c as f64 + 0.5) * step;
                        let block = surface_block(heights[iz * n + c] as f64);
                        let (v, lo, hi) = layer.value(&source.habitat(x, z, block));
                        *px = (((v - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0) as u8;
                    }
                }
            });
        }
    });
    ImageBuffer::from_raw(n as u32, n as u32, pixels).expect("pixel buffer matches the image size")
}

/// Trees per pixel (white at `TREES_PER_PIXEL_FULL`), placed exactly as the
/// generator will: `trees_in` over the map window, surface from the map's own
/// heights (lakes are not modelled here; open sea columns count as water).
fn write_trees(a: &Args, source: &(dyn HabitatSource + Sync), heights: &[f32]) -> ImageBuffer<Luma<u8>, Vec<u8>> {
    const TREES_PER_PIXEL_FULL: f64 = 4.0;
    let n = a.size;
    let step = a.span_m / n as f64;
    let (x0, z0) = (a.center.0 - a.span_m * 0.5, a.center.1 - a.span_m * 0.5);
    let pixel = |v: i32, origin: f64| (((v as f64 + 0.5 - origin) / step).floor().clamp(0.0, n as f64 - 1.0)) as usize;
    let surface = |x: i32, z: i32| {
        let block = surface_block(heights[pixel(z, z0) * n + pixel(x, x0)] as f64);
        (block >= SEA_TOP_BLOCK).then_some(block)
    };
    let trees = worldgen::trees::trees_in(
        x0.floor() as i32,
        z0.floor() as i32,
        (x0 + a.span_m).ceil() as i32,
        (z0 + a.span_m).ceil() as i32,
        a.seed,
        source,
        &surface,
    );
    let mut counts = vec![0u32; n * n];
    for t in &trees {
        let (c, r) = (pixel(t.base[0], x0), pixel(t.base[2], z0));
        counts[r * n + c] += 1;
    }
    println!("{} trees", trees.len());
    let pixels = counts.iter().map(|&c| ((c as f64 / TREES_PER_PIXEL_FULL).min(1.0) * 255.0) as u8).collect();
    ImageBuffer::from_raw(n as u32, n as u32, pixels).expect("pixel buffer matches the image size")
}
