# Vertical Chunk Stacking — Design

**Date:** 2026-07-06
**Status:** Design (approved for spec write-up)
**Scope:** Data-model foundation only — no new gameplay.

## 1. Motivation

Today the world is a flat, single layer of 32³ chunks: `worldgen::generate_region`
loops over `x, z` only and emits every chunk at `ChunkCoords::new(x, 0, z)` with a
scalar `floor_height`. Terrain occupies the bottom third of one 32-tall chunk;
there is no depth, no height, no caves, no mountains.

This spec restructures a region's terrain into **unbounded vertical columns** of
stacked sub-chunks, so the world gains a real vertical dimension. It deliberately
stops at the data model: it does **not** add gravity, falling, vertical region
crossing, real heightmap terrain, or digging. Those become tractable *after* this
foundation exists.

### Key enabling fact

The voxel / chunk / collider / mesh layer is already latently 3D. `ChunkCoords`
carries a `y`; `create_mesh` positions colliders at `(x*32, y*32, z*32)`
(`state.rs`); `iso_to_transform` maps all three translation components
(`convert.rs`); `build_chunk_mesh` greedy-meshes the full 32³ voxel array
(`meshing.rs`). A `y > 0` chunk would render and collide correctly *today*. The
flatness lives in only three places:

1. `RegionCoords { x, z }` + `world_offset()` hardcoding Y=0 (`protocol.rs`)
2. `worldgen::generate_region` looping x/z only at `y=0`, scalar `floor_height`
3. Horizontal-only boundary handoff (`scan_boundaries`, `departure_offset`,
   `rebase_isometry`)

The model chosen here (column-of-sub-chunks, Veloren "Chonk" style) leaves #1 and
#3 **untouched** — verticality is a region-local data change, not a change to the
2D region topology that the rollback / handoff / parking netcode is built around.
This matches the existing invariant "sims stay region-local, the world offset
exists only at the render boundary."

## 2. Reference-game basis

All three surveyed shipping voxel games — Minecraft, Vintage Story, Veloren — use
the **column model** (2D horizontal chunk coords, verticality as a stack of cubic
sub-chunks *inside* the column). None uses full-3D chunk coordinates in
production. The two techniques we adopt:

- **Sentinel-bounded columns (Veloren):** a column stores a small populated band
  plus constant sentinels — solid material below the band, air above — so the
  column is effectively **unbounded** in height while only the surface transition
  zone is materialized.
- **Homogeneity collapse (Minecraft single-value palette / Veloren sub-chunk
  homogeneity):** a fully-uniform sub-chunk collapses to O(1) storage. We take the
  **minimal** form of this: only all-**air** sub-chunks collapse (see §4).

Vertical is loaded all-or-nothing per column (you tile *horizontally* with a view
radius; the whole vertical stack comes along), which is only affordable because
empty sub-chunks are near-free.

## 3. Non-goals

Explicitly out of scope for this change:

- Gravity / falling / climbing. Players keep `gravity_scale(0.0)`.
- Vertical region crossing. Regions still tile the plane; you can never leave a
  region up or down. `RegionCoords` stays `{ x, z }`.
- Real terrain: heightmaps, hills, mountains, caves, ores. Worldgen keeps today's
  parity floor, merely re-expressed as a proper column.
- Digging / block editing gameplay. `WriteVol` is specified and tested, but no
  gameplay path calls it yet.
- Vertical simulation gating (only ticking the player-occupied band). Not needed
  until there is vertical gameplay.

## 4. Core types

New module `crates/game/src/volume.rs` (Bevy-free, deterministic, like the rest of
`game`). `lib.rs` glob-re-exports it at the crate root, consistent with the
existing macro-expansion requirement.

### 4.1 `SubChunk`

```rust
/// One 32³ slice of a column. Only the all-air case is collapsed; every other
/// uniform case (including all-stone) stays `Dense`. All-stone Dense sub-chunks
/// simply get no collider (see §7) and cost one chunk's worth of memory — an
/// accepted trade for a minimal enum.
pub enum SubChunk {
    Air,
    Dense(Box<Chunk>),
}
```

Rationale for air-only collapse (user decision): the below-band sentinel already
absorbs the overwhelmingly common all-stone volume (everything under the surface),
so a second `Uniform(Stone)` variant would rarely fire and would introduce a
canonicalization ambiguity (`Dense`-all-stone vs `Uniform(Stone)`) that threatens
the bit-exact hash bar. Air-only collapse keeps exactly one representation per
content.

### 4.2 `Column`

