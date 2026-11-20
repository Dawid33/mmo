//! Orthographic PNGs of one generated tree: front (looking along +z), side
//! (looking along −x) and top, one pixel per voxel, coloured by part and shaded
//! by depth. `--grid` instead writes 3×3 sheets (front and top) of nine seeds;
//! `--time N` prints the generation cost per tree over N seeds.
//!
//! `just tree-view --species oak|pine|birch|spruce|acacia|palm|jungle|cactus --seed N --age N [--out <stem>] [--scale N] [--grid] [--time N]`
//! Output: `target/tree-view/<species>-s<seed>-a<age>-{front,side,top}.png` (or `-grid.png`, `-grid-top.png`).

use std::collections::HashMap;
use std::path::PathBuf;

use image::{Rgb, RgbImage};
use worldgen::trees::{MAX_AGE, SPECIES, TreeBlock, TreePart, rasterize, skeleton};

const BV: i32 = 16;
const SKY: [u8; 3] = [200, 220, 235];
const GROUND: [u8; 3] = [110, 85, 60];
/// Blocks of empty margin around the tree.
const MARGIN: i32 = 1;

struct Palette {
    log: [u8; 3],
    branch: [u8; 3],
    leaves: [u8; 3],
    /// Birch bark: dark marks on light wood.
    marks: Option<[u8; 3]>,
}

fn palette(species: &str) -> Palette {
    match species {
        "pine" => Palette {
            log: [105, 62, 40],
            branch: [120, 78, 52],
            leaves: [38, 82, 52],
            marks: None,
        },
        "birch" => Palette {
            log: [228, 225, 214],
            branch: [200, 196, 185],
            leaves: [124, 170, 66],
            marks: Some([40, 38, 36]),
        },
        "spruce" => Palette {
            log: [88, 58, 40],
            branch: [104, 70, 48],
            leaves: [30, 66, 48],
            marks: None,
        },
        "acacia" => Palette {
            log: [120, 96, 72],
            branch: [134, 108, 80],
            leaves: [112, 132, 52],
            marks: None,
        },
        "palm" => Palette {
            log: [150, 124, 88],
            branch: [120, 140, 60],
            leaves: [70, 140, 50],
            marks: None,
        },
        "jungle" => Palette {
            log: [96, 84, 62],
            branch: [110, 96, 70],
            leaves: [34, 104, 38],
            marks: None,
        },
        "cactus" => Palette {
            log: [70, 120, 60],
            branch: [80, 132, 66],
            leaves: [70, 120, 60],
            marks: None,
        },
        _ => Palette {
            log: [92, 68, 44],
            branch: [112, 84, 56],
            leaves: [62, 112, 44],
            marks: None,
        },
    }
}

