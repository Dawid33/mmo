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
//! Heights come straight from `TerrainPipeline::column` (the exact field, not
//! the 4 m `ProfileGrid` lattice `generate_region` interpolates — the two agree
//! to well under a block), so a map costs one profile per pixel and never
//! materializes a region.

use std::path::PathBuf;

use image::{ImageBuffer, Luma, Rgb};
use worldgen::pipeline::{ColumnProfile, SEA_LEVEL, SEA_TOP_BLOCK, TerrainPipeline};

// The world band the density ramp spans (pipeline.rs RAMP_FROM_Y/RAMP_TO_Y).
const HEIGHT_MIN_M: f64 = -64.0;
const HEIGHT_MAX_M: f64 = 320.0;

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
/// positive), so bisection finds it; 16 halvings of the 384 m band is ~6 mm.
fn surface_m(prof: &ColumnProfile) -> f64 {
    if prof.density_at(HEIGHT_MIN_M) <= 0.0 {
        return HEIGHT_MIN_M;
    }
    if prof.density_at(HEIGHT_MAX_M) > 0.0 {
        return HEIGHT_MAX_M;
    }
    let (mut lo, mut hi) = (HEIGHT_MIN_M, HEIGHT_MAX_M);
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if prof.density_at(mid) > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Topmost solid block for a continuous surface height — `generate_region`'s
/// rule: a block is solid when its CENTER (`by + 0.5`) is below the surface.
fn surface_block(h: f64) -> i32 {
    (h - 0.5).ceil() as i32 - 1
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
    };
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
            other => panic!(
                "unknown argument {other}; flags: --span --size --center --out --seed --relief --world"
            ),
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

/// Base colour for a column. Follows `Materials::material_at`: sand through
/// one block above the sea top, grass above — the grass tinted by elevation so
/// height stays readable; water darkens with depth.
fn base_color(h: f64) -> [f64; 3] {
    let by = surface_block(h);
    if by < SEA_TOP_BLOCK {
        let depth = SEA_LEVEL - h;
        return ramp(
            &[
                (0.0, [110.0, 190.0, 215.0]),
                (8.0, [60.0, 140.0, 200.0]),
                (40.0, [30.0, 80.0, 160.0]),
                (128.0, [12.0, 30.0, 90.0]),
            ],
            depth,
        );
    }
    if by <= SEA_TOP_BLOCK + 1 {
        return [222.0, 205.0, 150.0]; // sand
    }
    ramp(
        &[
            (0.0, [96.0, 168.0, 84.0]),
            (30.0, [70.0, 140.0, 66.0]),
            (80.0, [120.0, 130.0, 80.0]),
            (140.0, [140.0, 120.0, 100.0]),
            (200.0, [170.0, 165.0, 160.0]),
            (250.0, [245.0, 245.0, 250.0]),
        ],
        h - SEA_LEVEL,
    )
}

fn percentile(sorted: &[f32], q: f64) -> f32 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn main() {
    let a = parse_args();
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
    let sun = {
        let v = [-1.0f64, 1.4142, -1.0];
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / len, v[1] / len, v[2] / len]
    };
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
            let water = surface_block(h) < SEA_TOP_BLOCK || (fresh as f64) > h;
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
                ramp(&[(0.0, [120.0, 185.0, 225.0]), (3.0, [60.0, 130.0, 205.0])], fresh as f64 - h)
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
