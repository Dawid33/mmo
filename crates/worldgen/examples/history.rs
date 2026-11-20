//! World history: bootstrap + fast-forward a forged world's history and print
//! statistics and the log (the event manager is `game::history`).
//!
//! Run: just history --years 100
//!      just history --world worlds/x/terrain.world --civs 6 --years 300 --save
//!      just history --dump worlds/x/terrain.history --log 80
//!
//! Flags: --world <file> --civs N --years N --founders N --seed N --log N
//! --save (write `history_path_for(world)`; refuses to overwrite without --force)
//! --force --dump <history file> (print an existing history instead of bootstrapping).

use std::path::PathBuf;
use std::time::Instant;

use game::history::bootstrap::{BootstrapParams, bootstrap};
use game::history::{History, SocietyGrid, persist};

const DEFAULT_WORLD: &str = "worlds/default.world";

fn main() {
    let mut world = PathBuf::from(DEFAULT_WORLD);
    let mut params = BootstrapParams::default();
    let mut log_lines = 40usize;
    let (mut save, mut force) = (false, false);
    let mut dump: Option<PathBuf> = None;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--world" => world = PathBuf::from(value()),
            "--civs" => params.civs = value().parse().expect("--civs N"),
            "--years" => params.years = value().parse().expect("--years N"),
            "--founders" => params.founders_per_civ = value().parse().expect("--founders N"),
            "--seed" => params.seed = value().parse().expect("--seed N"),
            "--log" => log_lines = value().parse().expect("--log N"),
            "--save" => save = true,
            "--force" => force = true,
            "--dump" => dump = Some(PathBuf::from(value())),
            other => panic!(
                "unknown argument {other}; flags: --world --civs --years --founders --seed --log --save --force --dump"
            ),
        }
    }

    if !world.exists() {
        eprintln!(
            "{}: no such world file; export one from its store with `just worldctl export --db <store> <out.world>`",
            world.display()
        );
        std::process::exit(2);
    }
    let t = Instant::now();
    let forged = worldgen::forged::ForgedWorld::load(&world)
        .unwrap_or_else(|e| panic!("{}: {e}", world.display()));
    let map = worldgen::tile_map::build_world_map(&forged);
    println!(
        "world {} loaded, {} tiles ({:.1?})",
        world.display(),
        map.tiles.len(),
        t.elapsed()
    );

    let history: History = if let Some(path) = dump {
        let grid = SocietyGrid::from_world_map(&map);
        persist::load(&path, grid).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    } else {
        let out = persist::history_path_for(&world);
        if save && out.exists() && !force {
            eprintln!("{} exists; pass --force to overwrite", out.display());
            std::process::exit(1);
        }
        let t = Instant::now();
        let h = bootstrap(&map, &params).unwrap_or_else(|e| {
            eprintln!("bootstrap failed: {e}");
            std::process::exit(1);
        });
        println!(
            "bootstrapped {} years, seed {} ({:.1?})",
            params.years,
            params.seed,
            t.elapsed()
        );
        if save {
            persist::save(&h, &out).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
            println!("saved {}", out.display());
        }
        h
    };

    println!("{}", history.stats());
    let skip = history.log.len().saturating_sub(log_lines);
    println!(
        "log (last {} of {}):",
        history.log.len() - skip,
        history.log.len()
    );
    for e in &history.log[skip..] {
        println!("  {}", e.describe(&history));
    }
}