fn hash(a: i32, b: i32, c: i32) -> u32 {
    let mut h = (a as u32).wrapping_mul(0x9E37_79B1)
        ^ (b as u32).wrapping_mul(0x85EB_CA77)
        ^ (c as u32).wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

/// Solid voxels of the tree, world-voxel coordinates relative to the base block.
fn voxels(blocks: &[TreeBlock]) -> Vec<([i32; 3], TreePart)> {
    let mut out = Vec::new();
    for b in blocks {
        let base = [b.offset[0] * BV, b.offset[1] * BV, b.offset[2] * BV];
        for vz in 0..BV {
            for vy in 0..BV {
                for vx in 0..BV {
                    out.push(([base[0] + vx, base[1] + vy, base[2] + vz], b.part));
                }
            }
        }
    }
    out
}

#[derive(Clone, Copy)]
enum View {
    Front,
    Side,
    Top,
}

fn render(blocks: &[TreeBlock], view: View, pal: &Palette, scale: u32) -> RgbImage {
    let vox = voxels(blocks);
    let lo = |i: usize| blocks.iter().map(|b| b.offset[i]).min().unwrap_or(0).min(0) - MARGIN;
    let hi = |i: usize| blocks.iter().map(|b| b.offset[i]).max().unwrap_or(0).max(0) + 1 + MARGIN;
    let (min, max) = (
        [lo(0) * BV, lo(1) * BV, lo(2) * BV],
        [hi(0) * BV, hi(1) * BV, hi(2) * BV],
    );
    // (u axis, v axis, depth axis, depth sign): screen u right, v down.
    let (u, v, w) = match view {
        View::Front => (0, 1, 2),
        View::Side => (2, 1, 0),
        View::Top => (0, 2, 1),
    };
    let (width, height) = ((max[u] - min[u]) as u32, (max[v] - min[v]) as u32);
    let depth_span = (max[w] - min[w]) as f32;
    let mut best: HashMap<(i32, i32), (i32, TreePart, [i32; 3])> = HashMap::new();
    for (p, part) in vox {
        // Front looks along +z (nearest = smallest z); side along −x (largest x);
        // top down (largest y).
        let d = match view {
            View::Front => p[2],
            View::Side | View::Top => -p[w],
        };
        let px = (
            p[u] - min[u],
            match view {
                View::Top => p[v] - min[v],
                _ => max[v] - 1 - p[v],
            },
        );
        let e = best.entry(px).or_insert((i32::MAX, part, p));
        if d < e.0 {
            *e = (d, part, p);
        }
    }
    let mut img = RgbImage::from_pixel(width * scale, height * scale, Rgb(SKY));
    if !matches!(view, View::Top) {
        // Ground below y = 0.
        let ground_rows = (0 - min[1]) as u32;
        for y in height - ground_rows..height {
            for x in 0..width {
                for s in 0..scale * scale {
                    img.put_pixel(x * scale + s % scale, y * scale + s / scale, Rgb(GROUND));
                }
            }
        }
    }
    for (&(x, y), &(d, part, p)) in &best {
        let base = match part {
            TreePart::Log => match pal.marks {
                Some(m) if hash(p[0] / 3, p[1] / 2, p[2] / 3) % 9 == 0 => m,
                _ => pal.log,
            },
            TreePart::Branch => pal.branch,
            TreePart::Leaves => pal.leaves,
        };
        // Nearer is lighter; leaves vary per block.
        let near = 1.0
            - ((d + if matches!(view, View::Front) {
                -min[w]
            } else {
                max[w]
            }) as f32
                / depth_span);
        let mut k = 0.55 + 0.45 * near;
        if part == TreePart::Leaves {
            let b = [
                p[0].div_euclid(BV),
                p[1].div_euclid(BV),
                p[2].div_euclid(BV),
            ];
            k *= 0.88 + 0.24 * (hash(b[0], b[1], b[2]) % 100) as f32 / 100.0;
        }
        let c = Rgb([
            (base[0] as f32 * k).min(255.0) as u8,
            (base[1] as f32 * k).min(255.0) as u8,
            (base[2] as f32 * k).min(255.0) as u8,
        ]);
        for s in 0..scale * scale {
            img.put_pixel(
                x as u32 * scale + s % scale,
                y as u32 * scale + s / scale,
                c,
            );
        }
    }
    img
}

fn main() {
    let mut species = "oak".to_string();
    let (mut seed, mut age, mut scale) = (1u64, 60u16, 1u32);
    let (mut out, mut grid, mut time) = (None::<PathBuf>, false, None::<u64>);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--species" => species = val(),
            "--seed" => seed = val().parse().expect("--seed N"),
            "--age" => age = val().parse::<u16>().expect("--age N").min(MAX_AGE),
            "--scale" => scale = val().parse().expect("--scale N"),
            "--out" => out = Some(PathBuf::from(val())),
            "--grid" => grid = true,
            "--time" => time = Some(val().parse::<u64>().expect("--time N").max(1)),
            _ => panic!(
                "unknown flag {a}; flags: --species <name in SPECIES> --seed N --age N --out <stem> --scale N --grid --time N"
            ),
        }
    }
    let sp = SPECIES
        .iter()
        .find(|s| s.name == species)
        .unwrap_or_else(|| panic!("no species {species}"));
    let pal = palette(sp.name);
    let stem = out
        .unwrap_or_else(|| PathBuf::from(format!("target/tree-view/{}-s{seed}-a{age}", sp.name)));
    if let Some(dir) = stem.parent() {
        std::fs::create_dir_all(dir).expect("create output dir");
    }
    let path = |suffix: &str| {
        let mut p = stem.clone().into_os_string();
        p.push(format!("-{suffix}.png"));
        PathBuf::from(p)
    };
    if grid {
        let trees: Vec<Vec<TreeBlock>> = (0..9)
            .map(|i| rasterize(&skeleton(sp, seed + i, age)))
            .collect();
        for (view, name) in [(View::Front, "grid"), (View::Top, "grid-top")] {
            let views: Vec<RgbImage> = trees.iter().map(|t| render(t, view, &pal, scale)).collect();
            let cw = views.iter().map(|v| v.width()).max().unwrap();
            let ch = views.iter().map(|v| v.height()).max().unwrap();
            let mut sheet = RgbImage::from_pixel(cw * 3, ch * 3, Rgb([255, 255, 255]));
            for (i, v) in views.iter().enumerate() {
                // Bottom-centre each tree in its cell.
                let (ox, oy) = (
                    (i as u32 % 3) * cw + (cw - v.width()) / 2,
                    (i as u32 / 3) * ch + ch - v.height(),
                );
                image::imageops::replace(&mut sheet, v, ox as i64, oy as i64);
            }
            let p = path(name);
            sheet.save(&p).expect("save grid");
            println!(
                "{} seeds {seed}..{} age {age}: {}",
                sp.name,
                seed + 8,
                p.display()
            );
        }
        return;
    }
    if let Some(n) = time {
        // Cost of generating one tree: skeleton + rasterise, `n` seeds from `seed`.
        let t0 = std::time::Instant::now();
        let mut blocks = 0;
        for i in 0..n {
            blocks += rasterize(&skeleton(sp, seed + i, age)).len();
        }
        let per = t0.elapsed().as_secs_f64() * 1e3 / n as f64;
        let t1 = std::time::Instant::now();
        let nodes: usize = (0..n)
            .map(|i| skeleton(sp, seed + i, age).nodes.len())
            .sum();
        let skel = t1.elapsed().as_secs_f64() * 1e3 / n as f64;
        println!(
            "{} age {age}: {per:.3} ms per tree ({n} trees, {} blocks avg)",
            sp.name,
            blocks as u64 / n
        );
        println!(
            "  of which skeleton {skel:.3} ms ({} nodes avg)",
            nodes as u64 / n
        );
        return;
    }
    let sk = skeleton(sp, seed, age);
    let blocks = rasterize(&sk);
    for (view, name) in [
        (View::Front, "front"),
        (View::Side, "side"),
        (View::Top, "top"),
    ] {
        let p = path(name);
        render(&blocks, view, &pal, scale)
            .save(&p)
            .expect("save png");
        println!("{}", p.display());
    }
    let count = |part| blocks.iter().filter(|b| b.part == part).count();
    let ext = |i: usize| {
        let v = blocks.iter().map(|b| b.offset[i]);
        (v.clone().min().unwrap(), v.max().unwrap())
    };
    println!(
        "{} seed {seed} age {age}: {} nodes, {} leaf clusters; blocks: {} log, {} branch, {} leaves; x {:?} y {:?} z {:?}",
        sp.name,
        sk.nodes.len(),
        sk.leaves.len(),
        count(TreePart::Log),
        count(TreePart::Branch),
        count(TreePart::Leaves),
        ext(0),
        ext(1),
        ext(2),
    );
}
