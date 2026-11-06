use water_proto::edit::{Edit, apply};
use water_proto::fixture::{carve, fill_solid, fill_water, finish, gate_basins};
use water_proto::grid::Dims;
use water_proto::link::{Link, LinkKind};
use water_proto::state::World;
use water_proto::{FULL, GATE_CAP, Mode, PUMP_RATE, ca};

fn basins() -> (World, u32, u32) {
    let w = gate_basins(Mode::Bodies);
    let a = w.dims.idx(19, 12, 11);
    let b = w.dims.idx(21, 12, 11);
    assert_ne!(w.owner[a as usize], 0);
    (w, a, b)
}

fn box_water(w: &World, xs: std::ops::RangeInclusive<u32>, ys: std::ops::RangeInclusive<u32>, zs: std::ops::RangeInclusive<u32>) -> u64 {
    let mut t = 0;
    for y in ys {
        for z in zs.clone() {
            for x in xs.clone() {
                let i = w.dims.idx(x, y, z);
                t += match w.owner[i as usize] {
                    0 => w.water[i as usize] as u64,
                    _ => 0,
                };
            }
        }
    }
    t
}

fn hash_no_step(w: &mut World) -> u64 {
    let s = w.step;
    w.step = 0;
    let h = w.hash();
    w.step = s;
    h
}

fn step_with(w: &mut World, edits: &[(u32, Edit)]) {
    let s = w.step;
    for &(at, e) in edits {
        if at == s {
            apply(w, e).unwrap();
        }
    }
    ca::step(w);
}

#[test]
fn gate_closed_holds() {
    let (mut w, a, b) = basins();
    let m0 = w.accounted();
    w.add_link(Link { a, b, kind: LinkKind::Gate { open: false } });
    let h0 = hash_no_step(&mut w);
    for _ in 0..50 {
        ca::step(&mut w);
    }
    assert_eq!(w.accounted(), m0);
    assert_eq!(hash_no_step(&mut w), h0);
}

#[test]
fn gate_open_moves_cap_per_step() {
    let (mut w, a, b) = basins();
    let m0 = w.accounted();
    let id = w.add_link(Link { a, b, kind: LinkKind::Gate { open: false } });
    let edits = [
        (10, Edit::SetLink(id, LinkKind::Gate { open: true })),
        (210, Edit::SetLink(id, LinkKind::Gate { open: false })),
    ];
    let mut slept = false;
    for _ in 0..3000 {
        step_with(&mut w, &edits);
        if w.step > 210 && w.active.count() == 0 && w.stats.link_moved_last_step == 0 {
            slept = true;
            break;
        }
    }
    assert!(slept, "never slept");
    assert_eq!(w.accounted(), m0);
    let moved = 200 * GATE_CAP;
    let in_b = box_water(&w, 21..=44, 4..=20, 4..=19)
        + w.bodies.values().filter(|bd| bd.lo[0] >= 21).map(|bd| bd.volume).sum::<u64>();
    assert_eq!(in_b, moved);
    let in_a: u64 = w.bodies.values().filter(|bd| bd.lo[0] < 21).map(|bd| bd.volume).sum();
    assert_eq!(in_a, 1536 * FULL as u64 - moved);
}

#[test]
fn gate_open_close_same_step_is_closed() {
    let (mut w1, a, b) = basins();
    let id = w1.add_link(Link { a, b, kind: LinkKind::Gate { open: false } });
    let (mut w2, a2, b2) = basins();
    w2.add_link(Link { a: a2, b: b2, kind: LinkKind::Gate { open: false } });
    let edits = [
        (10, Edit::SetLink(id, LinkKind::Gate { open: true })),
        (10, Edit::SetLink(id, LinkKind::Gate { open: false })),
    ];
    for _ in 0..30 {
        step_with(&mut w1, &edits);
        step_with(&mut w2, &[]);
    }
    assert_eq!(w1.hash(), w2.hash());
}

#[test]
fn scarce_source_is_shared_proportionally() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=20);
    let d = w.dims;
    let cells = [d.idx(10, 15, 10), d.idx(20, 15, 20), d.idx(28, 15, 28)];
    for &c in &cells {
        let (x, y, z) = d.xyz(c);
        carve(&mut w, x..=x, y..=y, z..=z);
    }
    finish(&mut w);
    w.add_link(Link { a: cells[0], b: cells[1], kind: LinkKind::Pump { rate: 1000 } });
    w.add_link(Link { a: cells[0], b: cells[2], kind: LinkKind::Pump { rate: 1000 } });
    apply(&mut w, Edit::Pour(cells[0], 1000)).unwrap();
    ca::step(&mut w);
    assert_eq!(w.accounted(), 1000);
    assert_eq!(w.water[cells[0] as usize], 0);
    assert_eq!(w.water[cells[1] as usize], 500);
    assert_eq!(w.water[cells[2] as usize], 500);
}

