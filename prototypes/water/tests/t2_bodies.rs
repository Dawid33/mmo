use water_proto::body::{self, check_invariants};
use water_proto::fixture::{carve, fill_solid, fill_water, finish};
use water_proto::grid::Dims;
use water_proto::state::World;
use water_proto::{EXPAND_BACKOFF, FULL, Mode};

const F: u64 = FULL as u64;

/// 40×40×8 m lake in a 64³ world, 4 m freeboard: water y 8..=15, air to y 19.
pub fn lake(mode: Mode) -> World {
    let mut w = World::new(Dims::cube(64), mode);
    fill_solid(&mut w, 0..=19);
    carve(&mut w, 12..=51, 8..=19, 12..=51);
    fill_water(&mut w, 12..=51, 8..=15, 12..=51);
    finish(&mut w);
    w
}

#[test]
fn lake_settles_into_one_body() {
    let w = lake(Mode::Bodies);
    assert_eq!(w.bodies.len(), 1);
    let b = w.bodies.values().next().unwrap();
    assert_eq!(b.cells, 12_800);
    assert_eq!(b.volume, 12_800 * F);
    assert_eq!(b.level(), 16 * F);
    assert!(w.water.iter().all(|&v| v == 0));
    assert_eq!(w.active.count(), 0);
    check_invariants(&w).unwrap();
}

#[test]
fn cell_only_lake_stays_cells() {
    let w = lake(Mode::CellOnly);
    assert!(w.bodies.is_empty());
    assert_eq!(w.mass(), 12_800 * F);
    assert_eq!(w.active.count(), 0);
}

#[test]
fn remove_volume_lowers_level_then_drops_a_layer() {
    let mut w = lake(Mode::Bodies);
    let id = *w.bodies.keys().next().unwrap();
    body::remove_volume(&mut w, id, 1600);
    assert_eq!(w.bodies[&id].level(), 16 * F - 1);
    body::remove_volume(&mut w, id, 1600 * F - 1600);
    let b = &w.bodies[&id];
    assert_eq!(b.top, 14);
    assert_eq!(b.level(), 15 * F);
    let vacated = w.dims.idx(20, 15, 20);
    assert_eq!(w.owner[vacated as usize], 0);
    assert!(w.active.get(vacated));
    check_invariants(&w).unwrap();
}

/// Two 6×6 basins joined over a sill (x 10..=13 solid up to y 9), water to y 12.
pub fn two_basins() -> World {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=15);
    carve(&mut w, 4..=9, 4..=15, 4..=9);
    carve(&mut w, 14..=19, 4..=15, 4..=9);
    carve(&mut w, 10..=13, 10..=15, 4..=9);
    fill_water(&mut w, 4..=9, 4..=12, 4..=9);
    fill_water(&mut w, 14..=19, 4..=12, 4..=9);
    fill_water(&mut w, 10..=13, 10..=12, 4..=9);
    finish(&mut w);
    w
}

#[test]
fn split_at_sill_shares_the_top_layer() {
    let mut w = two_basins();
    assert_eq!(w.bodies.len(), 1, "joined over the sill");
    let id = *w.bodies.keys().next().unwrap();
    let total = w.bodies[&id].volume;
    // layers 10..=12 hold 96 cells each (36 + 36 + 24 over the sill)
    body::remove_volume(&mut w, id, 288 * F + 1);
    assert_eq!(w.bodies.len(), 2);
    let v: Vec<u64> = w.bodies.values().map(|b| b.volume).collect();
    assert_eq!(v.iter().sum::<u64>(), total - 288 * F - 1);
    assert!(v[0].abs_diff(v[1]) <= 1);
    let over_sill = w.dims.idx(11, 10, 6);
    assert_eq!(w.owner[over_sill as usize], 0);
    check_invariants(&w).unwrap();
}

#[test]
fn expansion_onto_a_plain_is_refused_and_backs_off() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=3);
    carve(&mut w, 10..=11, 2..=3, 10..=11);
    fill_water(&mut w, 10..=11, 2..=3, 10..=11);
    finish(&mut w);
    let id = *w.bodies.keys().next().unwrap();
    let above = w.dims.idx(10, 4, 10);
    w.water[above as usize] = FULL; // a pour landed on the full pit
    assert!(!body::try_expand(&mut w, id));
    assert_eq!(w.stats.expansion_refusals, 1);
    assert_eq!(w.bodies[&id].expand_retry, w.step + EXPAND_BACKOFF);
    let flooded = w.stats.flood_cells;
    assert!(!body::try_expand(&mut w, id));
    assert_eq!(w.stats.flood_cells, flooded, "backoff: no second flood");
}

#[test]
fn merge_keeps_the_larger_body() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=7);
    carve(&mut w, 2..=5, 4..=7, 2..=5); // 16-cell layers
    carve(&mut w, 10..=11, 4..=7, 2..=3); // 4-cell layers
    fill_water(&mut w, 2..=5, 4..=5, 2..=5);
    fill_water(&mut w, 10..=11, 4..=5, 2..=3);
    finish(&mut w);
    let ids: Vec<u32> = w.bodies.keys().copied().collect();
    assert_eq!(ids.len(), 2);
    let total = w.mass();
    let keep = body::merge(&mut w, ids[1], ids[0]);
    assert_eq!(keep, ids[0], "the 32-cell body survives");
    assert_eq!(w.bodies.len(), 1);
    assert_eq!(w.mass(), total);
    assert_eq!(w.bodies[&keep].cells, 40);
    check_invariants(&w).unwrap();
}