```rust
/// An unbounded vertical column of sub-chunks over one region-local (x, z) cell.
/// Storage is sparse: only `sub_chunks` is materialized; everything below the
/// band reads as `below`, everything above reads as `above`.
pub struct Column {
    /// Sub-chunk index (signed) of `sub_chunks[0]`.
    base_sub_y: i32,
    /// Sentinel block returned for any sub_y < base_sub_y (terrain: Stone).
    below: Block,
    /// Sentinel block returned for any sub_y >= base_sub_y + sub_chunks.len()
    /// (terrain: Air).
    above: Block,
    sub_chunks: Vec<SubChunk>,
}
```

`below`/`above` are stored as `Block` (self-describing; a future sky/void region
could invert them) rather than hardcoded, but for terrain `below = Block::new(stone)`
and `above = Block::new(BlockId::AIR)`.

### 4.3 Coordinates

- `ChunkCoords.y` changes from `usize` to **`i32`** so sub-chunks can sit below
  the origin. `x`/`z` stay region-local `usize` (0..`REGION_CHUNKS`); they never
  go negative within a region, so they are left unchanged to minimize churn.
- New `ColumnCoords { x: usize, z: usize }` (region-local, 0..`REGION_CHUNKS`)
  addresses a column within a region. This is the 2D key worldgen and the region
  produce; a materialized sub-chunk maps to `ChunkCoords { x, sub_y, z }` for the
  existing `create_mesh` path.

The `ChunkCoords` `usize → i32` change is the one breaking coordinate edit. It is
also the fix for the `wasm32 usize sentinel` hazard on this axis: a signed
sub-index never serializes `usize::MAX`.

## 5. Volume traits

```rust
pub trait ReadVol {
    /// Block at a position. Position frame is defined per-impl (see below).
    fn get(&self, pos: IVec3) -> Block;
}

pub trait WriteVol {
    /// Set a block; returns the previous block at that position.
    fn set(&mut self, pos: IVec3, block: Block) -> Block;
}
```

Position frames:

- `impl ReadVol/WriteVol for SubChunk` — `pos` is local voxel coords `0..32` on
  each axis.
- `impl ReadVol/WriteVol for Column` — `pos.x`/`pos.z` are `0..32` (voxel within
  the column footprint); `pos.y` is a **column-global voxel Y** (signed, unbounded).
  `Column::get` computes `sub_y = pos.y.div_euclid(32)`, dispatches to the sentinel
  or the stored `SubChunk`, and indexes the remainder.
- A later `RegionGrid` impl (not in this change) will address blocks in region-local
  voxel space across all 64 columns; the trait is defined now so the mesher/physics/
  bridge are written against it and that impl is free when it lands.

Meshing, collider building, and the render bridge are rewritten to be generic over
`ReadVol` (`fn build_chunk_mesh<V: ReadVol>(vol: &V, ...)`) instead of taking a
concrete `Chunk`.

`WriteVol::set` on a `Column` grows the band when `pos.y` falls outside it
(prepending `SubChunk::Air`s and lowering `base_sub_y`, or appending), then
re-normalizes (§6). No gameplay calls this yet, but it is specified and tested so
the write path is correct for future digging.

## 6. Determinism & canonicalization

The rollback bar is bit-exact `hash(before) == hash(after undo)`, and state hashing
is `crc32fast` over the `bincode`-serialized bytes. The sparse form therefore MUST
be **canonical** — exactly one byte representation per logical content. A single
`Column::normalize(&mut self)` enforces:

1. **Air collapse:** any `SubChunk::Dense` whose 32³ is entirely `BlockId::AIR`
   becomes `SubChunk::Air`. Worldgen never emits a uniform-air `Dense`; every
   `set` re-checks the touched sub-chunk.
2. **Trailing-air trim:** trailing `SubChunk::Air` entries (equal to the `above`
   sentinel) are dropped.
3. **Leading trim (bounded):** leading `SubChunk::Air` entries are dropped only
   when `above == below`; for terrain `below = Stone ≠ Air`, so leading entries are
   never trimmed and `base_sub_y` is fixed by worldgen at the lowest materialized
   sub-chunk. (In foundation scope nothing writes stone below `base_sub_y`, so the
   below-band stays pure sentinel.)

`normalize` MUST be **idempotent** and run after construction and after every
`WriteVol::set`. Because all-stone stays `Dense` (no `Uniform(Stone)` variant),
there is no dual representation to reconcile — the air-only rule is sufficient for
canonicality.

`Column`, `SubChunk`, `ChunkCoords`, `ColumnCoords` derive `Serialize`/`Deserialize`
(bincode) and hash via their serialized bytes, matching existing chunk state.

### Interaction with the undo/rollback layer

Terrain chunks are inserted into the physics/ECS world as individual fixed-body
entities via `create_mesh`; a region does not hold a `[[Chunk; N]; N]` field, so
this change adds no new tier-1/tier-2 undo-wrapped field. Columns are a
worldgen/serialization-side unit that feeds `create_mesh` per materialized
sub-chunk. Edits that would mutate terrain at runtime remain out of scope, so the
`#[rollback]` surface is unchanged by this spec.

