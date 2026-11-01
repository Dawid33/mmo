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
//! Passes 1 and 5 are per cell and run in parallel by rows; passes 2 and 6
//! run in parallel over groups of trees (descent trees, then outlet trees:
//! each tree owns its cells, and a group's trees are traversed in root-index
//! order, so `order` is the same concatenation for any thread count). Passes
//! 3 and 4 stay serial. Ties are broken by cell index and neighbour order
//! only, so the output is deterministic — and bit-identical for any thread
//! count (`erosion::tests::parallel_step_matches_serial`).
//!
//! Unlike `route_into`, the final receiver is NOT re-picked as the steepest
//! neighbour on the epsilon-raised surface: it is the steepest on the
//! depression-free surface (or the breadth-first one on a flat). It is still
//! strictly lower on `filled`, which is all the order needs.

use std::collections::VecDeque;

use super::erosion::{Routing, StepScratch, NO_RECEIVER};
use super::par;
use super::world::World;

/// Water surface at a sea outlet (sea level). A land-edge outlet's base level
/// is its own height.
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
    /// Final receiver direction on the depression-free surface (NO_DIR at
    /// outlets, and on flats until the flat routing reaches them).
    dir: Vec<u8>,
    /// Bit k set: the neighbour in direction k drains into this cell.
    donors: Vec<u8>,
    /// Basin per cell (SEA, or one per local minimum).
    basin: Vec<u32>,
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
    /// Tops of the descent trees (pass 2), then the outlet trees' roots
    /// (land cells draining straight to an outlet, pass 6).
    roots: Vec<u32>,
}

/// Outlet-tree roots per parallel work item in pass 6. Trees vary from one
/// cell to a whole river basin, so items stay small for work stealing.
const ROOTS_PER_TASK: usize = 64;

/// The world's columns as `route_fast` reads them, held as plain slices so
/// the per-cell loops keep them in registers.
#[derive(Clone, Copy)]
struct Grid<'a> {
    h: &'a [f64],
    ocean: &'a [bool],
    outlet: &'a [bool],
}

impl<'a> Grid<'a> {
    fn new(w: &'a World) -> Self {
        // Cells that drain off the map: the sea and the land edges' border.
        Grid { h: &w.h, ocean: &w.ocean, outlet: &w.outlet }
    }

    #[inline]
    fn is_outlet(&self, i: usize) -> bool {
        self.outlet[i]
    }

    /// Height as drainage sees it: the sea at sea level, not its floor; a
    /// land-edge outlet (and any land) at its own height.
    #[inline]
    fn surface(&self, i: usize) -> f64 {
        if self.ocean[i] { OUTLET_LEVEL_M } else { self.h[i] }
    }
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
#[derive(Clone, Copy)]
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
    let (g, s) = (Grid::new(w), Steps::new(w));
    for k in 0..n {
        for i in [k, (n - 1) * n + k, k * n, k * n + n - 1] {
            assert!(g.is_outlet(i), "route_fast: the map edge must be outlets (cell {i} is not)");
        }
    }
    let FastScratch {
        down,
        dir,
        donors,
        basin,
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
        roots,
    } = &mut scratch.fast;
    let Routing { filled, receiver, receiver_dist, order, tree_starts, .. } = out;

    // 1. Steepest descent on h. Outlets next to land sit at sea level in
    // `filled`, as the Priority-Flood seeds them; the rest keep their floor.
    // Every element of `filled`, `down` and `basin` is written below. The
    // tops of the descent trees (land cells that are minima or step straight
    // onto an outlet) are collected for pass 2.
    filled.resize(cells, 0.0);
    down.resize(cells, NO_DIR);
    basin.resize(cells, NO_BASIN);
    par::for_rows3(filled, down, basin, n, roots, |z0, filled, down, basin, tops| {
        let (g, s) = (g, s);
        // Column x of this row has land in the row above, below or itself.
        let mut land_near = vec![false; n];
        for (dz, ((filled, down), basin)) in filled.chunks_mut(n).zip(down.chunks_mut(n)).zip(basin.chunks_mut(n)).enumerate() {
            let z = z0 + dz;
            let (above, below) = (z.saturating_sub(1), (z + 1).min(n - 1));
            for (x, near) in land_near.iter_mut().enumerate() {
                *near = !(g.is_outlet(above * n + x) && g.is_outlet(z * n + x) && g.is_outlet(below * n + x));
            }
            for x in 0..n {
                let i = z * n + x;
                if g.is_outlet(i) {
                    // A shore outlet: land among its (on-map) neighbours. A
                    // land-edge outlet stays at its own height.
                    let shore = land_near[x.saturating_sub(1)..=(x + 1).min(n - 1)].iter().any(|&l| l);
                    filled[x] = if shore && g.ocean[i] { OUTLET_LEVEL_M } else { g.h[i] };
                    (down[x], basin[x]) = (NO_DIR, SEA);
                    continue;
                }
                let hi = g.h[i];
                let (mut best, mut dir) = (0.0, NO_DIR);
                for k in 0..8 {
                    let slope = (hi - g.surface(s.at(i, k))) * s.inv_dist[k];
                    if slope > best {
                        best = slope;
                        dir = k as u8;
                    }
                }
                (filled[x], down[x], basin[x]) = (hi, dir, NO_BASIN);
                if dir == NO_DIR || g.is_outlet(s.at(i, dir as usize)) {
                    tops.push(i as u32);
                }
            }
        }
    });
    donors.resize(cells, 0);
    let land = w.outlet.iter().filter(|&&o| !o).count();

