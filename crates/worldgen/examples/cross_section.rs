//! Vertical slice of the ground the engine generates: cover, soil, forge rock
//! layers, standing water. Reads the region's stud ground recipes
//! (`generate_stud_region` -> `StudColumnGround::material_at`), so what it
//! shows is what the game would build (trees, feature bricks, are not drawn).
//!
//! Run: just cross-section                       (the default world file, worlds/default.world)
//!      just cross-section --world target/world-forge/xsec.world --region 1,0 --axis z --at 20
//!      just cross-section --world procedural      (the noise world)
//!
//! Flags: --world <file>|procedural --region X,Z --axis x|z --at <block in region> --depth <m> --out <png> --seed <hex>.
//! A missing world file is an error, never a silent procedural slice: worlds
//! live in world stores now, so write the file out first with
//! `just worldctl export --db <store> <out.world>`.
//! `just cross-section` builds at the play region size (`play_chunks`), so
//! `--region` / `--at` address the same grid as the game; an `--at` outside
//! the region is an error.
//! Axis x runs the image along +x at a fixed z (`--at`, default the region
//! centre); axis z runs along +z at a fixed x. One stud column is
//! `PX_PER_STUD` pixels wide and one plate `PX_PER_PLATE` tall (10 px per
//! metre both ways), from the highest surface (or water) + 8 m down to
//! `--depth` metres (default 70) under the lowest surface. Also prints the
//! stratum sequence (material, top plate, bottom plate) of the centre column.

use std::collections::BTreeMap;
use std::path::PathBuf;

use game::{stud_material, RegionCoords, StudColumnGround, REGION_BLOCKS, STUD_CHUNK_STUDS};
use image::{ImageBuffer, Rgb};
use worldgen::forge::geology::ROCKS;
use worldgen::stud_gen::{PLATES_PER_M, STUDS_PER_M};

const DEFAULT_WORLD: &str = "worlds/default.world";
/// Metres drawn above the highest surface and (by default) under the lowest.
const ABOVE_SURFACE_M: i32 = 8;
const DEFAULT_DEPTH_M: i32 = 70;
/// A stud is 0.5 m wide, a plate 0.2 m tall: 10 px per metre both ways.
const PX_PER_STUD: u32 = 5;
const PX_PER_PLATE: u32 = 2;
const SKY: [u8; 3] = [168, 206, 236];
const WATER: [u8; 3] = [48, 92, 180];

