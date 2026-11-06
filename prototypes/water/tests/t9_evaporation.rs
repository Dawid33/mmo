use water_proto::edit::{Edit, apply};
use water_proto::fixture::{carve, fill_solid, finish};
use water_proto::grid::Dims;
use water_proto::scenario::{build, run};
use water_proto::state::World;
use water_proto::{EVAP_AFTER_STEPS, FULL, Mode, ca};

const F: u64 = FULL as u64;

#[test]
fn film_evaporates_into_ledger() {
    let mut sc = build("film", Mode::Bodies);
    sc.world.evap_after = EVAP_AFTER_STEPS;
    sc.max_steps = 5000;
    let r = run(&mut sc, |_| {});
    assert!(r.mass_ok);
    assert!(r.asleep_at.is_some());
    let w = &sc.world;
    assert!(w.water.iter().all(|&v| v == 0));
    assert_eq!(w.evaporated, sc.initial_mass + w.poured);
    assert_eq!(w.accounted(), sc.initial_mass + w.poured);
    assert!(w.evap_due.is_empty() && w.evap_of.is_empty());
}

#[test]
fn film_stays_when_evaporation_off() {
    let mut sc = build("film", Mode::Bodies);
    sc.max_steps = 5000;
    let r = run(&mut sc, |_| {});
    assert!(r.mass_ok);
    assert_eq!(sc.world.evaporated, 0);
    let wet = sc.world.water.iter().filter(|&&v| v > 0).count();
    assert!((32..=160).contains(&wet), "wet {wet}");
}

#[test]
fn evaporation_spares_bodies_and_deep_cells() {
    let mut sc = build("bucket", Mode::Bodies);
    sc.world.evap_after = EVAP_AFTER_STEPS;
    let r = run(&mut sc, |_| {});
    assert!(r.mass_ok);
    assert_eq!(sc.world.bodies.values().next().unwrap().level(), 3 * F + 2 * F / 9);
    assert_eq!(sc.world.accounted(), sc.initial_mass + sc.world.poured);

    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=31);
    carve(&mut w, 5..=5, 21..=21, 5..=5);
    carve(&mut w, 15..=15, 21..=21, 15..=15);
    finish(&mut w);
    w.evap_after = EVAP_AFTER_STEPS;
    let deep = w.dims.idx(5, 21, 5);
    let thin = w.dims.idx(15, 21, 15);
    w.begin_log();
    apply(&mut w, Edit::Pour(deep, 600)).unwrap();
    apply(&mut w, Edit::Pour(thin, 500)).unwrap();
    w.end_log();
    for _ in 0..300 {
        if w.step < EVAP_AFTER_STEPS {
            assert_eq!(w.water[thin as usize], 500);
        }
        w.begin_log();
        ca::step(&mut w);
        w.end_log();
    }
    assert_eq!(w.water[deep as usize], 600);
    assert_eq!(w.water[thin as usize], 0);
    assert_eq!(w.evaporated, 500);
}

#[test]
fn evaporation_undoes_exactly_in_both_journals() {
    use water_proto::state::JournalMode;
    for j in [JournalMode::PerCell, JournalMode::Brick] {
        let mut w = World::new(Dims::cube(32), Mode::Bodies);
        w.journal = j;
        fill_solid(&mut w, 0..=31);
        carve(&mut w, 15..=15, 21..=21, 15..=15);
        finish(&mut w);
        w.evap_after = 8;
        let thin = w.dims.idx(15, 21, 15);
        w.begin_log();
        apply(&mut w, Edit::Pour(thin, 500)).unwrap();
        w.end_log();
        for _ in 0..40 {
            let h = w.hash();
            w.begin_log();
            ca::step(&mut w);
            let log = w.end_log();
            let h2 = w.hash();
            w.undo(log.clone());
            assert_eq!(w.hash(), h, "{j:?}");
            w.begin_log();
            ca::step(&mut w);
            w.end_log();
            assert_eq!(w.hash(), h2);
        }
        assert_eq!(w.evaporated, 500);
    }
}
