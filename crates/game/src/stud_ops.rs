//! Stud chunks as sim entities: raw genesis, undo-safe runtime create/set
//! with collider sync, and the surface query spawning reads.
//!
//! A stud chunk is its own entity (slots `rigidbody` + `stud` at genesis,
//! every slot at runtime) with one fixed body at `StudChunkCoords::origin`
//! and exactly the colliders [`stud_collider_shapes`] lists, in that order,
//! each carrying the entity key as `user_data`. It lives in
//! `GameData::stud_index`.

use rapier3d::math::IVector;
use rapier3d::prelude::{Collider, ColliderBuilder, ColliderHandle, RigidBodyBuilder};
use slotmapd::Key as _;

use crate::block::CHUNK_UNITS;
use crate::stud::{
    plate_units, StudBrick, StudChunk, StudChunkCoords, StudShape, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, STUD_UNITS,
};

/// Largest box one carve may span, in stud cells per axis (`[studs, plates, studs]`):
/// 16 m x 18 m x 16 m. Bounds the work of a single order.
pub const STUD_CARVE_MAX: [i32; 3] = [32, 90, 32];
use crate::stud_ground::StudColumnGround;
use crate::stud_collider::{
    stud_collider_cells, stud_collider_shapes, stud_hulls, StudColliderCells, STUD_COARSE_CELL,
    STUD_FINE_CELL,
};
use crate::{
    body_slot_digest, EntityKey, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, Pose,
    Rollback,
};

