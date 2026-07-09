# GPU Data Path (Voxel Rendering Phase 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Shrink chunk vertices from ~60 bytes to 8 bytes (packed integers unpacked in the vertex shader), and free CPU-side mesh copies (`RENDER_WORLD`-only assets).

**Architecture:** Two-stage cutover, both stages shippable. Stage A (hybrid): keep `POSITION` (Float32x3) and replace NORMAL/UV/COLOR/TEX_INDEX with ONE packed `Uint32x2` attribute — 60→20 B/vertex, and shadows/prepass/Aabb keep working untouched because the default prepass vertex shader and auto-Aabb only need `POSITION`. Stage B (pure): drop `POSITION` for the spec's 8 B/vertex — requires a custom prepass vertex shader and a manually inserted constant chunk `Aabb`. The material stays `ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>` (base `specialize` runs first, the extension then overwrites `descriptor.vertex.buffers` — verified order in bevy_pbr 0.18.1 `extended_material.rs:410-424`).

**Tech Stack:** Rust, Bevy 0.18.1 (client crate only), WGSL (naga_oil `#import` shaders).

**Spec:** `docs/superpowers/specs/2026-07-08-voxel-rendering-optimization-design.md` §6.

## Global Constraints

- All changes live in `crates/client` + `assets/shaders/voxel_texture.wgsl`. `game`/`server`/vendored forks untouched.
- Nothing enters rollback-hashed state.
- The packed bit layout is LOCKED (Rust and WGSL must mirror it exactly):
  - word0: bits 0-5 `x`, 6-11 `y`, 12-17 `z` (vertex corner position, 0..=32), 18-20 `face` (0..=5, `Face::index()` order PosX,NegX,PosY,NegY,PosZ,NegZ), 21-22 `ao` (0..=3), 23 `cu`, 24 `cv`; bits 25-31 zero.
  - word1: bits 0-5 `w`, 6-11 `h` (quad extents 1..=32), 12-27 `layer` (texture array layer, 16 bits); bits 28-31 zero.
