//! Water bodies: a connected pool whose water is one `volume`. Every member layer
//! below `top` is virtually full; the top layer shares `frac` evenly.

use crate::grid::{ALL, Dims, Dir, HORIZONTAL};
use crate::state::{BodyId, Fnv, NO_BODY, World};
use crate::{EXPAND_AREA_FACTOR, EXPAND_BACKOFF, EXPAND_BUDGET, FILM, FULL};

pub(crate) const F: u64 = FULL as u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Body {
    pub id: BodyId,
    pub volume: u64,
    /// Highest member layer.
    pub top: u32,
    /// Member count per layer (len = dims.y).
    pub area: Vec<u32>,
    /// Members strictly below `top` (= cells - area[top]).
    pub below: u64,
    pub cells: u64,
    /// Inclusive bounding box of members, (x, y, z). Grows only.
    pub lo: [u32; 3],
    pub hi: [u32; 3],
    /// Sorted non-member, non-solid cells to wake when the level rises (spill edges, edit holes).
    pub shore: Vec<u32>,
    /// A refused expansion is not retried before this step.
    pub expand_retry: u32,
}

impl Body {
    pub fn empty(id: BodyId, dims: Dims) -> Self {
        Body {
            id,
            volume: 0,
            top: 0,
            area: vec![0; dims.y as usize],
            below: 0,
            cells: 0,
            lo: [u32::MAX; 3],
            hi: [0; 3],
            shore: Vec::new(),
            expand_retry: 0,
        }
    }
    pub fn top_area(&self) -> u64 {
        self.area[self.top as usize] as u64
    }
    pub fn capacity(&self) -> u64 {
        F * (self.below + self.top_area())
    }
    /// Water in the top layer.
    pub fn frac(&self) -> u64 {
        self.volume.saturating_sub(F * self.below)
    }
    pub fn room(&self) -> u64 {
        self.capacity().saturating_sub(self.volume)
    }
    /// The water a member at layer `y` holds, as a CA cell would see it.
    pub fn virtual_at(&self, y: u32) -> u16 {
        if y < self.top {
            FULL
        } else if y == self.top && self.top_area() > 0 {
            (self.frac() / self.top_area()) as u16
        } else {
            0
        }
    }
    /// Head for water LEAVING the body: top layer rounded up, so a draining last layer empties.
    pub fn outflow_at(&self, y: u32) -> u16 {
        if y < self.top {
            FULL
        } else if y == self.top && self.top_area() > 0 {
            self.frac().div_ceil(self.top_area()) as u16
        } else {
            0
        }
    }
    /// Surface height in units.
    pub fn level(&self) -> u64 {
        self.top as u64 * F + self.virtual_at(self.top) as u64
    }
    pub fn byte_size(&self) -> usize {
        48 + 4 * self.area.len() + 4 * self.shore.len()
    }
    pub fn hash_into(&self, h: &mut Fnv) {
        h.u32(self.id);
        h.u64(self.volume);
        h.u32(self.top);
        for a in &self.area {
            h.u32(*a);
        }
        h.u64(self.below);
        h.u64(self.cells);
        for v in self.lo.iter().chain(&self.hi) {
            h.u32(*v);
        }
        for s in &self.shore {
            h.u32(*s);
        }
        h.u32(self.expand_retry);
    }
    pub(crate) fn grow_bbox(&mut self, (x, y, z): (u32, u32, u32)) {
        for (k, v) in [x, y, z].into_iter().enumerate() {
            self.lo[k] = self.lo[k].min(v);
            self.hi[k] = self.hi[k].max(v);
        }
    }
}

pub fn on_solid(w: &World, i: u32) -> bool {
    w.dims.step(i, Dir::Down).is_none_or(|b| w.solid.get(b))
}

pub fn on_solid_or_member(w: &World, i: u32) -> bool {
    w.dims
        .step(i, Dir::Down)
        .is_none_or(|b| w.solid.get(b) || w.owner[b as usize] != NO_BODY)
}

