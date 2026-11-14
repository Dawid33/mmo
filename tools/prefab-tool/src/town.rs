//! `prefab-tool town`: kit, layout and a top-down picture of the town.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use game::prefab::{column_top_world, load_prefab_dirs, BUILTIN_PREFAB_DIR};
use game::registry::BlockRegistry;
use worldgen::material::Materials;
use worldgen::sites::kit::town_kit;
use worldgen::sites::town::{layout_town, TownLayout, TownParams};
use worldgen::structures::{paint_at, Paint};

use crate::render::{write_png, BlockColours, Image};
use crate::{load_registry, Failure, BLOCKS_DIR, USAGE};

const FLAT_GROUND: i32 = 70;
const MAX_WIDTH: u32 = 2048;
const MARGIN: i32 = 8;
const IMPORTS_DIR: &str = "prefab-imports";
const BACKGROUND_LOW: [u8; 3] = [64, 84, 58];
const BACKGROUND_HIGH: [u8; 3] = [124, 146, 104];
const WATER: [u8; 3] = [40, 80, 160];
const OUTLINE: [u8; 3] = [20, 20, 24];
const FRONT_MARK: [u8; 3] = [255, 0, 255];
/// A house's lowest top keeps this share of its colour, its highest all of it.
const LOW_SHADE: f32 = 0.6;
/// The background samples the ground once per cell of this many blocks per side.
const GROUND_CELL: i32 = 4;
/// How many square rings (of `ORIGIN_SCAN_STEP` blocks) around (0, 0) to look for a dry origin under
/// `--terrain seed`.
const ORIGIN_SCAN_RINGS: i32 = 64;
const ORIGIN_SCAN_STEP: i32 = 16;
/// Streets and street length as the server takes them (`server::dev_structures::TOWN_ARMS` /
/// `TOWN_ARM_LEN`).
const ARMS: std::ops::RangeInclusive<u8> = 1..=4;
const ARM_LEN: std::ops::RangeInclusive<i32> = 16..=256;
/// The longest picture side, in pixels.
const MAX_PICTURE_SIDE: u64 = 8192;

struct TownArgs {
    style: String,
    dirs: Option<Vec<PathBuf>>,
    seed: u64,
    arms: Option<u8>,
    len: Option<i32>,
    seed_terrain: bool,
    out: Option<PathBuf>,
    scale: Option<u32>,
}

fn parse(args: &[String]) -> Result<TownArgs, Failure> {
    let mut a = TownArgs {
        style: String::new(),
        dirs: None,
        seed: 0,
        arms: None,
        len: None,
        seed_terrain: false,
        out: None,
        scale: None,
    };
    let mut style = None;
    let mut it = args.iter();
    let bad = |m: String| -> Failure { (2, m) };
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| bad(format!("{flag} needs a value")));
        match arg.as_str() {
            "-o" => a.out = Some(PathBuf::from(value("-o")?)),
            "--dirs" => {
                a.dirs = Some(value("--dirs")?.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect())
            }
            "--seed" => {
                let v = value("--seed")?;
                a.seed = v.parse().map_err(|_| bad(format!("--seed: \"{v}\" is not a whole number")))?;
            }
            "--arms" => {
                let v = value("--arms")?;
                match v.parse::<u8>() {
                    Ok(n) if ARMS.contains(&n) => a.arms = Some(n),
                    _ => return Err(bad(format!("--arms: \"{v}\" is not 1 to 4"))),
                }
            }
            "--len" => {
                let v = value("--len")?;
                match v.parse::<i32>() {
                    Ok(n) if ARM_LEN.contains(&n) => a.len = Some(n),
                    _ => return Err(bad(format!("--len: \"{v}\" is not 16 to 256 blocks"))),
                }
            }
            "--terrain" => match value("--terrain")?.as_str() {
                "flat" => a.seed_terrain = false,
                "seed" => a.seed_terrain = true,
                v => return Err(bad(format!("--terrain: \"{v}\" is not flat or seed"))),
            },
            "--scale" => {
                let v = value("--scale")?;
                match v.parse::<u32>() {
                    Ok(n) if n >= 1 => a.scale = Some(n),
                    _ => return Err(bad(format!("--scale: \"{v}\" is not a whole number of at least 1"))),
                }
            }
            flag if flag.starts_with('-') => return Err(bad(format!("unknown flag {flag}\n{USAGE}"))),
            _ if style.is_none() => style = Some(arg.clone()),
            _ => return Err(bad(USAGE.to_string())),
        }
    }
    a.style = style.ok_or_else(|| bad(USAGE.to_string()))?;
    Ok(a)
}

