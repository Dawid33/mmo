//! Linear-time drainage routing with a depression graph (after Cordonnier,
//! Bovy & Braun 2019, "A versatile, linear complexity algorithm for flow
//! routing in topographies with depressions"). `erosion::step` routes with it
//! after the first iteration instead of a full Priority-Flood every time; the
//! Priority-Flood (`erosion::route`) stays for the first iteration and for the
//! analysis passes.
//!
//! Steps — every one linear in the cell count except the edge sort, which only
//! sees cells on a depression basin's boundary:
//!   1. receivers by steepest descent on `h`, outlets at sea level;
//!   2. basins: each cell labelled with the root its descent ends at — an
//!      outlet (all outlets are one node, `SEA`) or a local minimum (one node
//!      each);
//!   3. the basin graph: an edge per pair of neighbouring cells in different
//!      basins, weighted by the higher of the two (the pass between them);
//!   4. Kruskal over the edges, lowest first: the weight at which a basin's
//!      tree first joins the sea's is its spill level — the highest pass on its
//!      cheapest way out (the minimax path), which is exactly what the
//!      Priority-Flood floods it to;
//!   5. the depression-free surface `max(h, spill level)`; receivers by
//!      steepest descent on it, and flats (lake surfaces, plateaus) routed
//!      breadth-first from their exits — so a lake drains through its pass;
//!   6. `order` by a stack traversal from the outlets, which also adds the
//!      epsilon gradient: a cell sits at least `epsilon * distance` above its
//!      receiver, as in the Priority-Flood.
//!
//! Passes 1 and 5 are per cell (independent, parallelisable); everything else
//! works per basin or per drainage tree. Ties are broken by cell index and
//! neighbour order only, so the output is deterministic.
//!
//! Unlike `route_into`, the final receiver is NOT re-picked as the steepest
//! neighbour on the epsilon-raised surface: it is the steepest on the
//! depression-free surface (or the breadth-first one on a flat). It is still
//! strictly lower on `filled`, which is all the order needs.

use std::collections::VecDeque;

use super::erosion::{Routing, StepScratch, NO_RECEIVER};
use super::world::World;

/// Water surface at an outlet (sea level): the base level everything drains to.
const OUTLET_LEVEL_M: f64 = 0.0;
/// Basin of every outlet and of all land that descends straight to one.
const SEA: u32 = 0;
const NO_BASIN: u32 = u32::MAX;
/// `down` of a cell with no lower neighbour (an outlet or a local minimum).
const NO_DIR: u8 = 8;
/// For each neighbour direction, the direction that leads back.
const OPPOSITE: [u8; 8] = [1, 0, 3, 2, 7, 6, 5, 4];

/// `route_fast`'s working buffers, kept in `StepScratch` across iterations.
#[derive(Default)]
pub struct FastScratch {
    /// Steepest-descent direction on `h` (NO_DIR at outlets and minima).
    down: Vec<u8>,
    /// Bit k set: the neighbour in direction k drains into this cell.
    donors: Vec<u8>,
    /// Basin per cell (SEA, or one per local minimum).
    basin: Vec<u32>,
    /// Cells walked while labelling a basin.
    path: Vec<u32>,
    /// Cells of the depression basins (every basin but SEA).
    depressed: Vec<u32>,
    /// Basin graph edges: (pass height key, lower basin, higher basin).
    edges: Vec<(u64, u32, u32)>,
    /// Union-find over basins: parent, size, circular member list, holds SEA.
    uf_parent: Vec<u32>,
    uf_size: Vec<u32>,
    uf_next: Vec<u32>,
    uf_sea: Vec<bool>,
    /// Spill level per basin (-inf for SEA).
    spill: Vec<f64>,
    /// Land cells with no lower neighbour on the depression-free surface.
    flats: Vec<u32>,
    /// (flat cell, direction of its exit) pairs that start the flat routing.
    seeds: Vec<(u32, u8)>,
    queue: VecDeque<u32>,
    stack: Vec<u32>,
}

/// Cells that drain off the map: the sea today (land edges would join here).
#[inline]
fn is_outlet(w: &World, i: usize) -> bool {
    w.ocean[i]
}

/// Height as drainage sees it: outlets at sea level, not their floor.
#[inline]
fn surface(w: &World, i: usize) -> f64 {
    if is_outlet(w, i) { OUTLET_LEVEL_M } else { w.h[i] }
}

