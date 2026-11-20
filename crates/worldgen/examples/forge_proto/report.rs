//! `forge_proto --report [--seeds N]`: forge in-process and print one
//! `key: value` line per number — the evidence later forge work is judged by.
//! A line's key never changes meaning; new numbers are APPENDED in [`report`].

use game::Biome;
use worldgen::forge::climate;
use worldgen::forge::world::EDGE_NAMES;
use worldgen::forge::{forge, ForgeParams, Forged, World};
use worldgen::forged::{ForgedWorld, WorldArtifact};

use crate::Params;

/// Biomes that are water (no share of land).
pub fn is_water(b: Biome) -> bool {
    matches!(b, Biome::Ocean | Biome::FrozenOcean | Biome::Lake)
}

/// Every biome with its share of all cells and, for a land biome, of land
/// cells (what `print_biomes` prints).
pub fn biome_shares_with_land(a: &WorldArtifact) -> Vec<(Biome, f64, Option<f64>)> {
    let land_cells = a.biome.iter().filter(|&&b| !Biome::from_u8(b).is_some_and(is_water)).count().max(1) as f64;
    let total = a.biome.len() as f64;
    worldgen::forge::regions::biome_shares(&a.biome)
        .into_iter()
        .map(|(b, share)| (b, share, (!is_water(b)).then(|| share * total / land_cells)))
        .collect()
}

/// Land components (4-neighbour, `!ocean` cells) above `fraction` of ALL
/// the world's cells.
fn land_components_over(sizes: &[usize], cells: usize, fraction: f64) -> usize {
    sizes.iter().filter(|&&n| n as f64 > fraction * cells as f64).count()
}

