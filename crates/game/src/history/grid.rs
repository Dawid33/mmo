//! The society grid: per-region capacity and passability from the world map,
//! plus deterministic A* routing over it.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::rules::*;
use crate::protocol::RegionCoords;
use crate::WorldMap;

pub const ROUTE_MAX_NODES: u32 = 20_000;

#[derive(Debug, Clone)]
pub struct SocietyGrid {
    origin: RegionCoords,
    width: u32,
    height: u32,
    capacity: Vec<u32>,
    passable: Vec<bool>,
    step_cost: Vec<u32>,
    /// Per tile: 8-connected component id of `passable` tiles (meaningless where impassable).
    component: Vec<u32>,
    /// Per tile: the world map's landing column (region-local blocks), if any.
    landing: Vec<Option<[u32; 2]>>,
    world_id: u32,
}

/// Label 8-connected components of `passable` on a `width`-wide grid, in row-major order.
fn label_components(passable: &[bool], width: u32) -> Vec<u32> {
    let w = width as usize;
    let n = passable.len();
    let h = if w == 0 { 0 } else { n / w };
    let mut label = vec![u32::MAX; n];
    let mut next = 0u32;
    let mut stack = Vec::new();
    for start in 0..n {
        if !passable[start] || label[start] != u32::MAX {
            continue;
        }
        label[start] = next;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, z) = ((i % w) as i64, (i / w) as i64);
            for dz in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, nz) = (x + dx, z + dz);
                    if (dx, dz) == (0, 0) || nx < 0 || nz < 0 || nx >= w as i64 || nz >= h as i64 {
                        continue;
                    }
                    let ni = nz as usize * w + nx as usize;
                    if passable[ni] && label[ni] == u32::MAX {
                        label[ni] = next;
                        stack.push(ni);
                    }
                }
            }
        }
        next += 1;
    }
    label
}

impl SocietyGrid {
    pub fn from_world_map(map: &WorldMap) -> SocietyGrid {
        let n = map.tiles.len();
        let mut capacity = Vec::with_capacity(n);
        let mut passable = Vec::with_capacity(n);
        let mut step_cost = Vec::with_capacity(n);
        for t in &map.tiles {
            let dry = (1.0 - t.sea_fraction - t.lake_fraction).clamp(0.0, 1.0);
            let dry_pm = (dry * 1000.0).round() as u32;
            let has_landing = t.landing.is_some();
            passable.push(has_landing && dry_pm > 0);
            let relief_pen = (t.relief_m.max(0.0) as u32 / 100) * CAPACITY_RELIEF_PENALTY_PER_100M;
            step_cost.push(1000 + relief_pen);
            let cap = if !has_landing || dry_pm < MIN_DRY_FRACTION_PER_MILLE {
                0
            } else {
                let lake = if t.lake_fraction > 0.0 { CAPACITY_LAKE } else { 0 };
                let raw = (CAPACITY_BASE + u32::from(t.rivers) * CAPACITY_PER_RIVER + lake)
                    .saturating_sub(relief_pen)
                    .min(CAPACITY_MAX);
                raw * biome_capacity_per_mille(t.biome) / 1000
            };
            capacity.push(cap);
        }
        let component = label_components(&passable, map.width);
        let landing = map.tiles.iter().map(|t| t.landing).collect();
        let bytes = bincode::serialize(map).expect("world map serialises");
        SocietyGrid {
            origin: map.origin,
            width: map.width,
            height: map.height,
            capacity,
            passable,
            step_cost,
            component,
            landing,
            world_id: crc32fast::hash(&bytes),
        }
    }

    fn index(&self, rc: RegionCoords) -> Option<usize> {
        let dx = i64::from(rc.x) - i64::from(self.origin.x);
        let dz = i64::from(rc.z) - i64::from(self.origin.z);
        if (0..i64::from(self.width)).contains(&dx) && (0..i64::from(self.height)).contains(&dz) {
            Some((dz * i64::from(self.width) + dx) as usize)
        } else {
            None
        }
    }

    pub fn contains(&self, rc: RegionCoords) -> bool {
        self.index(rc).is_some()
    }

