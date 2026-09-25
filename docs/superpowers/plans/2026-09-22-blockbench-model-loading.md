# Blockbench Model Loading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Load Blockbench `.bbmodel` files natively and show them in the client: static models with textures, bone hierarchy, and keyframe animations. The slime becomes the first entity rendered from a model file.

**Architecture:**
- A new Bevy-free crate, `crates/bbmodel`, owns the file format:
  - parsing format 3.x/4.x/5.x into a normalized `Model`;
  - turning cubes into triangle `MeshPart`s, the same way Blockbench's own preview does;
  - sampling animations.
- The client's `renderer/model.rs` uploads a `Model` once as shared Bevy assets (`LoadedModel`), spawns a bone-entity hierarchy per instance, and drives bones from `ModelAnimator`.
- The slime's hand-built 5-cube body in `renderer/avatar.rs` is replaced by `assets/models/slime.bbmodel`, spawned under the existing squish pivot.

**Tech Stack:** Rust 2024 (new crate), `serde_json` (parsed as `Value` for tolerance), `base64`, `image` 0.25 (PNG), Bevy (whatever the root `Cargo.toml` pins — see Global Constraints).

**Spec:** `docs/superpowers/specs/2026-09-22-blockbench-model-loading-investigation.md`. Read it first, especially "Option B" and "Test corpus: complex mobs, and what they will break".

**Decisions taken for the spec's open questions.** These are defaults; the user may overturn them at plan review.
1. Use a native `.bbmodel` loader, not glTF.
2. The parser lives in its **own crate** `crates/bbmodel`. It does not go in `game`: `game` stays free of `serde_json`, and the parser still stays Bevy-free, so `game` can depend on it later for model→`ChiselData` import.
3. Assets are `include_bytes!`-embedded (`EMBEDDED_MODELS`), matching the block textures, so wasm needs no asset fetch.
4. Animation v1 supports linear, step, Catmull-Rom and pre/post keys. Bezier plays as linear with a warning. Molang expressions evaluate to 0 with a warning. Effect tracks are ignored. Mesh elements are skipped with a warning.

## Global Constraints

- `game` and `server` must stay Bevy-free and windowing-free. `crates/bbmodel` is **also** Bevy-free: no `bevy` dependency, ever.
- 1 Blockbench pixel = 1 world unit = 1 voxel (`BLOCK_VOXELS` = 16 per block = 1 m). The loader applies **no scaling**. Callers scale if they want a different size (the slime does).
- The sim never reads a model file. `SLIME_EDGE_M` stays authoritative; the model is render-only.
- Blockbench axes: +Y up, `north` = −Z, `south` = +Z, `east` = +X. Rotations are Euler degrees in three.js order `'ZYX'`: matrix Rz·Ry·Rx, so X applies first. In Bevy that is `Quat::from_euler(EulerRot::ZYX, z, y, x)`.
- **Never commit or copy corpus files** (`target/test-models/**`) into the repo. Most are All Rights Reserved. Corpus tests read them in place and pass (skip) when the directory is absent.
- **Bevy version is in flux:** the root `Cargo.toml` was moved from 0.18 to 0.19.1 in the working tree while this plan was written.
  - Every Bevy API used below was checked to have the same signature in both versions: `Image::new`, `ImageSampler::nearest`, `Mesh::new`, `with_inserted_attribute`, `with_inserted_indices`, `StandardMaterial{double_sided,cull_mode,alpha_mode,unlit,base_color_texture}`, `ChildOf`, `EulerRot::ZYX`.
  - Copy import paths from the neighbouring code **at execution time**: `bevy::mesh::{Indices, Mesh, PrimitiveTopology}` as in `renderer/meshing/emit.rs`; the `render_resource` imports as in `renderer/mod.rs`.
- **Verification:**
  - `just test-one bbmodel "--lib"` is fast (no Bevy).
  - Client tests (`just test-one client "--lib renderer::model::"`) link the Bevy test binary, which takes minutes cold. CLAUDE.md makes that an explicit opt-in. The user approves it by approving this plan; say so when running them.
  - Never run whole suites. Finish with `just smoke`.
- Tag any test slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`. For corpus tests, point the tag text at `just model-corpus` instead.
- **Shared, dirty working tree.** Other sessions have uncommitted work in `Cargo.toml`, `Cargo.lock`, `justfile`, `CLAUDE.md`, `crates/client/src/renderer/avatar.rs` and `renderer/mod.rs`.
  - **Do not commit.** CLAUDE.md: commit only when the user asks.
  - Each task ends with a `git status --short` checkpoint instead.
  - Task 10 offers the commit, file by file.
- **No git worktrees.** Work in the main checkout on `develop` (CLAUDE.md).
- Name tunables as constants: `MODEL_ALPHA_CUTOFF`, `UNTEXTURED_COLOR`, `SLIME_MODEL_EDGE_PX`, `BOX_UV_INSET_PX`.

## Review Focus

1. **High-resolution textures.** When a texture's `uv_width`/`uv_height` differs from its pixel size, UVs must be divided by the UV size, not the image size. Otherwise HD skins render zoomed. Pinned in Task 3 (`per_texture_uv_size_scales_uvs`).
2. **Pre-5.0 animations.** They must negate position.x, rotation.x and rotation.y on load, or every 4.x model animates mirrored. Pinned in Task 5 (`pre_5_0_animations_negate_x_and_rotation_y`).
3. **Elements outside any group.** A model with zero groups, or elements at the outliner root, must still render: they land on an implicit root bone. Pinned in Task 1 (`root_level_elements_land_on_the_root_bone`).
4. **Bad texture references.** A face whose texture index is out of range must fall back to the default texture, as Blockbench does, not panic. A model with no textures renders untextured grey. Pinned in Task 4 (`out_of_range_texture_falls_back_to_default`, `no_textures_renders_untextured`).
5. **Many instances.** A hundred slimes must share one set of mesh, material and image handles, not upload per instance. Pinned in Task 7 (`instances_share_meshes_and_materials`).

---

## File Structure

| file | responsibility |
|---|---|
| `crates/bbmodel/Cargo.toml` | new crate, edition 2024, deps serde_json/base64/image |
| `crates/bbmodel/src/lib.rs` | module wiring + public re-exports |
| `crates/bbmodel/src/math.rs` | `Vec3`, `Mat3`, `euler_zyx_deg`, tiny vector ops |
| `crates/bbmodel/src/model.rs` | `parse()`, `Model`/`Bone`/`Cube`/`Face`/`Texture`, outliner walk (3.x–5.x), texture decode + resolution rules, `bone_world`, `rest_bounds` |
| `crates/bbmodel/src/geometry.rs` | cube corners (Blockbench `setShape` table), box/per-face UVs, alpha classing, `mesh_parts()` |
| `crates/bbmodel/src/anim.rs` | animation parse + `Channel::eval` + `Animation::sample` |
| `crates/bbmodel/src/test_util.rs` | `#[cfg(test)]` JSON/PNG fixture builders |
| `crates/bbmodel/tests/corpus.rs` | local-corpus invariants (ignored, `just model-corpus`) |
| `crates/client/src/renderer/model.rs` | `LoadedModel`, `ModelLibrary`, `EMBEDDED_MODELS`, `spawn_model`, `ModelAnimator`, `animate_models` |
| `crates/client/examples/model_viewer.rs` | interactive viewer + `--shots` batch screenshots |
| `scripts/make_slime_bbmodel.py` | writes the original `assets/models/slime.bbmodel` (stdlib only) |
| `assets/models/slime.bbmodel` | generated slime model (checked in) |
| modify `Cargo.toml` | workspace member + `bbmodel`, `base64` workspace deps |
| modify `crates/client/Cargo.toml` | `bbmodel` dep |
| modify `crates/client/src/renderer/mod.rs` | `pub mod model;`, `ModelLibrary` resource, `animate_models` system |
| modify `crates/client/src/renderer/avatar.rs` | slime body from the model |
| modify `justfile` | `model-corpus`, `model-view`, bbmodel lib tests in `smoke` |
| modify `CLAUDE.md` | workspace-layout entry for `crates/bbmodel` + recipes |

---

### Task 1: `bbmodel` crate, parse to a bone hierarchy

**Files:**
- Create: `crates/bbmodel/Cargo.toml`, `crates/bbmodel/src/{lib.rs,math.rs,model.rs,test_util.rs}`
- Create (stubs filled by later tasks): `crates/bbmodel/src/geometry.rs`, `crates/bbmodel/src/anim.rs`
- Modify: `Cargo.toml` (workspace `members` + `[workspace.dependencies]`)

**Interfaces:**
- Produces:
  - `bbmodel::parse(bytes: &[u8]) -> Result<Model, Error>`
  - `Model { format_version: (u32, u32), model_format: String, resolution: [f32; 2], bones: Vec<Bone>, textures: Vec<Texture>, animations: Vec<Animation>, warnings: Vec<String> }`
  - `Bone { name, uuid, parent: Option<usize>, origin: Vec3, rotation: Vec3, cubes: Vec<Cube> }`. `bones[0]` is always the implicit root (name `""`, origin 0), and every parent index is lower than its child's.
  - `Cube { name, from, to, origin, rotation, inflate: f32, stretch: Vec3, box_uv: bool, uv_offset: [f32; 2], mirror_uv: bool, faces: [Option<Face>; 6] }`, with faces in `FaceDir::ALL` order.
  - `Face { uv: [f32; 4], texture: Option<usize>, rotation: i32 }`. `None` for the face itself means absent; `texture: None` means untextured.
  - `FaceDir::{East, West, Up, Down, South, North}`, with `ALL`, `key()` and `normal()`.
  - `Texture { name, image: Option<image::RgbaImage>, uv_size: [f32; 2], frames: u32, emissive: bool }`
  - `Error` (impl `Display` + `std::error::Error`).
  - `math::{Vec3, Mat3, euler_zyx_deg, mul_vec, mat_mul, add, sub}`.

- [ ] **Step 1: Crate skeleton and workspace wiring**

`Cargo.toml` (root): add `"crates/bbmodel",` to `members`, and to `[workspace.dependencies]`:

```toml
bbmodel = { path = "crates/bbmodel" }
# Embedded Blockbench textures are base64 data URLs (already in Cargo.lock via other deps).
base64 = "0.22"
```

`crates/bbmodel/Cargo.toml`:

```toml
[package]
name = "bbmodel"
version = "0.1.0"
edition = "2024"

# Blockbench .bbmodel parsing, cube meshing and animation sampling. Bevy-free
# (the client adapts it; `game` may use it later for model -> ChiselData).
[dependencies]
serde_json = { workspace = true }
base64 = { workspace = true }
image = { workspace = true }
```

`crates/bbmodel/src/lib.rs`:

```rust
//! Blockbench `.bbmodel` files, engine-free: [`parse`] into a normalized
//! [`Model`] (formats 3.x–5.x), [`mesh_parts`] to turn its cubes into
//! triangles exactly as Blockbench's own preview does, and
//! [`Animation::sample`] for keyframe animation.
//!
//! Units are Blockbench pixels, which are also world units here (16 per
//! block). Axes: +Y up, north = −Z, east = +X; rotations are Euler degrees in
//! three.js order 'ZYX' (X applied first).
//!
//! Spec: docs/superpowers/specs/2026-09-22-blockbench-model-loading-investigation.md

mod anim;
mod geometry;
pub mod math;
mod model;
#[cfg(test)]
mod test_util;

pub use anim::{Animation, BonePose, BoneTrack, Channel, Interp, Keyframe, LoopMode};
pub use geometry::{mesh_parts, AlphaClass, MeshPart};
pub use model::{parse, Bone, Cube, Error, Face, FaceDir, Model, Texture};
```

`crates/bbmodel/src/geometry.rs` and `crates/bbmodel/src/anim.rs` start as stubs so the crate compiles. Tasks 2 and 5 replace them. `geometry.rs`:

```rust
//! Cube → triangles (Task 2/3/4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaClass { Opaque, Mask, Blend }
#[derive(Debug, Clone)]
pub struct MeshPart;
pub fn mesh_parts(_model: &crate::Model) -> Vec<MeshPart> { Vec::new() }
```

`anim.rs`:

```rust
//! Animations (Task 5).
use crate::math::Vec3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum LoopMode { Once, Loop, Hold }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Interp { Linear, Step, CatmullRom, Bezier }
#[derive(Debug, Clone)] pub struct Keyframe { pub time: f32, pub interp: Interp, pub pre: Vec3, pub post: Vec3, pub two_points: bool }
#[derive(Debug, Clone, Default)] pub struct Channel { pub keys: Vec<Keyframe> }
#[derive(Debug, Clone)] pub struct BoneTrack { pub bone: usize, pub rotation: Channel, pub position: Channel, pub scale: Channel }
#[derive(Debug, Clone)] pub struct Animation { pub name: String, pub length: f32, pub loop_mode: LoopMode, pub tracks: Vec<BoneTrack> }
#[derive(Debug, Clone, Copy, PartialEq)] pub struct BonePose { pub position: Vec3, pub rotation: Vec3, pub scale: Vec3 }
pub(crate) fn parse_animations(_root: &serde_json::Value, _bones: &[crate::Bone], _version: (u32, u32), _warnings: &mut Vec<String>) -> Vec<Animation> { Vec::new() }
```

`crates/bbmodel/src/math.rs`:

```rust
//! Just enough 3D math for model space; no dependency on a math crate.

pub type Vec3 = [f32; 3];
/// Row-major 3×3 matrix.
pub type Mat3 = [[f32; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Blockbench's rotation for cubes and groups: Euler degrees in three.js order
/// 'ZYX', i.e. the matrix Rz·Ry·Rx (X is applied to a vector first).
pub fn euler_zyx_deg(r: Vec3) -> Mat3 {
    let (sx, cx) = r[0].to_radians().sin_cos();
    let (sy, cy) = r[1].to_radians().sin_cos();
    let (sz, cz) = r[2].to_radians().sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    mat_mul(mat_mul(rz, ry), rx)
}

pub fn mat_mul(a: Mat3, b: Mat3) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

pub fn mul_vec(m: Mat3, v: Vec3) -> Vec3 {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    #[test]
    fn x_rotation_turns_up_toward_south() {
        // +90° about X takes +Y to +Z (right-handed, three.js convention).
        assert!(close(mul_vec(euler_zyx_deg([90.0, 0.0, 0.0]), [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
    }

    #[test]
    fn zyx_order_applies_x_first() {
        // X then Z: +Y --X90--> +Z --Z90--> +Z (Z leaves it). Z-first would
        // give +Y --Z90--> -X --X90--> -X. Only X-first yields +Z.
        let m = euler_zyx_deg([90.0, 0.0, 90.0]);
        assert!(close(mul_vec(m, [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
    }
}
```

`crates/bbmodel/src/test_util.rs`:

```rust
//! Fixture builders for unit tests: minimal .bbmodel JSON and PNG data URLs.
use base64::Engine as _;
use serde_json::{json, Value};

pub fn model(v: Value) -> crate::Model {
    crate::parse(v.to_string().as_bytes()).expect("fixture parses")
}

/// A `data:image/png;base64,...` URL of `img`.
pub fn data_url(img: &image::RgbaImage) -> String {
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// An embedded texture entry of `w`×`h` filled with `rgba`.
pub fn texture(w: u32, h: u32, rgba: [u8; 4]) -> Value {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
    json!({"name": "t.png", "uuid": format!("tex-{w}x{h}-{rgba:?}"), "width": w, "height": h,
           "uv_width": w, "uv_height": h, "source": data_url(&img)})
}

/// A per-face-UV cube from `from` to `to`, every face on texture 0 with UV
/// rect `[0,0,1,1]`.
pub fn cube(uuid: &str, from: [f32; 3], to: [f32; 3]) -> Value {
    let face = json!({"uv": [0, 0, 1, 1], "texture": 0});
    json!({"type": "cube", "uuid": uuid, "name": uuid, "from": from, "to": to, "origin": [0, 0, 0],
           "faces": {"north": face, "east": face, "south": face, "west": face, "up": face, "down": face}})
}
```

- [ ] **Step 2: Write the failing parse tests**

