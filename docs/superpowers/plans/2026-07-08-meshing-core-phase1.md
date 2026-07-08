# Meshing Core (Voxel Rendering Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the client's `block_mesh::greedy_quads` pipeline with an in-house binary greedy mesher that culls faces against real neighbor-chunk data, bakes per-vertex ambient occlusion, and drains remeshes through a distance-prioritized, budgeted queue.

**Architecture:** Meshing stays a pure function (padded 34³ voxel snapshot → quads → `Mesh`) run on `AsyncComputeTaskPool`. New pieces: a bitmask-based mesher (`binary.rs`), AO from the same occupancy (`ao.rs`), quad→Mesh emission with AO in the existing COLOR attribute (`emit.rs`), a world-keyed chunk grid + neighbor gather (`neighbors.rs`), and dirty-tracking/scheduling with per-frame budgets (`queue.rs`). The vendored `block-mesh` remains as a test oracle (dev-dependency) — production client code stops using it.

**Tech Stack:** Rust, Bevy 0.18 (client crate only), crc32fast (boundary-zone fingerprints), vendored `block-mesh` fork as parity oracle in tests.

**Spec:** `docs/superpowers/specs/2026-07-08-voxel-rendering-optimization-design.md` (§5 Phase 1).

## Global Constraints

- All changes live in `crates/client`. `game`/`server` stay Bevy-free and untouched (except nothing — this plan touches only `crates/client` and `CLAUDE.md`).
- Nothing enters rollback-hashed state: meshes, AO, fingerprints, the chunk grid, and the remesh queue are derived render-side data.
- Tunables are named constants: `MAX_IN_FLIGHT_MESH_TASKS`, `MESH_UPLOADS_PER_FRAME`, `AO_CURVE`.
- Tag any test measured >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- `just test` for routine checks; `just test-full` as the final gate. No justfile changes are needed (no new commands).
- Do not modify the vendored forks (`crates/block-mesh` etc.).
- Voxel solidity = `!Voxel::is_air()` (binary opaque/empty), matching current behavior. `BlockId` is `pub struct BlockId(pub u16)` with `BlockId::AIR = BlockId(0)`.
- Existing invariants to preserve verbatim: `VoxelData` presence is the sole liveness authority in `apply_meshed_chunks`; inserting a fresh `MeshingTask` supersedes (cancels) any in-flight task.

**Preflight:** The working tree currently has uncommitted changes (the 34³ padding fix in `meshing.rs`, far-ring enable, test hygiene). Run `git status`; if those changes are still uncommitted, STOP and ask the user to commit them first — this plan builds on top of that state and every task commits.

**Coordinate conventions used throughout:**
- Chunk-local voxel coords: `0..32` per axis, linear index `x + 32*y + 32*32*z` (matches `game::ChunkShape` / `game::voxel_index`).
- Padded coords: `0..34` per axis (`PADDED = 34`), interior at `1..=32` (chunk-local + 1), linear index `x + 34*(y + 34*z)`.
- World chunk key: `[i32; 3]`, world position / 32 (chunks are 32 world units).

---

### Task 1: Module split, `PaddedVoxels`, occupancy bitmasks, face culling

**Files:**
- Move: `crates/client/src/renderer/meshing.rs` → `crates/client/src/renderer/meshing/mod.rs` (via `git mv`)
- Create: `crates/client/src/renderer/meshing/binary.rs`
- Test: unit tests inside `binary.rs`

