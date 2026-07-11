# Face Pulling (Custom Render Path) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render chunk and LOD terrain from 8-byte-per-quad storage-buffer records via a
mesh-free custom render path (no vertex/index buffers), replacing the 56-byte-per-quad
Mesh asset path — Option A from the 2026-07-09 research report, chosen by the user on
2026-07-10 over the Material-safe compaction fallback.

**Architecture:** A `QuadRecord([u32; 2])` per greedy quad is built where `quads_to_mesh`
runs today (chunk + LOD paths both funnel through `emit`), stored on a `ChunkQuads`
component, extracted to the render world, uploaded to a per-chunk storage buffer, and
drawn with `pass.draw(0..quads*6, 0..1)` by custom `RenderCommand` phase items in three
phases: `Opaque3d` (forward, custom lighting WGSL), `Opaque3dPrepass` (depth), and
`Shadow` (caster). Because we issue the draw ourselves, `@builtin(vertex_index)` starts
at 0 — this sidesteps the verified Bevy 0.18.1 blocker (mesh allocator skips
attribute-less meshes, `allocator.rs:479`; `DrawMesh` passes the slab offset as
`base_vertex`, `bevy_pbr mesh.rs:3283`, shader-invisible). A `RenderPath` resource
toggles old/new paths until parity is confirmed; the Material path is NOT deleted in
this plan.

**Tech Stack:** Bevy 0.18.1 (`bevy_render` custom phase items + `SpecializedRenderPipeline`),
WGSL, wgpu storage buffers (read-only, vertex-stage — confirmed OK on browser WebGPU).

## Global Constraints

- **Record layout LOCKED** (mirrored in Rust + WGSL; bit positions shared with pack.rs where fields overlap):
  - `w0: x:6 | y:6 | z:6 | face:3 | ao0:2 | ao1:2 | ao2:2 | ao3:2 | flip:1` (bits 30–31 zero)
  - `w1: w:6 | h:6 | layer:16` (bits 28–31 zero)