    // 2. Basins: label each cell with the root its descent ends at. A
    // descent tree's top is a land cell that is a minimum or steps straight
    // onto an outlet; trees are labelled top-down, in parallel. Labels are
    // only names: the spill levels below don't depend on them.
    {
        let down: &[u8] = down;
        par::for_rows(donors, n, &mut Vec::new(), |z0, rows, _| {
            let (g, s, down) = (g, s, down);
            for (j, m) in rows.iter_mut().enumerate() {
                let i = z0 * n + j;
                *m = 0;
                if !g.is_outlet(i) {
                    for (k, &back) in OPPOSITE.iter().enumerate() {
                        if down[s.at(i, k)] == back {
                            *m |= 1 << k;
                        }
                    }
                }
            }
        });
    }
    let mut basins = SEA + 1;
    let tops: Vec<(u32, u32)> = roots
        .iter()
        .map(|&i| {
            if down[i as usize] == NO_DIR {
                basins += 1;
                (i, basins - 1)
            } else {
                (i, SEA)
            }
        })
        .collect();
    let groups: Vec<&[(u32, u32)]> = tops.chunks(ROOTS_PER_TASK).collect();
    let labelled = {
        let donors: &[u8] = donors;
        let basin = par::Disjoint::new(basin);
        par::map(&groups, |group| {
            let (s, donors) = (s, donors);
            let (mut depressed, mut stack) = (Vec::new(), Vec::new());
            for &(top, b) in group.iter() {
                stack.push(top);
                while let Some(c) = stack.pop() {
                    let c = c as usize;
                    // SAFETY: each descent tree is labelled by one worker.
                    unsafe { basin.set(c, b) };
                    if b != SEA {
                        depressed.push(c as u32);
                    }
                    let mut m = donors[c];
                    while m != 0 {
                        stack.push(s.at(c, m.trailing_zeros() as usize) as u32);
                        m &= m - 1;
                    }
                }
            }
            depressed
        })
    };
    depressed.clear();
    for part in labelled {
        depressed.extend_from_slice(&part);
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
                let pass = g.h[i].max(g.surface(j));
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
        filled[i] = g.h[i].max(spill[basin[i] as usize]);
    }
    dir.clear();
    dir.resize(cells, NO_DIR);
    {
        let (filled, down): (&[f64], &[u8]) = (filled, down);
        par::for_rows(dir, n, flats, |z0, rows, flats| {
            let (g, s, filled, down) = (g, s, filled, down);
            for (j, d) in rows.iter_mut().enumerate() {
                let i = z0 * n + j;
                if g.is_outlet(i) {
                    continue;
                }
                let fi = filled[i];
                let k = down[i];
                // Neither the cell nor its descent on h was raised: no
                // neighbour can be steeper on the raised surface (raising only
                // lowers slopes toward it), and ties resolve the same way.
                if k != NO_DIR {
                    let to = s.at(i, k as usize);
                    if fi == g.h[i] && filled[to] == g.surface(to) {
                        *d = k;
                        continue;
                    }
                }
                let (mut best, mut best_dir) = (0.0, NO_DIR);
                for k in 0..8 {
                    let slope = (fi - filled[s.at(i, k)]) * s.inv_dist[k];
                    if slope > best {
                        best = slope;
                        best_dir = k as u8;
                    }
                }
                *d = best_dir;
                if best_dir == NO_DIR {
                    flats.push(i as u32);
                }
            }
        });
    }