- Shader AO shade formula `0.55 + 0.15 * ao` MUST equal `ao::AO_CURVE` (`[0.55, 0.70, 0.85, 1.0]`) — pinned by test.
- Per-quad texture layer collapses the `[top, side, bottom]` triple CPU-side by face: `PosY → [0]`, `NegY → [2]`, else `[1]` (same semantics as the current shader's normal-based pick).
- Existing behavior contracts survive: winding (cross·normal test), quad geometry spans, one shared material asset for all chunks (single batch bin), `VoxelData` liveness rule, task supersede rule.
- Existing test suites must stay green at every task boundary (`cargo test -p client`); visual correctness is verified once at the end (controller-run smoke).
- Tag any test measured >100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- SPEC DEVIATION (approved rationale): spec §6.4 asks for a naga WGSL validation unit test. Plain naga 27 cannot parse naga_oil `#import` directives, so that test is replaced by (a) the Rust pack/unpack mirror + AO-curve pin tests and (b) the live smoke (pipeline creation fails loudly on WGSL errors). Record this deviation in the plan-completion notes.

**Verified API facts (bevy 0.18.1, from local sources — do not re-derive):**
- `MaterialExtension` may override both `vertex_shader()` and `specialize()`; base runs first.
- `MeshVertexBufferLayoutRef.0.get_layout(&[attr.at_shader_location(n)]) -> Result<VertexBufferLayout, MissingVertexAttributeError>` converts via `?`.
- Meshes without `POSITION` panic nowhere (layout/allocator/batching all treat it as optional); consequences are only no-auto-`Aabb` and prepass vertex incompatibility.
- Auto-Aabb: `calculate_bounds` requires POSITION; manual `Aabb` needs the `NoAutoAabb` marker or the `Changed<Mesh3d>` requery wipes it (`bevy_camera::visibility`).
- `Material::specialize` IS called for the prepass pipeline (`prepass/mod.rs:357`); default prepass vertex reads `@location(0) position` gated on `VERTEX_POSITIONS`.
- `RenderAssetUsages::RENDER_WORLD`-only meshes free CPU data after extraction; GPU memory IS reclaimed on handle drop in 0.18 (`MeshAllocator::free_meshes` frees allocations and removes empty slabs — issue #13500 resolved).
- Headless `MinimalPlugins` tests never extract, so test-side `mesh.attribute(...)` reads keep working even with `RENDER_WORLD`-only meshes.

**Preflight:** verify `git status` clean and HEAD ≥ `9f5007c` on the worktree branch; `cargo test -p client 2>&1 | tail -3` green before Task 1.

---

### Task 1: Packing module (`pack.rs`) + `Face` indexing

**Files:**
- Create: `crates/client/src/renderer/meshing/pack.rs`
- Modify: `crates/client/src/renderer/meshing/binary.rs` (add `Face::index`/`Face::from_index`)
- Modify: `crates/client/src/renderer/meshing/mod.rs` (add `pub mod pack;`)
- Test: unit tests inside `pack.rs`

**Interfaces:**
- Consumes: `Face` (Task adds `pub fn index(self) -> u32` returning PosX=0, NegX=1, PosY=2, NegY=3, PosZ=4, NegZ=5, and `pub fn from_index(i: u32) -> Face`).
- Produces (used by Tasks 2, 4):
  - `pub struct UnpackedVertex { pub pos: [u32; 3], pub face: Face, pub ao: u8, pub cu: u32, pub cv: u32, pub w: u32, pub h: u32, pub layer: u32 }`
  - `pub fn pack(pos: [u32; 3], face: Face, ao: u8, cu: u32, cv: u32, w: u32, h: u32, layer: u32) -> [u32; 2]`
  - `pub fn unpack(p: [u32; 2]) -> UnpackedVertex`
  - `pub fn layer_for_face(faces: [u32; 3], face: Face) -> u32`
  - `pub fn shade(ao: u8) -> f32` — the WGSL formula `0.55 + 0.15 * ao`, mirrored in Rust for tests.

- [ ] **Step 1: Add `Face::index`/`from_index` with a failing test**

In `binary.rs`, append to the test module:

```rust
    #[test]
    fn face_index_roundtrips_in_locked_order() {
        let order = [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ, Face::NegZ];
        for (i, f) in order.into_iter().enumerate() {
            assert_eq!(f.index(), i as u32);
            assert_eq!(Face::from_index(i as u32), f);
        }
    }
```

Run: `cargo test -p client --lib binary::tests::face_index 2>&1 | tail -3` — Expected: COMPILE ERROR.

Implement in `binary.rs` inside `impl Face` (the order is LOCKED — the WGSL normal table indexes by it):

```rust
    /// Packed-format face id (see meshing/pack.rs and voxel_texture.wgsl —
    /// the WGSL FACE_NORMALS table is indexed by this; order is locked).
    pub fn index(self) -> u32 {
        match self {
            Face::PosX => 0,
            Face::NegX => 1,
            Face::PosY => 2,
            Face::NegY => 3,
            Face::PosZ => 4,
            Face::NegZ => 5,
        }
    }

    pub fn from_index(i: u32) -> Self {
        match i {
            0 => Face::PosX,
            1 => Face::NegX,
            2 => Face::PosY,
            3 => Face::NegY,
            4 => Face::PosZ,
            5 => Face::NegZ,
            _ => unreachable!("face index is 0..6"),
        }
    }
```

Run: `cargo test -p client --lib binary::tests::face_index 2>&1 | tail -3` — Expected: PASS.

- [ ] **Step 2: Write failing pack.rs tests**

Create `crates/client/src/renderer/meshing/pack.rs`:

```rust
//! The packed voxel vertex format: two u32s per vertex, mirrored bit-for-bit
//! by the decode in assets/shaders/voxel_texture.wgsl. Layout (LOCKED):
//!   word0: x:6 | y:6 | z:6 | face:3 | ao:2 | cu:1 | cv:1   (bits 25-31 zero)
//!   word1: w:6 | h:6 | layer:16                            (bits 28-31 zero)
//! Rust packs (emit.rs) and unpacks (tests only); the shader only unpacks.

use super::binary::Face;

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ao::AO_CURVE;

    #[test]
    fn roundtrips_across_the_full_domain() {
        // Exhaustive on the small fields, boundary values on the big ones.
        for face_i in 0..6u32 {
            let face = Face::from_index(face_i);
            for ao in 0..4u8 {
                for (cu, cv) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
                    for pos in [[0u32, 0, 0], [32, 32, 32], [1, 17, 32]] {
                        for (w, h) in [(1u32, 1u32), (32, 32), (5, 17)] {
                            for layer in [0u32, 1, 255, 65535] {
                                let p = pack(pos, face, ao, cu, cv, w, h, layer);
                                let u = unpack(p);
                                assert_eq!(
                                    (u.pos, u.face, u.ao, u.cu, u.cv, u.w, u.h, u.layer),
                                    (pos, face, ao, cu, cv, w, h, layer)
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn reserved_bits_stay_zero() {
        let p = pack([32, 32, 32], Face::NegZ, 3, 1, 1, 32, 32, 65535);
        assert_eq!(p[0] >> 25, 0, "word0 bits 25-31 reserved");
        assert_eq!(p[1] >> 28, 0, "word1 bits 28-31 reserved");
    }

    #[test]
    fn shade_formula_matches_ao_curve() {
        // The WGSL computes 0.55 + 0.15*ao; it must equal ao::AO_CURVE exactly
        // or baked AO silently diverges between CPU and shader.
        for ao in 0..4usize {
            assert_eq!(shade(ao as u8), AO_CURVE[ao]);
        }
    }

    #[test]
    fn layer_collapse_matches_the_normal_pick_semantics() {
        // Shader used to pick: top (index 0) when normal.y > 0, side (1) when
        // y == 0, bottom (2) when y < 0.
        let faces = [10, 20, 30];
        assert_eq!(layer_for_face(faces, Face::PosY), 10);
        assert_eq!(layer_for_face(faces, Face::NegY), 30);
        for f in [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ] {
            assert_eq!(layer_for_face(faces, f), 20);
        }
    }
}
```

Run: `cargo test -p client --lib pack:: 2>&1 | tail -3` (after adding `pub mod pack;` to `meshing/mod.rs`) — Expected: COMPILE ERROR.

- [ ] **Step 3: Implement**

Add above the test module in `pack.rs`:

```rust
/// Decoded form of a packed vertex — the Rust mirror of the WGSL decode,
/// used by tests to assert on mesh content without float attributes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnpackedVertex {
    pub pos: [u32; 3],
    pub face: Face,
    pub ao: u8,
    pub cu: u32,
    pub cv: u32,
    pub w: u32,
    pub h: u32,
    pub layer: u32,
}

pub fn pack(
    pos: [u32; 3],
    face: Face,
    ao: u8,
    cu: u32,
    cv: u32,
    w: u32,
    h: u32,
    layer: u32,
) -> [u32; 2] {
    debug_assert!(pos.iter().all(|&c| c <= 32) && w <= 32 && h <= 32);
    debug_assert!(ao <= 3 && cu <= 1 && cv <= 1 && layer <= 0xFFFF);
    let w0 = pos[0]
        | pos[1] << 6
        | pos[2] << 12
        | face.index() << 18
        | (ao as u32) << 21
        | cu << 23
        | cv << 24;
    let w1 = w | h << 6 | layer << 12;
    [w0, w1]
}

pub fn unpack(p: [u32; 2]) -> UnpackedVertex {
    let [w0, w1] = p;
    UnpackedVertex {
        pos: [w0 & 63, (w0 >> 6) & 63, (w0 >> 12) & 63],
        face: Face::from_index((w0 >> 18) & 7),
        ao: ((w0 >> 21) & 3) as u8,
        cu: (w0 >> 23) & 1,
        cv: (w0 >> 24) & 1,
        w: w1 & 63,
        h: (w1 >> 6) & 63,
        layer: (w1 >> 12) & 0xFFFF,
    }
}

/// Collapse the per-block `[top, side, bottom]` layer triple to the single
/// layer this quad's face shows (same semantics as the old shader's
/// normal-based pick).
pub fn layer_for_face(faces: [u32; 3], face: Face) -> u32 {
    match face {
        Face::PosY => faces[0],
        Face::NegY => faces[2],
        _ => faces[1],
    }
}

/// The AO shade the vertex shader computes: `0.55 + 0.15 * ao`. Must equal
/// `ao::AO_CURVE[ao]` (test-pinned) — the WGSL hardcodes the same formula.
pub fn shade(ao: u8) -> f32 {
    0.55 + 0.15 * ao as f32
}
```

Run: `cargo test -p client --lib pack:: 2>&1 | tail -3` — Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/client/src/renderer/meshing
git commit -m "feat(client): packed voxel vertex format (2xu32) with Rust mirror + AO-curve pin"
```

---

### Task 2: Hybrid cutover — emit packed attribute, rewrite material + WGSL

One atomic task: after it, meshes carry `POSITION` + `ATTRIBUTE_VOXEL_PACKED` only, and the shader consumes them. Splitting emit from shader would leave the renderer broken mid-branch.

**Files:**
- Modify: `crates/client/src/renderer/meshing/emit.rs` (attribute swap + test rewrite)
- Modify: `crates/client/src/renderer/voxel_material.rs` (attribute const, `vertex_layout()`)
- Modify: `assets/shaders/voxel_texture.wgsl` (full replacement below)
- Test: updated tests in `emit.rs`; existing suites green

**Interfaces:**
- Consumes: `pack`, `layer_for_face` (Task 1); `Quad`/`Face`/`cross_axes`, `ao_flips_quad` (existing).
- Produces (used by Tasks 3-4):
  - `voxel_material::ATTRIBUTE_VOXEL_PACKED: MeshVertexAttribute` — `MeshVertexAttribute::new("VoxelPacked", 988540917, VertexFormat::Uint32x2)`, shader location 8.
  - `quads_to_mesh` emits exactly two attributes: `POSITION` (Float32x3, loc 0) and `ATTRIBUTE_VOXEL_PACKED` (Uint32x2, loc 8). NORMAL/UV_0/COLOR/TEX_INDEX are GONE; `ATTRIBUTE_TEX_INDEX` const deleted.
  - Bytes/vertex after this task: 20 (12 + 8), asserted by test.

- [ ] **Step 1: Rewrite emit.rs tests (failing first)**

Replace the bodies of the format-coupled tests in `emit.rs`'s test module (`ao_lands_in_vertex_color_via_curve`, `uvs_tile_by_quad_extent`) and add the new ones. `winding_faces_outward_for_all_six_faces_default_and_flipped` and `quad_geometry_spans_w_by_h_at_the_face_plane` stay untouched (they read POSITION, which survives this task). New/replaced tests:

```rust
    fn packed_of(mesh: &Mesh) -> Vec<crate::renderer::meshing::pack::UnpackedVertex> {
        let VertexAttributeValues::Uint32x2(packed) =
            mesh.attribute(voxel_material::ATTRIBUTE_VOXEL_PACKED).unwrap()
        else {
            panic!("packed attribute is Uint32x2")
        };
        packed.iter().map(|&p| crate::renderer::meshing::pack::unpack(p)).collect()
    }

    #[test]
    fn packed_carries_ao_face_and_extent() {
        let mesh = quads_to_mesh(&[unit_quad(Face::PosY, [0, 1, 2, 3])], &BlockTextureLayers::default())
            .unwrap();
        let verts = packed_of(&mesh);
        assert_eq!(verts.len(), 4);
        for (i, v) in verts.iter().enumerate() {
            assert_eq!(v.ao as usize, i, "corner i carries ao level i for this quad");
            assert_eq!(v.face, Face::PosY);
            assert_eq!((v.w, v.h), (2, 3));
        }
        // Corner order (cu, cv) = (0,0),(1,0),(0,1),(1,1).
        assert_eq!(
            verts.iter().map(|v| (v.cu, v.cv)).collect::<Vec<_>>(),
            vec![(0, 0), (1, 0), (0, 1), (1, 1)]
        );
    }

    #[test]
    fn packed_position_matches_float_position() {
        // The packed x/y/z must agree with the POSITION attribute — this is
        // what lets Task 4 drop POSITION entirely.
        let mesh = quads_to_mesh(&[unit_quad(Face::PosX, [3; 4])], &BlockTextureLayers::default())
            .unwrap();
        let VertexAttributeValues::Float32x3(pos) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!()
        };
        for (p, v) in pos.iter().zip(packed_of(&mesh)) {
            assert_eq!([p[0] as u32, p[1] as u32, p[2] as u32], v.pos);
        }
    }

    #[test]
    fn packed_layer_uses_face_collapsed_triple() {
        let mut layers = BlockTextureLayers::default();
        layers.0.insert(BlockId(1), [10, 20, 30]);
        for (face, expect) in [(Face::PosY, 10u32), (Face::NegX, 20), (Face::NegY, 30)] {
            let mesh = quads_to_mesh(&[unit_quad(face, [3; 4])], &layers).unwrap();
            assert!(packed_of(&mesh).iter().all(|v| v.layer == expect));
        }
    }

    #[test]
    fn hybrid_format_is_20_bytes_per_vertex() {
        let mesh = quads_to_mesh(&[unit_quad(Face::PosY, [3; 4])], &BlockTextureLayers::default())
            .unwrap();
        let total: usize = mesh
            .attributes()
            .map(|(_, values)| values.get_bytes().len())
            .sum();
        assert_eq!(total / mesh.count_vertices(), 20, "12B position + 8B packed");
        assert_eq!(mesh.attributes().count(), 2, "exactly POSITION + VOXEL_PACKED");
    }
```

Delete `ao_lands_in_vertex_color_via_curve` and `uvs_tile_by_quad_extent` (their guarantees now live in `packed_carries_ao_face_and_extent` + Task 1's `shade_formula_matches_ao_curve`). If `values.get_bytes()` doesn't exist on this Bevy version, use `values.len() * values.size()` per the `VertexAttributeValues` API — check compiler suggestion, keep the 20-byte assertion intact.

Run: `cargo test -p client --lib emit:: 2>&1 | tail -3` — Expected: COMPILE ERROR / failures.

- [ ] **Step 2: Rewrite `quads_to_mesh`**

Replace the vertex-building section of `quads_to_mesh` (keep `quad_indices` and the function signature):

```rust
pub fn quads_to_mesh(quads: &[Quad], layers: &BlockTextureLayers) -> Option<Mesh> {
    if quads.is_empty() {
        return None;
    }
    let n = quads.len() * 4;
    let mut positions = Vec::with_capacity(n);
    let mut packed: Vec<[u32; 2]> = Vec::with_capacity(n);
    let mut indices = Vec::with_capacity(quads.len() * 6);

    for q in quads {
        let axis = q.face.axis();
        let (ua, va) = cross_axes(axis);
        let base = positions.len() as u32;
        indices.extend(quad_indices(q.face, ao_flips_quad(&q.ao)).map(|i| base + i));

        let faces = layers.0.get(&q.block).copied().unwrap_or([0, 0, 0]);
        let layer = pack::layer_for_face(faces, q.face);

        // The face plane sits at min[axis] for negative faces, +1 for positive.
        let mut origin = [q.min[0], q.min[1], q.min[2]];
        if q.face.positive() {
            origin[axis] += 1;
        }
        for (i, (cu, cv)) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
            let mut pos = origin;
            pos[ua] += cu * q.w;
            pos[va] += cv * q.h;
            positions.push([pos[0] as f32, pos[1] as f32, pos[2] as f32]);
            packed.push(pack::pack(pos, q.face, q.ao[i], cu, cv, q.w, q.h, layer));
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, VertexAttributeValues::Float32x3(positions));
    mesh.insert_attribute(
        voxel_material::ATTRIBUTE_VOXEL_PACKED,
        VertexAttributeValues::Uint32x2(packed),
    );
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}
```

Add `use super::pack;` and update the module doc comment (the format is now POSITION + packed; Stage B drops POSITION). `RenderAssetUsages::default()` stays for now (Task 3 changes it).

- [ ] **Step 3: Update `voxel_material.rs`**

Replace `ATTRIBUTE_TEX_INDEX` and `vertex_layout()`:

```rust
/// The packed voxel vertex word pair — layout documented in
/// crates/client/src/renderer/meshing/pack.rs and mirrored by the decode in
/// assets/shaders/voxel_texture.wgsl.
pub const ATTRIBUTE_VOXEL_PACKED: MeshVertexAttribute =
    MeshVertexAttribute::new("VoxelPacked", 988540917, VertexFormat::Uint32x2);

pub fn vertex_layout() -> Vec<VertexAttributeDescriptor> {
    vec![
        Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
        ATTRIBUTE_VOXEL_PACKED.at_shader_location(8),
    ]
}
```

The `MaterialExtension` impl (shaders, prepass short-circuit in `specialize`) stays EXACTLY as-is this task — the prepass still runs bevy's default vertex path off `POSITION`.

- [ ] **Step 4: Replace `assets/shaders/voxel_texture.wgsl`**

Full new content:

```wgsl
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    mesh_functions,
    view_transformations::position_world_to_clip
}
#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}
#endif

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var mat_array_texture: texture_2d_array<f32>;

