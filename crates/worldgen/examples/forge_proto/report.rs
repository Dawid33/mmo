//! `forge_proto --report [--seeds N]`: forge in-process and print one
//! `key: value` line per number — the evidence later forge work is judged by.
//! A line's key never changes meaning; new numbers are APPENDED in [`report`].

use game::Biome;
use worldgen::forge::climate;
use worldgen::forge::knobs::CHOICES;
use worldgen::forge::world::EDGE_NAMES;
use worldgen::forge::{forge, ForgeParams, Forged, World};
use worldgen::forged::WorldArtifact;

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

/// The seam's step against the interior's: the 99th percentile over rows of
/// `|v(nx - 1, z) - v(0, z)|` over the 99th percentile of every interior
/// column-to-column `|v(x + 1, z) - v(x, z)|`.
fn seam_step_ratio(v: &[f64], nx: usize, nz: usize) -> f64 {
    let seam = p99((0..nz).map(|z| (v[z * nx + nx - 1] - v[z * nx]).abs()).collect());
    let interior = p99((0..nz).flat_map(|z| (0..nx - 1).map(move |x| (v[z * nx + x + 1] - v[z * nx + x]).abs())).collect());
    if interior > 0.0 { seam / interior } else { f64::INFINITY }
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
    let layouts = CHOICES.iter().find(|(key, _)| *key == "layout").map(|(_, names)| *names).unwrap_or(&[]);
    let layout = p.knobs.layout();
    row("layout", format!("{layout} ({})", layouts.get(layout as usize).copied().unwrap_or("?")));
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
    // Task 10 (the planet layout): polar seas, climate rows, drainage and the
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
    row("seam_step_ratio_height", format!("{:.3}", seam_step_ratio(&w.h, w.nx, w.nz)));
    let precip: Vec<f64> = forged.climate.precip_mm.iter().map(|&v| v as f64).collect();
    row("seam_step_ratio_precip", format!("{:.3}", seam_step_ratio(&precip, w.nx, w.nz)));
    row("lakes_across_seam", lakes_across_seam(&artifact).to_string());
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
}
