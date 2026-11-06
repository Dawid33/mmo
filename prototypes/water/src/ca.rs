//! One water step. The awake CA cells are visited by checkerboard colour (this step's parity
//! first). Per colour group: vertical edges, then all CA<->CA sideways wants computed from the
//! same state, capped per donor and per receiver, then applied (the amounts do not depend on
//! visit order, except which donor gets a receiver's leftover units, which rotates with the
//! step); then body edges, absorb/seed and sleep per cell. Afterwards (bodies mode) requested
//! expansions run, bodies whose level rose wake their shore, and the step advances.

use crate::body::{self, Body};
use crate::grid::{ALL, Dir, horizontal_order, parity};
use crate::state::{BodyId, NO_BODY, World};
use crate::{ABSORB_TOL, FILM, FULL, MAX_FLOW, Mode, SEED_QUIET, SLEEP_STEPS};

pub fn step(w: &mut World) {
    let s = w.step;
    w.expand_requests.clear();
    let order = horizontal_order(s);
    let all = w.active.ones();
    let dims = w.dims;
    // checkerboard: cells of this step's parity first, then the rest, each group ascending.
    // Same-colour cells are never horizontal neighbours, so a group is processed order-free.
    for colour in [s % 2, (s + 1) % 2] {
        let group: Vec<u32> = all.iter().copied().filter(|&c| parity(dims, c) == colour).collect();
        // (A) vertical edges
        let mut live = Vec::with_capacity(group.len());
        for &c in &group {
            if !w.is_ca(c) {
                w.set_active(c, false);
                continue;
            }
            w.stats.cells_visited += 1;
            vertical(w, c);
            live.push(c);
        }
        // (B) wants from the state after (A), (C) caps, (D) apply
        let mut moves: Vec<(u32, u32, u16)> = Vec::new();
        for &c in &live {
            if w.is_ca(c) {
                donor_wants(w, c, &mut moves);
            }
        }
        cap_and_apply(w, moves);
        for c in live {
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
    }
    crate::link::flux_all(w);
    if w.mode == Mode::Bodies {
        after_step(w);
    }
    w.set_step(s + 1);
}

pub(crate) fn wake(w: &mut World, c: u32) {
    w.set_active(c, true);
    w.touch(c);
}

/// A cell that lost water may now sit below a neighbour that is asleep: sideways flow is
/// donor-driven, so wake every neighbour that holds water (it re-checks its wants).
pub(crate) fn wake_donors(w: &mut World, c: u32) {
    for d in crate::grid::HORIZONTAL {
        if let Some(n) = w.dims.step(c, d) {
            if w.is_ca(n) && w.water[n as usize] > 0 {
                wake(w, n);
            }
        }
    }
}

fn move_ca(w: &mut World, from: u32, to: u32, q: u16) {
    let (a, b) = (w.water[from as usize], w.water[to as usize]);
    let a2 = a.checked_sub(q).expect("move_ca: donor underflow");
    w.set_water(from, a2);
    w.set_water(to, b + q);
    wake(w, from);
    wake(w, to);
    wake_donors(w, from);
    w.stats.transfers += 1;
}

fn ca_to_body(w: &mut World, from: u32, id: BodyId, q: u16) {
    let a = w.water[from as usize];
    w.set_water(from, a.checked_sub(q).expect("ca_to_body: donor underflow"));
    body::add_volume(w, id, q as u64);
    wake(w, from);
    wake_donors(w, from);
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
        NO_BODY => {} // CA<->CA sideways exchange is `split_out`
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

/// Amount cell `c` (as donor) sends to CA neighbour `n` under the per-edge rule.
fn want(w: &World, c: u32, n: u32) -> u16 {
    let (wc, wn) = (w.water[c as usize], w.water[n as usize]);
    if wc <= wn || !w.supported(c) {
        return 0; // not the higher cell, or a donor in mid-air only falls
    }
    let diff = wc - wn;
    let q = if !w.supported(n) {
        diff.div_ceil(2) // over an edge: pour, rounding up so films drain off
    } else if wc <= FILM || diff < 2 {
        0 // film rule
    } else {
        diff / 2
    };
    q.min(MAX_FLOW)
}

/// Collect cell `c`'s wants toward its CA neighbours (nothing is applied).
fn donor_wants(w: &World, c: u32, out: &mut Vec<(u32, u32, u16)>) {
    let dims = w.dims;
    let start = out.len();
    let mut sum = 0u64;
    for d in crate::grid::HORIZONTAL {
        let Some(n) = dims.step(c, d) else { continue };
        if w.solid.get(n) || w.owner[n as usize] != NO_BODY {
            continue;
        }
        let q = want(w, c, n);
        if q > 0 {
            out.push((c, n, q));
            sum += q as u64;
        }
    }
    let avail = w.water[c as usize] as u64;
    if sum > avail {
        for m in &mut out[start..] {
            m.2 = (m.2 as u64 * avail / sum) as u16;
        }
    }
}

/// Receiver caps, then apply in a fixed order (the amounts do not depend on the order).
fn cap_and_apply(w: &mut World, mut moves: Vec<(u32, u32, u16)>) {
    // largest single want per receiver
    let mut incoming: std::collections::BTreeMap<u32, (u64, u64)> = Default::default();
    for &(_, n, q) in &moves {
        let e = incoming.entry(n).or_default();
        e.0 += q as u64;
        e.1 = e.1.max(q as u64);
    }
    // room cap: proportional floor, never overfills
    for m in &mut moves {
        let (tot, _) = incoming[&m.1];
        let room = (FULL - w.water[m.1 as usize]) as u64;
        if tot > room {
            m.2 = (m.2 as u64 * room / tot) as u16;
        }
    }
    let mut after: std::collections::BTreeMap<u32, u64> = Default::default();
    for &(_, n, q) in &moves {
        *after.entry(n).or_default() += q as u64;
    }
    // midpoint cap: a receiver takes in at most the biggest single want (never past the midpoint
    // with its highest donor), or simultaneous donors overshoot and ping-pong. Floor shares; the
    // leftover units go one each to donors that still move something (so floor(q*b/t)+1 <= q),
    // starting at a donor rotated by the step so no fixed index is favoured.
    let mut left: std::collections::BTreeMap<u32, (u64, Vec<usize>)> = Default::default();
    for (i, m) in moves.iter_mut().enumerate() {
        let (_, biggest) = incoming[&m.1];
        let tot = after[&m.1];
        if tot > biggest && m.2 > 0 {
            m.2 = (m.2 as u64 * biggest / tot) as u16;
            let e = left.entry(m.1).or_insert((biggest, Vec::new()));
            e.0 -= m.2 as u64;
            e.1.push(i);
        }
    }
    let rot = w.step as usize;
    for (_, (mut l, idx)) in left {
        let k = idx.len();
        for j in 0..k {
            if l == 0 {
                break;
            }
            moves[idx[(j + rot) % k]].2 += 1;
            l -= 1;
        }
        debug_assert_eq!(l, 0);
    }
    for (from, to, q) in moves {
        if q > 0 {
            move_ca(w, from, to, q);
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
    // c joined below the top with open ground above it: the upper layers no longer
    // describe the connected container (e.g. the far shaft of a U-tube), so re-derive them
    if w.bodies.contains_key(&keep) && y < w.bodies[&keep].top {
        if let Some(u) = dims.step(c, Dir::Up) {
            if !w.solid.get(u) && w.owner[u as usize] != keep {
                w.stats.relevels += 1;
                body::relevel(w, keep, y);
            }
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