fn supported_by(w: &World, i: u32, id: BodyId) -> bool {
    w.dims
        .step(i, Dir::Down)
        .is_none_or(|b| w.solid.get(b) || w.owner[b as usize] == id)
}

/// Members of `id` in layer `y`, ascending (bbox scan).
pub fn layer_members(w: &World, id: BodyId, y: u32) -> Vec<u32> {
    let b = &w.bodies[&id];
    let mut out = Vec::new();
    if b.cells == 0 {
        return out;
    }
    for z in b.lo[2]..=b.hi[2] {
        for x in b.lo[0]..=b.hi[0] {
            let i = w.dims.idx(x, y, z);
            if w.owner[i as usize] == id {
                out.push(i);
            }
        }
    }
    out
}

pub fn all_members(w: &World, id: BodyId) -> Vec<u32> {
    let b = &w.bodies[&id];
    if b.cells == 0 {
        return Vec::new();
    }
    (b.lo[1]..=b.hi[1]).flat_map(|y| layer_members(w, id, y)).collect()
}

/// Move CA cell `c` (and its water) into body `id`. The caller has set `top`
/// so that `c`'s layer is at or below it.
pub fn add_member(w: &mut World, id: BodyId, c: u32) {
    let dims = w.dims;
    let water = w.water[c as usize] as u64;
    w.set_water(c, 0);
    w.set_owner(c, id);
    w.set_active(c, false);
    let y = dims.y_of(c);
    let opens: Vec<u32> = HORIZONTAL
        .iter()
        .filter_map(|&d| dims.step(c, d))
        .filter(|&n| w.is_ca(n))
        .collect();
    let b = w.body_mut(id);
    b.volume += water;
    b.area[y as usize] += 1;
    b.cells += 1;
    if y < b.top {
        b.below += 1;
    }
    b.grow_bbox(dims.xyz(c));
    b.shore.retain(|&s| s != c);
    for n in opens {
        if let Err(p) = b.shore.binary_search(&n) {
            b.shore.insert(p, n);
        }
    }
}

pub fn add_volume(w: &mut World, id: BodyId, q: u64) {
    let b = w.body_mut(id);
    assert!(q <= b.room(), "add_volume beyond room");
    b.volume += q;
}

pub fn remove_volume(w: &mut World, id: BodyId, q: u64) {
    let b = w.body_mut(id);
    assert!(q <= b.volume, "remove_volume beyond volume");
    b.volume -= q;
    drop_empty_layers(w, id);
}

/// While the top layer holds no water its cells leave the body (dry CA cells,
/// awake). A body with no cells is removed. Dropping a layer can disconnect the body.
pub fn drop_empty_layers(w: &mut World, id: BodyId) {
    let mut dropped = false;
    loop {
        let (frac, cells, top) = {
            let b = &w.bodies[&id];
            (b.frac(), b.cells, b.top)
        };
        if cells == 0 || frac > 0 {
            break;
        }
        for c in layer_members(w, id, top) {
            w.set_owner(c, NO_BODY);
            w.set_active(c, true);
            w.touch(c);
        }
        let b = w.body_mut(id);
        b.cells -= b.area[top as usize] as u64;
        b.area[top as usize] = 0;
        match (0..top).rev().find(|&y| b.area[y as usize] > 0) {
            Some(y) => {
                b.top = y;
                b.below -= b.area[y as usize] as u64;
            }
            None => {
                b.top = 0;
                b.below = 0;
            }
        }
        w.stats.layer_drops += 1;
        dropped = true;
    }
    if w.bodies[&id].cells == 0 {
        assert_eq!(w.bodies[&id].volume, 0, "an emptied body kept water");
        w.remove_body(id);
        return;
    }
    if dropped {
        split_if_disconnected(w, id);
    }
}

