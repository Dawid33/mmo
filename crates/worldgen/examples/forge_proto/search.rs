//! Generate-and-test, the Dwarf Fortress way: forge many candidate worlds at a
//! coarse resolution, score each against what a playable world needs, keep the
//! best. Affordable because drainage structure is resolution-stable (F0: river
//! mouths, longest river and biggest basin agree across 32/16/8 m cells), so a
//! 5-second 32 m world predicts the 16 m or 8 m one forged from the same seed.

use worldgen::forge::hydrology::Stats;
use worldgen::forge::world::smoothstep;
use worldgen::forge::{forge, ForgeParams};
use crate::Params;
use crate::render;

/// Cell size used for candidates.
const SEARCH_CELL_M: f64 = 32.0;
/// Share of the logical cores the search may use (the repo's 80% rule).
const CORE_SHARE: f64 = 0.8;
/// How many of the best candidates get a thumbnail.
const THUMBNAILS: usize = 3;

/// 0..1 for each thing a world should have; the score is their product, with
/// a floor per term so one weak point lowers a world instead of zeroing it.
pub fn criteria(s: &Stats, cells: usize) -> Vec<(&'static str, f64)> {
    let rocks = s.rock_shares.iter().filter(|&&share| share >= 0.04).count() as f64;
    use worldgen::forge::knobs::{LAYOUT_CONTINENT, LAYOUT_CONTINENTS, LAYOUT_INLAND, LAYOUT_ONE_ISLAND};
    let one_island = s.layout == LAYOUT_ONE_ISLAND;
    let continental = [LAYOUT_CONTINENT, LAYOUT_CONTINENTS, LAYOUT_INLAND].contains(&s.layout);
    // Several islands share the sea between them: less land is right, and an
    // island a few km across cannot hold a 9 km river or an 18 km^2 basin.
    // A continent is mostly land, running off the map on its land edges.
    let (land, peaks, river, basin, lakes, buildable) = if continental {
        ((0.40, 0.55), (140.0, 200.0), (5.0, 8.5), (8.0, 18.0), (2.0, 6.0), (15.0, 35.0))
    } else if one_island {
        ((0.22, 0.32), (140.0, 200.0), (5.0, 8.5), (8.0, 18.0), (2.0, 6.0), (15.0, 35.0))
    } else {
        ((0.10, 0.18), (100.0, 160.0), (2.5, 5.0), (2.0, 6.0), (1.0, 4.0), (8.0, 20.0))
    };
    let too_much_land = if continental { 1.0 } else { 1.0 - smoothstep(0.48, 0.58, s.land_share) };
    let mut criteria = vec![
        // Enough land to live on, enough sea that the rim stays far away.
        ("land", smoothstep(land.0, land.1, s.land_share) * too_much_land),
        // Real mountains. No upper bound: the forge fits overshoot to the
        // engine's height band (post::fit_height_band), and a 32 m candidate
        // under-predicts the 16 m peak by up to ~30% anyway.
        ("peaks", smoothstep(peaks.0, peaks.1, s.heights[4])),
        ("long river", smoothstep(river.0, river.1, s.longest_river_km)),
        ("big basin", smoothstep(basin.0, basin.1, s.biggest_basin_km2)),
        ("lakes", smoothstep(lakes.0, lakes.1, s.lake_count as f64)),
        ("buildable", smoothstep(buildable.0, buildable.1, s.buildable_km2)),
        ("rock variety", smoothstep(2.0, 5.0, rocks)),
        // The island must not lean on the forced ocean margin.
        ("clear of rim", 1.0 - smoothstep(0.002, 0.02, s.margin_drowned as f64 / cells as f64)),
    ];
    if !one_island && s.layout != LAYOUT_CONTINENT && s.layout != LAYOUT_INLAND {
        // An archipelago that merged into one landmass is not one.
        criteria.push(("islands", smoothstep(1.5, 3.5, s.islands as f64)));
    }
    criteria
}

pub fn score(s: &Stats, cells: usize) -> f64 {
    criteria(s, cells).iter().map(|(_, v)| 0.15 + 0.85 * v).product::<f64>()
}

pub fn run(base: &Params) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = ((cores as f64 * CORE_SHARE) as usize).clamp(1, base.search);
    println!(
        "searching {} seeds from {:#X} at {SEARCH_CELL_M} m cells, {} iterations, {threads} threads",
        base.search, base.seed, base.knobs.iterations()
    );
    let started = std::time::Instant::now();
    let seeds: Vec<u64> = (0..base.search as u64).map(|k| base.seed.wrapping_add(k)).collect();
    let mut results: Vec<(f64, u64, Stats, usize)> = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let seeds = &seeds;
                scope.spawn(move || {
                    let mut out = Vec::new();
                    for &seed in seeds.iter().skip(t).step_by(threads) {
                        let p = ForgeParams { seed, cell_m: SEARCH_CELL_M, quiet: true, ..base.forge.clone() };
                        let forged = forge(&p);
                        let cells = forged.world.n * forged.world.n;
                        out.push((score(&forged.analysis.stats, cells), seed, forged.analysis.stats, cells));
                    }
                    out
                })
            })
            .collect();
        for h in handles {
            results.extend(h.join().expect("search thread"));
        }
    });
    results.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("{} worlds in {:.1?}", results.len(), started.elapsed());

    println!(
        "{:>5}  {:<18} {:>5} {:>5} {:>6} {:>6} {:>5} {:>6} {:>5}  weakest criterion",
        "score", "seed", "land%", "peak", "river", "basin", "lakes", "build", "rocks"
    );
    for (sc, seed, s, cells) in results.iter().take(12) {
        let weakest = criteria(s, *cells).into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        println!(
            "{:5.3}  {:<#18X} {:5.1} {:5.0} {:6.1} {:6.1} {:5} {:6.1} {:5}  {} ({:.2})",
            sc,
            seed,
            100.0 * s.land_share,
            s.heights[4],
            s.longest_river_km,
            s.biggest_basin_km2,
            s.lake_count,
            s.buildable_km2,
            s.rock_shares.iter().filter(|&&share| share >= 0.04).count(),
            weakest.0,
            weakest.1
        );
    }
    let stem = base.out.to_string_lossy().to_string();
    for (rank, (_, seed, ..)) in results.iter().take(THUMBNAILS).enumerate() {
        let p = ForgeParams { seed: *seed, cell_m: SEARCH_CELL_M, quiet: true, ..base.forge.clone() };
        let forged = forge(&p);
        let path = format!("{stem}-search-{}-{seed:X}.png", rank + 1);
        render::layer(&forged, worldgen::forge::render::PreviewLayer::Rivers, &path);
    }
    if let Some((_, seed, ..)) = results.first() {
        println!("forge the winner at full resolution: just forge-proto --seed {seed:X}");
    }
}