/// Order-preserving map of an f64 onto u64 (total order, exact inverse).
fn height_key(x: f64) -> u64 {
    let b = x.to_bits();
    if b >> 63 == 1 { !b } else { b | 1 << 63 }
}

fn height_from_key(k: u64) -> f64 {
    f64::from_bits(if k >> 63 == 1 { k & !(1 << 63) } else { !k })
}

/// The 8 neighbours as index offsets, in `World::neighbors` order, with
/// their distances (the same f64s `neighbors` gives). Valid off the map edge
/// only: `route_fast` checks the edge is all outlets, so every land cell it
/// expands is interior.
struct Steps {
    off: [isize; 8],
    dist: [f64; 8],
    inv_dist: [f64; 8],
}

impl Steps {
    fn new(w: &World) -> Steps {
        let n = w.n as isize;
        let (o, d) = (w.dx, std::f64::consts::SQRT_2 * w.dx);
        let dist = [o, o, o, o, d, d, d, d];
        Steps {
            off: [1, -1, n, -n, n + 1, -n + 1, n - 1, -n - 1],
            dist,
            inv_dist: dist.map(|d| 1.0 / d),
        }
    }

    #[inline]
    fn at(&self, i: usize, k: usize) -> usize {
        i.wrapping_add_signed(self.off[k])
    }
}

/// Cell `i` drains to its neighbour in direction `k`.
#[inline]
fn link(s: &Steps, receiver: &mut [u32], receiver_dist: &mut [f64], donors: &mut [u8], i: usize, k: usize) {
    let r = s.at(i, k);
    receiver[i] = r as u32;
    receiver_dist[i] = s.dist[k];
    donors[r] |= 1 << OPPOSITE[k];
}

fn find(parent: &mut [u32], mut x: usize) -> usize {
    while parent[x] as usize != x {
        let g = parent[parent[x] as usize];
        parent[x] = g;
        x = g as usize;
    }
    x
}