**Interfaces:**
- Consumes: `game::Voxel` (`.is_air()`, field `block: BlockId`), `game::CHUNK_VOXEL_COUNT`.
- Produces (used by Tasks 2–5):
  - `pub const PADDED: usize = 34;`
  - `pub struct PaddedVoxels(pub Vec<Voxel>)` with `pub fn index(x,y,z: usize) -> usize`, `pub fn get(&self, x,y,z) -> Voxel`, `pub fn is_solid(&self, x,y,z) -> bool`, `pub fn from_interior(voxels: &[Voxel]) -> Self`
  - `pub(crate) struct Occupancy` with `pub fn new(p: &PaddedVoxels) -> Self` and `pub fn face_bits(&self, axis: usize, positive: bool, u: usize, v: usize) -> u64`
  - Axis/cross-axis convention `pub fn cross_axes(axis: usize) -> (usize, usize)` returning the fixed (U, V) axes: X→(Y,Z), Y→(X,Z), Z→(X,Y). (Fully `pub`, not `pub(crate)` — Task 8's integration test in `tests/` imports it through the library crate.)

- [ ] **Step 1: Convert `meshing.rs` to a directory module**

```bash
cd /home/dawids/Projects/mmo
git mv crates/client/src/renderer/meshing.rs crates/client/src/renderer/meshing/mod.rs
cargo build -p client
```
Expected: builds clean (path move only; `mod meshing;` in `renderer/mod.rs` resolves the directory form automatically).

- [ ] **Step 2: Write failing tests for occupancy + face culling**

Create `crates/client/src/renderer/meshing/binary.rs` with only the test module for now, and add `pub mod binary;` to `crates/client/src/renderer/meshing/mod.rs` (top of file, after the existing `use` block):

```rust
//! Binary greedy meshing: occupancy packed as per-axis u64 column masks over
//! the padded 34³ space; face culling via bitshifts; greedy merging (task 3)
//! over per-merge-key bitplanes. Pure functions — no Bevy types here.

use game::{Voxel, CHUNK_VOXEL_COUNT};

#[cfg(test)]
mod tests {
    use super::*;
    use game::BlockId;

    fn interior_with(cells: &[([usize; 3], BlockId)]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for ([x, y, z], id) in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(*id);
        }
        v
    }

    #[test]
    fn padded_roundtrip_and_shell_is_air() {
        let p = PaddedVoxels::from_interior(&interior_with(&[([0, 5, 31], BlockId(1))]));
        assert!(p.is_solid(1, 6, 32), "chunk-local (0,5,31) is padded (1,6,32)");
        assert!(!p.is_solid(0, 6, 32), "shell stays air with from_interior");
        assert!(!p.is_solid(1, 6, 33), "shell stays air with from_interior");
    }

    #[test]
    fn lone_voxel_has_six_visible_faces() {
        let p = PaddedVoxels::from_interior(&interior_with(&[([5, 6, 7], BlockId(1))]));
        let occ = Occupancy::new(&p);
        // padded coords (6, 7, 8)
        assert_eq!(occ.face_bits(0, true, 7, 8) & (1 << 6), 1 << 6, "+X visible");
        assert_eq!(occ.face_bits(0, false, 7, 8) & (1 << 6), 1 << 6, "-X visible");
        assert_eq!(occ.face_bits(1, true, 6, 8) & (1 << 7), 1 << 7, "+Y visible");
        assert_eq!(occ.face_bits(1, false, 6, 8) & (1 << 7), 1 << 7, "-Y visible");
        assert_eq!(occ.face_bits(2, true, 6, 7) & (1 << 8), 1 << 8, "+Z visible");
        assert_eq!(occ.face_bits(2, false, 6, 7) & (1 << 8), 1 << 8, "-Z visible");
    }

    #[test]
    fn touching_voxels_cull_shared_faces() {
        // Two voxels adjacent along X at chunk-local (5,6,7) and (6,6,7).
        let p = PaddedVoxels::from_interior(&interior_with(&[
            ([5, 6, 7], BlockId(1)),
            ([6, 6, 7], BlockId(1)),
        ]));
        let occ = Occupancy::new(&p);
        // +X of the left voxel (padded x=6) faces the right voxel: culled.
        assert_eq!(occ.face_bits(0, true, 7, 8) & (1 << 6), 0);
        // -X of the right voxel (padded x=7): culled.
        assert_eq!(occ.face_bits(0, false, 7, 8) & (1 << 7), 0);
        // Outer X faces remain.
        assert_eq!(occ.face_bits(0, false, 7, 8) & (1 << 6), 1 << 6);
        assert_eq!(occ.face_bits(0, true, 7, 8) & (1 << 7), 1 << 7);
    }

    #[test]
    fn solid_shell_neighbor_culls_boundary_face() {
        // Solid voxel at chunk-local x=31 (padded x=32); a solid PAD cell at
        // padded x=33 (neighbor data) must cull its +X face.
        let mut p = PaddedVoxels::from_interior(&interior_with(&[([31, 5, 5], BlockId(1))]));
        p.0[PaddedVoxels::index(33, 6, 6)] = Voxel::new(BlockId(1));
        let occ = Occupancy::new(&p);
        assert_eq!(occ.face_bits(0, true, 6, 6) & (1 << 32), 0, "+X culled by neighbor");
        assert_eq!(occ.face_bits(0, false, 6, 6) & (1 << 32), 1 << 32, "-X still visible");
    }

    #[test]
    fn shell_cells_never_emit_faces() {
        // A solid cell in the shell itself (padded x=33) must not produce
        // visible faces — only interior bits 1..=32 may emit.
        let mut p = PaddedVoxels::from_interior(&vec![Voxel::default(); CHUNK_VOXEL_COUNT]);
        p.0[PaddedVoxels::index(33, 6, 6)] = Voxel::new(BlockId(1));
        let occ = Occupancy::new(&p);
        for u in 0..PADDED {
            for v in 0..PADDED {
                assert_eq!(occ.face_bits(0, true, u, v), 0);
                assert_eq!(occ.face_bits(0, false, u, v), 0);
            }
        }
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p client binary:: 2>&1 | tail -5`
Expected: COMPILE ERROR — `PaddedVoxels`, `Occupancy` not defined.

- [ ] **Step 4: Implement `PaddedVoxels` and `Occupancy`**

Add above the test module in `binary.rs`:

```rust
/// Padded chunk edge: 32 interior + 1 shell cell each side.
pub const PADDED: usize = 34;
/// Only interior positions (bits 1..=32) may emit faces.
const INTERIOR_MASK: u64 = ((1u64 << 32) - 1) << 1;

/// A 34³ voxel snapshot: the chunk's 32³ interior at `1..=32` per axis plus a
/// 1-voxel shell of neighbor data (task 5) or air (`from_interior`).
pub struct PaddedVoxels(pub Vec<Voxel>);

impl PaddedVoxels {
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        x + PADDED * (y + PADDED * z)
    }

    pub fn get(&self, x: usize, y: usize, z: usize) -> Voxel {
        self.0[Self::index(x, y, z)]
    }

    pub fn is_solid(&self, x: usize, y: usize, z: usize) -> bool {
        !self.get(x, y, z).is_air()
    }

    /// Interior from a chunk's 32³ voxels, shell all-air (isolated chunk).
    pub fn from_interior(voxels: &[Voxel]) -> Self {
        debug_assert_eq!(voxels.len(), CHUNK_VOXEL_COUNT);
        let mut p = vec![Voxel::default(); PADDED * PADDED * PADDED];
        for (i, v) in voxels.iter().enumerate() {
            let (x, y, z) = (i % 32, (i / 32) % 32, i / (32 * 32));
            p[Self::index(x + 1, y + 1, z + 1)] = *v;
        }
        Self(p)
    }
}

/// The fixed cross-axes (U, V) each face axis merges over. Emission (task 4)
/// derives winding from these, so they must never change independently.
pub fn cross_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2), // X faces: U=Y, V=Z
        1 => (0, 2), // Y faces: U=X, V=Z
        2 => (0, 1), // Z faces: U=X, V=Y
        _ => unreachable!("axis is 0..3"),
    }
}

/// Solidity packed as u64 columns along each axis: `cols[axis][u][v]` has bit
/// `a` set iff the voxel at position `a` along `axis` (cross coords `(u, v)`
/// per `cross_axes`) is solid. 34 ≤ 64, so one word per column.
pub(crate) struct Occupancy {
    cols: Box<[[[u64; PADDED]; PADDED]; 3]>,
}

impl Occupancy {
    pub fn new(p: &PaddedVoxels) -> Self {
        let mut cols: Box<[[[u64; PADDED]; PADDED]; 3]> =
            vec![[[0u64; PADDED]; PADDED]; 3].try_into().unwrap();
        for z in 0..PADDED {
            for y in 0..PADDED {
                for x in 0..PADDED {
                    if p.is_solid(x, y, z) {
                        cols[0][y][z] |= 1 << x;
                        cols[1][x][z] |= 1 << y;
                        cols[2][x][y] |= 1 << z;
                    }
                }
            }
        }
        Self { cols }
    }

    /// Visible-face bits along `axis` at cross coords `(u, v)`: bit `a` set
    /// iff the voxel at `a` is solid and its `positive`/negative neighbor
    /// along `axis` is air. Shell positions are masked out.
    pub fn face_bits(&self, axis: usize, positive: bool, u: usize, v: usize) -> u64 {
        let col = self.cols[axis][u][v];
        let vis = if positive { col & !(col >> 1) } else { col & !(col << 1) };
        vis & INTERIOR_MASK
    }
}
```

Note the `try_into().unwrap()` boxes the 27 KB array without a stack round-trip; if the conversion fights the borrow of array types, the fallback is `Box::new([[[0u64; PADDED]; PADDED]; 3])` (stack-then-move is fine at 27 KB).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p client binary:: 2>&1 | tail -5`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add -A crates/client/src/renderer/meshing
git commit -m "feat(client): binary-mesher occupancy masks + bitshift face culling"
```

---

### Task 2: Baked ambient occlusion (`ao.rs`)

**Files:**
- Create: `crates/client/src/renderer/meshing/ao.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs` (add `pub mod ao;`)
- Test: unit tests inside `ao.rs`

**Interfaces:**
- Consumes: `PaddedVoxels` (`is_solid`), `cross_axes` from Task 1.
- Produces (used by Tasks 3–4):
  - `pub fn vertex_ao(side1: bool, side2: bool, corner: bool) -> u8` (0..=3, 0 = darkest)
  - `pub fn face_ao(p: &PaddedVoxels, axis: usize, positive: bool, pos: [usize; 3]) -> [u8; 4]` — AO of the 4 corners of the face of the voxel at padded coords `pos`, corner order `(cu, cv)` = (0,0), (1,0), (0,1), (1,1) over `cross_axes(axis)`.
  - `pub fn ao_flips_quad(ao: &[u8; 4]) -> bool` — true when the triangulation diagonal must flip.
  - `pub const AO_CURVE: [f32; 4] = [0.55, 0.70, 0.85, 1.0];` — vertex color multiplier per AO level (named tunable).

- [ ] **Step 1: Write failing tests**

Create `crates/client/src/renderer/meshing/ao.rs`:

```rust
//! 0fps-style baked ambient occlusion, computed from the padded occupancy at
//! mesh time. Render-derived data only — never part of rollback state.
//! Reference: https://0fps.net/2013/07/03/ambient-occlusion-for-minecraft-like-worlds/

use super::binary::{cross_axes, PaddedVoxels};

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    #[test]
    fn vertex_ao_truth_table() {
        assert_eq!(vertex_ao(false, false, false), 3, "open corner");
        assert_eq!(vertex_ao(false, false, true), 2, "corner only");
        assert_eq!(vertex_ao(true, false, false), 2, "one side");
        assert_eq!(vertex_ao(true, false, true), 1, "side + corner");
        assert_eq!(vertex_ao(true, true, false), 0, "both sides occlude fully");
        assert_eq!(vertex_ao(true, true, true), 0, "both sides + corner");
    }

    #[test]
    fn lone_voxel_top_face_is_unoccluded() {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        v[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let p = PaddedVoxels::from_interior(&v);
        assert_eq!(face_ao(&p, 1, true, [6, 6, 6]), [3, 3, 3, 3]);
    }

    #[test]
    fn side_neighbor_darkens_the_two_corners_it_touches() {
        // Voxel at (5,5,5); a second voxel at (4,6,5) sits diagonally above
        // along -X: for the top (+Y) face it occludes the two cu=0 corners.
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        v[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        v[4 + 32 * 6 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let p = PaddedVoxels::from_interior(&v);
        // +Y face cross axes are (U=X, V=Z). The occluder at padded (5,7,6)
        // is the side1 (U-direction) sample for both cu=0 corners, so
        // corners (0,0) and (0,1) darken to 2 while (1,0) and (1,1) stay 3.
        assert_eq!(face_ao(&p, 1, true, [6, 6, 6]), [2, 3, 2, 3]);
    }

    #[test]
    fn flip_rule_uses_diagonal_sums() {
        assert!(ao_flips_quad(&[3, 0, 0, 3]), "bright 0-3 diagonal flips");
        assert!(!ao_flips_quad(&[0, 3, 3, 0]), "bright 1-2 diagonal keeps default");
        assert!(!ao_flips_quad(&[3, 3, 3, 3]), "uniform never flips");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p client ao:: 2>&1 | tail -5`
Expected: COMPILE ERROR — functions not defined. (Also add `pub mod ao;` to `meshing/mod.rs` now or this won't even be discovered.)

- [ ] **Step 3: Implement**

Add above the test module in `ao.rs`:

```rust
/// Vertex color multiplier per AO level (0 = fully occluded corner). Tunable.
pub const AO_CURVE: [f32; 4] = [0.55, 0.70, 0.85, 1.0];

/// Classic 0fps corner rule. `side1`/`side2` are the two face-plane-adjacent
/// neighbors of the corner, `corner` the diagonal one.
pub fn vertex_ao(side1: bool, side2: bool, corner: bool) -> u8 {
    if side1 && side2 {
        0
    } else {
        3 - (side1 as u8 + side2 as u8 + corner as u8)
    }
}

/// AO for the 4 corners of the (`axis`, `positive`) face of the voxel at
/// padded coords `pos`. Corner order (cu, cv) = (0,0), (1,0), (0,1), (1,1)
/// over `cross_axes(axis)` — the same order emission (task 4) writes vertices.
pub fn face_ao(p: &PaddedVoxels, axis: usize, positive: bool, pos: [usize; 3]) -> [u8; 4] {
    let (ua, va) = cross_axes(axis);
    // The occlusion samples live in the voxel layer the face looks into.
    let mut layer = [pos[0] as i32, pos[1] as i32, pos[2] as i32];
    layer[axis] += if positive { 1 } else { -1 };

    let solid = |c: [i32; 3]| -> bool {
        // Padded coords are 0..34; samples can step to -1 or 34 when the face
        // voxel touches the shell. Out-of-buffer counts as open (no occlusion)
        // — consistent with "missing neighbor = air" (task 5).
        if c.iter().any(|&v| v < 0 || v as usize >= super::binary::PADDED) {
            return false;
        }
        p.is_solid(c[0] as usize, c[1] as usize, c[2] as usize)
    };

    let mut ao = [0u8; 4];
    for (i, (cu, cv)) in [(0i32, 0i32), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
        let du = if cu == 0 { -1 } else { 1 };
        let dv = if cv == 0 { -1 } else { 1 };
        let mut s1 = layer;
        s1[ua] += du;
        let mut s2 = layer;
        s2[va] += dv;
        let mut c = layer;
        c[ua] += du;
        c[va] += dv;
        ao[i] = vertex_ao(solid(s1), solid(s2), solid(c));
    }
    ao
}

/// 0fps anisotropy fix: the default triangulation splits along the 0-3
/// diagonal; when the 0-3 corner pair is brighter than 1-2, flip to split
/// along 1-2 so interpolation crosses the darker pair consistently.
pub fn ao_flips_quad(ao: &[u8; 4]) -> bool {
    ao[0] + ao[3] > ao[1] + ao[2]
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p client ao:: 2>&1 | tail -5`
Expected: 4 passed. If `side_neighbor_darkens_the_two_corners_it_touches` fails with `[2, 3, 2, 3]` vs a permutation, the corner order in `face_ao` does not match (cu, cv) = (0,0),(1,0),(0,1),(1,1) — fix `face_ao`, not the test.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/meshing
git commit -m "feat(client): baked per-vertex AO (0fps corner rule + flip predicate)"
```

---

### Task 3: Greedy merge → quads

**Files:**
- Modify: `crates/client/src/renderer/meshing/binary.rs`
- Test: unit tests inside `binary.rs`

**Interfaces:**
- Consumes: `Occupancy`, `PaddedVoxels`, `cross_axes` (Task 1), `face_ao` (Task 2).
- Produces (used by Tasks 4–5, 8):
  - `#[derive(Debug, Clone, Copy, PartialEq)] pub enum Face { PosX, NegX, PosY, NegY, PosZ, NegZ }` with `pub fn axis(self) -> usize`, `pub fn positive(self) -> bool`, `pub fn normal(self) -> [f32; 3]`
  - `#[derive(Debug, Clone, PartialEq)] pub struct Quad { pub min: [u32; 3], pub w: u32, pub h: u32, pub face: Face, pub block: BlockId, pub ao: [u8; 4] }` — `min` chunk-local; `w` along `cross_axes(axis).0`, `h` along `.1`; `ao` corner order (0,0),(1,0),(0,1),(1,1) over (U, V).
  - `pub fn mesh_quads(p: &PaddedVoxels) -> Vec<Quad>`

- [ ] **Step 1: Write failing tests**

Append to the test module in `binary.rs`:

```rust
    #[test]
    fn lone_voxel_yields_six_unit_quads() {
        let p = PaddedVoxels::from_interior(&interior_with(&[([5, 6, 7], BlockId(1))]));
        let quads = mesh_quads(&p);
        assert_eq!(quads.len(), 6);
        for q in &quads {
            assert_eq!((q.w, q.h), (1, 1));
            assert_eq!(q.min, [5, 6, 7]);
            assert_eq!(q.block, BlockId(1));
            assert_eq!(q.ao, [3, 3, 3, 3], "isolated voxel is unoccluded");
        }
        let mut faces: Vec<Face> = quads.iter().map(|q| q.face).collect();
        faces.dedup();
        assert_eq!(faces.len(), 6, "one quad per face direction");
    }

    #[test]
    fn full_slab_merges_to_single_quads_per_face() {
        // One-voxel-thick 32×32 slab at y=5: ±Y faces must merge to a single
        // 32×32 quad each; the four rims merge to 32×1.
        let mut cells = Vec::new();
        for x in 0..32 {
            for z in 0..32 {
                cells.push(([x, 5, z], BlockId(1)));
            }
        }
        let p = PaddedVoxels::from_interior(&interior_with(&cells));
        let quads = mesh_quads(&p);
        let top: Vec<&Quad> = quads.iter().filter(|q| q.face == Face::PosY).collect();
        assert_eq!(top.len(), 1, "uniform top face merges fully");
        assert_eq!((top[0].w, top[0].h), (32, 32));
        assert_eq!(top[0].min, [0, 5, 0]);
        // Total: 2 big faces + 4 rim strips = 6 quads.
        assert_eq!(quads.len(), 6);
    }

    #[test]
    fn differing_block_ids_do_not_merge() {
        let p = PaddedVoxels::from_interior(&interior_with(&[
            ([5, 5, 5], BlockId(1)),
            ([6, 5, 5], BlockId(2)),
        ]));
        let quads = mesh_quads(&p);
        let tops: Vec<&Quad> = quads.iter().filter(|q| q.face == Face::PosY).collect();
        assert_eq!(tops.len(), 2, "different blocks stay separate quads");
    }

    #[test]
    fn differing_ao_does_not_merge() {
        // A 2×1 floor pair whose left cell gets a corner darkened by a block
        // sitting diagonally above-left: AO differs, so top faces must not merge.
        let p = PaddedVoxels::from_interior(&interior_with(&[
            ([5, 5, 5], BlockId(1)),
            ([6, 5, 5], BlockId(1)),
            ([4, 6, 5], BlockId(1)), // occluder for (5,5,5)'s top face
        ]));
        let quads = mesh_quads(&p);
        let tops: Vec<&Quad> = quads
            .iter()
            .filter(|q| q.face == Face::PosY && q.min[1] == 5)
            .collect();
        assert_eq!(tops.len(), 2, "AO mismatch splits the merge");
    }

    #[test]
    fn covered_area_matches_visible_faces() {
        // Decomposing quads into unit faces must reproduce exactly the visible
        // face set — no overlap, no gap. Deterministic pseudo-random content.
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for cell in v.iter_mut() {
            if rng() % 4 == 0 {
                *cell = Voxel::new(BlockId((rng() % 3 + 1) as u16));
            }
        }
        let p = PaddedVoxels::from_interior(&v);
        let occ = Occupancy::new(&p);
        let quads = mesh_quads(&p);

        let mut covered = std::collections::BTreeSet::new();
        for q in &quads {
            let (ua, va) = cross_axes(q.face.axis());
            for du in 0..q.w {
                for dv in 0..q.h {
                    let mut cell = q.min;
                    cell[ua] += du;
                    cell[va] += dv;
                    let fresh = covered.insert((q.face.axis(), q.face.positive(), cell));
                    assert!(fresh, "quads must not overlap");
                }
            }
        }
        let mut visible = std::collections::BTreeSet::new();
        for axis in 0..3 {
            let (ua, va) = cross_axes(axis);
            for u in 1..=32usize {
                for v2 in 1..=32usize {
                    for positive in [true, false] {
                        let bits = occ.face_bits(axis, positive, u, v2);
                        for a in 1..=32usize {
                            if bits >> a & 1 == 1 {
                                let mut cell = [0u32; 3];
                                cell[axis] = a as u32 - 1;
                                cell[ua] = u as u32 - 1;
                                cell[va] = v2 as u32 - 1;
                                visible.insert((axis, positive, cell));
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(covered, visible);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p client binary:: 2>&1 | tail -5`
Expected: COMPILE ERROR — `Face`, `Quad`, `mesh_quads` not defined.

- [ ] **Step 3: Implement `Face`, `Quad`, `mesh_quads`**

Add to `binary.rs` (above tests):

```rust
use std::collections::HashMap;

use game::BlockId;

use super::ao::face_ao;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

impl Face {
    pub fn from_axis(axis: usize, positive: bool) -> Self {
        match (axis, positive) {
            (0, true) => Face::PosX,
            (0, false) => Face::NegX,
            (1, true) => Face::PosY,
            (1, false) => Face::NegY,
            (2, true) => Face::PosZ,
            (2, false) => Face::NegZ,
            _ => unreachable!("axis is 0..3"),
        }
    }

    pub fn axis(self) -> usize {
        match self {
            Face::PosX | Face::NegX => 0,
            Face::PosY | Face::NegY => 1,
            Face::PosZ | Face::NegZ => 2,
        }
    }

    pub fn positive(self) -> bool {
        matches!(self, Face::PosX | Face::PosY | Face::PosZ)
    }

    pub fn normal(self) -> [f32; 3] {
        let mut n = [0.0; 3];
        n[self.axis()] = if self.positive() { 1.0 } else { -1.0 };
        n
    }
}

/// A greedy-merged rectangle of identical (block, AO) faces. `min` is the
/// chunk-local voxel coord of the rectangle's minimum cell; `w` extends along
/// `cross_axes(axis).0`, `h` along `.1`. `ao` corner order (cu, cv) =
/// (0,0), (1,0), (0,1), (1,1) over (U, V).
#[derive(Debug, Clone, PartialEq)]
pub struct Quad {
    pub min: [u32; 3],
    pub w: u32,
    pub h: u32,
    pub face: Face,
    pub block: BlockId,
    pub ao: [u8; 4],
}

/// Greedy-mesh a padded chunk. For each face direction and slice, visible
/// faces are grouped by merge key (block, ao) into per-key row bitplanes,
/// then rectangles are grown with bit scans: width along U within a row,
/// height along V across rows.
pub fn mesh_quads(p: &PaddedVoxels) -> Vec<Quad> {
    let occ = Occupancy::new(p);
    let mut quads = Vec::new();

    for axis in 0..3 {
        let (ua, va) = cross_axes(axis);
        for positive in [true, false] {
            let face = Face::from_axis(axis, positive);
            // Slice `a` = position along the face axis (interior 1..=32).
            for a in 1..=32usize {
                // rows[key][v] = bitmask over u (bits 1..=32) of visible
                // faces in this slice whose merge key is `key`.
                let mut rows: HashMap<(BlockId, [u8; 4]), [u64; PADDED]> = HashMap::new();
                for v in 1..=32usize {
                    for u in 1..=32usize {
                        if occ.face_bits(axis, positive, u, v) >> a & 1 == 0 {
                            continue;
                        }
                        let mut pos = [0usize; 3];
                        pos[axis] = a;
                        pos[ua] = u;
                        pos[va] = v;
                        let block = p.get(pos[0], pos[1], pos[2]).block;
                        let ao = face_ao(p, axis, positive, pos);
                        rows.entry((block, ao)).or_insert([0; PADDED])[v] |= 1 << u;
                    }
                }
                // Deterministic emission order: sort keys.
                let mut keys: Vec<_> = rows.keys().copied().collect();
                keys.sort_unstable_by_key(|(b, ao)| (b.0, *ao));
                for key in keys {
                    let plane = rows.get_mut(&key).expect("key from rows");
                    for v in 1..=32usize {
                        while plane[v] != 0 {
                            let u0 = plane[v].trailing_zeros() as usize;
                            let run = plane[v] >> u0;
                            let w = run.trailing_ones() as usize;
                            let mask = (((1u128 << w) - 1) as u64) << u0;
                            plane[v] &= !mask;
                            // Grow height while the same u-run is fully set.
                            let mut h = 1usize;
                            while v + h <= 32 && plane[v + h] & mask == mask {
                                plane[v + h] &= !mask;
                                h += 1;
                            }
                            let mut min = [0u32; 3];
                            min[axis] = a as u32 - 1;
                            min[ua] = u0 as u32 - 1;
                            min[va] = v as u32 - 1;
                            quads.push(Quad {
                                min,
                                w: w as u32,
                                h: h as u32,
                                face,
                                block: key.0,
                                ao: key.1,
                            });
                        }
                    }
                }
            }
        }
    }
    quads
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p client binary:: 2>&1 | tail -5`
Expected: all `binary::` tests pass (10 total across tasks 1+3).

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/meshing
git commit -m "feat(client): binary greedy merge to quads with (block, AO) merge key"
```

---

### Task 4: Quad emission → Bevy `Mesh`; rewire `build_chunk_mesh`

**Files:**
- Create: `crates/client/src/renderer/meshing/emit.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs` (replace `build_chunk_mesh` internals; adapt `queue_meshing`; update tests)
- Test: unit tests in `emit.rs` + existing tests in `mod.rs`

**Interfaces:**
- Consumes: `Quad`, `Face`, `mesh_quads`, `PaddedVoxels` (Tasks 1/3), `ao_flips_quad`, `AO_CURVE` (Task 2), `BlockTextureLayers`, `voxel_material::ATTRIBUTE_TEX_INDEX` (existing).
- Produces (used by Tasks 5/7):
  - `pub fn quads_to_mesh(quads: &[Quad], layers: &BlockTextureLayers) -> Option<Mesh>` in `emit.rs`
  - `pub fn build_chunk_mesh(padded: &PaddedVoxels, layers: &BlockTextureLayers) -> Option<Mesh>` in `mod.rs` — NOTE the signature change from `&[Voxel]` to `&PaddedVoxels`; all callers updated in this task.

**Winding convention (locked here, verified by test):** corners are emitted in (cu, cv) order (0,0), (1,0), (0,1), (1,1). For faces where `cross_axes` U×V equals the outward normal (`PosX`, `NegY`, `PosZ`) the default index pattern is `[0,1,3, 0,3,2]` and the AO-flipped pattern is `[0,1,2, 1,3,2]`. For the other three faces (`NegX`, `PosY`, `NegZ`) each triangle is reversed: default `[0,3,1, 0,2,3]`, flipped `[0,2,1, 1,2,3]`. The test asserts every triangle's `cross(v1-v0, v2-v0) · normal > 0`, so a table mistake cannot survive.

- [ ] **Step 1: Write failing tests**

Create `crates/client/src/renderer/meshing/emit.rs`:

```rust
//! Quad list → Bevy `Mesh`. Vertex format unchanged from the block-mesh era
//! (POSITION/NORMAL/UV_0/COLOR/TEX_INDEX) except COLOR now carries baked AO
//! shading instead of constant white — the voxel shader already multiplies
//! vertex color, so AO renders with no shader change. Phase 2 replaces this
//! format wholesale (packed u32 attributes).

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};

use super::super::block_textures::BlockTextureLayers;
use super::super::voxel_material;
use super::ao::{ao_flips_quad, AO_CURVE};
use super::binary::{cross_axes, Face, Quad};

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;
    use game::BlockId;

    fn unit_quad(face: Face, ao: [u8; 4]) -> Quad {
        Quad { min: [5, 6, 7], w: 2, h: 3, face, block: BlockId(1), ao }
    }

    fn tri_normals_positive(mesh: &Mesh, expected_normal: [f32; 3]) {
        let VertexAttributeValues::Float32x3(pos) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!("positions are Float32x3")
        };
        let Indices::U32(idx) = mesh.indices().unwrap() else { panic!("u32 indices") };
        let n = Vec3::from(expected_normal);
        for tri in idx.chunks(3) {
            let [a, b, c] = [pos[tri[0] as usize], pos[tri[1] as usize], pos[tri[2] as usize]];
            let cross = (Vec3::from(b) - Vec3::from(a)).cross(Vec3::from(c) - Vec3::from(a));
            assert!(cross.dot(n) > 0.0, "triangle {tri:?} winds against its normal");
        }
    }

    #[test]
    fn winding_faces_outward_for_all_six_faces_default_and_flipped() {
        for face in [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ, Face::NegZ] {
            for ao in [[3, 3, 3, 3], [3, 0, 0, 3]] {
                // [3,0,0,3] triggers the flip rule (ao[0]+ao[3] > ao[1]+ao[2]).
                let mesh =
                    quads_to_mesh(&[unit_quad(face, ao)], &BlockTextureLayers::default())
                        .expect("one quad meshes");
                tri_normals_positive(&mesh, face.normal());
            }
        }
    }

    #[test]
    fn quad_geometry_spans_w_by_h_at_the_face_plane() {
        // +X quad at min (5,6,7), w=2 (along Y), h=3 (along Z): the face plane
        // sits at x = 6, y spans 6..8, z spans 7..10.
        let mesh = quads_to_mesh(&[unit_quad(Face::PosX, [3; 4])], &BlockTextureLayers::default())
            .unwrap();
        let VertexAttributeValues::Float32x3(pos) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!()
        };
        assert!(pos.iter().all(|p| p[0] == 6.0));
        assert_eq!(pos.iter().map(|p| p[1] as i32).min(), Some(6));
        assert_eq!(pos.iter().map(|p| p[1] as i32).max(), Some(8));
        assert_eq!(pos.iter().map(|p| p[2] as i32).min(), Some(7));
        assert_eq!(pos.iter().map(|p| p[2] as i32).max(), Some(10));
    }

    #[test]
    fn ao_lands_in_vertex_color_via_curve() {
        let mesh = quads_to_mesh(&[unit_quad(Face::PosY, [0, 1, 2, 3])], &BlockTextureLayers::default())
            .unwrap();
        let VertexAttributeValues::Float32x4(colors) =
            mesh.attribute(Mesh::ATTRIBUTE_COLOR).unwrap()
        else {
            panic!()
        };
        for (i, c) in colors.iter().enumerate() {
            let expect = AO_CURVE[i]; // corner i has ao level i in this quad
            assert_eq!(c[..3], [expect, expect, expect]);
            assert_eq!(c[3], 1.0);
        }
    }

    #[test]
    fn uvs_tile_by_quad_extent() {
        let mesh = quads_to_mesh(&[unit_quad(Face::PosY, [3; 4])], &BlockTextureLayers::default())
            .unwrap();
        let VertexAttributeValues::Float32x2(uvs) =
            mesh.attribute(Mesh::ATTRIBUTE_UV_0).unwrap()
        else {
            panic!()
        };
        assert_eq!(uvs, &vec![[0.0, 0.0], [2.0, 0.0], [0.0, 3.0], [2.0, 3.0]]);
    }

    #[test]
    fn empty_quad_list_yields_none() {
        assert!(quads_to_mesh(&[], &BlockTextureLayers::default()).is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod emit;` to `meshing/mod.rs`, then:
Run: `cargo test -p client emit:: 2>&1 | tail -5`
Expected: COMPILE ERROR — `quads_to_mesh` not defined.

- [ ] **Step 3: Implement `quads_to_mesh`**

Add above the test module in `emit.rs`:

```rust
/// Index patterns by (winding reversed?, ao flipped?). Corner order is
/// (0,0), (1,0), (0,1), (1,1) over (U, V); default diagonal splits 0-3,
/// flipped splits 1-2. Reversed faces (NegX, PosY, NegZ — where U×V points
/// inward) reverse each triangle. Verified by the winding test.
fn quad_indices(face: Face, flipped: bool) -> [u32; 6] {
    let reversed = matches!(face, Face::NegX | Face::PosY | Face::NegZ);
    match (reversed, flipped) {
        (false, false) => [0, 1, 3, 0, 3, 2],
        (false, true) => [0, 1, 2, 1, 3, 2],
        (true, false) => [0, 3, 1, 0, 2, 3],
        (true, true) => [0, 2, 1, 1, 2, 3],
    }
}

pub fn quads_to_mesh(quads: &[Quad], layers: &BlockTextureLayers) -> Option<Mesh> {
    if quads.is_empty() {
        return None;
    }
    let n = quads.len() * 4;
    let mut positions = Vec::with_capacity(n);
    let mut normals = Vec::with_capacity(n);
    let mut uvs = Vec::with_capacity(n);
    let mut colors = Vec::with_capacity(n);
    let mut tex_indices: Vec<[u32; 3]> = Vec::with_capacity(n);
    let mut indices = Vec::with_capacity(quads.len() * 6);

    for q in quads {
        let axis = q.face.axis();
        let (ua, va) = cross_axes(axis);
        let base = positions.len() as u32;
        indices.extend(quad_indices(q.face, ao_flips_quad(&q.ao)).map(|i| base + i));

        // The face plane sits at min[axis] for negative faces, +1 for positive.
        let mut origin = [q.min[0] as f32, q.min[1] as f32, q.min[2] as f32];
        if q.face.positive() {
            origin[axis] += 1.0;
        }
        for (i, (cu, cv)) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
            let mut pos = origin;
            pos[ua] += (cu * q.w) as f32;
            pos[va] += (cv * q.h) as f32;
            positions.push(pos);
            normals.push(q.face.normal());
            uvs.push([(cu * q.w) as f32, (cv * q.h) as f32]);
            let shade = AO_CURVE[q.ao[i] as usize];
            colors.push([shade, shade, shade, 1.0]);
        }
        let faces = layers.0.get(&q.block).copied().unwrap_or([0, 0, 0]);
        tex_indices.extend(std::iter::repeat(faces).take(4));
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, VertexAttributeValues::Float32x3(positions));
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, VertexAttributeValues::Float32x3(normals));
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, VertexAttributeValues::Float32x2(uvs));
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, VertexAttributeValues::Float32x4(colors));
    mesh.insert_attribute(voxel_material::ATTRIBUTE_TEX_INDEX, VertexAttributeValues::Uint32x3(tex_indices));
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}
```

- [ ] **Step 4: Run emit tests**

Run: `cargo test -p client emit:: 2>&1 | tail -5`
Expected: 5 passed. If the winding test fails on specific faces, fix the `reversed` set in `quad_indices` (never the test).

- [ ] **Step 5: Rewire `build_chunk_mesh` and `queue_meshing`**

In `meshing/mod.rs`:

1. Replace the entire old `build_chunk_mesh` (the `PaddedChunkShape`/`greedy_quads` implementation, currently ~lines 14–71) and its `block_mesh`/`ChunkShape` imports with:

```rust
use binary::PaddedVoxels;

