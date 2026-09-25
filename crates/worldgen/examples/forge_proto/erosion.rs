//! Drainage routing and one step of landscape evolution.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::geology::Geology;
use crate::world::{Params, World};

/// Priority-Flood epsilon (m per meter of travel): the routing surface falls
/// at least this much toward the sea, so flats and filled lakes still drain.
pub const FLOOD_EPSILON: f64 = 1.0e-5;
/// Critical drainage area (`Knobs::hillslope_area_m2`): below it a cell is hillslope, not channel,
/// and incision uses this area instead of its own. Without it relief depends
/// on the grid — steady-state slope goes as 1/sqrt(A) and the smallest A IS
/// one cell, so 8 m cells built peaks 45% taller than 16 m cells.
/// It is an AREA, scaled by the local rain: a dry flank starts channels at
/// the same spacing as a wet one (it is just as dissected), its streams are
/// only weaker. Comparing rain-weighted discharge to a fixed threshold instead
/// left the whole lee side of the ranges smooth, like clay.

pub const NO_RECEIVER: u32 = u32::MAX;

/// Depression-free routing over the land, from one Priority-Flood pass.
pub struct Routing {
    /// Flooded surface: `h` raised so every land cell has a falling path to
    /// sea. It is also the water surface: flat over lakes, the bed elsewhere.
    pub filled: Vec<f64>,
    /// Downstream neighbor (steepest descent on `filled`); NO_RECEIVER at sea.
    pub receiver: Vec<u32>,
    pub receiver_dist: Vec<f64>,
    /// Land cells, downstream-first: a cell always comes after its receiver.
    pub order: Vec<u32>,
}

/// Priority-Flood(+epsilon) seeded from the coast (Barnes et al. 2014). Cells
/// pop in nondecreasing flooded height, so pop order is a valid
/// downstream-first order for ANY receiver with a strictly lower flooded height.
pub fn route(w: &World, epsilon: f64) -> Routing {
    let cells = w.n * w.n;
    let mut filled = w.h.clone();
    let mut closed = w.ocean.clone();
    let mut receiver = vec![NO_RECEIVER; cells];
    let mut receiver_dist = vec![0.0; cells];
    let mut order = Vec::with_capacity(cells);
    // Key = flooded height as order-preserving bits (offset keeps it positive).
    let key = |height: f64| (height + 100_000.0).to_bits();
    let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
    for i in 0..cells {
        if w.ocean[i] && w.neighbors(i).any(|(nb, _)| !w.ocean[nb]) {
            // Sea level, not the sea floor, is the base level.
            filled[i] = 0.0;
            heap.push(Reverse((key(0.0), i as u32)));
        }
    }
    while let Some(Reverse((_, c))) = heap.pop() {
        let c = c as usize;
        if !w.ocean[c] {
            order.push(c as u32);
        }
        for (nb, dist) in w.neighbors(c) {
            if closed[nb] {
                continue;
            }
            closed[nb] = true;
            filled[nb] = w.h[nb].max(filled[c] + epsilon * dist);
            receiver[nb] = c as u32;
            receiver_dist[nb] = dist;
            heap.push(Reverse((key(filled[nb]), nb as u32)));
        }
    }
    // The flood's receiver is the LOWEST neighbor; prefer the STEEPEST one
    // (diagonals are longer), which is still strictly lower, so order holds.
    if epsilon > 0.0 {
        for &i in &order {
            let i = i as usize;
            let mut best = (filled[i] - filled[receiver[i] as usize]) / receiver_dist[i];
            for (nb, dist) in w.neighbors(i) {
                let slope = (filled[i] - filled[nb]) / dist;
                if slope > best {
                    best = slope;
                    receiver[i] = nb as u32;
                    receiver_dist[i] = dist;
                }
            }
        }
    }
    Routing { filled, receiver, receiver_dist, order }
}

/// Discharge through each land cell, itself included: drained land area (m^2)
/// weighted by relative rainfall — "m^2 of average rain".
pub fn discharge(w: &World, r: &Routing, rain: &[f64]) -> Vec<f64> {
    let mut q = vec![0.0; w.n * w.n];
    for &i in &r.order {
        q[i as usize] = w.dx * w.dx * rain[i as usize];
    }
    for &i in r.order.iter().rev() {
        let rc = r.receiver[i as usize] as usize;
        if !w.ocean[rc] {
            q[rc] += q[i as usize];
        }
    }
    q
}

/// One time step: uplift + implicit stream-power incision dh/dt = U - K*sqrt(Q)*S
/// along the routing tree (Braun & Willett 2013, n = 1), sediment into
/// depressions, then hillslope diffusion. Returns the cells under depressions.
pub fn step(w: &mut World, p: &Params, geo: &Geology, rain: &[f64], scratch: &mut Vec<f64>) -> usize {
    let routing = route(w, FLOOD_EPSILON);
    let q = discharge(w, &routing, rain);
    let (k, dt) = (&p.knobs, p.knobs.dt_years);
    let mut drowned = 0;
    for &i in &routing.order {
        let i = i as usize;
        let depth = routing.filled[i] - w.h[i];
        if depth > 1.0e-3 {
            // Under a lake: no incision; it silts up instead.
            drowned += 1;
            w.h[i] += dt * w.uplift[i] + k.depression_fill * depth;
            w.uplifted[i] += dt * w.uplift[i];
            continue;
        }
        let rc = routing.receiver[i] as usize;
        let base = if w.ocean[rc] { 0.0 } else { w.h[rc] }; // already updated: downstream-first
        let erodibility = k.erodibility * geo.erodibility(w, i);
        let f = dt * erodibility * q[i].max(k.hillslope_area_m2 * rain[i]).sqrt() / routing.receiver_dist[i];
        w.h[i] = ((w.h[i] + dt * w.uplift[i] + f * base) / (1.0 + f)).max(0.05);
        w.uplifted[i] += dt * w.uplift[i];
    }

    // Explicit diffusion, sub-stepped to stay stable (coefficient <= 0.2).
    let coeff = k.diffusivity * dt / (w.dx * w.dx);
    let substeps = (coeff / 0.2).ceil().max(1.0) as usize;
    let c = coeff / substeps as f64;
    let n = w.n;
    for _ in 0..substeps {
        scratch.clone_from(&w.h);
        for z in 1..n - 1 {
            for x in 1..n - 1 {
                let i = z * n + x;
                if w.ocean[i] {
                    continue;
                }
                // The sea holds sea level at the shore, not its floor depth.
                let s = |j: usize| if w.ocean[j] { 0.0 } else { scratch[j] };
                let lap = s(i - 1) + s(i + 1) + s(i - n) + s(i + n) - 4.0 * scratch[i];
                w.h[i] = (scratch[i] + c * lap).max(0.05);
            }
        }
    }
    drowned
}
