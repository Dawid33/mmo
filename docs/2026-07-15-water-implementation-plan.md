# Deterministic water implementation plan

**Status:** proposed  
**Date:** 2026-07-15  
**Scope:** world water, local flow, swimming/buoyancy, rendering, rollback,
streaming, and eventual cross-region flow

## Outcome

Implement water as a separate, rollback-hashed liquid volume at the existing
1 m block resolution. Store an integer amount per cell, simulate only an active
frontier at a lower cadence than the 50 ms game tick, render it with a dedicated
transparent mesh/material, and sample it directly for player physics. Do not
put dynamic water through the solid `Chunk` collider or opaque voxel mesher.

The first playable milestone is static worldgen oceans that render correctly,
do not collide as rock, and support swimming. Local dynamic flow follows. Flow
across region ownership boundaries is a distinct milestone because the current
region threads have no deterministic tick barrier.

## What exists today

- `assets/blocks/blocks.ron` reserves stable `BlockId(5)` for `water`.
- `worldgen::generate_region` fills empty blocks through sea level with that ID,
  including coarse LOD output.
- `VoxelVisibility`, `collider_cells`, `surface_voxel_y`, and LOD solidity checks
  distinguish only air from non-air. Water is consequently opaque, collidable,
  and counted as ground.
- A `Chunk` contains eight 1 m blocks (2 x 2 x 2) and derives 16 subdivisions per
  block for meshing and chisel collision. Physics/render coordinates use these
  subdivisions, so a water cell edge is 16 simulation units.
- `GameData` is rollback-generated and hash-checked. Region ticks execute on the
  server and predicted client, and reconciliation rolls back and replays them.
- Rendering receives dense derived voxels through `SetVoxelComponent` and sends
  them through an opaque binary mesher and pulled-quad shader path.
- Player bodies are kinematic. Rapier forces do not act on kinematic bodies, so
  swimming must be implemented in the game controller before the Rapier step,
  not by attaching a buoyancy force or solid/sensor water collider.
- `TerrainSnapshot` and the LOD cache currently contain only solid-style chunks.

## Research and model choice

