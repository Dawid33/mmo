# Data-Driven Block Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hardcoded `BlockKind`/`VoxelType` enums with a single numeric `BlockId` assigned by a runtime RON manifest (`assets/blocks/blocks.ron`), so adding a textured block is an asset + manifest edit with per-face texture support and a guaranteed missing-texture fallback.

**Architecture:** A new `BlockRegistry` in the Bevy-free `game` crate parses the RON manifest into explicit-id `BlockDef`s. `BlockId` becomes the single material identity at both block and voxel granularity; the sim keeps "non-air = solid". The client builds an array texture and a `BlockId → [top,side,bottom]` layer map from the registry (layer 0 reserved as a procedurally generated magenta/black checkerboard); the mesher writes per-face layer indices into the existing `tex_idx` vertex attribute, which the shader already samples by face normal.

**Tech Stack:** Rust (game/server edition 2021), Bevy 0.18 (client), `ron` 0.12 (manifest), `image` 0.24 (textures), `bincode`/`crc32fast` (wire + hash), `serde`.

**Spec:** `docs/superpowers/specs/2026-07-06-data-driven-block-registry-design.md`

## Global Constraints

- `game` and `server` stay Bevy-free and windowing-free (shared by client + server).
- Determinism is bit-exact: `hash(before) == hash(after undo)`; client and server must agree on every serialized value. `BlockId` is serialized (`SetVoxelComponent(Vec<Voxel>)`) and hashed (crc32) — its representation must be identical on both sides.
- Block ids are **explicit and stable** in the manifest; reordering the file must never renumber an existing block (parked `SerializedRegion` blobs and the wire embed the id).
- `BlockId` is `u16`; `BlockId::AIR = BlockId(0)` reserved.
- Vendored forks under `crates/{nalgebra,simba,parry,rapier,approx,ordered-float,slotmapd,block-mesh}` are not modified.
- Manifest format is RON via `ron = "0.12"` (already in `Cargo.lock`).
- Field names: `Block.id`, `Voxel.block`.
- Build/test with stable `cargo` (no cranelift required for CI correctness): `cargo test -p <crate>`.

---

### Task 1: Block registry foundation (`game`)

Pure addition — introduces `BlockId`, the registry types, the RON parser, and the shipped manifest. Nothing consumes them yet, so `game` stays green.

**Files:**
- Create: `crates/game/src/registry.rs`
- Create: `assets/blocks/blocks.ron`
- Modify: `crates/game/src/lib.rs` (add `pub mod registry;` + glob re-export)
- Modify: `crates/game/Cargo.toml` (add `ron`)

**Interfaces:**
- Produces:
  - `pub struct BlockId(pub u16)` with `pub const AIR: BlockId = BlockId(0)`; derives `Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Debug, Serialize, Deserialize` (Default = `AIR`).
  - `pub enum TextureSpec { Untextured, All(String), Faces { top: String, side: String, bottom: String } }` — `Clone, Debug, PartialEq, Eq, Serialize, Deserialize`.
  - `pub struct BlockDef { pub id: u16, pub name: String, pub textures: TextureSpec }`.
  - `pub struct BlockRegistry` with `pub fn from_ron(src: &str) -> Result<Self, RegistryError>`, `pub fn id_of(&self, name: &str) -> Option<BlockId>`, `pub fn def(&self, id: BlockId) -> Option<&BlockDef>`, `pub fn iter(&self) -> impl Iterator<Item = (BlockId, &BlockDef)>` (ascending id order).
  - `pub enum RegistryError { Parse(String), DuplicateId(u16), DuplicateName(String), MissingAir, BadAir }`.

- [ ] **Step 1: Add the `ron` dependency**

In `crates/game/Cargo.toml`, under `[dependencies]`, add next to the other serde deps:

```toml
ron = "0.12"
```

- [ ] **Step 2: Create the manifest `assets/blocks/blocks.ron`**

```ron
// Block registry. `id` is explicit and STABLE — never renumber an existing
// block; only append. id 0 must be `air` / Untextured. Texture paths are
// relative to assets/blocks/. Faces = per-face (top/side/bottom); the shader
// picks the slot by face normal. Missing textures render as the layer-0
// magenta/black "missing texture" checkerboard.
(
    blocks: [
        (id: 0, name: "air",   textures: Untextured),
        (id: 1, name: "dirt",  textures: All("dirt.png")),
        (id: 2, name: "stone", textures: All("dirt.png")), // placeholder art until stone.png exists
        // Per-face example — drop grass_top.png / grass_side.png into
        // assets/blocks/ and uncomment:
        // (id: 3, name: "grass", textures: Faces(top: "grass_top.png", side: "grass_side.png", bottom: "dirt.png")),
    ],
)
```

- [ ] **Step 3: Write the failing tests for `registry.rs`**

