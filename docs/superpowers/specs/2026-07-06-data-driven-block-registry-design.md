# Data-Driven Block Registry — Design

**Date:** 2026-07-06
**Status:** Approved design, pre-implementation
**Goal:** Define blocks and their textures as data. Adding a new textured block
should be an asset/manifest edit, not a multi-file enum change. Voxels become an
implementation detail of rendering and collision; the block is the unit an author
thinks in.

## Motivation

Today block/texture identity is split across two hardcoded, 1:1-redundant enums:

- `BlockKind` (`crates/game/src/block.rs`) — gameplay material (`Air`, `Stone`)
- `VoxelType` (`crates/game/src/voxel.rs`) — render material (`Air`, `Black`)

Rendering keys textures off `VoxelType` via `VoxelTypeLayers`
(`crates/client/src/renderer/mod.rs`), and `setup_scene` hardcodes the single
mapping `VoxelType::Black → black.png` (which doesn't even exist, so it falls back
to `dirt.png`). Adding a visually distinct block requires edits to `BlockKind`,
`VoxelType`, `BlockKind::voxel()`, and `setup_scene` — across two crates.

Key architectural finding that shapes this design: **the simulation only ever asks
whether a voxel is air or solid.** Collision is built from
`voxels[i].kind != VoxelType::Air` (`state.rs:281`) and nothing else. Which
*specific* non-air material a voxel is is purely a render concern. The only catch:
that material tag rides on `Voxel.kind`, which is serialized (`SetVoxelComponent`
sends `Vec<Voxel>`) and hashed (crc32 state hash), so its representation must be
deterministic and identical on client and server.

## Decisions (locked)

1. **Full collapse to a single numeric id.** Delete `BlockKind` and `VoxelType`;
   introduce `BlockId(u16)` used at both block and voxel granularity.
   `BlockId::AIR = 0` reserved.
2. **Runtime RON manifest** at `assets/blocks/blocks.ron` is the source of truth
   for identity. Read at startup by each binary (native), embedded on wasm.
3. **Explicit stable ids** in the manifest — reordering the file must never
   renumber an existing block, because parked `SerializedRegion` blobs and the
   wire format embed the id.
4. **RON** format via the `ron` crate (already in `Cargo.lock` transitively via
   Bevy; added as a direct dep of `game`). Chosen over `serde_json` (zero-dep) for
   inline comments and clean enum authoring in a hand-edited file.
5. **Per-face textures** supported from day one (`all` shorthand + explicit
   top/side/bottom), since the fragment shader already selects a layer by face
   normal.
6. **YAGNI scope:** v1 manifest is identity + textures only. No per-block `solid`
   flag (collision stays "non-air = solid"), no overlays/tinting/variants, no
   runtime hot-reload. All are clean follow-ons.

### Default choices (adjustable at review)

- `BlockId` is `u16` (future-proof, cheap on the wire vs the current enum's u32
  bincode variant index).
- Field names: `Block.id: BlockId`, `Voxel.block: BlockId`.

## Architecture

```
assets/blocks/blocks.ron  ──read──►  BlockRegistry (game crate, Bevy-free)
        │                                   │
        │                                   ├─► worldgen: name → BlockId (create stone)
        │                                   ├─► server sim: passes ids through; collision = id != AIR
        │                                   └─► client: BlockId → textures
        │
   (PNG files) ───────────────────────────► client atlas + BlockTextureLayers
```

The registry is deterministic shared data, injected into the sim the same way the
`worldgen` closure already is. `game` never touches the filesystem itself — callers
read the file (or embed the bytes on wasm) and hand `game` the parsed string.

## Components

### 1. Manifest — `assets/blocks/blocks.ron`

```ron
(
    blocks: [
        (id: 0, name: "air",   textures: None),
        (id: 1, name: "dirt",  textures: All("dirt.png")),
        (id: 2, name: "grass", textures: Faces(
            top: "grass_top.png", side: "grass_side.png", bottom: "dirt.png",
        )),
        (id: 3, name: "stone", textures: All("stone.png")),
    ],
)
```

### 2. `crates/game/src/registry.rs` (new)

Bevy-free. Plain-string texture specs; the server ignores them.

```rust
pub struct BlockId(pub u16);          // AIR = BlockId(0)

pub enum TextureSpec {
    None,                              // air / invisible
    All(String),
    Faces { top: String, side: String, bottom: String },
}

pub struct BlockDef {
    pub id: BlockId,
    pub name: String,
    pub textures: TextureSpec,
}

pub struct BlockRegistry { /* Vec<BlockDef> indexed by id + name→id map */ }

impl BlockRegistry {
    pub fn from_ron(src: &str) -> Result<Self, RegistryError>;
    pub fn id_of(&self, name: &str) -> Option<BlockId>;
    pub fn def(&self, id: BlockId) -> Option<&BlockDef>;
}
```

