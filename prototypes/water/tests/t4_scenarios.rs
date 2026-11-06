use water_proto::body::check_invariants;
use water_proto::edit::Edit;
use water_proto::fixture::{carve, fill_solid, fill_water, finish};
use water_proto::grid::Dims;
use water_proto::metrics::column_surface;
use water_proto::scenario::{self, Scenario, build, drain_a_spread, run};
use water_proto::state::World;
use water_proto::{EXPAND_BACKOFF, FULL, Mode};

const F: u64 = FULL as u64;

fn ok(r: &scenario::Report) {
    assert!(r.mass_ok, "{} {}: mass drifted", r.name, r.mode.name());
    assert_eq!(r.edit_errors, 0, "{} {}: edit rejected", r.name, r.mode.name());
}

#[test]
fn settled_costs_nothing_in_both_modes() {
    for mode in [Mode::CellOnly, Mode::Bodies] {
        let mut sc = build("settled", mode);
        let r = run(&mut sc, |_| {});
        ok(&r);
        assert_eq!(r.steps, 100);
        assert_eq!(r.stats.cells_visited, 0, "{}", mode.name());
    }
}

#[test]
fn small_spill_settles_in_both_modes() {
    for mode in [Mode::CellOnly, Mode::Bodies] {
        let mut sc = build("small_spill", mode);
        let r = run(&mut sc, |_| {});
        ok(&r);
        assert!(r.asleep_at.is_some(), "{} never slept", mode.name());
    }
}

#[test]
fn bucket_fills_the_pit() {
    let mut sc = build("bucket", Mode::Bodies);
    let r = run(&mut sc, |_| {});
    ok(&r);
    let w = &sc.world;
    assert_eq!(w.bodies.len(), 1);
    // 9-cell pit: y1, y2 full (18 m³), 2 m³ on y3
    assert_eq!(w.bodies.values().next().unwrap().level(), 3 * F + 2 * F / 9);
    check_invariants(w).unwrap();
}

#[test]
fn film_puddle_stays_in_both_modes() {
    for mode in [Mode::CellOnly, Mode::Bodies] {
        let mut sc = build("film", mode);
        let r = run(&mut sc, |_| {});
        ok(&r);
        assert!(r.asleep_at.is_some());
        let wet = sc.world.water.iter().filter(|&&v| v > 0).count();
        assert!((32..=160).contains(&wet), "{} wet {wet}", mode.name());
        assert!(sc.world.bodies.is_empty());
    }
}

#[test]
fn u_tube_communicating_vessels() {
    let mut sc = build("u_tube", Mode::Bodies);
    let r = run(&mut sc, |_| {});
    ok(&r);
    assert_eq!(sc.world.bodies.len(), 1);
    // layers 4,5: 44 cells each; 6: 8 cells; 4 m³ over the 8 cells of layer 7
    assert_eq!(sc.world.bodies.values().next().unwrap().level(), 7 * F + F / 2);

    // the cell-only CA has no pressure: the far shaft stays lower (known limitation)
    let mut sc = build("u_tube", Mode::CellOnly);
    let r = run(&mut sc, |_| {});
    ok(&r);
    let left = column_surface(&sc.world, 20, 30).unwrap();
    let right = column_surface(&sc.world, 40, 30).unwrap();
    assert!(right < left, "cell-only: right {right} left {left}");
}

#[test]
fn big_drain_merges_at_a_common_level() {
    let mut sc = build("big_drain", Mode::Bodies);
    let mut worst = 0;
    let r = run(&mut sc, |w| {
        if w.step % 100 == 0 {
            worst = worst.max(drain_a_spread(w).unwrap_or(0));
        }
    });
    ok(&r);
    assert!(r.asleep_at.is_some(), "drain never settled");
    assert_eq!(worst, 0, "lake A's surface stays flat while draining");
    let w = &sc.world;
    assert_eq!(w.bodies.len(), 1, "A, channel and B are one body");
    let level = w.bodies.values().next().unwrap().level();
    let expect = 23 * F + 1148 * F / 1920;
    assert!(level.abs_diff(expect) <= 8, "level {level} expect {expect}");
    check_invariants(w).unwrap();
}

#[test]
fn floor_hole_drains_lake_into_cavity() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=20);
    carve(&mut w, 4..=7, 2..=5, 4..=7); // 64 m³ cavity
    carve(&mut w, 4..=7, 12..=13, 4..=7); // 32 m³ lake, directly above
    fill_water(&mut w, 4..=7, 12..=13, 4..=7);
    finish(&mut w);
    let d = w.dims;
    let edits = (6..=11).map(|y| (0, Edit::Remove(d.idx(5, y, 5)))).collect();
    let initial_mass = w.mass();
    let mut sc = Scenario {
        name: "floor_hole",
        world: w,
        edits,
        max_steps: 20_000,
        stop_when_asleep: true,
        initial_mass,
    };
    let r = run(&mut sc, |_| {});
    ok(&r);
    let w = &sc.world;
    assert_eq!(w.bodies.len(), 1, "the drained lake body is gone");
    let b = w.bodies.values().next().unwrap();
    assert_eq!(b.level(), 4 * F, "cavity y2, y3 full (32 m³ over 16 cells)");
    assert!(b.hi[1] <= 5);
    check_invariants(w).unwrap();
}

#[test]
fn plain_overflow_backs_off() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=3);
    carve(&mut w, 10..=11, 2..=3, 10..=11); // 8 m³ pit in a plain
    finish(&mut w);
    let spout = w.dims.idx(10, 8, 10);
    let edits = (0..12).map(|k| (k, Edit::Pour(spout, FULL))).collect();
    let mut sc = Scenario {
        name: "plain_overflow",
        world: w,
        edits,
        max_steps: 5_000,
        stop_when_asleep: true,
        initial_mass: 0,
    };
    let r = run(&mut sc, |_| {});
    ok(&r);
    let w = &sc.world;
    assert_eq!(w.bodies.len(), 1, "the pit is a body");
    assert_eq!(w.water.iter().map(|&v| v as u64).sum::<u64>(), 4 * F, "4 m³ film on the plain");
    assert!(r.stats.expansion_refusals >= 1);
    let bound = (r.steps as u64 / EXPAND_BACKOFF as u64 + 2) * 1024 + 64;
    assert!(r.stats.flood_cells <= bound, "flooded {} > {bound}", r.stats.flood_cells);
}