/// Every sub-directory of `prefab-imports/`, sorted; none when it does not exist.
fn default_dirs() -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(IMPORTS_DIR) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs
}

fn block(registry: &BlockRegistry, name: &str) -> Result<game::BlockId, Failure> {
    registry.id_of(name).ok_or((2, format!("block \"{name}\" is not in {BLOCKS_DIR}/blocks.ron")))
}

/// Ground and water of the chosen terrain.
struct Terrain {
    mats: Option<Materials>,
    seed: u64,
}

impl Terrain {
    fn ground(&self, x: i32, z: i32) -> i32 {
        match &self.mats {
            Some(m) => worldgen::ground_y(x, z, self.seed, *m),
            None => FLAT_GROUND,
        }
    }
    fn wet(&self, x: i32, z: i32) -> bool {
        self.mats.is_some_and(|m| worldgen::is_wet(x, z, self.seed, m))
    }
}

pub fn run(args: &[String]) -> Result<(), Failure> {
    let a = parse(args)?;
    let registry = load_registry()?;
    let mut dirs: Vec<PathBuf> = vec![PathBuf::from(BUILTIN_PREFAB_DIR)];
    dirs.extend(a.dirs.clone().unwrap_or_else(default_dirs));
    for d in &dirs {
        if !d.is_dir() {
            return Err((2, format!("{}: not a directory", d.display())));
        }
    }
    let dir_refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    let set = load_prefab_dirs(&dir_refs, &|n| registry.id_of(n)).map_err(|e| (1, e))?;
    let kit = town_kit(&set, &a.style).map_err(|e| (1, e))?;

    let road = block(&registry, "cobbled_road")?;
    let plaza_block = block(&registry, "smooth_stone")?;
    let mut params = TownParams::small(a.seed, road, plaza_block);
    if let Some(n) = a.arms {
        params.arms = n;
    }
    if let Some(n) = a.len {
        params.arm_len = n;
    }

    let terrain = Terrain {
        mats: a.seed_terrain.then(|| {
            Materials::from_names(|n| registry.id_of(n).unwrap_or_else(|| panic!("block manifest must define \"{n}\"")))
        }),
        seed: a.seed,
    };
    let origin = if a.seed_terrain {
        dry_origin(&terrain, &params).ok_or((
            1,
            format!(
                "no dry site within {} blocks of (0, 0): the origin and every street's middle must be dry",
                ORIGIN_SCAN_RINGS * ORIGIN_SCAN_STEP
            ),
        ))?
    } else {
        [0, 0]
    };

    let layout = layout_town(&kit, origin, &params, &|x, z| terrain.ground(x, z), &|x, z| terrain.wet(x, z));
    let extent = extent_of(&layout, origin, &params);
    let (w, d) = ((extent[2] - extent[0]) as u32, (extent[3] - extent[1]) as u32);
    let scale = a.scale.unwrap_or_else(|| (MAX_WIDTH / w.max(1)).max(1));
    let (pw, ph) = (u64::from(w) * u64::from(scale), u64::from(d) * u64::from(scale));
    if pw > MAX_PICTURE_SIDE || ph > MAX_PICTURE_SIDE {
        return Err((
            2,
            format!("the picture would be {pw}x{ph} px, over {MAX_PICTURE_SIDE} px a side: lower --scale (now {scale})"),
        ));
    }

    let colours = BlockColours::load(&registry, Path::new(BLOCKS_DIR));
    let image = draw(&layout, &colours, &terrain, extent, scale);
    let out = a.out.unwrap_or_else(|| Path::new("target/prefab-view").join(format!("town-{}-{}.png", a.style, a.seed)));
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| (2, format!("{}: {e}", dir.display())))?;
    }
    write_png(&out, &image).map_err(|e| (2, format!("{}: {e}", out.display())))?;

    println!("{}", out.display());
    println!("style {}", a.style);
    println!("kit {} houses", kit.houses.len());
    println!("lots {} laid, {} skipped", layout.lots.len(), layout.skipped);
    println!("extent {w}x{d} blocks from ({}, {}), origin ({}, {}), scale {scale}", extent[0], extent[1], origin[0], origin[1]);
    for lot in &layout.lots {
        let (min, max) = lot.bounds;
        let level = layout
            .placements
            .iter()
            .find(|p| p.bounds() == lot.bounds)
            .and_then(|p| p.pad)
            .map_or(0, |p| p.level);
        println!(
            "{}  {}  {}  {}x{}  {level}",
            lot.name,
            lot.arm,
            if lot.left { "left" } else { "right" },
            max[0] - min[0],
            max[2] - min[2]
        );
    }
    Ok(())
}

