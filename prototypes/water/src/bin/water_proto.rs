//! Scenario table: cost, undo bytes, state size, structural counters and hashes
//! for both models. `--golden` prints only "<scenario> <mode> <hash>" lines (then one
//! "<seam scenario> pair <hash>" line each). `--seam` prints the untimed seam table.

use water_proto::metrics::column_surface;
use water_proto::state::{NO_BODY, World};
use water_proto::{EVAP_AFTER_STEPS, Mode};
use water_proto::state::JournalMode;
use water_proto::scenario::{self, NAMES, drain_a_spread};
use water_proto::seam;

/// Timing runs repeat each scenario this many times and report medians.
const TIMING_REPEATS: usize = 5;

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// Non-timing columns taken from a finished world.
fn members(w: &World) -> u64 {
    w.bodies.values().map(|b| b.cells).sum()
}

fn wet_cells(w: &World) -> usize {
    (0..w.dims.len()).filter(|&i| !w.solid.get(i as u32) && w.owner[i] == NO_BODY && w.water[i] > 0).count()
}

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
    let evap = args.iter().any(|a| a == "--evap");
    let repeats = if golden { 1 } else { TIMING_REPEATS };
    if !golden {
        println!("# evap={} repeats={repeats} (medians of per-repeat timings)", if evap { "on" } else { "off" });
        println!(
            "{:<12} {:<6} {:>6} {:>6} {:>9} {:>9} {:>9} {:>11} {:>9} {:>9} {:>9} {:>9} {:>9} {:>4} {:>5} {:>5} {:>5} {:>5} {:>5} {:>7} {:>7} {:>5} {:>6} {:>7} {:>7} {:>8} {:>8}",
            "scenario", "mode", "steps", "asleep", "mean_us", "p99_us", "max_us", "total_us", "visit/st",
            "undoB/st", "undoBmax", "denseKiB", "sparseKiB", "bods", "seed", "absb", "expn",
            "split", "merge", "A_sprd", "flood", "relev", "drops", "members", "wet", "U_left", "U_right"
        );
    }
    let mut failed = false;
    for name in NAMES {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        for &mode in &modes {
            let mut runs: Vec<(scenario::Report, u64, scenario::Scenario)> = Vec::new();
            for _ in 0..repeats {
                let mut sc = scenario::build(name, mode);
                sc.world.journal = journal;
                if evap {
                    sc.world.evap_after = EVAP_AFTER_STEPS;
                }
                let mut spread = 0u64;
                let r = scenario::run(&mut sc, |w| {
                    if name == "big_drain" && w.step % 100 == 0 {
                        spread = spread.max(drain_a_spread(w).unwrap_or(0));
                    }
                });
                runs.push((r, spread, sc));
            }
            for run in &runs[1..] {
                assert_eq!(run.0.final_hash, runs[0].0.final_hash, "{name} {}: repeats diverged", mode.name());
            }
            let (r, spread, sc) = &runs[0];
            let spread = *spread;
            if !(r.mass_ok && r.edit_errors == 0) {
                failed = true;
                eprintln!("FAILURE {name} {}: mass_ok={} edit_errors={}", mode.name(), r.mass_ok, r.edit_errors);
            }
            if golden {
                println!("{name} {} {:016x}", mode.name(), r.final_hash);
                continue;
            }
            let n = r.step_ns.len().max(1) as f64;
            let (mut totals, mut means, mut p99s, mut maxs) = (vec![], vec![], vec![], vec![]);
            for (rr, _, _) in &runs {
                let mut ns = rr.step_ns.clone();
                ns.sort_unstable();
                let sum = rr.step_ns.iter().sum::<u64>() as f64;
                totals.push(sum / 1000.0);
                means.push(sum / n / 1000.0);
                p99s.push(ns.get(((ns.len() as f64) * 0.99) as usize).copied().unwrap_or(0) as f64 / 1000.0);
                maxs.push(ns.last().copied().unwrap_or(0) as f64 / 1000.0);
            }
            let undo_mean = r.undo_bytes.iter().map(|&b| b as f64).sum::<f64>() / n;
            let (u_left, u_right) = if name == "u_tube" {
                let f = |x| column_surface(&sc.world, x, 30).map_or("-".to_string(), |v| v.to_string());
                (f(20), f(40))
            } else {
                ("-".to_string(), "-".to_string())
            };
            println!(
                "{:<12} {:<6} {:>6} {:>6} {:>9.2} {:>9.2} {:>9.2} {:>11.0} {:>9.1} {:>9.0} {:>9} {:>9.1} {:>9.1} {:>4} {:>5} {:>5} {:>5} {:>5} {:>5} {:>7} {:>7} {:>5} {:>6} {:>7} {:>7} {:>8} {:>8}{}",
                name,
                mode.name(),
                r.steps,
                r.asleep_at.map_or("-".to_string(), |s| s.to_string()),
                median(&mut means),
                median(&mut p99s),
                median(&mut maxs),
                median(&mut totals),
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
                r.stats.flood_cells,
                r.stats.relevels,
                r.stats.layer_drops,
                members(&sc.world),
                wet_cells(&sc.world),
                u_left,
                u_right,
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
    println!("# SEAM_GAIN_DIV={} SEAM_CAP={} SEAM_LAG_STEPS={}", seam::SEAM_GAIN_DIV, water_proto::SEAM_CAP, water_proto::SEAM_LAG_STEPS);
    println!(
        "{:<12} {:>6} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8} {:>6} {:>5}",
        "scenario", "steps", "settled", "A_mass", "A_level", "B_mass", "B_level", "lvl_diff", "sent_AB", "sent_BA",
        "bounced", "bnc_shr", "flips", "mass"
    );
    let level = |w: &water_proto::state::World| -> String {
        match w.bodies.len() {
            1 => w.bodies.values().next().unwrap().level().to_string(),
            n => format!("{n}bod"),
        }
    };
    let lvl_num = |w: &water_proto::state::World| -> Option<u64> {
        (w.bodies.len() == 1).then(|| w.bodies.values().next().unwrap().level())
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
        let diff = match (lvl_num(&p.a), lvl_num(&p.b)) {
            (Some(x), Some(y)) => x.abs_diff(y).to_string(),
            _ => "-".to_string(),
        };
        let gross = p.gross_ab + p.gross_ba;
        let share = if gross > 0 { format!("{:.3}", p.bounced as f64 / gross as f64) } else { "-".to_string() };
        println!(
            "{:<12} {:>6} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>8} {:>6} {:>5}",
            name, steps, p.settled(), p.a.mass(), level(&p.a), p.b.mass(), level(&p.b), diff, p.gross_ab, p.gross_ba,
            p.bounced, share, flips, if mass_ok { "ok" } else { "FAIL" },
        );
    }
    code
}