@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var mat_array_texture_sampler: sampler;

// Packed voxel vertex — bit layout mirrored by
// crates/client/src/renderer/meshing/pack.rs (the Rust source of truth):
//   w0: x:6 | y:6 | z:6 | face:3 | ao:2 | cu:1 | cv:1
//   w1: w:6 | h:6 | layer:16
struct Vertex {
    @builtin(instance_index) instance_index: u32,
#ifdef VERTEX_POSITIONS
    @location(0) position: vec3<f32>,
#endif
    @location(8) packed: vec2<u32>,
};

struct CustomVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shade: f32,
    @location(4) @interpolate(flat) layer: u32,
    @location(6) @interpolate(flat) instance_index: u32,
}

// Indexed by the packed face id — order locked to Face::index() in binary.rs.
const FACE_NORMALS = array<vec3<f32>, 6>(
    vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(-1.0, 0.0, 0.0),
    vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(0.0, -1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0),
    vec3<f32>(0.0, 0.0, -1.0),
);

@vertex
fn vertex(vertex: Vertex) -> CustomVertexOutput {
    var out: CustomVertexOutput;
    let w0 = vertex.packed.x;
    let w1 = vertex.packed.y;
    let face = (w0 >> 18u) & 7u;
    let ao = (w0 >> 21u) & 3u;
    let cu = (w0 >> 23u) & 1u;
    let cv = (w0 >> 24u) & 1u;
    let quad_w = w1 & 63u;
    let quad_h = (w1 >> 6u) & 63u;

    var model = mesh_functions::get_world_from_local(vertex.instance_index);
#ifdef VERTEX_POSITIONS
    let local_pos = vertex.position;
#else
    // Stage B (no POSITION attribute): decode the corner from the packed word.
    let local_pos = vec3<f32>(
        f32(w0 & 63u),
        f32((w0 >> 6u) & 63u),
        f32((w0 >> 12u) & 63u),
    );
#endif
    out.world_position = mesh_functions::mesh_position_local_to_world(
        model, vec4<f32>(local_pos, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        FACE_NORMALS[face], vertex.instance_index);
    // Quad-extent UV tiling (repeat sampler wraps it per voxel cell).
    out.uv = vec2<f32>(f32(cu * quad_w), f32(cv * quad_h));
    // Baked AO — must match ao::AO_CURVE (0.55 + 0.15 * ao); test-pinned in pack.rs.
    out.shade = 0.55 + 0.15 * f32(ao);
    out.layer = (w1 >> 12u) & 65535u;
    out.instance_index = vertex.instance_index;
    return out;
}

@fragment
fn fragment(
    in: CustomVertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var standard_in: VertexOutput;
    standard_in.position = in.position;
    standard_in.world_normal = in.world_normal;
    standard_in.world_position = in.world_position;
    standard_in.instance_index = in.instance_index;
    var pbr_input = pbr_input_from_standard_material(standard_in, is_front);

    let tex = textureSample(mat_array_texture, mat_array_texture_sampler, in.uv, in.layer);
    pbr_input.material.base_color = tex * vec4<f32>(in.shade, in.shade, in.shade, 1.0);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif

    return out;
}
```

Compile-hazard notes for the implementer (fix per compiler/naga error, keep semantics):
- `standard_in.instance_index` exists only under `VERTEX_OUTPUT_INSTANCE_INDEX`; the OLD shader assigned it unconditionally and compiled, so the def is effectively always on — if naga_oil complains, wrap the assignment in `#ifdef VERTEX_OUTPUT_INSTANCE_INDEX`.
- The `PREPASS_PIPELINE` fragment branch is inherited dead code (the extension doesn't override `prepass_fragment_shader`, and deferred isn't used) — keep it only if it compiles as it did before; if `deferred_output(in, ...)` rejects the custom struct, delete the branch and the prepass imports (they were unreachable).
- Do NOT read `in.uv`/`in.color` on `standard_in` — the mesh no longer sets `VERTEX_UVS`/`VERTEX_COLORS`, so those `VertexOutput` fields don't exist.

- [ ] **Step 5: Run the full client suite**

Run: `cargo test -p client 2>&1 | tail -8` and `cargo test -p client -- --include-ignored 2>&1 | tail -5`
Expected: all green. The queue.rs wall-regrowth tests and mod.rs span tests read POSITION+NORMAL... NORMAL IS GONE — `neighbor_culling`/`despawning_a_chunk_redirties_its_neighbor` (queue.rs) read `Mesh::ATTRIBUTE_NORMAL`. Update those two tests: derive the normal check from the packed attribute instead — a vertex belongs to the shared plane iff `unpack(packed).face == Face::PosX` (for chunk a) / `Face::NegX` (for b) and its float POSITION x equals the plane. Keep the assertion strength identical (plane + facing), just source `face` from `pack::unpack` instead of the deleted NORMAL attribute.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer assets/shaders/voxel_texture.wgsl
git commit -m "feat(client): hybrid packed vertex format — 60 to 20 bytes/vertex, shader-side unpack"
```

---

### Task 3: `RENDER_WORLD`-only meshes + constant chunk Aabb

**Files:**
- Modify: `crates/client/src/renderer/meshing/emit.rs` (RenderAssetUsages)
- Modify: `crates/client/src/renderer/meshing/mod.rs` (`apply_meshed_chunks` inserts Aabb)
- Test: headless test in `meshing/mod.rs`

**Interfaces:**
- Consumes: nothing new. Produces: chunk entities carry a constant `Aabb` (center 16³, half-extents 16³ — chunks always occupy within the 0..32 cube) + `NoAutoAabb`, inserted alongside `Mesh3d`. This pre-satisfies Task 4 (no POSITION → no auto-Aabb) and removes any main-world dependence on mesh data.

- [ ] **Step 1: Failing test**

Append to `meshing/mod.rs` tests (reuse the existing headless app pattern from `async_meshing_attaches_mesh_eventually`):

```rust
    #[test]
    fn meshed_chunks_get_a_constant_aabb() {
        use bevy::camera::primitives::Aabb;
        use bevy::camera::visibility::NoAutoAabb;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
        settle_until_meshed(&mut app, e);
        let aabb = app.world().entity(e).get::<Aabb>().expect("constant chunk Aabb inserted");
        assert_eq!(aabb.center, bevy::math::Vec3A::splat(16.0));
        assert_eq!(aabb.half_extents, bevy::math::Vec3A::splat(16.0));
        assert!(app.world().entity(e).contains::<NoAutoAabb>(), "auto-Aabb must not overwrite it");
    }
```

If `bevy::camera::visibility::NoAutoAabb`/`bevy::camera::primitives::Aabb` paths don't resolve, the types live in `bevy_camera` re-exported under `bevy::render::primitives`/`bevy::render::view::visibility` in the facade — follow the compiler, the types are `bevy_camera::primitives::Aabb` and `bevy_camera::visibility::NoAutoAabb` (verified present in bevy_camera-0.18.1).

Run: `cargo test -p client --lib meshed_chunks_get_a_constant_aabb 2>&1 | tail -3` — Expected: FAIL (no Aabb).

- [ ] **Step 2: Implement**

In `emit.rs`, change `Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())` to `RenderAssetUsages::RENDER_WORLD` with a comment: CPU copy freed after extraction; colliders come from rapier, tests never extract (MinimalPlugins), and the entity Aabb is inserted constant by `apply_meshed_chunks` so nothing reads mesh data main-world after upload.

In `apply_meshed_chunks` (`meshing/mod.rs`), in the `Some(mesh)` arm, extend the insert:

```rust
                let mut ec = commands.entity(e);
                ec.insert((
                    Mesh3d(meshes.add(mesh)),
                    // Chunks always occupy within their 0..32 local cube; a
                    // constant conservative Aabb keeps frustum culling working
                    // without POSITION-derived bounds (required once Stage B
                    // drops the POSITION attribute) and independent of mesh
                    // data lifetime (RENDER_WORLD-only assets).
                    bevy::camera::primitives::Aabb {
                        center: bevy::math::Vec3A::splat(16.0),
                        half_extents: bevy::math::Vec3A::splat(16.0),
                    },
                    bevy::camera::visibility::NoAutoAabb,
                ));