/// Under `--terrain seed`: the first column, scanning square rings outward from (0, 0) in steps of
/// `ORIGIN_SCAN_STEP` (each ring from its north-west corner, row by row), where the origin and the middle of
/// every street the town has are dry. The cheap origin test first.
fn dry_origin(terrain: &Terrain, p: &TownParams) -> Option<[i32; 2]> {
    let mid = p.plaza / 2 + 1 + p.arm_len / 2;
    let dirs = [[1, 0], [-1, 0], [0, 1], [0, -1]];
    let dry_site = |[x, z]: [i32; 2]| {
        !terrain.wet(x, z) && dirs[..usize::from(p.arms.min(4))].iter().all(|[dx, dz]| !terrain.wet(x + dx * mid, z + dz * mid))
    };
    (0..=ORIGIN_SCAN_RINGS)
        .flat_map(|r| {
            (-r..=r).flat_map(move |j| (-r..=r).map(move |i| (i, j))).filter(move |&(i, j)| i.abs().max(j.abs()) == r)
        })
        .map(|(i, j)| [i * ORIGIN_SCAN_STEP, j * ORIGIN_SCAN_STEP])
        .find(|&c| dry_site(c))
}

/// The bounding box [x0, z0, x1, z1] (max exclusive) of the plaza, every arm's full street strip and
/// every placement's reach, plus a margin.
fn extent_of(layout: &TownLayout, origin: [i32; 2], p: &TownParams) -> [i32; 4] {
    let [ox, oz] = origin;
    let (h, s) = (p.plaza / 2, p.street_w / 2);
    let end = h + 1 + p.arm_len;
    let mut e = [ox - h, oz - h, ox + h + 1, oz + h + 1];
    let mut grow = |r: [i32; 4]| {
        e = [e[0].min(r[0]), e[1].min(r[1]), e[2].max(r[2]), e[3].max(r[3])];
    };
    for arm in 0..p.arms.min(4) {
        grow(match arm {
            0 => [ox, oz - s, ox + end, oz + s + 1],
            1 => [ox - end + 1, oz - s, ox + 1, oz + s + 1],
            2 => [ox - s, oz, ox + s + 1, oz + end],
            _ => [ox - s, oz - end + 1, ox + s + 1, oz + 1],
        });
    }
    for pl in &layout.placements {
        grow(pl.reach_xz());
    }
    [e[0] - MARGIN, e[1] - MARGIN, e[2] + MARGIN, e[3] + MARGIN]
}

fn shade(c: [u8; 3], f: f32) -> [u8; 3] {
    c.map(|v| (v as f32 * f).round().min(255.0) as u8)
}

