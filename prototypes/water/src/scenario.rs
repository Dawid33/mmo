//! The benchmark scenarios. Each is a 64³ world, built identically for both modes.

use crate::edit::{self, Edit};
use crate::fixture::{carve, fill_solid, fill_water, finish, gate_basins};
use crate::link::{Link, LinkKind};
use crate::{PUMP_RATE};
use crate::grid::Dims;
use crate::metrics::{dense_bytes, sparse_bytes, surface_spread};
use crate::state::{Stats, Undo, World};
use crate::{FULL, Mode, ca};

pub const NAMES: [&str; 10] = [
    "settled", "small_spill", "big_drain", "bucket", "film", "u_tube",
    "dam_breach", "gate_flood", "pumped_cistern", "scoop",
];
/// Full-grid mass checks cost O(grid); run one every this many steps (and at the end).
pub const MASS_CHECK_EVERY: u32 = 64;

pub struct Scenario {
    pub name: &'static str,
    pub world: World,
    /// (step, edit), applied at the start of that step.
    pub edits: Vec<(u32, Edit)>,
    pub max_steps: u32,
    pub stop_when_asleep: bool,
    pub initial_mass: u64,
}

pub struct Report {
    pub name: &'static str,
    pub mode: Mode,
    pub steps: u32,
    pub asleep_at: Option<u32>,
    /// Nanoseconds per `ca::step` (all zero on wasm).
    pub step_ns: Vec<u64>,
    pub undo_bytes: Vec<u32>,
    pub mass_ok: bool,
    pub edit_errors: u32,
    pub stats: Stats,
    pub final_hash: u64,
    pub bodies: usize,
    pub dense_bytes: usize,
    pub sparse_bytes: usize,
}

pub fn build(name: &str, mode: Mode) -> Scenario {
    let d = Dims::cube(64);
    let mut w = World::new(d, mode);
    let mut finished = false;
    let (name, edits, max_steps, stop): (&'static str, Vec<(u32, Edit)>, u32, bool) = match name {
        "settled" => {
            lake(&mut w);
            ("settled", vec![], 100, false)
        }
        "small_spill" => {
            lake(&mut w);
            carve(&mut w, 8..=9, 10..=10, 20..=24);
            let e = vec![(0, Edit::Remove(d.idx(10, 10, 22))), (0, Edit::Remove(d.idx(11, 10, 22)))];
            ("small_spill", e, 20_000, true)
        }
        "big_drain" => {
            fill_solid(&mut w, 0..=35);
            carve(&mut w, 4..=27, 20..=35, 4..=59); // lake A, floor y 20
            carve(&mut w, 30..=53, 4..=35, 4..=27); // basin B, floor y 4
            fill_water(&mut w, 4..=27, 20..=31, 4..=59);
            let e = [(28, 20), (29, 20), (28, 21), (29, 21)]
                .map(|(x, y)| (0, Edit::Remove(d.idx(x, y, 16))))
                .to_vec();
            ("big_drain", e, 60_000, true)
        }
        "bucket" => {
            fill_solid(&mut w, 0..=3);
            carve(&mut w, 30..=32, 1..=3, 30..=32);
            let e = (0..20).map(|k| (k, Edit::Pour(d.idx(31, 10, 31), FULL))).collect();
            ("bucket", e, 2_000, true)
        }
        "film" => {
            fill_solid(&mut w, 0..=3);
            let e = vec![(0, Edit::Pour(d.idx(20, 4, 20), FULL)), (0, Edit::Pour(d.idx(21, 4, 20), FULL))];
            ("film", e, 2_000, true)
        }
        "u_tube" => {
            fill_solid(&mut w, 0..=45);
            carve(&mut w, 20..=21, 4..=45, 30..=31);
            carve(&mut w, 40..=41, 4..=45, 30..=31);
            carve(&mut w, 22..=39, 4..=5, 30..=31);
            let e = (0..100u32)
                .map(|k| (k, Edit::Pour(d.idx(20 + k % 2, 44, 30 + (k / 2) % 2), FULL)))
                .collect();
            ("u_tube", e, 20_000, true)
        }
        "dam_breach" => {
            w = gate_basins(mode);
            finished = true;
            let mut e = Vec::new();
            for y in 12..=20 {
                for z in 11..=12 {
                    e.push((0, Edit::Remove(d.idx(20, y, z))));
                }
            }
            ("dam_breach", e, 20_000, true)
        }
        "gate_flood" => {
            w = gate_basins(mode);
            finished = true;
            let id = w.add_link(Link { a: d.idx(19, 12, 11), b: d.idx(21, 12, 11), kind: LinkKind::Gate { open: false } });
            let e = vec![
                (10, Edit::SetLink(id, LinkKind::Gate { open: true })),
                (210, Edit::SetLink(id, LinkKind::Gate { open: false })),
            ];
            ("gate_flood", e, 20_000, true)
        }
        "pumped_cistern" => {
            fill_solid(&mut w, 0..=14);
            carve(&mut w, 4..=11, 4..=14, 4..=11);
            fill_water(&mut w, 4..=11, 4..=8, 4..=11);
            carve(&mut w, 30..=32, 12..=14, 30..=32);
            finished = false;
            let id = w.add_link(Link { a: d.idx(4, 4, 4), b: d.idx(31, 14, 31), kind: LinkKind::Pump { rate: 0 } });
            let e = vec![
                (0, Edit::SetLink(id, LinkKind::Pump { rate: PUMP_RATE })),
                (72, Edit::SetLink(id, LinkKind::Pump { rate: 0 })),
            ];
            ("pumped_cistern", e, 20_000, true)
        }
        "scoop" => {
            w = gate_basins(mode);
            finished = true;
            ("scoop", vec![(0, Edit::Scoop(d.idx(19, 12, 11), FULL))], 20_000, true)
        }
        other => panic!("unknown scenario {other}"),
    };
    if !finished {
        finish(&mut w);
    }
    let initial_mass = w.mass();
    Scenario { name, world: w, edits, max_steps, stop_when_asleep: stop, initial_mass }
}