```
(The `None`/removal arms stay untouched — a stripped mesh entity keeping a stale Aabb is harmless: no `Mesh3d`, nothing renders.)

Run: `cargo test -p client 2>&1 | tail -5` — Expected: all green.

- [ ] **Step 3: Commit**

```bash
git add crates/client/src/renderer/meshing
git commit -m "perf(client): RENDER_WORLD-only chunk meshes + constant Aabb (NoAutoAabb)"
```

---

### Task 4: Stage B — drop POSITION (8 bytes/vertex) + custom prepass vertex

**DESCOPE VALVE:** if the prepass/shadow path fights back beyond ~2 focused attempts (shadow acne, naga_oil import errors in prepass context, motion-vector field mismatches), STOP and report DONE_WITH_CONCERNS: Stage A (20 B/vertex, working shadows) is shippable, and this task is independently droppable with user sign-off. Do not ship broken shadows.

**Files:**
- Modify: `crates/client/src/renderer/meshing/emit.rs` (drop POSITION)
- Modify: `crates/client/src/renderer/voxel_material.rs` (layouts + prepass hooks)
- Modify: `assets/shaders/voxel_texture.wgsl` (prepass vertex entry)
- Test: emit tests updated; queue/mod tests' POSITION reads replaced by packed decode

**Interfaces:** after this task, chunk meshes carry exactly ONE attribute (`ATTRIBUTE_VOXEL_PACKED`), 8 bytes/vertex, asserted.

- [ ] **Step 1: Failing test — flip the bytes-per-vertex assertion**

In `emit.rs`, change `hybrid_format_is_20_bytes_per_vertex` to:

```rust
    #[test]
    fn packed_format_is_8_bytes_per_vertex() {
        let mesh = quads_to_mesh(&[unit_quad(Face::PosY, [3; 4])], &BlockTextureLayers::default())
            .unwrap();
        let total: usize = mesh
            .attributes()
            .map(|(_, values)| values.get_bytes().len())
            .sum();
        assert_eq!(total / mesh.count_vertices(), 8, "one Uint32x2 per vertex");
        assert_eq!(mesh.attributes().count(), 1, "exactly VOXEL_PACKED");
    }