## 7. Integration points

- **Worldgen** (`crates/worldgen/src/lib.rs`): `generate_region(coords, solid)`
  returns `Vec<(ColumnCoords, Column)>` (64 columns). For each column:
  `below = Block::new(solid)`, `above = Block::new(BlockId::AIR)`,
  `base_sub_y = 0`, and `sub_chunks = [Dense(flat_floor_with(depth, solid))]` —
  the existing straddling parity-floor chunk. After trailing-air trim this is a
  one-`Dense`-sub-chunk column: tiny. Still a pure deterministic function of region
  coords; regenerate-on-wake stays bit-exact.
- **Region** (`crates/game/src/region.rs`): `from_chunks` → `from_columns`,
  iterating each column's materialized sub-chunks and calling
  `create_mesh(ChunkCoords { x, y: sub_y, z }, chunk)` as today.
- **Meshing** (`crates/client/src/renderer/meshing.rs`): generic over `ReadVol`;
  `SubChunk::Air` produces no mesh; only `Dense` sub-chunks are meshed.
- **Physics** (`create_mesh`, `state.rs`): one collider per `Dense` sub-chunk,
  positioned at `sub_y*32` (already honored). `SubChunk::Air` → no collider.
  Interior all-stone `Dense` sub-chunks get a collider like any other `Dense`
  chunk; the below-band **sentinel** stone (unmaterialized) gets **no** collider —
  accepted, since no gameplay reaches beneath the surface in this scope.
- **Render bridge** (`crates/client/src/renderer/bridge.rs`): unchanged in
  structure — chunks are placed by their body transform in 3D. `world_offset()`
  keeps Y=0 for the region root; each sub-chunk's own `sub_y*32` supplies its
  height, exactly as a `y>0` chunk would today.

## 8. Serialization / parking

`SerializedRegion` serializes the sparse column form (`base_sub_y`, `below`,
`above`, `Vec<SubChunk>`). `SubChunk::Air` serializes to a discriminant byte, so
parked blobs stay small. Round-trip MUST reproduce the canonical form exactly:
`park → restore` yields an identical hash, preserving the existing bit-exact
restore guarantee.

## 9. Testing

Unit (`crates/game`):
- `normalize` idempotence: `normalize(normalize(c)) == normalize(c)`.
- Sentinel reads: `get` below `base_sub_y` returns `below`; above the band returns
  `above`; within a `Dense` returns the stored block; within an `Air` returns air.
- Air collapse: a `Dense` filled with air normalizes to `SubChunk::Air`.
- Trailing-air trim: appended air sub-chunks vanish after normalize; `base_sub_y`
  and length are canonical.
- `WriteVol::set` growth: writing above/below the band grows and re-normalizes
  correctly; `set` then inverse `set` restores the exact hash (feeds the rollback
  bar).

Determinism:
- `hash(column) == hash(deserialize(serialize(column)))`.
- Two `generate_region` calls with equal coords produce byte-identical columns.

Worldgen:
- Column count is `REGION_CHUNKS² = 64`; deterministic across calls; the rendered
  floor matches the pre-change parity floor visually (regression).

Client (`crates/client`): existing headless suite still passes; a `Dense`+`Air`
column meshes to the same triangles as the old single-chunk region for the floor
band.

## 10. Rollout

Incremental, each step compiling and testable:

1. Add `volume.rs`: `SubChunk`, `Column`, `ReadVol`/`WriteVol`, `normalize`, unit
   tests. Change `ChunkCoords.y` to `i32`; add `ColumnCoords`. No callers yet.
2. Point meshing / collider building / bridge at `ReadVol` (mechanical; `Chunk`
   already satisfies a thin `ReadVol` adapter, or is wrapped in a one-`Dense`
   `Column`).
3. Switch `generate_region` to columns and `Region::from_chunks` → `from_columns`.
4. Update `SerializedRegion` to the column form; verify park/restore hash equality.
5. Update the `justfile`/tests if any command changed; run `just test` +
   `just test-client`.

## 11. Related specs

- `2026-07-06-block-abstraction-data-model-design.md` — the `Block`/`BlockId` layer
  this builds on.
- `2026-07-06-data-driven-block-registry-design.md` — source of the `solid` /
  stone `BlockId` worldgen resolves.
- `2026-07-04-terrain-colliders-design.md` — the per-chunk collider path extended
  here per sub-chunk.
- `2026-07-04-scalable-undo-hashing-design.md` — the bit-exact hashing bar §6
  must satisfy.
- `2026-07-05-multi-region-world-design.md` — the 2D region topology this change
  deliberately leaves untouched.