    pub fn capacity(&self, rc: RegionCoords) -> u32 {
        self.index(rc).map_or(0, |i| self.capacity[i])
    }

    pub fn passable(&self, rc: RegionCoords) -> bool {
        self.index(rc).is_some_and(|i| self.passable[i])
    }

    /// The landmass of `rc`: its 8-connected component of passable regions (numbered in
    /// row-major order of their first region). `None` off the grid or on impassable ground.
    /// Two regions in different components have no route between them.
    pub fn component(&self, rc: RegionCoords) -> Option<u32> {
        self.index(rc).and_then(|i| self.passable[i].then(|| self.component[i]))
    }

    /// The region's landing column (region-local blocks, `TileSummary::landing`); `None`
    /// off the grid or with no dry land.
    pub fn landing(&self, rc: RegionCoords) -> Option<[u32; 2]> {
        self.index(rc).and_then(|i| self.landing[i])
    }

    pub fn step_cost(&self, rc: RegionCoords) -> u32 {
        self.index(rc).map_or(1000, |i| self.step_cost[i])
    }

    /// Every region, row-major (z then x).
    pub fn regions(&self) -> impl Iterator<Item = RegionCoords> + '_ {
        let (w, o) = (self.width, self.origin);
        (0..self.width * self.height)
            .map(move |i| RegionCoords::new(o.x + (i % w) as i32, o.z + (i / w) as i32))
    }

    /// crc32 of the world map's bincode, so a history from another world is refused.
    pub fn world_id(&self) -> u32 {
        self.world_id
    }
}

/// Deterministic A* over 8-neighbour region tiles on `passable`.
pub fn route(
    grid: &SocietyGrid,
    from: RegionCoords,
    to: RegionCoords,
    max_nodes: u32,
) -> Option<Vec<RegionCoords>> {
    let (fi, ti) = (grid.index(from)?, grid.index(to)?);
    if !grid.passable[fi] || !grid.passable[ti] || grid.component[fi] != grid.component[ti] {
        return None;
    }
    let n = grid.passable.len();
    let h = |rc: RegionCoords| -> u64 {
        let dx = (i64::from(rc.x) - i64::from(to.x)).unsigned_abs();
        let dz = (i64::from(rc.z) - i64::from(to.z)).unsigned_abs();
        dx.max(dz) * 1000
    };
    let mut g = vec![u64::MAX; n];
    let mut prev = vec![usize::MAX; n];
    let mut closed = vec![false; n];
    let mut open = BinaryHeap::new();
    g[fi] = 0;
    open.push(Reverse((h(from), 0u64, from.z, from.x)));
    let mut expanded = 0u32;
    while let Some(Reverse((_, gc, z, x))) = open.pop() {
        let cur = RegionCoords::new(x, z);
        let ci = grid.index(cur).expect("queued tiles are in the grid");
        if closed[ci] || gc > g[ci] {
            continue;
        }
        closed[ci] = true;
        if ci == ti {
            let mut path = vec![cur];
            let mut i = ci;
            while prev[i] != usize::MAX {
                i = prev[i];
                let w = grid.width as usize;
                path.push(RegionCoords::new(
                    grid.origin.x + (i % w) as i32,
                    grid.origin.z + (i / w) as i32,
                ));
            }
            path.reverse();
            return Some(path);
        }
        expanded += 1;
        if expanded > max_nodes {
            return None;
        }
        for dz in -1i32..=1 {
            for dx in -1i32..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let nb = RegionCoords::new(x + dx, z + dz);
                let Some(ni) = grid.index(nb) else { continue };
                if !grid.passable[ni] || closed[ni] {
                    continue;
                }
                let mut step = u64::from(grid.step_cost[ni]);
                if dx != 0 && dz != 0 {
                    step = step * 1414 / 1000;
                }
                let ng = gc + step;
                if ng < g[ni] {
                    g[ni] = ng;
                    prev[ni] = ci;
                    open.push(Reverse((ng + h(nb), ng, nb.z, nb.x)));
                }
            }
        }
    }
    None
}