Create `crates/game/src/registry.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_id_is_reserved_zero() {
        assert_eq!(BlockId::AIR, BlockId(0));
        assert_eq!(BlockId::default(), BlockId::AIR);
    }

    #[test]
    fn parses_shipped_manifest() {
        let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron"))
            .expect("shipped manifest must parse");
        assert_eq!(reg.id_of("air"), Some(BlockId::AIR));
        assert!(reg.id_of("dirt").is_some());
        assert!(reg.id_of("stone").is_some());
        assert_eq!(reg.def(BlockId::AIR).unwrap().name, "air");
    }

    #[test]
    fn iter_is_ascending_by_id() {
        let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
        let ids: Vec<u16> = reg.iter().map(|(id, _)| id.0).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "iter must yield ascending ids");
    }

    #[test]
    fn parses_per_face_textures() {
        let src = r#"(blocks:[
            (id:0,name:"air",textures:Untextured),
            (id:1,name:"grass",textures:Faces(top:"t.png",side:"s.png",bottom:"b.png")),
        ])"#;
        let reg = BlockRegistry::from_ron(src).unwrap();
        let g = reg.def(reg.id_of("grass").unwrap()).unwrap();
        assert_eq!(
            g.textures,
            TextureSpec::Faces { top: "t.png".into(), side: "s.png".into(), bottom: "b.png".into() }
        );
    }

    #[test]
    fn rejects_duplicate_id() {
        let src = r#"(blocks:[(id:0,name:"air",textures:Untextured),(id:0,name:"dup",textures:Untextured)])"#;
        assert!(matches!(BlockRegistry::from_ron(src), Err(RegistryError::DuplicateId(0))));
    }

    #[test]
    fn rejects_duplicate_name() {
        let src = r#"(blocks:[(id:0,name:"air",textures:Untextured),(id:1,name:"air",textures:All("x.png"))])"#;
        assert!(matches!(BlockRegistry::from_ron(src), Err(RegistryError::DuplicateName(_))));
    }

    #[test]
    fn requires_air_at_zero() {
        let src = r#"(blocks:[(id:1,name:"dirt",textures:All("dirt.png"))])"#;
        assert!(matches!(BlockRegistry::from_ron(src), Err(RegistryError::MissingAir)));
    }

    #[test]
    fn rejects_misdefined_air() {
        let src = r#"(blocks:[(id:0,name:"stone",textures:All("dirt.png"))])"#;
        assert!(matches!(BlockRegistry::from_ron(src), Err(RegistryError::BadAir)));
    }

    #[test]
    fn rejects_malformed_ron() {
        assert!(matches!(BlockRegistry::from_ron("not ron {"), Err(RegistryError::Parse(_))));
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail (compile error)**

Run: `cargo test -p game --lib registry 2>&1 | head -20`
Expected: FAIL — `cannot find type BlockId` / `BlockRegistry` not defined.

- [ ] **Step 5: Implement `registry.rs` above the test module**

Prepend to `crates/game/src/registry.rs` (before the `#[cfg(test)] mod tests`):

```rust
//! Data-driven block registry. The RON manifest (`assets/blocks/blocks.ron`)
//! is the source of truth for block identity: each `BlockDef` carries an
//! explicit, stable `id`. `BlockId` is the single material identity used at
//! both block and voxel granularity; the sim only distinguishes AIR from
//! solid. Texture specs are plain strings — the server ignores them, the
//! client resolves them to array-texture layers. Bevy-free.

use std::collections::BTreeMap;

/// Numeric material identity, assigned by the manifest. `AIR` is reserved.
/// Serialized (over the wire) and hashed, so client and server must agree.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default,
    serde::Serialize, serde::Deserialize,
)]
pub struct BlockId(pub u16);

impl BlockId {
    pub const AIR: BlockId = BlockId(0);
}

/// How a block's faces are textured. `Untextured` blocks (air) never mesh.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextureSpec {
    Untextured,
    All(String),
    Faces { top: String, side: String, bottom: String },
}

/// One manifest entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockDef {
    pub id: u16,
    pub name: String,
    pub textures: TextureSpec,
}

/// Top-level RON document shape: `( blocks: [ ... ] )`.
#[derive(Debug, Clone, serde::Deserialize)]
struct BlockManifest {
    blocks: Vec<BlockDef>,
}

#[derive(Debug)]
pub enum RegistryError {
    Parse(String),
    DuplicateId(u16),
    DuplicateName(String),
    MissingAir,
    BadAir,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Parse(e) => write!(f, "malformed block manifest: {e}"),
            RegistryError::DuplicateId(id) => write!(f, "duplicate block id {id}"),
            RegistryError::DuplicateName(n) => write!(f, "duplicate block name {n:?}"),
            RegistryError::MissingAir => write!(f, "block manifest must define id 0 as air"),
            RegistryError::BadAir => write!(f, "block id 0 must be name \"air\" with Untextured textures"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Parsed, validated registry. Indexed by ascending `BlockId`.
#[derive(Debug, Clone, Default)]
pub struct BlockRegistry {
    defs: BTreeMap<BlockId, BlockDef>,
    by_name: BTreeMap<String, BlockId>,
}

impl BlockRegistry {
    pub fn from_ron(src: &str) -> Result<Self, RegistryError> {
        let manifest: BlockManifest =
            ron::from_str(src).map_err(|e| RegistryError::Parse(e.to_string()))?;

        let mut defs = BTreeMap::new();
        let mut by_name = BTreeMap::new();
        for def in manifest.blocks {
            let id = BlockId(def.id);
            if by_name.insert(def.name.clone(), id).is_some() {
                return Err(RegistryError::DuplicateName(def.name));
            }
            if defs.insert(id, def).is_some() {
                return Err(RegistryError::DuplicateId(id.0));
            }
        }

        match defs.get(&BlockId::AIR) {
            None => return Err(RegistryError::MissingAir),
            Some(d) if d.name == "air" && d.textures == TextureSpec::Untextured => {}
            Some(_) => return Err(RegistryError::BadAir),
        }

        Ok(Self { defs, by_name })
    }

    pub fn id_of(&self, name: &str) -> Option<BlockId> {
        self.by_name.get(name).copied()
    }

    pub fn def(&self, id: BlockId) -> Option<&BlockDef> {
        self.defs.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (BlockId, &BlockDef)> {
        self.defs.iter().map(|(id, def)| (*id, def))
    }
}
```

- [ ] **Step 6: Wire the module into `game`'s crate root**

In `crates/game/src/lib.rs`, add the module declaration alongside the others (after `pub mod protocol;`):

```rust
pub mod registry;
```

