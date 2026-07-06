# Block Abstraction Data Model — Design

Date: 2026-07-06
Status: Design (data-model milestone; interaction deferred)

## Overview

Introduce a **block** as the primary gameplay unit of the world, with voxels
reframed as a subdivision of blocks rather than the source of truth. A block is
**16×16×16 voxels = 1 m³** (the world is already scaled at 1 voxel = 1/16 m).
Blocks tile a regular grid; voxels only exist as finer detail *inside* a block.

This milestone is **data-model only**: it flips the chunk representation from
"dense voxel array, no block layer" to "block array authoritative, voxels
derived," and rebuilds worldgen, meshing input, and the collider input around
that. It deliberately ships **no player interaction** (no break/place/use, no
raycasting, no runtime block edits). Those are follow-on milestones that build
on this foundation.

The design follows the dominant pattern across established voxel games (see
Prior Art): the gameplay unit is a small fixed-width value in a per-chunk array,
and sub-block detail is sparse/opt-in — nobody stores a dense sub-block voxel
grid for every block. Vintage Story's chiseling system is the direct analog for
the sub-block layer, and its lessons shape the `ChiselData` representation.

## Goals

- A `Block` is the authoritative per-cell gameplay unit; a chunk stores blocks,
  not voxels.
- Sub-block 16³ voxel detail ("chiseling") is supported but **sparse** — only
  blocks that are actually subdivided carry voxel data.
- The existing meshing (`block_mesh` greedy mesher) and rapier voxel collider
  keep working essentially unchanged: they consume a 32³ voxel array that is now
  *derived* from blocks instead of stored.
- Rollback correctness is preserved: `hash(before) == hash(after undo)`,
  bit-exact, with **no new undo machinery** (blocks are set-once this milestone).
- Cross-machine determinism and wasm32 wire-compatibility are preserved.
- Worldgen produces blocks, exercising the chisel path end-to-end via a chiseled
  top slab for sub-block floor heights.

## Non-Goals (explicit)

- **No interaction**: no break, place, or use; no raycasting; mouse input stays
  dropped as it is today. No runtime block edits.
- **No undo-mutation machinery / no incremental collider rebuild** — not needed
  while blocks are set-once at chunk creation.
- **No block entities / block behaviors** (chests, doors, furnaces). The
  storage location for block-level rich state is an open question (see below),
  deliberately unresolved here.
- **No global `BlockCoords`** addressing type — within-chunk `BlockIndex` is all
  the data model needs until raycasting arrives.
- **No palette compression** of the block array (Minecraft-style). It is a later
  optimization, not part of this foundation.
- **No vertical chunk stacking / world-topology changes.** The world stays a
  single chunk layer; this model neither fixes nor precludes stacking.
- **No VS-style packed-cuboid wire format** for chisel data. Chisel ships as the
  dense bitset; cuboid packing only matters once edits are networked.

## Prior Art (research summary)

Verified across primary sources (protocol wikis, engine rustdoc, first-party dev
blogs; deep-research pass 2026-07-06):

- **Universal pattern**: the gameplay unit ("block"/"node"/"Block") is a small
  fixed-width value in a dense or paletted per-chunk array. Sub-block detail is
  either rendered-only model geometry (Minecraft models, Luanti nodeboxes) or
  the block layer is dropped entirely and voxels are the primitive (Teardown,
  Ace of Spades — which then have *no* metadata/state layer at all).
- **Minecraft** (Java): 16³-block sections, paletted container (bits-per-entry +
  palette + packed longs); block-states + block-entities separate; no sub-block
  voxels.
- **Luanti/Minetest**: 16³-node MapBlocks; dense `{param0, param1, param2}`;
  node metadata in a separate side-table; nodeboxes are geometry only.
- **Veloren** (Rust, closest analog): each block is a compact 4-byte value
  (`Block { kind: BlockKind, data: [u8;3] }`); per-chunk metadata is a separate
  generic parameter; wire format (`WireChonk`) is decoupled from in-memory
  storage via a pluggable compression codec.
- **Vintage Story** (direct analog for our sub-block layer, code-confirmed in
  `vssurvivalmod/Systems/Microblock/`): a chiseled cell is an ordinary block in
  the chunk array whose id is a dedicated "microblock" type; the 16³ detail
  hangs off a **block entity** keyed by position, never inline in the block
  array. Sub-voxel resolution is 16³. VS's *stored* form is a merged-cuboid list
  of packed `uint`s (4 bits each for min x/y/z, 4 bits for max−1, 8 bits material
  index) plus a per-block palette of material block-ids (≤256, multi-material
  allowed). The dense 16³ array is a transient edit buffer; VS greedy-merges it
  back to cuboids after each edit. Wire model: client→server edit is a tiny delta
  (touched voxel + material + brush size); server→client and disk is a full-blob
  resend of the cuboid list + palette via `TreeAttributes`.