Append to a new `crates/bbmodel/src/model.rs`: the test module first, with the implementation arriving in Step 4.

```rust
#[cfg(test)]
mod tests {
    use crate::test_util::{cube, model, texture};
    use serde_json::json;

    #[test]
    fn v5_groups_array_builds_the_hierarchy() {
        let m = model(json!({
            "meta": {"format_version": "5.0", "model_format": "free"},
            "resolution": {"width": 16, "height": 16},
            "elements": [cube("c1", [0.,0.,0.], [1.,1.,1.]), cube("c2", [0.,0.,0.], [2.,2.,2.])],
            "groups": [
                {"uuid": "g1", "name": "body", "origin": [0, 8, 0], "rotation": [0, 45, 0]},
                {"uuid": "g2", "name": "head", "origin": [0, 16, 0]}
            ],
            "outliner": [{"uuid": "g1", "children": ["c1", {"uuid": "g2", "children": ["c2"]}]}],
            "textures": [texture(16, 16, [255, 0, 0, 255])]
        }));
        let names: Vec<_> = m.bones.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["", "body", "head"]);
        assert_eq!(m.bones[1].parent, Some(0));
        assert_eq!(m.bones[2].parent, Some(1));
        assert_eq!(m.bones[1].rotation, [0.0, 45.0, 0.0]);
        assert_eq!(m.bones[2].origin, [0.0, 16.0, 0.0]);
        assert_eq!(m.bones[1].cubes.len(), 1);
        assert_eq!(m.bones[2].cubes[0].to, [2.0, 2.0, 2.0]);
        assert_eq!(m.format_version, (5, 0));
    }

    #[test]
    fn v4_inline_outliner_groups_parse_the_same() {
        let m = model(json!({
            "meta": {"format_version": "4.10", "model_format": "free"},
            "elements": [cube("c1", [0.,0.,0.], [1.,1.,1.])],
            "outliner": [{"uuid": "g1", "name": "body", "origin": [0, 8, 0], "rotation": [10, 0, 0],
                          "children": ["c1"]}],
            "textures": []
        }));
        assert_eq!(m.bones.len(), 2);
        assert_eq!(m.bones[1].name, "body");
        assert_eq!(m.bones[1].rotation, [10.0, 0.0, 0.0]);
        assert_eq!(m.bones[1].cubes.len(), 1);
    }

    #[test]
    fn root_level_elements_land_on_the_root_bone() {
        // Review Focus 3: no groups at all, plus an element missing from the
        // outliner entirely (orphan) — both must still render.
        let m = model(json!({
            "meta": {"format_version": "5.0", "model_format": "free"},
            "elements": [cube("a", [0.,0.,0.], [1.,1.,1.]), cube("orphan", [0.,0.,0.], [1.,1.,1.])],
            "outliner": ["a"],
            "textures": []
        }));
        assert_eq!(m.bones.len(), 1);
        assert_eq!(m.bones[0].cubes.len(), 2);
    }

    #[test]
    fn hidden_and_unexported_things_are_skipped() {
        let mut hidden = cube("h", [0.,0.,0.], [1.,1.,1.]);
        hidden["visibility"] = json!(false);
        let m = model(json!({
            "meta": {"format_version": "5.0", "model_format": "free"},
            "elements": [hidden, cube("in_hidden_group", [0.,0.,0.], [1.,1.,1.]), cube("ok", [0.,0.,0.], [1.,1.,1.])],
            "groups": [{"uuid": "g", "name": "ref", "export": false}],
            "outliner": ["h", {"uuid": "g", "children": ["in_hidden_group"]}, "ok"],
            "textures": []
        }));
        let cubes: Vec<_> = m.bones.iter().flat_map(|b| &b.cubes).map(|c| c.name.as_str()).collect();
        assert_eq!(cubes, ["ok"], "hidden cube, unexported group and its children are all skipped");
        assert!(m.bones.iter().all(|b| b.name != "ref"));
    }

    #[test]
    fn mesh_elements_are_skipped_with_a_warning() {
        let m = model(json!({
            "meta": {"format_version": "5.0", "model_format": "free"},
            "elements": [{"type": "mesh", "uuid": "m", "vertices": {}, "faces": {}}],
            "outliner": ["m"], "textures": []
        }));
        assert!(m.bones[0].cubes.is_empty());
        assert!(m.warnings.iter().any(|w| w.contains("mesh")), "{:?}", m.warnings);
    }

    #[test]
    fn numbers_as_strings_and_unknown_fields_are_tolerated() {
        let mut c = cube("c", [0.,0.,0.], [1.,1.,1.]);
        c["from"] = json!(["1", "2", "3"]);
        c["some_future_field"] = json!({"x": 1});
        let m = model(json!({
            "meta": {"format_version": "5.0", "model_format": "free"}, "future_root": 7,
            "elements": [c], "outliner": ["c"], "textures": []
        }));
        assert_eq!(m.bones[0].cubes[0].from, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn null_texture_face_is_absent_and_single_texture_formats_use_the_default() {
        let mut c = cube("c", [0.,0.,0.], [1.,1.,1.]);
        c["faces"]["up"]["texture"] = json!(null);
        c["faces"]["north"]["texture"] = json!(1);
        let mut t1 = texture(16, 16, [0, 255, 0, 255]);
        t1["use_as_default"] = json!(true);
        let m = model(json!({
            "meta": {"format_version": "5.0", "model_format": "bedrock"},
            "elements": [c], "outliner": ["c"],
            "textures": [texture(16, 16, [255, 0, 0, 255]), t1]
        }));
        let faces = &m.bones[0].cubes[0].faces;
        assert!(faces[crate::FaceDir::Up as usize].is_none());
        // bedrock is single-texture: every present face shows the default (1).
        assert!(faces.iter().flatten().all(|f| f.texture == Some(1)));
    }

    #[test]
    fn not_a_model_is_an_error() {
        assert!(crate::parse(br#"{"format_version":"1.12.0","minecraft:geometry":[]}"#).is_err());
        assert!(crate::parse(b"not json").is_err());
    }
}
```

- [ ] **Step 3: Run the tests; they fail**

Run: `just test-one bbmodel "--lib"`
Expected: compile error or FAIL. `parse`, `Model` and the rest don't exist yet.

- [ ] **Step 4: Implement `model.rs` (above the test module)**

```rust
//! `.bbmodel` JSON → [`Model`]. Parsed from `serde_json::Value` by hand
//! rather than derived structs: Blockbench files span format 3.x–5.x, carry
//! plugin fields, and occasionally store numbers as strings.

use std::collections::{HashMap, HashSet};
use std::fmt;

use base64::Engine as _;
use serde_json::Value;

use crate::anim::{self, Animation};
use crate::math::{self, Mat3, Vec3};

/// Formats in which every present face shows the default texture whatever
/// index it stores (Blockbench `Format.single_texture`; the last two are the
/// GeckoLib plugin's formats, which behave the same).
const SINGLE_TEXTURE_FORMATS: &[&str] =
    &["bedrock", "bedrock_old", "modded_entity", "skin", "image", "geckolib_model", "animated_entity_model"];
/// Formats whose face UVs are in each texture's own `uv_width`×`uv_height`
/// (Blockbench `Format.per_texture_uv_size`); all others use `resolution`.
const PER_TEXTURE_UV_FORMATS: &[&str] = &["free", "optifine_entity", "optifine_part", "image"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaceDir { East, West, Up, Down, South, North }

impl FaceDir {
    /// Blockbench's `Canvas.face_order`; also the order of `Cube::faces`.
    pub const ALL: [FaceDir; 6] =
        [FaceDir::East, FaceDir::West, FaceDir::Up, FaceDir::Down, FaceDir::South, FaceDir::North];

    pub fn key(self) -> &'static str {
        match self {
            FaceDir::East => "east", FaceDir::West => "west", FaceDir::Up => "up",
            FaceDir::Down => "down", FaceDir::South => "south", FaceDir::North => "north",
        }
    }

    pub fn normal(self) -> Vec3 {
        match self {
            FaceDir::East => [1.0, 0.0, 0.0], FaceDir::West => [-1.0, 0.0, 0.0],
            FaceDir::Up => [0.0, 1.0, 0.0], FaceDir::Down => [0.0, -1.0, 0.0],
            FaceDir::South => [0.0, 0.0, 1.0], FaceDir::North => [0.0, 0.0, -1.0],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    /// `[u1, v1, u2, v2]` in the texture's UV space (pixels, v down). For
    /// box-UV cubes this is ignored and recomputed from the layout.
    pub uv: [f32; 4],
    /// Texture index; `None` = untextured (the model has no textures).
    pub texture: Option<usize>,
    /// 0 / 90 / 180 / 270.
    pub rotation: i32,
}

#[derive(Debug, Clone)]
pub struct Cube {
    pub name: String,
    pub from: Vec3,
    pub to: Vec3,
    /// Rotation pivot, absolute model pixels.
    pub origin: Vec3,
    /// Euler degrees, 'ZYX'.
    pub rotation: Vec3,
    pub inflate: f32,
    pub stretch: Vec3,
    pub box_uv: bool,
    pub uv_offset: [f32; 2],
    pub mirror_uv: bool,
    /// In [`FaceDir::ALL`] order; `None` = no face there.
    pub faces: [Option<Face>; 6],
}

#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    pub uuid: String,
    /// Always a lower index than this bone; `None` only for `bones[0]`.
    pub parent: Option<usize>,
    /// Pivot, absolute model pixels.
    pub origin: Vec3,
    /// Rest rotation, Euler degrees 'ZYX'.
    pub rotation: Vec3,
    pub cubes: Vec<Cube>,
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub name: String,
    /// Decoded pixels; `None` if the file only references an external path
    /// or the PNG did not decode (a warning says which).
    pub image: Option<image::RgbaImage>,
    /// The UV space face UVs are expressed in (pixels, per frame).
    pub uv_size: [f32; 2],
    /// Animated textures stack frames vertically; v1 shows frame 0.
    pub frames: u32,
    pub emissive: bool,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub format_version: (u32, u32),
    pub model_format: String,
    pub resolution: [f32; 2],
    /// `bones[0]` is the implicit root holding root-level elements; parents
    /// always precede children.
    pub bones: Vec<Bone>,
    pub textures: Vec<Texture>,
    pub animations: Vec<Animation>,
    /// Deduplicated notes about things skipped or approximated.
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum Error {
    Json(serde_json::Error),
    /// Valid JSON, but not a Blockbench project (no `meta`).
    NotAModel,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Json(e) => write!(f, "not valid JSON: {e}"),
            Error::NotAModel => write!(f, "not a Blockbench project (no `meta`)"),
        }
    }
}

impl std::error::Error for Error {}

pub(crate) fn warn(warnings: &mut Vec<String>, msg: impl Into<String>) {
    let msg = msg.into();
    if !warnings.contains(&msg) {
        warnings.push(msg);
    }
}

pub(crate) fn num(v: Option<&Value>) -> Option<f32> {
    match v? {
        Value::Number(n) => n.as_f64().map(|x| x as f32),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn floats<const N: usize>(v: Option<&Value>, default: [f32; N]) -> [f32; N] {
    match v.and_then(Value::as_array) {
        Some(a) if a.len() >= N => std::array::from_fn(|i| num(a.get(i)).unwrap_or(default[i])),
        _ => default,
    }
}

pub(crate) fn string(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn flag(v: Option<&Value>, default: bool) -> bool {
    v.and_then(Value::as_bool).unwrap_or(default)
}

fn parse_version(s: &str) -> (u32, u32) {
    let mut it = s.split('.').map(|p| p.trim().parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Pixel data from a `data:image/png;base64,...` URL.
fn decode_data_url(s: &str) -> Option<image::RgbaImage> {
    let (_, b64) = s.split_once(',')?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    Some(image::load_from_memory(&bytes).ok()?.to_rgba8())
}

/// Blockbench `Texture.frameCount`: how many `uv_size`-shaped frames are
/// stacked in a `w`×`h` image.
pub fn frame_count(w: u32, h: u32, uv_size: [f32; 2]) -> u32 {
    let ratio = w as f32 / h as f32;
    let frames = ((uv_size[0] / uv_size[1]) / ratio - 0.05).ceil();
    if frames > 1.0 { frames as u32 } else { 1 }
}

struct TextureRules {
    count: usize,
    default: Option<usize>,
    single: bool,
    by_uuid: HashMap<String, usize>,
}

impl TextureRules {
    /// Blockbench's face-texture resolution (face.ts getTexture + the
    /// bbmodel loader): single-texture formats always show the default; an
    /// index or uuid that doesn't resolve falls back to the default.
    fn resolve(&self, tex: Option<&Value>, warnings: &mut Vec<String>) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        if self.single {
            return self.default;
        }
        let idx = match tex {
            Some(Value::Number(n)) => n.as_u64().map(|i| i as usize),
            Some(Value::String(s)) => self.by_uuid.get(s).copied().or_else(|| s.trim_start_matches('#').parse().ok()),
            _ => None,
        };
        match idx {
            Some(i) if i < self.count => Some(i),
            Some(i) => {
                warn(warnings, format!("face texture index {i} out of range; using the default texture"));
                self.default
            }
            None => self.default,
        }
    }
}

fn parse_textures(root: &Value, model_format: &str, resolution: [f32; 2], warnings: &mut Vec<String>) -> (Vec<Texture>, TextureRules) {
    let per_texture_uv = PER_TEXTURE_UV_FORMATS.contains(&model_format);
    let raw = root.get("textures").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut textures = Vec::with_capacity(raw.len());
    let mut by_uuid = HashMap::new();
    let mut default = None;
    for (i, t) in raw.iter().enumerate() {
        let name = string(t.get("name"));
        let source = string(t.get("source"));
        let image = if source.starts_with("data:") { decode_data_url(&source) } else { None };
        if image.is_none() {
            warn(warnings, format!("texture {name:?} is not embedded or did not decode; faces using it render untextured"));
        }
        let uv_size = if per_texture_uv {
            [num(t.get("uv_width")).unwrap_or(resolution[0]), num(t.get("uv_height")).unwrap_or(resolution[1])]
        } else {
            resolution
        };
        let frames = image.as_ref().map_or(1, |img| frame_count(img.width(), img.height(), uv_size));
        if frames > 1 {
            warn(warnings, format!("texture {name:?} is animated ({frames} frames); showing frame 0"));
        }
        let emissive = string(t.get("render_mode")) == "emissive";
        if flag(t.get("use_as_default"), false) && default.is_none() {
            default = Some(i);
        }
        by_uuid.insert(string(t.get("uuid")), i);
        textures.push(Texture { name, image, uv_size, frames, emissive });
    }
    let rules = TextureRules {
        count: textures.len(),
        default: default.or((!textures.is_empty()).then_some(0)),
        single: SINGLE_TEXTURE_FORMATS.contains(&model_format),
        by_uuid,
    };
    (textures, rules)
}

fn parse_cube(e: &Value, rules: &TextureRules, meta_box_uv: bool, warnings: &mut Vec<String>) -> Cube {
    let faces_v = e.get("faces");
    let faces = FaceDir::ALL.map(|dir| {
        let f = faces_v?.get(dir.key())?;
        let tex = f.get("texture");
        if matches!(tex, Some(Value::Null)) || !flag(f.get("enabled"), true) {
            return None;
        }
        Some(Face {
            uv: floats(f.get("uv"), [0.0; 4]),
            texture: rules.resolve(tex, warnings),
            rotation: (num(f.get("rotation")).unwrap_or(0.0) as i32).rem_euclid(360),
        })
    });
    Cube {
        name: string(e.get("name")),
        from: floats(e.get("from"), [0.0; 3]),
        to: floats(e.get("to"), [0.0; 3]),
        origin: floats(e.get("origin"), [0.0; 3]),
        rotation: floats(e.get("rotation"), [0.0; 3]),
        inflate: num(e.get("inflate")).unwrap_or(0.0),
        stretch: floats(e.get("stretch"), [1.0; 3]),
        box_uv: flag(e.get("box_uv"), meta_box_uv),
        uv_offset: floats(e.get("uv_offset"), [0.0; 2]),
        mirror_uv: flag(e.get("mirror_uv"), false),
        faces,
    }
}

struct Walk<'a> {
    elements: HashMap<String, &'a Value>,
    groups: HashMap<String, &'a Value>,
    placed: HashSet<String>,
    bones: Vec<Bone>,
    rules: TextureRules,
    meta_box_uv: bool,
    warnings: Vec<String>,
}

impl Walk<'_> {
    fn element(&mut self, uuid: &str, bone: usize) {
        let Some(e) = self.elements.get(uuid).copied() else { return };
        self.placed.insert(uuid.to_owned());
        if !flag(e.get("visibility"), true) || !flag(e.get("export"), true) {
            warn(&mut self.warnings, "hidden or non-exported elements skipped");
            return;
        }
        match e.get("type").and_then(Value::as_str).unwrap_or("cube") {
            "cube" => {
                let cube = parse_cube(e, &self.rules, self.meta_box_uv, &mut self.warnings);
                self.bones[bone].cubes.push(cube);
            }
            "mesh" => warn(&mut self.warnings, "mesh elements are not supported yet; skipped"),
            // locator, null_object, texture_mesh, ...: no geometry to draw.
            _ => {}
        }
    }

    fn mark_placed(&mut self, node: &Value) {
        match node {
            Value::String(uuid) => { self.placed.insert(uuid.clone()); }
            Value::Object(_) => {
                for c in node.get("children").and_then(Value::as_array).into_iter().flatten() {
                    self.mark_placed(c);
                }
            }
            _ => {}
        }
    }

    fn node(&mut self, node: &Value, parent: usize) {
        match node {
            Value::String(uuid) => self.element(uuid, parent),
            Value::Object(_) => {
                let uuid = string(node.get("uuid"));
                // 5.x keeps group fields in `groups[]`; ≤4.x inline in the node.
                let g = self.groups.get(&uuid).copied().unwrap_or(node);
                if !flag(g.get("visibility"), true) || !flag(g.get("export"), true) {
                    warn(&mut self.warnings, "hidden or non-exported groups skipped");
                    self.mark_placed(node);
                    return;
                }
                self.bones.push(Bone {
                    name: string(g.get("name")),
                    uuid,
                    parent: Some(parent),
                    origin: floats(g.get("origin"), [0.0; 3]),
                    rotation: floats(g.get("rotation"), [0.0; 3]),
                    cubes: Vec::new(),
                });
                let me = self.bones.len() - 1;
                for c in node.get("children").and_then(Value::as_array).into_iter().flatten() {
                    self.node(c, me);
                }
            }
            _ => {}
        }
    }
}

/// Parse a `.bbmodel` file.
pub fn parse(bytes: &[u8]) -> Result<Model, Error> {
    let root: Value = serde_json::from_slice(bytes).map_err(Error::Json)?;
    let meta = root.get("meta").ok_or(Error::NotAModel)?;
    let format_version = parse_version(&string(meta.get("format_version")));
    let model_format = string(meta.get("model_format"));
    let res = root.get("resolution");
    let resolution = [
        num(res.and_then(|r| r.get("width"))).unwrap_or(16.0),
        num(res.and_then(|r| r.get("height"))).unwrap_or(16.0),
    ];
    let mut warnings = Vec::new();
    let (textures, rules) = parse_textures(&root, &model_format, resolution, &mut warnings);

    let empty = Vec::new();
    let elements_v = root.get("elements").and_then(Value::as_array).unwrap_or(&empty);
    let mut walk = Walk {
        elements: elements_v.iter().map(|e| (string(e.get("uuid")), e)).collect(),
        groups: root.get("groups").and_then(Value::as_array).into_iter().flatten()
            .map(|g| (string(g.get("uuid")), g)).collect(),
        placed: HashSet::new(),
        bones: vec![Bone { name: String::new(), uuid: String::new(), parent: None, origin: [0.0; 3], rotation: [0.0; 3], cubes: Vec::new() }],
        rules,
        meta_box_uv: flag(meta.get("box_uv"), false),
        warnings,
    };
    for node in root.get("outliner").and_then(Value::as_array).into_iter().flatten() {
        walk.node(node, 0);
    }
    // Elements the outliner never mentions still belong to the model.
    for e in elements_v {
        let uuid = string(e.get("uuid"));
        if !walk.placed.contains(&uuid) {
            walk.element(&uuid, 0);
        }
    }
    let mut warnings = walk.warnings;
    let animations = anim::parse_animations(&root, &walk.bones, format_version, &mut warnings);
    Ok(Model { format_version, model_format, resolution, bones: walk.bones, textures, animations, warnings })
}

impl Model {
    /// Rest-pose model-space transform of every bone: (rotation, pivot).
    pub fn bone_world(&self) -> Vec<(Mat3, Vec3)> {
        let mut out: Vec<(Mat3, Vec3)> = Vec::with_capacity(self.bones.len());
        for b in &self.bones {
            let local_rot = math::euler_zyx_deg(b.rotation);
            let (rot, pos) = match b.parent {
                None => (local_rot, b.origin),
                Some(p) => {
                    let (prot, ppos) = out[p];
                    let local_t = math::sub(b.origin, self.bones[p].origin);
                    (math::mat_mul(prot, local_rot), math::add(ppos, math::mul_vec(prot, local_t)))
                }
            };
            out.push((rot, pos));
        }
        out
    }

    /// Model-space bounding box of the rest pose, or `None` if nothing draws.
    pub fn rest_bounds(&self) -> Option<(Vec3, Vec3)> {
        let world = self.bone_world();
        let mut bounds: Option<(Vec3, Vec3)> = None;
        for part in crate::mesh_parts(self) {
            let (rot, pos) = world[part.bone];
            for p in &part.positions {
                let w = math::add(pos, math::mul_vec(rot, *p));
                let (lo, hi) = bounds.get_or_insert((w, w));
                for i in 0..3 {
                    lo[i] = lo[i].min(w[i]);
                    hi[i] = hi[i].max(w[i]);
                }
            }
        }
        bounds
    }
}
```