/// Connected components of `members` (6-connected through members of `id`), each
/// sorted, in order of their lowest cell.
fn components(w: &mut World, members: &[u32], id: BodyId) -> Vec<Vec<u32>> {
    w.mark_gen += 1;
    let generation = w.mark_gen;
    let mut out = Vec::new();
    for &start in members {
        if w.mark[start as usize] == generation {
            continue;
        }
        w.mark[start as usize] = generation;
        let mut comp = vec![start];
        let mut k = 0;
        while k < comp.len() {
            let c = comp[k];
            k += 1;
            for d in ALL {
                if let Some(n) = w.dims.step(c, d) {
                    if w.owner[n as usize] == id && w.mark[n as usize] != generation {
                        w.mark[n as usize] = generation;
                        comp.push(n);
                    }
                }
            }
        }
        w.stats.flood_cells += comp.len() as u64;
        comp.sort_unstable();
        out.push(comp);
    }
    out
}

/// If `id` is no longer connected, split it. Every layer below the old top stays
/// full; the old top layer's water is shared by each part's cell count there
/// (remainder units go to the earliest parts). The first part keeps the id.
pub fn split_if_disconnected(w: &mut World, id: BodyId) {
    let members = all_members(w, id);
    let comps = components(w, &members, id);
    if comps.len() <= 1 {
        return;
    }
    w.stats.splits += 1;
    let old = w.remove_body(id);
    let t = old.top;
    let frac = old.frac();
    let at: Vec<u64> = comps
        .iter()
        .map(|c| c.iter().filter(|&&i| w.dims.y_of(i) == t).count() as u64)
        .collect();
    let total: u64 = at.iter().sum();
    let mut shares: Vec<u64> = at
        .iter()
        .map(|&a| if total == 0 { 0 } else { frac * a / total })
        .collect();
    let mut rem = frac - shares.iter().sum::<u64>();
    for k in 0..shares.len() {
        if rem == 0 {
            break;
        }
        if at[k] > 0 {
            shares[k] += 1;
            rem -= 1;
        }
    }
    assert_eq!(rem, 0, "split lost water");
    let mut ids = Vec::new();
    for (k, cells) in comps.iter().enumerate() {
        let nid = if k == 0 { id } else { w.alloc_id() };
        let mut b = Body::empty(nid, w.dims);
        for &c in cells {
            b.area[w.dims.y_of(c) as usize] += 1;
            b.grow_bbox(w.dims.xyz(c));
        }
        b.cells = cells.len() as u64;
        b.top = (0..w.dims.y)
            .rev()
            .find(|&y| b.area[y as usize] > 0)
            .expect("non-empty component");
        b.below = b.cells - b.top_area();
        b.volume = if b.top == t { F * b.below + shares[k] } else { F * b.cells };
        b.shore = old
            .shore
            .iter()
            .copied()
            .filter(|&s| {
                ALL.iter()
                    .filter_map(|&d| w.dims.step(s, d))
                    .any(|n| cells.binary_search(&n).is_ok())
            })
            .collect();
        if k > 0 {
            for &c in cells {
                w.set_owner(c, nid);
            }
        }
        w.insert_body(b);
        ids.push(nid);
    }
    for nid in ids {
        if w.bodies.contains_key(&nid) {
            drop_empty_layers(w, nid);
        }
    }
}

/// A full, quiet CA cell on solid ground starts a body: the connected patch of
/// full CA cells on solid ground in its layer becomes the body's first layer.
pub fn seed(w: &mut World, start: u32) -> BodyId {
    let dims = w.dims;
    let y = dims.y_of(start);
    w.mark_gen += 1;
    let generation = w.mark_gen;
    w.mark[start as usize] = generation;
    let mut patch = vec![start];
    let mut k = 0;
    while k < patch.len() {
        let c = patch[k];
        k += 1;
        for d in HORIZONTAL {
            if let Some(n) = dims.step(c, d) {
                if w.mark[n as usize] != generation
                    && w.is_ca(n)
                    && w.water[n as usize] == FULL
                    && on_solid(w, n)
                {
                    w.mark[n as usize] = generation;
                    patch.push(n);
                }
            }
        }
    }
    patch.sort_unstable();
    w.stats.flood_cells += patch.len() as u64;
    w.stats.seeds += 1;
    let id = w.alloc_id();
    let mut b = Body::empty(id, dims);
    b.top = y;
    w.insert_body(b);
    for c in patch {
        add_member(w, id, c);
    }
    id
}