**Key rollback takeaway from VS:** the packed-cuboid list is *derived and
order-independent* (a greedy merge with no per-edit inverse), so it is **not**
naturally LIFO-invertible. For our bit-exact rollback bar, the naturally
invertible and trivially-hashable substrate is the **dense 16³ array** (a voxel
toggle is its own inverse; a flat bitset + material bytes hashes
deterministically). Therefore we make the dense form authoritative and treat
cuboid packing as a *future* wire/mesh optimization, not a stored form.

## Current State (what exists today)

- `crates/game/src/voxel.rs`: `Chunk { voxels: Vec<Voxel>, collider: Vec<ColliderHandle> }`,
  `ChunkShape = ConstShape3u32<32,32,32>`, `CHUNK_VOXEL_COUNT = 32768`. `Voxel {
  kind: VoxelType }`, `VoxelType { Black, Air }`. `Chunk::flat_floor(depth)`
  fills `y < depth` with `Black`. The `collider` field is **vestigial** — always
  constructed empty; the real collider lives in rapier's `PhysicsState`.
- `crates/game/src/state.rs`: `Rollback::create_mesh(coords, chunk)` creates an
  entity, collects solid voxel grid points, stores the `chunk` component, builds
  a fixed rigid body at `coords * 32`, and attaches a per-voxel voxel collider.
  `Chunk` is a `Component<Chunk>` set once via `set_safe` (tier-2 snapshot undo).
  `GameDataUpdateKind::SetVoxelComponent(EntityKey, Option<Vec<Voxel>>)` is the
  render-update channel carrying a whole chunk's voxels.
- `crates/worldgen/src/lib.rs`: `generate_region(RegionCoords) -> Vec<(ChunkCoords, Chunk)>`,
  8×8×1 = 64 chunks, `y = 0` only; parity-checkerboard `flat_floor(8 or 12)`.
- `crates/client/src/renderer/bridge.rs`: `spawn_region_snapshot` clones
  `chunk.voxels` into a `VoxelData(Vec<Voxel>)` component;
  `drain_region_updates` handles `SetVoxelComponent` → insert/remove `VoxelData`.
  `meshing.rs` re-meshes on `Changed<VoxelData>` (greedy quads over `ChunkShape`).
- Region math: `REGION_CHUNKS = 8`, `REGION_SIZE = 256.0`; chunk world position
  is `coords * 32`. The world is currently only one chunk tall (~2 blocks).

## Design

### The three-layer model

Coarsest layer is authoritative; everything finer is sparse or derived.

```
Chunk (32³ voxels of geometry; region math unchanged):
  ├─ blocks: Vec<Block>              AUTHORITATIVE. len = 8 (2×2×2 blocks per chunk).
  │                                  Block = compact value, hashes trivially.
  ├─ chisel: BTreeMap<BlockIndex, ChiselData>
  │                                  SPARSE. Present iff a block is subdivided.
  │                                  Empty in the common (uniform) case.
  └─ voxels: TRANSIENT / DERIVED. Never stored, serialized, or hashed.
             Computed by game::derive_voxels(blocks, chisel) on demand:
             - sim side: to build the rapier collider at chunk creation
             - client side: to feed the mesher (VoxelData)
```

Block-level rich state ("block entities") is **not** in this model — see Open
Questions.

### Types (`crates/game/src/block.rs`, new module)

