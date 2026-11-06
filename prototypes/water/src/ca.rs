//! One water step. Every awake CA cell settles its six edges once, in ascending
//! index order. Then (bodies mode) cells are absorbed/seeded, requested
//! expansions run, bodies whose level rose wake their shore, and the step advances.

use crate::body::{self, Body};
use crate::grid::{ALL, Dir, horizontal_order};
use crate::state::{BodyId, NO_BODY, World};
use crate::{ABSORB_TOL, FILM, FULL, MAX_FLOW, Mode, SEED_QUIET, SLEEP_STEPS};

pub fn step(w: &mut World) {
    let s = w.step;
    w.expand_requests.clear();
    let order = horizontal_order(s);
    for c in w.active.ones() {
        if !w.is_ca(c) {
            w.set_active(c, false);
            continue;
        }
        w.stats.cells_visited += 1;
        vertical(w, c);
        for d in order {
            if w.is_ca(c) {
                horizontal(w, c, d);
            }
        }
        if w.mode == Mode::Bodies && w.is_ca(c) {
            absorb_or_seed(w, c);
        }
        if w.is_ca(c) && s >= w.last_touch[c as usize] + SLEEP_STEPS {
            w.set_active(c, false);
        }
    }
    if w.mode == Mode::Bodies {
        after_step(w);
    }
    w.set_step(s + 1);
}

fn wake(w: &mut World, c: u32) {
    w.set_active(c, true);
    w.touch(c);
}

fn move_ca(w: &mut World, from: u32, to: u32, q: u16) {
    let (a, b) = (w.water[from as usize], w.water[to as usize]);
    w.set_water(from, a - q);
    w.set_water(to, b + q);
    wake(w, from);
    wake(w, to);
    w.stats.transfers += 1;
}

fn ca_to_body(w: &mut World, from: u32, id: BodyId, q: u16) {
    let a = w.water[from as usize];
    w.set_water(from, a - q);
    body::add_volume(w, id, q as u64);
    wake(w, from);
    w.stats.transfers += 1;
}

fn body_to_ca(w: &mut World, id: BodyId, to: u32, q: u16) {
    body::remove_volume(w, id, q as u64);
    let b = w.water[to as usize];
    w.set_water(to, b + q);
    wake(w, to);
    w.stats.transfers += 1;
}

fn vertical(w: &mut World, c: u32) {
    let dims = w.dims;
    // c falls into the cell below
    if let Some(b) = dims.step(c, Dir::Down) {
        let wc = w.water[c as usize];
        if wc > 0 && !w.solid.get(b) {
            match w.owner[b as usize] {
                NO_BODY => {
                    let q = wc.min(FULL - w.water[b as usize]);
                    if q > 0 {
                        move_ca(w, c, b, q);
                    }
                }
                id => {
                    let (top, room) = {
                        let bd = &w.bodies[&id];
                        (bd.top, bd.room())
                    };
                    if dims.y_of(b) == top {
                        if room > 0 {
                            ca_to_body(w, c, id, (wc as u64).min(room) as u16);
                        } else {
                            w.expand_requests.push(id);
                        }
                    }
                }
            }
        }
    }
    // the cell above falls into c
    if let Some(a) = dims.step(c, Dir::Up) {
        if !w.solid.get(a) && w.is_ca(c) {
            let room = FULL - w.water[c as usize];
            match w.owner[a as usize] {
                NO_BODY => {
                    let q = w.water[a as usize].min(room);
                    if q > 0 {
                        move_ca(w, a, c, q);
                    }
                }
                id => {
                    let (v, vol) = {
                        let bd = &w.bodies[&id];
                        (bd.outflow_at(dims.y_of(a)), bd.volume)
                    };
                    let q = (v.min(room).min(MAX_FLOW) as u64).min(vol) as u16;
                    if q > 0 {
                        body_to_ca(w, id, c, q);
                    }
                }
            }
        }
    }
}

