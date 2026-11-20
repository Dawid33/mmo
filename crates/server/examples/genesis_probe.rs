//! Headless probe: how long does a region take to start on a real server
//! region thread, and how much memory does each running region cost? Spawns
//! regions through `ThreadRegionSpawner` with production worldgen columns —
//! the path `server::run` takes on a client's first subscription (deferred
//! seeds: each region thread runs its own worldgen) — times each until its
//! first output and prints the process RSS as they come up, then keeps them
//! ticking for `hold` seconds, sampling RSS.
//!
//!   just genesis-probe [regions=1] [hold_secs=0] [center_x,center_z] [people=0]
//!   (`just world_file=<file> genesis-probe …` for a forged world)
//!
//! `people=N` spawns N people (`SpawnPerson` on a 4-block column grid) in the center region
//! once it is up, so the hold measures resident cost on real terrain: the region thread's
//! `[region …] avg= max= budget= lag_ticks=` line every 5 s (the server's own tick metrics).
//!
//! `regions=9` starts a 3x3 window at once, like a production-scale join (or
//! the fortress neighbourhood). `VOXEL_WORLD_FILE=<file>` serves a forged
//! world with the server's real block materials; unset = procedural.

use std::time::{Duration, Instant};

use game::{RegionCoords, RegionSeed, RegionSpawner};

/// Resident set size of this process in MiB (Linux `/proc`; 0 elsewhere).
fn rss_mib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map_or(0, |kib| kib / 1024)
}

fn main() {
    game::set_hash_mode_from_env();
    let mut args = std::env::args().skip(1);
    let count: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(1);
    let hold_secs: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(0);
    let center = args
        .next()
        .and_then(|a| {
            let (x, z) = a.split_once(',')?;
            Some(RegionCoords::new(
                x.trim().parse().ok()?,
                z.trim().parse().ok()?,
            ))
        })
        .unwrap_or(RegionCoords::new(0, 0));
    let people: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(0);
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default()).unwrap();

    // The server's block registry and materials (biome blocks, trees).
    let manifest = include_str!("../../../assets/blocks/blocks.ron");
    let registry = game::BlockRegistry::from_ron(manifest).expect("invalid block manifest");
    game::install_block_props(registry.props());
    let materials = worldgen::material::Materials::from_names(|name| {
        registry
            .id_of(name)
            .unwrap_or_else(|| panic!("block manifest must define \"{name}\""))
    });
    match std::env::var(worldgen::forged::WORLD_FILE_ENV)
        .ok()
        .filter(|p| !p.is_empty() && p != "procedural")
    {
        Some(path) => {
            let world = worldgen::forged::ForgedWorld::load(std::path::Path::new(&path))
                .unwrap_or_else(|e| panic!("{path}: {e}"));
            worldgen::forged::install(Some(world));
            println!("world: forged {path}");
        }
        None => {
            worldgen::forged::install(None);
            println!("world: procedural");
        }
    }
    println!(
        "region {} blocks, chunk {} blocks; rss before regions: {} MiB",
        game::REGION_BLOCKS,
        game::CHUNK_BLOCKS,
        rss_mib()
    );

    // The center first, so `regions=1` runs the center itself.
    let mut window: Vec<RegionCoords> = (-1..=1)
        .flat_map(|x| (-1..=1).map(move |z| RegionCoords::new(center.x + x, center.z + z)))
        .collect();
    window.sort_by_key(|rc| rc != &center);
    let regions: Vec<RegionCoords> = window.into_iter().take(count.max(1)).collect();
    let (out_send, out_recv) = crossbeam::channel::unbounded();
    // Production path: region threads generate their own columns.
    let generator: server::region_threads::SharedRegionGenerator = std::sync::Arc::new(move |rc| {
        worldgen::generate_region(rc, worldgen::WORLD_SEED, materials)
    });
    let mut spawner = server::region_threads::ThreadRegionSpawner::with_region_generator(generator);
    let started = Instant::now();
    let mut inputs = Vec::new();
    for &rc in &regions {
        inputs.push(spawner.spawn(rc, RegionSeed::Deferred(None), out_send.clone()));
    }
    println!(
        "spawned {} region threads in {:.1} ms",
        regions.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    let mut pending: std::collections::BTreeSet<RegionCoords> = regions.iter().copied().collect();
    while !pending.is_empty() {
        let (rc, _output) = out_recv.recv().expect("region thread died");
        if pending.remove(&rc) {
            println!(
                "first output from {rc:?} after {:.0} ms; rss {} MiB",
                started.elapsed().as_secs_f64() * 1000.0,
                rss_mib()
            );
        }
    }
    if people > 0 {
        let at = regions
            .iter()
            .position(|&rc| rc == center)
            .expect("the center region runs");
        let edge = game::REGION_BLOCKS as u32;
        let grid: Vec<[u32; 2]> = (4..edge - 4)
            .step_by(4)
            .flat_map(|z| (4..edge - 4).step_by(4).map(move |x| [x, z]))
            .collect();
        assert!(
            people as usize <= grid.len(),
            "at most {} people fit the grid",
            grid.len()
        );
        for (i, &column) in grid.iter().take(people as usize).enumerate() {
            let spawn = game::PersonSpawn {
                person: game::history::PersonId(i as u32),
                at: game::SpawnAt::Column(column),
                walk_to: None,
            };
            inputs[at]
                .send(game::RegionInput::Event(game::GameEventKind::SpawnPerson(
                    spawn,
                )))
                .expect("region input");
        }
        println!("spawned {people} people in {center:?}");
    }
    let hold = Instant::now();
    while hold.elapsed() < Duration::from_secs(hold_secs) {
        // Drain outputs (as the manager would) so they don't pile up here.
        while out_recv.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_secs(2));
        println!("t+{:>3} s: rss {} MiB", hold.elapsed().as_secs(), rss_mib());
    }
    drop(inputs);
}
