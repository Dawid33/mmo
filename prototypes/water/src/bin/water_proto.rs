//! Scenario table: cost, undo bytes, state size, structural counters and hashes
//! for both models. `--golden` prints only "<scenario> <mode> <hash>" lines.

use water_proto::Mode;
use water_proto::scenario::{self, NAMES, drain_a_spread};

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let golden = args.iter().any(|a| a == "--golden");
    let only = arg(&args, "--scenario");
    let modes = match arg(&args, "--mode").as_deref() {
        Some("cell") => vec![Mode::CellOnly],
        Some("bodies") => vec![Mode::Bodies],
        _ => vec![Mode::CellOnly, Mode::Bodies],
    };
    if !golden {
        println!(
            "{:<12} {:<6} {:>6} {:>6} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>4} {:>5} {:>5} {:>5} {:>5} {:>5} {:>7}",
            "scenario", "mode", "steps", "asleep", "mean_us", "p99_us", "max_us", "visit/st",
            "undoB/st", "undoBmax", "denseKiB", "sparseKiB", "bods", "seed", "absb", "expn",
            "split", "merge", "A_sprd"
        );
    }
    for name in NAMES {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        for &mode in &modes {
            let mut sc = scenario::build(name, mode);
            let mut spread = 0u64;
            let r = scenario::run(&mut sc, |w| {
                if name == "big_drain" && w.step % 100 == 0 {
                    spread = spread.max(drain_a_spread(w).unwrap_or(0));
                }
            });
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
}
