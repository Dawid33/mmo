use super::VoxelsChunk;
use crate::math::{Point, Real, Vector, DIM};
use crate::shape::{VoxelState, Voxels};

impl Voxels {
    /// Updates the state of the neighbors of the voxel `key`.
    ///
    /// Modifies the state of the neighbors of `key` to account for it being empty or full.
    /// Returns (but doesn’t modify) the new state of the voxel specified by `key`.
    #[must_use]
    pub(super) fn update_neighbors_state(
        &mut self,
        key: Point<i32>,
        center_is_empty: bool,
    ) -> VoxelState {
        let mut key_data = 0;

        for k in 0..DIM {
            let mut left = key;
            let mut right = key;
            left[k] -= 1;
            right[k] += 1;

            // TODO PERF: all the calls to `linear_index` result in a hashmap lookup each time.
            //            We should instead be smarter and detect if left/right are in the same chunk
            //            to only look it up once.
            if let Some(left_id) = self.linear_index(left) {
                let left_state = &mut self.chunks[left_id.chunk_id].states[left_id.id_in_chunk];
                if !left_state.is_empty() {
                    if center_is_empty {
                        left_state.0 &= !(1 << (k * 2));
                    } else {
                        left_state.0 |= 1 << (k * 2);
                        key_data |= 1 << (k * 2 + 1);
                    }
                }
            }

            if let Some(right_id) = self.linear_index(right) {
                let right_state = &mut self.chunks[right_id.chunk_id].states[right_id.id_in_chunk];
                if !right_state.is_empty() {
                    if center_is_empty {
                        right_state.0 &= !(1 << (k * 2 + 1));
                    } else {
                        right_state.0 |= 1 << (k * 2 + 1);
                        key_data |= 1 << (k * 2);
                    }
                }
            }
        }

        if center_is_empty {
            VoxelState::EMPTY
        } else {
            VoxelState::new(key_data)
        }
    }

    /// Recomputes every voxel's neighborhood state from the occupancy alone.
    ///
    /// LOCAL PATCH (voxel MMO fork): the straightforward version (kept below as
    /// `recompute_all_voxels_states_reference`) resolves each voxel and each of
    /// its `2 * DIM` neighbors through `linear_index`, i.e. a `chunk_headers`
    /// hash lookup per neighbor per voxel slot. Building the terrain colliders
    /// of one game region spent ~72% of its time there (2026-09-19 profile).
    /// A neighbor is almost always in the same internal chunk, so resolve the
    /// chunk and its `2 * DIM` adjacent chunks ONCE, then index directly.
    ///
    /// The result is identical: a state depends only on which neighbors are
    /// empty, and emptiness (`EMPTY_FACE_MASK`) never changes here — computed
    /// states only use the low `2 * DIM` face bits — so evaluation order and
    /// the in-place update cannot matter.
    pub(super) fn recompute_all_voxels_states(&mut self) {
        const D: usize = VoxelsChunk::VOXELS_PER_CHUNK_DIM;
        let mut strides = [1usize; DIM];
        for k in 1..DIM {
            strides[k] = strides[k - 1] * D;
        }
        let chunk_ids: alloc::vec::Vec<(Point<i32>, usize)> = self
            .chunk_headers
            .iter()
            .map(|(key, header)| (*key, header.id))
            .collect();

        for (chunk_key, chunk_id) in chunk_ids {
            // [axis][0 = next (+1), 1 = prev (-1)]
            let mut adjacent = [[None::<usize>; 2]; DIM];
            for k in 0..DIM {
                let (mut next, mut prev) = (chunk_key, chunk_key);
                next[k] += 1;
                prev[k] -= 1;
                adjacent[k][0] = self.chunk_headers.get(&next).map(|h| h.id);
                adjacent[k][1] = self.chunk_headers.get(&prev).map(|h| h.id);
            }

            let mut states = self.chunks[chunk_id].states;
            for id_in_chunk in 0..VoxelsChunk::VOXELS_PER_CHUNK {
                if self.chunks[chunk_id].states[id_in_chunk].is_empty() {
                    states[id_in_chunk] = VoxelState::EMPTY;
                    continue;
                }
                let mut occupied_faces = 0;
                for k in 0..DIM {
                    let local = (id_in_chunk / strides[k]) % D;
                    // +1 along k: same chunk, or the first layer of the next chunk.
                    let next_filled = if local + 1 < D {
                        !self.chunks[chunk_id].states[id_in_chunk + strides[k]].is_empty()
                    } else {
                        adjacent[k][0].is_some_and(|c| {
                            !self.chunks[c].states[id_in_chunk - (D - 1) * strides[k]].is_empty()
                        })
                    };
                    // -1 along k: same chunk, or the last layer of the previous chunk.
                    let prev_filled = if local > 0 {
                        !self.chunks[chunk_id].states[id_in_chunk - strides[k]].is_empty()
                    } else {
                        adjacent[k][1].is_some_and(|c| {
                            !self.chunks[c].states[id_in_chunk + (D - 1) * strides[k]].is_empty()
                        })
                    };
                    if next_filled {
                        occupied_faces |= 1 << (k * 2);
                    }
                    if prev_filled {
                        occupied_faces |= 1 << (k * 2 + 1);
                    }
                }
                states[id_in_chunk] = VoxelState::new(occupied_faces);
            }
            self.chunks[chunk_id].states = states;
        }
    }

