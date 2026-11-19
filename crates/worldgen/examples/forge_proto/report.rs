//! `forge_proto --report [--seeds N]`: forge in-process and print one
//! `key: value` line per number — the evidence later forge work is judged by.
//! A line's key never changes meaning; new numbers are APPENDED in [`report`].

use game::Biome;
use worldgen::forge::knobs::CHOICES;
use worldgen::forge::world::EDGE_NAMES;
use worldgen::forge::{forge, ForgeParams, Forged};
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

/// Cell counts of the land's connected components (4-neighbour).
fn land_component_sizes(f: &Forged) -> Vec<usize> {
    let w = &f.world;
    let (nx, nz) = (w.nx, w.nz);
    let mut seen = vec![false; nx * nz];
    let mut sizes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..nx * nz {
        if seen[start] || w.ocean[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut n = 0;
        while let Some(i) = stack.pop() {
            n += 1;
            let (x, z) = (i % nx, i / nx);
            let mut visit = |j: usize| {
                if !seen[j] && !w.ocean[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 { visit(i - 1); }
            if x + 1 < nx { visit(i + 1); }
            if z > 0 { visit(i - nx); }
            if z + 1 < nz { visit(i + nx); }
        }
        sizes.push(n);
    }
    sizes
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
    let shares = biome_shares_with_land(&forged.artifact(p));
    for &b in Biome::ALL.iter().filter(|&&b| !is_water(b)) {
        let land = shares.iter().find(|(s, _, _)| *s == b).and_then(|(_, _, land)| *land).unwrap_or(0.0);
        row(&format!("biome_share.{}", b.name().replace(' ', "_")), format!("{land:.6}"));
    }
    rows
}

/// The `--report` run: each seed in turn (`seed`, `seed + 1`, …), then the
/// summary (min / median / max of every numeric key).
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
        if numbers.len() == values.len() && !numbers.is_empty() {
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