/// Mesh a padded chunk snapshot. `None` = no visible faces (don't render).
pub fn build_chunk_mesh(padded: &PaddedVoxels, layers: &BlockTextureLayers) -> Option<Mesh> {
    emit::quads_to_mesh(&binary::mesh_quads(padded), layers)
}
```

2. In `queue_meshing`, change the task body (neighbor gathering arrives in Task 5/7 — for now, isolated padding preserves behavior):

```rust
        let padded = PaddedVoxels::from_interior(&voxels.0);
        let layers = layers.0.clone();
        let task = pool.spawn(async move { build_chunk_mesh(&padded, &BlockTextureLayers(layers)) });
```
(The `let voxels = voxels.0.clone();` line is replaced by building `padded` directly from `&voxels.0`.)

3. Update the existing tests in `mod.rs`: every `build_chunk_mesh(&voxels, ...)` call becomes `build_chunk_mesh(&PaddedVoxels::from_interior(&voxels), ...)`. Remove the now-unused `use block_mesh::ndshape::ConstShape;` from the test module and replace `ChunkShape::linearize([x,y,z]) as usize` occurrences with the plain index expression `x + 32 * y + 32 * 32 * z` (cast literals as needed). The assertions themselves (24 verts for a lone voxel, spans [0,32], boundary shell meshed, etc.) stay — they must pass unchanged.

- [ ] **Step 6: Run the full client suite**

Run: `cargo test -p client 2>&1 | tail -15`
Expected: all tests pass, including the pre-existing `boundary_voxels_are_meshed`, `full_chunk_mesh_spans_full_extent`, `single_voxel_yields_cube` (24 vertices — our mesher also emits 6 unit quads for a lone voxel), and the async pipeline tests.

- [ ] **Step 7: Visual smoke check (manual, quick)**

Run `just run` briefly: terrain must look identical to before except corners/creases now show subtle AO darkening. Watch for inside-out faces (winding bugs) and missing chunk borders. Kill after inspection.

- [ ] **Step 8: Commit**

```bash
git add crates/client/src/renderer/meshing
git commit -m "feat(client): switch chunk meshing to the binary mesher; AO shades vertex color"
```

---

### Task 5: Neighbor gather (`neighbors.rs`)

**Files:**
- Create: `crates/client/src/renderer/meshing/neighbors.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs` (add `pub mod neighbors;`)
- Test: unit tests inside `neighbors.rs`

**Interfaces:**
- Consumes: `PaddedVoxels` (Task 1), `mesh_quads` (Task 3).
- Produces (used by Tasks 6–7):
  - `pub fn zone_offsets() -> impl Iterator<Item = [i32; 3]>` — the 26 non-zero offsets in `(-1..=1)³`, fixed z-outer/y-middle/x-inner order.
  - `pub fn zone_index(off: [i32; 3]) -> usize` — position of `off` in that enumeration (0..26).
  - `pub fn gather_padded(center: &[Voxel], neighbors: &BTreeMap<[i32; 3], &[Voxel]>) -> PaddedVoxels` — interior from `center`, shell cells from the matching neighbor's boundary voxels; missing neighbors leave air.

- [ ] **Step 1: Write failing tests**

Create `crates/client/src/renderer/meshing/neighbors.rs`:

```rust
//! Assembling the 34³ padded snapshot from a chunk and its up-to-26 grid
//! neighbors. Face neighbors feed culling; edge/corner neighbors feed AO.

