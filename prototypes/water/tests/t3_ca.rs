use water_proto::body::check_invariants;
use water_proto::edit::{Edit, EditError, apply};
use water_proto::fixture::{carve, fill_solid, fill_water, finish};
use water_proto::grid::Dims;
use water_proto::state::World;
use water_proto::{FILM, FULL, Mode, ca};

const F: u64 = FULL as u64;

fn run_until_asleep(w: &mut World, cap: u32) -> u32 {
    for _ in 0..cap {
        ca::step(w);
        if w.active.count() == 0 {
            return w.step;
        }
    }
    panic!("not asleep after {cap} steps");
}

fn lake(mode: Mode) -> World {
    let mut w = World::new(Dims::cube(64), mode);
    fill_solid(&mut w, 0..=19);
    carve(&mut w, 12..=51, 8..=19, 12..=51);
    fill_water(&mut w, 12..=51, 8..=15, 12..=51);
    finish(&mut w);
    w
}

#[test]
fn column_falls_to_the_floor() {
    let mut w = World::new(Dims::cube(16), Mode::CellOnly);
    fill_solid(&mut w, 0..=0);
    let at = w.dims.idx(5, 10, 5);
    apply(&mut w, Edit::Pour(at, FULL)).unwrap();
    run_until_asleep(&mut w, 400);
    assert_eq!(w.mass(), F);
    for i in 0..w.dims.len() as u32 {
        if w.water[i as usize] > 0 {
            assert_eq!(w.dims.y_of(i), 1, "all water rests on the floor");
        }
    }
}

#[test]
fn pit_levels_flat() {
    let mut w = World::new(Dims::cube(16), Mode::CellOnly);
    fill_solid(&mut w, 0..=3);
    carve(&mut w, 5..=7, 3..=3, 5..=7);
    let top = w.dims.idx(6, 8, 6);
    for _ in 0..4 {
        apply(&mut w, Edit::Pour(top, FULL)).unwrap();
        for _ in 0..8 {
            ca::step(&mut w);
        }
    }
    run_until_asleep(&mut w, 2000);
    let pit: Vec<u16> = (5..=7)
        .flat_map(|z| (5..=7).map(move |x| (x, z)))
        .map(|(x, z)| w.water[w.dims.idx(x, 3, z) as usize])
        .collect();
    assert_eq!(pit.iter().map(|&v| v as u64).sum::<u64>(), 4 * F);
    // diff < 2 never flows (film rule), so up to 1 unit per edge can remain across the 4-edge diagonal
    assert!(pit.iter().max().unwrap() - pit.iter().min().unwrap() <= 4, "{pit:?}");
}

#[test]
fn film_stops_spreading() {
    let mut w = World::new(Dims::cube(32), Mode::CellOnly);
    fill_solid(&mut w, 0..=3);
    let at = w.dims.idx(15, 4, 15);
    apply(&mut w, Edit::Pour(at, FULL)).unwrap();
    let at = w.dims.idx(16, 4, 15);
    apply(&mut w, Edit::Pour(at, FULL)).unwrap();
    run_until_asleep(&mut w, 2000);
    let wet = w.water.iter().filter(|&&v| v > 0).count();
    assert_eq!(w.mass(), 2 * F);
    assert!((32..=160).contains(&wet), "wet cells {wet}");
    assert!(w.water.iter().all(|&v| v <= 2 * FILM));
}

#[test]
fn settled_cells_cost_nothing() {
    let mut w = lake(Mode::CellOnly);
    for _ in 0..20 {
        ca::step(&mut w);
    }
    assert_eq!(w.stats.cells_visited, 0);
}

#[test]
fn pour_in_corner_conserves() {
    let mut w = World::new(Dims::cube(16), Mode::Bodies);
    fill_solid(&mut w, 0..=0);
    let at = w.dims.idx(0, 1, 0);
    apply(&mut w, Edit::Pour(at, FULL)).unwrap();
    run_until_asleep(&mut w, 2000);
    assert_eq!(w.mass(), F);
    check_invariants(&w).unwrap();
}

#[test]
fn waterfall_into_full_lake_joins_the_body() {
    let mut w = lake(Mode::Bodies);
    let spout = w.dims.idx(30, 18, 30);
    for _ in 0..20 {
        apply(&mut w, Edit::Pour(spout, FULL)).unwrap();
        ca::step(&mut w);
    }
    run_until_asleep(&mut w, 5000);
    assert_eq!(w.bodies.len(), 1);
    assert_eq!(w.water.iter().map(|&v| v as u64).sum::<u64>(), 0, "no stray CA water");
    let b = w.bodies.values().next().unwrap();
    assert_eq!(b.volume, 12_820 * F);
    assert_eq!(b.level(), 16 * F + 20 * F / 1600);
    check_invariants(&w).unwrap();
}

#[test]
fn hole_fills_pocket_and_joins_the_lake() {
    let mut w = World::new(Dims::cube(64), Mode::Bodies);
    fill_solid(&mut w, 0..=19);
    carve(&mut w, 12..=51, 8..=19, 12..=51);
    carve(&mut w, 8..=9, 10..=10, 20..=24); // 10 m³ dry pocket inside the west wall
    fill_water(&mut w, 12..=51, 8..=15, 12..=51);
    finish(&mut w);
    let at = w.dims.idx(10, 10, 22);
    apply(&mut w, Edit::Remove(at)).unwrap();
    let at = w.dims.idx(11, 10, 22);
    apply(&mut w, Edit::Remove(at)).unwrap();
    run_until_asleep(&mut w, 5000);
    assert_eq!(w.bodies.len(), 1);
    assert_eq!(w.water.iter().map(|&v| v as u64).sum::<u64>(), 0);
    let (id, b) = w.bodies.iter().next().unwrap();
    assert_eq!(w.owner[w.dims.idx(8, 10, 20) as usize], *id, "pocket joined the lake");
    // 12 new cells at y 10: below-top = 7*1600 + 12, frac = 1588 F over 1600 cells
    assert_eq!(b.level(), 15 * F + 1588 * F / 1600);
    check_invariants(&w).unwrap();
}

#[test]
fn place_into_sealed_body_rejected() {
    let mut w = World::new(Dims::cube(16), Mode::Bodies);
    fill_solid(&mut w, 0..=15);
    carve(&mut w, 4..=7, 4..=6, 4..=7);
    fill_water(&mut w, 4..=7, 4..=6, 4..=7);
    finish(&mut w);
    let h = w.hash();
    let at = w.dims.idx(5, 5, 5);
    assert_eq!(apply(&mut w, Edit::Place(at)), Err(EditError::NoRoom));
    assert_eq!(w.hash(), h, "a rejected edit changes nothing");
}

#[test]
fn place_into_full_lake_raises_level() {
    let mut w = lake(Mode::Bodies);
    let at = w.dims.idx(20, 10, 20);
    apply(&mut w, Edit::Place(at)).unwrap();
    run_until_asleep(&mut w, 2000);
    assert_eq!(w.mass(), 12_800 * F);
    assert_eq!(w.bodies.len(), 1);
    assert_eq!(w.bodies.values().next().unwrap().level(), 16 * F + F / 1600);
    check_invariants(&w).unwrap();
}

#[test]
fn settled_bodies_world_floods_nothing() {
    let mut w = lake(Mode::Bodies);
    for _ in 0..20 {
        ca::step(&mut w);
    }
    assert_eq!(w.stats.cells_visited, 0);
    assert_eq!(w.stats.flood_cells, 0);
}
