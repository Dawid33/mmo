//! Drainage routing and one step of landscape evolution.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::forge::geology::Geology;
use crate::forge::par;
use crate::forge::routing_fast::route_fast;
use crate::forge::world::{ForgeParams, World, LAND_FLOOR_M};

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
    /// Where each outlet tree starts in `order`, when `order` is a
    /// concatenation of whole trees (`route_fast`); empty when it is not
    /// (`route_into`), and the trees can't be told apart.
    pub tree_starts: Vec<u32>,
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
    /// Ranges of `order` holding whole outlet trees: the parallel work items
    /// of discharge and incision.
    groups: Vec<std::ops::Range<usize>>,
    /// `routing_fast::route_fast`'s working buffers.
    pub(super) fast: crate::forge::routing_fast::FastScratch,
    /// `step` has run with this scratch: later steps route with `route_fast`.
    /// Tied to the scratch, not the World: a scratch reused for ANOTHER world
    /// (or the same world on a new grid) would skip that world's Priority-
    /// Flood initial condition — use a fresh `StepScratch` per world/level.
    routed_once: bool,
}

impl StepScratch {
    /// The water surface of the last `step`'s routing (`Routing::filled`), or
    /// `None` before the first step.
    pub fn filled(&self) -> Option<&[f64]> {
        self.routed_once.then_some(&self.routing.filled[..])
    }
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
///
/// Outlets (`World::outlet`) are where drainage ends: the sea at sea level,
/// a land edge's border cells at their own height.
pub fn route_into(w: &World, epsilon: f64, out: &mut Routing) {
    let cells = w.cells();
    let Routing { filled, receiver, receiver_dist, order, tree_starts, closed, heap } = out;
    tree_starts.clear();
    filled.clear();
    filled.extend_from_slice(&w.h);
    closed.clear();
    closed.extend_from_slice(&w.outlet);
    receiver.clear();
    receiver.resize(cells, NO_RECEIVER);
    receiver_dist.clear();
    receiver_dist.resize(cells, 0.0);
    order.clear();
    order.reserve(cells);
    heap.clear();
    let key = |height: f64| (height + 100_000.0).to_bits();
    for i in 0..cells {
        if w.outlet[i] && w.neighbors(i).any(|(nb, _)| !w.outlet[nb]) {
            // Sea level, not the sea floor, is the sea's base level; a land
            // edge's is the cell's own height.
            let level = if w.ocean[i] { 0.0 } else { w.h[i] };
            filled[i] = level;
            heap.push(Reverse((key(level), i as u32)));
        }
    }
    while let Some(Reverse((_, c))) = heap.pop() {
        let c = c as usize;
        if !w.outlet[c] {
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
    q.resize(w.cells(), 0.0);
    for &i in &r.order {
        q[i as usize] = w.dx * w.dx * rain[i as usize];
    }
    for &i in r.order.iter().rev() {
        let rc = r.receiver[i as usize] as usize;
        if !w.outlet[rc] {
            q[rc] += q[i as usize];
        }
    }
}

/// One time step: uplift + implicit stream-power incision dh/dt = U - K*sqrt(Q)*S
/// along the routing tree (Braun & Willett 2013, n = 1), sediment into
/// depressions, then hillslope diffusion. Returns the cells under depressions.
/// Routing is the Priority-Flood on the first step with a scratch and
/// `route_fast` on every later one.
pub fn step(w: &mut World, p: &ForgeParams, geo: &Geology, rain: &[f64], scratch: &mut StepScratch) -> usize {
    // The first iteration floods the initial condition with the full
    // Priority-Flood; after that the depression-graph routing is linear time.
    let mut routing = std::mem::take(&mut scratch.routing);
    if scratch.routed_once {
        route_fast(w, FLOOD_EPSILON, scratch, &mut routing);
    } else {
        route_into(w, FLOOD_EPSILON, &mut routing);
        scratch.routed_once = true;
    }
    let drowned = evolve(w, p, geo, rain, &routing, scratch);
    // Land-edge outlets follow the land behind them (no-op without land edges).
    w.follow_land_edges(crate::forge::world::EdgeFollow::Lower);
    scratch.routing = routing;
    drowned
}

/// `step` after routing: incision along `routing`, silting, diffusion.
///
/// Discharge and incision run in parallel over groups of whole outlet trees
/// (each tree owns its cells and its sums run in the same order as a serial
/// pass), diffusion over rows: the result is the same for any thread count.
fn evolve(w: &mut World, p: &ForgeParams, geo: &Geology, rain: &[f64], routing: &Routing, scratch: &mut StepScratch) -> usize {
    let StepScratch { q, diffuse, groups, .. } = scratch;
    let (k, dt) = (&p.knobs, p.knobs.dt_years);
    let (nx, nz, cell_area) = (w.nx, w.nz, w.dx * w.dx);
    if routing.tree_starts.is_empty() {
        groups.clear();
        groups.push(0..routing.order.len());
    } else {
        par::group_trees(&routing.tree_starts, routing.order.len(), groups);
    }

    // Discharge (`discharge_into`, tree by tree): m^2 of average rain.
    q.clear();
    q.resize(nx * nz, 0.0);
    par::for_rows(q, nx, &mut Vec::new(), |z0, rows, _| {
        for (j, qi) in rows.iter_mut().enumerate() {
            let i = z0 * nx + j;
            if !w.outlet[i] {
                *qi = cell_area * rain[i];
            }
        }
    });
    {
        let q = par::Disjoint::new(q);
        par::for_ranges(groups, |range| {
            for &i in routing.order[range].iter().rev() {
                let rc = routing.receiver[i as usize] as usize;
                // An outlet (sea or land edge) collects from many trees: it
                // must never be written here.
                if !w.outlet[rc] {
                    // SAFETY: a cell and its non-outlet receiver are in one
                    // tree, and every tree is in exactly one group.
                    unsafe { q.set(rc, q.get(rc) + q.get(i as usize)) };
                }
            }
        });
    }

    // Implicit incision downstream-first, silting under lakes.
    let drowned: usize = {
        let World { h, uplifted, ocean, uplift, .. } = w;
        let (q, ocean, uplift) = (&*q, &*ocean, &*uplift);
        let (h, uplifted) = (par::Disjoint::new(h), par::Disjoint::new(uplifted));
        par::map(groups, |range| {
            let mut drowned = 0;
            for &i in &routing.order[range.clone()] {
                let i = i as usize;
                // SAFETY: as above — this worker owns cell i's tree, and its
                // land receiver is in the same tree.
                unsafe {
                    let hi = h.get(i);
                    let depth = routing.filled[i] - hi;
                    let lift = dt * uplift[i];
                    let before = uplifted.get(i);
                    uplifted.set(i, before + lift);
                    if depth > 1.0e-3 {
                        // Under a lake: no incision; it silts up instead.
                        drowned += 1;
                        h.set(i, hi + (lift + k.depression_fill * depth));
                        continue;
                    }
                    let rc = routing.receiver[i] as usize;
                    // Sea level at the sea; a land-edge outlet's height (fixed during the step)
                    // (never written); else already updated: downstream-first.
                    let base = if ocean[rc] { 0.0 } else { h.get(rc) };
                    let erodibility = k.erodibility * geo.erodibility_at(i, hi, before);
                    let f = dt * erodibility * q[i].max(k.hillslope_area_m2 * rain[i]).sqrt() / routing.receiver_dist[i];
                    h.set(i, ((hi + lift + f * base) / (1.0 + f)).max(LAND_FLOOR_M));
                }
            }
            drowned
        })
        .into_iter()
        .sum()
    };

    // Explicit diffusion, sub-stepped to stay stable (coefficient <= 0.2).
    let coeff = k.diffusivity * dt / (w.dx * w.dx);
    let substeps = (coeff / 0.2).ceil().max(1.0) as usize;
    let c = coeff / substeps as f64;
    let World { h, ocean, wrap_x, .. } = w;
    let (ocean, wrap_x) = (&*ocean, *wrap_x);
    for _ in 0..substeps {
        diffuse.clone_from(h);
        let diffuse = &*diffuse;
        par::for_rows(h, nx, &mut Vec::new(), |z0, rows, _| {
            for (dz, row) in rows.chunks_mut(nx).enumerate() {
                let z = z0 + dz;
                if z == 0 || z == nz - 1 {
                    continue;
                }
                // The sea holds sea level at the shore, not its floor depth.
                let s = |j: usize| if ocean[j] { 0.0 } else { diffuse[j] };
                for x in 1..nx - 1 {
                    let i = z * nx + x;
                    if ocean[i] {
                        continue;
                    }
                    let lap = s(i - 1) + s(i + 1) + s(i - nx) + s(i + nx) - 4.0 * diffuse[i];
                    row[x] = (diffuse[i] + c * lap).max(LAND_FLOOR_M);
                }
                // A wrapping world's first and last columns are neighbours:
                // the same stencil with x +- 1 taken mod nx.
                if wrap_x {
                    for x in [0, nx - 1] {
                        let i = z * nx + x;
                        if ocean[i] {
                            continue;
                        }
                        let (west, east) = (z * nx + (x + nx - 1) % nx, z * nx + (x + 1) % nx);
                        let lap = s(west) + s(east) + s(i - nx) + s(i + nx) - 4.0 * diffuse[i];
                        row[x] = (diffuse[i] + c * lap).max(LAND_FLOOR_M);
                    }
                }
            }
        });
    }
    drowned
}