use std::collections::BTreeMap;

use game::{Voxel, CHUNK_VOXEL_COUNT};

use super::binary::{PaddedVoxels, PADDED};

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::binary::{mesh_quads, Face};
    use game::BlockId;

    fn full_chunk(id: BlockId) -> Vec<Voxel> {
        vec![Voxel::new(id); CHUNK_VOXEL_COUNT]
    }

    #[test]
    fn zone_offsets_enumerates_26_and_indexes_roundtrip() {
        let offs: Vec<[i32; 3]> = zone_offsets().collect();
        assert_eq!(offs.len(), 26);
        assert!(!offs.contains(&[0, 0, 0]));
        for (i, off) in offs.iter().enumerate() {
            assert_eq!(zone_index(*off), i);
        }
    }

    #[test]
    fn missing_neighbors_leave_air_shell() {
        let p = gather_padded(&full_chunk(BlockId(1)), &BTreeMap::new());
        assert!(!p.is_solid(0, 5, 5));
        assert!(!p.is_solid(33, 5, 5));
        assert!(p.is_solid(1, 5, 5));
    }

    #[test]
    fn face_neighbor_boundary_fills_the_shell() {
        // Neighbor at +X: its x=0 plane becomes our x=33 shell plane.
        let mut nv = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        nv[0 + 32 * 4 + 32 * 32 * 4] = Voxel::new(BlockId(2)); // its (0,4,4)
        let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        neighbors.insert([1, 0, 0], nv.as_slice());
        let p = gather_padded(&full_chunk(BlockId(1)), &neighbors);
        assert!(p.is_solid(33, 5, 5), "neighbor (0,4,4) lands at padded (33,5,5)");
        assert!(!p.is_solid(33, 6, 5));
    }

    #[test]
    fn corner_neighbor_fills_the_corner_cell() {
        // Neighbor at (-1,-1,-1): its voxel (31,31,31) is our padded (0,0,0).
        let mut nv = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        nv[31 + 32 * 31 + 32 * 32 * 31] = Voxel::new(BlockId(2));
        let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        neighbors.insert([-1, -1, -1], nv.as_slice());
        let p = gather_padded(&full_chunk(BlockId(1)), &neighbors);
        assert!(p.is_solid(0, 0, 0));
    }

    #[test]
    fn solid_neighbor_culls_the_whole_shared_face() {
        // THE Phase-1 payoff test: a full chunk with a full +X neighbor emits
        // zero +X faces; isolated it emits the whole 32×32 wall.
        let center = full_chunk(BlockId(1));
        let neighbor = full_chunk(BlockId(1));
        let isolated = mesh_quads(&gather_padded(&center, &BTreeMap::new()));
        assert!(isolated.iter().any(|q| q.face == Face::PosX));

        let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        neighbors.insert([1, 0, 0], neighbor.as_slice());
        let meshed = mesh_quads(&gather_padded(&center, &neighbors));
        assert!(
            meshed.iter().all(|q| q.face != Face::PosX),
            "faces against a solid neighbor must be culled"
        );
        // The other five walls still render.
        assert!(meshed.iter().any(|q| q.face == Face::NegX));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p client neighbors:: 2>&1 | tail -5`
Expected: COMPILE ERROR.

- [ ] **Step 3: Implement**

Add above the test module:

```rust
/// The 26 neighbor offsets in fixed enumeration order (z-outer, x-inner).
pub fn zone_offsets() -> impl Iterator<Item = [i32; 3]> {
    (-1..=1).flat_map(|z| {
        (-1..=1).flat_map(move |y| {
            (-1..=1).filter_map(move |x| {
                if [x, y, z] == [0, 0, 0] {
                    None
                } else {
                    Some([x, y, z])
                }
            })
        })
    })
}

/// Position of `off` within `zone_offsets()` (0..26).
pub fn zone_index(off: [i32; 3]) -> usize {
    let linear = (off[2] + 1) * 9 + (off[1] + 1) * 3 + (off[0] + 1);
    // Offsets after the center [0,0,0] (linear 13) shift down by one.
    if linear > 13 { linear as usize - 1 } else { linear as usize }
}

fn shell_dir(c: usize) -> i32 {
    if c == 0 {
        -1
    } else if c == PADDED - 1 {
        1
    } else {
        0
    }
}

/// Padded coord → the neighbor's chunk-local coord along one axis.
fn neighbor_coord(c: usize, dir: i32) -> usize {
    match dir {
        -1 => 31, // our x=0 shell cell is the -X neighbor's x=31 plane
        1 => 0,   // our x=33 shell cell is the +X neighbor's x=0 plane
        _ => c - 1,
    }
}

/// Build the padded snapshot: interior from `center`, shell from neighbors
/// keyed by grid offset. Missing neighbors leave air (over-draws, never holes).
pub fn gather_padded(center: &[Voxel], neighbors: &BTreeMap<[i32; 3], &[Voxel]>) -> PaddedVoxels {
    let mut p = PaddedVoxels::from_interior(center);
    if neighbors.is_empty() {
        return p;
    }
    for z in 0..PADDED {
        for y in 0..PADDED {
            for x in 0..PADDED {
                let off = [shell_dir(x), shell_dir(y), shell_dir(z)];
                if off == [0, 0, 0] {
                    continue;
                }
                let Some(nv) = neighbors.get(&off) else { continue };
                let (nx, ny, nz) = (
                    neighbor_coord(x, off[0]),
                    neighbor_coord(y, off[1]),
                    neighbor_coord(z, off[2]),
                );
                p.0[PaddedVoxels::index(x, y, z)] = nv[nx + 32 * ny + 32 * 32 * nz];
            }
        }
    }
    p
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p client neighbors:: 2>&1 | tail -5`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/meshing
git commit -m "feat(client): neighbor-aware padded gather for cross-chunk face culling"
```

---

### Task 6: Chunk grid, boundary fingerprints, dirty marking (`queue.rs`)

**Files:**
- Create: `crates/client/src/renderer/meshing/queue.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs` (add `pub mod queue;`)
- Modify: `crates/client/Cargo.toml` (add `crc32fast = { workspace = true }` under `[dependencies]`)
- Test: unit + headless app tests inside `queue.rs`

**Interfaces:**
- Consumes: `zone_offsets`, `zone_index` (Task 5), `bridge::VoxelData` (existing), `crc32fast::Hasher`.
- Produces (used by Task 7):
  - `#[derive(Resource, Default)] pub struct ChunkGrid(pub BTreeMap<[i32; 3], Entity>)`
  - `#[derive(Component, Copy, Clone)] pub struct ChunkKey(pub [i32; 3])`
  - `#[derive(Component)] pub struct BoundaryZones(pub [u32; 26])`
  - `pub fn boundary_zones(voxels: &[Voxel]) -> [u32; 26]`
  - `#[derive(Resource, Default)] pub struct RemeshQueue { pub dirty: BTreeMap<Entity, Option<[i32; 3]>> }` (entity → its grid key if known)
  - `pub fn mark_remesh_dirty(...)` system — keys new chunks into the grid (synchronously, via `ResMut<ChunkGrid>`), records dirty entities, and propagates dirtiness to neighbors whose boundary zone actually changed.
  - `pub fn prune_chunk_grid(...)` system — drops grid entries for despawned/cleared chunks (respecting the same-frame-reinsert liveness rule).

**Design notes for the implementer:**
- The grid key is computed from the chunk's `Transform` plus its `ChildOf` parent's `Transform` (region roots sit at `RegionId::world_offset()`; chunks are direct children — see `bridge.rs` `spawn_far_region`/`spawn_region_snapshot`). Entities without a parented transform (unit tests spawn bare `VoxelData`) get key `None` and simply skip neighbor logic.
- Keying happens inside `mark_remesh_dirty` (not a separate system) so a chunk spawned, keyed, and meshed in the same frame is possible — a deferred-`Commands` key insert would leave the whole join burst meshed with air shells and nothing to re-dirty it (`ChunkKey` the component is written via `Commands` for later frames, but the freshly computed key is used immediately and stored in `RemeshQueue`).
- The 26 zone hashes cover overlapping cell sets (a corner voxel is in 3 face zones, 3 edge zones, 1 corner zone) so any boundary change dirties every neighbor whose view of us changed — including diagonal neighbors that only need it for AO.
- First sight of a chunk (no `BoundaryZones` yet) counts as "all zones changed": neighbors remesh when a late-arriving chunk appears next to them. Dirty sets dedupe, so the join burst stays O(chunks).

- [ ] **Step 1: Add the dependency**

In `crates/client/Cargo.toml` `[dependencies]`, after `bincode = { workspace = true }`:

```toml
crc32fast = { workspace = true }
```

Run: `cargo build -p client` — Expected: clean.

- [ ] **Step 2: Write failing tests**

Create `crates/client/src/renderer/meshing/queue.rs`:

```rust
//! Remesh scheduling: world-keyed chunk grid, boundary-zone change detection,
//! and the dirty queue drained by `spawn_mesh_tasks` (task 7). All state here
//! is render-derived — never rollback-hashed.

use std::collections::BTreeMap;

use bevy::prelude::*;
use game::{Voxel, CHUNK_VOXEL_COUNT};

use super::super::bridge::VoxelData;
use super::neighbors::{zone_index, zone_offsets};

#[cfg(test)]
mod tests {
    use super::*;
    use game::BlockId;

    fn voxels_with(cells: &[[usize; 3]]) -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        for [x, y, z] in cells {
            v[x + 32 * y + 32 * 32 * z] = Voxel::new(BlockId(1));
        }
        v
    }

    #[test]
    fn boundary_zone_reacts_only_to_its_zone() {
        let a = boundary_zones(&voxels_with(&[[5, 5, 5]]));
        let b = boundary_zones(&voxels_with(&[[5, 5, 6]]));
        assert_eq!(a, b, "interior-only changes leave every zone hash equal");

        let c = boundary_zones(&voxels_with(&[[31, 5, 5]]));
        let face_px = zone_index([1, 0, 0]);
        assert_ne!(a[face_px], c[face_px], "+X face zone changed");
        let face_nx = zone_index([-1, 0, 0]);
        assert_eq!(a[face_nx], c[face_nx], "-X face zone did not");

        let d = boundary_zones(&voxels_with(&[[31, 31, 31]]));
        assert_ne!(a[zone_index([1, 1, 1])], d[zone_index([1, 1, 1])], "corner zone changed");
        assert_ne!(a[zone_index([1, 1, 0])], d[zone_index([1, 1, 0])], "edge zone changed");
        assert_ne!(a[zone_index([1, 0, 0])], d[zone_index([1, 0, 0])], "face zone changed");
    }

    fn grid_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .add_systems(Update, (mark_remesh_dirty, prune_chunk_grid));
        app
    }

    /// Region-root + chunk child at region-local chunk coords, like the bridge builds.
    fn spawn_chunk(app: &mut App, root_at: Vec3, local: Vec3, voxels: Vec<Voxel>) -> Entity {
        let root = app
            .world_mut()
            .spawn((Transform::from_translation(root_at), Visibility::default()))
            .id();
        app.world_mut()
            .spawn((
                Transform::from_translation(local),
                Visibility::default(),
                VoxelData(voxels),
                ChildOf(root),
            ))
            .id()
    }

    #[test]
    fn chunks_key_into_the_grid_by_world_position() {
        let mut app = grid_app();
        let e = spawn_chunk(&mut app, Vec3::new(256.0, 0.0, -512.0), Vec3::new(64.0, 32.0, 0.0), voxels_with(&[]));
        app.update();
        let grid = app.world().resource::<ChunkGrid>();
        assert_eq!(grid.0.get(&[10, 1, -16]), Some(&e), "(256+64)/32, 32/32, -512/32");
        assert!(app.world().resource::<RemeshQueue>().dirty.contains_key(&e));
    }

    #[test]
    fn boundary_change_dirties_only_affected_neighbors() {
        let mut app = grid_app();
        // Two X-adjacent chunks under one root at origin.
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let b = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(32.0, 0.0, 0.0), voxels_with(&[]));
        app.update();
        // Drain the initial dirtiness.
        app.world_mut().resource_mut::<RemeshQueue>().dirty.clear();

        // Interior edit in `a`: nobody's boundary view changes except a itself.
        app.world_mut().get_mut::<VoxelData>(a).unwrap().0
            [6 + 32 * 6 + 32 * 32 * 6] = Voxel::new(BlockId(1));
        app.update();
        {
            let queue = app.world().resource::<RemeshQueue>();
            assert!(queue.dirty.contains_key(&a));
            assert!(!queue.dirty.contains_key(&b), "interior edit must not dirty the neighbor");
        }
        app.world_mut().resource_mut::<RemeshQueue>().dirty.clear();

        // Boundary edit on a's +X face: b's view of a changed → b dirties.
        app.world_mut().get_mut::<VoxelData>(a).unwrap().0
            [31 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        app.update();
        let queue = app.world().resource::<RemeshQueue>();
        assert!(queue.dirty.contains_key(&a));
        assert!(queue.dirty.contains_key(&b), "+X boundary edit must dirty the +X neighbor");
    }

    #[test]
    fn despawned_chunks_leave_the_grid() {
        let mut app = grid_app();
        let e = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[]));
        app.update();
        assert!(!app.world().resource::<ChunkGrid>().0.is_empty());
        app.world_mut().entity_mut(e).despawn();
        app.update();
        assert!(app.world().resource::<ChunkGrid>().0.is_empty());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Add `pub mod queue;` to `meshing/mod.rs`, then:
Run: `cargo test -p client queue:: 2>&1 | tail -5`
Expected: COMPILE ERROR.

- [ ] **Step 4: Implement**

Add above the test module in `queue.rs`:

```rust
/// World-space chunk grid: key = world position / 32. Render-side spatial
/// index for neighbor lookups; rebuilt from entities, never persisted.
#[derive(Resource, Default)]
pub struct ChunkGrid(pub BTreeMap<[i32; 3], Entity>);

/// This chunk's grid key, stamped when first keyed.
#[derive(Component, Copy, Clone)]
pub struct ChunkKey(pub [i32; 3]);

/// crc32 fingerprint of each of the 26 boundary zones (indexed by
/// `zone_index`). A neighbor only needs remeshing when the zone it can see
/// actually changed — this is what makes edit-driven remeshes O(affected).
#[derive(Component)]
pub struct BoundaryZones(pub [u32; 26]);

/// Chunks awaiting a mesh task, with their grid key if known. BTreeMap for
/// deterministic drain order (entity order is arbitrary but stable).
#[derive(Resource, Default)]
pub struct RemeshQueue {
    pub dirty: BTreeMap<Entity, Option<[i32; 3]>>,
}

/// Hash the 26 boundary zones of a 32³ chunk. Zones overlap on purpose: a
/// corner voxel contributes to 3 face + 3 edge + 1 corner zone.
pub fn boundary_zones(voxels: &[Voxel]) -> [u32; 26] {
    debug_assert_eq!(voxels.len(), CHUNK_VOXEL_COUNT);
    let mut zones = [0u32; 26];
    for (i, off) in zone_offsets().enumerate() {
        let range = |o: i32| -> std::ops::Range<usize> {
            match o {
                -1 => 0..1,
                1 => 31..32,
                _ => 0..32,
            }
        };
        let mut h = crc32fast::Hasher::new();
        for z in range(off[2]) {
            for y in range(off[1]) {
                for x in range(off[0]) {
                    h.update(&voxels[x + 32 * y + 32 * 32 * z].block.0.to_le_bytes());
                }
            }
        }
        zones[i] = h.finalize();
        debug_assert_eq!(zone_index(off), i);
    }
    zones
}

/// Key changed chunks into the grid, mark them dirty, and dirty any neighbor
/// whose boundary zone changed. Runs in `Update` before `spawn_mesh_tasks`.
pub fn mark_remesh_dirty(
    mut commands: Commands,
    mut grid: ResMut<ChunkGrid>,
    mut queue: ResMut<RemeshQueue>,
    mut changed: Query<
        (
            Entity,
            &VoxelData,
            Option<&Transform>,
            Option<&ChildOf>,
            Option<&ChunkKey>,
            Option<&mut BoundaryZones>,
        ),
        Changed<VoxelData>,
    >,
    roots: Query<&Transform, Without<VoxelData>>,
) {
    for (e, voxels, tf, parent, key, mut zones) in &mut changed {
        // Resolve (or compute) the grid key. Bare test entities (no parented
        // transform) stay unkeyed and skip all neighbor logic.
        let key: Option<[i32; 3]> = key.map(|k| k.0).or_else(|| {
            let (tf, parent) = (tf?, parent?);
            let root = roots.get(parent.parent()).ok()?;
            let world = root.translation + tf.translation;
            let k = [
                (world.x / 32.0).round() as i32,
                (world.y / 32.0).round() as i32,
                (world.z / 32.0).round() as i32,
            ];
            grid.0.insert(k, e);
            commands.entity(e).insert(ChunkKey(k));
            Some(k)
        });
        queue.dirty.insert(e, key);

        let new_zones = boundary_zones(&voxels.0);
        if let Some(k) = key {
            let old = zones.as_ref().map(|z| z.0);
            for (i, off) in zone_offsets().enumerate() {
                if old.is_some_and(|o| o[i] == new_zones[i]) {
                    continue;
                }
                let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
                if let Some(&ne) = grid.0.get(&nk) {
                    queue.dirty.entry(ne).or_insert(Some(nk));
                }
            }
        }
        match zones.as_deref_mut() {
            Some(z) => z.0 = new_zones,
            None => {
                commands.entity(e).insert(BoundaryZones(new_zones));
            }
        }
    }
}

/// Drop grid + queue entries for chunks whose `VoxelData` is gone for good
/// (same liveness rule as `apply_meshed_chunks`: presence wins over the
/// frame's removal log, so same-frame remove+reinsert survives).
pub fn prune_chunk_grid(
    mut grid: ResMut<ChunkGrid>,
    mut queue: ResMut<RemeshQueue>,
    mut removed: RemovedComponents<VoxelData>,
    still_alive: Query<(), With<VoxelData>>,
) {
    let dead: Vec<Entity> = removed.read().filter(|e| !still_alive.contains(*e)).collect();
    if dead.is_empty() {
        return;
    }
    grid.0.retain(|_, e| !dead.contains(e));
    for e in dead {
        queue.dirty.remove(&e);
    }
}
```

If `parent.parent()` doesn't exist on this Bevy version's `ChildOf`, use `parent.0` (it's a tuple struct in some 0.1x releases) — check with `cargo doc` or the compiler error, don't guess further.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p client queue:: 2>&1 | tail -5`
Expected: 4 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/client/Cargo.toml Cargo.lock crates/client/src/renderer/meshing
git commit -m "feat(client): chunk grid + boundary-zone fingerprints + dirty marking"
```

---

### Task 7: Prioritized, budgeted task spawning; wire the new pipeline in

**Files:**
- Modify: `crates/client/src/renderer/meshing/queue.rs` (add `spawn_mesh_tasks`, budgets, rank)
- Modify: `crates/client/src/renderer/meshing/mod.rs` (delete `queue_meshing`; cap uploads in `apply_meshed_chunks`)
- Modify: `crates/client/src/renderer/mod.rs:67-78` (system registration)
- Test: headless app tests in `queue.rs` + updated pipeline tests in `meshing/mod.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–6; `MeshingTask`, `apply_meshed_chunks`, `BlockTextureLayers`, `bevy::camera::primitives::{Frustum, Sphere}`.
- Produces:
  - `pub const MAX_IN_FLIGHT_MESH_TASKS: usize = 64;`
  - `pub const MESH_UPLOADS_PER_FRAME: usize = 32;`
  - `#[derive(Resource)] pub struct MeshBudgets { pub max_in_flight: usize, pub uploads_per_frame: usize }` (`Default` = the two constants; tests override)
  - `pub fn spawn_mesh_tasks(...)` system — drains `RemeshQueue` nearest-first (out-of-frustum last) up to the in-flight budget, gathering neighbor-padded snapshots.
  - `apply_meshed_chunks` applies at most `uploads_per_frame` completed meshes per frame (removal handling stays uncapped).

**Behavior contract (from the spec):** existing supersede semantics stay — re-dirtying an in-flight chunk spawns a replacement `MeshingTask` whose insert cancels the old task. `VoxelData` presence stays the sole liveness authority. The join burst becomes a queue drained nearest-first over several frames instead of one giant task dump.

- [ ] **Step 1: Write failing tests**

Append to the test module in `queue.rs`:

```rust
    use super::super::{apply_meshed_chunks, MeshingTask};
    use super::super::super::block_textures::BlockTextureLayers;

    fn pipeline_app(budgets: MeshBudgets) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<ChunkGrid>()
            .init_resource::<RemeshQueue>()
            .init_resource::<BlockTextureLayers>()
            .insert_resource(budgets)
            .add_systems(
                Update,
                (mark_remesh_dirty, prune_chunk_grid, spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        app
    }

    #[test]
    fn in_flight_budget_is_respected_and_nearest_meshes_first() {
        let mut app = pipeline_app(MeshBudgets { max_in_flight: 1, uploads_per_frame: 32 });
        // Camera at origin.
        app.world_mut().spawn((
            Camera3d::default(),
            GlobalTransform::from(Transform::from_translation(Vec3::ZERO)),
        ));
        let near = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let far = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(320.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));
        app.update();
        assert!(
            app.world().entity(near).contains::<MeshingTask>(),
            "nearest chunk claims the single task slot"
        );
        assert!(
            !app.world().entity(far).contains::<MeshingTask>(),
            "budget of 1 leaves the far chunk queued"
        );
        assert!(app.world().resource::<RemeshQueue>().dirty.contains_key(&far));
    }

    #[test]
    fn upload_budget_spreads_mesh_attach_over_frames() {
        let mut app = pipeline_app(MeshBudgets { max_in_flight: 64, uploads_per_frame: 1 });
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let b = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(32.0, 0.0, 0.0), voxels_with(&[[5, 5, 5]]));
        // Let tasks finish, then count Mesh3d arrivals per frame.
        for _ in 0..200 {
            app.update();
            let done = [a, b]
                .iter()
                .filter(|e| app.world().entity(**e).contains::<Mesh3d>())
                .count();
            if done == 1 {
                // Exactly one landed this frame — the cap held. Let the other land.
                for _ in 0..200 {
                    app.update();
                    if app.world().entity(b).contains::<Mesh3d>()
                        && app.world().entity(a).contains::<Mesh3d>()
                    {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                panic!("second mesh never attached");
            }
            assert!(done < 2, "both meshes attached in one frame despite uploads_per_frame=1");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no mesh ever attached");
    }

    #[test]
    fn meshing_uses_neighbor_data_from_the_grid() {
        // Two solid X-adjacent chunks: after both mesh, neither may have quads
        // on the shared plane. This is the end-to-end neighbor-culling test.
        let mut app = pipeline_app(MeshBudgets::default());
        let full: Vec<Voxel> = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        let a = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, full.clone());
        let b = spawn_chunk(&mut app, Vec3::ZERO, Vec3::new(32.0, 0.0, 0.0), full);
        for _ in 0..200 {
            app.update();
            if app.world().entity(a).contains::<Mesh3d>()
                && app.world().entity(b).contains::<Mesh3d>()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Chunk a spans x 0..32: with a solid +X neighbor its mesh must have
        // no vertex at x=32 with a +X normal. (b meshed before a existed may
        // still carry its -X wall only if it meshed first without a — the
        // zone diff re-dirties it; loop until both settle.)
        for _ in 0..200 {
            app.update();
            std::thread::sleep(std::time::Duration::from_millis(2));
            if app.world().resource::<RemeshQueue>().dirty.is_empty()
                && app.world().query::<&MeshingTask>().iter(app.world()).next().is_none()
            {
                break;
            }
        }
        let meshes = app.world().resource::<Assets<Mesh>>();
        let check = |e: Entity, plane_x: f32, normal_x: f32| {
            let handle = app.world().entity(e).get::<Mesh3d>().expect("meshed").0.clone();
            let mesh = meshes.get(&handle).unwrap();
            let (Some(VertexAttributeValues::Float32x3(pos)), Some(VertexAttributeValues::Float32x3(nor))) = (
                mesh.attribute(Mesh::ATTRIBUTE_POSITION),
                mesh.attribute(Mesh::ATTRIBUTE_NORMAL),
            ) else {
                panic!()
            };
            assert!(
                !pos.iter().zip(nor).any(|(p, n)| p[0] == plane_x && n[0] == normal_x),
                "face on the shared plane survived neighbor culling"
            );
        };
        check(a, 32.0, 1.0);
        check(b, 0.0, -1.0);
    }
```

Note: `app.world().query` needs `&mut` world access in some Bevy versions — if it doesn't compile, restructure as `app.world_mut().query::<&MeshingTask>().iter(app.world()).next().is_none()` computed before the immutable borrows, or track via a simple `for` over `Query` in a probe system. Prefer the smallest change that compiles; the assertion intent is "queue empty and no task in flight".

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p client queue:: 2>&1 | tail -5`
Expected: COMPILE ERROR — `MeshBudgets`, `spawn_mesh_tasks` not defined.

- [ ] **Step 3: Implement `spawn_mesh_tasks` and budgets**

Add to `queue.rs`:

```rust
use bevy::camera::primitives::{Frustum, Sphere};
use bevy::tasks::AsyncComputeTaskPool;

use super::super::block_textures::BlockTextureLayers;
use super::neighbors::gather_padded;
use super::{build_chunk_mesh, MeshingTask};
use super::binary::PaddedVoxels;

/// Cap on concurrently queued meshing tasks. Each task owns a ~77 KB padded
/// snapshot; the cap bounds join-burst memory and lets priority reordering
/// act on everything not yet started. Tunable.
pub const MAX_IN_FLIGHT_MESH_TASKS: usize = 64;
/// Cap on finished meshes attached per frame — bounds per-frame GPU upload
/// spikes during bursts. Tunable.
pub const MESH_UPLOADS_PER_FRAME: usize = 32;

#[derive(Resource)]
pub struct MeshBudgets {
    pub max_in_flight: usize,
    pub uploads_per_frame: usize,
}

impl Default for MeshBudgets {
    fn default() -> Self {
        Self {
            max_in_flight: MAX_IN_FLIGHT_MESH_TASKS,
            uploads_per_frame: MESH_UPLOADS_PER_FRAME,
        }
    }
}

/// Sort rank: out-of-frustum chunks after in-frustum, then by squared
/// distance to the camera. No camera (headless/startup) ranks everything 0.
fn chunk_rank(key: [i32; 3], cam: Option<(&GlobalTransform, Option<&Frustum>)>) -> u64 {
    let Some((tf, frustum)) = cam else { return 0 };
    let center = Vec3::new(
        key[0] as f32 * 32.0 + 16.0,
        key[1] as f32 * 32.0 + 16.0,
        key[2] as f32 * 32.0 + 16.0,
    );
    let dist2 = tf.translation().distance_squared(center) as u64;
    let out = frustum
        .map(|f| {
            !f.intersects_sphere(
                &Sphere { center: center.into(), radius: 16.0 * 3f32.sqrt() },
                false,
            )
        })
        .unwrap_or(false);
    ((out as u64) << 62) | dist2.min((1 << 62) - 1)
}

/// Drain the dirty queue into meshing tasks, nearest-first, up to the
/// in-flight budget. Re-inserting `MeshingTask` supersedes (cancels) any
/// prior in-flight task for that entity — same rule as the old pipeline.
pub fn spawn_mesh_tasks(
    mut commands: Commands,
    mut queue: ResMut<RemeshQueue>,
    budgets: Res<MeshBudgets>,
    grid: Res<ChunkGrid>,
    chunks: Query<&VoxelData>,
    in_flight: Query<(), With<MeshingTask>>,
    camera: Query<(&GlobalTransform, Option<&Frustum>), With<Camera3d>>,
    layers: Res<BlockTextureLayers>,
) {
    if queue.dirty.is_empty() {
        return;
    }
    // Entities that lost VoxelData while queued are stale — drop them.
    queue.dirty.retain(|e, _| chunks.contains(*e));
    let capacity = budgets.max_in_flight.saturating_sub(in_flight.iter().count());
    if capacity == 0 {
        return;
    }
    let cam = camera.iter().next();
    let mut ranked: Vec<(u64, Entity, Option<[i32; 3]>)> = queue
        .dirty
        .iter()
        .map(|(&e, &key)| (key.map_or(0, |k| chunk_rank(k, cam)), e, key))
        .collect();
    ranked.sort_unstable_by_key(|(rank, e, _)| (*rank, *e));

    let pool = AsyncComputeTaskPool::get();
    for (_, e, key) in ranked.into_iter().take(capacity) {
        queue.dirty.remove(&e);
        let Ok(voxels) = chunks.get(e) else { continue };
        let padded = match key {
            Some(k) => {
                let mut neigh: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
                for off in zone_offsets() {
                    let nk = [k[0] + off[0], k[1] + off[1], k[2] + off[2]];
                    if let Some(&ne) = grid.0.get(&nk) {
                        if let Ok(nv) = chunks.get(ne) {
                            neigh.insert(off, nv.0.as_slice());
                        }
                    }
                }
                gather_padded(&voxels.0, &neigh)
            }
            None => PaddedVoxels::from_interior(&voxels.0),
        };
        let layers = layers.0.clone();
        let task = pool.spawn(async move { build_chunk_mesh(&padded, &BlockTextureLayers(layers)) });
        commands.entity(e).insert(MeshingTask(task));
    }
}
```

- [ ] **Step 4: Delete `queue_meshing`, cap uploads, rewire registration**

1. In `meshing/mod.rs`: delete the `queue_meshing` system entirely. In `apply_meshed_chunks`, add `budgets: Res<queue::MeshBudgets>` as a parameter and a counter around the poll loop:

```rust
    let mut applied = 0usize;
    for (e, mut task) in &mut tasks {
        if applied >= budgets.uploads_per_frame {
            break; // remaining completed tasks attach next frame
        }
        let Some(result) = block_on(future::poll_once(&mut task.0)) else { continue };
        applied += 1;
        // ... existing Some/None arms unchanged ...
```
(The removal-cleanup loop below stays exactly as is — uncapped.)

2. In `renderer/mod.rs`, replace the `Update` system tuple (currently lines 67–78) and add the resources:

```rust
            .init_resource::<meshing::queue::ChunkGrid>()
            .init_resource::<meshing::queue::RemeshQueue>()
            .init_resource::<meshing::queue::MeshBudgets>()
            .add_systems(
                Update,
                (
                    (
                        meshing::queue::mark_remesh_dirty,
                        meshing::queue::prune_chunk_grid,
                        meshing::queue::spawn_mesh_tasks,
                        meshing::apply_meshed_chunks,
                    )
                        .chain(),
                    interpolate::interpolate_transforms,
                    avatar::attach_avatars,
                    hud::toggle_debug,
                    hud::update_debug_text,
                    hud::update_crosshair_visibility,
                ),
            );
```

3. Update the old pipeline tests in `meshing/mod.rs` (`async_meshing_attaches_mesh_eventually`, `same_frame_remove_and_reinsert_keeps_mesh`, `removal_while_task_in_flight_clears_mesh`): replace `add_systems(Update, (queue_meshing, apply_meshed_chunks))` with the new chain `(queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks).chain()` and add `.init_resource::<queue::ChunkGrid>() .init_resource::<queue::RemeshQueue>() .init_resource::<queue::MeshBudgets>()`. The entities in those tests have no `ChildOf`, so they take the unkeyed path — assertions stay unchanged.

- [ ] **Step 5: Run the full client suite**

Run: `cargo test -p client 2>&1 | tail -15`
Expected: everything passes, including bridge tests (`far_region_roots_at_offset_with_meshable_chunks` exercises the real spawn path) and the three rewired pipeline tests.

Run: `cargo test -p client -- --include-ignored 2>&1 | tail -5`
Expected: `removal_while_task_in_flight_clears_mesh` (slow-tagged) also passes.

- [ ] **Step 6: Live smoke test**

Run `just run` (server + client). Verify: world loads progressively nearest-first (visible ring-out during join instead of an all-at-once stall), no missing chunk faces, no holes at region borders, AO visible in creases. Underground/hidden faces: fly to terrain and confirm no z-fighting or gaps along chunk seams.

- [ ] **Step 7: Commit**

```bash
git add crates/client/src/renderer
git commit -m "feat(client): prioritized budgeted remesh queue replaces per-change task dump"
```

---

### Task 8: Oracle parity, perf gate, dependency demotion, docs

**Files:**
- Create: `crates/client/tests/mesh_parity.rs` (integration test)
- Modify: `crates/client/Cargo.toml` (move `block-mesh` from `[dependencies]` to `[dev-dependencies]`)
- Modify: `CLAUDE.md` (renderer file list: `meshing.rs` → `meshing/`)
- Test: the new integration test file

**Interfaces:**
- Consumes: `client::renderer::meshing::{binary::{mesh_quads, Face, PaddedVoxels}, neighbors::gather_padded}` — these paths must be reachable; make `renderer` and the `meshing` submodules `pub` in `crates/client/src/lib.rs`/`renderer/mod.rs` if they aren't already (check `lib.rs`; the headless test suite suggests `renderer` is already public enough — mirror whatever `crates/client/tests/` files already import).
- Consumes: vendored `block_mesh::{greedy_quads, GreedyQuadsBuffer, RIGHT_HANDED_Y_UP_CONFIG, ndshape::{ConstShape, ConstShape3u32}}` as the oracle.

- [ ] **Step 1: Demote block-mesh to dev-dependency**

In `crates/client/Cargo.toml`: remove `block-mesh = { workspace = true }` from `[dependencies]`; add a `[dev-dependencies]` section (or extend it if one exists) with `block-mesh = { workspace = true }`.

Run: `cargo build -p client 2>&1 | tail -5`
Expected: clean — proves no production client code still uses block-mesh. If anything fails, that use must be ported (only test modules may keep it; move such tests' imports under `#[cfg(test)]`).

- [ ] **Step 2: Write the parity + perf tests**

Create `crates/client/tests/mesh_parity.rs`:

```rust
//! Oracle parity and performance gates for the binary mesher against the
//! vendored block-mesh greedy mesher on identical padded input.

use std::collections::BTreeSet;

use block_mesh::ndshape::{ConstShape, ConstShape3u32};
use block_mesh::{greedy_quads, GreedyQuadsBuffer, RIGHT_HANDED_Y_UP_CONFIG};
use client::renderer::meshing::binary::{mesh_quads, PaddedVoxels};
use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

type OracleShape = ConstShape3u32<34, 34, 34>;

/// Deterministic xorshift chunk content: density ~1/4, three block types.
fn random_chunk(seed: u64) -> Vec<Voxel> {
    let mut s = seed | 1;
    let mut rng = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
    for cell in v.iter_mut() {
        if rng() % 4 == 0 {
            *cell = Voxel::new(BlockId((rng() % 3 + 1) as u16));
        }
    }
    v
}

/// Decompose our quads into (axis, positive, unit cell) faces.
fn ours(padded: &PaddedVoxels) -> BTreeSet<(usize, bool, [u32; 3])> {
    let mut set = BTreeSet::new();
    for q in mesh_quads(padded) {
        let axis = q.face.axis();
        let (ua, va) = client::renderer::meshing::binary::cross_axes(axis);
        for du in 0..q.w {
            for dv in 0..q.h {
                let mut cell = q.min;
                cell[ua] += du;
                cell[va] += dv;
                assert!(set.insert((axis, q.face.positive(), cell)), "overlapping quads");
            }
        }
    }
    set
}

/// Decompose block-mesh oracle quads into the same unit-face set.
fn oracle(padded: &PaddedVoxels) -> BTreeSet<(usize, bool, [u32; 3])> {
    let mut buffer = GreedyQuadsBuffer::new(padded.0.len());
    greedy_quads(
        &padded.0,
        &OracleShape {},
        [0; 3],
        [33; 3],
        &RIGHT_HANDED_Y_UP_CONFIG.faces,
        &mut buffer,
    );
    let mut set = BTreeSet::new();
    for (group, face) in buffer
        .quads
        .groups
        .iter()
        .zip(RIGHT_HANDED_Y_UP_CONFIG.faces.into_iter())
    {
        for quad in group.iter() {
            // Unit-face decomposition via the face's own u/v axes.
            let n = face.signed_normal();
            let (axis, positive) = if n.x != 0 {
                (0, n.x > 0)
            } else if n.y != 0 {
                (1, n.y > 0)
            } else {
                (2, n.z > 0)
            };
            let axes = [face.permutation()[1], face.permutation()[2]];
            for du in 0..quad.width {
                for dv in 0..quad.height {
                    let mut cell = [
                        quad.minimum[0] - 1,
                        quad.minimum[1] - 1,
                        quad.minimum[2] - 1,
                    ];
                    cell[axes[0] as usize] += du;
                    cell[axes[1] as usize] += dv;
                    set.insert((axis, positive, cell));
                }
            }
        }
    }
    set
}

#[test]
fn parity_with_block_mesh_on_random_chunks() {
    for seed in 1..=20u64 {
        let padded = PaddedVoxels::from_interior(&random_chunk(seed));
        assert_eq!(ours(&padded), oracle(&padded), "seed {seed}");
    }
}

#[test]
fn parity_on_structured_chunks() {
    for chunk in [
        vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT], // full
        {
            let c = game::Chunk::flat_floor(24);
            game::derive_voxels(&c.blocks, &c.chisel)
        },
        {
            let c = game::Chunk::flat_floor(8);
            game::derive_voxels(&c.blocks, &c.chisel)
        },
    ] {
        let padded = PaddedVoxels::from_interior(&chunk);
        assert_eq!(ours(&padded), oracle(&padded));
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn binary_mesher_beats_the_oracle_on_the_join_burst() {
    // The measured join burst is ~1088 dense subchunks per region. Gate:
    // the new mesher must be ≥2× faster than block-mesh on the same inputs
    // (relative gate — robust across debug/release and machines).
    let chunks: Vec<Vec<Voxel>> = (1..=64u64).map(random_chunk).collect();
    let padded: Vec<PaddedVoxels> =
        chunks.iter().map(|c| PaddedVoxels::from_interior(c)).collect();

    let t0 = std::time::Instant::now();
    let mut quads = 0usize;
    for _ in 0..17 {
        for p in &padded {
            quads += mesh_quads(p).len();
        }
    }
    let ours_t = t0.elapsed();

    let t1 = std::time::Instant::now();
    let mut oracle_quads = 0usize;
    for _ in 0..17 {
        for p in &padded {
            let mut buffer = GreedyQuadsBuffer::new(p.0.len());
            greedy_quads(
                &p.0,
                &OracleShape {},
                [0; 3],
                [33; 3],
                &RIGHT_HANDED_Y_UP_CONFIG.faces,
                &mut buffer,
            );
            oracle_quads += buffer.quads.num_quads();
        }
    }
    let oracle_t = t1.elapsed();

    assert!(quads > 0 && oracle_quads > 0);
    assert!(
        ours_t * 2 <= oracle_t,
        "binary mesher must be ≥2x block-mesh: ours {ours_t:?} vs oracle {oracle_t:?} \
         over {} chunk-meshes",
        64 * 17
    );
}
```

API notes for the implementer: the oracle decomposition uses `OrientedBlockFace::signed_normal()` and `permutation()` from the vendored fork (`crates/block-mesh/src/geometry.rs` — check the exact method names there; if `permutation()` doesn't exist, the u/v axes are recoverable from `face.quad_mesh_positions(&quad, 1.0)` by diffing corner positions, which is version-proof). Do not weaken the set-equality assertion.

- [ ] **Step 3: Run the parity tests**

Run: `cargo test -p client --test mesh_parity 2>&1 | tail -5`
Expected: 2 pass, 1 ignored.

Run: `cargo test -p client --test mesh_parity -- --include-ignored 2>&1 | tail -5`
Expected: all 3 pass. If the perf gate fails in debug, run `cargo test -p client --release --test mesh_parity -- --include-ignored` and report both numbers — if it fails in release too, the mesher has a real perf problem: profile before weakening the gate (that requires user sign-off).

- [ ] **Step 4: Update CLAUDE.md**

In the `crates/client` bullet of CLAUDE.md, change the renderer file list `(bridge.rs/convert.rs/interpolate.rs/meshing.rs/voxel_material.rs)` to `(bridge.rs/convert.rs/interpolate.rs/meshing/ (binary greedy mesher, AO, neighbor gather, remesh queue)/voxel_material.rs)`.

- [ ] **Step 5: Full-suite final gate**

```bash
just test 2>&1 | tail -5
just test-full 2>&1 | tail -5
cargo test -p server 2>&1 | tail -3   # just test-full misses -p server (known gap)
```
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/client Cargo.lock CLAUDE.md
git commit -m "test(client): block-mesh parity oracle + join-burst perf gate; block-mesh to dev-dep"
```

---

## Self-Review Notes (already applied)

- Spec §5.1 (binary mesher) → Tasks 1, 3; §5.2 (neighbor padding + dirty propagation) → Tasks 5, 6; §5.3 (AO + scheduling) → Tasks 2, 7; §5.5 (testing incl. parity + perf gate) → Tasks 4, 8. The spec's "distance-prioritized, in-frustum-first" queue is Task 7's `chunk_rank`.
- Success criterion "boundary faces against solid loaded neighbors emit zero quads" → Task 5 `solid_neighbor_culls_the_whole_shared_face` (pure) + Task 7 `meshing_uses_neighbor_data_from_the_grid` (end-to-end).
- Type consistency: `PaddedVoxels`/`Occupancy`/`cross_axes` (T1) consumed by T2/T3/T5/T8 under those exact names; `Quad {min, w, h, face, block, ao}` (T3) consumed by T4; `zone_offsets`/`zone_index` (T5) consumed by T6/T7; `MeshingTask`/`build_chunk_mesh` keep their existing names.
- Known risk flags left in-plan on purpose: `ChildOf::parent()` vs `.0` (T6), `bevy::camera::primitives` import path (verified against bevy_camera 0.18.1 — `Frustum::intersects_sphere(&Sphere, bool)` exists), block-mesh oracle face-axis recovery (T8, with a version-proof fallback).
