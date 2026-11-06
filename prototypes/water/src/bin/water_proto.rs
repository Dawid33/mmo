//! Scenario table: cost, undo bytes, state size, structural counters and hashes
//! for both models. `--golden` prints only "<scenario> <mode> <hash>" lines (then one
//! "<seam scenario> pair <hash>" line each). `--seam` prints the untimed seam table.

use water_proto::Mode;
use water_proto::state::JournalMode;
use water_proto::scenario::{self, NAMES, drain_a_spread};
use water_proto::seam;

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let golden = args.iter().any(|a| a == "--golden");
    if args.iter().any(|a| a == "--seam") {
        std::process::exit(seam_table());
    }
    let only = arg(&args, "--scenario");
    let modes = match arg(&args, "--mode").as_deref() {
        Some("cell") => vec![Mode::CellOnly],
        Some("bodies") => vec![Mode::Bodies],
        _ => vec![Mode::CellOnly, Mode::Bodies],
    };
    let journal = match arg(&args, "--undo").as_deref() {
        Some("brick") => JournalMode::Brick,
        Some("cell") | None => JournalMode::PerCell,
        Some(o) => panic!("--undo takes cell|brick, not {o}"),
    };
    if !golden {
        println!(
            "{:<12} {:<6} {:>6} {:>6} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>4} {:>5} {:>5} {:>5} {:>5} {:>5} {:>7}",
            "scenario", "mode", "steps", "asleep", "mean_us", "p99_us", "max_us", "visit/st",
            "undoB/st", "undoBmax", "denseKiB", "sparseKiB", "bods", "seed", "absb", "expn",
            "split", "merge", "A_sprd"
        );
    }
    let mut failed = false;
    for name in NAMES {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        for &mode in &modes {
            let mut sc = scenario::build(name, mode);
            sc.world.journal = journal;
            let mut spread = 0u64;
            let r = scenario::run(&mut sc, |w| {
                if name == "big_drain" && w.step % 100 == 0 {
                    spread = spread.max(drain_a_spread(w).unwrap_or(0));
                }
            });
            if !(r.mass_ok && r.edit_errors == 0) {
                failed = true;
                eprintln!("FAILURE {name} {}: mass_ok={} edit_errors={}", mode.name(), r.mass_ok, r.edit_errors);
            }
            if golden {
                println!("{name} {} {:016x}", mode.name(), r.final_hash);
                continue;
            }
            let n = r.step_ns.len().max(1) as f64;
            let mut ns = r.step_ns.clone();
            ns.sort_unstable();
            let p99 = ns.get(((ns.len() as f64) * 0.99) as usize).copied().unwrap_or(0);
            let undo_mean = r.undo_bytes.iter().map(|&b| b as f64).sum::<f64>() / n;
            println!(
                "{:<12} {:<6} {:>6} {:>6} {:>9.2} {:>9.2} {:>9.2} {:>9.1} {:>9.0} {:>9} {:>9.1} {:>9.1} {:>4} {:>5} {:>5} {:>5} {:>5} {:>5} {:>7}{}",
                name,
                mode.name(),
                r.steps,
                r.asleep_at.map_or("-".to_string(), |s| s.to_string()),
                r.step_ns.iter().sum::<u64>() as f64 / n / 1000.0,
                p99 as f64 / 1000.0,
                ns.last().copied().unwrap_or(0) as f64 / 1000.0,
                r.stats.cells_visited as f64 / n,
                undo_mean,
                r.undo_bytes.iter().max().copied().unwrap_or(0),
                r.dense_bytes as f64 / 1024.0,
                r.sparse_bytes as f64 / 1024.0,
                r.bodies,
                r.stats.seeds,
                r.stats.absorbs,
                r.stats.expansions,
                r.stats.splits,
                r.stats.merges,
                spread,
                if r.mass_ok && r.edit_errors == 0 { "" } else { "  !! MASS/EDIT FAILURE" },
            );
        }
    }
    if golden && only.is_none() {
        for name in seam::NAMES {
            let mut p = seam::build(name);
            p.run(seam::SEAM_MAX_STEPS);
            println!("{name} pair {:016x}", p.hash());
        }
    }
    if failed {
        std::process::exit(1);
    }
}

/// `--seam`: each seam scenario run to rest (untimed). Returns the exit code.
fn seam_table() -> i32 {
    println!(
        "{:<12} {:>6} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>6} {:>5}",
        "scenario", "steps", "settled", "A_mass", "A_level", "B_mass", "B_level", "bounced", "flips", "mass"
    );
    let level = |w: &water_proto::state::World| -> String {
        match w.bodies.len() {
            1 => w.bodies.values().next().unwrap().level().to_string(),
            n => format!("{n}bod"),
        }
    };
    let mut code = 0;
    for name in seam::NAMES {
        let mut p = seam::build(name);
        let (mut steps, mut flips, mut last, mut mass_ok) = (0, 0, 0i64, true);
        while steps < seam::SEAM_MAX_STEPS {
            p.step();
            steps += 1;
            mass_ok &= p.accounted() == p.initial;
            let s = p.net_flux_last_step().signum();
            if s != 0 {
                if last != 0 && s != last {
                    flips += 1;
                }
                last = s;
            }
            if p.settled() {
                break;
            }
        }
        if !mass_ok {
            code = 1;
        }
        println!(
            "{:<12} {:>6} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>6} {:>5}",
            name, steps, p.settled(), p.a.mass(), level(&p.a), p.b.mass(), level(&p.b), p.bounced, flips,
            if mass_ok { "ok" } else { "FAIL" },
        );
    }
    code
}
