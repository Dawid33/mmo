//! Two worlds joined at a seam, as two regions on different threads would be. A's face
//! `x = dims.x - 1` touches B's face `x = 0`; face cells are indexed by `(y, z)`.
//!
//! Each side sends only its own water: it takes it at once and posts an arrival that lands
//! `SEAM_LAG_STEPS` later. The heads (and rooms) a side sees across the seam are the other
//! side's as reported at the end of the previous step. An arrival that does not fit bounces
//! back to its sender, due next step; water is never dropped.
//!
//! One pair step: (1) deliver due arrivals in in-flight order, (2) `ca::step` A then B,
//! (3) per face, each side sends `min(SEAM_CAP, (own - seen_head) / SEAM_GAIN_DIV, seen_room)`
//! when positive, capped by what its cell or body holds (faces of one body share its volume
//! proportionally, then take from the planned body by id), (4) refresh both seen vectors.

use std::collections::BTreeMap;

use crate::fixture::{carve, fill_solid, fill_water, finish};
use crate::grid::Dims;
use crate::link::{avail, give, head, room, source_key, take_planned};
use crate::state::{Fnv, NO_BODY, Undo, World};
use crate::{Mode, SEAM_CAP, SEAM_LAG_STEPS, ca};

/// Divisor of the head difference a seam face sends per step. 2 = the CA's own
/// equalising rule (each side ends half way); see the Task 10 report for the damping test.
pub const SEAM_GAIN_DIV: u64 = 2;

pub const NAMES: [&str; 3] = ["seam_drain", "seam_lake", "seam_bounce"];
/// Step budget for running a seam scenario to rest (goldens, the CLI table).
pub const SEAM_MAX_STEPS: u32 = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival {
    /// Destination: B's face (true) or A's face (false).
    pub to_b: bool,
    pub y: u32,
    pub z: u32,
    pub amount: u64,
    /// Pair step (the worlds' `step` before stepping) at which it is delivered.
    pub due: u32,
}

/// What a face cell reports across the seam: `(head, room)`, `None` when solid.
pub type Seen = Option<(u64, u64)>;

pub struct Pair {
    pub a: World,
    pub b: World,
    pub inflight: Vec<Arrival>,
    /// B's face as A last saw it, indexed `y * dims.z + z`.
    pub seen_by_a: Vec<Seen>,
    /// A's face as B last saw it.
    pub seen_by_b: Vec<Seen>,
    /// Units that arrived to no room and were sent back.
    pub bounced: u64,
    /// Total mass at construction (both worlds; nothing in flight).
    pub initial: u64,
    /// A→B minus B→A sent in the last step (diagnostics, restored by undo, not hashed).
    last_flux: i64,
}

pub struct PairUndo {
    a: Vec<Undo>,
    b: Vec<Undo>,
    inflight: Vec<Arrival>,
    seen_by_a: Vec<Seen>,
    seen_by_b: Vec<Seen>,
    bounced: u64,
    last_flux: i64,
}

fn face_cell(w: &World, a_side: bool, y: u32, z: u32) -> u32 {
    let x = if a_side { w.dims.x - 1 } else { 0 };
    w.dims.idx(x, y, z)
}

/// One side's face as it reports it: each cell's head and room. A body's room is shared
/// evenly among its face cells (floored), so the other side's faces together never offer
/// more than the body can take; only the report's one-step staleness can then bounce.
fn report(w: &World, a_side: bool) -> Vec<Seen> {
    let mut faces: BTreeMap<u32, u64> = BTreeMap::new(); // body -> face cells
    for y in 0..w.dims.y {
        for z in 0..w.dims.z {
            let c = face_cell(w, a_side, y, z);
            if !w.solid.get(c) && w.owner[c as usize] != NO_BODY {
                *faces.entry(w.owner[c as usize]).or_default() += 1;
            }
        }
    }
    let mut v = Vec::with_capacity((w.dims.y * w.dims.z) as usize);
    for y in 0..w.dims.y {
        for z in 0..w.dims.z {
            let c = face_cell(w, a_side, y, z);
            let share = match w.owner[c as usize] {
                NO_BODY => 1,
                id => faces[&id],
            };
            v.push(head(w, c).map(|h| (h, room(w, c) / share)));
        }
    }
    v
}

impl Pair {
    pub fn new(a: World, b: World) -> Pair {
        assert_eq!((a.dims.y, a.dims.z), (b.dims.y, b.dims.z), "seam faces must match");
        assert_eq!(a.step, b.step, "seam worlds must be at the same step");
        let initial = a.accounted() + b.accounted();
        let seen_by_a = report(&b, false);
        let seen_by_b = report(&a, true);
        Pair { a, b, inflight: Vec::new(), seen_by_a, seen_by_b, bounced: 0, initial, last_flux: 0 }
    }

    pub fn step(&mut self) -> PairUndo {
        let mut u = PairUndo {
            a: Vec::new(),
            b: Vec::new(),
            inflight: self.inflight.clone(),
            seen_by_a: self.seen_by_a.clone(),
            seen_by_b: self.seen_by_b.clone(),
            bounced: self.bounced,
            last_flux: self.last_flux,
        };
        let s = self.a.step;
        self.a.begin_log();
        self.b.begin_log();
        // 1: deliver
        let (due, rest): (Vec<Arrival>, Vec<Arrival>) = std::mem::take(&mut self.inflight).into_iter().partition(|r| r.due == s);
        self.inflight = rest;
        for r in due {
            let w = if r.to_b { &mut self.b } else { &mut self.a };
            let c = face_cell(w, !r.to_b, r.y, r.z);
            let left = r.amount - give(w, c, r.amount);
            if left > 0 {
                self.bounced += left;
                self.inflight.push(Arrival { to_b: !r.to_b, amount: left, due: s + 1, ..r });
            }
        }
        // 2: step both worlds
        ca::step(&mut self.a);
        ca::step(&mut self.b);
        // 3: outflow
        let sent_a = send(&mut self.a, true, &self.seen_by_a, s, &mut self.inflight);
        let sent_b = send(&mut self.b, false, &self.seen_by_b, s, &mut self.inflight);
        self.last_flux = sent_a as i64 - sent_b as i64;
        // 4: report heads
        self.seen_by_a = report(&self.b, false);
        self.seen_by_b = report(&self.a, true);
        u.a = self.a.end_log();
        u.b = self.b.end_log();
        u
    }

