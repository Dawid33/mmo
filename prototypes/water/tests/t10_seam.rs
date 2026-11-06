use water_proto::edit::{Edit, apply};
use water_proto::fixture::{fill_solid, finish};
use water_proto::grid::Dims;
use water_proto::link::avail;
use water_proto::seam::{self, NAMES, Pair};
use water_proto::state::World;
use water_proto::{FULL, Mode, SEAM_CAP, SEAM_SETTLE_TOL, ca};

const F: u64 = FULL as u64;

/// Step until both worlds sleep with nothing in flight; returns the step count.
fn settle(p: &mut Pair, max_steps: u32) -> u32 {
    for k in 0..max_steps {
        p.step();
        if p.settled() {
            return k + 1;
        }
    }
    panic!("pair did not settle in {max_steps} steps");
}

fn sole_level(w: &World) -> u64 {
    assert_eq!(w.bodies.len(), 1, "expected one body, got {}", w.bodies.len());
    w.bodies.values().next().unwrap().level()
}

#[test]
fn seam_conserves_mass_every_step() {
    for mode in [Mode::Bodies, Mode::CellOnly] {
        for name in NAMES {
            let mut p = seam::build_in(name, mode);
            for k in 0..2000 {
                p.step();
                assert_eq!(p.accounted(), p.initial, "{name} {mode:?} step {k}");
            }
        }
    }
}

#[test]
fn seam_drain_moves_lake_into_neighbour() {
    let mut p = seam::build("seam_drain");
    let steps = settle(&mut p, 20_000);
    let lb = sole_level(&p.b);
    let residue = p.a.mass();
    println!("seam_drain: settled at {steps}, B level {lb} (10F = {}), A residue {residue}, bounced {}", 10 * F, p.bounced);
    assert!(lb.abs_diff(10 * F) <= 512, "B level {lb}");
    assert!(residue < 2 * 448, "A residue {residue}");
    assert_eq!(p.accounted(), p.initial);
}

#[test]
fn seam_lake_levels_across_the_seam() {
    let mut p = seam::build("seam_lake");
    let steps = settle(&mut p, 20_000);
    let (la, lb) = (sole_level(&p.a), sole_level(&p.b));
    println!("seam_lake: settled at {steps}, A {la}, B {lb}, bounced {}", p.bounced);
    assert!(la.abs_diff(lb) <= SEAM_SETTLE_TOL, "A {la} B {lb}");
    assert!((la + lb).abs_diff(2 * 15 * F) <= SEAM_SETTLE_TOL, "A {la} B {lb}");
    assert_eq!(p.accounted(), p.initial);
}

#[test]
fn seam_flux_is_damped() {
    let mut p = seam::build("seam_lake");
    let mut last_sign = 0i64;
    let mut changes = 0;
    for _ in 0..20_000 {
        p.step();
        let s = p.net_flux_last_step().signum();
        if s != 0 {
            if last_sign != 0 && s != last_sign {
                changes += 1;
            }
            last_sign = s;
        }
        if p.settled() {
            break;
        }
    }
    assert!(p.settled());
    println!("seam_flux_is_damped: {changes} sign changes (gain divisor {})", seam::SEAM_GAIN_DIV);
    assert!(changes <= 4, "{changes} sign changes");
}

#[test]
fn seam_full_receiver_fills_without_bounce() {
    let mut p = seam::build("seam_bounce");
    settle(&mut p, 20_000);
    let pocket = p.b.dims.idx(0, 13, 12);
    assert_eq!(avail(&p.b, pocket), F);
    assert_eq!(p.b.mass(), F);
    assert_eq!(p.accounted(), p.initial);
    assert_eq!(p.bounced, 0);
}

#[test]
fn seam_receiver_filled_during_lag_bounces_back() {
    let mut p = seam::build("seam_bounce");
    let a0 = p.a.mass();
    p.step(); // A sends SEAM_CAP into the pocket, due next step
    assert_eq!(p.inflight.len(), 1);
    let pocket = p.b.dims.idx(0, 13, 12);
    apply(&mut p.b, Edit::Pour(pocket, FULL)).unwrap(); // the pocket fills from its own side
    settle(&mut p, 20_000);
    println!("lag bounce: bounced {}", p.bounced);
    assert!(p.bounced >= SEAM_CAP, "bounced {}", p.bounced);
    assert_eq!(p.accounted(), p.initial + p.b.poured);
    assert_eq!(avail(&p.b, pocket), F);
    assert_eq!(p.b.mass(), F);
    assert_eq!(p.a.mass(), a0, "every bounced unit returned to A");
}

#[test]
fn seam_solid_face_is_closed() {
    let solid_b = || {
        let mut b = World::new(Dims::cube(32), Mode::Bodies);
        fill_solid(&mut b, 0..=31);
        finish(&mut b);
        b
    };
    let mut p = Pair::new(seam::drain_a(Mode::Bodies), solid_b());
    let mut alone = seam::drain_a(Mode::Bodies);
    for _ in 0..200 {
        p.step();
        assert!(p.inflight.is_empty());
        assert_eq!(p.net_flux_last_step(), 0);
        ca::step(&mut alone);
    }
    assert_eq!(p.bounced, 0);
    assert_eq!(p.a.hash(), alone.hash());
}

#[test]
fn pair_step_undo_is_exact() {
    let mut p = seam::build("seam_lake");
    for _ in 0..30 {
        p.step();
    }
    let mut hashes = Vec::new();
    let mut undos = Vec::new();
    for _ in 0..10 {
        hashes.push(p.hash());
        undos.push(p.step());
    }
    while let Some(u) = undos.pop() {
        p.undo(u);
        assert_eq!(p.hash(), hashes.pop().unwrap());
    }
}
