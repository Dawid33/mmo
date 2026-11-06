use water_proto::body;
use water_proto::fixture::{carve, fill_solid, fill_water, finish};
use water_proto::grid::Dims;
use water_proto::scenario::{NAMES, build, run, step_once};
use water_proto::state::World;
use water_proto::{FULL, Mode};

/// Every 5th step: hash, logged step, undo must restore the hash, and replay must
/// reproduce the post-step hash.
fn check_undo(name: &str, mode: Mode, steps: u32) {
    let mut sc = build(name, mode);
    for _ in 0..steps {
        if sc.world.step % 5 == 0 {
            let h0 = sc.world.hash();
            let (log, _) = step_once(&mut sc);
            let h1 = sc.world.hash();
            sc.world.undo(log);
            assert_eq!(sc.world.hash(), h0, "{name} {} undo at step {}", mode.name(), sc.world.step);
            step_once(&mut sc);
            assert_eq!(sc.world.hash(), h1, "{name} {} replay diverged", mode.name());
        } else {
            step_once(&mut sc);
        }
    }
}

#[test]
fn undo_is_exact_all_scenarios() {
    for name in NAMES {
        for mode in [Mode::CellOnly, Mode::Bodies] {
            check_undo(name, mode, 300);
        }
    }
}

#[test]
fn undo_across_split() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=15);
    carve(&mut w, 4..=9, 4..=15, 4..=9);
    carve(&mut w, 14..=19, 4..=15, 4..=9);
    carve(&mut w, 10..=13, 10..=15, 4..=9);
    fill_water(&mut w, 4..=9, 4..=12, 4..=9);
    fill_water(&mut w, 14..=19, 4..=12, 4..=9);
    fill_water(&mut w, 10..=13, 10..=12, 4..=9);
    finish(&mut w);
    let id = *w.bodies.keys().next().unwrap();
    let h0 = w.hash();
    w.begin_log();
    body::remove_volume(&mut w, id, 288 * FULL as u64 + 1);
    let log = w.end_log();
    assert_eq!(w.bodies.len(), 2);
    w.undo(log);
    assert_eq!(w.hash(), h0);
    assert_eq!(w.bodies.len(), 1);
}

#[test]
fn rerun_is_bit_identical() {
    for name in NAMES {
        for mode in [Mode::CellOnly, Mode::Bodies] {
            if name == "big_drain" && mode == Mode::CellOnly {
                continue; // 60k slow steps; the wasm golden check covers it
            }
            let a = run(&mut build(name, mode), |_| {});
            let b = run(&mut build(name, mode), |_| {});
            assert_eq!(a.final_hash, b.final_hash, "{name} {}", mode.name());
            assert!(a.mass_ok && a.edit_errors == 0, "{name} {}", mode.name());
        }
    }
}