```

Run: `cargo test -p client --lib emit:: 2>&1 | tail -3` — Expected: FAIL (still 20).

- [ ] **Step 2: Drop POSITION in emit.rs**

Remove the `positions` vec and the `ATTRIBUTE_POSITION` insert (the packed word already carries the corner — `packed_position_matches_float_position` proved equality; delete that test now, and `winding_faces_outward...`/`quad_geometry_spans...` + the two queue.rs tests switch from POSITION reads to `pack::unpack(...).pos` — same assertions, decoded source. `tri_normals_positive` rebuilds its positions as `Vec3::new(v.pos[0] as f32, ...)` from unpacked vertices.)

- [ ] **Step 3: Prepass support in voxel_material.rs**

```rust
    fn prepass_vertex_shader() -> ShaderRef {
        "shaders/voxel_texture.wgsl".into()
    }
```

And `specialize` loses the short-circuit — the packed layout now applies to BOTH pipelines:

```rust
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let vertex_layout =
            layout.0.get_layout(&[ATTRIBUTE_VOXEL_PACKED.at_shader_location(8)])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
    }
```
(`vertex_layout()` shrinks to the single packed entry or is inlined as above; remove the now-unused `Mesh::ATTRIBUTE_POSITION` import if dangling.)

- [ ] **Step 4: Prepass vertex entry in the WGSL**

The prepass pipeline compiles this file with `PREPASS_PIPELINE` defined and uses the SAME `@vertex fn vertex` entry name. Restructure the vertex stage so that under `PREPASS_PIPELINE` it returns `prepass_io::VertexOutput` populated minimally:

```wgsl
#ifdef PREPASS_PIPELINE
@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let w0 = vertex.packed.x;
    let local_pos = vec3<f32>(
        f32(w0 & 63u),
        f32((w0 >> 6u) & 63u),
        f32((w0 >> 12u) & 63u),
    );
    var model = mesh_functions::get_world_from_local(vertex.instance_index);
    let world_position = mesh_functions::mesh_position_local_to_world(model, vec4<f32>(local_pos, 1.0));
    out.position = position_world_to_clip(world_position.xyz);
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
    return out;
}
#else
@vertex
fn vertex(vertex: Vertex) -> CustomVertexOutput {
    // ... existing forward vertex body from Task 2, minus the #ifdef VERTEX_POSITIONS
    //     branch (decode local_pos from the packed word unconditionally) ...
}
#endif
```

Implementer notes: read `bevy_pbr-0.18.1`'s `prepass_io.wgsl` for `VertexOutput`'s exact fields and which are `#ifdef`-gated (`world_position` exists under some prepass modes; motion vectors need `previous_world_position` only under `MOTION_VECTOR_PREPASS`, which this app does not enable). Populate gated fields under the same `#ifdef`s the struct declares them with. The old `Vertex` struct's `#ifdef VERTEX_POSITIONS @location(0) position` field can be deleted entirely (no mesh has POSITION anymore).

