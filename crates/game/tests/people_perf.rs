//! Region tick cost against the number of residents (people with bodies) — a measurement probe,
//! not a pass/fail test. It sets the process-global HASH_MODE (Off, as the binaries run), so it
//! has its own test target. Run in release:
//! `just test-one game "--release --test people_perf -- --ignored --nocapture"`.
#[allow(dead_code)]
mod common;

use std::time::Duration;

use game::history::PersonId;
use game::{GameEventKind, HashMode, Region, BLOCK_VOXELS, REGION_BLOCKS, REGION_SIZE, TICK_RATE};

const WARMUP_TICKS: u32 = 40;
const MEASURED_TICKS: u32 = 400;

fn floored_region() -> Region {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(game::Rollback::new(Some(send.clone())), Some(send), common::r0(), None);
    let size = REGION_SIZE;
    region.with_data(|r| r.seed_terrain_genesis(common::floor_chunks(0.0..size, 0.0..size, 16)));
    common::seed_floor_halo(&mut region, common::r0());
    region
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

/// `n` distinct columns spread over the region's inner area, 4 blocks apart.
fn columns(n: usize) -> Vec<(f32, f32)> {
    let edge = REGION_BLOCKS as i32;
    let bv = BLOCK_VOXELS as f32;
    let mut out = Vec::new();
    let mut z = 4;
    'rows: while z < edge - 4 {
        let mut x = 4;
        while x < edge - 4 {
            if out.len() == n {
                break 'rows;
            }
            out.push(((x as f32 + 0.5) * bv, (z as f32 + 0.5) * bv));
            x += 4;
        }
        z += 4;
    }
    assert_eq!(out.len(), n, "region too small for {n} people");
    out
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[test]
#[ignore = "measurement probe — run in release with --nocapture"]
fn tick_cost_by_resident_count() {
    game::set_hash_mode(HashMode::Off);
    println!("budget {TICK_RATE} ms/tick; {MEASURED_TICKS} ticks after {WARMUP_TICKS} warm-up; hash mode Off");
    println!("{:>4} {:>9} {:>9} {:>9} {:>9} {:>9}", "n", "avg_ms", "max_ms", "ctrl_ms", "input_ms", "scan_ms");
    for n in [0usize, 1, 5, 10, 20, 40, 80] {
        let mut region = floored_region();
        region.with_data(|d| {
            for (i, xz) in columns(n).into_iter().enumerate() {
                d.spawn_person_safe(PersonId(i as u32), xz).expect("spawned");
            }
        });
        for _ in 0..WARMUP_TICKS {
            tick(&mut region);
        }
        let _ = region.take_tick_perf();
        for _ in 0..MEASURED_TICKS {
            tick(&mut region);
        }
        let p = region.take_tick_perf();
        let per = |d: Duration| ms(d) / p.ticks.max(1) as f64;
        println!(
            "{n:>4} {:>9.3} {:>9.3} {:>9.3} {:>9.3} {:>9.3}",
            per(p.total),
            ms(p.max),
            per(p.controllers),
            per(p.input),
            per(p.scan)
        );
    }
    game::set_hash_mode(HashMode::Full);
}