fn lerp(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

fn draw(layout: &TownLayout, colours: &BlockColours, terrain: &Terrain, extent: [i32; 4], scale: u32) -> Image {
    let [x0, z0, x1, z1] = extent;
    let (w, d) = ((x1 - x0) as u32, (z1 - z0) as u32);
    let mut img = Image { width: w * scale, height: d * scale, rgb: vec![0; (w * scale * d * scale * 3) as usize] };

    // Background: `ground_y` builds a terrain patch per call, far too slow per pixel, so the ground is
    // sampled once per GROUND_CELL x GROUND_CELL block cell (at its centre) and reused for the cell.
    let cells_x = (x1 - x0 + GROUND_CELL - 1) / GROUND_CELL;
    let cells_z = (z1 - z0 + GROUND_CELL - 1) / GROUND_CELL;
    let mut samples: BTreeMap<(i32, i32), (i32, bool)> = BTreeMap::new();
    for cz in 0..cells_z {
        for cx in 0..cells_x {
            let (sx, sz) = (x0 + cx * GROUND_CELL + GROUND_CELL / 2, z0 + cz * GROUND_CELL + GROUND_CELL / 2);
            samples.insert((cx, cz), (terrain.ground(sx, sz), terrain.wet(sx, sz)));
        }
    }
    let lo = samples.values().map(|s| s.0).min().unwrap_or(0);
    let hi = samples.values().map(|s| s.0).max().unwrap_or(0);
    let px = |img: &mut Image, bx: i32, bz: i32, c: [u8; 3]| {
        img.fill((bx - x0) as u32 * scale, (bz - z0) as u32 * scale, scale, scale, c)
    };
    for bz in z0..z1 {
        for bx in x0..x1 {
            let (g, wet) = samples[&((bx - x0) / GROUND_CELL, (bz - z0) / GROUND_CELL)];
            let c = if wet {
                WATER
            } else {
                lerp(BACKGROUND_LOW, BACKGROUND_HIGH, if hi > lo { (g - lo) as f32 / (hi - lo) as f32 } else { 0.5 })
            };
            px(&mut img, bx, bz, c);
        }
    }
    // Paints, in the block's colour (the first paint in slice order wins, as in generation).
    let paints: &[Paint] = &layout.paints;
    for bz in z0..z1 {
        for bx in x0..x1 {
            if let Some(id) = paint_at(paints, bx, bz) {
                px(&mut img, bx, bz, colours.of(id));
            }
        }
    }
    // Houses by their own plan, shaded by height within the house.
    let first_lot = layout.placements.len() - layout.lots.len();
    for pl in &layout.placements {
        let (min, max) = pl.bounds();
        let tops: Vec<(i32, i32, i32, game::BlockId)> = (min[2]..max[2])
            .flat_map(|z| (min[0]..max[0]).map(move |x| (x, z)))
            .filter_map(|(x, z)| column_top_world(&pl.prefab, &pl.at, x, z).map(|(y, id)| (x, z, y, id)))
            .collect();
        let lo = tops.iter().map(|t| t.2).min().unwrap_or(0);
        let hi = tops.iter().map(|t| t.2).max().unwrap_or(0);
        for &(x, z, y, id) in &tops {
            let f = if hi > lo { LOW_SHADE + (1.0 - LOW_SHADE) * (y - lo) as f32 / (hi - lo) as f32 } else { 1.0 };
            px(&mut img, x, z, shade(colours.of(id), f));
        }
    }
    // Outlines and front marks last, so no house paints over a neighbour's edge.
    for (i, pl) in layout.placements.iter().enumerate() {
        let (min, max) = pl.bounds();
        let (ix, iz) = ((min[0] - x0) as u32 * scale, (min[2] - z0) as u32 * scale);
        let (iw, ih) = ((max[0] - min[0]) as u32 * scale, (max[2] - min[2]) as u32 * scale);
        img.fill(ix, iz, iw, 1, OUTLINE);
        img.fill(ix, iz + ih - 1, iw, 1, OUTLINE);
        img.fill(ix, iz, 1, ih, OUTLINE);
        img.fill(ix + iw - 1, iz, 1, ih, OUTLINE);
        if i >= first_lot {
            let faces = layout.lots[i - first_lot].faces;
            let (mx, mz) = ((min[0] + max[0] - 1) / 2, (min[2] + max[2] - 1) / 2);
            // The middle block of the front face's outermost row.
            let (bx, bz) = match faces {
                game::prefab::Facing::North => (mx, min[2]),
                game::prefab::Facing::South => (mx, max[2] - 1),
                game::prefab::Facing::West => (min[0], mz),
                game::prefab::Facing::East => (max[0] - 1, mz),
            };
            px(&mut img, bx, bz, FRONT_MARK);
        }
    }
    img
}