/// Cell counts of the land's connected components (4-neighbour; joined
/// across the seam of a world that wraps east-west).
fn land_component_sizes(f: &Forged) -> Vec<usize> {
    let w = &f.world;
    let mut seen = vec![false; w.cells()];
    let mut sizes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w.cells() {
        if seen[start] || w.ocean[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut n = 0;
        while let Some(i) = stack.pop() {
            n += 1;
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                if let Some(j) = w.step(i, dx, dz) {
                    if !seen[j] && !w.ocean[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        sizes.push(n);
    }
    sizes
}

/// Land cells (not ocean) in the first and last rows.
fn polar_land_cells(w: &World) -> usize {
    let last = (w.nz - 1) * w.nx;
    (0..w.nx).filter(|&x| !w.ocean[x]).count() + (0..w.nx).filter(|&x| !w.ocean[last + x]).count()
}

/// Share of land cells whose receiver chain (the analysis' routing) ends at
/// an ocean cell — not at a land-edge outlet, nor in a cell with no receiver
/// or a cycle. Each cell is walked once (memoised), so it is linear.
fn drains_to_sea(f: &Forged) -> f64 {
    const UNKNOWN: u8 = 0;
    const WALKING: u8 = 1;
    const SEA: u8 = 2;
    const NOT_SEA: u8 = 3;
    let (w, receiver) = (&f.world, &f.analysis.routing.receiver);
    let mut state = vec![UNKNOWN; w.cells()];
    let mut path = Vec::new();
    let (mut land, mut to_sea) = (0usize, 0usize);
    for start in 0..w.cells() {
        if w.ocean[start] {
            continue;
        }
        land += 1;
        let mut c = start;
        // Walk down until a cell whose answer is known (or decided here).
        let end = loop {
            if w.ocean[c] {
                break SEA;
            }
            match state[c] {
                SEA | NOT_SEA => break state[c],
                WALKING => break NOT_SEA, // a cycle
                _ => {}
            }
            state[c] = WALKING;
            path.push(c);
            let r = receiver[c];
            if r == worldgen::forge::erosion::NO_RECEIVER {
                break NOT_SEA; // a land-edge outlet or an unrouted cell
            }
            c = r as usize;
        };
        for &p in &path {
            state[p] = end;
        }
        path.clear();
        if state[start] == SEA {
            to_sea += 1;
        }
    }
    to_sea as f64 / land.max(1) as f64
}

/// 99th percentile (nearest rank) of `v`; 0 when empty.
fn p99(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * 0.99).round() as usize]
}

/// The seam's step against every interior column pair's, for a field whose
/// step between cells `a` and `b` is `step(a, b)` and whose statistic over a
/// column pair's rows is `stat`: (seam ÷ the pooled interior's statistic,
/// the share of interior column pairs whose own statistic is at least the
/// seam's — 1.0 = the seam is the smoothest pair, 0.0 = the roughest).
fn seam_steps(nx: usize, nz: usize, step: impl Fn(usize, usize) -> f64, stat: fn(Vec<f64>) -> f64) -> (f64, f64) {
    let pair = |x0: usize, x1: usize| -> Vec<f64> { (0..nz).map(|z| step(z * nx + x0, z * nx + x1)).collect() };
    let seam = stat(pair(nx - 1, 0));
    let pooled = stat((0..nx - 1).flat_map(|x| pair(x, x + 1)).collect());
    let ratio = if pooled > 0.0 { seam / pooled } else if seam > 0.0 { f64::INFINITY } else { 0.0 };
    let rougher = (0..nx - 1).filter(|&x| stat(pair(x, x + 1)) >= seam).count();
    (ratio, rougher as f64 / (nx - 1).max(1) as f64)
}

/// Mean of `v`; 0 when empty.
fn mean(v: Vec<f64>) -> f64 {
    if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
}

/// A seed passes a seam field when its ratio is at most this…
const SEAM_RATIO_OK: f64 = 1.5;
/// …or the seam is not among the roughest this share of column pairs.
const SEAM_RANK_OK: f64 = 0.05;
/// A field passes over a seed set when at most this many seeds fail
/// (`seam_ok_*: no`; an ordinary column pair fails about 5% of the time, so
/// 3 or more of 8 has a chance of about 0.5%)…
const SEAM_PASS_MAX_NOT_OK: usize = 2;
/// …and the median rank is at least this (an ordinary seam's ranks are
/// uniform on 0–1).
const SEAM_PASS_MEDIAN_RANK: f64 = 0.25;

/// Rows of the map `aliased_regions_identical` compares.
const ALIAS_ROWS: i32 = 4;
/// Rows of the map `seam_region_cliff` measures.
const CLIFF_ROWS: i32 = 8;

/// The block registry's materials (the game's, with trees and biome cover).
fn materials() -> worldgen::material::Materials {
    let manifest = include_str!("../../../../assets/blocks/blocks.ron");
    let reg = game::BlockRegistry::from_ron(manifest).expect("valid block manifest");
    game::install_block_props(reg.props());
    worldgen::material::Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")))
}

/// Surface block y of region-local block column `(x, z)` of generated columns.
fn surface(columns: &[(game::ColumnCoords, game::Column)], x: usize, z: usize) -> i32 {
    let cb = game::CHUNK_BLOCKS;
    let (_, column) = columns.iter().find(|(c, _)| c.x == x / cb && c.z == z / cb).expect("a generated column");
    i32::from(column.ground().expect("generated column has ground").surface[(x % cb) * cb + z % cb])
}

/// The world-runtime seam lines (Task 12) of a world that wraps east–west:
/// `aliased_regions_identical`, `seam_region_cliff`, `world_map_wraps`,
/// `rivers_across_seam` (+ its gaps), `trees_across_seam_ok`. `n/a` for a
/// world that does not wrap.
fn runtime_seam(world: &ForgedWorld) -> Vec<(String, String)> {
    let a = world.artifact().expect("whole world");
    let wraps = a.edges[1] == worldgen::forge::world::EdgeKind::Wrap;
    let keys = [
        "aliased_regions_identical",
        "seam_region_cliff",
        "world_map_wraps",
        "rivers_across_seam",
        "rivers_across_seam_max_gap_cells",
        "rivers_across_seam_gaps_cells",
        "trees_across_seam_ok",
    ];
    if !wraps {
        return keys.iter().map(|k| (k.to_string(), "n/a".to_string())).collect();
    }
    let mats = materials();
    let map = worldgen::tile_map::build_world_map(world);
    let (o, w, h) = (map.origin, map.width as i32, map.height as i32);
    let mut rows = Vec::new();
    // A region one lap east is its canonical image, column for column.
    let encode = |rc: game::RegionCoords| bincode::serialize(&worldgen::generate_region_forged(world, rc, mats)).expect("columns serialize");
    let identical = (1..=ALIAS_ROWS).all(|k| {
        let z = o.z + k * h / (ALIAS_ROWS + 1);
        encode(game::RegionCoords::new(o.x + w + 2, z)) == encode(game::RegionCoords::new(o.x + 2, z))
    });
    rows.push((keys[0].to_string(), (if identical { "yes" } else { "no" }).to_string()));
    // The surface step from the last canonical region's east column to the
    // first's west column, over the largest step between neighbouring
    // columns inside either.
    let n = game::REGION_BLOCKS as usize;
    let (mut seam, mut interior) = (0i32, 0i32);
    for k in 1..=CLIFF_ROWS {
        let z = o.z + k * h / (CLIFF_ROWS + 1);
        let east = worldgen::generate_region_forged(world, game::RegionCoords::new(o.x + w - 1, z), mats);
        let west = worldgen::generate_region_forged(world, game::RegionCoords::new(o.x, z), mats);
        for lz in 0..n {
            seam = seam.max((surface(&east, n - 1, lz) - surface(&west, 0, lz)).abs());
            for columns in [&east, &west] {
                for lx in 0..n - 1 {
                    interior = interior.max((surface(columns, lx + 1, lz) - surface(columns, lx, lz)).abs());
                }
            }
        }
    }
    let cliff = if interior > 0 { f64::from(seam) / f64::from(interior) } else if seam > 0 { f64::INFINITY } else { 0.0 };
    rows.push((keys[1].to_string(), format!("{cliff:.3}")));
    let tile_m = game::REGION_BLOCKS as f64;
    let exact = f64::from(map.width) * tile_m == a.width_m;
    rows.push((keys[2].to_string(), (if map.wrap_x && exact { "yes" } else { "no" }).to_string()));
    // Rivers with a stored pair across the seam: along their pairs within a
    // few cells of the seam, sampled every metre, the largest gap between
    // consecutive carved water columns (in raster cells).
    let (width, cell) = (a.width_m, a.cell_m);
    let (min, _) = world.artifact_bounds_engine_m();
    let near_seam = |x: f64| x < 3.0 * cell || x > width - 3.0 * cell;
    let mut gaps: Vec<f64> = Vec::new();
    for reach in &a.rivers {
        let crosses = reach.points.windows(2).any(|q| (f64::from(q[1].x) - f64::from(q[0].x)).abs() > 0.5 * width);
        if !crosses {
            continue;
        }
        let (mut along, mut last_wet, mut gap) = (0.0f64, None::<f64>, 0.0f64);
        for q in reach.points.windows(2) {
            let (x0, z0) = (f64::from(q[0].x), f64::from(q[0].z));
            let (mut x1, z1) = (f64::from(q[1].x), f64::from(q[1].z));
            if x1 - x0 > 0.5 * width {
                x1 -= width;
            } else if x0 - x1 > 0.5 * width {
                x1 += width;
            }
            let len = ((x1 - x0).powi(2) + (z1 - z0).powi(2)).sqrt();
            if near_seam(x0) || near_seam(x1.rem_euclid(width)) {
                let steps = len.ceil().max(1.0) as usize;
                for s in 0..=steps {
                    let t = s as f64 / steps as f64;
                    let (x, z) = (x0 + (x1 - x0) * t, z0 + (z1 - z0) * t);
                    if world.sample(min[0] + x, min[1] + z).water_y.is_some() {
                        let here = along + t * len;
                        if let Some(prev) = last_wet {
                            gap = gap.max(here - prev);
                        }
                        last_wet = Some(here);
                    }
                }
            }
            along += len;
        }
        gaps.push(gap / cell);
    }
    rows.push((keys[3].to_string(), gaps.len().to_string()));
    rows.push((keys[4].to_string(), format!("{:.3}", gaps.iter().copied().fold(0.0, f64::max))));
    let list: Vec<String> = gaps.iter().map(|g| format!("{g:.3}")).collect();
    rows.push((keys[5].to_string(), if list.is_empty() { "none".to_string() } else { list.join(",") }));
    // The trees of the last canonical region of a row and of the first
    // regions of that row and its two neighbours, the latter one lap east:
    // no two trunks closer than the placement's spacing rule allows (the
    // wider crown radius of the two).
    let lap = w * game::REGION_BLOCKS as i32;
    let spaced = (1..=CLIFF_ROWS).all(|k| {
        let z = o.z + k * h / (CLIFF_ROWS + 1);
        let east = worldgen::region_trees_forged(world, game::RegionCoords::new(o.x + w - 1, z), mats);
        (-1..=1).all(|dz| {
            let west = worldgen::region_trees_forged(world, game::RegionCoords::new(o.x, z + dz), mats);
            east.iter().all(|e| {
                west.iter().all(|t| {
                    let r = worldgen::trees::SPECIES[e.species as usize]
                        .crown_radius_blocks
                        .max(worldgen::trees::SPECIES[t.species as usize].crown_radius_blocks);
                    let (dx, dz) = (t.base[0] + lap - e.base[0], t.base[2] - e.base[2]);
                    dx * dx + dz * dz >= r * r
                })
            })
        })
    });
    rows.push((keys[6].to_string(), (if spaced { "yes" } else { "no" }).to_string()));
    rows
}

/// Stored lake bodies with a cell in both column 0 and column `nx - 1`.
fn lakes_across_seam(a: &WorldArtifact) -> usize {
    let nx = a.nx as usize;
    a.lakes
        .iter()
        .filter(|l| {
            let column = |want: usize| {
                l.cells.iter().any(|&(start, len)| (start as usize..(start + len) as usize).any(|i| i % nx == want))
            };
            column(0) && column(nx - 1)
        })
        .count()
}

/// The report's lines for one forged world, in order.
pub fn report(forged: &Forged, p: &ForgeParams) -> Vec<(String, String)> {
    let w = &forged.world;
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut row = |key: &str, value: String| rows.push((key.to_string(), value));
    row("size", format!("{} × {} m at {} m, {} × {}", p.width_m, p.height_m, p.cell_m, w.nx, w.nz));
    let edges: Vec<String> = EDGE_NAMES.iter().zip(w.edges).map(|(name, e)| format!("{name} {e:?}")).collect();
    row("edges", edges.join(", "));
    let bytes: Vec<u8> = w.h.iter().flat_map(|v| v.to_le_bytes()).collect();
    row("height_crc", format!("{:08x}", crc32fast::hash(&bytes)));
    let q = worldgen::forge::quality::quality(forged);
    row("land_share", format!("{:.6}", q.land_share));
    let sizes = land_component_sizes(forged);
    row("land_components_over_1pct", land_components_over(&sizes, w.cells(), 0.01).to_string());
    row("land_components_over_3pct", land_components_over(&sizes, w.cells(), 0.03).to_string());
    row("lakes", format!("{}", q.lakes));
    row("river_mouths", format!("{}", q.river_mouths));
    row("max_peak_m", format!("{:.1}", q.max_peak_m));
    // Every land biome, in `Biome::ALL` order (0 when absent), so each seed
    // prints the same keys.
    let artifact = forged.artifact(p);
    let shares = biome_shares_with_land(&artifact);
    for &b in Biome::ALL.iter().filter(|&&b| !is_water(b)) {
        let land = shares.iter().find(|(s, _, _)| *s == b).and_then(|(_, _, land)| *land).unwrap_or(0.0);
        row(&format!("biome_share.{}", b.name().replace(' ', "_")), format!("{land:.6}"));
    }
    // Task 10 (the Planet): polar seas, climate rows, drainage and the
    // seam.
    let polar = polar_land_cells(w);
    row("polar_rows_all_sea", format!("{} ({polar} land cells)", if polar == 0 { "yes" } else { "no" }));
    let lat = climate::effective_latitudes(w.nx, w.nz, w.dx, p);
    let row_temp = |z: usize| {
        (0..w.nx).map(|x| climate::sea_level_temperature_c(lat[z * w.nx + x] as f64, p)).sum::<f64>() / w.nx as f64
    };
    row("sea_level_temp_c.north", format!("{:.1}", row_temp(0)));
    row("sea_level_temp_c.middle", format!("{:.1}", row_temp(w.nz / 2)));
    row("sea_level_temp_c.south", format!("{:.1}", row_temp(w.nz - 1)));
    row("drains_to_sea", format!("{:.3}", drains_to_sea(forged)));
    // The seam's steps (Task 11 ranks them among the interior column pairs;
    // the ratios are the same p99 numbers `seam_ok_*` judges).
    let (nx, nz) = (w.nx, w.nz);
    let precip: Vec<f64> = forged.climate.precip_mm.iter().map(|&v| v as f64).collect();
    let height = seam_steps(nx, nz, |a, b| (w.h[b] - w.h[a]).abs(), p99);
    let precip_steps = seam_steps(nx, nz, |a, b| (precip[b] - precip[a]).abs(), p99);
    row("seam_step_ratio_height", format!("{:.3}", height.0));
    row("seam_step_ratio_precip", format!("{:.3}", precip_steps.0));
    row("lakes_across_seam", lakes_across_seam(&artifact).to_string());
    // Task 11 (periodic noise and geometry): the biome's seam.
    let biome = seam_steps(nx, nz, |a, b| if artifact.biome[a] != artifact.biome[b] { 1.0 } else { 0.0 }, mean);
    row("seam_step_ratio_biome", format!("{:.3}", biome.0));
    row("seam_step_rank_height", format!("{:.3}", height.1));
    row("seam_step_rank_precip", format!("{:.3}", precip_steps.1));
    row("seam_step_rank_biome", format!("{:.3}", biome.1));
    for (name, (ratio, rank)) in [("height", height), ("precip", precip_steps), ("biome", biome)] {
        let ok = ratio <= SEAM_RATIO_OK || rank >= SEAM_RANK_OK;
        row(&format!("seam_ok_{name}"), (if ok { "yes" } else { "no" }).to_string());
    }
    // Task 12 (the runtime sampler wraps): regions, the world map and rivers
    // at the seam, read through the game's sampler.
    for (key, value) in runtime_seam(&ForgedWorld::new(artifact)) {
        row(&key, value);
    }
    rows
}

/// Keys whose values are never numbers, however they read (a crc can be all
/// digits).
const NOT_NUMBERS: &[&str] = &["seed", "height_crc"];

/// The `--report` run: each seed in turn (`seed`, `seed + 1`, …), then the
/// summary (min / median / max of every numeric key; `NOT_NUMBERS` and any
/// key with a non-numeric value: the value, or how many distinct ones).
pub fn run(base: &Params) {
    let seeds = base.seeds.max(1);
    let mut all: Vec<Vec<(String, String)>> = Vec::new();
    for k in 0..seeds {
        let p = ForgeParams { seed: base.seed.wrapping_add(k as u64), quiet: true, ..base.forge.clone() };
        let forged = forge(&p);
        let rows = report(&forged, &p);
        println!("seed: {:#018X}", p.seed);
        for (key, value) in &rows {
            println!("{key}: {value}");
        }
        if base.out_given {
            let path = if seeds == 1 {
                format!("{}.world", base.out.to_string_lossy())
            } else {
                format!("{}-{:016X}.world", base.out.to_string_lossy(), p.seed)
            };
            forged.artifact(&p).save(std::path::Path::new(&path)).expect("write world file");
            println!("world_file: {path}");
        }
        println!();
        all.push(rows);
    }
    println!("summary ({seeds} seed(s)):");
    let mut keys: Vec<&str> = Vec::new();
    for rows in &all {
        for (key, _) in rows {
            if !keys.contains(&key.as_str()) {
                keys.push(key);
            }
        }
    }
    for key in keys {
        let values: Vec<&str> =
            all.iter().flat_map(|rows| rows.iter().filter(|(k, _)| k == key).map(|(_, v)| v.as_str())).collect();
        let mut numbers: Vec<f64> = values.iter().filter_map(|v| v.parse::<f64>().ok()).collect();
        if !NOT_NUMBERS.contains(&key) && numbers.len() == values.len() && !numbers.is_empty() {
            numbers.sort_by(f64::total_cmp);
            let n = numbers.len();
            let median = if n % 2 == 1 { numbers[n / 2] } else { (numbers[n / 2 - 1] + numbers[n / 2]) / 2.0 };
            println!("{key}: min {} median {median} max {}", numbers[0], numbers[n - 1]);
        } else {
            let mut distinct = values.clone();
            distinct.sort();
            distinct.dedup();
            if distinct.len() == 1 {
                println!("{key}: {}", distinct[0]);
            } else {
                println!("{key}: {} distinct values", distinct.len());
            }
        }
    }
    // The seam rule over the seed set, per field: at most
    // `SEAM_PASS_MAX_NOT_OK` seeds fail and the median rank is at least
    // `SEAM_PASS_MEDIAN_RANK` (one seed: that seed's own rule).
    let values = |key: &str| -> Vec<String> {
        all.iter().flat_map(|rows| rows.iter().filter(|(k, _)| k == key).map(|(_, v)| v.clone())).collect()
    };
    for field in ["height", "precip", "biome"] {
        let oks = values(&format!("seam_ok_{field}"));
        if oks.is_empty() {
            continue;
        }
        let not_ok = oks.iter().filter(|v| *v == "no").count();
        let mut ranks: Vec<f64> = values(&format!("seam_step_rank_{field}")).iter().filter_map(|v| v.parse().ok()).collect();
        ranks.sort_by(f64::total_cmp);
        let n = ranks.len();
        let median = if n == 0 { 0.0 } else if n % 2 == 1 { ranks[n / 2] } else { (ranks[n / 2 - 1] + ranks[n / 2]) / 2.0 };
        let pass = if seeds == 1 { not_ok == 0 } else { not_ok <= SEAM_PASS_MAX_NOT_OK && median >= SEAM_PASS_MEDIAN_RANK };
        println!("seam_seeds_not_ok_{field}: {not_ok}");
        println!("seam_pass_{field}: {}", if pass { "yes" } else { "no" });
    }
}