fn main() {
    let mut world: Option<PathBuf> = None;
    let mut region = (0i32, 0i32);
    let mut axis_x = true;
    let mut at: Option<usize> = None;
    let mut depth_m = DEFAULT_DEPTH_M;
    let mut out: Option<PathBuf> = None;
    let mut seed = worldgen::WORLD_SEED;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--world" => world = Some(PathBuf::from(value())),
            "--region" => {
                let v = value();
                let (x, z) = v.split_once(',').expect("--region X,Z");
                region = (x.parse().expect("region x"), z.parse().expect("region z"));
            }
            "--axis" => {
                axis_x = match value().as_str() {
                    "x" => true,
                    "z" => false,
                    _ => panic!("--axis x|z"),
                }
            }
            "--at" => at = Some(value().parse().expect("--at <block>")),
            "--depth" => depth_m = value().parse().expect("--depth <m>"),
            "--out" => out = Some(PathBuf::from(value())),
            "--seed" => {
                let v = value();
                seed = u64::from_str_radix(v.trim_start_matches("0x").replace('_', "").as_str(), 16).expect("--seed <hex>");
            }
            other => panic!("unknown argument {other}; flags: --world --region --axis --at --depth --out --seed"),
        }
    }
    let world = match world {
        Some(path) if path.as_os_str() == worldgen::forged::PROCEDURAL_KEYWORD => None,
        Some(path) => Some(path),
        None => Some(PathBuf::from(DEFAULT_WORLD)),
    };
    if let Some(path) = world.as_ref().filter(|p| !p.exists()) {
        eprintln!(
            "{}: no such world file; export one from its store with `just worldctl export --db <store> <out.world>` \
             (or pass --world procedural for the noise world)",
            path.display()
        );
        std::process::exit(2);
    }

    let coords = RegionCoords { x: region.0, z: region.1 };
    let forged = world
        .as_ref()
        .map(|path| worldgen::forged::ForgedWorld::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
    let (label, seed) = match (&world, &forged) {
        (Some(path), Some(w)) => {
            let stem = path.file_stem().map_or("world".into(), |s| s.to_string_lossy().into_owned());
            (stem, worldgen::stud_gen::generate_stud_region_forged(w, coords))
        }
        _ => ("procedural".to_string(), worldgen::stud_gen::generate_stud_region(coords, seed)),
    };
    let by_coords: BTreeMap<(usize, usize), &StudColumnGround> = seed.ground.iter().map(|(c, g)| ((c.x, c.z), g)).collect();

    let n = REGION_BLOCKS; // blocks along an edge
    let at = at.unwrap_or(n / 2);
    if at >= n {
        eprintln!("--at {at} is outside the region: blocks run 0..{n} (REGION_BLOCKS = {REGION_BLOCKS})");
        std::process::exit(2);
    }
    // Stud column `u` along the slice (the slice runs through the first stud of block `at`).
    let studs = n * STUDS_PER_M as usize;
    let cs = STUD_CHUNK_STUDS;
    let column = |u: usize| -> (&StudColumnGround, usize, usize) {
        let (sx, sz) = if axis_x { (u, at * STUDS_PER_M as usize) } else { (at * STUDS_PER_M as usize, u) };
        (by_coords[&(sx / cs, sz / cs)], sx % cs, sz % cs)
    };

    let (mut top, mut bottom) = (i32::MIN, i32::MAX);
    for u in 0..studs {
        let (g, sx, sz) = column(u);
        let s = g.surface(sx, sz);
        top = top.max(s).max(g.water_top(sx, sz).unwrap_or(s));
        bottom = bottom.min(s);
    }
    // Plates `bottom..top` are drawn.
    let (top, bottom) = (top + ABOVE_SURFACE_M * PLATES_PER_M, bottom - depth_m * PLATES_PER_M);

    let colour = |id: u16| -> [u8; 3] {
        let name = stud_material(id).map_or("?", |m| m.name);
        if let Some(rock) = ROCKS.iter().find(|r| r.name == name) {
            return rock.rgb;
        }
        match name {
            "grass" => [86, 150, 60],
            "dirt" => [112, 80, 52],
            "sand" => [236, 224, 160],
            "bedrock" => [20, 20, 24],
            "stone" => [128, 128, 128],
            "snow" => [244, 246, 250],
            "ice" => [150, 200, 240],
            "glacier_ice" => [96, 214, 214],
            "mud" => [92, 72, 50],
            "peat" => [60, 44, 32],
            "gravel" => [150, 144, 136],
            "red_sand" => [200, 104, 52],
            "dry_grass" => [176, 168, 84],
            "podzol" => [96, 76, 44],
            "frozen_dirt" => [132, 120, 112],
            "salt" => [236, 236, 228],
            _ => [255, 0, 255],
        }
    };
    // What the slice shows at `plate` of stud column `u`: ground, the water over it, or sky.
    let pixel = |u: usize, plate: i32| -> [u8; 3] {
        let (g, sx, sz) = column(u);
        match g.material_at(sx, plate, sz) {
            Some(id) => colour(id),
            None if g.water_top(sx, sz).is_some_and(|w| plate < w) => WATER,
            None => SKY,
        }
    };

    let (width, height) = (studs as u32 * PX_PER_STUD, (top - bottom) as u32 * PX_PER_PLATE);
    let mut img = ImageBuffer::<Rgb<u8>, _>::new(width, height);
    for u in 0..studs {
        for plate in bottom..top {
            let rgb = Rgb(pixel(u, plate));
            let (x0, y0) = (u as u32 * PX_PER_STUD, (top - 1 - plate) as u32 * PX_PER_PLATE);
            for dx in 0..PX_PER_STUD {
                for dy in 0..PX_PER_PLATE {
                    img.put_pixel(x0 + dx, y0 + dy, rgb);
                }
            }
        }
    }
    let out = out.unwrap_or_else(|| {
        PathBuf::from(format!(
            "target/cross-section/{label}-{}_{}-{}{at}.png",
            region.0,
            region.1,
            if axis_x { "z" } else { "x" }
        ))
    });
    std::fs::create_dir_all(out.parent().unwrap_or(".".as_ref())).ok();
    img.save(&out).expect("write png");
    println!("{} ({width}x{height} px, plates {bottom}..{top}, {PLATES_PER_M} plates per metre)", out.display());

    // Stratum sequence of the centre stud column of the slice.
    let u = studs / 2;
    if let Some(w) = &forged {
        let b = u / STUDS_PER_M as usize;
        let (bx, bz) = if axis_x { (b, at) } else { (at, b) };
        let (x, z) = ((coords.x * n as i32 + bx as i32) as f64 + 0.5, (coords.z * n as i32 + bz as i32) as f64 + 0.5);
        let c = w.column_climate(x, z);
        println!(
            "centre column biome {} ({:.1} C, snow line {:.0} m); region biomes: {}",
            c.biome.name(),
            c.temperature_c,
            c.snow_line_m,
            region_biomes(w, coords, n)
        );
    }
    let (g, sx, sz) = column(u);
    let name = |plate: i32| -> &'static str {
        match g.material_at(sx, plate, sz) {
            Some(id) => stud_material(id).map_or("?", |m| m.name),
            None if g.water_top(sx, sz).is_some_and(|w| plate < w) => "water",
            None => "air",
        }
    };
    let mut run: Option<(&str, i32)> = None; // (material, top plate)
    println!("centre stud column u={u} (material, top plate, bottom plate):");
    for plate in (bottom..top).rev() {
        let now = name(plate);
        match run {
            Some((was, _)) if was == now => {}
            Some((was, from)) => {
                println!("  {was:<14} {from:>6} {:>6}", plate + 1);
                run = Some((now, plate));
            }
            None => run = Some((now, plate)),
        }
    }
    if let Some((was, from)) = run {
        println!("  {was:<14} {from:>6} {bottom:>6}");
    }
}

/// The dithered biomes of region `coords` (every 8th block column), by share.
fn region_biomes(w: &worldgen::forged::ForgedWorld, coords: RegionCoords, n: usize) -> String {
    let mut count: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut total = 0;
    for bx in (0..n).step_by(8) {
        for bz in (0..n).step_by(8) {
            let (x, z) = ((coords.x * n as i32 + bx as i32) as f64 + 0.5, (coords.z * n as i32 + bz as i32) as f64 + 0.5);
            *count.entry(w.biome_at(x, z).name()).or_default() += 1;
            total += 1;
        }
    }
    let mut v: Vec<_> = count.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.iter().map(|(name, k)| format!("{name} {:.0}%", 100.0 * *k as f64 / total as f64)).collect::<Vec<_>>().join(", ")
}
