//! Vertical slice of the ground the engine generates: soil, forge rock layers,
//! bedrock. Reads blocks through the engine's own `Column` / `ColumnGround`
//! (materialised band from the column, deeper blocks from `block_at`), so what
//! it shows is what the game would build.
//!
//! Run: just cross-section                       (the default world file, worlds/default.world)
//!      just cross-section --world target/world-forge/xsec.world --region 1,0 --axis z --at 20
//!      just cross-section --world procedural      (the noise world)
//!
//! Flags: --world <file>|procedural --region X,Z --axis x|z --at <block in region> --out <png> --seed <hex>.
//! A missing world file is an error, never a silent procedural slice: worlds
//! live in world stores now, so write the file out first with
//! `just worldctl export --db <store> <out.world>`.
//! `just cross-section` builds at the play region size (`play_chunks`), so
//! `--region` / `--at` address the same grid as the game; an `--at` outside
//! the region is an error.
//! Axis x runs the image along +x at a fixed z (`--at`, default the region
//! centre); axis z runs along +z at a fixed x. One pixel per block, from the
//! highest surface + 8 down to the deepest bedrock + 2. Also prints the stratum
//! sequence (block, top y, bottom y) of the centre block column.

use std::collections::BTreeMap;
use std::path::PathBuf;

use game::{Block, BlockId, BlockPos, CHUNK_BLOCKS, Column, REGION_BLOCKS, ReadVol, RegionCoords};
use image::{ImageBuffer, Rgb};
use worldgen::forge::geology::ROCKS;
use worldgen::material::Materials;

const DEFAULT_WORLD: &str = "worlds/default.world";
const ABOVE_SURFACE: i32 = 8;
const BELOW_BEDROCK: i32 = 2;