/// Depression-free routing over the land, the same `Routing` as
/// `erosion::route_into` (filled surface with the epsilon gradient, receivers
/// strictly downhill on it, downstream-first order) in linear time.
pub fn route_fast(w: &World, epsilon: f64, scratch: &mut StepScratch, out: &mut Routing) {
    let (n, cells) = (w.n, w.n * w.n);
    for k in 0..n {
        for i in [k, (n - 1) * n + k, k * n, k * n + n - 1] {
            assert!(is_outlet(w, i), "route_fast: the map edge must be outlets (cell {i} is not)");
        }
    }
    let s = Steps::new(w);
    let FastScratch {
        down,
        donors,
        basin,
        path,
        depressed,
        edges,
        uf_parent,
        uf_size,
        uf_next,
        uf_sea,
        spill,
        flats,
        seeds,
        queue,
        stack,
    } = &mut scratch.fast;
    let Routing { filled, receiver, receiver_dist, order, .. } = out;

    // 1. Steepest descent on h. Outlets next to land sit at sea level in
    // `filled`, as the Priority-Flood seeds them; the rest keep their floor.
    filled.clear();
    filled.extend_from_slice(&w.h);
    down.clear();
    down.resize(cells, NO_DIR);
    basin.clear();
    basin.resize(cells, NO_BASIN);
    let mut land = 0;
    for i in 0..cells {
        if is_outlet(w, i) {
            basin[i] = SEA;
            continue;
        }
        land += 1;
        let hi = w.h[i];
        let (mut best, mut dir) = (0.0, NO_DIR);
        for k in 0..8 {
            let nb = s.at(i, k);
            let lower = if is_outlet(w, nb) {
                filled[nb] = OUTLET_LEVEL_M;
                OUTLET_LEVEL_M
            } else {
                w.h[nb]
            };
            let slope = (hi - lower) * s.inv_dist[k];
            if slope > best {
                best = slope;
                dir = k as u8;
            }
        }
        down[i] = dir;
    }

    // 2. Basins: label each cell with the root of its descent.
    depressed.clear();
    let mut basins = SEA + 1;
    for i in 0..cells {
        if basin[i] != NO_BASIN {
            continue;
        }
        path.clear();
        let mut c = i;
        while basin[c] == NO_BASIN {
            path.push(c as u32);
            let k = down[c];
            if k == NO_DIR {
                basin[c] = basins;
                basins += 1;
                break;
            }
            c = s.at(c, k as usize);
        }
        let b = basin[c];
        for &p in path.iter() {
            basin[p as usize] = b;
        }
        if b != SEA {
            depressed.extend_from_slice(path);
        }
    }

    // 3. Basin graph: every neighbouring pair across a depression basin's
    // boundary, once (from the depression side; from the higher index when
    // both sides are depressions).
    edges.clear();
    for &i in depressed.iter() {
        let (i, bi) = (i as usize, basin[i as usize]);
        for k in 0..8 {
            let j = s.at(i, k);
            let bj = basin[j];
            if bj != bi && (bj == SEA || j > i) {
                let pass = w.h[i].max(surface(w, j));
                edges.push((height_key(pass), bi.min(bj), bi.max(bj)));
            }
        }
    }
    edges.sort_unstable();

    // 4. Kruskal from the sea: a basin's spill level is the pass at which its
    // tree joins the sea's.
    let nb = basins as usize;
    spill.clear();
    spill.resize(nb, f64::INFINITY);
    spill[SEA as usize] = f64::NEG_INFINITY;
    uf_parent.clear();
    uf_parent.extend(0..basins);
    uf_size.clear();
    uf_size.resize(nb, 1);
    uf_next.clear();
    uf_next.extend(0..basins);
    uf_sea.clear();
    uf_sea.resize(nb, false);
    uf_sea[SEA as usize] = true;
    let mut unresolved = nb - 1;
    for &(key, a, b) in edges.iter() {
        if unresolved == 0 {
            break;
        }
        let (ra, rb) = (find(uf_parent, a as usize), find(uf_parent, b as usize));
        if ra == rb {
            continue;
        }
        if uf_sea[ra] != uf_sea[rb] {
            let level = height_from_key(key);
            let inland = if uf_sea[ra] { rb } else { ra };
            let mut m = inland;
            loop {
                spill[m] = level;
                unresolved -= 1;
                m = uf_next[m] as usize;
                if m == inland {
                    break;
                }
            }
        }
        let (big, small) = if uf_size[ra] >= uf_size[rb] { (ra, rb) } else { (rb, ra) };
        uf_parent[small] = big as u32;
        uf_size[big] += uf_size[small];
        uf_sea[big] |= uf_sea[small];
        uf_next.swap(ra, rb); // splice the two circular member lists
    }
    assert_eq!(unresolved, 0, "route_fast: {unresolved} basins have no way to an outlet");

    // 5. Depression-free surface (only depression cells can rise) and
    // steepest descent on it.
    for &i in depressed.iter() {
        let i = i as usize;
        filled[i] = w.h[i].max(spill[basin[i] as usize]);
    }
    receiver.clear();
    receiver.resize(cells, NO_RECEIVER);
    receiver_dist.clear();
    receiver_dist.resize(cells, 0.0);
    donors.clear();
    donors.resize(cells, 0);
    flats.clear();
    for i in 0..cells {
        if is_outlet(w, i) {
            continue;
        }
        let fi = filled[i];
        let k = down[i];
        // Neither the cell nor its descent on h was raised: no neighbour can be
        // steeper on the raised surface (raising only lowers slopes toward it),
        // and ties resolve the same way.
        if k != NO_DIR {
            let d = s.at(i, k as usize);
            if fi == w.h[i] && filled[d] == surface(w, d) {
                link(&s, receiver, receiver_dist, donors, i, k as usize);
                continue;
            }
        }
        let (mut best, mut dir) = (0.0, NO_DIR);
        for k in 0..8 {
            let slope = (fi - filled[s.at(i, k)]) * s.inv_dist[k];
            if slope > best {
                best = slope;
                dir = k as u8;
            }
        }
        if dir == NO_DIR {
            flats.push(i as u32);
        } else {
            link(&s, receiver, receiver_dist, donors, i, dir as usize);
        }
    }

    // Flats drain breadth-first toward their exits: same-height neighbours
    // that already descend (or are outlets). Seeds are all found before any
    // is assigned, so distances are true breadth-first ones.
    seeds.clear();
    for &i in flats.iter() {
        let i = i as usize;
        let exit = (0..8).find(|&k| {
            let nb = s.at(i, k);
            filled[nb] == filled[i] && (is_outlet(w, nb) || receiver[nb] != NO_RECEIVER)
        });
        if let Some(k) = exit {
            seeds.push((i as u32, k as u8));
        }
    }
    queue.clear();
    for &(i, k) in seeds.iter() {
        link(&s, receiver, receiver_dist, donors, i as usize, k as usize);
        queue.push_back(i);
    }
    while let Some(c) = queue.pop_front() {
        let c = c as usize;
        for (k, &back) in OPPOSITE.iter().enumerate() {
            let nb = s.at(c, k);
            if receiver[nb] == NO_RECEIVER && !is_outlet(w, nb) && filled[nb] == filled[c] {
                link(&s, receiver, receiver_dist, donors, nb, back as usize);
                queue.push_back(nb as u32);
            }
        }
    }

    // 6. Downstream-first order from the outlets, adding the epsilon gradient
    // (a cell's receiver is final before the cell is visited).
    order.clear();
    order.reserve(cells);
    stack.clear();
    for i in 0..cells {
        if is_outlet(w, i) {
            continue;
        }
        let r = receiver[i];
        assert_ne!(r, NO_RECEIVER, "route_fast: land cell {i} has no way out");
        if !is_outlet(w, r as usize) {
            continue;
        }
        stack.push(i as u32);
        while let Some(c) = stack.pop() {
            let c = c as usize;
            filled[c] = filled[c].max(filled[receiver[c] as usize] + epsilon * receiver_dist[c]);
            order.push(c as u32);
            let mut m = donors[c];
            while m != 0 {
                stack.push(s.at(c, m.trailing_zeros() as usize) as u32);
                m &= m - 1;
            }
        }
    }
    assert_eq!(order.len(), land, "route_fast: receivers form a cycle");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::erosion::{route, FLOOD_EPSILON, NO_RECEIVER};
    use crate::forge::world::hash01;

    const N: usize = 64;
    const CELL_M: f64 = 32.0;
    const SEA_FLOOR_M: f64 = -5.0;

    /// An N x N world, ocean on the border ring, land heights from `f(x, z)`.
    fn world(f: impl Fn(usize, usize) -> f64) -> World {
        let mut w = World {
            n: N,
            dx: CELL_M,
            h: vec![0.0; N * N],
            ocean: vec![false; N * N],
            uplift: vec![0.0; N * N],
            uplifted: vec![0.0; N * N],
            margin_drowned: 0,
        };
        for z in 0..N {
            for x in 0..N {
                let i = z * N + x;
                if x == 0 || z == 0 || x == N - 1 || z == N - 1 {
                    w.ocean[i] = true;
                    w.h[i] = SEA_FLOOR_M;
                } else {
                    w.h[i] = f(x, z);
                }
            }
        }
        w
    }

    fn dist(x: usize, z: usize, cx: f64, cz: f64) -> f64 {
        ((x as f64 - cx).powi(2) + (z as f64 - cz).powi(2)).sqrt()
    }

    /// Per-cell hash noise: pits everywhere.
    fn noise() -> World {
        world(|x, z| 0.5 + 20.0 * hash01(7, x as u64, z as u64))
    }

    /// One closed bowl (minimum 1 m at the centre) inside a rim at 11 m.
    fn bowl() -> World {
        world(|x, z| {
            let d = dist(x, z, 31.5, 31.5);
            if d < 20.0 { 1.0 + 0.5 * d } else { (11.0 - 0.3 * (d - 20.0)).max(0.5) }
        })
    }

    /// Pits A and B merge into one lake that spills into pit C, which drains
    /// to the sea down a channel; all inside a flat plateau at 30 m.
    fn three_pits() -> World {
        world(|x, z| {
            let a = 5.0 + 3.0 * dist(x, z, 20.0, 32.0);
            let b = 6.0 + 3.0 * dist(x, z, 28.0, 32.0);
            let c = 2.0 + 3.0 * dist(x, z, 40.0, 32.0);
            let channel = if z == 32 && x >= 40 { 12.0 - 0.5 * (x - 40) as f64 } else { f64::MAX };
            a.min(b).min(c).min(channel).min(30.0)
        })
    }

    /// A perfectly flat island.
    fn flat() -> World {
        world(|_, _| 5.0)
    }

    fn terrains() -> [(&'static str, World); 4] {
        [("noise", noise()), ("bowl", bowl()), ("three pits", three_pits()), ("flat", flat())]
    }

    fn fast(w: &World) -> Routing {
        let mut out = Routing::default();
        route_fast(w, FLOOD_EPSILON, &mut StepScratch::default(), &mut out);
        out
    }

    #[test]
    fn fast_routing_drains_every_land_cell() {
        for (name, w) in terrains() {
            let r = fast(&w);
            for i in (0..N * N).filter(|&i| !w.ocean[i]) {
                let mut c = i;
                let mut steps = 0;
                while !w.ocean[c] {
                    let rc = r.receiver[c];
                    assert_ne!(rc, NO_RECEIVER, "{name}: land cell {c} has no receiver");
                    let rc = rc as usize;
                    assert!(r.filled[c] > r.filled[rc], "{name}: receiver of {c} is not strictly downhill");
                    c = rc;
                    steps += 1;
                    assert!(steps <= N * N, "{name}: cell {i} never reaches the sea");
                }
            }
        }
    }

    #[test]
    fn fast_routing_order_is_downstream_first() {
        for (name, w) in terrains() {
            let r = fast(&w);
            let mut pos = vec![usize::MAX; N * N];
            for (k, &i) in r.order.iter().enumerate() {
                assert_eq!(pos[i as usize], usize::MAX, "{name}: cell {i} twice in order");
                pos[i as usize] = k;
            }
            for i in 0..N * N {
                assert_eq!(pos[i] != usize::MAX, !w.ocean[i], "{name}: order must hold exactly the land cells");
                let rc = r.receiver[i];
                if w.ocean[i] || w.ocean[rc as usize] {
                    continue;
                }
                assert!(pos[i] > pos[rc as usize], "{name}: cell {i} comes before its receiver");
            }
        }
    }

    /// Depression fill heights must match the Priority-Flood within 1e-3 m.
    /// On top of that, both surfaces add the epsilon gradient across filled
    /// flats along DIFFERENT paths (the flood accumulates it in flood order,
    /// `route_fast` toward the pass), so the path lengths may differ by up to
    /// a grid's width: an extra `FLOOD_EPSILON * cell_m * n` is allowed
    /// (controller ruling R4) — and nothing more.
    #[test]
    fn fast_routing_filled_matches_priority_flood() {
        let tolerance = 1.0e-3 + FLOOD_EPSILON * CELL_M * N as f64;
        for (name, w) in terrains() {
            let r = fast(&w);
            let pf = route(&w, FLOOD_EPSILON);
            for i in 0..N * N {
                let diff = (r.filled[i] - pf.filled[i]).abs();
                assert!(
                    diff <= tolerance,
                    "{name}: cell {i} filled {} vs Priority-Flood {} (diff {diff})",
                    r.filled[i],
                    pf.filled[i]
                );
            }
        }
    }

    /// Without the epsilon gradient both surfaces are the minimax spill
    /// surface, so they agree bit for bit: the fill heights are exact, and
    /// any difference above is the gradient's path length alone.
    #[test]
    fn fast_routing_spill_levels_are_exact() {
        for (name, w) in terrains() {
            let mut r = Routing::default();
            route_fast(&w, 0.0, &mut StepScratch::default(), &mut r);
            let pf = route(&w, 0.0);
            let worst = (0..N * N).map(|i| (r.filled[i] - pf.filled[i]).abs()).fold(0.0, f64::max);
            assert_eq!(r.filled, pf.filled, "{name}: worst difference {worst}");
        }
    }

    /// The terrains really exercise depressions (or the flat).
    #[test]
    fn test_terrains_have_depressions() {
        for (name, w) in terrains() {
            let pf = route(&w, 0.0);
            let lake = (0..N * N).filter(|&i| !w.ocean[i] && pf.filled[i] - w.h[i] > 1.0e-3).count();
            assert_eq!(lake == 0, name == "flat", "{name}: {lake} cells under depressions");
        }
    }

    #[test]
    fn fast_routing_is_deterministic_and_reuses_scratch() {
        let mut scratch = StepScratch::default();
        let mut reused = Routing::default();
        for (name, w) in terrains() {
            route_fast(&w, FLOOD_EPSILON, &mut scratch, &mut reused);
            let fresh = fast(&w);
            assert_eq!(reused.filled, fresh.filled, "{name}");
            assert_eq!(reused.receiver, fresh.receiver, "{name}");
            assert_eq!(reused.receiver_dist, fresh.receiver_dist, "{name}");
            assert_eq!(reused.order, fresh.order, "{name}");
        }
    }
}