and add the glob re-export alongside the others (after `pub use protocol::*;`):

```rust
pub use registry::*;
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p game --lib registry`
Expected: PASS — all 9 registry tests green.

- [ ] **Step 8: Commit**

```bash
git add crates/game/src/registry.rs crates/game/src/lib.rs crates/game/Cargo.toml assets/blocks/blocks.ron Cargo.lock
git commit -m "feat(game): BlockRegistry + BlockId parsed from RON manifest

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Client texture-layer builder (`client`)

Isolated, fully unit-tested rendering logic: the guaranteed layer-0 missing-texture checkerboard, `build_layers` (dedup + per-face), and the `BlockTextureLayers` resource. Does not yet wire into `setup_scene`/`meshing` — `VoxelTypeLayers` still runs the live path, so the client stays green.

**Files:**
- Create: `crates/client/src/renderer/block_textures.rs`
- Modify: `crates/client/src/renderer/mod.rs` (add `mod block_textures;`)

**Interfaces:**
- Consumes: `game::{BlockId, BlockRegistry, TextureSpec}` (Task 1).
- Produces:
  - `pub struct BlockTextureLayers(pub std::collections::BTreeMap<BlockId, [u32; 3]>)` — a Bevy `Resource`, `Default`, `Clone`.
  - `pub fn missing_texture_image(size: u32) -> image::RgbaImage`.
  - `pub fn build_layers(registry: &BlockRegistry, load: impl FnMut(&str) -> Option<image::RgbaImage>) -> (Vec<image::RgbaImage>, BlockTextureLayers)` — `layers[0]` is always the checkerboard; real textures at index ≥ 1; shared paths dedup; failed/absent load → layer 0.

- [ ] **Step 1: Write the failing tests**

Create `crates/client/src/renderer/block_textures.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use game::BlockRegistry;
    use image::{Rgba, RgbaImage};

    fn solid(size: u32, px: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(size, size, Rgba(px))
    }

    // Loader that returns a distinct solid image per known path, None otherwise.
    fn loader(path: &str) -> Option<RgbaImage> {
        match path {
            "dirt.png" => Some(solid(16, [100, 60, 20, 255])),
            "grass_top.png" => Some(solid(16, [0, 200, 0, 255])),
            "grass_side.png" => Some(solid(16, [80, 120, 40, 255])),
            _ => None,
        }
    }

    fn registry() -> BlockRegistry {
        BlockRegistry::from_ron(
            r#"(blocks:[
                (id:0,name:"air",textures:Untextured),
                (id:1,name:"dirt",textures:All("dirt.png")),
                (id:2,name:"grass",textures:Faces(top:"grass_top.png",side:"grass_side.png",bottom:"dirt.png")),
            ])"#,
        )
        .unwrap()
    }

    #[test]
    fn layer_zero_is_missing_texture_and_air_is_absent() {
        let reg = registry();
        let (layers, map) = build_layers(&reg, loader);
        assert!(!layers.is_empty(), "must always have the fallback layer");
        assert!(!map.0.contains_key(&BlockId::AIR), "air is never meshed/textured");
    }

    #[test]
    fn all_spec_maps_three_equal_layers() {
        let reg = registry();
        let (_, map) = build_layers(&reg, loader);
        let dirt = map.0[&BlockId(1)];
        assert_eq!(dirt[0], dirt[1]);
        assert_eq!(dirt[1], dirt[2]);
        assert!(dirt[0] >= 1, "real textures live at layers >= 1");
    }

    #[test]
    fn faces_spec_maps_distinct_top_side_bottom_and_dedups_shared() {
        let reg = registry();
        let (_, map) = build_layers(&reg, loader);
        let grass = map.0[&BlockId(2)];
        let dirt = map.0[&BlockId(1)];
        assert_ne!(grass[0], grass[1], "top != side");
        assert_ne!(grass[1], grass[2], "side != bottom");
        assert_eq!(grass[2], dirt[0], "grass bottom shares the dirt.png layer (dedup)");
    }

    #[test]
    fn missing_texture_resolves_to_layer_zero() {
        let reg = BlockRegistry::from_ron(
            r#"(blocks:[(id:0,name:"air",textures:Untextured),(id:1,name:"ghost",textures:All("nope.png"))])"#,
        )
        .unwrap();
        let (_, map) = build_layers(&reg, loader);
        assert_eq!(map.0[&BlockId(1)], [0, 0, 0], "unloadable texture falls back to layer 0");
    }

    #[test]
    fn empty_registry_still_has_fallback_layer() {
        let reg = BlockRegistry::from_ron(r#"(blocks:[(id:0,name:"air",textures:Untextured)])"#).unwrap();
        let (layers, map) = build_layers(&reg, loader);
        assert_eq!(layers.len(), 1, "just the checkerboard");
        assert!(map.0.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p client --lib block_textures 2>&1 | head -20`
Expected: FAIL — `build_layers` / `BlockTextureLayers` not found.

- [ ] **Step 3: Implement `block_textures.rs` above the test module**

Prepend to `crates/client/src/renderer/block_textures.rs`:

```rust
//! Builds the client's block array-texture layers and the
//! `BlockId -> [top, side, bottom]` layer map from the shared `BlockRegistry`.
//! Layer 0 is a procedurally generated magenta/black "missing texture" that is
//! guaranteed present (needs no asset file), so any unresolved texture or
//! unknown block renders as an unmistakable checkerboard rather than silently
//! as another block. Pure/engine-agnostic apart from the Bevy `Resource`
//! derive so it can be unit-tested without a GPU.

use std::collections::BTreeMap;

use bevy::prelude::Resource;
use game::{BlockId, BlockRegistry, TextureSpec};
use image::{Rgba, RgbaImage};

/// Maps each renderable `BlockId` to its `[top, side, bottom]` array-texture
/// layers. The mesher writes these into the `tex_idx` vertex attribute; the
/// fragment shader selects a slot by face normal.
#[derive(Resource, Default, Clone)]
pub struct BlockTextureLayers(pub BTreeMap<BlockId, [u32; 3]>);

/// A high-contrast magenta/black checkerboard, sized to match the block
/// textures so the array texture stays uniform. Reserved as layer 0.
pub fn missing_texture_image(size: u32) -> RgbaImage {
    let cell = (size / 4).max(1);
    let magenta = Rgba([255, 0, 255, 255]);
    let black = Rgba([0, 0, 0, 255]);
    RgbaImage::from_fn(size, size, |x, y| {
        if (x / cell + y / cell) % 2 == 0 { magenta } else { black }
    })
}

/// Intern a texture path to its array layer, loading (once) via `load`.
/// Returns 0 (the missing-texture layer) if the path fails to load.
fn intern<F: FnMut(&str) -> Option<RgbaImage>>(
    path: &str,
    load: &mut F,
    path_layer: &mut BTreeMap<String, u32>,
    images: &mut Vec<RgbaImage>,
    size: &mut Option<u32>,
) -> u32 {
    if let Some(&layer) = path_layer.get(path) {
        return layer;
    }
    match load(path) {
        Some(img) => {
            let (w, h) = img.dimensions(); // inherent on ImageBuffer/RgbaImage
            assert_eq!(w, h, "block texture {path} must be square");
            match *size {
                Some(s) => assert_eq!(s, w, "all block textures must share dimensions"),
                None => *size = Some(w),
            }
            let layer = (images.len() + 1) as u32; // +1: layer 0 is reserved
            images.push(img);
            path_layer.insert(path.to_string(), layer);
            layer
        }
        None => {
            bevy::log::warn!("missing block texture {path:?}; using fallback layer 0");
            0
        }
    }
}

/// Build the ordered array-texture layers (index 0 = checkerboard) and the
/// `BlockId -> [top, side, bottom]` map. Shared paths dedup to one layer;
/// `Untextured` blocks are omitted from the map.
pub fn build_layers<F: FnMut(&str) -> Option<RgbaImage>>(
    registry: &BlockRegistry,
    mut load: F,
) -> (Vec<RgbaImage>, BlockTextureLayers) {
    let mut path_layer: BTreeMap<String, u32> = BTreeMap::new();
    let mut images: Vec<RgbaImage> = Vec::new();
    let mut size: Option<u32> = None;
    let mut map: BTreeMap<BlockId, [u32; 3]> = BTreeMap::new();

    for (id, def) in registry.iter() {
        let triple = match &def.textures {
            TextureSpec::Untextured => continue,
            TextureSpec::All(p) => {
                let l = intern(p, &mut load, &mut path_layer, &mut images, &mut size);
                [l, l, l]
            }
            TextureSpec::Faces { top, side, bottom } => [
                intern(top, &mut load, &mut path_layer, &mut images, &mut size),
                intern(side, &mut load, &mut path_layer, &mut images, &mut size),
                intern(bottom, &mut load, &mut path_layer, &mut images, &mut size),
            ],
        };
        map.insert(id, triple);
    }

    let dim = size.unwrap_or(16);
    let mut layers = Vec::with_capacity(images.len() + 1);
    layers.push(missing_texture_image(dim));
    layers.extend(images);
    (layers, BlockTextureLayers(map))
}
```

- [ ] **Step 4: Register the module**

In `crates/client/src/renderer/mod.rs`, add near the other `mod` lines (e.g. after `mod avatar;`):

```rust
mod block_textures;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p client --lib block_textures`
Expected: PASS — all 5 tests green. (A dead-code warning on `build_layers`/`BlockTextureLayers` is expected until Task 3 wires them in; it is not an error.)

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/renderer/block_textures.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): build_layers + missing-texture fallback (unwired)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Atomic migration — `BlockId` everywhere + registry wiring

Replaces `VoxelType`/`BlockKind` with `BlockId` across `game`, switches `worldgen`/`server`/client-offline to registry-resolved ids, and rewires the client renderer to `build_layers`. This is one atomic commit because the type change spans crates and no intermediate state compiles. `Chunk::flat_floor(depth)` keeps its signature (defaulting to an arbitrary non-air material) so the many existing test call sites are untouched; `flat_floor_with(depth, solid)` is the registry-driven constructor for `worldgen`.

**Files:**
- Modify: `crates/game/src/voxel.rs`, `crates/game/src/block.rs`, `crates/game/src/state.rs`
- Modify: `crates/game/tests/chunk_blocks.rs`
- Modify: `crates/worldgen/src/lib.rs`
- Modify: `crates/server/src/lib.rs`
- Create: `crates/client/src/blocks.rs`; Modify: `crates/client/src/main.rs`, `crates/client/src/local_server.rs`
- Modify: `crates/client/src/renderer/mod.rs`, `crates/client/src/renderer/meshing.rs`

**Interfaces:**
- Consumes: `game::{BlockId, BlockRegistry, TextureSpec}` (Task 1); `block_textures::{build_layers, BlockTextureLayers}` (Task 2).
- Produces:
  - `game::Voxel { pub block: BlockId }` with `Voxel::new(BlockId)` and `Voxel::is_air() -> bool`.
  - `game::Block { pub id: BlockId, pub data: [u8; 3] }` with `Block::new(BlockId)`.
  - `game::Chunk::flat_floor(depth: u32)` (unchanged signature) and `game::Chunk::flat_floor_with(depth: u32, solid: BlockId)`.
  - `game::derive_voxels(&[Block], &BTreeMap<BlockIndex, ChiselData>) -> Vec<Voxel>` (signature unchanged; now emits `BlockId`).
  - `worldgen::generate_region(coords: RegionCoords, solid: BlockId) -> Vec<(ChunkCoords, Chunk)>`.
  - `client::blocks::load_registry() -> game::BlockRegistry` (native: reads `assets/blocks/blocks.ron`, falls back to embedded; wasm: embedded).

- [ ] **Step 1: Migrate `game/src/voxel.rs` — `Voxel.block`, delete `VoxelType`, split `flat_floor`**

Replace the imports line, the `Voxel`/`VoxelType` block, and the `block_mesh` impls. In `crates/game/src/voxel.rs`:

Change the import (line ~6) from:
```rust
use crate::block::{Block, BlockIndex, BlockKind, ChiselData, CHUNK_BLOCKS, CHUNK_BLOCK_COUNT, BLOCK_VOXELS};
```
to:
```rust
use crate::block::{Block, BlockIndex, ChiselData, CHUNK_BLOCKS, CHUNK_BLOCK_COUNT, BLOCK_VOXELS};
use crate::registry::BlockId;
```

Replace the `flat_floor` body's material references: change `Block::new(BlockKind::Air)` → `Block::new(BlockId::AIR)`, and make it delegate. Replace the whole `pub fn flat_floor` with:
```rust
    /// Solid floor for voxel heights `y < depth` using an arbitrary non-air
    /// material — a dev/test convenience. Production worldgen uses
    /// [`Chunk::flat_floor_with`] to place a registry-resolved material.
    pub fn flat_floor(depth: u32) -> Self {
        Self::flat_floor_with(depth, BlockId(1))
    }

    /// Solid `solid`-material floor for voxel heights `y < depth`, air above,
    /// built at block granularity: whole blocks below the floor, a chiseled
    /// slab for the block straddling it, air above.
    pub fn flat_floor_with(depth: u32, solid: BlockId) -> Self {
        let depth = depth as usize;
        let mut blocks = vec![Block::new(BlockId::AIR); CHUNK_BLOCK_COUNT];
        let mut chisel = BTreeMap::new();
        for by in 0..CHUNK_BLOCKS {
            let (y0, y1) = (by * BLOCK_VOXELS, by * BLOCK_VOXELS + BLOCK_VOXELS);
            for bx in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    let bi = BlockIndex::from_xyz(bx, by, bz);
                    if y1 <= depth {
                        blocks[bi.0 as usize] = Block::new(solid);
                    } else if y0 >= depth {
                        // stays Air
                    } else {
                        blocks[bi.0 as usize] = Block::new(solid);
                        let mut c = ChiselData::new(vec![solid]);
                        for vy in 0..BLOCK_VOXELS {
                            if y0 + vy < depth {
                                for vx in 0..BLOCK_VOXELS {
                                    for vz in 0..BLOCK_VOXELS {
                                        c.set(vx, vy, vz, true, 0);
                                    }
                                }
                            }
                        }
                        chisel.insert(bi, c);
                    }
                }
            }
        }
        Self { blocks, chisel }
    }
```

Replace the `Voxel` struct + `VoxelType` enum + its `Default` + the two `block_mesh` impls (lines ~88–132) with:
```rust
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Copy, Clone, Hash, PartialEq, Eq)]
pub struct Voxel {
    pub block: BlockId,
}