#[test]
fn pump_lifts_water_uphill() {
    let (mut w, a, _) = basins();
    let m0 = w.accounted();
    let out = w.dims.idx(50, 21, 50);
    w.add_link(Link { a, b: out, kind: LinkKind::Pump { rate: PUMP_RATE } });
    for _ in 0..8 {
        ca::step(&mut w);
    }
    assert_eq!(w.accounted(), m0);
    // the plain's surface is y = 21; both basins are carved only up to y = 20
    let outside = box_water(&w, 0..=63, 21..=63, 0..=63);
    assert_eq!(outside, 8 * PUMP_RATE as u64);
}

#[test]
fn pump_from_dry_source_moves_nothing() {
    let (mut w, _, b) = basins();
    let dry = w.dims.idx(30, 10, 5);
    let m0 = w.accounted();
    w.add_link(Link { a: dry, b, kind: LinkKind::Pump { rate: PUMP_RATE } });
    let h0 = hash_no_step(&mut w);
    for _ in 0..10 {
        ca::step(&mut w);
    }
    assert_eq!(w.accounted(), m0);
    assert_eq!(hash_no_step(&mut w), h0);
}

fn column_lake(xs: std::ops::RangeInclusive<u32>, pockets: &[u32]) -> World {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=20);
    carve(&mut w, xs.clone(), 12..=13, 5..=5);
    fill_water(&mut w, xs, 12..=13, 5..=5);
    for &x in pockets {
        carve(&mut w, x..=x, 15..=15, 10..=10);
    }
    finish(&mut w);
    w
}

#[test]
fn drained_top_layer_does_not_break_link_phase() {
    // two stacked cells; two pumps into one sealed pocket drain the top layer mid-phase
    let mut w = column_lake(5..=5, &[10]);
    let d = w.dims;
    let (top, pocket) = (d.idx(5, 13, 5), d.idx(10, 15, 10));
    assert_ne!(w.owner[top as usize], 0);
    let m0 = w.accounted();
    w.add_link(Link { a: top, b: pocket, kind: LinkKind::Pump { rate: 3000 } });
    w.add_link(Link { a: top, b: pocket, kind: LinkKind::Pump { rate: 3000 } });
    for _ in 0..10 {
        ca::step(&mut w);
        assert_eq!(w.accounted(), m0);
        assert!(water_proto::link::avail(&w, pocket) <= FULL as u64);
    }
    // (a full pocket may have seeded a one-cell body, so read it through avail)
    assert_eq!(water_proto::link::avail(&w, pocket), FULL as u64);
    let lake = w.accounted() - FULL as u64;
    assert_eq!(lake, FULL as u64);
}

#[test]
fn links_from_several_top_members_share_a_dropping_body() {
    // 4x1 lake, two layers. Links 1-4 (one per top member) take the whole top layer, which
    // drops after link 4; link 5 (from a member that just went dry) still takes from the body.
    let pockets = [10, 14, 18, 22, 26];
    let mut w = column_lake(4..=7, &[]);
    let d = w.dims;
    for &x in &pockets {
        carve(&mut w, x..=x, 15..=15, 10..=10);
    }
    let m0 = w.accounted();
    let src = [d.idx(4, 13, 5), d.idx(5, 13, 5), d.idx(6, 13, 5), d.idx(7, 13, 5), d.idx(4, 13, 5)];
    assert_ne!(w.owner[src[0] as usize], 0);
    for (k, &x) in pockets.iter().enumerate() {
        w.add_link(Link { a: src[k], b: d.idx(x, 15, 10), kind: LinkKind::Pump { rate: 4096 } });
    }
    ca::step(&mut w);
    assert_eq!(w.accounted(), m0);
    for &x in &pockets {
        assert_eq!(w.water[d.idx(x, 15, 10) as usize], FULL, "pocket {x}");
    }
    let left: u64 = w.bodies.values().map(|b| b.volume).sum::<u64>()
        + (4..=7).map(|x| w.water[d.idx(x, 13, 5) as usize] as u64 + w.water[d.idx(x, 12, 5) as usize] as u64).sum::<u64>();
    assert_eq!(left, 8 * FULL as u64 - 5 * FULL as u64);
}

#[test]
fn links_roll_back_exactly() {
    let (mut w, a, b) = basins();
    let id = w.add_link(Link { a, b, kind: LinkKind::Gate { open: false } });
    for _ in 0..3 {
        ca::step(&mut w);
    }
    let h0 = w.hash();
    w.begin_log();
    let extra = w.add_link(Link { a, b: w.dims.idx(50, 21, 50), kind: LinkKind::Pump { rate: PUMP_RATE } });
    apply(&mut w, Edit::SetLink(id, LinkKind::Gate { open: true })).unwrap();
    w.add_scooped(7);
    ca::step(&mut w);
    assert_eq!(extra, id + 1);
    assert_ne!(w.hash(), h0);
    let log = w.end_log();
    w.undo(log);
    assert_eq!(w.hash(), h0);
    assert_eq!(w.links.len(), 1);
    assert_eq!(apply(&mut w, Edit::SetLink(99, LinkKind::Gate { open: true })), Err(water_proto::edit::EditError::NoRoom));
}
