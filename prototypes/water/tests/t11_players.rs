use water_proto::edit::{Edit, EditError, apply};
use water_proto::scenario::{build, run};
use water_proto::state::World;
use water_proto::fixture::gate_basins;
use water_proto::{FULL, GATE_CAP, Mode};

type R = std::ops::RangeInclusive<u32>;

fn box_total(w: &World, xs: R, ys: R, zs: R) -> u64 {
    let mut t = 0;
    for y in ys {
        for z in zs.clone() {
            for x in xs.clone() {
                let i = w.dims.idx(x, y, z);
                t += match w.owner[i as usize] {
                    0 => w.water[i as usize] as u64,
                    id => w.bodies[&id].virtual_at(y) as u64,
                };
            }
        }
    }
    t
}

fn body_volume_in(w: &World, xs: R) -> u64 {
    w.bodies.values().filter(|b| xs.contains(&b.lo[0])).map(|b| b.volume).sum()
}

fn run_name(name: &str) -> (World, water_proto::scenario::Report) {
    let mut sc = build(name, Mode::Bodies);
    let r = run(&mut sc, |_| {});
    assert!(r.mass_ok, "{name}: mass");
    assert_eq!(r.edit_errors, 0, "{name}: edit errors");
    assert!(r.asleep_at.is_some(), "{name}: never slept");
    (sc.world, r)
}

#[test]
fn dam_breach_drains_lake_into_valley() {
    let (w, _) = run_name("dam_breach");
    let b: Vec<_> = w.bodies.values().filter(|b| b.lo[0] >= 21).collect();
    assert_eq!(b.len(), 1);
    assert!(b[0].level().abs_diff(8 * FULL as u64) <= 512, "level {}", b[0].level());
    let a_bodies = body_volume_in(&w, 0..=20); // A-side bodies
    let a_cells = box_total(&w, 4..=19, 12..=20, 4..=19);
    eprintln!("residue: bodies {a_bodies}, cells {a_cells}");
    assert!(a_bodies.max(a_cells) <= 512, "A residue");
}

#[test]
fn gate_flood_open_then_close() {
    let mut sc = build("gate_flood", Mode::Bodies);
    let mut at_211 = None;
    let r = run(&mut sc, |w| {
        if w.step == 211 {
            at_211 = Some(box_total(w, 21..=44, 4..=20, 4..=19));
        }
    });
    assert!(r.mass_ok);
    assert_eq!(r.edit_errors, 0);
    assert!(r.asleep_at.is_some());
    let end = box_total(&sc.world, 21..=44, 4..=20, 4..=19);
    assert_eq!(end, 200 * GATE_CAP);
    assert_eq!(at_211, Some(end));
}

#[test]
fn pumped_cistern_fills_one_layer() {
    let (w, _) = run_name("pumped_cistern");
    assert_eq!(box_total(&w, 30..=32, 12..=14, 30..=32), 9 * FULL as u64);
    let pool: Vec<_> = w.bodies.values().filter(|b| b.lo[0] == 4).collect();
    assert_eq!(pool.len(), 1);
    assert_eq!(pool[0].level(), 9 * FULL as u64 - 576);
}

#[test]
fn scoop_lowers_lake_by_one_bucket() {
    let (w, _) = run_name("scoop");
    assert_eq!(w.scooped, FULL as u64);
    let a: Vec<_> = w.bodies.values().filter(|b| b.lo[0] < 21).collect();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].level(), 18 * FULL as u64 - 16);
}

#[test]
fn scoop_dry_cell_rejected() {
    let mut w = gate_basins(Mode::Bodies);
    let dry = w.dims.idx(30, 10, 5);
    let h0 = w.hash();
    assert_eq!(apply(&mut w, Edit::Scoop(dry, FULL)), Err(EditError::Dry));
    assert_eq!(w.hash(), h0);
}