impl Voxel {
    pub fn new(block: BlockId) -> Self {
        Self { block }
    }

    pub fn is_air(&self) -> bool {
        self.block == BlockId::AIR
    }
}

impl block_mesh::Voxel for Voxel {
    fn get_visibility(&self) -> VoxelVisibility {
        if self.is_air() {
            VoxelVisibility::Empty
        } else {
            VoxelVisibility::Opaque
        }
    }
}

impl MergeVoxel for Voxel {
    type MergeValue = BlockId;
    type MergeValueFacingNeighbour = BlockId;

    fn merge_value(&self) -> Self::MergeValue {
        self.block
    }

    fn merge_value_facing_neighbour(&self) -> Self::MergeValueFacingNeighbour {
        self.block
    }
}
```

- [ ] **Step 2: Migrate `game/src/block.rs` — `Block.id`, `BlockId` palette, delete `BlockKind`**

In `crates/game/src/block.rs`:

Change the imports (top of file) to add `BlockId`:
```rust
use crate::registry::BlockId;
```
(add this line after the existing `use crate::voxel::{...}` import).

Change the `voxel` import line from:
```rust
use crate::voxel::{ChunkShape, Voxel, VoxelType, CHUNK_VOXEL_COUNT};
```
to:
```rust
use crate::voxel::{ChunkShape, Voxel, CHUNK_VOXEL_COUNT};
```

Replace the `Block` struct + impl (lines ~46–58):
```rust
#[derive(
    Copy, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Debug,
)]
pub struct Block {
    pub id: BlockId,
    pub data: [u8; 3],
}