fn main() {
    let mut world: Option<PathBuf> = None;
    let mut region = (0i32, 0i32);
    let mut axis_x = true;
    let mut at: Option<usize> = None;
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
            "--out" => out = Some(PathBuf::from(value())),
            "--seed" => {
                let v = value();
                seed =
                    u64::from_str_radix(v.trim_start_matches("0x").replace('_', "").as_str(), 16)
                        .expect("--seed <hex>");
            }
            other => {
                panic!("unknown argument {other}; flags: --world --region --axis --at --out --seed")
            }
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

    let manifest = include_str!("../../../assets/blocks/blocks.ron");
    let reg = game::BlockRegistry::from_ron(manifest).expect("valid block manifest");
    let mats =
        Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")));
    let coords = RegionCoords {
        x: region.0,
        z: region.1,
    };
    let forged = world.as_ref().map(|path| {
        worldgen::forged::ForgedWorld::load(path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    });
    let (label, columns) = match (&world, &forged) {
        (Some(path), Some(w)) => {
            let stem = path
                .file_stem()
                .map_or("world".into(), |s| s.to_string_lossy().into_owned());
            (stem, worldgen::generate_region_forged(w, coords, mats))
        }
        _ => (
            "procedural".to_string(),
            worldgen::generate_region(coords, seed, mats),
        ),
    };
    let by_coords: BTreeMap<(usize, usize), &Column> =
        columns.iter().map(|(c, col)| ((c.x, c.z), col)).collect();

    let n = REGION_BLOCKS; // blocks along an edge
    let at = at.unwrap_or(n / 2);
    if at >= n {
        eprintln!(
            "--at {at} is outside the region: blocks run 0..{n} (REGION_BLOCKS = {REGION_BLOCKS})"
        );
        std::process::exit(2);
    }
    // Block (u, v) of the region -> (x, z) by axis.
    let block_xz = |u: usize| if axis_x { (u, at) } else { (at, u) };
    let read = |u: usize, y: i32| -> Block {
        let (bx, bz) = block_xz(u);
        let col = by_coords[&(bx / CHUNK_BLOCKS, bz / CHUNK_BLOCKS)];
        let (lx, lz) = (bx % CHUNK_BLOCKS, bz % CHUNK_BLOCKS);
        let ground = col.ground();
        match ground {
            Some(g) if g.is_implied(y.div_euclid(CHUNK_BLOCKS as i32)) => g.block_at(lx, lz, y),
            _ => col.get(BlockPos::new(lx as i32, y, lz as i32)),
        }
    };

    let (mut top, mut bottom) = (i32::MIN, i32::MAX);
    for u in 0..n {
        let (bx, bz) = block_xz(u);
        let g = by_coords[&(bx / CHUNK_BLOCKS, bz / CHUNK_BLOCKS)]
            .ground()
            .expect("generated column has ground");
        let i = (bx % CHUNK_BLOCKS) * CHUNK_BLOCKS + bz % CHUNK_BLOCKS;
        let s = i32::from(g.surface[i]);
        top = top.max(s);
        bottom = bottom.min(s - game::UNDERGROUND_DEPTH);
    }
    let (top, bottom) = (top + ABOVE_SURFACE, bottom - BELOW_BEDROCK);

    let colour = |id: BlockId| -> [u8; 3] {
        if id == BlockId::AIR {
            [168, 206, 236]
        } else if id == mats.grass {
            [86, 150, 60]
        } else if id == mats.dirt {
            [112, 80, 52]
        } else if id == mats.sand {
            [236, 224, 160]
        } else if id == mats.water {
            [48, 92, 180]
        } else if id == mats.bedrock {
            [20, 20, 24]
        } else if let Some(r) = mats.rocks.iter().position(|&r| r == id) {
            ROCKS[r].rgb
        } else if id == mats.stone {
            [128, 128, 128]
        } else if let Some(rgb) = biome_colour(&mats.biome, id) {
            rgb
        } else if let Some(rgb) = tree_colour(&mats.trees, id) {
            rgb
        } else {
            [255, 0, 255]
        }
    };

    let height = (top - bottom + 1) as u32;
    let mut img = ImageBuffer::<Rgb<u8>, _>::new(n as u32, height);
    for u in 0..n {
        for y in bottom..=top {
            img.put_pixel(u as u32, (top - y) as u32, Rgb(colour(read(u, y).id)));
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
    println!(
        "{} ({}x{} px, y {bottom}..={top})",
        out.display(),
        n,
        height
    );

    // Stratum sequence of the centre block column of the slice.
    let u = n / 2;
    if let Some(w) = &forged {
        let (bx, bz) = block_xz(u);
        let (x, z) = (
            (coords.x * n as i32 + bx as i32) as f64 + 0.5,
            (coords.z * n as i32 + bz as i32) as f64 + 0.5,
        );
        let c = w.column_climate(x, z);
        println!(
            "centre column biome {} ({:.1} C, snow line {:.0} m); region biomes: {}",
            c.biome.name(),
            c.temperature_c,
            c.snow_line_m,
            region_biomes(w, coords, n)
        );
    }
    let name = |id: BlockId| -> String {
        reg.def(id)
            .map_or_else(|| format!("#{}", id.0), |d| d.name.to_string())
    };
    let mut run: Option<(BlockId, i32)> = None; // (block, top y)
    println!("centre column u={u} (block, top y, bottom y):");
    for y in (bottom..=top).rev() {
        let id = read(u, y).id;
        match run {
            Some((rid, _)) if rid == id => {}
            Some((rid, ry)) => {
                println!("  {:<14} {ry:>5} {:>5}", name(rid), y + 1);
                run = Some((id, y));
            }
            None => run = Some((id, y)),
        }
    }
    if let Some((rid, ry)) = run {
        println!("  {:<14} {ry:>5} {bottom:>5}", name(rid));
    }
}

/// Colours of the biome cover and ice blocks (`None` for any other block).
fn biome_colour(b: &worldgen::material::BiomeBlocks, id: BlockId) -> Option<[u8; 3]> {
    let table = [
        (b.snow, [244, 246, 250]),
        (b.ice, [150, 200, 240]),
        (b.glacier_ice, [96, 214, 214]),
        (b.mud, [92, 72, 50]),
        (b.peat, [60, 44, 32]),
        (b.gravel, [150, 144, 136]),
        (b.red_sand, [200, 104, 52]),
        (b.dry_grass, [176, 168, 84]),
        (b.podzol, [96, 76, 44]),
        (b.frozen_dirt, [132, 120, 112]),
        (b.salt, [236, 236, 228]),
    ];
    table
        .iter()
        .find(|(block, _)| *block == id && id != BlockId::AIR)
        .map(|&(_, rgb)| rgb)
}

/// The dithered biomes of region `coords` (every 8th block column), by share.
fn region_biomes(w: &worldgen::forged::ForgedWorld, coords: RegionCoords, n: usize) -> String {
    let mut count: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut total = 0;
    for bx in (0..n).step_by(8) {
        for bz in (0..n).step_by(8) {
            let (x, z) = (
                (coords.x * n as i32 + bx as i32) as f64 + 0.5,
                (coords.z * n as i32 + bz as i32) as f64 + 0.5,
            );
            *count.entry(w.biome_at(x, z).name()).or_default() += 1;
            total += 1;
        }
    }
    let mut v: Vec<_> = count.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.iter()
        .map(|(name, k)| format!("{name} {:.0}%", 100.0 * *k as f64 / total as f64))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Tree blocks: wood brown, leaves green; a one-block (leafless) species
/// such as the cactus is its own green.
fn tree_colour(trees: &worldgen::material::TreeBlocks, id: BlockId) -> Option<[u8; 3]> {
    let s = trees
        .species
        .iter()
        .find(|s| [s.log, s.branch, s.leaves].contains(&id))?;
    Some(if s.log == s.leaves {
        [70, 130, 60]
    } else if id == s.leaves {
        [40, 104, 44]
    } else {
        [104, 74, 46]
    })
}