thread_local! {
    /// Whole-shape stud collider syncs on this thread (an edit that could not
    /// be patched in place) — a test probe like `COLLIDER_REBUILDS`. Not
    /// state: never hashed, serialized or undone.
    static STUD_COLLIDER_REBUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `a \ b` for two key lists sorted by `(y, z, x)` without duplicates, in
/// `a` order.
fn sorted_minus(a: &[IVector], b: &[IVector]) -> Vec<IVector> {
    let key = |k: &IVector| (k.y, k.z, k.x);
    let mut out = Vec::new();
    let mut j = 0;
    for k in a {
        while j < b.len() && key(&b[j]) < key(k) {
            j += 1;
        }
        if j >= b.len() || b[j] != *k {
            out.push(*k);
        }
    }
    out
}

/// The colliders a stud chunk's body carries, in `stud_collider_shapes` order.
fn stud_colliders(chunk: &StudChunk, e: EntityKey) -> Vec<Collider> {
    stud_colliders_tagged(chunk, e.data().as_ffi() as u128)
}

/// [`stud_colliders`] with any `user_data` (a halo chunk's body has no entity).
pub(crate) fn stud_colliders_tagged(chunk: &StudChunk, user_data: u128) -> Vec<Collider> {
    stud_collider_shapes(chunk)
        .into_iter()
        .map(|shape| {
            // A fresh build: the join snapshot may leave it off the wire (`region_wire.rs`).
            crate::region_wire::note_fresh_shape(&shape);
            ColliderBuilder::new(shape).user_data(user_data).build()
        })
        .collect()
}

/// The empty cells of the chunk cell box `lo .. hi` as disjoint boxes `(min, size)`:
/// runs of plates over which the same columns are free become slabs, and each slab's
/// free columns are covered by rectangles found row by row (x first, then z), each
/// grown as far as it goes along x, then along z. Deterministic: a pure function of
/// the occupancy and the box.
fn free_boxes(occ: &crate::stud::StudOccupancy, lo: [u8; 3], hi: [u8; 3]) -> Vec<([u8; 3], [u8; 3])> {
    let mut out = Vec::new();
    if (0..3).any(|i| lo[i] >= hi[i]) {
        return out;
    }
    let (w, d) = ((hi[0] - lo[0]) as usize, (hi[2] - lo[2]) as usize);
    // Free columns of plate `y`, row-major (`z * w + x`).
    let mask = |y: u8| -> Vec<bool> {
        (0..d)
            .flat_map(|z| (0..w).map(move |x| (x, z)))
            .map(|(x, z)| !occ.get([lo[0] as i32 + x as i32, y as i32, lo[2] as i32 + z as i32]))
            .collect()
    };
    let mut y = lo[1];
    while y < hi[1] {
        let free = mask(y);
        let mut top = y + 1;
        while top < hi[1] && mask(top) == free {
            top += 1;
        }
        let mut taken = vec![false; w * d];
        for z in 0..d {
            for x in 0..w {
                if !free[z * w + x] || taken[z * w + x] {
                    continue;
                }
                let open = |x: usize, z: usize, taken: &[bool]| free[z * w + x] && !taken[z * w + x];
                let mut x1 = x + 1;
                while x1 < w && open(x1, z, &taken) {
                    x1 += 1;
                }
                let mut z1 = z + 1;
                while z1 < d && (x..x1).all(|x| open(x, z1, &taken)) {
                    z1 += 1;
                }
                for zz in z..z1 {
                    for xx in x..x1 {
                        taken[zz * w + xx] = true;
                    }
                }
                out.push((
                    [lo[0] + x as u8, y, lo[2] + z as u8],
                    [(x1 - x) as u8, top - y, (z1 - z) as u8],
                ));
            }
        }
        y = top;
    }
    out
}

/// The region stud cell box `(lo, hi)` (`hi` exclusive) of brick `b` of chunk `c`.
fn stud_brick_box(c: StudChunkCoords, b: &StudBrick) -> ([i32; 3], [i32; 3]) {
    let base = [
        (c.x * STUD_CHUNK_STUDS) as i32,
        c.y.saturating_mul(STUD_CHUNK_PLATES as i32),
        (c.z * STUD_CHUNK_STUDS) as i32,
    ];
    let size = b.size();
    let lo = [0, 1, 2].map(|a| base[a].saturating_add(i32::from(b.min[a])));
    (lo, [0, 1, 2].map(|a| lo[a].saturating_add(i32::from(size[a]))))
}

impl Rollback {
    /// Bulk stud-chunk build for region genesis: raw and unlogged
    /// (genesis is the base state, never rolled back below). An empty chunk still gets its entity and body.
    pub fn seed_stud_genesis(&mut self, chunks: Vec<(StudChunkCoords, StudChunk)>) {
        for (coords, chunk) in chunks {
            let e = self.ecs.create_stud_entity_raw();
            let colliders = stud_colliders(&chunk, e);
            self.ecs.stud.raw_mut().insert(e, Some(chunk));
            self.stud_index.insert_raw(coords, e);
            let body = RigidBodyBuilder::fixed()
                .pose(Pose::from_translation(coords.origin()))
                .user_data(e.data().as_ffi() as u128)
                .build();
            let handle = self.data.physics.raw_mut().bodies.raw_mut().insert(body);
            self.ecs.rigidbody.raw_mut().insert(e, Some(handle));
            let phys = self.data.physics.raw_mut();
            for collider in colliders {
                phys.colliders.raw_mut().insert_with_parent(
                    collider,
                    handle,
                    phys.bodies.raw_mut(),
                );
            }
        }
    }

    /// Genesis from stud ground recipes: stores them raw and seeds the pristine chunk
    /// of every surface layer of every chunk column, and of every layer under those
    /// that a lower column beside it opens onto (`stud_wall_layers`: a cliff between
    /// two chunk columns has bricks and a collider all the way down). Chunks below
    /// stay implied by the recipe until a carve reaches them.
    ///
    /// `overrides` replace (or add to) those chunks: content that is not a function of
    /// the recipe, such as a pristine chunk with feature bricks merged in.
    pub fn seed_stud_ground_genesis(
        &mut self,
        ground: Vec<(crate::ColumnCoords, StudColumnGround)>,
        mut overrides: std::collections::BTreeMap<StudChunkCoords, StudChunk>,
    ) {
        let mut chunks = Vec::new();
        let walls: Vec<std::collections::BTreeSet<i32>> = {
            let recipes: std::collections::BTreeMap<crate::ColumnCoords, &StudColumnGround> = ground.iter().map(|(c, g)| (*c, g)).collect();
            ground.iter().map(|(c, _)| crate::stud_ground::stud_wall_layers(&recipes, *c)).collect()
        };
        for ((col, g), walls) in ground.into_iter().zip(walls) {
            for y in walls.into_iter().chain(g.surface_layers()) {
                let c = StudChunkCoords { x: col.x, y, z: col.z };
                if let Some(chunk) = overrides.remove(&c).or_else(|| g.pristine_chunk(y)) {
                    chunks.push((c, chunk));
                }
            }
            self.stud_ground.insert_raw(col, g);
        }
        chunks.extend(overrides);
        self.seed_stud_genesis(chunks);
    }

    /// The chunk at `c` as the sim would see it: the materialised one, else the
    /// pristine chunk its ground recipe implies.
    pub fn stud_chunk_or_pristine(&self, c: StudChunkCoords) -> Option<std::borrow::Cow<'_, StudChunk>> {
        if let Some(chunk) = self.stud_chunk_at(c) {
            return Some(std::borrow::Cow::Borrowed(chunk));
        }
        let g = self.data.stud_ground.get(&crate::ColumnCoords::new(c.x, c.z))?;
        g.pristine_chunk(c.y).map(std::borrow::Cow::Owned)
    }

    /// The material at region stud cell `[stud x, plate y, stud z]` (plate 0 is the
    /// bottom of layer 0): from the materialised chunk when there is one (a chunk's
    /// edits win over the recipe), else from the ground recipe. `None` is air or
    /// outside the region.
    pub fn stud_material_at(&self, cell: [i32; 3]) -> Option<u16> {
        let (cw, ch) = (STUD_CHUNK_STUDS as i32, STUD_CHUNK_PLATES as i32);
        if cell[0] < 0 || cell[2] < 0 {
            return None;
        }
        let c = StudChunkCoords { x: (cell[0] / cw) as usize, y: cell[1].div_euclid(ch), z: (cell[2] / cw) as usize };
        let local = [(cell[0] % cw) as u8, cell[1].rem_euclid(ch) as u8, (cell[2] % cw) as u8];
        if let Some(chunk) = self.stud_chunk_at(c) {
            return chunk.brick_at(local).map(|b| b.material);
        }
        let g = self.data.stud_ground.get(&crate::ColumnCoords::new(c.x, c.z))?;
        g.material_at(local[0] as usize, cell[1], local[2] as usize)
    }

    /// The stud chunk entity at `c`, if any.
    pub fn stud_key_at(&self, c: StudChunkCoords) -> Option<EntityKey> {
        self.data.stud_index.get(&c).copied()
    }

    /// The stud chunk at `c`, if any.
    pub fn stud_chunk_at(&self, c: StudChunkCoords) -> Option<&StudChunk> {
        let key = self.stud_key_at(c)?;
        self.data.ecs.stud.try_get(key).as_ref()
    }

    /// A stud entity's coordinates, from its body (which sits exactly at
    /// `StudChunkCoords::origin`).
    pub(crate) fn stud_coords_of(&self, key: EntityKey) -> Option<StudChunkCoords> {
        let handle = (*self.data.ecs.rigidbody.try_get(key))?;
        let t = self.data.physics.bodies.get(handle)?.translation();
        Some(StudChunkCoords::from_origin(t))
    }

    /// Runtime stud-chunk insertion, undo-safe.
    /// Emits `AddStudChunk` on Do; on undo the entity insert's own emit
    /// sends `RemoveEntity`.
    pub fn create_stud_chunk_safe(&mut self, c: StudChunkCoords, chunk: StudChunk) -> EntityKey {
        let e = self.ecs.create_entity_safe();
        let colliders = stud_colliders(&chunk, e);
        self.ecs.stud.set_safe(e, Some(chunk.clone()));
        self.stud_index.insert(c, e);
        crate::stud_halo::touch_stud_chunk(c);
        let body = RigidBodyBuilder::fixed()
            .pose(Pose::from_translation(c.origin()))
            .user_data(e.data().as_ffi() as u128)
            .build();
        // Tier-2 with a true fork inverse + delta digest: reverts the arena
        // allocator exactly; verification hashes just this slot + alloc state.
        let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
        let mut scope = self.data.physics.bodies.undo_scope_digest();
        let handle = scope.insert(body);
        scope.register(
            body_slot_digest((prev_head, prev_len), None),
            move |bodies, _| {
                bodies.revert_insert(handle, prev_head, prev_len);
                body_slot_digest(bodies.alloc_state(), bodies.get(handle))
            },
        );
        self.ecs.rigidbody.set_safe(e, Some(handle));
        for collider in colliders {
            self.attach_collider_safe(handle, collider);
        }
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddStudChunk(e, c, Box::new(chunk)),
        ));
        e
    }

    /// Replaces a stud chunk's content, undo-safe, and syncs its colliders.
    /// Emits `SetStudChunk(new)` on Do and `SetStudChunk(old)` on Undo.
    ///
    /// Colliders are patched in place ([`Self::patch_stud_colliders_safe`])
    /// when the edit leaves the hulls alone; otherwise, or when patching
    /// cannot express the edit, the whole-shape sync runs.
    ///
    /// `key` must be a stud entity: anything else is a caller bug, and the
    /// call writes nothing (a stud write would graft a stud component and
    /// colliders onto a foreign entity).
    pub fn set_stud_chunk_safe(&mut self, key: EntityKey, chunk: StudChunk) {
        let Some(old) = self.data.ecs.stud.try_get(key).clone() else {
            debug_assert!(false, "set_stud_chunk_safe: {key:?} is not a stud chunk entity");
            return;
        };
        if let Some(c) = self.stud_coords_of(key) {
            crate::stud_halo::touch_stud_chunk(c);
        }
        let old_cells = stud_collider_cells(&old);
        let old_hulls = stud_hulls(&old);
        // LIFO: registered before the write, so the old chunk is re-sent
        // after the component value is restored.
        self.data.ecs.stud.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetStudChunk(key, Box::new(old)),
        ));
        self.data.ecs.stud.set_safe(key, Some(chunk.clone()));
        let new_hulls = stud_hulls(&chunk);
        if old_hulls == new_hulls {
            let new_cells = stud_collider_cells(&chunk);
            let n = old_hulls.len();
            self.patch_stud_colliders_safe(key, &old_cells, &new_cells, n, n);
        } else {
            self.full_stud_sync_safe(key);
        }
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetStudChunk(key, Box::new(chunk)),
        ));
    }

    /// Places `b` into stud chunk `c`, undo-safe (the `Order::PlaceStud`
    /// body). A missing chunk is created holding just the brick. `false`
    /// with no state touched: `c` outside the region's x/z chunk range, the
    /// brick not a catalog part (`StudShape::Filler` is worldgen-only), or the
    /// brick invalid / overlapping.
    pub fn place_stud_safe(&mut self, c: StudChunkCoords, b: StudBrick) -> bool {
        if !matches!(b.shape, StudShape::Part { .. }) {
            return false;
        }
        if c.x >= crate::REGION_CHUNKS
            || c.z >= crate::REGION_CHUNKS
            || !crate::stud::stud_layer_ok(c.y)
        {
            return false;
        }
        match self.stud_key_at(c) {
            Some(key) => {
                let Some(mut chunk) = self.data.ecs.stud.try_get(key).clone() else {
                    return false;
                };
                if chunk.place(b).is_err() {
                    return false;
                }
                self.set_stud_chunk_safe(key, chunk);
            }
            None => {
                let Ok(chunk) = StudChunk::new([b]) else {
                    return false;
                };
                self.create_stud_chunk_safe(c, chunk);
            }
        }
        let (lo, hi) = stud_brick_box(c, &b);
        let _destroyed = self.stud_piles_after_write_safe(lo, hi);
        true
    }

    /// Removes the brick covering `cell` of stud chunk `c`, undo-safe (the
    /// `Order::RemoveStud` body). `false`, nothing touched, when there is no
    /// such chunk or brick.
    pub fn remove_stud_safe(&mut self, c: StudChunkCoords, cell: [u8; 3]) -> bool {
        // No range check: `stud_index` only holds in-region, in-layer chunks.
        let Some(key) = self.stud_key_at(c) else {
            return false;
        };
        let Some(mut chunk) = self.data.ecs.stud.try_get(key).clone() else {
            return false;
        };
        let Some(removed) = chunk.remove_at(cell) else {
            return false;
        };
        self.set_stud_chunk_safe(key, chunk);
        let (lo, hi) = stud_brick_box(c, &removed);
        let _destroyed = self.stud_containers_after_write_safe(lo, hi);
        let _destroyed = self.stud_piles_after_write_safe(lo, hi);
        true
    }

    /// Carves the box of region stud cells `lo .. hi` (`[stud x, plate y, stud z]`, `hi`
    /// exclusive; plate 0 is the bottom of layer 0) out of every stud chunk it meets,
    /// undo-safe. Returns the removed cell count per material, ascending by material.
    /// Empty with no state touched: an empty box, one larger than [`STUD_CARVE_MAX`] on
    /// any axis, or nothing diggable inside it. The box is clipped to the region.
    pub fn carve_stud_safe(&mut self, lo: [i32; 3], hi: [i32; 3]) -> Vec<(u16, u32)> {
        self.carve_stud_boxes_safe(&[(lo, hi)]).pop().unwrap_or_default()
    }

    /// [`Self::carve_stud_safe`] for several boxes at once (a batch of dug tiles): the
    /// same cells go, but each chunk is written once however many boxes cut it, and
    /// the implied chunks the holes open onto are materialised once. Returns each
    /// box's removed cell count per material, in `boxes` order (empty for a box that
    /// removed nothing or is refused as `carve_stud_safe` refuses it). Boxes are
    /// carved in order, so a cell two boxes share counts for the first.
    pub fn carve_stud_boxes_safe(&mut self, boxes: &[([i32; 3], [i32; 3])]) -> Vec<Vec<(u16, u32)>> {
        use std::collections::{BTreeMap, BTreeSet};
        let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
        let n = crate::REGION_CHUNKS as i64 * cw;
        let size = [cw, ch, cw];
        // Chunk coordinates touched by the cell box `lo .. hi`, ascending.
        let chunks_in = |lo: [i64; 3], hi: [i64; 3]| {
            let first = [lo[0].div_euclid(cw), lo[1].div_euclid(ch), lo[2].div_euclid(cw)];
            let last = [(hi[0] - 1).div_euclid(cw), (hi[1] - 1).div_euclid(ch), (hi[2] - 1).div_euclid(cw)];
            let mut out = Vec::new();
            for cx in first[0]..=last[0] {
                for cy in first[1]..=last[1] {
                    for cz in first[2]..=last[2] {
                        if let Ok(y) = i32::try_from(cy) {
                            if crate::stud::stud_layer_ok(y) {
                                out.push(StudChunkCoords { x: cx as usize, y, z: cz as usize });
                            }
                        }
                    }
                }
            }
            out
        };
        // Plan first (materialised chunks, or the pristine ones the recipe implies), so
        // a carve that removes nothing touches no state. `None`: nothing at that chunk.
        let mut work: BTreeMap<StudChunkCoords, Option<(StudChunk, bool)>> = BTreeMap::new();
        let mut totals = Vec::with_capacity(boxes.len());
        // Chunks within one cell of a box that removed something.
        let mut opened: BTreeSet<StudChunkCoords> = BTreeSet::new();
        // The (clipped) boxes that removed something: piles there settle again.
        let mut written: Vec<([i32; 3], [i32; 3])> = Vec::new();
        for &(lo, hi) in boxes {
            let mut total: BTreeMap<u16, u32> = BTreeMap::new();
            let (lo, hi) = (lo.map(i64::from), hi.map(i64::from));
            let refused = (0..3).any(|i| hi[i] <= lo[i] || hi[i] - lo[i] > STUD_CARVE_MAX[i] as i64);
            let lo = [lo[0].max(0), lo[1], lo[2].max(0)];
            let hi = [hi[0].min(n), hi[1], hi[2].min(n)];
            if refused || lo[0] >= hi[0] || lo[2] >= hi[2] {
                totals.push(Vec::new());
                continue;
            }
            for c in chunks_in(lo, hi) {
                let slot = work
                    .entry(c)
                    .or_insert_with(|| self.stud_chunk_or_pristine(c).map(|chunk| (chunk.into_owned(), false)));
                let Some((chunk, changed)) = slot else { continue };
                let base = [c.x as i64 * cw, c.y as i64 * ch, c.z as i64 * cw];
                let l: [u8; 3] = std::array::from_fn(|i| (lo[i] - base[i]).clamp(0, size[i]) as u8);
                let h: [u8; 3] = std::array::from_fn(|i| (hi[i] - base[i]).clamp(0, size[i]) as u8);
                for (m, cells) in chunk.carve(l, h) {
                    *total.entry(m).or_default() += cells;
                    *changed = true;
                }
            }
            if !total.is_empty() {
                // The hole may open onto chunks that were only implied: every chunk
                // within one cell of it (x/z clipped to the region).
                let grow_lo = [(lo[0] - 1).max(0), lo[1] - 1, (lo[2] - 1).max(0)];
                let grow_hi = [(hi[0] + 1).min(n), hi[1] + 1, (hi[2] + 1).min(n)];
                opened.extend(chunks_in(grow_lo, grow_hi));
                written.push((lo.map(|v| v as i32), hi.map(|v| v as i32)));
            }
            totals.push(total.into_iter().collect());
        }
        for (c, slot) in work {
            let Some((chunk, true)) = slot else { continue };
            match self.stud_key_at(c) {
                Some(key) => self.set_stud_chunk_safe(key, chunk),
                None => {
                    self.create_stud_chunk_safe(c, chunk);
                }
            }
        }
        // Give the opened chunks their bricks and colliders.
        for c in opened {
            if self.stud_key_at(c).is_some() {
                continue;
            }
            let pristine = self
                .data
                .stud_ground
                .get(&crate::ColumnCoords::new(c.x, c.z))
                .and_then(|g| g.pristine_chunk(c.y));
            if let Some(chunk) = pristine {
                self.create_stud_chunk_safe(c, chunk);
            }
        }
        for (lo, hi) in written {
            // A chest whose bricks went spills first, so its pile settles with the rest.
            let _destroyed = self.stud_containers_after_write_safe(lo, hi);
            let _destroyed = self.stud_piles_after_write_safe(lo, hi);
        }
        totals
    }

    /// [`Self::carve_stud_boxes_safe`], with what each box removed put down as item
    /// piles (`mining::DropTally`: an item per tile's worth of cells across the batch,
    /// the rest rolled) in
    /// this region, at the tile under the middle of the box's lowest plates (settled
    /// from there like any deposit, spilling to the cells around a full pile). What a
    /// dig, a deconstruction and a neighbour's carve all go through, so a carve
    /// yields the same on either side of a seam. Returns what the carve returns.
    pub fn carve_stud_boxes_dropping_safe(&mut self, boxes: &[([i32; 3], [i32; 3])]) -> Vec<Vec<(u16, u32)>> {
        let removed = self.carve_stud_boxes_safe(boxes);
        let tick = *self.data.tick;
        let n = (crate::REGION_CHUNKS * STUD_CHUNK_STUDS) as i32;
        // The tile under the middle of the lowest plates of the box as carved
        // (clipped to the region).
        let tile_of = |lo: [i32; 3], hi: [i32; 3]| {
            let (x0, x1) = (lo[0].max(0), hi[0].min(n));
            let (z0, z1) = (lo[2].max(0), hi[2].min(n));
            [
                (x0 + (x1 - x0 - 1) / 2).div_euclid(crate::TILE_STUDS as i32),
                lo[1].div_euclid(crate::TILE_PLATES as i32),
                (z0 + (z1 - z0 - 1) / 2).div_euclid(crate::TILE_STUDS as i32),
            ]
        };
        let mut tally = crate::mining::DropTally::default();
        let mut last = None;
        for (&(lo, hi), cells) in boxes.iter().zip(&removed) {
            if cells.is_empty() {
                continue;
            }
            last = Some((lo, hi));
            let drops = tally.add(cells);
            if !drops.is_empty() {
                self.deposit_drops_safe(tile_of(lo, hi), &drops, tick);
            }
        }
        // What is left of a tile's worth is rolled, at the last box carved.
        if let Some((lo, hi)) = last {
            let subject = crate::mining::drop_subject(crate::world_stud_cell(*self.data.region, lo.map(i64::from)));
            let drops = tally.finish(tick, subject);
            if !drops.is_empty() {
                self.deposit_drops_safe(tile_of(lo, hi), &drops, tick);
            }
        }
        removed
    }

    /// Fills the empty cells of the box of region stud cells `lo .. hi` (as
    /// [`Self::carve_stud_safe`]'s) with catalog box parts of `material`
    /// (`stud_carve::fill_box`), undo-safe; occupied cells are left as they are.
    /// Returns the number of cells filled. 0 with no state touched: an empty box, one
    /// larger than [`STUD_CARVE_MAX`] on any axis, an unknown material, or no empty
    /// cell inside it. The box is clipped to the region and the legal layers; implied
    /// ground it reaches into is materialised (the chunk becomes its pristine bricks
    /// plus the fill).
    pub fn fill_stud_box_safe(&mut self, lo: [i32; 3], hi: [i32; 3], material: u16) -> u32 {
        self.fill_stud_boxes_safe(&[(lo, hi, material)]).pop().unwrap_or(0)
    }

    /// [`Self::fill_stud_box_safe`] for several boxes at once (a batch of built
    /// tiles), each chunk written once. Returns each box's filled cell count, in
    /// `boxes` order. Boxes are filled in order: a cell two boxes share goes to the
    /// first.
    pub fn fill_stud_boxes_safe(&mut self, boxes: &[([i32; 3], [i32; 3], u16)]) -> Vec<u32> {
        let tagged: Vec<_> = boxes.iter().map(|&(lo, hi, material)| (lo, hi, material, 0)).collect();
        self.fill_stud_boxes_tagged_safe(&tagged)
    }

    /// [`Self::fill_stud_boxes_safe`] with each box's bricks carrying its `data` tag
    /// (the 4th element; 0 for plain builds — a chest's bricks carry
    /// `containers::STUD_CHEST_TAG`).
    pub(crate) fn fill_stud_boxes_tagged_safe(&mut self, boxes: &[([i32; 3], [i32; 3], u16, u16)]) -> Vec<u32> {
        use std::collections::BTreeMap;
        let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
        let n = crate::REGION_CHUNKS as i64 * cw;
        let size = [cw, ch, cw];
        // Working copies: the chunk as the sim sees it (materialised, pristine, or
        // empty) and whether a fill changed it.
        let mut work: BTreeMap<StudChunkCoords, (StudChunk, crate::stud::StudOccupancy, bool)> = BTreeMap::new();
        let mut totals = Vec::with_capacity(boxes.len());
        for &(lo, hi, material, data) in boxes {
            let (lo, hi) = (lo.map(i64::from), hi.map(i64::from));
            let refused = (0..3).any(|i| hi[i] <= lo[i] || hi[i] - lo[i] > STUD_CARVE_MAX[i] as i64)
                || crate::stud_material::stud_material(material).is_none();
            let lo = [lo[0].max(0), lo[1], lo[2].max(0)];
            let hi = [hi[0].min(n), hi[1], hi[2].min(n)];
            if refused || lo[0] >= hi[0] || lo[2] >= hi[2] {
                totals.push(0);
                continue;
            }
            let first = [lo[0].div_euclid(cw), lo[1].div_euclid(ch), lo[2].div_euclid(cw)];
            let last = [(hi[0] - 1).div_euclid(cw), (hi[1] - 1).div_euclid(ch), (hi[2] - 1).div_euclid(cw)];
            let mut filled = 0u32;
            for cx in first[0]..=last[0] {
                for cy in first[1]..=last[1] {
                    for cz in first[2]..=last[2] {
                        let Some(y) = i32::try_from(cy).ok().filter(|y| crate::stud::stud_layer_ok(*y)) else { continue };
                        let c = StudChunkCoords { x: cx as usize, y, z: cz as usize };
                        let (chunk, occ, changed) = work.entry(c).or_insert_with(|| {
                            let chunk = self.stud_chunk_or_pristine(c).map(|c| c.into_owned()).unwrap_or_default();
                            let occ = chunk.occupancy();
                            (chunk, occ, false)
                        });
                        let base = [cx * cw, cy * ch, cz * cw];
                        let l: [u8; 3] = std::array::from_fn(|i| (lo[i] - base[i]).clamp(0, size[i]) as u8);
                        let h: [u8; 3] = std::array::from_fn(|i| (hi[i] - base[i]).clamp(0, size[i]) as u8);
                        let mut bricks = Vec::new();
                        for (min, size) in free_boxes(occ, l, h) {
                            crate::stud_carve::fill_box(min, size, material, data, &mut bricks);
                        }
                        for b in bricks {
                            // Disjoint from the chunk by construction; a refusal
                            // (never expected) leaves the cells empty and uncounted.
                            if chunk.place(b).is_ok() {
                                for cell in b.cells() {
                                    occ.set(cell.map(i32::from), true);
                                    filled += 1;
                                }
                                *changed = true;
                            }
                        }
                    }
                }
            }
            totals.push(filled);
        }
        for (c, (chunk, _, changed)) in work {
            if !changed {
                continue;
            }
            match self.stud_key_at(c) {
                Some(key) => self.set_stud_chunk_safe(key, chunk),
                None => {
                    self.create_stud_chunk_safe(c, chunk);
                }
            }
        }
        for (&(lo, hi, _, _), &cells) in boxes.iter().zip(&totals) {
            if cells > 0 {
                let _destroyed = self.stud_piles_after_write_safe(lo, hi);
            }
        }
        totals
    }

    /// How many stud edits on this thread fell back to the whole-shape
    /// collider sync instead of patching the grids. Test hook only.
    #[doc(hidden)]
    pub fn stud_collider_rebuilds() -> u64 {
        STUD_COLLIDER_REBUILDS.with(|c| c.get())
    }

    fn full_stud_sync_safe(&mut self, key: EntityKey) {
        STUD_COLLIDER_REBUILDS.with(|c| c.set(c.get() + 1));
        self.sync_stud_colliders_safe(key);
    }

    /// Patches stud body `key`'s coarse/fine `Voxels` grids in place for one
    /// edit: `old` / `new` are the chunk's collider cells before and after,
    /// `old_hulls` / `new_hulls` its hull counts (the caller has checked the
    /// hull point lists are equal). Each grid's diff — removals in `old`
    /// order, then additions in `new` order — goes into its shape with one
    /// `set_voxels` through `mutate_collider_safe`, exactly as
    /// `edit_block_colliders_safe` does for terrain; an edited shape hashes
    /// equal to a fresh build of the same keys.
    ///
    /// Falls back to the whole-shape sync where terrain falls back: the
    /// enabled colliders are not exactly `[coarse?, fine?, hulls…]` as
    /// `stud_collider_shapes` lists them for `old`, a grid gains its first
    /// cell or loses its last, or the hull count changes.
    fn patch_stud_colliders_safe(
        &mut self,
        key: EntityKey,
        old: &StudColliderCells,
        new: &StudColliderCells,
        old_hulls: usize,
        new_hulls: usize,
    ) {
        let Some(body) = *self.data.ecs.rigidbody.try_get(key) else {
            return;
        };
        let Some(b) = self.data.physics.bodies.get(body) else {
            return;
        };
        let grids = [
            (STUD_COARSE_CELL, &old.coarse, &new.coarse),
            (STUD_FINE_CELL, &old.fine, &new.fine),
        ];
        let mut patchable = old_hulls == new_hulls
            && grids
                .iter()
                .all(|(_, o, n)| o.is_empty() == n.is_empty());
        // The enabled colliders must be the grids `old` has, coarsest first,
        // then exactly `old_hulls` non-voxel shapes.
        let mut handles: [Option<ColliderHandle>; 2] = [None, None];
        if patchable {
            let enabled: Vec<_> = b
                .colliders()
                .iter()
                .copied()
                .filter(|&ch| {
                    self.data
                        .physics
                        .colliders
                        .get(ch)
                        .is_some_and(|c| c.is_enabled())
                })
                .collect();
            let mut it = enabled.iter().copied().peekable();
            for (i, (size, o, _)) in grids.iter().enumerate() {
                if o.is_empty() {
                    continue;
                }
                let Some(ch) = it.next() else {
                    patchable = false;
                    break;
                };
                let ok = self
                    .data
                    .physics
                    .colliders
                    .get(ch)
                    .and_then(|c| c.shape().as_voxels())
                    .is_some_and(|v| v.voxel_size() == *size);
                if !ok {
                    patchable = false;
                    break;
                }
                handles[i] = Some(ch);
            }
            if patchable {
                let hulls: Vec<_> = it.collect();
                patchable = hulls.len() == old_hulls
                    && hulls.iter().all(|&ch| {
                        self.data
                            .physics
                            .colliders
                            .get(ch)
                            .is_some_and(|c| c.shape().as_voxels().is_none())
                    });
            }
        }
        if !patchable {
            self.full_stud_sync_safe(key);
            return;
        }
        for (i, (_, o, n)) in grids.iter().enumerate() {
            let removed = sorted_minus(o, n);
            let added = sorted_minus(n, o);
            if removed.is_empty() && added.is_empty() {
                continue;
            }
            let ch = handles[i].expect("checked: a non-empty grid has its collider");
            let edits: Vec<_> = removed
                .into_iter()
                .map(|k| (k, false))
                .chain(added.into_iter().map(|k| (k, true)))
                .collect();
            self.data.physics.colliders.mutate_collider_safe(ch, move |c| {
                c.shape_mut()
                    .as_voxels_mut()
                    .expect("checked: a voxels shape")
                    .set_voxels(edits);
            });
        }
    }

    /// Brings a stud body's colliders in line with `stud_collider_shapes`:
    /// existing colliders are reused in order (shape set, re-enabled), spares
    /// disabled, missing ones attached — `rebuild_body_colliders_safe`'s
    /// pattern. An empty chunk keeps its body with every collider disabled.
    fn sync_stud_colliders_safe(&mut self, key: EntityKey) {
        let Some(body) = *self.data.ecs.rigidbody.try_get(key) else {
            return;
        };
        let Some(chunk) = self.data.ecs.stud.try_get(key).as_ref() else {
            return;
        };
        let wanted = stud_colliders(chunk, key);
        self.sync_body_colliders_to_safe(body, wanted);
    }

    /// Brings `body`'s colliders in line with `wanted`, in order (see
    /// [`Self::sync_stud_colliders_safe`]; also the stud halo's chunks,
    /// `stud_halo.rs`).
    pub(crate) fn sync_body_colliders_to_safe(&mut self, body: rapier3d::prelude::RigidBodyHandle, wanted: Vec<Collider>) {
        let existing: Vec<_> = match self.data.physics.bodies.get(body) {
            Some(b) => b.colliders().to_vec(),
            None => return,
        };
        let mut wanted = wanted.into_iter();
        for ch in existing {
            match wanted.next() {
                Some(w) => {
                    let shape = w.shared_shape().clone();
                    self.data
                        .physics
                        .colliders
                        .mutate_collider_safe(ch, move |c| {
                            c.set_shape(shape);
                            if c.position_wrt_parent()
                                .is_some_and(|p| *p != Pose::IDENTITY)
                            {
                                c.set_position_wrt_parent(Pose::IDENTITY);
                            }
                            if !c.is_enabled() {
                                c.set_enabled(true);
                            }
                        });
                }
                None => {
                    let enabled = self
                        .data
                        .physics
                        .colliders
                        .get(ch)
                        .is_some_and(|c| c.is_enabled());
                    if enabled {
                        self.data
                            .physics
                            .colliders
                            .mutate_collider_safe(ch, |c| c.set_enabled(false));
                    }
                }
            }
        }
        for w in wanted {
            self.attach_collider_safe(body, w);
        }
    }

    /// The top surface (region-local units) of the stud column holding
    /// `(x, z)`: the highest `origin.y + plate_units(top_plate)` over that
    /// column's stud chunks. `None` outside the region or over no bricks.
    pub fn stud_surface_y(&self, x: f32, z: f32) -> Option<f32> {
        if x < 0.0 || z < 0.0 {
            return None;
        }
        let (ux, uz) = (x as usize, z as usize);
        let (cx, cz) = (ux / CHUNK_UNITS, uz / CHUNK_UNITS);
        let sx = ((ux % CHUNK_UNITS) / STUD_UNITS) as u8;
        let sz = ((uz % CHUNK_UNITS) / STUD_UNITS) as u8;
        let lo = StudChunkCoords {
            x: cx,
            y: i32::MIN,
            z: cz,
        };
        let hi = StudChunkCoords {
            x: cx,
            y: i32::MAX,
            z: cz,
        };
        // Keys order by (x, y, z): the range also spans other z; skip them.
        for (coords, &e) in self.data.stud_index.range(lo..=hi).rev() {
            if coords.z != cz {
                continue;
            }
            let Some(chunk) = self.data.ecs.stud.try_get(e).as_ref() else {
                continue;
            };
            if let Some(top) = chunk.top_plate(sx, sz) {
                return Some(coords.origin().y + plate_units(top as i32));
            }
        }
        None
    }
}