```rust
pub const BLOCK_VOXELS: usize = 16;              // 1 block = 16³ voxels = 1 m³
pub const CHUNK_BLOCKS: usize = 32 / BLOCK_VOXELS; // = 2 blocks per axis
pub const CHUNK_BLOCK_COUNT: usize = CHUNK_BLOCKS.pow(3); // = 8

/// Index of a block within a chunk, linear over CHUNK_BLOCKS³ (0..8).
/// Ordered so it can key a BTreeMap deterministically.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
pub struct BlockIndex(u8);

/// The authoritative per-cell gameplay value. Compact (Veloren-style): a kind
/// plus a few payload bytes for future block-state. Hashes trivially.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct Block {
    pub kind: BlockKind,
    pub data: [u8; 3],   // reserved for future block-state; zeroed for now
}

/// Gameplay material atom at block granularity. Extensible; replaces the
/// voxel-level Black/Air atom as the *authoritative* material identity.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum BlockKind {
    #[default]
    Air,
    Stone,
}

/// Dense 16³ sub-block detail for a chiseled block. Dense (not cuboid-packed)
/// because dense is naturally LIFO-invertible and trivially hashable — the
/// property VS's derived cuboid list lacks. Vec-backed (not [T; 4096]) to avoid
/// serde big-array friction and stay deterministic.
#[derive(Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ChiselData {
    occupancy: Vec<u64>,       // 64 words = 4096-bit bitset (1 = solid)
    material:  Vec<u8>,        // len 4096, palette index per voxel
    palette:   Vec<BlockKind>, // small per-block material list (VS-style, ≤256)
}
```

`Block`, `BlockKind`, `ChiselData` all satisfy `Default + Clone + Hash + Serialize
+ Deserialize` so `Chunk` remains a valid `Component<Chunk>`. `BlockIndex: Ord`
keys the `BTreeMap`.

### Reshaped `Chunk` (`crates/game/src/voxel.rs`)

```rust
#[derive(Debug, Serialize, Deserialize, Clone, ::borrow::Partial, Hash)]
#[module(crate)]
pub struct Chunk {
    pub blocks: Vec<Block>,                       // authoritative, len CHUNK_BLOCK_COUNT
    pub chisel: BTreeMap<BlockIndex, ChiselData>, // sparse
}

// Default must produce a full all-Air chunk (8 blocks), NOT an empty vec —
// Component<T> requires T: Default, and derive_voxels expects CHUNK_BLOCK_COUNT
// blocks. So Default is hand-written (mirroring today's all-air Chunk::default),
// not derived.
impl Default for Chunk {
    fn default() -> Self {
        Self {
            blocks: vec![Block::default(); CHUNK_BLOCK_COUNT], // all Air
            chisel: BTreeMap::new(),
        }
    }
}
```

The vestigial `collider` field is removed. There is **no** `voxels` field — the
voxel array is transient, produced only by `derive_voxels`, so it can never
become part of authoritative state and there is nothing to exclude from hashing.

### The derive path (`game::derive_voxels`, shared by sim and client)

```rust
/// Deterministic. Identical output on sim and client (single source in `game`).
/// Output layout is byte-identical to today's Chunk.voxels, so the mesher and
/// collider consume it unchanged.
pub fn derive_voxels(blocks: &[Block], chisel: &BTreeMap<BlockIndex, ChiselData>) -> Vec<Voxel> {
    // For each of the 8 blocks:
    //   if chisel.contains(idx): copy that block's 16³ occupancy+material into
    //                            its 16³ voxel sub-cube.
    //   else:                    fill the 16³ sub-cube uniformly from block.kind
    //                            (Air => empty voxels).
    // Voxel (x,y,z) for block (bx,by,bz) local (vx,vy,vz):
    //   world voxel = (bx*16+vx, by*16+vy, bz*16+vz), linearized via ChunkShape.
}
```

Membership in `chisel` is the *sole* authority for "this block is subdivided" —
there is no redundant `Chiseled` marker in `Block.kind` to keep in sync. The
derive step does at most 8 cheap map lookups per chunk.

`BlockKind → VoxelType` mapping (for the derived voxel material) is a small
total function in `block.rs` (`Air → Air`, `Stone → Black`, extensible).

### Consumers that change

- **`crates/worldgen/src/lib.rs`**: `generate_region` produces blocks. A helper
  builds either a whole block or a chiseled partial block. The parity-checkerboard
  keeps its sub-block floor heights (8/12 voxels) by emitting `ChiselData` for the
  partial top block — exercising derive-with-chisel, chiseled meshing, and chisel
  hashing through the real generated world. `generation_is_pure` extends to
  assert deterministic block contents.
- **`crates/game/src/state.rs`**: `create_mesh` becomes `create_chunk(coords,
  blocks, chisel)`: `derive_voxels` locally → collect solid points → build the
  fixed body + voxel collider (as today) → store the `Chunk { blocks, chisel }`
  component → emit the render update. Blocks are set-once → existing `set_safe`
  snapshot undo suffices; no new undo machinery.
- **Render-update enum**: `SetVoxelComponent` changes from carrying `Vec<Voxel>`
  to carrying block data (either reshape it or add `SetChunkBlocks(EntityKey,
  Vec<Block>, BTreeMap<BlockIndex, ChiselData>)`). The client derives.