Jos Stam's *Stable Fluids* is the standard real-time Eulerian reference, but it
uses velocity/density grids, advection, diffusion, and a pressure projection.
That is a good visual CFD model, not a good default authoritative model here:
it is dense, iteration-order sensitive, floating-point heavy, and expensive to
undo and replay ([paper](https://graphics.stanford.edu/courses/cs448-01-spring/papers/stam.pdf)).

Cellular/lattice models are discrete and use local transition rules, making a
sparse active set and exact integer conservation natural. Real-time game work
has demonstrated this family of models, though its lattice-gas formulation is
more detailed than this game's MVP needs ([Judice, Coutinho, and Giraldi,
2008](https://www.researchgate.net/publication/257527902_A_Cellular_Automata_Framework_for_Real_Time_Fluid_Animation)).

Use a simpler volume-transfer cellular model for authoritative gameplay. It is
an intentional approximation: it should produce pools, waterfalls, spreading,
draining, and conserved volume, but it will not model turbulence, vortices,
breaking waves, or realistic pressure in v1. Visual waves belong in the shader.
NVIDIA's real-time water chapter supports separating low-frequency geometry
from high-frequency animated normals, including depth-aware shore treatment
([GPU Gems, chapter 1](https://developer.nvidia.com/gpugems/gpugems/part-i-natural-effects/chapter-1-effective-water-simulation-physical-models)).

Bevy 0.18 already provides the required material hooks and transparency modes:
use a custom/extended material for the first renderer, and consider OIT only if
ordinary alpha sorting is visibly unacceptable ([extended material
example](https://bevy.org/examples-webgpu/shaders/extended-material/),
[transparency example](https://bevy.org/examples/3d-rendering/transparency-3d/),
[OIT background](https://bevy.org/news/bevy-0-15/#order-independent-transparency)).

Rapier documents that kinematic bodies are immune to forces, matching the
current player implementation. Direct controller integration is therefore the
correct first gameplay path ([Rapier character controller](https://rapier.rs/docs/user_guides/rust/character_controller/),
[gravity/force behavior](https://rapier.rs/docs/user_guides/rust/rigid_body_gravity)).

## Authoritative representation

Add `crates/game/src/water.rs` with these conceptual types (names may change
during implementation):

```rust
pub const WATER_UNITS_PER_CELL: u16 = 4096;
pub const WATER_UNITS_PER_LAYER: u16 = 256; // 16 x 16 micro-voxel footprint

pub struct WaterCell {
    pub amount: u16, // 0..=4096 in v1
    pub flags: u8,   // SOURCE is the first flag
}

pub struct WaterChunk {
    pub cells: [WaterCell; CHUNK_BLOCK_COUNT], // aligned with a terrain Chunk
    pub active_mask: u8,
    pub settle_age: [u8; CHUNK_BLOCK_COUNT],
}

pub struct WaterCellPos { pub x: i32, pub y: i32, pub z: i32 }
```

One unit is 1/4096 m3 and one full cell is 1 m3. This maps exactly to the
existing 16 x 16 x 16 block subdivisions. Rendering may quantize height to 16
levels using `ceil(amount / 256)` while gameplay uses the exact integer amount.

Store non-empty `WaterChunk`s in an ordered map keyed by region-local
`ChunkCoords`, aligned with the existing 2 x 2 x 2 block chunks. Put that map in
`GameData` under rollback control. Commit a changed `WaterChunk` once per water
step so the undo log stores one before-image per touched chunk, rather than one
entry per cell mutation. Keep the active mask authoritative and rollback-hashed:
sleep/wake state changes future evolution and cannot be render-only state.

Reserve `BlockId(5)` as water's material/render identity, but stop representing
water cells as solid `Block`s. Extend the block registry with an explicit class,
for example `kind: Air | Solid | Liquid`, so future lava or foliage does not
reintroduce `id != AIR` as a solidity rule. Registry data itself is immutable;
the hot simulation path should use resolved IDs or a small immutable property
table, never string lookups.

### Worldgen migration

Change generation to return both solid columns and water chunks, preferably a
`GeneratedRegion { columns, water }`. Below-sea empty cells become water entries
and remain air in solid terrain. `Region::from_columns`/`from_chunks`, local
server setup, server setup, cache keys, and tests must accept the combined
result. Bump the worldgen cache format/key explicitly.

Before this migration lands, add characterization tests proving how much water
the selected spawn region contains. After migration, prove that the combined
solid-plus-water occupancy is identical to the old generated material layout,
while solid colliders and `surface_voxel_y` exclude water.

## Deterministic local flow

Run water every two game ticks initially (10 Hz), after input/controller intent
has been collected and before movement physics samples the next state. Keep the
cadence and all constants in `game`; the client and server must execute the same
step during prediction/replay.

Use integer-only transfer and a canonical `WaterCellPos` ordering. A prototype
should compare two deterministic solvers before locking the rule:

1. **Recommended baseline: ordered edge sweep.** Visit each relevant cell and
   each undirected edge once. Move as much as possible downward, then relax the
   four horizontal edges toward equal fill, capped by `MAX_FLOW_PER_STEP`.
   Alternate the horizontal axis/direction permutation from `tick % 4` to avoid
   permanent east/north bias. In-place updates are deterministic because the
   order is part of the protocol.
2. **Fallback: snapshot plus flux buffer.** Compute proposed edge transfers from
   an immutable snapshot, deterministically ration destination capacity, then
   commit. This is less order-biased but needs more scratch memory and logic.

Both candidates must satisfy these non-negotiable rules:

- `sum(amount)` changes only through explicit sources, sinks, or cross-region
  flux events; normal movement is bit-exactly mass-conserving.
- A destination never exceeds `WATER_UNITS_PER_CELL`, and a source never goes
  negative. Upward pressure/fountains are deferred; v1 water is incompressible
  and gravity-led.
- A solid block has zero liquid capacity. A chiseled block is treated as solid
  for v1; fractional coexistence with chisel geometry is a later feature.
- Any changed cell wakes itself and its six face neighbors. A cell sleeps only
  after it and those neighbors produce no transfer for a fixed number of water
  steps. Settled oceans therefore cost zero per tick.
- Missing vertical data uses the column sentinels consistently. Missing
  horizontal neighbor-region data is a closed boundary until the cross-region
  phase is implemented; expose a diagnostic counter so this is not invisible.
- Sources are explicit flags/events, not magic amounts. A source refills its
  cell to full at the beginning of a water step; the added mass is metered.

Do not parallelize the authoritative solver until the scalar implementation has
golden hashes. Later parallel work must preserve the exact edge partition and
commit order, not merely look equivalent.

### Suggested solver tests

- A full cell over empty space falls and conserves exactly 4096 units.
- Water fills a one-cell basin, equalizes across a symmetric basin, flows around
  an obstacle, forms a waterfall, and drains after an outlet opens.
- A sealed lake reaches sleep and performs zero cell visits thereafter.
- Rotated/mirrored fixtures stay within a documented one-unit tolerance or have
  exact symmetry if the flux-buffer solver is selected.
- Seeded randomized solid/water grids preserve mass and bounds for 10,000 steps.
- `hash(before) == hash(after undo)` for one step, wake/sleep, source refill,
  terrain edit, and multi-step rollback/replay.
- Native and wasm execute a shared golden fixture to the same serialized bytes
  and state hash.

## Terrain edits and events

All solid edits must go through one API that atomically updates terrain,
collision, and water:

- Placing solid into water displaces volume in deterministic face order. Reject
  the edit if all legal destinations are full; never silently delete water.
- Removing solid wakes adjacent water.
- Removing/placing a source is a game event and rollbackable.
- Chiseling a wet cell is rejected in v1 with a clear reason.

Add events such as `SetBlock`, `PlaceWaterSource`, and the later
`ApplyBoundaryWaterFlux`; do not mutate water from a render or server-only side
channel. This keeps client prediction and the existing event reconciliation
model coherent.

## Player water physics

Add a `WaterController` before `PhysicsController`, or integrate the sampling
into the movement controller if ordering is clearer. Query water amount at the
capsule bottom, center, and head; derive a deterministic submerged fraction.
Because the player is kinematic, directly modify intended velocity:

- scale downward acceleration toward neutral buoyancy as submersion rises;
- apply separate horizontal and vertical drag;
- allow jump/swim input to add upward swim acceleration while submerged;
- cap ascent and descent speeds;
- expose `feet_wet`, `swimming`, and `head_submerged` state for animation,
  audio, and the underwater render effect.

Start with zero current coupling. Once the water solver records signed recent
edge flux, derive a small cell current and add bounded advection to the player.
Do not infer current from iteration order or render animation.

Acceptance should include entering/exiting at a shoreline, floating idle,
diving, swimming under a ceiling, falling through a waterfall, rollback while
crossing the surface, and region handoff while submerged.

## Rendering

Introduce a separate bridge update, for example
`SetWaterChunk(ChunkCoords, Option<WaterChunkRenderData>)`. It must be a
last-write-wins replacement on both Do and Undo, following `SimTarget` and
`SetVoxelComponent` bridge semantics. Do not send a full region water snapshot
on every cell change: coalesce all mutations in a water step to one update per
touched water chunk.

Add `renderer/water/` rather than complicating the opaque mesher:

- Build top faces at `amount / 4096` cell height and exposed side faces down to
  the neighboring surface. Cull faces between two full adjacent cells. A first
  implementation may emit ordinary Bevy meshes; adopt pulled records only after
  profiling proves upload or memory pressure.
- Use a forward transparent extended material with depth-aware color/alpha,
  Fresnel-weighted reflection tint, scrolling normal maps, and shoreline foam
  keyed by shallow depth or large height difference. Visual time is render-only
  and must never feed the simulation hash.
- Use ordinary alpha blending first. Sort water chunks by camera distance and
  split meshes if artifacts are severe. Gate OIT behind a measured visual need
  because it costs additional GPU memory.
- Exclude water from terrain AO/visibility connectivity so a sea does not close
  the solid chunk portal graph. Water gets its own conservative AABB and remesh
  queue with the same in-flight/upload budget pattern as terrain.
- Add an underwater fullscreen effect only after `head_submerged` is available:
  fog/color absorption first, then optional distortion/caustics. Bevy 0.18's
  fullscreen material API is suitable, but WebGL2 must retain a simple fallback.

LOD v1 should render generated/settled water as a flat sea surface and omit
transient dynamic details. Later, extend `TerrainSnapshot` to a broader
`WorldSnapshot` carrying a compact water surface representation and include its
hash in LOD invalidation. Avoid downsampling water as opaque volume; distant
water needs a surface mesh, not a filled solid cube.

## Networking, persistence, and region boundaries

Water state inside `Rollback` naturally travels in initial `ServerPacket::Region`
snapshots and parked serialized regions. Add a snapshot schema/version before
shipping the change because bincode layouts will change.

Normal local water evolution should not produce per-cell network packets: the
server and predicted client run the same integer solver. Authoritative game
events cover edits and sources. Existing state-hash/reconciliation tests then
detect divergence.

Cross-region flow cannot safely be a direct neighbor-thread write. Region
threads tick independently and manager delivery order is asynchronous. Add a
water-specific manager protocol in a later phase:

1. At water tick `N`, each active region publishes a one-cell-deep boundary
   strip for each horizontal face, versioned by region and tick (keep the
   message extensible to deeper halos).
2. The `WorldManager` pairs adjacent strips and computes each shared edge once,
   owned by the lexicographically smaller region pair.
3. It emits equal-and-opposite `ApplyBoundaryWaterFlux` events for tick `N+1`,
   sorted by world cell coordinate. Applying either half twice is rejected with
   a `(pair, tick)` idempotency token.
4. Clients receive the authoritative boundary events. A late event uses normal
   rollback/replay; no wall-clock state participates.
5. If one region is parked, either wake it before opening the boundary or keep
   the edge closed. Never flow into an untracked void. Add bounded wake rules to
   prevent water from recursively loading the world without a player.

The equal-and-opposite event pair and manager ledger must prove global mass
conservation across handoff, retry, region unload/reload, and server restart.

## Delivery phases

### Phase 0: semantics and characterization

- Add registry material class and helpers (`is_solid`, `is_liquid`,
  `occludes`, `has_collision`).
- Replace gameplay `id != AIR` checks where their actual meaning is solidity.
- Add current-worldgen characterization and collider/surface tests.
- No visual change is required yet.

**Gate:** existing hashes change only where the corrected water semantics demand
it; all solid-only tests retain behavior.

### Phase 1: static playable water

- Split worldgen solid and water output; add rollback water storage and schema
  versioning.
- Add water bridge, basic surface/side mesh, transparent material, and flat LOD.
- Add controller sampling, drag, buoyancy, swim input, and head-submerged state.
- Water is settled and does not flow.

**Gate:** generated ocean renders on native and WebGL2, has no terrain collider,
does not count as spawn ground, and the player can enter, float, dive, and leave.

### Phase 2: local deterministic flow

- Implement and benchmark both solver candidates on identical fixtures; lock
  the simpler one meeting symmetry and tick budget.
- Add active masks, sources/sinks, terrain edit interaction, coalesced render
  updates, rollback fuzz tests, and metrics.

**Gate:** exact conservation/undo tests pass; a settled sea visits zero cells;
p95 water work is below 1 ms per active region in a release benchmark and a
20-tick client replay remains within the existing keep-up budget.

### Phase 3: cross-region flux

- Add boundary-strip outputs, manager pairing/ledger, stamped flux events,
  parked-neighbor policy, and retry/idempotency tests.

**Gate:** a reservoir drains across a region seam with no mass change, visible
seam, duplicate application, or hash divergence under delayed/reordered relay.

### Phase 4: rendering polish and broader bodies

- Currents, foam, improved normals, underwater post-process, audio hooks, and
  optional dynamic-water LOD.
- Add buoyancy for dynamic rigid bodies only when such bodies exist. For them,
  approximate displaced volume by deterministic sample points and journal all
  force/velocity changes through the existing physics rollback path.

## File-level change map

- `crates/game/src/registry.rs`, `assets/blocks/blocks.ron`: material semantics.
- `crates/game/src/water.rs`, `state.rs`, `region.rs`, `protocol.rs`: water state,
  step, events, rollback, and render updates.
- `crates/game/src/block.rs`, `voxel.rs`, `lod.rs`: solid/liquid separation.
- `crates/game/src/camera.rs` or a new movement/water controller: swimming.
- `crates/game/src/world_manager.rs`, `region_runner.rs`: cross-region phase.
- `crates/worldgen/src/lib.rs`, `cache.rs`: split generation and cache bump.
- `crates/client/src/renderer/bridge.rs`, `renderer/water/`, `renderer/mod.rs`:
  water projection, meshing, material, and underwater effect.
- `crates/server/src/region_threads.rs` and local-server mirrors: boundary relay.
- `assets/shaders/water.wgsl` plus water normal/foam assets.
- `justfile`: add a release water benchmark recipe if the benchmark becomes a
  required workflow; otherwise keep the existing recipes unchanged.

## Instrumentation and final verification

Record per-region water metrics: active/sleeping cells, visited cells, moved
units, source/sink delta, boundary-blocked attempts, touched chunks, undo bytes,
step duration, render updates, and water mesh queue depth. Include water time in
the existing tick breakdown instead of hiding it under boundary scan.

Required final checks for each implementation phase:

```text
just fmt-check
just clippy
just test
just test-full
```

Rendering phases also require native and WebGL2 screenshots or recordings;
performance phases require the release benchmark and keep-up evidence. Slow
water fixtures must follow the repository's `#[ignore = "slow: >100ms ..."]`
policy.

## Explicit non-goals for the first release

- Navier–Stokes, SPH, lattice-Boltzmann, turbulence, or realistic wave dynamics.
- Water sharing the chisel micro-volume with solid material.
- Erosion, sediment, temperature, freezing, evaporation, or mixing liquids.
- Arbitrarily loading distant regions to follow an unattended flood.
- GPU-authoritative simulation. GPU animation is visual only; authoritative
  water remains integer CPU state shared by native and wasm.

These can be added behind the same liquid-state boundary without replacing the
solid terrain, rollback, or rendering architecture.