**Validation** (`from_ron`): ids unique; id 0 present and named `air` with
`textures: None`; ids form a usable index. Texture-file existence is *not* checked
here (that's a client concern, and `game` has no filesystem access).

### 3. Sim-side changes (mechanical type swap)

- `block.rs`: `Block { id: BlockId, data: [u8;3] }`; `ChiselData.palette:
  Vec<BlockId>`; delete `BlockKind` and `BlockKind::voxel()`; `derive_voxels`
  becomes a straight copy of `BlockId` (chiseled and full voxels identical).
- `voxel.rs`: `Voxel { block: BlockId }`; `Voxel::is_air()` = `block == AIR`;
  delete `VoxelType`.
- `state.rs:281`: `!= VoxelType::Air` → `!= BlockId::AIR` (behavior unchanged).
- `lib.rs`: update glob re-exports (`BlockId`, `BlockRegistry`, `TextureSpec`
  replace `BlockKind`/`VoxelType`).
- `worldgen`: needed ids (e.g. `stone`) are resolved from the registry once at
  wiring time and captured by the injected generation closure; `generate_region`
  stays a pure function of coords plus the resolved ids, creating `Block { id }`
  instead of `BlockKind::Stone`. This keeps the registry out of the generation hot
  path and off `worldgen`'s public signature.

The `#[rollback]` macro operates on field wrappers and is transparent to the inner
type change; `Voxel` inside the `SetVoxelComponent` update variant is unaffected in
shape.

### 4. Client / render changes

- Load `blocks.ron` → `BlockRegistry`.
- `setup_scene` (rewrite): iterate the registry instead of scanning the directory.
  For each referenced PNG, load once and dedup by path (`dirt.png` shared by
  `dirt.All` and `grass.bottom` → one array layer). Build the array texture and a
  `BlockTextureLayers: BTreeMap<BlockId, [u32; 3]>` (top, side, bottom layer
  indices). `TextureSpec::All(p)` → `[layer(p); 3]`.
- `meshing.rs` (`build_chunk_mesh`): look up `[u32;3]` by `voxels[i].block` and
  write per-face indices into the existing `ATTRIBUTE_TEX_INDEX` (`tex_idx`)
  attribute — the fragment shader already picks the slot by face normal
  (top/side/bottom). This finally exercises the shader's per-face capability;
  **no shader change**.
- Rename `VoxelTypeLayers` → `BlockTextureLayers` throughout.
- **Fallback texture (guaranteed):** array **layer 0 is reserved** as a
  procedurally generated "missing texture" — a high-contrast magenta/black
  checkerboard built in code (a small `RgbaImage`) at atlas-build time, so it
  depends on no asset file and can never itself be missing. Real block textures
  occupy layers ≥ 1. Any block whose texture path fails to load, or any `BlockId`
  with no layer entry, resolves to layer 0 on all three faces and logs a warning —
  never panics, and renders as an unmistakable "missing texture" rather than
  silently as another block. This holds even when `assets/blocks/` is empty or the
  manifest references nothing.

### 5. Determinism, wire format, wasm

- Changing `Voxel`/`Block` types changes the bincode wire format and the crc32
  state hash. Client and server rebuild together (dev stage). Parked
  `SerializedRegion` blobs are in-memory/ephemeral — no persistent migration.
- Explicit ids ⇒ client and server agree on numbering regardless of load order.
- wasm: keep the embedded-bytes pattern, generalized. `EMBEDDED_BLOCK_TEXTURES`
  (filename → bytes) plus the embedded manifest string; the client resolves
  filenames to bytes from that map on wasm and from the filesystem on native. The
  existing `embedded_block_textures_match_assets_dir` test is extended to require
  every PNG referenced by the manifest to be embedded.

## Data Flow (adding a block, author's view)

1. Drop `sand.png` into `assets/blocks/`.
2. Add `(id: 4, name: "sand", textures: All("sand.png"))` to `blocks.ron`.
3. (wasm only) add `sand.png` to `EMBEDDED_BLOCK_TEXTURES` — a test enforces this.
4. Restart. No enum edits, no `setup_scene` edits, no shader edits.

## Error Handling

- Manifest parse/validation errors (`from_ron`) are fatal at startup with a clear
  message (duplicate id, missing/misdefined air, malformed RON).
- Missing texture file (client): warn and fall back to the reserved layer-0
  "missing texture" checkerboard; do not crash.
- Unknown `BlockId` in the mesher (id with no layer entry): fall back to the
  layer-0 missing texture.
- The missing-texture layer is generated in code, so it is always present even
  with an empty assets dir or a manifest that references no files.

## Testing

**`game`:**
- `BlockRegistry::from_ron` — valid manifest parses; errors on duplicate id,
  missing/misdefined air, malformed RON.
- `derive_voxels` emits correct `BlockId`s for full blocks and chiseled blocks
  (palette of `BlockId`s).
- Existing rollback / hash-invariant suite (`tests/log_model.rs`, `simple.rs`,
  `random_ops.rs`, `hash_restore.rs`) passes unchanged under the type swap — the
  primary safety net that determinism and undo invariants still hold.

**`client`:**
- Registry → `BlockTextureLayers` builds the array texture; shared texture paths
  dedup to one layer.
- `All` maps to three equal layers; `Faces` maps to distinct top/side/bottom.
- Mesher writes correct per-face `tex_idx` (top face uses top layer, sides use
  side layer, bottom uses bottom layer).
- Layer 0 is always the generated missing-texture checkerboard (present even with
  an empty assets dir); real textures start at layer ≥ 1.
- Missing-texture / unknown-`BlockId` fallback resolves to layer 0 without panic.
- Extended `embedded_block_textures_match_assets_dir` covers all manifest PNGs.

## Out of Scope (v1)

- Per-block `solid`/collision properties (collision stays non-air = solid).
- Texture overlays, blend modes, climate/season tinting, random alternates
  (Vintage Story `CompositeTexture` features).
- Runtime hot-reload of the manifest.
- Sending blocks+chisel over the wire and deriving voxels client-side (client
  still receives derived `Vec<Voxel>`).

## Files Touched

- `crates/game/src/registry.rs` (new)
- `crates/game/src/{block,voxel,state,lib}.rs`
- `crates/game/Cargo.toml` (+`ron`)
- `crates/worldgen/src/*` (registry-driven block creation)
- `crates/server/src/*` (load manifest, construct registry, inject)
- `crates/client/src/renderer/{mod,meshing}.rs`
- `assets/blocks/blocks.ron` (new), plus example PNGs
```