    /// The upstream per-voxel implementation; the behavioural reference for
    /// the patched `recompute_all_voxels_states`.
    #[cfg(test)]
    pub(super) fn recompute_all_voxels_states_reference(&mut self) {
        for (chunk_key, chunk_header) in self.chunk_headers.iter() {
            for id_in_chunk in 0..VoxelsChunk::VOXELS_PER_CHUNK {
                let voxel_key = VoxelsChunk::voxel_key_at_id(*chunk_key, id_in_chunk as u32);
                self.chunks[chunk_header.id].states[id_in_chunk] =
                    self.compute_voxel_state(voxel_key);
            }
        }
    }

    #[cfg(test)]
    fn compute_voxel_state(&self, key: Point<i32>) -> VoxelState {
        let Some(id) = self.linear_index(key) else {
            return VoxelState::EMPTY;
        };

        if self.chunks[id.chunk_id].states[id.id_in_chunk].is_empty() {
            return VoxelState::EMPTY;
        }

        self.compute_voxel_neighborhood_bits(key)
    }

    pub(super) fn compute_voxel_neighborhood_bits(&self, key: Point<i32>) -> VoxelState {
        let mut occupied_faces = 0;

        for k in 0..DIM {
            let (mut prev, mut next) = (key, key);
            prev[k] -= 1;
            next[k] += 1;

            if let Some(next_id) = self.linear_index(next) {
                if !self.chunks[next_id.chunk_id].states[next_id.id_in_chunk].is_empty() {
                    occupied_faces |= 1 << (k * 2);
                }
            }
            if let Some(prev_id) = self.linear_index(prev) {
                if !self.chunks[prev_id.chunk_id].states[prev_id.id_in_chunk].is_empty() {
                    occupied_faces |= 1 << (k * 2 + 1);
                }
            }
        }

        VoxelState::new(occupied_faces)
    }

    /// Merges voxel state (neighborhood) information of a given voxel (and all its neighbors)
    /// from `self` and `other`, to account for a recent change to the given `voxel` in `self`.
    ///
    /// This is designed to be called after `self` was modified with [`Voxels::set_voxel`].
    ///
    /// This is the same as [`Voxels::combine_voxel_states`] but localized to a single voxel and its
    /// neighbors.
    pub fn propagate_voxel_change(
        &mut self,
        other: &mut Self,
        voxel: Point<i32>,
        origin_shift: Vector<i32>,
    ) {
        let center_is_empty = self
            .voxel_state(voxel)
            .map(|vox| vox.is_empty())
            .unwrap_or(true);
        let center_state_delta =
            other.update_neighbors_state(voxel - origin_shift, center_is_empty);

        if let Some(vid) = self.linear_index(voxel) {
            self.chunks[vid.chunk_id].states[vid.id_in_chunk].0 |= center_state_delta.0;
        }
    }