- `crates/game` and `crates/server` stay Bevy-free — every change in this plan is inside `crates/client` (+ `assets/shaders/`).
- wasm32/WebGPU compatibility is a hard requirement: no push constants (use a uniform in the chunk bind group), no `multi_draw_indirect`, read-only storage in the vertex stage only (verified allowed: Bevy keeps `wgpu::Limits::default()` on `BROWSER_WEBGPU`, `bevy_render settings.rs:85-105`).
- The old Material path must keep compiling and working behind the `RenderPath` resource until the final task's verification passes. Do not delete `voxel_texture.wgsl`, `voxel_material.rs`, or `emit.rs`.
- **Known accepted regression:** chunk-vs-chunk HZB occlusion culling (camera gets `DepthPrepass` + `OcclusionCulling` in `visibility.rs:328-334`) does not apply to custom phase items. Cave-culling BFS (`visibility.rs`) remains the chunk-culling mechanism; our prepass task keeps terrain writing depth so *mesh* entities are still occluded by terrain. Do not try to reimplement GPU occlusion for pulled chunks in this plan.
- Bevy-internals reference files (exact, from `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`, versions 0.18.1 — verified present):
  - `bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs` — the canonical NonMesh phase-item + custom RenderCommand template (bin keys at `:260-276`, no mesh bind groups at `:153`).
  - `bevy_pbr-0.18.1/src/render/mesh.rs:3255-3283` — `DrawMesh` (what we are replacing; also the model for `RenderCommandResult::Skip` handling).
  - `bevy_pbr-0.18.1/src/render/light.rs` — `queue_shadows` (model for Task 7's shadow-view iteration).
  - `bevy_render-0.18.1/src/batching/mod.rs:217-224` — batching only merges instance ranges; distinct chunks never fuse (why per-chunk draws are not a new cost).
  - Bevy's WGSL: `bevy_pbr-0.18.1/src/render/mesh_view_bindings.wgsl`, `shadows.wgsl`, `prepass/prepass.wgsl`.
  - API drift is a known hazard (CLAUDE.md): when a signature in this plan doesn't compile, align it against these files — the *shape* (phase items, bind groups, draw call) is the contract; exact generics/fields may differ.
- Tag any new test measured >100ms `#[ignore = "slow: >100ms — run via just test-full"]`. `just test` while iterating, `just test-full` before declaring done.
- Update the `justfile` in the same change whenever a new runnable workflow appears (Task 4's smoke probe).

## File Structure

- Create: `crates/client/src/renderer/meshing/records.rs` — record packing + Rust mirror of the WGSL decode (the format's source of truth).
- Modify: `crates/client/src/renderer/meshing/mod.rs` — `MeshOutput` grows records; `apply_meshed_chunks` learns the pull path.
- Modify: `crates/client/src/renderer/meshing/queue.rs` — task spawn passes the path flag.
- Modify: `crates/client/src/renderer/meshing/lod.rs` — LOD tasks produce records on the pull path.
- Create: `crates/client/src/renderer/voxel_pull/mod.rs` — plugin, `RenderPath` resource, `ChunkQuads` component, extraction.
- Create: `crates/client/src/renderer/voxel_pull/pipeline.rs` — forward + depth specialized pipelines, chunk bind-group layout.
- Create: `crates/client/src/renderer/voxel_pull/prepare.rs` — per-chunk GPU buffers/bind groups.
- Create: `crates/client/src/renderer/voxel_pull/queue.rs` — queue systems (Opaque3d, Opaque3dPrepass, Shadow) + `RenderCommand`s.
- Create: `assets/shaders/voxel_pull.wgsl` (forward) and `assets/shaders/voxel_pull_depth.wgsl` (prepass/shadow vertex-only).
- Create: `crates/client/examples/pull_smoke.rs` — self-screenshot probe (model: `crates/client/examples/gizmo_smoke.rs`).
- Modify: `crates/client/src/renderer/mod.rs` — plugin registration, `BlockArrayTexture` resource.
- Modify: `justfile` — `pull-smoke` recipe.

---

### Task 1: Quad record format (`records.rs`)

**Files:**
- Create: `crates/client/src/renderer/meshing/records.rs`
- Modify: `crates/client/src/renderer/meshing/mod.rs` (add `pub mod records;`)

**Interfaces:**
- Consumes: `binary::{Quad, Face, cross_axes}`, `ao::ao_flips_quad`, `pack::layer_for_face`, `BlockTextureLayers`.
- Produces (later tasks rely on these exact names):
  - `pub struct QuadRecord(pub [u32; 2])` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn quads_to_records(quads: &[Quad], layers: &BlockTextureLayers) -> Vec<QuadRecord>`
  - `pub fn decode_corner(rec: QuadRecord, slot: u32) -> DecodedCorner` with `pub struct DecodedCorner { pub pos: [u32; 3], pub face: Face, pub ao: u8, pub layer: u32 }`
  - `pub const CORNER_SLOTS: [[u32; 6]; 4]`

- [ ] **Step 1: Write the failing parity test**

The strongest possible pin: the existing indexed mesh (via `emit::quads_to_mesh`) is the
oracle. For every face × AO-flip combination, expanding the mesh's 6 indices per quad must
yield, slot by slot, exactly what `decode_corner` reconstructs — positions, AO, layer, and
triangle order (winding is implied by per-slot position equality).

```rust
// in records.rs
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::binary::{Face, Quad};
    use super::super::emit;
    use super::super::pack;
    use super::super::super::block_textures::BlockTextureLayers;
    use bevy::mesh::{Indices, VertexAttributeValues};
    use game::BlockId;

    fn sample_quad(face: Face, ao: [u8; 4]) -> Quad {
        Quad { min: [5, 6, 7], w: 2, h: 3, face, block: BlockId(1), ao }
    }

    #[test]
    fn record_decode_matches_the_indexed_mesh_oracle() {
        let mut layers = BlockTextureLayers::default();
        layers.0.insert(BlockId(1), [10, 20, 30]);
        for face in [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ, Face::NegZ] {
            // [3,0,0,3] triggers the AO flip rule (ao[0]+ao[3] > ao[1]+ao[2]).
            for ao in [[3u8, 3, 3, 3], [3, 0, 0, 3]] {
                let q = sample_quad(face, ao);
                let mesh = emit::quads_to_mesh(&[q], &layers).expect("one quad meshes");
                let VertexAttributeValues::Uint32x2(packed) = mesh
                    .attribute(crate::renderer::voxel_material::ATTRIBUTE_VOXEL_PACKED)
                    .unwrap()
                else { panic!("packed attribute") };
                let Indices::U32(idx) = mesh.indices().unwrap() else { panic!("u32 indices") };
                let rec = quads_to_records(&[q], &layers)[0];
                for (slot, &i) in idx.iter().enumerate() {
                    let oracle = pack::unpack(packed[i as usize]);
                    let got = decode_corner(rec, slot as u32);
                    assert_eq!(got.pos, oracle.pos, "{face:?} ao={ao:?} slot {slot}: position");
                    assert_eq!(got.ao, oracle.ao, "{face:?} ao={ao:?} slot {slot}: ao");
                    assert_eq!(got.layer, oracle.layer, "{face:?} slot {slot}: layer");
                    assert_eq!(got.face, face, "face survives the roundtrip");
                }
            }
        }
    }

    #[test]
    fn reserved_bits_stay_zero() {
        let q = sample_quad(Face::NegZ, [3, 0, 0, 3]);
        let rec = quads_to_records(&[q], &BlockTextureLayers::default())[0];
        assert_eq!(rec.0[0] >> 30, 0, "w0 bits 30-31 reserved");
        assert_eq!(rec.0[1] >> 28, 0, "w1 bits 28-31 reserved");
    }

    #[test]
    fn eight_bytes_per_quad() {
        assert_eq!(std::mem::size_of::<QuadRecord>(), 8);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p client records -- --nocapture`
Expected: FAIL — `quads_to_records`/`decode_corner` not defined.

- [ ] **Step 3: Implement the record module**

```rust
//! Per-quad 8-byte records for the pulled (mesh-free) render path.
//! Layout (LOCKED, mirrored by assets/shaders/voxel_pull.wgsl — the shader
//! only decodes; this file is the source of truth):
//!   w0: x:6 | y:6 | z:6 | face:3 | ao0:2 | ao1:2 | ao2:2 | ao3:2 | flip:1  (bits 30-31 zero)
//!   w1: w:6 | h:6 | layer:16                                               (bits 28-31 zero)
//! x/y/z/face sit at the same bit positions as pack.rs word0, and w1 equals
//! pack.rs word1 exactly, so the two formats stay diffable while both live.

use super::super::block_textures::BlockTextureLayers;
use super::ao::ao_flips_quad;
use super::binary::{cross_axes, Face, Quad};
use super::pack;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuadRecord(pub [u32; 2]);

/// Slot→corner tables indexed by `reversed*2 + flip`; corner order is
/// (0,0),(1,0),(0,1),(1,1) over (U,V). These are emit::quad_indices' four
/// patterns verbatim — the parity test enforces that they never drift.
pub const CORNER_SLOTS: [[u32; 6]; 4] = [
    [0, 1, 3, 0, 3, 2], // default winding, default diagonal
    [0, 1, 2, 1, 3, 2], // default winding, AO-flipped diagonal
    [0, 3, 1, 0, 2, 3], // reversed winding (NegX/PosY/NegZ), default diagonal
    [0, 2, 1, 1, 2, 3], // reversed winding, AO-flipped diagonal
];

pub fn pack_record(q: &Quad, layer: u32) -> QuadRecord {
    // The face plane sits at min[axis] for negative faces, +1 for positive —
    // same origin rule as emit::quads_to_mesh.
    let mut origin = q.min;
    if q.face.positive() {
        origin[q.face.axis()] += 1;
    }
    debug_assert!(origin.iter().all(|&c| c <= 33) && q.w <= 33 && q.h <= 33);
    debug_assert!(q.ao.iter().all(|&a| a <= 3) && layer <= 0xFFFF);
    let flip = ao_flips_quad(&q.ao) as u32;
    let w0 = origin[0]
        | origin[1] << 6
        | origin[2] << 12
        | q.face.index() << 18
        | (q.ao[0] as u32) << 21
        | (q.ao[1] as u32) << 23
        | (q.ao[2] as u32) << 25
        | (q.ao[3] as u32) << 27
        | flip << 29;
    let w1 = q.w | q.h << 6 | layer << 12;
    QuadRecord([w0, w1])
}

pub fn quads_to_records(quads: &[Quad], layers: &BlockTextureLayers) -> Vec<QuadRecord> {
    quads
        .iter()
        .map(|q| {
            let faces = layers.0.get(&q.block).copied().unwrap_or([0, 0, 0]);
            pack_record(q, pack::layer_for_face(faces, q.face))
        })
        .collect()
}

/// Decoded corner — the Rust mirror of the WGSL reconstruction, used by tests
/// (and only tests) as the bridge between record bits and expected geometry.
pub struct DecodedCorner {
    pub pos: [u32; 3],
    pub face: Face,
    pub ao: u8,
    pub layer: u32,
}

pub fn decode_corner(rec: QuadRecord, slot: u32) -> DecodedCorner {
    let [w0, w1] = rec.0;
    let face_id = (w0 >> 18) & 7;
    let flip = (w0 >> 29) & 1;
    // Reversed faces: NegX (1), PosY (2), NegZ (5) — from emit::quad_indices.
    let reversed = matches!(face_id, 1 | 2 | 5) as u32;
    let corner = CORNER_SLOTS[(reversed * 2 + flip) as usize][slot as usize];
    let (cu, cv) = (corner & 1, corner >> 1);
    let face = Face::from_index(face_id);
    let (ua, va) = cross_axes(face.axis());
    let mut pos = [w0 & 63, (w0 >> 6) & 63, (w0 >> 12) & 63];
    pos[ua] += cu * (w1 & 63);
    pos[va] += cv * ((w1 >> 6) & 63);
    DecodedCorner {
        pos,
        face,
        ao: ((w0 >> (21 + 2 * corner)) & 3) as u8,
        layer: (w1 >> 12) & 0xFFFF,
    }
}
```

Add `pub mod records;` next to the other module declarations in
`crates/client/src/renderer/meshing/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p client records`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/meshing/records.rs crates/client/src/renderer/meshing/mod.rs
git commit -m "feat(client): 8-byte per-quad records + decode mirror for the pull path"
```

---

### Task 2: `ChunkQuads` through the mesh and LOD pipelines

**Files:**
- Modify: `crates/client/src/renderer/meshing/mod.rs` (`MeshOutput`, `mesh_chunk`, `apply_meshed_chunks`)
- Modify: `crates/client/src/renderer/meshing/queue.rs` (`spawn_mesh_tasks` passes the flag)
- Modify: `crates/client/src/renderer/meshing/lod.rs` (LOD task output + apply)
- Create: `crates/client/src/renderer/voxel_pull/mod.rs` (component + resource only, this task)

**Interfaces:**
- Consumes: `records::{QuadRecord, quads_to_records}` from Task 1.
- Produces:
  - `pub struct RenderPath { pub pull: bool }` (`Resource`), default from `pub const DEFAULT_PULL_PATH: bool = false` (flip in Task 8; a named constant per the project's tunables preference).
  - `pub struct ChunkQuads(pub Arc<Vec<QuadRecord>>)` (`Component`, `#[require(Visibility)]` so cave-culling's `&mut Visibility` query and visibility propagation work without `Mesh3d`).
  - `MeshOutput` becomes `{ pub mesh: Option<Mesh>, pub records: Option<Vec<QuadRecord>>, pub connectivity: u16 }`; `mesh_chunk(padded, layers, pull: bool)` fills exactly one of `mesh`/`records`.

- [ ] **Step 1: Write the failing test** (in `meshing/mod.rs`'s test module, alongside `async_meshing_attaches_mesh_eventually` — reuse its app scaffold and `settle` helper verbatim, they are the local idiom)

```rust
#[test]
fn pull_path_attaches_records_and_no_mesh() {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
        .init_resource::<queue::ChunkGrid>()
        .init_resource::<queue::RemeshQueue>()
        .init_resource::<queue::MeshBudgets>()
        .init_resource::<queue::FrameUploads>()
        .insert_resource(crate::renderer::voxel_pull::RenderPath { pull: true })
        .add_systems(
            Update,
            (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                .chain(),
        );
    let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
    voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
    let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.update();
        if let Some(q) = app.world().entity(e).get::<crate::renderer::voxel_pull::ChunkQuads>() {
            assert_eq!(q.0.len(), 6, "single voxel = 6 quads");
            assert!(!app.world().entity(e).contains::<Mesh3d>(), "pull path must not attach a mesh");
            assert!(app.world().entity(e).contains::<Visibility>(), "required for cave-culling writes");
            assert!(app.world().entity(e).contains::<bevy::camera::primitives::Aabb>());
            break;
        }
        assert!(std::time::Instant::now() < deadline, "records never attached");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p client pull_path_attaches_records`
Expected: FAIL — `voxel_pull` module / `RenderPath` unresolved.

- [ ] **Step 3: Implement**

Create `crates/client/src/renderer/voxel_pull/mod.rs`:

```rust
//! The pulled (mesh-free) chunk render path: per-quad records in storage
//! buffers, drawn by custom phase items. Render-world half arrives in later
//! tasks; this module starts with the main-world component + toggle.

use std::sync::Arc;

use bevy::prelude::*;

use super::meshing::records::QuadRecord;

/// Default for the old-vs-pull render path toggle. Flipped to `true` once
/// visual parity is confirmed (Task 8); kept a named constant so it is
/// findable and overridable in tests either way.
pub const DEFAULT_PULL_PATH: bool = false;

#[derive(Resource)]
pub struct RenderPath {
    pub pull: bool,
}

impl Default for RenderPath {
    fn default() -> Self {
        Self { pull: DEFAULT_PULL_PATH }
    }
}

/// The pull-path replacement for `Mesh3d`: every visible greedy quad of one
/// chunk (or LOD cell), as GPU-ready records. `Arc` so render-world extraction
/// clones the handle, not the data. Requires `Visibility` because without
/// `Mesh3d` nothing else inserts it, and cave-culling writes `&mut Visibility`.
#[derive(Component, Clone)]
#[require(Visibility)]
pub struct ChunkQuads(pub Arc<Vec<QuadRecord>>);
```

Register in `crates/client/src/renderer/mod.rs`: `pub mod voxel_pull;` and
`app.init_resource::<voxel_pull::RenderPath>()` wherever the other renderer resources
are initialized.

In `meshing/mod.rs`:

```rust
pub struct MeshOutput {
    pub mesh: Option<Mesh>,
    pub records: Option<Vec<records::QuadRecord>>,
    pub connectivity: u16,
}

/// Mesh OR records (per `pull`) + connectivity in one pass over the snapshot.
pub fn mesh_chunk(padded: &PaddedVoxels, layers: &BlockTextureLayers, pull: bool) -> MeshOutput {
    let quads = binary::mesh_quads(padded);
    let (mesh, records) = if pull {
        let r = records::quads_to_records(&quads, layers);
        (None, (!r.is_empty()).then_some(r))
    } else {
        (emit::quads_to_mesh(&quads, layers), None)
    };
    MeshOutput { mesh, records, connectivity: visibility::face_connectivity(padded) }
}
```

`queue::spawn_mesh_tasks` gains `path: Res<RenderPath>` and captures `let pull = path.pull;`
into the async closure, passing it to `mesh_chunk` (find the existing `mesh_chunk(...)`
call inside the spawned task and add the argument).

`apply_meshed_chunks` handles the third case — records present. The `Some(mesh)` arm stays
byte-identical; add before the `match mesh`:

```rust
if let Some(records) = records {
    let mut ec = commands.entity(e);
    ec.insert((
        super::voxel_pull::ChunkQuads(std::sync::Arc::new(records)),
        bevy::camera::primitives::Aabb {
            center: bevy::math::Vec3A::splat(16.0),
            half_extents: bevy::math::Vec3A::splat(16.0),
        },
        bevy::camera::visibility::NoAutoAabb,
        visibility::ChunkConnectivity(connectivity),
    ));
    ec.remove::<MeshingTask>();
    continue;
}
```

and extend both removal arms (the `None` arm and the `removed.read()` sweep) to also
remove `super::voxel_pull::ChunkQuads` next to `Mesh3d`.

In `lod.rs`: `LodMeshTask`'s builder calls (`emit::quads_to_mesh(&quads, layers)` at
`lod.rs:199` and its callers) get the same `pull` split — return
`MeshOutput`-shaped data (reuse `MeshOutput`), and `apply_lod_meshes` mirrors
`apply_meshed_chunks`: records → insert `ChunkQuads` + its existing biased LOD `Aabb`
(`center (16, 16.5, 16), half (16, 16.5, 16)`) + `NoAutoAabb`, no `Mesh3d`/material; its
removal arm strips `ChunkQuads` too.

- [ ] **Step 4: Run the suite**

Run: `cargo test -p client`
Expected: new test passes; every pre-existing test still passes (they run with
`RenderPath::default()` = mesh path — `spawn_mesh_tasks` must fall back to
`RenderPath::default()` semantics if the resource is absent in old test scaffolds: use
`Option<Res<RenderPath>>` and `path.map_or(DEFAULT_PULL_PATH, |p| p.pull)`).

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer
git commit -m "feat(client): ChunkQuads records flow through mesh+LOD pipelines behind RenderPath"
```

---

### Task 3: Forward WGSL (`voxel_pull.wgsl`), unlit v0

**Files:**
- Create: `assets/shaders/voxel_pull.wgsl`

**Interfaces:**
- Consumes: record layout from Task 1 (bit-exact), Bevy's `mesh_view_bindings::view` (group 0).
- Produces: group(1) bind layout that Task 4's pipeline must create in this exact order:
  binding 0 = `var<storage, read> quads: array<vec2<u32>>`,
  binding 1 = `var<uniform> chunk: ChunkUniform { world_from_local: mat4x4<f32>, quad_count: u32 }`,
  binding 2 = block texture array, binding 3 = its sampler.
  Entry points `vertex` / `fragment`.

- [ ] **Step 1: Write the shader**

No GPU test exists headless; correctness is pinned by Task 1's Rust mirror (this decode is
its transliteration — keep them in lockstep) and verified live by Task 4's smoke probe.

```wgsl
// Pulled voxel forward shader. Record layout mirrored from
// crates/client/src/renderer/meshing/records.rs (the source of truth):
//   w0: x:6|y:6|z:6|face:3|ao0:2|ao1:2|ao2:2|ao3:2|flip:1   w1: w:6|h:6|layer:16
#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::view_transformations::position_world_to_clip

struct ChunkUniform {
    world_from_local: mat4x4<f32>,
    quad_count: u32,
}

@group(1) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(1) @binding(1) var<uniform> chunk: ChunkUniform;
@group(1) @binding(2) var mat_array_texture: texture_2d_array<f32>;
@group(1) @binding(3) var mat_array_texture_sampler: sampler;

// Locked to Face::index() order (PosX, NegX, PosY, NegY, PosZ, NegZ) — same
// tables as voxel_texture.wgsl (copied: WGSL has no cross-file consts here).
const FACE_NORMALS = array<vec3<f32>, 6>(
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0),
    vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, -1.0),
);
const FACE_U_MASK = array<vec3<f32>, 6>(
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(1.0, 0.0, 0.0),
);
const FACE_V_MASK = array<vec3<f32>, 6>(
    vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, -1.0, 0.0), vec3<f32>(0.0, -1.0, 0.0),
);
const BLOCK_VOXELS: f32 = 16.0;

// CORNER_SLOTS from records.rs, flattened: variant*6 + slot.
const CORNER_SLOTS = array<u32, 24>(
    0u, 1u, 3u, 0u, 3u, 2u,
    0u, 1u, 2u, 1u, 3u, 2u,
    0u, 3u, 1u, 0u, 2u, 3u,
    0u, 2u, 1u, 1u, 2u, 3u,
);

// Per-face (U,V) axis ids from binary::cross_axes: X faces→(1,2), Y→(0,2), Z→(0,1).
const FACE_UA = array<u32, 6>(1u, 1u, 0u, 0u, 0u, 0u);
const FACE_VA = array<u32, 6>(2u, 2u, 2u, 2u, 1u, 1u);

struct PullVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shade: f32,
    @location(4) @interpolate(flat) layer: u32,
}

@vertex
fn vertex(@builtin(vertex_index) vi: u32) -> PullVertexOutput {
    let quad = vi / 6u;
    let slot = vi % 6u;
    let rec = quads[quad];
    let w0 = rec.x;
    let w1 = rec.y;
    let face = (w0 >> 18u) & 7u;
    let flip = (w0 >> 29u) & 1u;
    // Reversed winding faces: NegX(1), PosY(2), NegZ(5).
    var reversed = 0u;
    if (face == 1u || face == 2u || face == 5u) { reversed = 1u; }
    let corner = CORNER_SLOTS[(reversed * 2u + flip) * 6u + slot];
    let cu = corner & 1u;
    let cv = corner >> 1u;

    var pos = vec3<u32>(w0 & 63u, (w0 >> 6u) & 63u, (w0 >> 12u) & 63u);
    pos[FACE_UA[face]] += cu * (w1 & 63u);
    pos[FACE_VA[face]] += cv * ((w1 >> 6u) & 63u);
    let local_pos = vec3<f32>(pos);

    var out: PullVertexOutput;
    out.world_position = chunk.world_from_local * vec4<f32>(local_pos, 1.0);
    out.position = position_world_to_clip(out.world_position.xyz);
    // Chunk transforms are translation+uniform-scale only; the normal needs no
    // inverse-transpose.
    out.world_normal = FACE_NORMALS[face];
    out.uv = vec2<f32>(
        dot(local_pos, FACE_U_MASK[face]),
        dot(local_pos, FACE_V_MASK[face]),
    ) / BLOCK_VOXELS;
    let ao = (w0 >> (21u + 2u * corner)) & 3u;
    // Must match ao::AO_CURVE / pack::shade (test-pinned there).
    out.shade = 0.55 + 0.15 * f32(ao);
    out.layer = (w1 >> 12u) & 65535u;
    return out;
}

@fragment
fn fragment(in: PullVertexOutput) -> @location(0) vec4<f32> {
    let tex = textureSample(mat_array_texture, mat_array_texture_sampler, in.uv, in.layer);
    // Unlit v0 — Task 5 replaces this with directional light + shadows + ambient.
    return vec4<f32>(tex.rgb * in.shade, 1.0);
}
```

- [ ] **Step 2: Sanity-check it parses**

Run: `naga assets/shaders/voxel_pull.wgsl` if the `naga-cli` binary is available
(`cargo install naga-cli` once, or skip — Task 4's first client run validates it through
Bevy's pipeline cache and logs WGSL errors with line numbers).
Expected: no parse errors (bevy_pbr imports resolve only inside Bevy — naga-cli will flag
them; comment the two `#import` lines out for the standalone parse check if used).

- [ ] **Step 3: Commit**

```bash
git add assets/shaders/voxel_pull.wgsl
git commit -m "feat(client): pulled-quad forward WGSL (unlit v0)"
```

---

### Task 4: Render-world plugin — extract, prepare, Opaque3d queue, draw

The heart of Option A. Model every piece on
`bevy-0.18.1/examples/shader_advanced/custom_phase_item.rs` (READ IT FIRST — it is the
compiling 0.18 reference for phase registration, bin keys, and the render-command chain);
where this plan's code and that file disagree on a signature, the example wins.

**Files:**
- Create: `crates/client/src/renderer/voxel_pull/pipeline.rs`
- Create: `crates/client/src/renderer/voxel_pull/prepare.rs`
- Create: `crates/client/src/renderer/voxel_pull/queue.rs`
- Modify: `crates/client/src/renderer/voxel_pull/mod.rs` (plugin wiring)
- Modify: `crates/client/src/renderer/mod.rs` (`BlockArrayTexture` resource, plugin registration)
- Create: `crates/client/examples/pull_smoke.rs`
- Modify: `justfile`

**Interfaces:**
- Consumes: `ChunkQuads`/`RenderPath` (Task 2), WGSL group(1) layout (Task 3, binding order locked).
- Produces:
  - `pub struct BlockArrayTexture(pub Handle<Image>)` (`Resource`, main world + extracted clone) — inserted in `renderer/mod.rs` right where the array image handle already exists (line ~252, next to `ChunkMaterial`).
  - `VoxelPullPlugin` (added to the app unconditionally; its queue systems no-op when no `ChunkQuads` exist, so the toggle needs no plugin gymnastics).
  - Render-world component `GpuChunkQuads { bind_group: BindGroup, quad_count: u32 }` — Tasks 6/7 reuse it.
  - `VoxelPullPipeline` resource with `chunk_layout: BindGroupLayout` and cached forward pipeline specialization — Tasks 6/7 add depth variants beside it.

- [ ] **Step 1: Extraction** (in `mod.rs`)

```rust
/// Render-world copy of one chunk's draw inputs. Re-extracted every frame
/// (cheap: Arc clone + matrices); buffer upload happens only when `changed`.
#[derive(Component)]
pub struct ExtractedChunkQuads {
    pub records: Arc<Vec<QuadRecord>>,
    pub world_from_local: Mat4,
    /// World-space AABB for per-view frustum tests at queue time (Bevy's
    /// visibility system only frustum-culls VisibilityClass'd entities, which
    /// mesh-less chunks are not).
    pub center: Vec3,
    pub half_extents: Vec3,
    pub changed: bool,
}

pub fn extract_chunk_quads(
    mut commands: Commands,
    chunks: Extract<
        Query<(
            RenderEntity,
            &ChunkQuads,
            Ref<ChunkQuads>,
            &GlobalTransform,
            &bevy::camera::primitives::Aabb,
            &InheritedVisibility,
        )>,
    >,
) {
    for (entity, quads, quads_ref, transform, aabb, vis) in &chunks {
        let mut ec = commands.entity(entity);
        // Cave-culling hides via Visibility → InheritedVisibility=false.
        if !vis.get() {
            ec.remove::<ExtractedChunkQuads>();
            continue;
        }
        let world_from_local = transform.to_matrix();
        let center = transform.transform_point(Vec3::from(aabb.center));
        let half = transform.scale() * Vec3::from(aabb.half_extents);
        ec.insert(ExtractedChunkQuads {
            records: quads.0.clone(),
            world_from_local,
            center,
            half_extents: half,
            changed: quads_ref.is_changed(),
        });
    }
}
```

(`RenderEntity`/sync-world plumbing: `ChunkQuads` needs `SyncToRenderWorld` semantics —
derive it by adding `bevy::render::sync_world::SyncToRenderWorld` to the `#[require(...)]`
list on `ChunkQuads`. `custom_phase_item.rs` shows the working pattern.)

- [ ] **Step 2: Pipeline** (`pipeline.rs`)

```rust
use bevy::{
    core_pipeline::core_3d::CORE_3D_DEPTH_FORMAT,
    pbr::MeshPipelineViewLayouts,
    prelude::*,
    render::{
        render_resource::{binding_types as bt, *},
        renderer::RenderDevice,
        view::ViewTarget,
    },
    shader::Shader,
};

#[derive(Resource)]
pub struct VoxelPullPipeline {
    pub chunk_layout: BindGroupLayout,
    pub forward_shader: Handle<Shader>,
    pub view_layouts: MeshPipelineViewLayouts,
}

impl FromWorld for VoxelPullPipeline {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        // Binding order LOCKED to voxel_pull.wgsl group(1).
        let chunk_layout = device.create_bind_group_layout(
            "voxel_pull_chunk_layout",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::VERTEX_FRAGMENT,
                (
                    bt::storage_buffer_read_only::<[u32; 2]>(false),
                    bt::uniform_buffer::<ChunkUniform>(false),
                    bt::texture_2d_array(TextureSampleType::Float { filterable: true }),
                    bt::sampler(SamplerBindingType::Filtering),
                ),
            ),
        );
        Self {
            chunk_layout,
            forward_shader: world.load_asset("shaders/voxel_pull.wgsl"),
            view_layouts: world.resource::<MeshPipelineViewLayouts>().clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxelPullPipelineKey {
    pub msaa_samples: u32,
    pub hdr: bool,
}

impl SpecializedRenderPipeline for VoxelPullPipeline {
    type Key = VoxelPullPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        // View layout variant must match what SetMeshViewBindGroup binds for
        // this view — derive the MeshPipelineKey the same way bevy_pbr does
        // (from_msaa_samples | from_hdr) and take the corresponding layout.
        // Reference: bevy_pbr-0.18.1/src/render/mesh.rs (MeshPipeline::specialize,
        // view_layout selection) — align the exact getter name there.
        let mesh_key = bevy::pbr::MeshPipelineKey::from_msaa_samples(key.msaa_samples)
            | bevy::pbr::MeshPipelineKey::from_hdr(key.hdr);
        let view_layout = self.view_layouts.get_view_layout(mesh_key.into()).clone();
        RenderPipelineDescriptor {
            label: Some("voxel_pull_forward".into()),
            layout: vec![view_layout, self.chunk_layout.clone()],
            vertex: VertexState {
                shader: self.forward_shader.clone(),
                entry_point: "vertex".into(),
                buffers: vec![], // the whole point
                ..default()
            },
            fragment: Some(FragmentState {
                shader: self.forward_shader.clone(),
                entry_point: "fragment".into(),
                targets: vec![Some(ColorTargetState {
                    format: if key.hdr { ViewTarget::TEXTURE_FORMAT_HDR } else { TextureFormat::bevy_default() },
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            primitive: PrimitiveState {
                cull_mode: Some(Face::Back),
                ..default()
            },
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: CompareFunction::GreaterEqual, // Bevy reverse-Z
                stencil: default(),
                bias: default(),
            }),
            multisample: MultisampleState { count: key.msaa_samples, ..default() },
            ..default()
        }
    }
}
```

with `ChunkUniform`:

```rust
#[derive(Clone, Copy, ShaderType)]
pub struct ChunkUniform {
    pub world_from_local: Mat4,
    pub quad_count: u32,
}
```

- [ ] **Step 3: Prepare** (`prepare.rs`) — upload buffers + bind group for changed chunks

```rust
#[derive(Component)]
pub struct GpuChunkQuads {
    pub bind_group: BindGroup,
    pub quad_count: u32,
}

pub fn prepare_chunk_quads(
    mut commands: Commands,
    chunks: Query<(Entity, &ExtractedChunkQuads)>,
    existing: Query<&GpuChunkQuads>,
    pipeline: Res<VoxelPullPipeline>,
    device: Res<RenderDevice>,
    queue: Res<bevy::render::renderer::RenderQueue>,
    images: Res<RenderAssets<GpuImage>>,
    block_texture: Res<BlockArrayTexture>,
) {
    let Some(texture) = images.get(&block_texture.0) else { return };
    for (entity, extracted) in &chunks {
        if !extracted.changed && existing.contains(entity) {
            continue; // steady state: nothing to upload
        }
        let records = device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("voxel_pull_records"),
            contents: bytemuck::cast_slice(&extracted.records),
            usage: BufferUsages::STORAGE,
        });
        let uniform = ChunkUniform {
            world_from_local: extracted.world_from_local,
            quad_count: extracted.records.len() as u32,
        };
        let mut buf = UniformBuffer::from(uniform);
        buf.write_buffer(&device, &queue);
        let bind_group = device.create_bind_group(
            "voxel_pull_chunk",
            &pipeline.chunk_layout,
            &BindGroupEntries::sequential((
                records.as_entire_binding(),
                buf.binding().unwrap(),
                &texture.texture_view,
                &texture.sampler,
            )),
        );
        commands.entity(entity).insert(GpuChunkQuads {
            bind_group,
            quad_count: extracted.records.len() as u32,
        });
    }
}
```

(`QuadRecord` needs `bytemuck::Pod + Zeroable` derives — add
`bytemuck = "1"` to `crates/client/Cargo.toml` if not already a dependency, and
`#[repr(transparent)] #[derive(bytemuck::Pod, bytemuck::Zeroable)]` on `QuadRecord`.
`BlockArrayTexture(pub Handle<Image>)` derives `ExtractResource + Resource + Clone`, so
`ExtractResourcePlugin::<BlockArrayTexture>` mirrors the same type into the render
world — the prepare system reads it directly.)

- [ ] **Step 4: Queue + draw** (`queue.rs`) — copy the structure of
`custom_phase_item.rs:260-276` exactly, then adapt:

```rust
pub type DrawVoxelPull = (
    SetItemPipeline,
    bevy::pbr::SetMeshViewBindGroup<0>,
    DrawChunkQuads,
);

pub struct DrawChunkQuads;

impl<P: PhaseItem> RenderCommand<P> for DrawChunkQuads {
    type Param = ();
    type ViewQuery = ();
    type ItemQuery = Read<GpuChunkQuads>;

    fn render<'w>(
        _item: &P,
        _view: (),
        gpu: Option<&'w GpuChunkQuads>,
        _param: (),
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some(gpu) = gpu else { return RenderCommandResult::Skip };
        pass.set_bind_group(1, &gpu.bind_group, &[]);
        pass.draw(0..gpu.quad_count * 6, 0..1);
        RenderCommandResult::Success
    }
}

pub fn queue_voxel_pull(
    pipeline: Res<VoxelPullPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<VoxelPullPipeline>>,
    cache: Res<PipelineCache>,
    draw_functions: Res<DrawFunctions<Opaque3d>>,
    mut phases: ResMut<ViewBinnedRenderPhases<Opaque3d>>,
    views: Query<(&ExtractedView, &bevy::camera::primitives::Frustum, &Msaa)>,
    chunks: Query<(Entity, &MainEntity, &ExtractedChunkQuads)>,
) {
    let draw = draw_functions.read().id::<DrawVoxelPull>();
    for (view, frustum, msaa) in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else { continue };
        let pipeline_id = pipelines.specialize(
            &cache,
            &pipeline,
            VoxelPullPipelineKey { msaa_samples: msaa.samples(), hdr: view.hdr },
        );
        for (entity, main_entity, chunk) in &chunks {
            // center/half_extents are already world-space (extraction applied
            // the chunk transform), so the OBB test runs with identity.
            let aabb = bevy::camera::primitives::Aabb {
                center: chunk.center.into(),
                half_extents: chunk.half_extents.into(),
            };
            if !frustum.intersects_obb(&aabb, &Mat4::IDENTITY.into(), true, false) {
                continue;
            }
            phase.add(
                Opaque3dBatchSetKey {
                    pipeline: pipeline_id,
                    draw_function: draw,
                    material_bind_group_index: None,
                    vertex_slab: default(),
                    index_slab: None,
                    lightmap_slab: None,
                },
                Opaque3dBinKey {
                    asset_id: AssetId::<Mesh>::invalid().untyped(),
                },
                (entity, *main_entity),
                InputUniformIndex::default(),
                BinnedRenderPhaseType::NonMesh,
            );
        }
    }
}
```

The bin/batch-set key field lists above are from `custom_phase_item.rs:260-276` — if the
0.18.1 example's fields differ, the example is authoritative. The manual frustum cull is
deliberate: chunk entities carry no `VisibilityClass`, so Bevy's own frustum culling
never sees them; without this test every chunk would draw in every view.

- [ ] **Step 5: Plugin wiring** (`mod.rs`)

```rust
pub struct VoxelPullPlugin;

impl Plugin for VoxelPullPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<BlockArrayTexture>::default());
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<SpecializedRenderPipelines<VoxelPullPipeline>>()
            .add_render_command::<Opaque3d, DrawVoxelPull>()
            .add_systems(ExtractSchedule, extract_chunk_quads)
            .add_systems(
                Render,
                (
                    prepare_chunk_quads.in_set(RenderSystems::PrepareBindGroups),
                    queue_voxel_pull.in_set(RenderSystems::Queue),
                ),
            );
    }

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app.init_resource::<VoxelPullPipeline>();
    }
}
```

Register `VoxelPullPlugin` in the client app builder (`renderer/mod.rs` or `main.rs`,
wherever render plugins are added), and insert
`BlockArrayTexture(handle.clone())` beside the `ChunkMaterial` insertion at
`renderer/mod.rs:252-258`.

- [ ] **Step 6: Compile against the real APIs**

Run: `cargo check -p client 2>&1 | head -80`
Expected: iterate until clean. Every failure here is API drift — fix by opening the
referenced registry file (`custom_phase_item.rs` first) and aligning names. Do NOT
trust IDE diagnostics mid-edit (project memory: they lag; use the compiler).

- [ ] **Step 7: Smoke probe** — create `crates/client/examples/pull_smoke.rs`, modeled
line-for-line on `crates/client/examples/gizmo_smoke.rs` (the existing self-screenshot
probe): build the client `App` the same way that example does, but
`insert_resource(RenderPath { pull: true })`, connect to the offline/local world (the
`?offline`/`local_server.rs` path the wasm build uses — no server process), wait for
`ChunkQuads` entities to exist, take a screenshot via the same screenshot API
`gizmo_smoke.rs` uses, and assert a non-trivial fraction of pixels differ from the clear
color. Add to `justfile`:

```make
# Self-screenshot probe: pulled-quad render path draws real terrain.
pull-smoke:
    {{cargo}} run -p client --example pull_smoke
```

Run: `just pull-smoke`
Expected: exits 0, screenshot saved, "terrain pixels detected" (or the probe's analogous
assertion) printed. Also eyeball the screenshot — geometry must look identical in shape to
the mesh path (AO shading present, textures mapped per block).

- [ ] **Step 8: Regression check + commit**

Run: `just test` (all green) and `just run` briefly with default `RenderPath` (mesh path
untouched, game renders as before).

```bash
git add crates/client assets/shaders justfile
git commit -m "feat(client): mesh-free pulled-quad render path (Opaque3d, unlit) + smoke probe"
```

---

### Task 5: Lighting parity in the forward shader

**Files:**
- Modify: `assets/shaders/voxel_pull.wgsl` (fragment + imports only; vertex decode untouched)

**Interfaces:**
- Consumes: group(0) view bindings (already bound by `SetMeshViewBindGroup<0>`).
- Produces: lit output visually matching the Material path (directional light + cascaded shadows received + ambient), same `PullVertexOutput`.

- [ ] **Step 1: Replace the fragment (and add imports)**

```wgsl
#import bevy_pbr::{
    mesh_view_bindings::{view, lights},
    shadows::fetch_directional_shadow,
}

@fragment
fn fragment(in: PullVertexOutput) -> @location(0) vec4<f32> {
    let tex = textureSample(mat_array_texture, mat_array_texture_sampler, in.uv, in.layer);
    let base = tex.rgb * in.shade;
    let n = normalize(in.world_normal);
    var lit = lights.ambient_color.rgb;
    let n_dir = min(lights.n_directional_lights, 4u);
    for (var i = 0u; i < n_dir; i += 1u) {
        let light = lights.directional_lights[i];
        let ndotl = max(dot(n, light.direction_to_light), 0.0);
        var shadow = 1.0;
        if (ndotl > 0.0) {
            let view_z = dot(vec4<f32>(view.world_from_view[2].xyz, -dot(view.world_from_view[2].xyz, view.world_position)), in.world_position);
            shadow = fetch_directional_shadow(i, in.world_position, n, view_z);
        }
        lit += light.color.rgb * ndotl * shadow;
    }
    return vec4<f32>(base * lit, 1.0);
}
```

Field names (`lights.n_directional_lights`, `directional_lights[i].direction_to_light`,
`fetch_directional_shadow(light_id, world_position, normal, view_z)`) come from
`bevy_pbr-0.18.1/src/render/mesh_view_types.wgsl` and `shadows.wgsl` — verify each against
those files; the `view_z` construction mirrors `prepass_utils`/`pbr_functions.wgsl` usage
(grep `fetch_directional_shadow(` there and copy its caller's `view_z` expression
verbatim). If shadow-filtering shader defs (e.g. `PCF` variants) are required by
`shadows.wgsl` in 0.18.1, add the same defs `MeshPipeline::specialize` sets to the
pipeline's `vertex/fragment.shader_defs` in `pipeline.rs` (grep `SHADOW_FILTER_METHOD` in
`bevy_pbr-0.18.1/src/render/mesh.rs` and mirror).

- [ ] **Step 2: Visual A/B**

Run: `just pull-smoke` (screenshot with lighting), then `just run` twice — once default
(mesh path), once with `RenderPath { pull: true }` wired temporarily via
`DEFAULT_PULL_PATH = true` — and compare: sun direction/shadow edges/ambient level match
within eyeball tolerance; terrain in shadow is darkened on both.
Expected: no obvious lighting difference; shadows fall identically (they come from the
same shadow maps — chunks don't CAST yet; use avatar/entity shadows onto terrain as the
receiving check).

- [ ] **Step 3: Commit**

```bash
git add assets/shaders/voxel_pull.wgsl crates/client/src/renderer/voxel_pull/pipeline.rs
git commit -m "feat(client): directional light + shadow receive in pulled-quad shader"
```

---

### Task 6: Depth prepass phase (keep the HZB fed)

The camera runs `DepthPrepass` + `OcclusionCulling` (`visibility.rs:328-334`). Without
this task, pulled terrain writes no prepass depth → the HZB sees empty terrain → mesh
entities (avatars etc.) behind mountains stop being occlusion-culled, and any
prepass-depth consumer breaks. Chunk-vs-chunk HZB culling is NOT restored (accepted
regression — Global Constraints).

**Files:**
- Create: `assets/shaders/voxel_pull_depth.wgsl`
- Modify: `crates/client/src/renderer/voxel_pull/pipeline.rs` (depth pipeline)
- Modify: `crates/client/src/renderer/voxel_pull/queue.rs` (prepass queue + command)
- Modify: `crates/client/src/renderer/voxel_pull/mod.rs` (registration)

**Interfaces:**
- Consumes: `GpuChunkQuads`, `ExtractedChunkQuads`, `VoxelPullPipeline.chunk_layout`.
- Produces: `DrawVoxelPullDepth` render command reused by Task 7.

- [ ] **Step 1: Depth WGSL** — `voxel_pull_depth.wgsl`: copy the `vertex` fn and decode
constants from `voxel_pull.wgsl`, output only `@builtin(position)`, no fragment entry
point. Prepass views bind a different view layout: use
`bevy::pbr::SetPrepassViewBindGroup<0>` (from `bevy_pbr::prepass`) in the draw chain and
import the prepass view uniform the way `bevy_pbr-0.18.1/src/prepass/prepass.wgsl` does
(`#import bevy_pbr::prepass_bindings` / `mesh_view_bindings::view` under the
`PREPASS_PIPELINE` def — copy its import block and clip-position computation exactly;
world→clip goes through the same `position_world_to_clip`).

- [ ] **Step 2: Depth pipeline + queue** — in `pipeline.rs`, add a second
`SpecializedRenderPipeline` impl (or key variant) `VoxelPullDepthPipeline`:
layout `[prepass_view_layout, chunk_layout]` (get the prepass view layout from
`bevy::pbr::PrepassPipeline`'s resource — grep `view_layout` in
`bevy_pbr-0.18.1/src/prepass/mod.rs` for the getter), vertex-only
(`fragment: None`), `depth_stencil` format `CORE_3D_DEPTH_FORMAT`, `GreaterEqual`,
multisample count 1 for prepass unless `Msaa` says otherwise (mirror what
`PrepassPipeline::specialize` does with msaa). In `queue.rs`, add
`queue_voxel_pull_prepass` targeting `ViewBinnedRenderPhases<Opaque3dPrepass>`
(`bevy::core_pipeline::prepass::Opaque3dPrepass`), same frustum cull, draw chain:

```rust
pub type DrawVoxelPullDepth = (
    SetItemPipeline,
    bevy::pbr::SetPrepassViewBindGroup<0>,
    DrawChunkQuads,
);
```

Register `add_render_command::<Opaque3dPrepass, DrawVoxelPullDepth>()` and the queue
system in the plugin. The `Opaque3dPrepass` bin-key shape: grep `Opaque3dPrepass` in
`bevy_pbr-0.18.1/src/prepass/mod.rs` (`queue_prepass_material_meshes`) and mirror the
key construction with `AssetId::<Mesh>::invalid().untyped()` as the asset id, like Task 4.

- [ ] **Step 3: Verify**

Run: `cargo check -p client`, then `just pull-smoke`.
Expected: compiles; smoke screenshot unchanged. Then `just run` with pull path on: enable
Bevy's depth-prepass debug view if wired, else verify indirectly — spawn location renders
identically and no wgpu validation errors appear in the log (a mismatched prepass view
layout fails loudly at pipeline creation).

- [ ] **Step 4: Commit**

```bash
git add assets/shaders/voxel_pull_depth.wgsl crates/client/src/renderer/voxel_pull
git commit -m "feat(client): pulled-quad depth prepass — HZB still sees terrain"
```

---

### Task 7: Shadow casting

**Files:**
- Modify: `crates/client/src/renderer/voxel_pull/queue.rs` (shadow queue)
- Modify: `crates/client/src/renderer/voxel_pull/pipeline.rs` (shadow key variant)
- Modify: `crates/client/src/renderer/voxel_pull/mod.rs` (registration)

**Interfaces:**
- Consumes: `DrawVoxelPullDepth` (Task 6), `GpuChunkQuads`.
- Produces: terrain shadows on terrain and on entities, cast by pulled chunks.

- [ ] **Step 1: Queue into the Shadow phase.** Model on `queue_shadows` in
`bevy_pbr-0.18.1/src/render/light.rs`: iterate shadow views
(`ViewBinnedRenderPhases<Shadow>` keyed by each light's cascade view entities), reuse
the depth pipeline specialized with the shadow map format (grep `SHADOW_FORMAT` /
`ShadowPassNode` in `light.rs` — mirror its `depth_stencil` format and any
`UNCLIPPED_DEPTH_ORTHO` handling for directional cascades; the WGSL from Task 6 already
carries the `UNCLIPPED_DEPTH_ORTHO_EMULATION` branch if copied from `prepass.wgsl` —
keep it). Frustum-cull per cascade view with the same world-AABB test. `Shadow` bin key:
mirror `queue_shadows`' construction, `AssetId::<Mesh>::invalid().untyped()` asset id.
Register `add_render_command::<Shadow, DrawVoxelPullDepth>()`.

- [ ] **Step 2: Verify live.** `just run` with pull path on, walk to terrain relief at a
sun angle: terrain casts shadows onto lower terrain and onto the avatar; toggling back to
the mesh path shows the same shadows.
Expected: parity within eyeball tolerance; no validation errors.

- [ ] **Step 3: Commit**

```bash
git add crates/client/src/renderer/voxel_pull
git commit -m "feat(client): pulled chunks cast shadows (Shadow phase items)"
```

---

### Task 8: Flip the default, full verification, docs

**Files:**
- Modify: `crates/client/src/renderer/voxel_pull/mod.rs` (`DEFAULT_PULL_PATH = true`)
- Modify: `CLAUDE.md` (renderer bullet: mention the pull path + toggle)
- Modify: `docs/superpowers/specs/2026-07-09-voxel-rendering-next-steps-research.md` — no edit; instead note completion in the plan header when done.

- [ ] **Step 1: Flip** `DEFAULT_PULL_PATH` to `true`. This flips every test that relies
on the legacy mesh path via the `Option<Res<RenderPath>>` fallback — before running the
suite, pin `insert_resource(RenderPath { pull: false })` in the test scaffolds that
introspect `ATTRIBUTE_VOXEL_PACKED` / `Mesh3d` on the legacy path
(`meshing/mod.rs` tests: `async_meshing_attaches_mesh_eventually`,
`same_frame_remove_and_reinsert_keeps_mesh`, `removal_while_task_in_flight_clears_mesh`,
`meshed_chunks_carry_their_connectivity_mask`, `meshed_chunks_get_a_constant_aabb`;
`queue.rs` tests around lines 539 and 655; the `lod.rs` apply tests). Duplicate the two
lifecycle-critical ones (`removal_while_task_in_flight_clears_mesh`,
`same_frame_remove_and_reinsert_keeps_mesh`) with `pull: true` variants asserting
`ChunkQuads` instead of `Mesh3d`, so the pull path's despawn/reinsert hygiene is pinned
by tests and not only by the live smoke.

- [ ] **Step 2: Memory evidence.** Add a one-shot startup-complete log in
`apply_meshed_chunks`' records arm accumulating
`records.len() * 8` per chunk into a counter resource, logged (info level, `[pull-mem]`
prefix) every 512 chunks: bytes now vs `56 * quads` old-path estimate. This is the
plan's headline claim — capture one number from a live run for the commit message.

- [ ] **Step 3: Full suite + live + wasm.**

Run, in order, expect all green / no regressions:
- `just test-full`
- `just run` — walk, edit terrain (block break/place → remesh lands on the pull path),
  cross a region boundary, verify LOD rings render (far terrain present), shadows + AO look right.
- WASM: `cargo build -p client --target wasm32-unknown-unknown` then the
  wasm-server-runner flow from CLAUDE.md; in-browser check that terrain renders (storage
  buffers in vertex stage on real browser WebGPU) with `?offline`.

- [ ] **Step 4: Update CLAUDE.md** — in the `crates/client` workspace bullet, after the
meshing parenthetical, add: pulled-quad render path (`renderer/voxel_pull/`, 8-byte
records in storage buffers, custom Opaque3d/prepass/Shadow phase items; `RenderPath`
toggle falls back to the legacy Mesh path; records.rs↔voxel_pull.wgsl layout locked).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(client): pull path on by default — 8B/quad records replace 56B/quad meshes

[pull-mem] <measured> MB vs <estimate> MB legacy at spawn ring"
```

---

## Deferred (explicitly NOT in this plan)

- Deleting the legacy Material path (`voxel_material.rs`, `emit.rs`, `voxel_texture.wgsl`,
  packed-vertex tests) — separate decision after the pull path has soaked.
- Pooled multi-chunk record buffers / indirect draws (research candidate 2; native-only track).
- Restoring chunk-vs-chunk GPU occlusion culling for pulled chunks.
- Reusing this pipeline for far-field quad-per-voxel impostors (2026-07-10 research
  candidate 1) — designed-for, not built.