    // Flats drain breadth-first toward their exits: same-height neighbours
    // that already descend (or are outlets). Seeds are all found before any
    // is assigned, so distances are true breadth-first ones.
    seeds.clear();
    for &i in flats.iter() {
        let i = i as usize;
        let exit = (0..8).find(|&k| {
            let nb = s.at(i, k);
            filled[nb] == filled[i] && (g.is_outlet(nb) || dir[nb] != NO_DIR)
        });
        if let Some(k) = exit {
            seeds.push((i as u32, k as u8));
        }
    }
    queue.clear();
    for &(i, k) in seeds.iter() {
        dir[i as usize] = k;
        queue.push_back(i);
    }
    while let Some(c) = queue.pop_front() {
        let c = c as usize;
        for (k, &back) in OPPOSITE.iter().enumerate() {
            let nb = s.at(c, k);
            if dir[nb] == NO_DIR && !g.is_outlet(nb) && filled[nb] == filled[c] {
                dir[nb] = back;
                queue.push_back(nb as u32);
            }
        }
    }

    // Receivers and donors from the final directions, per cell.
    // Every element of the three is written below.
    receiver.resize(cells, NO_RECEIVER);
    receiver_dist.resize(cells, 0.0);
    donors.resize(cells, 0);
    {
        let dir: &[u8] = dir;
        par::for_rows3(receiver, receiver_dist, donors, n, roots, |z0, receiver, receiver_dist, donors, roots| {
            let (g, s, dir) = (g, s, dir);
            for (j, ((r, rd), m)) in receiver.iter_mut().zip(receiver_dist.iter_mut()).zip(donors.iter_mut()).enumerate() {
                let i = z0 * n + j;
                (*r, *rd, *m) = (NO_RECEIVER, 0.0, 0);
                if g.is_outlet(i) {
                    continue;
                }
                assert_ne!(dir[i], NO_DIR, "route_fast: land cell {i} has no way out");
                let to = s.at(i, dir[i] as usize);
                (*r, *rd) = (to as u32, s.dist[dir[i] as usize]);
                if g.is_outlet(to) {
                    roots.push(i as u32);
                }
                // Bit k: the neighbour in direction k drains into this cell
                // (its direction leads back). Outlets keep no donors.
                for (k, &back) in OPPOSITE.iter().enumerate() {
                    if dir[s.at(i, k)] == back {
                        *m |= 1 << k;
                    }
                }
            }
        });
    }

    // 6. Downstream-first order from the outlets, adding the epsilon gradient
    // (a cell's receiver is final before the cell is visited). Each outlet
    // tree is a contiguous run of `order`, starting at `tree_starts`.
    let groups: Vec<&[u32]> = roots.chunks(ROOTS_PER_TASK).collect();
    let traversed = {
        let (receiver, receiver_dist, donors): (&[u32], &[f64], &[u8]) = (receiver, receiver_dist, donors);
        let filled = par::Disjoint::new(filled);
        par::map(&groups, |group| {
            let (s, receiver, receiver_dist, donors) = (s, receiver, receiver_dist, donors);
            let (mut cells_out, mut starts, mut stack) = (Vec::new(), Vec::with_capacity(group.len()), Vec::new());
            for &root in group.iter() {
                starts.push(cells_out.len() as u32);
                stack.push(root);
                while let Some(c) = stack.pop() {
                    let c = c as usize;
                    let r = receiver[c] as usize;
                    // SAFETY: a tree's cells are visited by its group's worker
                    // only, and a cell's receiver is in the same tree (written
                    // before it) or an outlet (never written here).
                    unsafe { filled.set(c, filled.get(c).max(filled.get(r) + epsilon * receiver_dist[c])) };
                    cells_out.push(c as u32);
                    let mut m = donors[c];
                    while m != 0 {
                        stack.push(s.at(c, m.trailing_zeros() as usize) as u32);
                        m &= m - 1;
                    }
                }
            }
            (cells_out, starts)
        })
    };
    order.clear();
    order.reserve(cells);
    tree_starts.clear();
    for (cells_out, starts) in traversed {
        let base = order.len() as u32;
        tree_starts.extend(starts.iter().map(|&t| base + t));
        order.extend_from_slice(&cells_out);
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
            edges: [crate::forge::world::EdgeKind::Sea; 4],
            outlet: Vec::new(),
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
        w.outlet = w.ocean.clone();
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