    pub fn undo(&mut self, u: PairUndo) {
        self.a.undo(u.a);
        self.b.undo(u.b);
        self.inflight = u.inflight;
        self.seen_by_a = u.seen_by_a;
        self.seen_by_b = u.seen_by_b;
        self.bounced = u.bounced;
        self.last_flux = u.last_flux;
    }

    pub fn hash(&self) -> u64 {
        let mut h = Fnv::new();
        h.u64(self.a.hash());
        h.u64(self.b.hash());
        h.u64(self.bounced);
        h.u64(self.inflight.len() as u64);
        for r in &self.inflight {
            h.u32(r.to_b as u32);
            h.u32(r.y);
            h.u32(r.z);
            h.u64(r.amount);
            h.u32(r.due);
        }
        for v in self.seen_by_a.iter().chain(&self.seen_by_b) {
            match *v {
                None => h.u32(0),
                Some((hd, rm)) => {
                    h.u32(1);
                    h.u64(hd);
                    h.u64(rm);
                }
            }
        }
        h.finish()
    }

    /// Both worlds' ledgers plus the water in flight.
    pub fn accounted(&self) -> u64 {
        self.a.accounted() + self.b.accounted() + self.inflight.iter().map(|r| r.amount).sum::<u64>()
    }

    /// Units sent A→B minus B→A in the last step.
    pub fn net_flux_last_step(&self) -> i64 {
        self.last_flux
    }

    /// Both worlds asleep (no awake cell, no link moved) and nothing in flight.
    pub fn settled(&self) -> bool {
        self.inflight.is_empty()
            && [&self.a, &self.b].iter().all(|w| w.active.count() == 0 && w.stats.link_moved_last_step == 0)
    }

    /// Step until settled or `max_steps` pair steps; returns the steps taken.
    pub fn run(&mut self, max_steps: u32) -> u32 {
        for k in 0..max_steps {
            self.step();
            if self.settled() {
                return k + 1;
            }
        }
        max_steps
    }
}

/// Phase 3 for one side: plan every face's send from its own head now and the stale seen
/// entry, share a source's volume among its faces, take by the planned key, post arrivals.
fn send(w: &mut World, a_side: bool, seen: &[Seen], s: u32, inflight: &mut Vec<Arrival>) -> u64 {
    let dz = w.dims.z;
    let mut wants: Vec<(u32, u32, u32, u64, (u8, u32))> = Vec::new(); // (y, z, cell, q, key)
    for y in 0..w.dims.y {
        for z in 0..dz {
            let Some((sh, sr)) = seen[(y * dz + z) as usize] else { continue };
            let c = face_cell(w, a_side, y, z);
            let Some(own) = head(w, c) else { continue };
            if own <= sh {
                continue;
            }
            let q = SEAM_CAP.min((own - sh) / SEAM_GAIN_DIV).min(sr).min(avail(w, c));
            if q > 0 {
                wants.push((y, z, c, q, source_key(w, c)));
            }
        }
    }
    // faces of one body (or one cell) share what it holds
    let mut out: BTreeMap<(u8, u32), (u64, u64)> = BTreeMap::new(); // key -> (asked, have)
    for &(_, _, c, q, k) in &wants {
        let have = avail(w, c);
        out.entry(k).or_insert((0, have)).0 += q;
    }
    let mut sent = 0;
    for (y, z, c, q, k) in wants {
        let (total, have) = out[&k];
        let q = if total > have { q * have / total } else { q };
        if q == 0 {
            continue;
        }
        let got = take_planned(w, k, c, q);
        if got > 0 {
            inflight.push(Arrival { to_b: a_side, y, z, amount: got, due: s + SEAM_LAG_STEPS });
            sent += got;
        }
    }
    sent
}

fn world(mode: Mode) -> World {
    let mut w = World::new(Dims::cube(32), mode);
    fill_solid(&mut w, 0..=20);
    w
}

/// Side A of every seam scenario: a 28×16 lake, floor y 12, 6 m deep (2688 m³), touching the seam.
pub fn drain_a(mode: Mode) -> World {
    let mut w = world(mode);
    carve(&mut w, 4..=31, 12..=20, 8..=23);
    fill_water(&mut w, 4..=31, 12..=17, 8..=23);
    finish(&mut w);
    w
}

pub fn build(name: &str) -> Pair {
    build_in(name, Mode::Bodies)
}

pub fn build_in(name: &str, mode: Mode) -> Pair {
    let mut b = world(mode);
    match name {
        // a dry, deep basin: floor y 4, 448 cells per layer
        "seam_drain" => carve(&mut b, 0..=27, 4..=20, 8..=23),
        // a dry basin level with A's floor
        "seam_lake" => carve(&mut b, 0..=27, 12..=20, 8..=23),
        // solid but for one sealed cell on the face
        "seam_bounce" => {
            fill_solid(&mut b, 0..=31);
            carve(&mut b, 0..=0, 13..=13, 12..=12);
        }
        other => panic!("unknown seam scenario {other}"),
    }
    finish(&mut b);
    Pair::new(drain_a(mode), b)
}
