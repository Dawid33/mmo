//! Headless probe: how long does a region take to start on a real server
//! region thread? Spawns regions through `ThreadRegionSpawner` with production
//! worldgen columns — the path `server::run` takes on a client's first
//! subscription (deferred seeds: each region thread runs its own worldgen) —
//! and times each until its first tick output.
//!
//!   cargo run --release -p server --example genesis_probe -- [regions=1]
//!
//! `regions=9` starts a 3x3 window at once, like a production-scale join.

use std::time::Instant;

use game::{BlockId, RegionCoords, RegionSeed, RegionSpawner};

fn main() {
    game::set_hash_mode_from_env();
    let count: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(1);
    let materials = worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
    };
    let regions: Vec<RegionCoords> = (-1..=1)
        .flat_map(|x| (-1..=1).map(move |z| RegionCoords::new(x, z)))
        .take(count.max(1))
        .collect();
    let (out_send, out_recv) = crossbeam::channel::unbounded();
    // Production path: region threads generate their own columns.
    let generator: server::region_threads::SharedRegionGenerator =
        std::sync::Arc::new(move |rc| worldgen::generate_region(rc, worldgen::WORLD_SEED, materials));
    let mut spawner = server::region_threads::ThreadRegionSpawner::with_generator(generator);
    let started = Instant::now();
    let mut inputs = Vec::new();
    for &rc in &regions {
        inputs.push(spawner.spawn(rc, RegionSeed::Deferred(None), out_send.clone()));
    }
    println!("spawned {} region threads in {:.1} ms", regions.len(), started.elapsed().as_secs_f64() * 1000.0);
    let mut pending: std::collections::BTreeSet<RegionCoords> = regions.iter().copied().collect();
    while !pending.is_empty() {
        let (rc, _output) = out_recv.recv().expect("region thread died");
        if pending.remove(&rc) {
            println!("first output from {rc:?} after {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
        }
    }
    drop(inputs);
}