/// Grow a full body by one layer (see the model section of the plan).
pub fn try_expand(w: &mut World, id: BodyId) -> bool {
    expand_layer(w, id, false)
}

/// `forced` skips the backoff, budget and plain/film checks (used by `relevel`).
fn expand_layer(w: &mut World, id: BodyId, forced: bool) -> bool {
    let dims = w.dims;
    let s = w.step;
    let (room, retry, top, top_area) = {
        let b = &w.bodies[&id];
        (b.room(), b.expand_retry, b.top, b.top_area())
    };
    if room > 0 || top + 1 >= dims.y || (!forced && s < retry) {
        return false;
    }
    let y = top + 1;
    w.mark_gen += 1;
    let generation = w.mark_gen;
    let mut cand: Vec<u32> = Vec::new();
    for m in layer_members(w, id, top) {
        if let Some(u) = dims.step(m, Dir::Up) {
            if w.is_ca(u) && w.mark[u as usize] != generation {
                w.mark[u as usize] = generation;
                cand.push(u);
            }
        }
    }
    let mut merge_with: Vec<BodyId> = Vec::new();
    let mut over = false;
    let mut k = 0;
    while k < cand.len() {
        if !forced && cand.len() as u32 > EXPAND_BUDGET {
            over = true;
            break;
        }
        let c = cand[k];
        k += 1;
        for d in HORIZONTAL {
            let Some(n) = dims.step(c, d) else { continue };
            if w.mark[n as usize] == generation {
                continue;
            }
            let o = w.owner[n as usize];
            if o != NO_BODY && o != id {
                if !merge_with.contains(&o) {
                    merge_with.push(o);
                }
                continue;
            }
            if w.is_ca(n) && supported_by(w, n, id) {
                w.mark[n as usize] = generation;
                cand.push(n);
            }
        }
    }
    w.stats.flood_cells += cand.len() as u64;
    if cand.is_empty() {
        return false;
    }
    let wet: u64 = cand.iter().map(|&c| w.water[c as usize] as u64).sum();
    if !forced && wet == 0 {
        return false; // ruling: a dry layer is never worth a body layer; no backoff
    }
    let bowl = cand.len() as u64 <= EXPAND_AREA_FACTOR as u64 * top_area;
    let filmy = wet >= FILM as u64 * cand.len() as u64;
    if !forced && (over || !(bowl || filmy)) {
        w.body_mut(id).expand_retry = s + EXPAND_BACKOFF;
        w.stats.expansion_refusals += 1;
        return false;
    }
    cand.sort_unstable();
    {
        let b = w.body_mut(id);
        b.below += b.top_area();
        b.top = y;
    }
    for c in cand {
        add_member(w, id, c);
    }
    w.stats.expansions += 1;
    merge_with.sort_unstable();
    let mut keep = id;
    for o in merge_with {
        if w.bodies.contains_key(&o) && w.bodies.contains_key(&keep) {
            keep = merge(w, keep, o);
        }
    }
    true
}

/// Cut membership back to layer `t0`, then regrow layer by layer while the volume
/// exceeds capacity. This re-derives the container after bodies at different
/// heights join (communicating vessels: one level, instantly).
pub fn relevel(w: &mut World, id: BodyId, t0: u32) {
    let top = w.bodies[&id].top;
    for y in (t0 + 1..=top).rev() {
        for c in layer_members(w, id, y) {
            w.set_owner(c, NO_BODY);
            w.set_active(c, true);
            w.touch(c);
        }
        let b = w.body_mut(id);
        b.cells -= b.area[y as usize] as u64;
        b.area[y as usize] = 0;
    }
    {
        let b = w.body_mut(id);
        b.top = t0;
        b.below = b.cells - b.top_area();
    }
    while w.bodies.contains_key(&id) && w.bodies[&id].volume > w.bodies[&id].capacity() {
        if !expand_layer(w, id, true) {
            break;
        }
    }
    if let Some(b) = w.bodies.get(&id) {
        assert!(b.volume <= b.capacity(), "relevel: no room to regrow");
    }
}

