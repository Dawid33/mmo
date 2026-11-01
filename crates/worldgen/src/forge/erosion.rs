//! Drainage routing and one step of landscape evolution.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::forge::geology::Geology;
use crate::forge::world::{ForgeParams, World};

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
#[derive(Default)]
pub struct Routing {
    /// Flooded surface: `h` raised so every land cell has a falling path to
    /// sea. It is also the water surface: flat over lakes, the bed elsewhere.
    pub filled: Vec<f64>,
    /// Downstream neighbor (steepest descent on `filled`); NO_RECEIVER at sea.
    pub receiver: Vec<u32>,
    pub receiver_dist: Vec<f64>,
    /// Land cells, downstream-first: a cell always comes after its receiver.
    pub order: Vec<u32>,
    /// Priority-Flood working set, kept so `route_into` reuses its capacity.
    closed: Vec<bool>,
    heap: BinaryHeap<Reverse<(u64, u32)>>,
}

/// Every per-iteration buffer of `step`, reused across iterations.
#[derive(Default)]
pub struct StepScratch {
    routing: Routing,
    /// Discharge per cell.
    q: Vec<f64>,
    /// Copy of `h` for the diffusion sub-steps.
    diffuse: Vec<f64>,
    /// `routing_fast::route_fast`'s working buffers.
    pub(super) fast: crate::forge::routing_fast::FastScratch,
}

/// Priority-Flood(+epsilon) seeded from the coast (Barnes et al. 2014). Cells
/// pop in nondecreasing flooded height, so pop order is a valid
/// downstream-first order for ANY receiver with a strictly lower flooded height.
pub fn route(w: &World, epsilon: f64) -> Routing {
    let mut out = Routing::default();
    route_into(w, epsilon, &mut out);
    out
}

/// `route` into reusable buffers (cleared and resized, not reallocated).
pub fn route_into(w: &World, epsilon: f64, out: &mut Routing) {
    let cells = w.n * w.n;
    let Routing { filled, receiver, receiver_dist, order, closed, heap } = out;
    filled.clear();
    filled.extend_from_slice(&w.h);
    closed.clear();
    closed.extend_from_slice(&w.ocean);
    receiver.clear();
    receiver.resize(cells, NO_RECEIVER);
    receiver_dist.clear();
    receiver_dist.resize(cells, 0.0);
    order.clear();
    order.reserve(cells);
    heap.clear();
    let key = |height: f64| (height + 100_000.0).to_bits();
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
        for &i in order.iter() {
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
}

/// Discharge through each land cell, itself included: drained land area (m^2)
/// weighted by relative rainfall — "m^2 of average rain".
pub fn discharge(w: &World, r: &Routing, rain: &[f64]) -> Vec<f64> {
    let mut q = Vec::new();
    discharge_into(w, r, rain, &mut q);
    q
}

/// `discharge` into a reusable buffer.
pub fn discharge_into(w: &World, r: &Routing, rain: &[f64], q: &mut Vec<f64>) {
    q.clear();
    q.resize(w.n * w.n, 0.0);
    for &i in &r.order {
        q[i as usize] = w.dx * w.dx * rain[i as usize];
    }
    for &i in r.order.iter().rev() {
        let rc = r.receiver[i as usize] as usize;
        if !w.ocean[rc] {
            q[rc] += q[i as usize];
        }
    }
}

/// One time step: uplift + implicit stream-power incision dh/dt = U - K*sqrt(Q)*S
/// along the routing tree (Braun & Willett 2013, n = 1), sediment into
/// depressions, then hillslope diffusion. Returns the cells under depressions.
pub fn step(w: &mut World, p: &ForgeParams, geo: &Geology, rain: &[f64], scratch: &mut StepScratch) -> usize {
    let StepScratch { routing, q, diffuse, .. } = scratch;
    route_into(w, FLOOD_EPSILON, routing);
    discharge_into(w, routing, rain, q);
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
        diffuse.clone_from(&w.h);
        for z in 1..n - 1 {
            for x in 1..n - 1 {
                let i = z * n + x;
                if w.ocean[i] {
                    continue;
                }
                // The sea holds sea level at the shore, not its floor depth.
                let s = |j: usize| if w.ocean[j] { 0.0 } else { diffuse[j] };
                let lap = s(i - 1) + s(i + 1) + s(i - n) + s(i + n) - 4.0 * diffuse[i];
                w.h[i] = (diffuse[i] + c * lap).max(0.05);
            }
        }
    }
    drowned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_with_reused_scratch_matches_fresh() {
        let p = ForgeParams { size_m: 4096.0, cell_m: 64.0, ..Default::default() };
        let mut a = World::new(&p);
        let mut b = World { h: a.h.clone(), ocean: a.ocean.clone(), uplift: a.uplift.clone(), uplifted: a.uplifted.clone(), ..a };
        let geo = Geology::new(&p, &a);
        let rain = vec![1.0; a.n * a.n];
        let mut reused = StepScratch::default();
        for _ in 0..20 {
            step(&mut a, &p, &geo, &rain, &mut reused);
            step(&mut b, &p, &geo, &rain, &mut StepScratch::default());
        }
        assert_eq!(a.h, b.h);
        assert_eq!(a.uplifted, b.uplifted);
    }
}