fn horizontal(w: &mut World, c: u32, d: Dir) {
    let dims = w.dims;
    let Some(n) = dims.step(c, d) else { return };
    if w.solid.get(n) {
        return;
    }
    let wc = w.water[c as usize];
    match w.owner[n as usize] {
        NO_BODY => {
            let wn = w.water[n as usize];
            let (hi, lo, whi, wlo) = if wc >= wn { (c, n, wc, wn) } else { (n, c, wn, wc) };
            let diff = whi - wlo;
            let q = if !w.supported(lo) {
                diff.div_ceil(2) // over an edge: pour, rounding up so films drain off
            } else if whi <= FILM || diff < 2 {
                0 // film rule
            } else {
                diff / 2
            };
            let q = q.min(MAX_FLOW);
            if q > 0 {
                move_ca(w, hi, lo, q);
            }
        }
        id => {
            let y = dims.y_of(c);
            let (v, top, room, vol) = {
                let b = &w.bodies[&id];
                (b.virtual_at(y), b.top, b.room(), b.volume)
            };
            let vout = w.bodies[&id].outflow_at(y);
            if vout > wc + ABSORB_TOL {
                let q = ((vout - wc) / 2).min(MAX_FLOW).min(FULL - wc);
                let q = (q as u64).min(vol) as u16;
                if q > 0 {
                    body_to_ca(w, id, c, q);
                }
            } else if wc > v + ABSORB_TOL && y == top {
                let q = (((wc - v) / 2) as u64).min(MAX_FLOW as u64).min(room) as u16;
                if q > 0 {
                    ca_to_body(w, c, id, q);
                }
            }
        }
    }
}

fn level_with(b: &Body, y: u32, wc: u16) -> bool {
    if y < b.top { FULL - wc <= ABSORB_TOL } else { wc.abs_diff(b.virtual_at(y)) <= ABSORB_TOL }
}

fn absorb_or_seed(w: &mut World, c: u32) {
    let dims = w.dims;
    let wc = w.water[c as usize];
    if wc == 0 || !body::on_solid_or_member(w, c) {
        return;
    }
    let y = dims.y_of(c);
    let mut touching: Vec<BodyId> = Vec::new();
    for d in ALL {
        if let Some(n) = dims.step(c, d) {
            let o = w.owner[n as usize];
            if o != NO_BODY && !touching.contains(&o) && y <= w.bodies[&o].top {
                touching.push(o);
            }
        }
    }
    if touching.is_empty() {
        if wc == FULL && body::on_solid(w, c) && w.step >= w.last_touch[c as usize] + SEED_QUIET {
            body::seed(w, c);
        }
        return;
    }
    // c joins only if it is level with every body it touches; otherwise it is a pipe
    if !touching.iter().all(|o| level_with(&w.bodies[o], y, wc)) {
        return;
    }
    touching.sort_unstable();
    body::add_member(w, touching[0], c);
    w.stats.absorbs += 1;
    let mut keep = touching[0];
    for &o in &touching[1..] {
        if w.bodies.contains_key(&o) && w.bodies.contains_key(&keep) && o != keep {
            keep = body::merge(w, keep, o);
        }
    }
}

fn after_step(w: &mut World) {
    let mut req = std::mem::take(&mut w.expand_requests);
    req.sort_unstable();
    req.dedup();
    for id in req {
        if w.bodies.contains_key(&id) {
            body::try_expand(w, id);
        }
    }
    // a rising level can start flow at spill edges and openings
    let touched = std::mem::take(&mut w.touched);
    let mut rose: Vec<BodyId> = touched
        .iter()
        .filter(|(id, v0)| w.bodies.get(id).is_some_and(|b| b.volume > *v0))
        .map(|t| t.0)
        .collect();
    rose.sort_unstable();
    for id in rose {
        let shore = w.bodies[&id].shore.clone();
        for s in shore {
            if w.is_ca(s) {
                wake(w, s);
            }
        }
    }
}