    /// Merges voxel state (neighborhood) information of each voxel from `self` and `other`.
    ///
    /// This allows each voxel from one shape to be aware of the presence of neighbors belonging to
    /// the other so that collision detection is capable of transitioning between the boundaries of
    /// one shape to the other without hitting an internal edge.
    ///
    /// Both voxels shapes are assumed to have the same [`Self::voxel_size`].
    /// If `other` lives in a coordinate space with a different origin than `self`, then
    /// `origin_shift` represents the distance (as a multiple of the `voxel_size`) from the origin
    /// of `self` to the origin of `other`. Therefore, a voxel with coordinates `key` on `other`
    /// will have coordinates `key + origin_shift` on `self`.
    pub fn combine_voxel_states(&mut self, other: &mut Self, origin_shift: Vector<i32>) {
        let one = Vector::repeat(1);
        let origin_shift_worldspace = origin_shift.cast::<Real>().component_mul(&self.voxel_size);

        for chunk_key in &self.chunk_keys {
            let mut aabb = VoxelsChunk::aabb(chunk_key, &self.voxel_size);
            // Enlarge by one-half voxel so we detect cases where we also detect neighbor chunks from `other`.
            aabb.mins -= self.voxel_size / Real::from(2.0);
            aabb.maxs += self.voxel_size / Real::from(2.0);
            // Shift to the local coordinate system of `other`.
            let shifted_aabb = aabb.translated(&-origin_shift_worldspace);

            if other.chunk_bvh.intersect_aabb(&shifted_aabb).any(|_| true) {
                // Check the voxels from this chunk against the other voxels shape.

                // Iterate on the domain intersection. If the voxel exists (and is non-empty) on both shapes, we
                // simply need to combine their bitmasks. If it doesn’t exist on both shapes, we need to
                // actually check the neighbors.
                //
                // The `domain` is expressed in the grid coordinate space of `self`.
                let mut domain = VoxelsChunk::keys_bounds(chunk_key);
                // Enlarge the domain by one voxel so that voxels from `other` but not existing in `self` are updated too.
                domain[0] -= one;
                domain[1] += one;

                for i in domain[0].x..domain[1].x {
                    for j in domain[0].y..domain[1].y {
                        #[cfg(feature = "dim2")]
                        let k_range = 0..1;
                        #[cfg(feature = "dim3")]
                        let k_range = domain[0].z..domain[1].z;
                        for _k in k_range {
                            #[cfg(feature = "dim2")]
                            let key0 = Point::new(i, j);
                            #[cfg(feature = "dim3")]
                            let key0 = Point::new(i, j, _k);
                            let key1 = key0 - origin_shift;
                            let vox0 = self
                                .linear_index(key0)
                                .map(|id| &mut self.chunks[id.chunk_id].states[id.id_in_chunk])
                                .filter(|state| !state.is_empty());
                            let vox1 = other
                                .linear_index(key1)
                                .map(|id| &mut other.chunks[id.chunk_id].states[id.id_in_chunk])
                                .filter(|state| !state.is_empty());

                            match (vox0, vox1) {
                                (Some(vox0), Some(vox1)) => {
                                    vox0.0 |= vox1.0;
                                    vox1.0 |= vox0.0;
                                }
                                (Some(vox0), None) => {
                                    vox0.0 |= other.compute_voxel_neighborhood_bits(key1).0;
                                }
                                (None, Some(vox1)) => {
                                    vox1.0 |= self.compute_voxel_neighborhood_bits(key0).0;
                                }
                                (None, None) => { /* Nothing to adjust. */ }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "dim3"))]
mod local_patch_tests {
    use crate::math::{Point, Vector};
    use crate::shape::Voxels;

    /// The patched whole-shape recompute must produce exactly the states the
    /// upstream per-voxel version does — they end up in collision detection and
    /// in the shape's cached hash.
    #[test]
    fn patched_recompute_matches_the_upstream_reference() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |modulus: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % modulus
        };
        for round in 0..40 {
            // Dense-ish blobs spanning several internal 8^3 chunks, negative
            // coordinates included, plus a few far-away stragglers.
            let extent = 4 + next(30) as i32;
            let origin = [next(40) as i32 - 20, next(40) as i32 - 20, next(40) as i32 - 20];
            let fill = 2 + next(6);
            let mut points = alloc::vec::Vec::new();
            for x in 0..extent {
                for y in 0..(1 + extent / 3) {
                    for z in 0..extent {
                        if next(fill) != 0 {
                            points.push(Point::new(origin[0] + x, origin[1] + y, origin[2] + z));
                        }
                    }
                }
            }
            for _ in 0..next(5) {
                points.push(Point::new(next(200) as i32 - 100, next(200) as i32 - 100, next(200) as i32 - 100));
            }
            if points.is_empty() {
                continue;
            }
            let patched = Voxels::new(Vector::repeat(crate::math::Real::from(1.0)), &points);
            let mut reference = patched.clone();
            reference.recompute_all_voxels_states_reference();
            assert_eq!(patched.chunks.len(), reference.chunks.len());
            for (id, (a, b)) in patched.chunks.iter().zip(&reference.chunks).enumerate() {
                assert!(a.states == b.states, "round {round}: states of internal chunk {id} differ");
            }
        }
    }
}