/// 40×40×8 m lake, 4 m freeboard.
fn lake(w: &mut World) {
    fill_solid(w, 0..=19);
    carve(w, 12..=51, 8..=19, 12..=51);
    fill_water(w, 12..=51, 8..=15, 12..=51);
}

/// Lake A's surface spread in `big_drain`.
pub fn drain_a_spread(w: &World) -> Option<u64> {
    surface_spread(w, 4..=27, 4..=59)
}

#[cfg(not(target_arch = "wasm32"))]
fn timed(f: impl FnOnce()) -> u64 {
    let t = std::time::Instant::now();
    f();
    t.elapsed().as_nanos() as u64
}

#[cfg(target_arch = "wasm32")]
fn timed(f: impl FnOnce()) -> u64 {
    f();
    0
}

fn step_timed(sc: &mut Scenario) -> (Vec<Undo>, u32, u64) {
    let Scenario { world: w, edits, .. } = sc;
    w.begin_log();
    let s = w.step;
    let mut errors = 0;
    for &(at, e) in edits.iter() {
        if at == s && edit::apply(w, e).is_err() {
            errors += 1;
        }
    }
    let ns = timed(|| ca::step(w));
    (w.end_log(), errors, ns)
}

/// One logged step (edits due now, then the water step). Returns the undo log and edit errors.
pub fn step_once(sc: &mut Scenario) -> (Vec<Undo>, u32) {
    let (log, errors, _) = step_timed(sc);
    (log, errors)
}

pub fn run(sc: &mut Scenario, mut probe: impl FnMut(&World)) -> Report {
    let last_edit = sc.edits.iter().map(|e| e.0).max();
    let mut r = Report {
        name: sc.name,
        mode: sc.world.mode,
        steps: 0,
        asleep_at: None,
        step_ns: Vec::new(),
        undo_bytes: Vec::new(),
        mass_ok: true,
        edit_errors: 0,
        stats: Stats::default(),
        final_hash: 0,
        bodies: 0,
        dense_bytes: 0,
        sparse_bytes: 0,
    };
    while sc.world.step < sc.max_steps {
        let (log, errors, ns) = step_timed(sc);
        r.edit_errors += errors;
        r.step_ns.push(ns);
        r.undo_bytes.push(log.iter().map(Undo::bytes).sum::<usize>() as u32);
        let s = sc.world.step;
        if s % MASS_CHECK_EVERY == 0 && sc.world.accounted() != sc.initial_mass + sc.world.poured {
            r.mass_ok = false;
        }
        probe(&sc.world);
        if sc.stop_when_asleep && last_edit.is_none_or(|e| s > e) && sc.world.active.count() == 0
            && sc.world.stats.link_moved_last_step == 0
            && sc.world.evap_due.is_empty()
        {
            r.asleep_at = Some(s);
            break;
        }
    }
    let w = &sc.world;
    r.mass_ok &= w.accounted() == sc.initial_mass + w.poured;
    r.steps = w.step;
    r.stats = w.stats;
    r.final_hash = w.hash();
    r.bodies = w.bodies.len();
    r.dense_bytes = dense_bytes(w);
    r.sparse_bytes = sparse_bytes(w);
    r
}
