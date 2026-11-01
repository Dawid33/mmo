//! Generate-and-test, the Dwarf Fortress way: forge many candidate worlds at a
//! coarse resolution, score each against what a playable world needs, keep the
//! best. Affordable because drainage structure is resolution-stable (F0: river
//! mouths, longest river and biggest basin agree across 32/16/8 m cells), so a
//! 5-second 32 m world predicts the 16 m or 8 m one forged from the same seed.
//! The criteria live in the lib (`forge::quality::world_score`).

use worldgen::forge::hydrology::Stats;
use worldgen::forge::quality::{quality, summarise, world_score};
use worldgen::forge::{forge, Forged, ForgeParams};
use worldgen::forged::WorldArtifact;
use crate::Params;
use crate::render;

/// Cell size used for candidates.
const SEARCH_CELL_M: f64 = 32.0;
/// Share of the logical cores the search may use (the repo's 80% rule).
const CORE_SHARE: f64 = 0.8;
/// How many of the best candidates get a thumbnail.
const THUMBNAILS: usize = 3;

/// `quality::world_score` of a forged world and its world file: the DF-style
/// criteria (spec section 10), in the terms' own order.
pub fn score(f: &Forged, artifact: &WorldArtifact, p: &ForgeParams) -> (f64, Vec<(&'static str, f64)>) {
    let polar = p.knobs.pole_choice() != worldgen::forge::climate::POLE_NONE;
    world_score(&quality(f), &summarise(artifact, polar))
}

pub fn run(base: &Params) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let threads = ((cores as f64 * CORE_SHARE) as usize).clamp(1, base.search);
    println!(
        "searching {} seeds from {:#X} at {SEARCH_CELL_M} m cells, {} iterations, {threads} threads",
        base.search, base.seed,
        worldgen::forge::schedule(&ForgeParams { cell_m: SEARCH_CELL_M, ..base.forge.clone() }).iter().map(|&(_, i)| i).sum::<usize>()
    );
    let started = std::time::Instant::now();
    let seeds: Vec<u64> = (0..base.search as u64).map(|k| base.seed.wrapping_add(k)).collect();
    let mut results: Vec<(f64, u64, Stats, Vec<(&'static str, f64)>)> = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let seeds = &seeds;
                scope.spawn(move || {
                    let mut out = Vec::new();
                    for &seed in seeds.iter().skip(t).step_by(threads) {
                        let p = ForgeParams { seed, cell_m: SEARCH_CELL_M, quiet: true, ..base.forge.clone() };
                        let forged = forge(&p);
                        let artifact = forged.artifact(&p);
                        let (sc, terms) = score(&forged, &artifact, &p);
                        out.push((sc, seed, forged.analysis.stats, terms));
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
    for (sc, seed, s, terms) in results.iter().take(12) {
        let weakest = terms.iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
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