`rest_bounds` needs `MeshPart { bone, positions, .. }` from Task 2. Until then the stub `MeshPart` has no fields. So add `rest_bounds` in Task 2 Step 3, and keep only `bone_world` here.

- [ ] **Step 5: Run the tests; they pass**

Run: `just test-one bbmodel "--lib"`
Expected: PASS for all `model::tests` and `math::tests`.

- [ ] **Step 6: Checkpoint**

Run: `git status --short Cargo.toml crates/bbmodel`
Expected: `Cargo.toml` modified; `crates/bbmodel/` untracked. `Cargo.lock` also changes; leave it.

---

### Task 2: Cube geometry, exactly as Blockbench draws it

**Files:**
- Modify: `crates/bbmodel/src/geometry.rs` (replace the stub)
- Modify: `crates/bbmodel/src/model.rs` (add `rest_bounds`, as written in Task 1 Step 4)

**Interfaces:**
- Consumes: `Model`, `Cube`, `Face`, `FaceDir`, `math::*` (Task 1).
- Produces:
  - `MeshPart { bone: usize, texture: Option<usize>, alpha: AlphaClass, positions: Vec<[f32;3]>, normals: Vec<[f32;3]>, uvs: Vec<[f32;2]>, indices: Vec<u32> }`. Positions are relative to the **bone pivot**; triangles are counter-clockwise when seen from outside.
  - `mesh_parts(&Model) -> Vec<MeshPart>`, one part per (bone, texture, alpha).
  - `pub(crate) fn cube_corners(&Cube) -> [[Vec3; 4]; 6]`.
  - `Model::rest_bounds() -> Option<(Vec3, Vec3)>`.
  - The UV and alpha logic arrives in Tasks 3–4. This task emits `uvs` as zeros and `alpha` as `Opaque`.