- **`crates/client/src/renderer/bridge.rs`**: both paths converge on
  `derive_voxels` → `VoxelData`:
  - `spawn_region_snapshot`: read `chunk.blocks`/`chisel` → derive → `VoxelData`.
  - `drain_region_updates`: block-shaped update → derive → `VoxelData`.
  `meshing.rs` is untouched (still keys on `Changed<VoxelData>`).

### Wire / serialization

The `ServerPacket::Region(RegionId, Rollback)` snapshot and the
`SerializedRegion` parking blob now carry `blocks + chisel` per chunk (the
authoritative state), not a dense voxel array. The client derives voxels for
meshing; the sim derives them for the collider. This shrinks the wire payload and
keeps a single source of truth, matching Veloren's "wire decoupled from mesh."

All new wire fields are `u8` / `Vec<u64>` / `Vec<u8>` / `Vec<BlockKind>` — no
`usize` crosses the wire, respecting the wasm32 `usize`-sentinel hazard by
construction.

## Rollback & Determinism

- Authoritative state per chunk is `blocks + chisel`, both built from
  deterministically-ordered, integer/byte data (`BTreeMap`, `Vec`). No float
  math is introduced, so cross-machine determinism is unaffected.
- `Chunk` is a set-once `Component<Chunk>`; the existing tier-2 snapshot undo
  (`set_safe`) already gives `hash(before) == hash(after undo)`. No tier-1
  wrappers, undo scopes, or `#[emit]` changes this milestone.
- The derived voxel array is never part of hashed state, so it cannot desync the
  rollback hash.

## Testing

- **`crates/game` (rollback/hash suite, `hash_restore.rs`)**: a `Chunk` carrying
  `ChiselData` round-trips bit-exact under clone→hash and
  serialize→deserialize→hash; a set-once `Component<Chunk>` undo restores the
  hash exactly.
- **`derive_voxels`**: deterministic; a uniform-`Stone` chunk derives to the
  exact voxel layout the old `flat_floor` produced (regression anchor proving the
  mesher/collider see identical input); a chiseled block derives to the expected
  occupancy/material.
- **`crates/worldgen`**: `generation_is_pure` extends to assert deterministic
  block contents (still 64 chunks) and that the chiseled-slab path is emitted for
  the checkerboard heights.
- **`crates/client`**: a bridge test that `blocks → derive_voxels → VoxelData`
  produces a mesh equivalent to today's for a uniform chunk, and meshes a
  chiseled chunk without panicking.

## Module Layout

- New `crates/game/src/block.rs`: `Block`, `BlockKind`, `BlockIndex`,
  `ChiselData`, `derive_voxels`, block constants, `BlockKind → VoxelType`.
  Re-exported at the crate root (glob re-export, like the other modules).
- `crates/game/src/voxel.rs`: reshaped `Chunk`; keeps the `Voxel`/`VoxelType`
  atom and `ChunkShape`.
- `crates/game/src/state.rs`: `create_chunk`, render-update enum change.
- `crates/worldgen/src/lib.rs`: block-based generation + chiseled slab helper.
- `crates/client/src/renderer/bridge.rs`: derive on snapshot + live update.

## Open Questions (deferred, for follow-on milestones)

1. **Where does block-level rich state ("block entities") live** — a chunk-local
   `BTreeMap<BlockIndex, _>` (Minecraft/VS locality) vs first-class region ECS
   entities positioned at the block (entity-generic, reuses the existing entity
   system)? Deferred to the block-behaviors milestone.
2. **Sub-block cuboid packing for the wire** (VS-style merged cuboids) as a
   bandwidth optimization once edits are networked — the dense bitset stays the
   authoritative/rollback form regardless.
3. **Palette compression** of the block array when block diversity grows.
4. **Vertical world topology** (chunk stacking) — needed before break/place is
   meaningful, since the world is currently ~2 blocks tall.
5. **Global `BlockCoords`** addressing type, added with raycasting.

## Follow-on Milestones (natural sequence)

1. Vertical world / more play space (prerequisite for meaningful building).
2. Break/place interaction loop: input → raycast targeting → block edit →
   re-derive/re-mesh + incremental collider rebuild → rollback-reconciled
   networking (new `GameEventKind`), with undo-safe block-edit machinery.
3. Chiseling tool (voxel-level edits within a block).
4. Block entities / behaviors (resolves Open Question 1).
5. Palette + cuboid-packing optimizations as scale demands.
