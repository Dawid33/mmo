//! Vertical slice of the ground the engine generates: soil, forge rock layers,
//! bedrock. Reads blocks through the engine's own `Column` / `ColumnGround`
//! (materialised band from the column, deeper blocks from `block_at`), so what
//! it shows is what the game would build.
//!
//! Run: just cross-section                       (default world if present, else procedural)
//!      just cross-section --world target/world-forge/xsec.world --region 1,0 --axis z --at 20
//!
//! Flags: --world <file> --region X,Z --axis x|z --at <block in region> --out <png> --seed <hex>.
//! `just cross-section` builds at the play region size (`play_chunks`), so
//! `--region` / `--at` address the same grid as the game; an `--at` outside
//! the region is an error.
//! Axis x runs the image along +x at a fixed z (`--at`, default the region
//! centre); axis z runs along +z at a fixed x. One pixel per block, from the
//! highest surface + 8 down to the deepest bedrock + 2. Also prints the stratum
//! sequence (block, top y, bottom y) of the centre block column.

use std::collections::BTreeMap;
use std::path::PathBuf;

use game::{Block, BlockId, BlockPos, Column, ReadVol, RegionCoords, CHUNK_BLOCKS, REGION_CHUNKS};
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
                seed = u64::from_str_radix(v.trim_start_matches("0x").replace('_', "").as_str(), 16).expect("--seed <hex>");
            }
            other => panic!("unknown argument {other}; flags: --world --region --axis --at --out --seed"),
        }
    }
    if world.is_none() && std::path::Path::new(DEFAULT_WORLD).exists() {
        world = Some(PathBuf::from(DEFAULT_WORLD));
    }

    let manifest = include_str!("../../../assets/blocks/blocks.ron");
    let reg = game::BlockRegistry::from_ron(manifest).expect("valid block manifest");
    let mats = Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")));
    let coords = RegionCoords { x: region.0, z: region.1 };
    let (label, columns) = match &world {
        Some(path) => {
            let w = worldgen::forged::ForgedWorld::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let stem = path.file_stem().map_or("world".into(), |s| s.to_string_lossy().into_owned());
            (stem, worldgen::generate_region_forged(&w, coords, mats))
        }
        None => ("procedural".to_string(), worldgen::generate_region(coords, seed, mats)),
    };
    let by_coords: BTreeMap<(usize, usize), &Column> = columns.iter().map(|(c, col)| ((c.x, c.z), col)).collect();

    let n = REGION_CHUNKS * CHUNK_BLOCKS; // blocks along an edge
    let at = at.unwrap_or(n / 2);
    if at >= n {
        eprintln!("--at {at} is outside the region: blocks run 0..{n} (REGION_CHUNKS = {REGION_CHUNKS})");
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
        let g = by_coords[&(bx / CHUNK_BLOCKS, bz / CHUNK_BLOCKS)].ground().expect("generated column has ground");
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
    println!("{} ({}x{} px, y {bottom}..={top})", out.display(), n, height);

    // Stratum sequence of the centre block column of the slice.
    let u = n / 2;
    let name = |id: BlockId| -> String {
        reg.def(id).map_or_else(|| format!("#{}", id.0), |d| d.name.to_string())
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