- [ ] **Step 1: Write the failing geometry tests** (bottom of `geometry.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{cube, model};
    use serde_json::json;

    fn one_cube(c: serde_json::Value) -> crate::Model {
        model(json!({"meta": {"format_version": "5.0", "model_format": "free"},
                     "elements": [c], "outliner": [c["uuid"].clone()], "textures": []}))
    }

    #[test]
    fn east_face_matches_blockbench_vertex_order_and_winds_outward() {
        let m = one_cube(cube("c", [0.,0.,0.], [2.,3.,4.]));
        let parts = mesh_parts(&m);
        assert_eq!(parts.len(), 1);
        let p = &parts[0];
        assert_eq!(p.positions.len(), 24);
        // Blockbench setShape, east: (to,to,to) (to,to,from) (to,from,to) (to,from,from)
        assert_eq!(&p.positions[0..4], &[[2.,3.,4.], [2.,3.,0.], [2.,0.,4.], [2.,0.,0.]]);
        // Triangles (0,2,1), (2,3,1): CCW seen from +X.
        let [a, b, c] = [0, 2, 1].map(|i| p.positions[p.indices[i] as usize]);
        let n = cross(crate::math::sub(b, a), crate::math::sub(c, a));
        assert!(n[0] > 0.0 && n[1].abs() < 1e-6 && n[2].abs() < 1e-6, "{n:?}");
        assert_eq!(p.normals[0], [1., 0., 0.]);
    }

    #[test]
    fn every_face_winds_toward_its_normal() {
        let m = one_cube(cube("c", [0.,0.,0.], [1.,1.,1.]));
        let p = &mesh_parts(&m)[0];
        for f in 0..6 {
            let base = f * 6;
            let [a, b, c] = [0, 1, 2].map(|i| p.positions[p.indices[base + i] as usize]);
            let n = cross(crate::math::sub(b, a), crate::math::sub(c, a));
            let want = p.normals[p.indices[base] as usize];
            assert!((0..3).map(|i| n[i] * want[i]).sum::<f32>() > 0.0, "face {f}");
        }
    }

    #[test]
    fn inflate_stretch_and_zero_thickness() {
        let mut c = cube("c", [0.,0.,0.], [2.,0.,2.]);
        c["inflate"] = json!(0.5);
        c["stretch"] = json!([2, 1, 1]);
        let m = one_cube(c);
        let (lo, hi) = m.rest_bounds().unwrap();
        // x: centre 1, half 1 → (1+0.5)*2 = 3 each side; y: 0 thick + inflate.
        assert_eq!(lo, [-2.0, -0.5, -0.5]);
        assert_eq!(hi, [4.0, 0.5, 2.5]);
    }

    #[test]
    fn a_zero_thickness_plane_keeps_a_sliver_so_both_faces_exist() {
        let m = one_cube(cube("c", [0.,0.,0.], [2.,0.,2.]));
        let p = &mesh_parts(&m)[0];
        let up = &p.positions[8..12];
        let down = &p.positions[12..16];
        assert!(up.iter().all(|v| (v[1] - 0.001).abs() < 1e-6));
        assert!(down.iter().all(|v| v[1] == 0.0));
    }

    #[test]
    fn cube_rotation_pivots_on_its_origin_and_bones_offset_by_pivot() {
        let mut c = cube("c", [0.,0.,0.], [2.,2.,2.]);
        c["origin"] = json!([0, 0, 0]);
        c["rotation"] = json!([0, 90, 0]);
        let m = model(json!({"meta": {"format_version": "5.0", "model_format": "free"},
            "elements": [c], "groups": [{"uuid": "g", "name": "g", "origin": [0, 10, 0]}],
            "outliner": [{"uuid": "g", "children": ["c"]}], "textures": []}));
        let p = &mesh_parts(&m)[0];
        assert_eq!(p.bone, 1);
        // +90° about Y maps +X to −Z; the east face's (2,2,2) corner → (2,2,−2),
        // then relative to the bone pivot (0,10,0): (2,−8,−2).
        let v = p.positions[0];
        assert!((v[0] - 2.0).abs() < 1e-5 && (v[1] + 8.0).abs() < 1e-5 && (v[2] + 2.0).abs() < 1e-5, "{v:?}");
        // Model space puts it back at y=2.
        let (lo, hi) = m.rest_bounds().unwrap();
        assert!((lo[1] - 0.0).abs() < 1e-5 && (hi[1] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn absent_faces_emit_nothing() {
        let mut c = cube("c", [0.,0.,0.], [1.,1.,1.]);
        c["faces"]["down"]["texture"] = json!(null);
        let p = &mesh_parts(&one_cube(c))[0];
        assert_eq!(p.positions.len(), 20);
        assert_eq!(p.indices.len(), 30);
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }
}
```

The fixture cubes have `"texture": 0` but `textures: []`. `TextureRules::resolve` returns `None` (untextured) when there are no textures, so parts group under `texture: None`.

- [ ] **Step 2: Run the tests; they fail**

Run: `just test-one bbmodel "--lib geometry::"`
Expected: compile errors (the `MeshPart` fields don't exist yet).

- [ ] **Step 3: Implement geometry** (replace the stub above the tests)

```rust
//! Cubes → triangles, mirroring Blockbench's preview (cube.js
//! `updateGeometry`/`updateFaces`/`updateUV`, three_custom.js `setShape`), so
//! a model looks here exactly as it does in the editor.

use std::collections::HashMap;

use crate::math::{self, Vec3};
use crate::model::{Cube, FaceDir, Model};

/// How a face's texels need blending, decided from the texels it samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaClass {
    /// Every sampled texel fully opaque.
    Opaque,
    /// Only fully opaque or fully transparent texels: alpha cutout.
    Mask,
    /// Some texel partially transparent: needs blending.
    Blend,
}

/// Triangles for one bone × texture × alpha class. Positions are relative to
/// the bone's pivot; triangles wind counter-clockwise seen from outside.
#[derive(Debug, Clone)]
pub struct MeshPart {
    pub bone: usize,
    pub texture: Option<usize>,
    pub alpha: AlphaClass,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

/// three_custom.js `setShape`: per face (in [`FaceDir::ALL`] order), its four
/// corners; `true` picks `to` on that axis, `false` picks `from`.
const SHAPE: [[[bool; 3]; 4]; 6] = {
    const T: bool = true;
    const F: bool = false;
    [
        [[T, T, T], [T, T, F], [T, F, T], [T, F, F]], // east
        [[F, T, F], [F, T, T], [F, F, F], [F, F, T]], // west
        [[F, T, F], [T, T, F], [F, T, T], [T, T, T]], // up
        [[F, F, T], [T, F, T], [F, F, F], [T, F, F]], // down
        [[F, T, T], [T, T, T], [F, F, T], [T, F, T]], // south
        [[T, T, F], [F, T, F], [T, F, F], [F, F, F]], // north
    ]
};
/// Blockbench `updateFaces`: two triangles per face over its 4 corners.
const FACE_INDICES: [u32; 6] = [0, 2, 1, 2, 3, 1];

/// Face corners relative to the cube's own origin, before its rotation:
/// inflate and stretch grow the box about its centre, and a zero-thickness
/// axis gets Blockbench's 0.001 sliver so both faces keep an area.
pub(crate) fn cube_corners(cube: &Cube) -> [[Vec3; 4]; 6] {
    let mut from = cube.from;
    let mut to = cube.to;
    for i in 0..3 {
        let half = (cube.to[i] - cube.from[i]) / 2.0;
        let centre = cube.from[i] + half;
        from[i] = centre - (half + cube.inflate) * cube.stretch[i] - cube.origin[i];
        to[i] = centre + (half + cube.inflate) * cube.stretch[i] - cube.origin[i];
        if from[i] == to[i] {
            to[i] += 0.001;
        }
    }
    SHAPE.map(|face| face.map(|pick| std::array::from_fn(|i| if pick[i] { to[i] } else { from[i] })))
}

/// Every drawable face of `model`, grouped by bone × texture × alpha class.
pub fn mesh_parts(model: &Model) -> Vec<MeshPart> {
    let mut parts: Vec<MeshPart> = Vec::new();
    let mut slot: HashMap<(usize, Option<usize>, AlphaClass), usize> = HashMap::new();
    for (bone_i, bone) in model.bones.iter().enumerate() {
        for cube in &bone.cubes {
            let rot = math::euler_zyx_deg(cube.rotation);
            let offset = math::sub(cube.origin, bone.origin);
            let corners = cube_corners(cube);
            for (fi, dir) in FaceDir::ALL.iter().enumerate() {
                let Some(face) = &cube.faces[fi] else { continue };
                let texture = face.texture;
                let uvs = [[0.0f32; 2]; 4]; // Task 3
                let alpha = AlphaClass::Opaque; // Task 4
                let key = (bone_i, texture, alpha);
                let i = *slot.entry(key).or_insert_with(|| {
                    parts.push(MeshPart { bone: bone_i, texture, alpha, positions: Vec::new(), normals: Vec::new(), uvs: Vec::new(), indices: Vec::new() });
                    parts.len() - 1
                });
                let part = &mut parts[i];
                let base = part.positions.len() as u32;
                let normal = math::mul_vec(rot, dir.normal());
                for k in 0..4 {
                    part.positions.push(math::add(math::mul_vec(rot, corners[fi][k]), offset));
                    part.normals.push(normal);
                    part.uvs.push(uvs[k]);
                }
                part.indices.extend(FACE_INDICES.map(|j| base + j));
            }
        }
    }
    parts
}
```

Then add `rest_bounds` (the code from Task 1 Step 4) to `impl Model` in `model.rs`.

- [ ] **Step 4: Run the tests; they pass**

Run: `just test-one bbmodel "--lib"`
Expected: PASS for all tests.

- [ ] **Step 5: Checkpoint.** `git status --short crates/bbmodel` shows only bbmodel files.

---

### Task 3: UVs: box UV, per-face UV, mirror, rotation, per-texture UV size

**Files:**
- Modify: `crates/bbmodel/src/geometry.rs`

**Interfaces:**
- Consumes: `Texture.uv_size`, `Texture.frames` (Task 1), `mesh_parts` (Task 2).
- Produces:
  - `pub(crate) fn face_uvs(cube: &Cube, dir: FaceDir, face: &Face, tex: Option<&Texture>, floor_box_uv: bool) -> [[f32; 2]; 4]`: normalized UVs with v down and frame 0, one per corner in `SHAPE` order.
  - `pub(crate) fn box_uv_rect(cube: &Cube, dir: FaceDir, floor: bool) -> [f32; 4]`.
  - `pub const BOX_UV_INSET_PX: f32 = 1.0 / 64.0`.
  - `mesh_parts` now fills `uvs`.

- [ ] **Step 1: Write the failing UV tests** (append to `geometry::tests`)

```rust
    use crate::model::{Face, FaceDir};

    fn test_cube(size: [f32; 3], box_uv: bool, mirror: bool) -> crate::Cube {
        crate::Cube { name: String::new(), from: [0.0; 3], to: size, origin: [0.0; 3], rotation: [0.0; 3],
            inflate: 0.0, stretch: [1.0; 3], box_uv, uv_offset: [0.0, 0.0], mirror_uv: mirror, faces: Default::default() }
    }

    fn tex(uv_size: [f32; 2], frames: u32) -> crate::Texture {
        crate::Texture { name: String::new(), image: None, uv_size, frames, emissive: false }
    }

    #[test]
    fn box_uv_layout_matches_blockbench() {
        let c = test_cube([8.0, 4.0, 2.0], true, false); // x=8, y=4, z=2
        assert_eq!(box_uv_rect(&c, FaceDir::East, true), [0.0, 2.0, 2.0, 6.0]);
        assert_eq!(box_uv_rect(&c, FaceDir::North, true), [2.0, 2.0, 10.0, 6.0]);
        assert_eq!(box_uv_rect(&c, FaceDir::West, true), [10.0, 2.0, 12.0, 6.0]);
        assert_eq!(box_uv_rect(&c, FaceDir::South, true), [12.0, 2.0, 20.0, 6.0]);
        assert_eq!(box_uv_rect(&c, FaceDir::Up, true), [10.0, 2.0, 2.0, 0.0]);
        assert_eq!(box_uv_rect(&c, FaceDir::Down, true), [18.0, 0.0, 10.0, 2.0]);
    }

    #[test]
    fn box_uv_floors_fractional_sizes_except_modded_entity() {
        let c = test_cube([2.5, 2.5, 2.5], true, false);
        assert_eq!(box_uv_rect(&c, FaceDir::North, true), [2.0, 2.0, 4.0, 4.0]);
        assert_eq!(box_uv_rect(&c, FaceDir::North, false), [2.5, 2.5, 5.0, 5.0]);
        // A 2-px span stored off-grid (-3.3..-1.3, the slime's eye) floors to 2.
        let mut eye = test_cube([0.0; 3], true, false);
        (eye.from, eye.to) = ([-3.3, 4.0, -3.5], [-1.3, 6.0, -1.5]);
        assert_eq!(box_uv_rect(&eye, FaceDir::North, true), [2.0, 2.0, 4.0, 4.0]);
    }

    #[test]
    fn mirror_flips_u_and_swaps_east_west() {
        let c = test_cube([8.0, 4.0, 2.0], true, true);
        assert_eq!(box_uv_rect(&c, FaceDir::East, true), [12.0, 2.0, 10.0, 6.0]);
        assert_eq!(box_uv_rect(&c, FaceDir::West, true), [2.0, 2.0, 0.0, 6.0]);
        assert_eq!(box_uv_rect(&c, FaceDir::North, true), [10.0, 2.0, 2.0, 6.0]);
    }

    #[test]
    fn per_face_uvs_normalize_and_rotate() {
        let c = test_cube([1.0; 3], false, false);
        let face = Face { uv: [0.0, 0.0, 8.0, 4.0], texture: Some(0), rotation: 0 };
        let t = tex([16.0, 16.0], 1);
        assert_eq!(face_uvs(&c, FaceDir::North, &face, Some(&t), true),
                   [[0.0, 0.0], [0.5, 0.0], [0.0, 0.25], [0.5, 0.25]]);
        let rotated = Face { rotation: 90, ..face };
        // Blockbench: arr0←arr2, arr2←arr3, arr3←arr1, arr1←arr0.
        assert_eq!(face_uvs(&c, FaceDir::North, &rotated, Some(&t), true),
                   [[0.0, 0.25], [0.0, 0.0], [0.5, 0.25], [0.5, 0.0]]);
    }

    #[test]
    fn per_texture_uv_size_scales_uvs() {
        // Review Focus 1: a 32 px image whose UV space is 16 (an HD texture).
        // UVs are divided by the UV size, not by the image's pixel size.
        let c = test_cube([1.0; 3], false, false);
        let face = Face { uv: [0.0, 0.0, 16.0, 16.0], texture: Some(0), rotation: 0 };
        let t = tex([16.0, 16.0], 1);
        assert_eq!(face_uvs(&c, FaceDir::North, &face, Some(&t), true)[3], [1.0, 1.0]);
    }

    #[test]
    fn animated_textures_show_frame_zero() {
        let c = test_cube([1.0; 3], false, false);
        let face = Face { uv: [0.0, 0.0, 16.0, 16.0], texture: Some(0), rotation: 0 };
        let t = tex([16.0, 16.0], 4);
        assert_eq!(face_uvs(&c, FaceDir::North, &face, Some(&t), true)[3], [1.0, 0.25]);
    }

    #[test]
    fn box_uv_is_inset_against_bleeding() {
        let c = test_cube([8.0, 4.0, 2.0], true, false);
        let face = Face { uv: [0.0; 4], texture: Some(0), rotation: 0 };
        let t = tex([64.0, 64.0], 1);
        let uv = face_uvs(&c, FaceDir::North, &face, Some(&t), true);
        assert_eq!(uv[0], [(2.0 + BOX_UV_INSET_PX) / 64.0, (2.0 + BOX_UV_INSET_PX) / 64.0]);
        assert_eq!(uv[3], [(10.0 - BOX_UV_INSET_PX) / 64.0, (6.0 - BOX_UV_INSET_PX) / 64.0]);
    }
```

- [ ] **Step 2: Run the tests; they fail**

Run: `just test-one bbmodel "--lib geometry::"`
Expected: compile errors (`box_uv_rect`, `face_uvs` and `BOX_UV_INSET_PX` are missing).

- [ ] **Step 3: Implement** (in `geometry.rs`; also add `Face`, `Texture` to the `use crate::model::...` line)

```rust
/// Blockbench shrinks every box-UV rect by this many texels per side so
/// linear sampling can't bleed the neighbouring face in (cube.js updateUV).
pub const BOX_UV_INSET_PX: f32 = 1.0 / 64.0;

/// A box-UV cube's face rect `[u1, v1, u2, v2]` (pixels), from its size and
/// `uv_offset` (cube.js updateUV). `floor` rounds the size down, which every
/// format except `modded_entity` (`box_uv_float_size`) does.
pub(crate) fn box_uv_rect(cube: &Cube, dir: FaceDir, floor: bool) -> [f32; 4] {
    let s: [f32; 3] = std::array::from_fn(|i| {
        let d = cube.to[i] - cube.from[i];
        // Blockbench nudges by 1e-7 before flooring because spans like
        // -1.3..-3.3 come out as 1.9999999999999998 in f64. f32 rounding
        // errors are ~1e-7 relative, too close to that nudge: use 1e-4.
        if floor { (d + 1e-4).floor() } else { d }
    });
    let (x, y, z) = (s[0], s[1], s[2]);
    // (face, from, size) in Blockbench's face_list order.
    let mut list = [
        (FaceDir::East, [0.0, z], [z, y]),
        (FaceDir::West, [z + x, z], [z, y]),
        (FaceDir::Up, [z + x, z], [-x, -z]),
        (FaceDir::Down, [z + 2.0 * x, 0.0], [-x, z]),
        (FaceDir::South, [2.0 * z + x, z], [x, y]),
        (FaceDir::North, [z, z], [x, y]),
    ];
    if cube.mirror_uv {
        for f in &mut list {
            f.1[0] += f.2[0];
            f.2[0] = -f.2[0];
        }
        let (east, west) = ((list[0].1, list[0].2), (list[1].1, list[1].2));
        (list[0].1, list[0].2) = west;
        (list[1].1, list[1].2) = east;
    }
    let (_, from, size) = list.into_iter().find(|f| f.0 == dir).expect("every face is listed");
    let o = cube.uv_offset;
    [from[0] + o[0], from[1] + o[1], from[0] + size[0] + o[0], from[1] + size[1] + o[1]]
}

/// Normalized UVs (v down, frame 0) for the face's four corners, in
/// [`SHAPE`] order, with face rotation applied.
pub(crate) fn face_uvs(cube: &Cube, dir: FaceDir, face: &Face, tex: Option<&Texture>, floor_box_uv: bool) -> [[f32; 2]; 4] {
    let mut uv = face.uv;
    if cube.box_uv {
        uv = box_uv_rect(cube, dir, floor_box_uv);
        for si in 0..2 {
            let m = if uv[si] > uv[si + 2] { -BOX_UV_INSET_PX } else { BOX_UV_INSET_PX };
            uv[si] += m;
            uv[si + 2] -= m;
        }
    }
    let (pw, ph) = tex.map_or((1.0, 1.0), |t| (t.uv_size[0], t.uv_size[1] * t.frames as f32));
    let mut arr = [
        [uv[0] / pw, uv[1] / ph],
        [uv[2] / pw, uv[1] / ph],
        [uv[0] / pw, uv[3] / ph],
        [uv[2] / pw, uv[3] / ph],
    ];
    let mut rot = face.rotation;
    while rot > 0 {
        let a = arr[0];
        arr[0] = arr[2];
        arr[2] = arr[3];
        arr[3] = arr[1];
        arr[1] = a;
        rot -= 90;
    }
    arr
}
```

In `mesh_parts`, compute once per model `let floor_box_uv = model.model_format != "modded_entity";` and replace the placeholder:

```rust
                let tex = texture.and_then(|t| model.textures.get(t));
                let uvs = face_uvs(cube, *dir, face, tex, floor_box_uv);
```

- [ ] **Step 4: Run the tests; they pass**

Run: `just test-one bbmodel "--lib"`
Expected: PASS for all tests.

- [ ] **Step 5: Checkpoint.** `git status --short crates/bbmodel`.

---

### Task 4: Textures: decode, frames, per-face alpha class, fallbacks

**Files:**
- Modify: `crates/bbmodel/src/geometry.rs` (alpha classing)
- Modify: `crates/bbmodel/src/model.rs` (tests only; decoding already landed in Task 1)

**Interfaces:**
- Consumes: `Texture.image` (Task 1), `face_uvs` (Task 3).
- Produces:
  - `pub(crate) fn classify_alpha(img: &image::RgbaImage, uvs: &[[f32; 2]; 4]) -> AlphaClass`.
  - `mesh_parts` groups by the real alpha class.

- [ ] **Step 1: Write the failing tests**

In `model::tests`:

```rust
    #[test]
    fn embedded_png_decodes_and_animated_frames_are_counted() {
        let mut anim = texture(16, 64, [0, 0, 255, 255]);
        anim["uv_width"] = json!(16);
        anim["uv_height"] = json!(16);
        let m = model(json!({"meta": {"format_version": "5.0", "model_format": "free"},
            "elements": [], "outliner": [], "textures": [texture(8, 8, [1, 2, 3, 255]), anim]}));
        let img = m.textures[0].image.as_ref().unwrap();
        assert_eq!((img.width(), img.height()), (8, 8));
        assert_eq!(img.get_pixel(0, 0).0, [1, 2, 3, 255]);
        assert_eq!(m.textures[1].frames, 4);
        assert!(m.warnings.iter().any(|w| w.contains("animated")));
    }

    #[test]
    fn out_of_range_texture_falls_back_to_default() {
        // Review Focus 4.
        let mut c = cube("c", [0.,0.,0.], [1.,1.,1.]);
        c["faces"]["north"]["texture"] = json!(9);
        let m = model(json!({"meta": {"format_version": "5.0", "model_format": "free"},
            "elements": [c], "outliner": ["c"], "textures": [texture(4, 4, [9, 9, 9, 255])]}));
        assert_eq!(m.bones[0].cubes[0].faces[crate::FaceDir::North as usize].as_ref().unwrap().texture, Some(0));
        assert!(m.warnings.iter().any(|w| w.contains("out of range")));
    }

    #[test]
    fn path_only_texture_warns_and_has_no_image() {
        let m = model(json!({"meta": {"format_version": "4.10", "model_format": "free"},
            "elements": [], "outliner": [],
            "textures": [{"name": "skin.png", "path": "/home/x/skin.png", "source": ""}]}));
        assert!(m.textures[0].image.is_none());
        assert!(m.warnings.iter().any(|w| w.contains("not embedded")));
    }
```

In `geometry::tests`:

```rust
    #[test]
    fn faces_are_classed_by_the_texels_they_sample() {
        // 4×1 texture: opaque | cutout (a=0) | translucent (a=128) | opaque.
        let mut img = image::RgbaImage::new(4, 1);
        for (x, a) in [255u8, 0, 128, 255].into_iter().enumerate() {
            img.put_pixel(x as u32, 0, image::Rgba([10, 20, 30, a]));
        }
        let quad = |u0: f32, u1: f32| [[u0, 0.0], [u1, 0.0], [u0, 1.0], [u1, 1.0]];
        assert_eq!(classify_alpha(&img, &quad(0.0, 0.25)), AlphaClass::Opaque);
        assert_eq!(classify_alpha(&img, &quad(0.0, 0.5)), AlphaClass::Mask);
        assert_eq!(classify_alpha(&img, &quad(0.0, 0.75)), AlphaClass::Blend);
        assert_eq!(classify_alpha(&img, &quad(0.75, 1.0)), AlphaClass::Opaque);
        // Mirrored / flipped rects (u1 < u0) cover the same texels.
        assert_eq!(classify_alpha(&img, &quad(0.5, 0.25)), AlphaClass::Mask);
    }

    #[test]
    fn no_textures_renders_untextured() {
        // Review Focus 4, second half.
        let p = &mesh_parts(&one_cube(cube("c", [0.,0.,0.], [1.,1.,1.])))[0];
        assert_eq!((p.texture, p.alpha), (None, AlphaClass::Opaque));
    }

    #[test]
    fn one_texture_splits_into_opaque_and_blend_parts() {
        // The slime case: gel faces sample translucent texels, core faces
        // opaque ones — same texture, two parts.
        let mut img = image::RgbaImage::from_pixel(2, 1, image::Rgba([0, 255, 0, 255]));
        img.put_pixel(0, 0, image::Rgba([0, 255, 0, 120]));
        let mut gel = cube("gel", [0.,0.,0.], [2.,2.,2.]);
        let mut core = cube("core", [0.,0.,0.], [1.,1.,1.]);
        for f in ["north", "east", "south", "west", "up", "down"] {
            gel["faces"][f]["uv"] = json!([0, 0, 1, 1]);
            core["faces"][f]["uv"] = json!([1, 0, 2, 1]);
        }
        let m = model(json!({"meta": {"format_version": "5.0", "model_format": "free"},
            "elements": [gel, core], "outliner": ["gel", "core"],
            "textures": [{"name": "t", "uuid": "t", "width": 2, "height": 1, "uv_width": 2, "uv_height": 1,
                          "source": crate::test_util::data_url(&img)}]}));
        let mut classes: Vec<_> = mesh_parts(&m).iter().map(|p| p.alpha).collect();
        classes.sort_by_key(|a| *a as u8);
        assert_eq!(classes, [AlphaClass::Opaque, AlphaClass::Blend]);
    }
```

- [ ] **Step 2: Run the tests; they fail**

Run: `just test-one bbmodel "--lib"`
Expected: `classify_alpha` is missing (compile error). The model tests may already pass, since decoding landed in Task 1.

- [ ] **Step 3: Implement**

```rust
/// The alpha class of the texels a face samples (frame 0). A rect thinner
/// than one texel samples the texel under its first corner.
pub(crate) fn classify_alpha(img: &image::RgbaImage, uvs: &[[f32; 2]; 4]) -> AlphaClass {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for [u, v] in uvs {
        (u0, u1, v0, v1) = (u0.min(*u), u1.max(*u), v0.min(*v), v1.max(*v));
    }
    let clamp_x = |x: f32| (x.max(0.0) as u32).min(img.width());
    let clamp_y = |y: f32| (y.max(0.0) as u32).min(img.height());
    let (mut x0, mut x1) = (clamp_x((u0 * w + 1e-3).floor()), clamp_x((u1 * w - 1e-3).ceil()));
    let (mut y0, mut y1) = (clamp_y((v0 * h + 1e-3).floor()), clamp_y((v1 * h - 1e-3).ceil()));
    if x0 >= x1 {
        x0 = clamp_x((uvs[0][0] * w).floor()).min(img.width() - 1);
        x1 = x0 + 1;
    }
    if y0 >= y1 {
        y0 = clamp_y((uvs[0][1] * h).floor()).min(img.height() - 1);
        y1 = y0 + 1;
    }
    let mut class = AlphaClass::Opaque;
    for y in y0..y1 {
        for x in x0..x1 {
            match img.get_pixel(x, y).0[3] {
                255 => {}
                0 => class = AlphaClass::Mask,
                _ => return AlphaClass::Blend,
            }
        }
    }
    class
}
```

In `mesh_parts`, replace the alpha placeholder:

```rust
                let alpha = tex.and_then(|t| t.image.as_ref()).map_or(AlphaClass::Opaque, |img| classify_alpha(img, &uvs));
```

- [ ] **Step 4: Run the tests; they pass**

Run: `just test-one bbmodel "--lib"`
Expected: PASS for all tests.

- [ ] **Step 5: Checkpoint.** `git status --short crates/bbmodel`.

---

### Task 5: Animation parse and sampling

**Files:**
- Modify: `crates/bbmodel/src/anim.rs` (replace the stub)

**Interfaces:**
- Consumes: `Bone.uuid`, `Bone.name` (Task 1); `model::{num, string, warn}` (Task 1, `pub(crate)`).
- Produces:
  - `Animation::sample(&self, t: f32) -> Vec<(usize, BonePose)>`, where `position` is **added** to the bone's rest translation (px), `rotation` is **added** to its rest Euler degrees, and `scale` **multiplies**.
  - `Animation::local_time(&self, t) -> Option<f32>`, and `Channel::eval(&self, t, looping) -> Option<Vec3>`.
  - `impl Default for BonePose` (0, 0, 1).

- [ ] **Step 1: Write the failing tests** (bottom of `anim.rs`)

```rust
#[cfg(test)]
mod tests {
    use crate::test_util::{cube, model};
    use serde_json::{json, Value};

    fn animated(version: &str, keyframes: Value, loop_mode: &str) -> crate::Model {
        model(json!({
            "meta": {"format_version": version, "model_format": "free"},
            "elements": [cube("c", [0.,0.,0.], [1.,1.,1.])],
            "groups": [{"uuid": "g", "name": "arm"}],
            "outliner": [{"uuid": "g", "name": "arm", "children": ["c"]}],
            "textures": [],
            "animations": [{"name": "wave", "loop": loop_mode, "length": 2.0,
                "animators": {"g": {"name": "arm", "type": "bone", "keyframes": keyframes}}}]
        }))
    }

    fn key(channel: &str, time: f32, interp: &str, points: Value) -> Value {
        json!({"channel": channel, "time": time, "interpolation": interp, "data_points": points})
    }

    fn rot_at(m: &crate::Model, t: f32) -> [f32; 3] {
        m.animations[0].sample(t).into_iter().find(|(b, _)| *b == 1).unwrap().1.rotation
    }

    #[test]
    fn linear_interpolates_between_keys_and_holds_ends() {
        let m = animated("5.0", json!([
            key("rotation", 0.0, "linear", json!([{"x": 0, "y": 0, "z": 0}])),
            key("rotation", 1.0, "linear", json!([{"x": 90, "y": 0, "z": 0}])),
        ]), "once");
        assert_eq!(rot_at(&m, 0.5), [45.0, 0.0, 0.0]);
        assert_eq!(rot_at(&m, 1.5), [90.0, 0.0, 0.0]); // after last key: hold its value
    }

    #[test]
    fn step_holds_the_previous_key() {
        let m = animated("5.0", json!([
            key("rotation", 0.0, "step", json!([{"x": 10, "y": 0, "z": 0}])),
            key("rotation", 1.0, "linear", json!([{"x": 90, "y": 0, "z": 0}])),
        ]), "once");
        assert_eq!(rot_at(&m, 0.9), [10.0, 0.0, 0.0]);
    }

    #[test]
    fn catmullrom_passes_through_keys_and_is_smooth() {
        let m = animated("5.0", json!([
            key("rotation", 0.0, "catmullrom", json!([{"x": 0, "y": 0, "z": 0}])),
            key("rotation", 1.0, "catmullrom", json!([{"x": 10, "y": 0, "z": 0}])),
            key("rotation", 2.0, "catmullrom", json!([{"x": 0, "y": 0, "z": 0}])),
        ]), "once");
        assert!((rot_at(&m, 1.0)[0] - 10.0).abs() < 1e-4);
        // Uniform Catmull-Rom, p0=p1=0, p2=10, p3=0 at t=0.5 → 5.625.
        assert!((rot_at(&m, 0.5)[0] - 5.625).abs() < 1e-4, "{:?}", rot_at(&m, 0.5));
    }

    #[test]
    fn pre_post_keys_jump() {
        let m = animated("5.0", json!([
            key("position", 0.0, "linear", json!([{"x": 0, "y": 0, "z": 0}])),
            key("position", 1.0, "linear", json!([{"x": 0, "y": 5, "z": 0}, {"x": 0, "y": 20, "z": 0}])),
            key("position", 2.0, "linear", json!([{"x": 0, "y": 20, "z": 0}])),
        ]), "once");
        let pos = |t| m.animations[0].sample(t).into_iter().find(|(b, _)| *b == 1).unwrap().1.position;
        assert_eq!(pos(0.5), [0.0, 2.5, 0.0]); // toward the "pre" value 5
        assert_eq!(pos(1.5), [0.0, 20.0, 0.0]); // from the "post" value 20
    }

    #[test]
    fn pre_5_0_animations_negate_x_and_rotation_y() {
        // Review Focus 2.
        let keys = json!([
            key("rotation", 0.0, "linear", json!([{"x": 10, "y": 20, "z": 30}])),
            key("position", 0.0, "linear", json!([{"x": 1, "y": 2, "z": 3}])),
        ]);
        let old = animated("4.5", keys.clone(), "once");
        let new = animated("5.0", keys, "once");
        let pose = |m: &crate::Model| m.animations[0].sample(0.0).into_iter().find(|(b, _)| *b == 1).unwrap().1;
        assert_eq!(pose(&new).rotation, [10.0, 20.0, 30.0]);
        assert_eq!(pose(&old).rotation, [-10.0, -20.0, 30.0]);
        assert_eq!(pose(&old).position, [-1.0, 2.0, 3.0]);
    }

    #[test]
    fn loop_wraps_hold_clamps_once_ends() {
        let keys = json!([
            key("rotation", 0.0, "linear", json!([{"x": 0, "y": 0, "z": 0}])),
            key("rotation", 2.0, "linear", json!([{"x": 20, "y": 0, "z": 0}])),
        ]);
        let looped = animated("5.0", keys.clone(), "loop");
        assert_eq!(rot_at(&looped, 2.5), [5.0, 0.0, 0.0]);
        let hold = animated("5.0", keys.clone(), "hold");
        assert_eq!(rot_at(&hold, 9.0), [20.0, 0.0, 0.0]);
        let once = animated("5.0", keys, "once");
        assert!(once.animations[0].sample(9.0).is_empty(), "a finished once-animation poses nothing");
    }

    #[test]
    fn molang_bezier_and_effects_warn_and_degrade() {
        let mut m_json = json!([
            key("rotation", 0.0, "bezier", json!([{"x": "math.sin(query.anim_time * 90) * 10", "y": "0", "z": 0}])),
            key("rotation", 1.0, "linear", json!([{"x": 4, "y": 0, "z": 0}])),
        ]);
        m_json.as_array_mut().unwrap().push(json!({"channel": "sound", "time": 0.0, "data_points": [{"effect": "x"}]}));
        let m = animated("5.0", m_json, "once");
        assert_eq!(rot_at(&m, 0.0)[0], 0.0, "molang evaluates to 0 for now");
        assert_eq!(rot_at(&m, 0.5)[0], 2.0, "bezier falls back to linear");
        for needle in ["molang", "bezier"] {
            assert!(m.warnings.iter().any(|w| w.contains(needle)), "{needle}: {:?}", m.warnings);
        }
    }

    #[test]
    fn animators_resolve_by_name_when_uuid_misses_and_warn_when_dangling() {
        let m = model(json!({
            "meta": {"format_version": "4.5", "model_format": "free"},
            "elements": [], "outliner": [{"uuid": "g", "name": "arm", "children": []}], "textures": [],
            "animations": [{"name": "a", "loop": "once", "length": 1.0, "animators": {
                "old-uuid": {"name": "arm", "keyframes": [key("rotation", 0.0, "linear", json!([{"x": 1, "y": 0, "z": 0}]))]},
                "gone": {"name": "tail", "keyframes": [key("rotation", 0.0, "linear", json!([{"x": 1, "y": 0, "z": 0}]))]}
            }}]
        }));
        assert_eq!(m.animations[0].tracks.len(), 1);
        assert_eq!(m.animations[0].tracks[0].bone, 1);
        assert!(m.warnings.iter().any(|w| w.contains("tail")));
    }
}
```

- [ ] **Step 2: Run the tests; they fail**

Run: `just test-one bbmodel "--lib anim::"`
Expected: FAIL. The stub parses no animations, and `sample` doesn't exist.

- [ ] **Step 3: Implement** (replace the stub)

```rust
//! Keyframe animations, sampled the way Blockbench's timeline does
//! (timeline_animators.js `interpolate`, keyframe.js `getLerp` /
//! `getCatmullromLerp`). v1 plays linear, step, catmullrom and pre/post
//! keys; bezier falls back to linear and molang expressions evaluate to 0
//! (both warn) — see the plan's decisions.

use std::collections::HashMap;

use serde_json::Value;

use crate::math::Vec3;
use crate::model::{num, string, warn, Bone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode { Once, Loop, Hold }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interp { Linear, Step, CatmullRom, Bezier }

#[derive(Debug, Clone)]
pub struct Keyframe {
    pub time: f32,
    pub interp: Interp,
    /// Value approaching the key (data point 0).
    pub pre: Vec3,
    /// Value leaving the key (data point 1; equals `pre` for a 1-point key).
    pub post: Vec3,
    pub two_points: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Channel {
    /// Sorted by time.
    pub keys: Vec<Keyframe>,
}

#[derive(Debug, Clone)]
pub struct BoneTrack {
    pub bone: usize,
    pub rotation: Channel,
    pub position: Channel,
    pub scale: Channel,
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub name: String,
    pub length: f32,
    pub loop_mode: LoopMode,
    pub tracks: Vec<BoneTrack>,
}

/// One bone's animated offsets: `position` (px) and `rotation` (Euler
/// degrees) are added to the rest pose, `scale` multiplies it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BonePose {
    pub position: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
}

impl Default for BonePose {
    fn default() -> Self {
        Self { position: [0.0; 3], rotation: [0.0; 3], scale: [1.0; 3] }
    }
}

/// Blockbench treats keys closer than this (seconds) as "at" the key.
const KEY_EPSILON: f32 = 1.0 / 1200.0;

fn catmull_rom(t: f32, p0: f32, p1: f32, p2: f32, p3: f32) -> f32 {
    // three.js Interpolations.CatmullRom (uniform), as SplineCurve uses.
    let v0 = (p2 - p0) * 0.5;
    let v1 = (p3 - p1) * 0.5;
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * p1 - 2.0 * p2 + v0 + v1) * t3 + (-3.0 * p1 + 3.0 * p2 - 2.0 * v0 - v1) * t2 + v0 * t + p1
}

impl Channel {
    pub fn eval(&self, t: f32, looping: bool) -> Option<Vec3> {
        let keys = &self.keys;
        let before = keys.iter().rposition(|k| k.time < t);
        let after = keys.iter().position(|k| k.time >= t);
        Some(match (before, after) {
            (None, None) => return None,
            (Some(b), _) if (keys[b].time - t).abs() < KEY_EPSILON => keys[b].post,
            (_, Some(a)) if (keys[a].time - t).abs() < KEY_EPSILON => keys[a].pre,
            (Some(b), _) if keys[b].interp == Interp::Step => keys[b].post,
            (Some(b), None) => keys[b].post,
            (None, Some(a)) => keys[a].pre,
            (Some(b), Some(a)) => {
                let (kb, ka) = (&keys[b], &keys[a]);
                let alpha = (t - kb.time) / (ka.time - kb.time);
                if kb.interp == Interp::CatmullRom || ka.interp == Interp::CatmullRom {
                    let n = keys.len();
                    let mut prev = b.checked_sub(1).map(|i| &keys[i]);
                    let mut next = keys.get(a + 1);
                    if looping && n >= 3 {
                        prev = prev.or(Some(&keys[n - 2]));
                        next = next.or(Some(&keys[1]));
                    }
                    let p0 = match prev { Some(k) if !kb.two_points => k.post, _ => kb.post };
                    let p3 = match next { Some(k) if !ka.two_points => k.pre, _ => ka.pre };
                    std::array::from_fn(|i| catmull_rom(alpha, p0[i], kb.post[i], ka.pre[i], p3[i]))
                } else {
                    // Linear; bezier is approximated as linear in v1.
                    std::array::from_fn(|i| kb.post[i] + (ka.pre[i] - kb.post[i]) * alpha)
                }
            }
        })
    }
}

impl Animation {
    /// Time inside the clip, or `None` once a play-once clip has ended.
    pub fn local_time(&self, t: f32) -> Option<f32> {
        match self.loop_mode {
            LoopMode::Loop => Some(if self.length > 0.0 { t.rem_euclid(self.length) } else { 0.0 }),
            LoopMode::Hold => Some(t.min(self.length)),
            LoopMode::Once => (t <= self.length).then_some(t),
        }
    }

    /// Pose of every animated bone at `t` seconds since the clip started.
    pub fn sample(&self, t: f32) -> Vec<(usize, BonePose)> {
        let Some(lt) = self.local_time(t) else { return Vec::new() };
        let looping = self.loop_mode == LoopMode::Loop;
        self.tracks
            .iter()
            .map(|tr| {
                let d = BonePose::default();
                (tr.bone, BonePose {
                    position: tr.position.eval(lt, looping).unwrap_or(d.position),
                    rotation: tr.rotation.eval(lt, looping).unwrap_or(d.rotation),
                    scale: tr.scale.eval(lt, looping).unwrap_or(d.scale),
                })
            })
            .collect()
    }
}

fn axis(v: Option<&Value>, default: f32, anim: &str, warnings: &mut Vec<String>) -> f32 {
    match v {
        None | Some(Value::Null) => default,
        Some(Value::String(s)) if s.trim().is_empty() => default,
        Some(other) => num(Some(other)).unwrap_or_else(|| {
            warn(warnings, format!("animation {anim:?}: molang expressions are not evaluated yet (read as 0)"));
            0.0
        }),
    }
}

pub(crate) fn parse_animations(root: &Value, bones: &[Bone], version: (u32, u32), warnings: &mut Vec<String>) -> Vec<Animation> {
    let pre_5 = version.0 < 5;
    let by_uuid: HashMap<&str, usize> = bones.iter().enumerate().skip(1).map(|(i, b)| (b.uuid.as_str(), i)).collect();
    let by_name: HashMap<&str, usize> = bones.iter().enumerate().skip(1).map(|(i, b)| (b.name.as_str(), i)).collect();
    let mut out = Vec::new();
    for a in root.get("animations").and_then(Value::as_array).into_iter().flatten() {
        let name = string(a.get("name"));
        let loop_mode = match string(a.get("loop")).as_str() {
            "loop" => LoopMode::Loop,
            "hold" => LoopMode::Hold,
            _ => LoopMode::Once,
        };
        let mut tracks = Vec::new();
        for (uuid, an) in a.get("animators").and_then(Value::as_object).into_iter().flatten() {
            if uuid == "effects" || string(an.get("type")) == "effect" {
                warn(warnings, "sound/particle/timeline effect tracks are ignored");
                continue;
            }
            let bone_name = string(an.get("name"));
            let Some(&bone) = by_uuid.get(uuid.as_str()).or_else(|| by_name.get(bone_name.as_str())) else {
                warn(warnings, format!("animation {name:?} animates missing bone {bone_name:?}"));
                continue;
            };
            let mut track = BoneTrack { bone, rotation: Channel::default(), position: Channel::default(), scale: Channel::default() };
            for k in an.get("keyframes").and_then(Value::as_array).into_iter().flatten() {
                let channel = string(k.get("channel"));
                let default = if channel == "scale" { 1.0 } else { 0.0 };
                let interp = match string(k.get("interpolation")).as_str() {
                    "step" => Interp::Step,
                    "catmullrom" => Interp::CatmullRom,
                    "bezier" => {
                        warn(warnings, format!("animation {name:?}: bezier keyframes play as linear"));
                        Interp::Bezier
                    }
                    _ => Interp::Linear,
                };
                let mut points: Vec<Vec3> = k.get("data_points").and_then(Value::as_array).into_iter().flatten()
                    .map(|dp| ["x", "y", "z"].map(|ax| axis(dp.get(ax), default, &name, warnings)))
                    .collect();
                // Blockbench 5.0 flipped these axes; older files are converted on load (bbmodel.js).
                if pre_5 && (channel == "position" || channel == "rotation") {
                    points.iter_mut().for_each(|p| p[0] = -p[0]);
                }
                if pre_5 && channel == "rotation" {
                    points.iter_mut().for_each(|p| p[1] = -p[1]);
                }
                let pre = points.first().copied().unwrap_or([default; 3]);
                let key = Keyframe {
                    time: num(k.get("time")).unwrap_or(0.0),
                    interp,
                    pre,
                    post: points.get(1).copied().unwrap_or(pre),
                    two_points: points.len() > 1,
                };
                match channel.as_str() {
                    "rotation" => track.rotation.keys.push(key),
                    "position" => track.position.keys.push(key),
                    "scale" => track.scale.keys.push(key),
                    _ => warn(warnings, "sound/particle/timeline effect tracks are ignored"),
                }
            }
            for ch in [&mut track.rotation, &mut track.position, &mut track.scale] {
                ch.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
            }
            tracks.push(track);
        }
        out.push(Animation { name, length: num(a.get("length")).unwrap_or(0.0), loop_mode, tracks });
    }
    out
}
```

- [ ] **Step 4: Run the tests; they pass**

Run: `just test-one bbmodel "--lib"`
Expected: PASS for all tests.

- [ ] **Step 5: Checkpoint.** `git status --short crates/bbmodel`.

---

### Task 6: Local-corpus test and `just model-corpus`

**Files:**
- Create: `crates/bbmodel/tests/corpus.rs`
- Modify: `justfile` (the `model-corpus` recipe, next to the existing `model-census` recipe; add `bbmodel` lib tests to `smoke`)

**Interfaces:**
- Consumes: the whole public `bbmodel` API.
- Produces: `just model-corpus`.

- [ ] **Step 1: Write the corpus test**

```rust
//! Every `.bbmodel` in the local test corpus (target/test-models/, gitignored
//! — see its MANIFEST.tsv; most files are All Rights Reserved and must never
//! be committed) through the loader, checking its invariants. Passes (skips)
//! when the corpus is absent. Run: `just model-corpus`.

use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-models")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "bbmodel") {
            out.push(p);
        }
    }
}

fn load(p: &Path) -> bbmodel::Model {
    bbmodel::parse(&std::fs::read(p).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
#[ignore = "slow: >100ms — local corpus, run via just model-corpus"]
fn every_corpus_model_loads() {
    let dir = corpus_dir();
    if !dir.exists() {
        eprintln!("no corpus at {}; skipping", dir.display());
        return;
    }
    let mut files = Vec::new();
    collect(&dir, &mut files);
    files.sort();
    assert!(!files.is_empty());
    for f in &files {
        let m = load(f);
        let name = f.strip_prefix(&dir).unwrap().display();
        for (i, b) in m.bones.iter().enumerate() {
            assert!(b.parent.is_none_or(|p| p < i), "{name}: parent after child");
        }
        let parts = bbmodel::mesh_parts(&m);
        for p in &parts {
            let finite = p.positions.iter().chain(&p.normals).flatten().chain(p.uvs.iter().flatten()).all(|v| v.is_finite());
            assert!(finite, "{name}: non-finite vertex data");
            assert!(p.indices.iter().all(|&i| (i as usize) < p.positions.len()), "{name}: index out of range");
        }
        for a in &m.animations {
            for k in 0..=20 {
                for (_, pose) in a.sample(a.length * k as f32 / 20.0) {
                    let v = pose.position.iter().chain(&pose.rotation).chain(&pose.scale);
                    assert!(v.clone().all(|x| x.is_finite()), "{name} / {}: non-finite pose", a.name);
                }
            }
        }
        println!("{name:64} bones {:3} parts {:3} anims {:2} {:?}", m.bones.len(), parts.len(), m.animations.len(), m.warnings);
    }
}

#[test]
#[ignore = "slow: >100ms — local corpus, run via just model-corpus"]
fn format_4_and_5_saves_of_one_model_agree() {
    // vengi ships the same model saved by Blockbench 4.5 and 5.0.3: the 5.0
    // groups[] layout and the animation axis flip must load identically.
    let dir = corpus_dir().join("vengi");
    let (a, b) = (dir.join("loy_s_goodies_female_template.bbmodel"), dir.join("loy_s_goodies_female_template_5_0_3.bbmodel"));
    if !a.exists() || !b.exists() {
        return;
    }
    let (a, b) = (load(&a), load(&b));
    let names = |m: &bbmodel::Model| m.bones.iter().map(|b| (b.name.clone(), b.origin, b.rotation)).collect::<Vec<_>>();
    assert_eq!(names(&a), names(&b));
    let (ba, bb) = (a.rest_bounds().unwrap(), b.rest_bounds().unwrap());
    for i in 0..3 {
        assert!((ba.0[i] - bb.0[i]).abs() < 1e-3 && (ba.1[i] - bb.1[i]).abs() < 1e-3, "bounds {ba:?} vs {bb:?}");
    }
    assert_eq!(a.animations.len(), b.animations.len());
    for (x, y) in a.animations.iter().zip(&b.animations) {
        for k in 0..=10 {
            let t = x.length * k as f32 / 10.0;
            let (px, py) = (x.sample(t), y.sample(t));
            assert_eq!(px.len(), py.len(), "{}", x.name);
            for ((bx, qx), (by, qy)) in px.iter().zip(&py) {
                assert_eq!(bx, by);
                for (u, v) in qx.rotation.iter().chain(&qx.position).zip(qy.rotation.iter().chain(&qy.position)) {
                    assert!((u - v).abs() < 1e-3, "{} t={t}: {qx:?} vs {qy:?}", x.name);
                }
            }
        }
    }
}
```

If `format_4_and_5_saves_of_one_model_agree` fails, **stop and diff the two files** before changing the loader. A re-save can legitimately change a model. Report the diff to the user instead of "fixing" the test.

- [ ] **Step 2: Add the justfile recipes.** Insert after the `model-census` recipe:

```just
# Loads every .bbmodel in the local corpus (target/test-models/, gitignored)
# through crates/bbmodel and checks its invariants; one line per file with
# its warnings. Skips when the corpus is absent.
model-corpus:
    {{cap}} {{cargo}} test -p bbmodel --test corpus -- --ignored --nocapture
```

In the `smoke` recipe, after the `server --lib` step, add:

```just
    step {{cargo}} test -q -p bbmodel --lib
```

- [ ] **Step 3: Run it**

Run: `just model-corpus`
Expected: both tests PASS, printing one line per model (31 `.bbmodel` files at the time of writing). Expect warnings for mesh (4 files), molang, bezier, effects, animated textures and hidden elements. **Any panic is a loader bug**: fix it in the task that owns the code and add a unit test that reproduces it.

- [ ] **Step 4: Checkpoint.** `git status --short crates/bbmodel justfile`. The justfile carries other sessions' changes too; don't touch those hunks.

---

### Task 7: Client: shared assets and a bone hierarchy per instance

**Files:**
- Modify: `crates/client/Cargo.toml` (`bbmodel = { workspace = true }` under `[dependencies]`)
- Create: `crates/client/src/renderer/model.rs`
- Modify: `crates/client/src/renderer/mod.rs` (`pub mod model;`, `.init_resource::<model::ModelLibrary>()`)

**Interfaces:**
- Consumes: `bbmodel::{parse, mesh_parts, Model, AlphaClass}`.
- Produces:
  - `LoadedModel { model: bbmodel::Model, parts: Vec<(usize, Handle<Mesh>, Handle<StandardMaterial>)> }` with `LoadedModel::build(model, &mut Assets<Mesh>, &mut Assets<StandardMaterial>, &mut Assets<Image>) -> LoadedModel` and `bone_rest(i) -> Transform`.
  - `ModelLibrary::get(&mut self, name: &'static str, meshes, materials, images) -> Arc<LoadedModel>`.
  - `EMBEDDED_MODELS: &[(&str, &[u8])]`.
  - `spawn_model(commands: &mut Commands, parent: Entity, model: &Arc<LoadedModel>) -> Entity`, which returns the root.
  - Components `ModelInstance { model: Arc<LoadedModel>, bones: Vec<Entity> }` and `ModelBone(pub usize)`.
  - `euler_zyx(deg: [f32; 3]) -> Quat`.
  - Constants `MODEL_ALPHA_CUTOFF`, `UNTEXTURED_COLOR`.
  - `EMBEDDED_MODELS` points at `assets/models/slime.bbmodel`, which Task 9 creates. **Do Task 9 Step 1 (generate the file) before this task's first build.**

- [ ] **Step 1: Write the failing tests** (bottom of `model.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Mesh>();
        app.init_asset::<StandardMaterial>();
        app.init_asset::<Image>();
        app.init_resource::<ModelLibrary>();
        app
    }

    fn slime(app: &mut App) -> Arc<LoadedModel> {
        app.world_mut().resource_scope(|world, mut lib: Mut<ModelLibrary>| {
            world.resource_scope(|world, mut meshes: Mut<Assets<Mesh>>| {
                world.resource_scope(|world, mut materials: Mut<Assets<StandardMaterial>>| {
                    let mut images = world.resource_mut::<Assets<Image>>();
                    lib.get("slime", &mut meshes, &mut materials, &mut images)
                })
            })
        })
    }

    #[test]
    fn embedded_models_parse() {
        for (name, bytes) in EMBEDDED_MODELS {
            let m = bbmodel::parse(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!bbmodel::mesh_parts(&m).is_empty(), "{name} draws nothing");
        }
    }

    #[test]
    fn euler_zyx_matches_bbmodel_math() {
        let deg = [30.0, -50.0, 70.0];
        let q = euler_zyx(deg);
        let m = bbmodel::math::euler_zyx_deg(deg);
        let v = [0.3, 0.5, -0.8];
        let a = q * Vec3::from(v);
        let b = bbmodel::math::mul_vec(m, v);
        assert!((a - Vec3::from(b)).length() < 1e-5, "{a} vs {b:?}");
    }

    #[test]
    fn spawn_builds_bones_and_parts_under_the_parent() {
        let mut app = test_app();
        let model = slime(&mut app);
        let parent = app.world_mut().spawn((Transform::default(), Visibility::default())).id();
        let root = {
            let world = app.world_mut();
            let mut queue = bevy::ecs::world::CommandQueue::default();
            let root = {
                let mut commands = Commands::new(&mut queue, world);
                spawn_model(&mut commands, parent, &model)
            };
            queue.apply(world);
            root
        };
        app.update();
        let inst = app.world().get::<ModelInstance>(root).unwrap();
        assert_eq!(inst.bones.len(), model.model.bones.len());
        assert_eq!(app.world().get::<ChildOf>(root).unwrap().parent(), parent);
        let meshes = app.world_mut().query::<&Mesh3d>().iter(app.world()).count();
        assert_eq!(meshes, model.parts.len());
    }

    #[test]
    fn instances_share_meshes_and_materials() {
        // Review Focus 5.
        let mut app = test_app();
        let a = slime(&mut app);
        let b = slime(&mut app);
        assert!(Arc::ptr_eq(&a, &b), "the library parses and uploads a model once");
        let images = app.world().resource::<Assets<Image>>().len();
        assert_eq!(images, a.model.textures.len());
    }

    #[test]
    fn blend_parts_get_blend_materials_and_everything_is_double_sided() {
        let mut app = test_app();
        let model = slime(&mut app);
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        let mut saw_blend = false;
        for (_, _, mat) in &model.parts {
            let m = materials.get(mat).unwrap();
            assert!(m.double_sided && m.cull_mode.is_none());
            saw_blend |= matches!(m.alpha_mode, AlphaMode::Blend);
        }
        assert!(saw_blend, "the slime's gel is translucent");
    }
}
```

- [ ] **Step 2: Run the tests; they fail**

Run (slow; this links the client test binary): `just test-one client "--lib renderer::model::"`
Expected: compile errors (the module is empty).

- [ ] **Step 3: Implement `model.rs`** (above the tests; copy the `render_resource` import path from `renderer/mod.rs` if it moved in the Bevy upgrade)

```rust
//! Blockbench models on screen. A parsed [`bbmodel::Model`] is uploaded once
//! as shared meshes/materials/images ([`LoadedModel`], cached by
//! [`ModelLibrary`]); each instance is a bone-entity hierarchy
//! ([`spawn_model`]) that [`animate_models`] poses. Model pixels are world
//! units — nothing here scales.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Alpha-cutout threshold for [`bbmodel::AlphaClass::Mask`] parts.
pub const MODEL_ALPHA_CUTOFF: f32 = 0.5;
/// Colour of faces on a model that has no usable texture.
pub const UNTEXTURED_COLOR: Color = Color::srgb(0.6, 0.6, 0.6);

/// Models compiled into the binary, by name: no asset fetch on wasm, and a
/// broken file fails `embedded_models_parse` instead of a live session.
pub const EMBEDDED_MODELS: &[(&str, &[u8])] =
    &[("slime", include_bytes!("../../../../assets/models/slime.bbmodel"))];

/// Blockbench Euler degrees (three.js 'ZYX': X applied first) as a Bevy quat.
pub fn euler_zyx(deg: [f32; 3]) -> Quat {
    Quat::from_euler(EulerRot::ZYX, deg[2].to_radians(), deg[1].to_radians(), deg[0].to_radians())
}

/// One model uploaded as shared assets; every instance reuses the handles.
pub struct LoadedModel {
    pub model: bbmodel::Model,
    /// (bone index, mesh, material): one per bone × texture × alpha class.
    pub parts: Vec<(usize, Handle<Mesh>, Handle<StandardMaterial>)>,
}

impl LoadedModel {
    pub fn build(
        model: bbmodel::Model,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
    ) -> Self {
        let image_handles: Vec<Option<Handle<Image>>> = model
            .textures
            .iter()
            .map(|t| t.image.as_ref().map(|img| images.add(texture_image(img))))
            .collect();
        let mut material_cache: HashMap<(Option<usize>, bbmodel::AlphaClass), Handle<StandardMaterial>> = HashMap::new();
        let parts = bbmodel::mesh_parts(&model)
            .into_iter()
            .map(|p| {
                let material = material_cache
                    .entry((p.texture, p.alpha))
                    .or_insert_with(|| {
                        let image = p.texture.and_then(|t| image_handles.get(t).cloned().flatten());
                        let emissive = p.texture.and_then(|t| model.textures.get(t)).is_some_and(|t| t.emissive);
                        materials.add(model_material(image, p.alpha, emissive))
                    })
                    .clone();
                let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, p.positions)
                    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, p.normals)
                    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, p.uvs)
                    .with_inserted_indices(Indices::U32(p.indices));
                (p.bone, meshes.add(mesh), material)
            })
            .collect();
        Self { model, parts }
    }

    /// Rest-pose local transform of bone `i`: at its pivot relative to its
    /// parent's pivot, rotated by its own Euler angles.
    pub fn bone_rest(&self, i: usize) -> Transform {
        let b = &self.model.bones[i];
        let parent_origin = b.parent.map_or([0.0; 3], |p| self.model.bones[p].origin);
        Transform {
            translation: Vec3::from(b.origin) - Vec3::from(parent_origin),
            rotation: euler_zyx(b.rotation),
            scale: Vec3::ONE,
        }
    }
}

fn texture_image(img: &image::RgbaImage) -> Image {
    let mut image = Image::new(
        Extent3d { width: img.width(), height: img.height(), depth_or_array_layers: 1 },
        TextureDimension::D2,
        img.as_raw().clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    // Pixel art: nearest filtering, and clamp (textures may be any size).
    image.sampler = ImageSampler::nearest();
    image
}

fn model_material(image: Option<Handle<Image>>, alpha: bbmodel::AlphaClass, emissive: bool) -> StandardMaterial {
    StandardMaterial {
        base_color: if image.is_some() { Color::WHITE } else { UNTEXTURED_COLOR },
        base_color_texture: image,
        alpha_mode: match alpha {
            bbmodel::AlphaClass::Opaque => AlphaMode::Opaque,
            bbmodel::AlphaClass::Mask => AlphaMode::Mask(MODEL_ALPHA_CUTOFF),
            bbmodel::AlphaClass::Blend => AlphaMode::Blend,
        },
        // Blockbench draws faces double-sided, and mob models are full of
        // zero-thickness planes (ears, fins, wings).
        double_sided: true,
        cull_mode: None,
        perceptual_roughness: 1.0,
        unlit: emissive,
        ..default()
    }
}

/// Embedded models, parsed and uploaded on first use.
#[derive(Resource, Default)]
pub struct ModelLibrary {
    loaded: HashMap<&'static str, Arc<LoadedModel>>,
}

impl ModelLibrary {
    /// The embedded model `name`. Panics on an unknown name or a file that
    /// fails to parse — both are build-time assets (`embedded_models_parse`).
    pub fn get(
        &mut self,
        name: &'static str,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
    ) -> Arc<LoadedModel> {
        self.loaded
            .entry(name)
            .or_insert_with(|| {
                let (_, bytes) = EMBEDDED_MODELS
                    .iter()
                    .find(|(n, _)| *n == name)
                    .unwrap_or_else(|| panic!("no embedded model {name:?}"));
                let model = bbmodel::parse(bytes).unwrap_or_else(|e| panic!("embedded model {name:?}: {e}"));
                for w in &model.warnings {
                    warn!("model {name}: {w}");
                }
                Arc::new(LoadedModel::build(model, meshes, materials, images))
            })
            .clone()
    }
}

/// The root of one spawned model.
#[derive(Component)]
pub struct ModelInstance {
    pub model: Arc<LoadedModel>,
    /// Bone entities, indexed like `model.model.bones`.
    pub bones: Vec<Entity>,
}

/// A bone entity: index into its model's `bones`.
#[derive(Component)]
pub struct ModelBone(pub usize);

/// Spawn `model` under `parent`: a root carrying [`ModelInstance`], one entity
/// per bone at its rest transform (parented as in Blockbench's outliner) and
/// one mesh entity per part under its bone. Returns the root.
pub fn spawn_model(commands: &mut Commands, parent: Entity, model: &Arc<LoadedModel>) -> Entity {
    let root = commands.spawn((Transform::default(), Visibility::default(), ChildOf(parent))).id();
    let mut bones = Vec::with_capacity(model.model.bones.len());
    for (i, b) in model.model.bones.iter().enumerate() {
        let parent_e = b.parent.map_or(root, |p| bones[p]);
        bones.push(commands.spawn((model.bone_rest(i), Visibility::default(), ModelBone(i), ChildOf(parent_e))).id());
    }
    for (bone, mesh, material) in &model.parts {
        commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone()), Transform::default(), ChildOf(bones[*bone])));
    }
    commands.entity(root).insert(ModelInstance { model: model.clone(), bones });
    root
}
```

In `renderer/mod.rs`, add `pub mod model;` to the module list and `.init_resource::<model::ModelLibrary>()` next to `.init_resource::<avatar::AvatarAssets>()`.

- [ ] **Step 4: Run the tests; they pass**

Run: `just test-one client "--lib renderer::model::"`
Expected: PASS for all tests.

- [ ] **Step 5: Checkpoint.** `git status --short crates/client`. Expect `Cargo.toml`, `renderer/model.rs` and `renderer/mod.rs` from this task, alongside other sessions' changes. Leave those alone.

---

### Task 8: Client animation playback

**Files:**
- Modify: `crates/client/src/renderer/model.rs`
- Modify: `crates/client/src/renderer/mod.rs` (add `model::animate_models` to the Update systems tuple, next to `avatar::attach_avatars`)

**Interfaces:**
- Consumes: `ModelInstance`, `ModelBone`, `LoadedModel::bone_rest`, `euler_zyx` (Task 7); `Animation::sample`, `BonePose` (Task 5).
- Produces:
  - Component `ModelAnimator { animation: Option<usize>, time: f32 }` with `play(&mut self, i)`.
  - `posed_bone(model: &LoadedModel, i: usize, pose: Option<bbmodel::BonePose>) -> Transform`.
  - System `animate_models`.

- [ ] **Step 1: Write the failing tests** (append to `model::tests`)

```rust
    #[test]
    fn posed_bone_adds_offsets_to_the_rest_pose() {
        let mut app = test_app();
        let model = slime(&mut app);
        let i = model.model.bones.len() - 1;
        let rest = model.bone_rest(i);
        assert_eq!(posed_bone(&model, i, None), rest);
        let pose = bbmodel::BonePose { position: [1.0, 2.0, 3.0], rotation: [0.0, 90.0, 0.0], scale: [2.0, 2.0, 2.0] };
        let posed = posed_bone(&model, i, Some(pose));
        assert_eq!(posed.translation, rest.translation + Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(posed.scale, Vec3::splat(2.0));
        let b = &model.model.bones[i];
        let want = euler_zyx([b.rotation[0], b.rotation[1] + 90.0, b.rotation[2]]);
        assert!(posed.rotation.angle_between(want) < 1e-5);
    }

    #[test]
    fn animator_without_a_clip_leaves_bones_at_rest() {
        let mut app = test_app();
        app.add_systems(Update, animate_models);
        let model = slime(&mut app);
        let parent = app.world_mut().spawn((Transform::default(), Visibility::default())).id();
        let root = {
            let world = app.world_mut();
            let mut queue = bevy::ecs::world::CommandQueue::default();
            let root = {
                let mut commands = Commands::new(&mut queue, world);
                spawn_model(&mut commands, parent, &model)
            };
            queue.apply(world);
            root
        };
        app.world_mut().entity_mut(root).insert(ModelAnimator::default());
        app.update();
        app.update();
        let inst = app.world().get::<ModelInstance>(root).unwrap();
        for (i, &e) in inst.bones.iter().enumerate() {
            assert_eq!(*app.world().get::<Transform>(e).unwrap(), model.bone_rest(i));
        }
    }
```

- [ ] **Step 2: Run the tests; they fail**

Run: `just test-one client "--lib renderer::model::"`
Expected: compile errors (`posed_bone`, `ModelAnimator` and `animate_models` are missing).

- [ ] **Step 3: Implement** (in `model.rs`, above the tests)

```rust
/// Plays one of a model's animations on its [`ModelInstance`] root.
#[derive(Component, Debug, Default)]
pub struct ModelAnimator {
    /// Index into the model's `animations`; `None` = rest pose.
    pub animation: Option<usize>,
    /// Seconds since the clip started.
    pub time: f32,
}

impl ModelAnimator {
    pub fn play(&mut self, animation: usize) {
        self.animation = Some(animation);
        self.time = 0.0;
    }
}

/// Bone `i` with an animation pose applied the way Blockbench does: position
/// and Euler rotation add to the rest pose, scale multiplies.
pub fn posed_bone(model: &LoadedModel, i: usize, pose: Option<bbmodel::BonePose>) -> Transform {
    let rest = model.bone_rest(i);
    let Some(p) = pose else { return rest };
    let r = model.model.bones[i].rotation;
    Transform {
        translation: rest.translation + Vec3::from(p.position),
        rotation: euler_zyx([r[0] + p.rotation[0], r[1] + p.rotation[1], r[2] + p.rotation[2]]),
        scale: Vec3::from(p.scale),
    }
}

/// Advance every [`ModelAnimator`] and pose its bones (rest pose first, so a
/// bone the clip doesn't animate — or a finished clip — returns to rest).
pub fn animate_models(
    time: Res<Time>,
    mut instances: Query<(&ModelInstance, &mut ModelAnimator)>,
    mut bones: Query<&mut Transform, With<ModelBone>>,
) {
    for (inst, mut animator) in &mut instances {
        animator.time += time.delta_secs();
        let mut poses = vec![None; inst.bones.len()];
        if let Some(clip) = animator.animation.and_then(|a| inst.model.model.animations.get(a)) {
            for (bone, pose) in clip.sample(animator.time) {
                if let Some(slot) = poses.get_mut(bone) {
                    *slot = Some(pose);
                }
            }
        }
        for (i, &e) in inst.bones.iter().enumerate() {
            if let Ok(mut tf) = bones.get_mut(e) {
                tf.set_if_neq(posed_bone(&inst.model, i, poses[i]));
            }
        }
    }
}
```

In `renderer/mod.rs`, add `model::animate_models,` to the Update tuple that holds `avatar::attach_avatars`. If Bevy complains that the tuple is too large, nest it into a sub-tuple, as that tuple already does.

- [ ] **Step 4: Run the tests; they pass**

Run: `just test-one client "--lib renderer::model::"`
Expected: PASS for all tests.

- [ ] **Step 5: Checkpoint.** `git status --short crates/client`.

---

### Task 9: Slime model file and switching the slime body over

**Before starting:** `renderer/avatar.rs` belongs to the slime work, which another session was editing while this plan was written. Run `git diff --stat crates/client/src/renderer/avatar.rs` and ask the user whether it's stable before editing it.

**Files:**
- Create: `scripts/make_slime_bbmodel.py`, `assets/models/slime.bbmodel` (generated)
- Modify: `crates/client/src/renderer/avatar.rs`

**Interfaces:**
- Consumes: `ModelLibrary::get`, `spawn_model` (Task 7); `game::slime::slime_edge_units`.
- Produces:
  - `pub const SLIME_MODEL_EDGE_PX: f32 = 8.0` in avatar.rs.
  - The slime body is the model, under the existing `SlimeSquish` pivot.

- [ ] **Step 1: Write and run the generator**

`scripts/make_slime_bbmodel.py`:

```python
#!/usr/bin/env python3
"""Write assets/models/slime.bbmodel: our own slime.

Cube layout follows Minecraft's slime proportions (an 8 px translucent gel
cube around a 6 px opaque core, two eyes and a mouth on the north face, -Z =
the sim's forward); the 64x32 texture is original and generated here. Stdlib
only. Re-run after editing; once the model is hand-tuned in Blockbench, edit
the .bbmodel instead and delete this script's claim on it.
"""
import base64
import json
import pathlib
import struct
import uuid
import zlib

W, H = 64, 32
GEL, GEL_EDGE = (110, 205, 95, 150), (85, 175, 70, 185)
CORE, CORE_EDGE = (80, 165, 62, 255), (62, 138, 50, 255)
FACE = (22, 42, 22, 255)
NS = uuid.UUID("5b1e0000-0000-4000-8000-000000000000")

px = [[(0, 0, 0, 0)] * W for _ in range(H)]


def box_uv_rects(u, v, x, y, z):
    """The six face rects (x0, y0, w, h) of Blockbench's box-UV layout."""
    return [(u, v + z, z, y), (u + z + x, v + z, z, y), (u + z, v, x, z),
            (u + z + x, v, x, z), (u + 2 * z + x, v + z, x, y), (u + z, v + z, x, y)]


def paint(u, v, s, fill, edge):
    for x0, y0, w, h in box_uv_rects(u, v, s, s, s):
        for yy in range(y0, y0 + h):
            for xx in range(x0, x0 + w):
                border = xx in (x0, x0 + w - 1) or yy in (y0, y0 + h - 1)
                px[yy][xx] = edge if border else fill


paint(0, 0, 8, GEL, GEL_EDGE)
paint(0, 16, 6, CORE, CORE_EDGE)
for u, v, s in [(32, 0, 2), (32, 4, 2), (32, 8, 1)]:
    paint(u, v, s, FACE, FACE)


def png(rows):
    raw = b"".join(b"\x00" + bytes(c for p in row for c in p) for row in rows)

    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def cube(name, frm, to, uv):
    face = {"uv": [0, 0, 0, 0], "texture": 0}
    return {"name": name, "type": "cube", "uuid": str(uuid.uuid5(NS, name)), "box_uv": True,
            "from": frm, "to": to, "origin": [0, 0, 0], "uv_offset": uv,
            "faces": {f: dict(face) for f in ("north", "east", "south", "west", "up", "down")}}


elements = [
    cube("gel", [-4, 0, -4], [4, 8, 4], [0, 0]),
    cube("core", [-3, 1, -3], [3, 7, 3], [0, 16]),
    cube("eye_right", [-3.3, 4, -3.5], [-1.3, 6, -1.5], [32, 0]),
    cube("eye_left", [1.3, 4, -3.5], [3.3, 6, -1.5], [32, 4]),
    cube("mouth", [-1, 2, -3.5], [0, 3, -2.5], [32, 8]),
]
body = str(uuid.uuid5(NS, "body"))
model = {
    "meta": {"format_version": "5.0", "model_format": "free", "box_uv": True},
    "name": "slime",
    "resolution": {"width": W, "height": H},
    "elements": elements,
    "groups": [{"name": "body", "uuid": body, "origin": [0, 0, 0], "rotation": [0, 0, 0]}],
    "outliner": [{"uuid": body, "isOpen": True, "children": [e["uuid"] for e in elements]}],
    "textures": [{"name": "slime.png", "uuid": str(uuid.uuid5(NS, "slime.png")), "width": W, "height": H,
                  "uv_width": W, "uv_height": H, "render_mode": "default", "internal": True,
                  "source": "data:image/png;base64," + base64.b64encode(png(px)).decode()}],
    "animations": [],
}
out = pathlib.Path(__file__).resolve().parent.parent / "assets/models/slime.bbmodel"
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(json.dumps(model, indent=1) + "\n")
print(f"wrote {out}")
```

Run: `python3 scripts/make_slime_bbmodel.py && just model-census assets/models`
Expected: `wrote .../assets/models/slime.bbmodel`. The census row reads `free 5.0`, 1 group, 5 cubes, 1 texture 64x32, and lists `alpha_partial`, `box_uv` and `frac` (the eyes).

- [ ] **Step 2: Write the failing avatar test** (append to `avatar.rs` tests)

Also update `test_app()` in `avatar.rs` to add `app.init_asset::<Image>(); app.init_resource::<crate::renderer::model::ModelLibrary>();`.

```rust
    #[test]
    fn slime_model_matches_the_sim_body() {
        let model = bbmodel::parse(
            crate::renderer::model::EMBEDDED_MODELS.iter().find(|(n, _)| *n == "slime").unwrap().1,
        )
        .unwrap();
        let (lo, hi) = model.rest_bounds().unwrap();
        // The pivot scales SLIME_MODEL_EDGE_PX to the sim edge: the model's
        // gel cube must be exactly that wide, sitting on y = 0 (the feet).
        assert!((hi[0] - lo[0] - SLIME_MODEL_EDGE_PX).abs() < 1e-3, "{lo:?}..{hi:?}");
        assert!(lo[1].abs() < 1e-3);
    }

    #[test]
    fn slime_body_is_the_model() {
        let mut app = test_app();
        let e = app.world_mut().spawn(SimKind(EntityKind::Slime { size: 1 })).id();
        app.update();
        let pivot = app.world().entity(e).get::<Children>().unwrap()[0];
        let root = app.world().entity(pivot).get::<Children>().unwrap()[0];
        assert!(app.world().get::<crate::renderer::model::ModelInstance>(root).is_some());
    }
```

Run: `just test-one client "--lib renderer::avatar::"`
Expected: FAIL (`SLIME_MODEL_EDGE_PX` is missing, and the body is still unit cubes).

- [ ] **Step 3: Switch the slime body to the model** (`avatar.rs`)

- Delete `SlimeAssets`, `SLIME_PARTS` and the gel/core/face materials.
- Change the `AvatarAssets.slime` field to `slime: Option<Arc<crate::renderer::model::LoadedModel>>`.
- Add `use std::sync::Arc;` and `use crate::renderer::model::{self, ModelLibrary};`.
- Add `mut images: ResMut<Assets<Image>>, mut models: ResMut<ModelLibrary>` to `attach_avatars`.

The slime arm becomes:

```rust
            EntityKind::Slime { size } => {
                let slime = assets
                    .slime
                    .get_or_insert_with(|| models.get("slime", &mut meshes, &mut materials, &mut images))
                    .clone();
                spawn_slime_body(&mut commands, e, size, &slime);
            }
```

Replace `spawn_slime_body` with:

```rust
/// Width of the slime model's gel cube in model pixels (assets/models/
/// slime.bbmodel); the pivot scales it to the sim's edge.
pub const SLIME_MODEL_EDGE_PX: f32 = 8.0;

fn spawn_slime_body(commands: &mut Commands, e: Entity, size: u8, slime: &Arc<model::LoadedModel>) {
    let edge = game::slime::slime_edge_units(size);
    commands.entity(e).insert(SlimeAvatar);
    let pivot = commands
        .spawn((
            // The sim pose is the body centre; the pivot sits at the feet.
            Transform::from_xyz(0.0, -edge / 2.0, 0.0).with_scale(Vec3::splat(edge / SLIME_MODEL_EDGE_PX)),
            Visibility::default(),
            SlimeSquish::new(size),
            ChildOf(e),
        ))
        .id();
    model::spawn_model(commands, pivot, slime);
}
```

In `squish_slimes`, change the scale line to:

```rust
        tf.set_if_neq(Transform { scale: squish.scale() * edge / SLIME_MODEL_EDGE_PX, ..*tf });
```

Update the `SLIME_PARTS` doc comment's replacement, and keep the note that the face is on −Z (north), the sim's forward.

- [ ] **Step 4: Run the avatar and model tests; they pass**

Run: `just test-one client "--lib renderer::avatar::"` then `just test-one client "--lib renderer::model::"`
Expected: PASS. That includes the existing `slime_gets_a_body_once` and the squish tests.

- [ ] **Step 5: Checkpoint.** `git status --short assets/models scripts crates/client`.

---

### Task 10: Model viewer, docs, final verification

**Files:**
- Create: `crates/client/examples/model_viewer.rs`
- Modify: `justfile` (`model-view` recipe), `CLAUDE.md` (workspace layout + recipes), `docs/superpowers/specs/2026-09-22-blockbench-model-loading-investigation.md` (Status line)

**Interfaces:**
- Consumes: `bbmodel::parse`, `LoadedModel::build`, `spawn_model`, `ModelAnimator`, `animate_models`, `ModelInstance` (Tasks 7–8).
- Produces: `just model-view [--shots <dir>] [paths...]`.

- [ ] **Step 1: Write the viewer**

```rust
//! Look at Blockbench models: `just model-view [--shots <dir>] [paths...]`
//! (files or directories; default target/test-models). Left/Right: previous
//! / next model; Up/Down: cycle its animations. With `--shots <dir>` every
//! model renders once from a fixed 3/4 view into <dir>/<n>_<name>.png and
//! the app exits — for eyeballing a whole corpus against Blockbench.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use client::renderer::model::{animate_models, spawn_model, LoadedModel, ModelAnimator};

/// Frames to wait after spawning before a screenshot (asset upload).
const SHOT_SETTLE_FRAMES: u32 = 8;
/// Frames between a screenshot and the next model (the save is async).
const SHOT_GAP_FRAMES: u32 = 4;

#[derive(Resource)]
struct Viewer {
    files: Vec<PathBuf>,
    current: usize,
    shown: Option<Entity>,
    shots: Option<PathBuf>,
    frames: u32,
    dirty: bool,
}

#[derive(Component)]
struct Anchor;
#[derive(Component)]
struct Label;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let shots = args.iter().position(|a| a == "--shots").map(|i| {
        let dir = PathBuf::from(args.get(i + 1).expect("--shots <dir>"));
        args.drain(i..=i + 1);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    });
    if args.is_empty() {
        args.push("target/test-models".into());
    }
    let mut files = Vec::new();
    for a in &args {
        collect(PathBuf::from(a), &mut files);
    }
    files.sort();
    assert!(!files.is_empty(), "no .bbmodel files under {args:?}");
    App::new()
        .add_plugins(DefaultPlugins)
        .insert_resource(Viewer { files, current: 0, shown: None, shots, frames: 0, dirty: true })
        .add_systems(Startup, setup)
        .add_systems(Update, (keys, show, animate_models, spin, shots).chain())
        .run();
}

fn collect(p: PathBuf, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
            collect(e.path(), out);
        }
    } else if p.extension().is_some_and(|e| e == "bbmodel") {
        out.push(p);
    }
}

fn setup(mut commands: Commands) {
    commands.spawn((Camera3d::default(), Transform::from_xyz(0.0, 0.0, 60.0).looking_at(Vec3::ZERO, Vec3::Y)));
    commands.spawn((DirectionalLight::default(), Transform::default().looking_at(Vec3::new(-1.0, -2.0, -1.5), Vec3::Y)));
    commands.spawn((Anchor, Transform::default(), Visibility::default()));
    commands.spawn((Label, Text::new(""), Node { position_type: PositionType::Absolute, left: Val::Px(8.0), top: Val::Px(8.0), ..default() }));
}

fn keys(input: Res<ButtonInput<KeyCode>>, mut viewer: ResMut<Viewer>, mut anims: Query<&mut ModelAnimator>) {
    let n = viewer.files.len();
    if input.just_pressed(KeyCode::ArrowRight) {
        viewer.current = (viewer.current + 1) % n;
        viewer.dirty = true;
    }
    if input.just_pressed(KeyCode::ArrowLeft) {
        viewer.current = (viewer.current + n - 1) % n;
        viewer.dirty = true;
    }
    if input.just_pressed(KeyCode::ArrowUp) || input.just_pressed(KeyCode::ArrowDown) {
        for mut a in &mut anims {
            let next = a.animation.map_or(0, |i| i + 1);
            a.play(next); // past the last clip: animation(len) samples nothing → rest pose
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn show(
    mut commands: Commands,
    mut viewer: ResMut<Viewer>,
    anchor: Single<Entity, With<Anchor>>,
    mut camera: Single<&mut Transform, With<Camera3d>>,
    mut label: Single<&mut Text, With<Label>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    if !viewer.dirty {
        return;
    }
    viewer.dirty = false;
    viewer.frames = 0;
    if let Some(old) = viewer.shown.take() {
        commands.entity(old).despawn();
    }
    let path = viewer.files[viewer.current].clone();
    let model = match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| bbmodel::parse(&b).map_err(|e| e.to_string())) {
        Ok(m) => m,
        Err(e) => {
            label.0 = format!("{}: {e}", path.display());
            return;
        }
    };
    let (lo, hi) = model.rest_bounds().unwrap_or(([-8.0; 3], [8.0; 3]));
    let centre = (Vec3::from(lo) + Vec3::from(hi)) / 2.0;
    let radius = (Vec3::from(hi) - Vec3::from(lo)).length().max(1.0) / 2.0;
    // 3/4 view from the front (north, −Z) left, a little above.
    **camera = Transform::from_translation(centre + Vec3::new(-0.8, 0.6, -1.2).normalize() * radius * 2.6).looking_at(centre, Vec3::Y);
    label.0 = format!(
        "[{}/{}] {}\nbones {}  animations {}  warnings {}\n{}",
        viewer.current + 1, viewer.files.len(), path.display(),
        model.bones.len(), model.animations.len(), model.warnings.len(), model.warnings.join("\n"),
    );
    let loaded = Arc::new(LoadedModel::build(model, &mut meshes, &mut materials, &mut images));
    let root = spawn_model(&mut commands, *anchor, &loaded);
    commands.entity(root).insert(ModelAnimator::default());
    viewer.shown = Some(root);
}

fn spin(time: Res<Time>, viewer: Res<Viewer>, mut anchor: Single<&mut Transform, With<Anchor>>) {
    if viewer.shots.is_none() {
        anchor.rotate_y(time.delta_secs() * 0.5);
    }
}

fn shots(mut commands: Commands, mut viewer: ResMut<Viewer>, mut exit: MessageWriter<AppExit>) {
    let Some(dir) = viewer.shots.clone() else { return };
    viewer.frames += 1;
    if viewer.frames == SHOT_SETTLE_FRAMES {
        let stem = viewer.files[viewer.current].file_stem().unwrap().to_string_lossy().into_owned();
        let path = dir.join(format!("{:02}_{stem}.png", viewer.current));
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if viewer.frames == SHOT_SETTLE_FRAMES + SHOT_GAP_FRAMES {
        if viewer.current + 1 == viewer.files.len() {
            exit.write(AppExit::Success);
        } else {
            viewer.current += 1;
            viewer.dirty = true;
        }
    }
}
```

`client/Cargo.toml` needs `bbmodel` as a regular dependency, which Task 7 added, so the example can `use bbmodel`. If `MessageWriter<AppExit>` or `Single` differs in the pinned Bevy, copy whatever `crates/client/examples/gizmo_smoke.rs` uses.

- [ ] **Step 2: Add the recipe** (next to `model-corpus`)

```just
# Blockbench model viewer (files or dirs; default target/test-models).
# Left/Right: model, Up/Down: animation. `--shots <dir>` renders every model
# to <dir>/<n>_<name>.png and exits. Honours adapter=… like the client.
model-view *args:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cargo}} run -p client --example model_viewer -- {{args}}
```

- [ ] **Step 3: Type-check the example**

Run: `nice -n 10 taskset -c 0-15 cargo check -p client --example model_viewer`
Expected: no errors.

- [ ] **Step 4: Visual check (ask the user first: it opens a window on their desktop)**

Run: `just model-view --shots target/model-shots assets/models target/test-models`
Then read a sample of the PNGs with the Read tool:
- `00_slime.png`: a translucent green cube with a visible core and a face on the camera side.
- The hephaestus `correct_{x,y,z}_axis_rotation_test` shots.
- `jumping_spider`, `CrocodileModel`, `evangelist`, `loy_s_goodies_female_template` and its `_5_0_3` twin, which must look identical.

Report anything that looks wrong (mirrored faces, scrambled UVs, bones in the wrong place) to the user with the image path. Do not tune blind.

- [ ] **Step 5: Docs**

In `CLAUDE.md`, under "Workspace Layout", add after the `crates/worldgen` bullet:

```markdown
- `crates/bbmodel` — Blockbench `.bbmodel` loader, Bevy-free (edition 2024): `parse` (formats 3.x–5.x: inline-outliner and `groups[]` layouts, pre-5.0 animation axis flip), `mesh_parts` (cubes → triangles exactly as Blockbench's preview: `setShape` vertex order, box/per-face UV, mirror, inflate/stretch, per-face alpha class), `Animation::sample` (linear/step/catmullrom/pre-post; bezier → linear, molang → 0, both warn). 1 Blockbench pixel = 1 world unit. The client side is `renderer/model.rs` (`ModelLibrary` of `include_bytes!` models in `EMBEDDED_MODELS`, `spawn_model`, `ModelAnimator`); the slime renders `assets/models/slime.bbmodel` (generated by `scripts/make_slime_bbmodel.py`). A local test corpus of real mob models lives in `target/test-models/` (gitignored, mostly All Rights Reserved — never commit it; `MANIFEST.tsv` lists sources): `just model-census` (feature matrix), `just model-corpus` (loader invariants), `just model-view [--shots <dir>]` (viewer). Spec + plan: `docs/superpowers/{specs,plans}/2026-09-22-blockbench-model-loading*`.
```

In the spec, change the status line to: `Status: **implemented (v1)** — see docs/superpowers/plans/2026-09-22-blockbench-model-loading.md; deferred: molang, bezier, mesh elements, Bedrock .geo.json front end, animated-texture playback.`

- [ ] **Step 6: Final verification**

Run, in order:
1. `just test-one bbmodel "--lib"`: PASS.
2. `just model-corpus`: PASS.
3. `just test-one client "--lib renderer::model::"` and `just test-one client "--lib renderer::avatar::"`: PASS.
4. `just smoke`: `smoke ok`.
5. `just clippy`: no new warnings in `bbmodel` or `renderer/model.rs`.

- [ ] **Step 7: Offer the commit (do not commit unasked)**

List the files this plan created or changed:
- `crates/bbmodel/**`, `crates/client/src/renderer/model.rs`, `crates/client/examples/model_viewer.rs`, `scripts/make_slime_bbmodel.py`, `scripts/bbmodel_census.py`, `assets/models/slime.bbmodel`, and the plan and spec docs.
- The **hunks** this plan made in shared dirty files: `Cargo.toml`, `Cargo.lock`, `justfile`, `CLAUDE.md`, `crates/client/Cargo.toml`, `renderer/mod.rs`, `renderer/avatar.rs`.

Ask the user how they want it committed. Those shared files also hold other sessions' uncommitted work, such as the Bevy 0.19 bump in `Cargo.toml`, so they can't simply be `git add`ed whole.

---

## Deferred (not in this plan)

- **Molang evaluation**, with named `variable.*` inputs fed by the entity's renderer. About 130 of the corpus's expressions read game-set variables. This is its own design.
- **Bezier keyframes**: need `bezier_left/right_time/value` and the 5.0 handle flip.
- **Mesh elements**: triangulate tris and quads with Blockbench's quad vertex sorting.
- **Bedrock `.geo.json` + `.animation.json` front end**: X mirrored vs Blockbench. It feeds the same `Model`.
- **Animated-texture playback**: frame cycling via a UV offset per material.
- **Model → `ChiselData`** (furniture): `game` depends on `bbmodel`; refuse off-grid or rotated cubes.