- [ ] **Step 5: Full suite + shadow check**

Run: `cargo test -p client 2>&1 | tail -8` and `cargo test -p client -- --include-ignored 2>&1 | tail -5` — Expected: all green.
Then a 30-second native run (`cargo run --bin server` + `WGPU_ADAPTER_NAME=6700 cargo run --bin client`, kill after): grep client logs for wgpu/naga pipeline errors — a broken prepass shader fails loudly at pipeline creation. Terrain must still cast/receive directional shadows (visible in the window; if you cannot judge visually, at minimum assert zero pipeline errors in the log and flag the visual check to the controller).

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer assets/shaders/voxel_texture.wgsl
git commit -m "perf(client): drop POSITION — 8-byte packed vertices with custom prepass vertex"
```

---

### Task 5: Gates, docs, deviation note

**Files:**
- Modify: `CLAUDE.md` (renderer file list: add `pack.rs` mention inside the meshing/ parenthetical)
- Test: full-suite gates

- [ ] **Step 1: CLAUDE.md** — in the `crates/client` bullet, change `meshing/ (binary greedy mesher, AO, neighbor gather, remesh queue)` to `meshing/ (binary greedy mesher, AO, packed vertex format, neighbor gather, remesh queue)`.

- [ ] **Step 2: Full gates**

```bash
just test 2>&1 | tail -3
just test-full 2>&1 | tail -3
cargo test -p server 2>&1 | tail -3
cargo test -p client --release --test mesh_parity -- --include-ignored 2>&1 | tail -4   # perf gate must still pass — emission changed
```
Expected: all green. Note the parity tests operate on `mesh_quads` output (pre-emission) so they are format-agnostic; the perf gate re-run guards against emission-cost regressions leaking into the measured path (it doesn't measure emission, but re-running is cheap insurance the suite still passes in release).

- [ ] **Step 3: Deviation note + commit**

Append to the plan-completion notes in the commit body: "Spec §6.4 naga-validation test replaced by Rust pack/unpack mirror + AO-curve pin + live pipeline check — plain naga 27 cannot parse naga_oil #import shaders."

```bash
git add CLAUDE.md
git commit -m "docs: phase 2 packed-vertex format in CLAUDE.md; note naga-test deviation"
```

---

## Self-Review Notes (already applied)

- Spec §6.1 (packed format, two u32) → Tasks 1, 2, 4; §6.2 (custom material keeping bevy_pbr lighting) → Task 2 (ExtendedMaterial route, justified by verified specialize ordering); §6.3 (RENDER_WORLD + Aabb safety) → Task 3; §6.4 tests → Tasks 1-4 + documented naga deviation; success criterion §4.2 (≤8 B/vertex) → Task 4 with an explicit descope valve (20 B fallback needs user sign-off to become final).
- Type consistency: `pack`/`unpack`/`UnpackedVertex`/`layer_for_face`/`shade` (T1) consumed by T2/T4 under those names; `ATTRIBUTE_VOXEL_PACKED` (T2) consumed by T4; `Face::index/from_index` locked to the WGSL FACE_NORMALS order.
- Tests that read deleted attributes are each explicitly redirected (emit COLOR/UV → packed asserts in T2; NORMAL readers in queue.rs → packed face decode in T2; POSITION readers → unpacked positions in T4). No assertion is weakened, only re-sourced.
- Known API-risk flags left in-plan deliberately: `NoAutoAabb`/`Aabb` facade path (T3), `VERTEX_OUTPUT_INSTANCE_INDEX` gating (T2), prepass_io field gates (T4), `VertexAttributeValues::get_bytes` availability (T2).