/// Join two bodies. The one with more cells survives (a tie goes to the lower id).
pub fn merge(w: &mut World, a: BodyId, b: BodyId) -> BodyId {
    let (keep, gone) = {
        let ka = (w.bodies[&a].cells, std::cmp::Reverse(a));
        let kb = (w.bodies[&b].cells, std::cmp::Reverse(b));
        if kb > ka { (b, a) } else { (a, b) }
    };
    let t0 = w.bodies[&a].top.min(w.bodies[&b].top);
    let moved = all_members(w, gone);
    let g = w.remove_body(gone);
    for &c in &moved {
        w.set_owner(c, keep);
    }
    let mut shore: Vec<u32> = w.bodies[&keep]
        .shore
        .iter()
        .chain(&g.shore)
        .copied()
        .filter(|&s| w.is_ca(s))
        .collect();
    shore.sort_unstable();
    shore.dedup();
    let k = w.body_mut(keep);
    k.volume += g.volume;
    for (x, y) in k.area.iter_mut().zip(&g.area) {
        *x += *y;
    }
    k.cells += g.cells;
    for i in 0..3 {
        k.lo[i] = k.lo[i].min(g.lo[i]);
        k.hi[i] = k.hi[i].max(g.hi[i]);
    }
    k.top = k.top.max(g.top);
    k.below = k.cells - k.top_area();
    k.shore = shore;
    w.stats.merges += 1;
    relevel(w, keep, t0);
    if w.bodies.contains_key(&keep) {
        drop_empty_layers(w, keep);
    }
    keep
}

/// Fixture setup: seed every full CA cell on solid ground, then expand until nothing grows.
pub fn settle_init(w: &mut World) {
    for c in 0..w.dims.len() as u32 {
        if w.is_ca(c) && w.water[c as usize] == FULL && on_solid(w, c) {
            seed(w, c);
        }
    }
    loop {
        let ids: Vec<BodyId> = w.bodies.keys().copied().collect();
        let mut grew = false;
        for id in ids {
            if w.bodies.contains_key(&id) && try_expand(w, id) {
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    for b in w.bodies.values_mut() {
        b.expand_retry = 0;
    }
}

/// Structural invariants. O(grid): for tests only.
pub fn check_invariants(w: &World) -> Result<(), String> {
    let mut counted: std::collections::BTreeMap<BodyId, u64> = Default::default();
    for i in 0..w.dims.len() as u32 {
        let (o, v) = (w.owner[i as usize], w.water[i as usize]);
        if w.solid.get(i) && (o != NO_BODY || v != 0) {
            return Err(format!("solid cell {i} holds water/owner"));
        }
        if v > FULL {
            return Err(format!("cell {i} over full: {v}"));
        }
        if o != NO_BODY {
            if v != 0 {
                return Err(format!("member {i} holds CA water"));
            }
            let Some(b) = w.bodies.get(&o) else { return Err(format!("{i} owned by dead body {o}")) };
            if w.dims.y_of(i) > b.top {
                return Err(format!("member {i} above top of {o}"));
            }
            *counted.entry(o).or_default() += 1;
        }
    }
    for (id, b) in &w.bodies {
        let sum: u64 = b.area.iter().map(|&a| a as u64).sum();
        if sum != b.cells || counted.get(id).copied().unwrap_or(0) != b.cells {
            return Err(format!("body {id}: area/cell count mismatch"));
        }
        if b.top_area() == 0 || b.below != b.cells - b.top_area() {
            return Err(format!("body {id}: bad top/below"));
        }
        if b.area[b.top as usize + 1..].iter().any(|&a| a != 0) {
            return Err(format!("body {id}: area above top"));
        }
        if !(F * b.below < b.volume && b.volume <= b.capacity()) {
            return Err(format!("body {id}: volume {} outside ({}, {}]", b.volume, F * b.below, b.capacity()));
        }
    }
    Ok(())
}