impl Block {
    pub fn new(id: BlockId) -> Self {
        Self { id, data: [0; 3] }
    }
}
```

Delete the entire `BlockKind` enum and its `impl` (the `pub enum BlockKind { Air, Stone }`, its `Default` is derived, and `impl BlockKind { pub fn voxel(...) }` — lines ~60–78).

In `ChiselData`, change the palette field type from `palette: Vec<BlockKind>` to:
```rust
    palette: Vec<BlockId>,
```

Change `ChiselData::new` signature from `pub fn new(palette: Vec<BlockKind>)` to:
```rust
    pub fn new(palette: Vec<BlockId>) -> Self {
```

Change `material_at` return type + body from `-> BlockKind`:
```rust
    pub fn material_at(&self, vx: usize, vy: usize, vz: usize) -> BlockId {
        let i = Self::local_index(vx, vy, vz);
        self.palette
            .get(self.material[i] as usize)
            .copied()
            .unwrap_or(BlockId::AIR)
    }
```

Rewrite `derive_voxels`'s inner match + write (the `let kind = match chiseled { ... }` and `voxels[li] = Voxel::new(kind.voxel());` lines) to:
```rust
                    let id = match chiseled {
                        Some(c) if c.is_solid(vx, vy, vz) => c.material_at(vx, vy, vz),
                        Some(_) => BlockId::AIR,
                        None => block.id,
                    };
                    let li = voxel_index(bx * BLOCK_VOXELS + vx, by * BLOCK_VOXELS + vy, bz * BLOCK_VOXELS + vz);
                    voxels[li] = Voxel::new(id);
```
and change the initial fill `let mut voxels = vec![Voxel::new(VoxelType::Air); CHUNK_VOXEL_COUNT];` to:
```rust
    let mut voxels = vec![Voxel::new(BlockId::AIR); CHUNK_VOXEL_COUNT];
```

Update `block.rs`'s own `#[cfg(test)] mod tests`: replace every `BlockKind::Stone` → `BlockId(1)`, `BlockKind::Air` → `BlockId::AIR`, `VoxelType::Black` → `BlockId(1)`, `VoxelType::Air` → `BlockId::AIR`, `.kind` (on voxels) → `.block`, and the `use crate::voxel::VoxelType;` line → `use crate::registry::BlockId;`. Concretely the assertions become e.g. `assert!(voxels.iter().all(|v| v.block == BlockId(1)));` and `assert_eq!(voxels[voxel_index(0, 0, 0)].block, BlockId(1));`.

- [ ] **Step 3: Migrate `game/src/state.rs` — collision filter**

In `crates/game/src/state.rs`, change the import (line 21) from:
```rust
use crate::voxel::{Chunk, ChunkCoords, ChunkShape, Voxel, VoxelType};
```
to:
```rust
use crate::voxel::{Chunk, ChunkCoords, ChunkShape, Voxel};
use crate::registry::BlockId;
```
and change the collision filter (line ~281) from:
```rust
            .filter(|i| voxels[*i as usize].kind != VoxelType::Air)
```
to:
```rust
            .filter(|i| voxels[*i as usize].block != BlockId::AIR)
```

- [ ] **Step 4: Migrate `game/tests/chunk_blocks.rs`**

In `crates/game/tests/chunk_blocks.rs`, change the import from `use game::{derive_voxels, voxel_index, Chunk, VoxelType};` to:
```rust
use game::{derive_voxels, voxel_index, BlockId, Chunk};
```
and change the two assertions (lines ~32–33) from `VoxelType::Black`/`VoxelType::Air` on `.kind`:
```rust
    assert_eq!(voxels[voxel_index(0, 7, 0)].block, BlockId(1), "y<8 solid");
    assert_eq!(voxels[voxel_index(0, 8, 0)].block, BlockId::AIR, "y>=8 air");
```
(`flat_floor(8)`/`flat_floor(12)` calls stay as-is — they use the default `BlockId(1)` material.)

- [ ] **Step 5: Run the `game` test suite**

Run: `cargo test -p game`
Expected: PASS — including the rollback/hash-invariant suites (`log_model`, `simple`, `random_ops`, `hash_restore`, `rollback_restore`). These are the primary determinism safety net; the type swap must not change their outcome.

- [ ] **Step 6: Migrate `worldgen`**

In `crates/worldgen/src/lib.rs`:

Change the import to add `BlockId`:
```rust
use game::{BlockId, Chunk, ChunkCoords, RegionCoords, REGION_CHUNKS};
```

Change `generate_region` to take the solid material and use `flat_floor_with`:
```rust
/// The full 8×8 chunk grid for one region, region-local coordinates.
/// Pure and deterministic; no clocks, no RNG. `solid` is the floor material,
/// resolved from the block registry by the caller.
pub fn generate_region(coords: RegionCoords, solid: BlockId) -> Vec<(ChunkCoords, Chunk)> {
    let depth = floor_height(coords);
    let mut chunks = Vec::with_capacity(REGION_CHUNKS * REGION_CHUNKS);
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            chunks.push((ChunkCoords::new(x, 0, z), Chunk::flat_floor_with(depth, solid)));
        }
    }
    chunks
}
```

In `worldgen`'s test module, add a stone constant and thread it through the `generate_region` calls:
```rust
    const TEST_STONE: game::BlockId = game::BlockId(2);
```
and change each `generate_region(RegionCoords::new(..))` call to `generate_region(RegionCoords::new(..), TEST_STONE)` (in `generation_is_pure`, `neighbouring_regions_differ`, `generated_chunks_carry_chisel_slabs`).

- [ ] **Step 7: Run the `worldgen` tests**

Run: `cargo test -p worldgen`
Expected: PASS.

- [ ] **Step 8: Wire the registry into the `server`**

In `crates/server/src/lib.rs`, immediately before the `let mut manager = game::WorldManager::new(` block (line ~228), insert the manifest load + id resolution:
```rust
    // Block registry: prefer the runtime manifest, fall back to the copy
    // embedded at build time (same repo file) so headless runs never fail.
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/blocks/blocks.ron"),
    )
    .unwrap_or_else(|_| include_str!("../../../assets/blocks/blocks.ron").to_string());
    let registry = game::BlockRegistry::from_ron(&manifest).expect("invalid block manifest");
    let stone = registry.id_of("stone").expect("block manifest must define \"stone\"");
```
and change the generator argument from `Box::new(worldgen::generate_region),` to:
```rust
        Box::new(move |rc| worldgen::generate_region(rc, stone)),
```

- [ ] **Step 9: Create the client manifest loader `crates/client/src/blocks.rs`**

```rust
//! Loads the shared block registry on the client. Prefers the runtime
//! manifest under `assets/blocks/` on native (edit + restart, no rebuild);
//! falls back to the copy embedded at build time, which is also what the
//! browser (no filesystem) uses. Both the renderer (textures) and the offline
//! `LocalServer` (worldgen material ids) read through here, so a single repo
//! file drives client identity.

use game::BlockRegistry;

/// The manifest embedded at build time — identical bytes to the on-disk file.
const EMBEDDED_MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");

fn manifest_src() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = crate::renderer::resolve_blocks_dir().join("blocks.ron");
        if let Ok(s) = std::fs::read_to_string(&path) {
            return s;
        }
    }
    EMBEDDED_MANIFEST.to_string()
}

/// Parse the block registry, panicking on a malformed manifest (a fatal
/// startup misconfiguration).
pub fn load_registry() -> BlockRegistry {
    BlockRegistry::from_ron(&manifest_src()).expect("invalid block manifest")
}
```

Note: this calls `crate::renderer::resolve_blocks_dir()`, which is currently `#[cfg(not(target_arch = "wasm32"))] fn`. In `crates/client/src/renderer/mod.rs`, change its declaration from `fn resolve_blocks_dir()` to `pub(crate) fn resolve_blocks_dir()` so `blocks.rs` can use it.

- [ ] **Step 10: Register `blocks` and use it in `local_server`**

In `crates/client/src/main.rs`, add near `mod local_server;`:
```rust
mod blocks;
```

In `crates/client/src/local_server.rs`, change the `WorldManager::new` generator argument (line ~36) from `Box::new(worldgen::generate_region),` to a closure capturing the resolved id. Immediately before the `let mut manager = WorldManager::new(` line, insert:
```rust
        let registry = crate::blocks::load_registry();
        let stone = registry.id_of("stone").expect("block manifest must define \"stone\"");
```
and change the argument to:
```rust
            Box::new(move |rc| worldgen::generate_region(rc, stone)),
```

- [ ] **Step 11: Rewire the renderer — `setup_scene` + `BlockTextureLayers`, delete `VoxelTypeLayers`**

In `crates/client/src/renderer/mod.rs`:

Delete the `VoxelTypeLayers` definition (lines ~31–33) and its doc comment. Add to the `use` of the block_textures module at the top (with the other `mod`/`use`):
```rust
use block_textures::{build_layers, BlockTextureLayers};
```

Everywhere `VoxelTypeLayers` was referenced as a resource, use `BlockTextureLayers`. In `SimBridgePlugin::build`, there is no `init_resource::<VoxelTypeLayers>()` today (the resource is inserted by `setup_scene`), so no change there beyond `setup_scene`.

Replace the body of `setup_scene` that collects `layers`/`layer_names`/`sorted` and builds `voxel_type_layers` (roughly lines 122–209) with a registry-driven build. Keep the `DirectionalLight`/`GlobalAmbientLight` spawns at the end unchanged. The new core:

```rust
fn setup_scene(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut voxel_materials: ResMut<Assets<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>>,
) {
    let registry = crate::blocks::load_registry();

    // Loader: native reads PNGs from assets/blocks/; wasm resolves from the
    // embedded texture map. Returns None on any failure (build_layers then
    // falls the block back to the layer-0 missing texture).
    let (rgba_layers, block_layers) = build_layers(&registry, |path| load_block_texture(path));

    let (w, h) = (rgba_layers[0].width(), rgba_layers[0].height());
    let data: Vec<u8> = rgba_layers.iter().flat_map(|l| l.as_raw().clone()).collect();
    let mut array_image = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: rgba_layers.len() as u32 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    let mut sampler = ImageSamplerDescriptor::nearest();
    sampler.address_mode_u = ImageAddressMode::Repeat;
    sampler.address_mode_v = ImageAddressMode::Repeat;
    sampler.address_mode_w = ImageAddressMode::Repeat;
    array_image.sampler = ImageSampler::Descriptor(sampler);
    array_image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..Default::default()
    });
    let handle = images.add(array_image);

    commands.insert_resource(meshing::ChunkMaterial(voxel_materials.add(ExtendedMaterial {
        base: StandardMaterial { perceptual_roughness: 0.9, ..Default::default() },
        extension: StandardVoxelMaterial { voxels_texture: handle },
    })));
    commands.insert_resource(block_layers);

    commands.spawn((
        DirectionalLight { color: Color::srgb(0.98, 0.95, 0.82), shadows_enabled: true, ..Default::default() },
        Transform::default().looking_at(Vec3::new(-0.15, -0.1, 0.15), Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.98, 0.95, 0.82),
        brightness: 100.0,
        ..Default::default()
    });
}
```

Add the texture loader helper below `setup_scene` (it replaces the old directory scan / embedded logic, reusing `resolve_blocks_dir` and `EMBEDDED_BLOCK_TEXTURES`):
```rust
/// Load one block texture as RGBA8. Native reads `assets/blocks/<path>`; wasm
/// resolves it from the compiled-in texture table. `None` on any failure.
fn load_block_texture(path: &str) -> Option<image::RgbaImage> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let full = resolve_blocks_dir().join(path);
        match image::ImageReader::open(&full).ok().and_then(|r| r.decode().ok()) {
            Some(img) => return Some(img.to_rgba8()),
            None => {
                warn!("could not read block texture {full:?}");
                return None;
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = EMBEDDED_BLOCK_TEXTURES.iter().find(|(n, _)| *n == path).map(|(_, b)| *b)?;
        match image::load_from_memory(bytes) {
            Ok(img) => Some(img.to_rgba8()),
            Err(e) => {
                warn!("could not decode embedded block texture {path}: {e:?}");
                None
            }
        }
    }
}
```

Keep `EMBEDDED_BLOCK_TEXTURES` and `resolve_blocks_dir` (now `pub(crate)`). Remove the now-unused imports if the compiler flags them (e.g. `BTreeMap`, `TextureDimension` stays). Keep the existing `#[cfg(test)] mod tests` in `mod.rs` (its `embedded_block_textures_match_assets_dir` / `resolve_blocks_dir` tests still apply).

- [ ] **Step 12: Rewire the mesher `meshing.rs` — per-face `BlockTextureLayers`**

In `crates/client/src/renderer/meshing.rs`:

Change `use super::VoxelTypeLayers;` to:
```rust
use super::block_textures::BlockTextureLayers;
```

Change `build_chunk_mesh`'s signature and the per-quad texture lookup. Signature:
```rust
pub fn build_chunk_mesh(voxels: &[Voxel], layers: &BlockTextureLayers) -> Option<Mesh> {
```
Replace the layer lookup (line ~36) and the `tex_indices.extend(...)` (line ~37):
```rust
            let faces = layers.0.get(&voxels[voxel_index].block).copied().unwrap_or([0, 0, 0]);
            tex_indices.extend(std::iter::repeat(faces).take(4));
```

In `queue_meshing`, change `layers: Res<VoxelTypeLayers>` to `layers: Res<BlockTextureLayers>` and the task body `build_chunk_mesh(&voxels, &VoxelTypeLayers(layers))` to `build_chunk_mesh(&voxels, &BlockTextureLayers(layers))`.

In `meshing.rs`'s `#[cfg(test)] mod tests`: change `use game::{Voxel, VoxelType, CHUNK_VOXEL_COUNT};` to `use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};`; replace every `Voxel::new(VoxelType::Black)` → `Voxel::new(BlockId(1))`; replace every `VoxelTypeLayers::default()` → `BlockTextureLayers::default()` and `crate::renderer::VoxelTypeLayers` → `crate::renderer::block_textures::BlockTextureLayers`. In the `same_frame_remove_and_reinsert_keeps_mesh` test that constructs `StandardVoxelMaterial { voxels_texture: image_handle }` and inits `VoxelTypeLayers`, use `BlockTextureLayers`. (`flat_floor(8)` in `derives_and_meshes_a_generated_floor_chunk` stays; with an empty `BlockTextureLayers` the block maps to `[0,0,0]` and still meshes.)

- [ ] **Step 13: Catch any stragglers**

Run: `grep -rn "VoxelType\|BlockKind\|VoxelTypeLayers\|\.kind\b" crates/game/src crates/game/tests crates/worldgen crates/server crates/client/src | grep -viE "EntityKind|GameEventKind|GameDataUpdate|ecs\.kind|self\.kind|GameDataUpdateKind|\.kind\b.*(ev|event|Entity)"`

Expected: no remaining references to the deleted `VoxelType`/`BlockKind`/`VoxelTypeLayers`, or to `.kind` on a `Voxel`/`Block`. Fix any that appear using the same rename rules (`VoxelType::Black`→`BlockId(1)`, `VoxelType::Air`→`BlockId::AIR`, voxel/block `.kind`→`.block`/`.id`).

- [ ] **Step 14: Build the whole workspace and run all affected test suites**

Run:
```bash
cargo build --workspace --bins
cargo test -p game && cargo test -p worldgen && cargo test -p client
```
Expected: workspace compiles; `game`, `worldgen`, and `client` suites all PASS. In particular the client `block_textures` tests, the `meshing` tests, and the `local_server` offline handshake tests pass, and the `game` rollback/hash suites are unchanged-green.

- [ ] **Step 15: Verify in the running app**

Run the server and client per `scripts/run.sh` (or the documented cranelift commands) and confirm the floor renders textured (dirt) and nothing renders as the magenta/black checkerboard (which would indicate an unresolved texture). Temporarily point `stone`'s texture at a nonexistent file in `blocks.ron`, restart, and confirm the floor shows the checkerboard fallback rather than crashing; then revert.

- [ ] **Step 16: Commit**

```bash
git add -A
git commit -m "feat: data-driven block registry — BlockId replaces VoxelType/BlockKind

Collapse the two hardcoded enums into a single BlockId assigned by the RON
manifest. worldgen/server/offline resolve materials by name; client builds
the array texture + per-face layer map from the registry, with a reserved
layer-0 missing-texture fallback. Sim keeps non-air = solid.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Notes for the implementer

- **Do not** reintroduce a hardcoded material enum anywhere. The only "magic" ids are `BlockId::AIR = 0` (reserved) and the `BlockId(1)` dev-floor default inside `flat_floor` (explicitly a test/dev convenience, never used by production worldgen).
- The `game` rollback suite (`cargo test -p game`) is the determinism gate. If any of `log_model`/`random_ops`/`hash_restore`/`rollback_restore` fails after the type swap, the migration changed serialized/hashed behavior incorrectly — investigate before proceeding, do not "adjust" the invariant.
- Parked `SerializedRegion` blobs and wire packets from before this change are incompatible (the `Voxel`/`Block` layout changed). This is expected; there is no persistent save to migrate.
- Adding a real block after this lands: drop `<name>.png` into `assets/blocks/`, add a `(id, name, textures)` line to `blocks.ron` (append-only ids), and on wasm add the PNG to `EMBEDDED_BLOCK_TEXTURES` (the `embedded_block_textures_match_assets_dir` test enforces the wasm list stays in sync). No code changes.
```
