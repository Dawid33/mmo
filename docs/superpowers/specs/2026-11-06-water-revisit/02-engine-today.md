# 02 — The engine today, as water would meet it

**Date:** 2026-11-06 · **Status:** code survey only (no builds, no tests run) · develop @ 436b656  
**Purpose:** re-check the 2026-09-22 water design
(`../2026-09-22-voxel-water-efficiency/00-synthesis.md`, "User decisions": deterministic in-sim
solver on server and every client, bodies + frontier CA every 4 ticks, finite water everywhere,
seam outflow as stamped events like `EntityArrived`) against the engine as it is now. That design
assumed 64 m regions, 2-block chunks and 25 predicted regions per client. Nothing from it was
built: there is no `prototypes/water/`, and no git commit mentions a water solver. The
2026-09-22 prototype plan (`docs/superpowers/plans/2026-09-22-water-prototype.md`) is unexecuted.

Line numbers are from develop at 436b656.

---

## 1. Water today

### 1.1 Representation

- Water is an ordinary block: `(id: 5, name: "water", textures: All("water.png"))`
  (`assets/blocks/blocks.ron:17`). It has no `solid:` key, so it takes the default
  `solid: true` (`crates/game/src/registry.rs:37-38`, `default_solid` at `:61`), the default opaque
  render, and no hardness. Ice (`id 23`, `:35`) and glacier ice (`id 24`, `:36`) are ordinary solid
  blocks.
- No `Liquid` kind exists. The registry knows `solid`, `render` (Opaque/Cutout) and
  `collider` scale only.
- **Water collides.** `collider_cells` and `push_block_cells` give a block a collider iff
  `id != AIR && is_solid(id)` (`crates/game/src/block.rs:397, 441, 448`). A chunk that is all water
  is a uniform full cube, so genesis gives it one `CHUNK_UNITS` cuboid
  (`crates/game/src/state.rs:884`). The rapier KCC therefore stands on the sea surface: there is no
  swimming and no sinking.
- **The sim is inconsistent about it.** Several sites hard-code `BlockId(5)` as "not ground", even
  though physics treats it as ground:
  - `pathfind.rs:32-44`: `WATER = BlockId(5)`, `is_water`. Water is neither passable nor solid,
    so A* neither walks in it nor stands on it.
  - `player.rs:138`: player restore treats water as clear space.
  - `item_piles.rs:210`: pile support excludes water, so piles sink to the floor.
  - `mining.rs:81`: water cannot be mined.
  - `slime.rs:42`: `SLIME_NO_SPAWN_BLOCK = BlockId(5)`.
  - Client `commander/height.rs:30, 69-83`: the height grid keeps the topmost non-water voxel as
    "ground"; the water id is passed in.
- **The edit ray treats water as solid.** `edit.rs:92-95` (`cell_solid`: `id != AIR`) and
  `:133-138`. A creative/fortress break clears a water block like any other block, and a place
  lands against a water face. The DF tool validity in `orders.rs:184, 198, 482` (`== AIR` tests)
  likewise treats water as "not air", i.e. solid.
- **Swimming code:** none. `pathfind.rs:3` says "no swimming", and TODO.md:15 lists
  "swimming/wading" as deferred. `grep -i swim|buoyan` finds nothing else.

### 1.2 Where worldgen puts it

- **Sea.** `SEA_LEVEL = 64.0` m and `SEA_TOP_BLOCK = 63` (`crates/worldgen/src/pipeline.rs:23, 32`).
  Every column fills with water from just above its ground up to `water_top`, which is
  `SEA_TOP_BLOCK` where only the sea applies (`crates/worldgen/src/lib.rs:142-147`, written at
  `:667-671` through `water_material` `:274-276`). The sea is implicit, "everything below SEA_LEVEL
  floods" (`forged.rs:616`), but it is materialised as real water blocks in chunks. A full-water
  sim chunk is `ChunkBlocks::Uniform(water)` (`voxel.rs:25-30`), which is cheap to store but is a
  solid cuboid collider.
- **Forged lakes and rivers.** `Patch::sample` returns `(profile, water_top, lake)`. The forged path
  computes `water_top = max(SEA_LEVEL + w − 1, SEA_TOP_BLOCK)` from the refined per-point water
  surface (`forged.rs:1225-1229`). Lake and river water surfaces snap to whole blocks, with
  carved channels, levees and berms per CLAUDE.md. The surface block under water is always whole
  (`lib.rs:167-170`). Lake and river beds are sand (`lib.rs:249`).
- **Ice.** Lake and sea water under 0 °C gets `ice_blocks()` (1–4) of `ice` from the top
  (`forged.rs:594-600`; `lib.rs:229-237` sets `ice_from`). Rivers stay liquid. Glacier ice is a
  separate solid layer on dry ground, and its top is the `ColumnGround` surface.
- **Implied ground** (`ground.rs`) never contains water. Water exists only in materialised chunks
  above the column surface. `ColumnGround.surface` is the ground under the water.
- **LOD.** `coarse_tile` emits the cell's topmost water block as the cell material
  (`lib.rs:840-866`). The server-side downsample is any-solid: `merge_group` treats water like any
  non-air material (`crates/game/src/lod.rs:56-81`). LOD has no separate water layer.

### 1.3 Rendering

- Water goes through the normal opaque voxel path: binary greedy mesher, pulled quads, coarse
  twins. No translucent pass exists, no water shader, and nothing water-specific appears in
  `assets/shaders/*.wgsl`. `bridge.rs:299`: "water counts as opaque". The minimap colours water
  by depth (`commander/minimap.rs:119`).

### 1.4 Every `== AIR` / `!= AIR` / `is_air()` site, recounted

`grep -E "(!=|==) *BlockId::AIR|\.is_air\(\)"` over `game`, `worldgen`, `client` and `server`
finds **~77 hits in 22 files**. That includes tests and lod.rs's 20, so the July plan's "~18" is
stale. The production sites a `Liquid` kind must decide on:

| Area | Sites | What water does there today |
|---|---|---|
| Colliders | `block.rs:397, 441, 448`; genesis `state.rs:884` | solid collider |
| Edit / ray | `edit.rs:93, 136, 242, 286, 481, 510` | solid to the ray; breakable; place target |
| Orders | `orders.rs:184, 198, 482` | counts as non-air |
| Items / mining | `items.rs:62`, `mining.rs:81` (water-aware), `:185` | — |
| Piles / pathfind / restore | `item_piles.rs:210`, `pathfind.rs:39, 43`, `player.rs:138` | already special-cased via `is_water` |
| Voxel bodies | `voxel_body.rs:188, 207, 214, 357` (doc `:478-481`: "water only" spawns no body) | detached as a block |
| Voxel / LOD | `voxel.rs:208, 215, 379, 385`; `lod.rs` (20 hits); `lod_builder.rs:124` | occupancy and material |
| Client meshing | `meshing/binary.rs:56, 79, 366, 379`; `bricks.rs:163, 241, 1006` | opaque faces, culls neighbours |
| Client fortress UI | `commander/tiles.rs:129`, `visible.rs:150`, `designate.rs:493, 514, 529`, `slice_plane.rs:65`, `height.rs:48, 290` | pick/hover treat water as solid |
| Far voxel (unused brick renderer) | `far_voxel/dda.rs`, `far_voxel/brick.rs` | — |

There is also an implicit site: the KCC in `camera.rs`/`movement.rs` sees water only through its
colliders.

---

## 2. Rollback cost model today

### 2.1 How a tick is logged

- `#[rollback]` (`crates/macros/src/lib.rs`) generates the log. Tier-1 fields
  (`#[undo(cell|map|slotmap)]`) push one typed `Entry { transaction, undo: UndoOp::Typed(Delta),
  pre_hash, verify }` per mutation (`macros/src/lib.rs:911-916, 996-1000`).
  - `UndoMap::insert`, `remove` and `get_mut` each clone the prior value at that key, take the
    global log `Mutex`, and `push_back` (`:1168-1215`).
  - `Delta` is one enum over all tier-1 fields, so every entry is as large as the largest variant.
    That variant is `clients(ClientId, Option<Client>)`, and `Client` holds an inventory `Vec` and
    `InputState` BTreeMaps. The entry is not tiny. Its exact size was not measured.
- Tier-2 fields (`Component<Chunk>`, `ColliderSet`, …) log boxed closures. Example:
  `set_chunk_block_safe` (`state.rs:671-700`) captures the old block and chisel. `HashMode::Off`
  is the binaries' default, so no per-entry hash is computed (`lib.rs` `DEFAULT_HASH_MODE`).
- Physics: `PhysicsPipeline::step_journaled` (`crates/rapier/src/pipeline/step_journal.rs:1-40`)
  first-touch-captures written bodies and colliders. It also clones the `IslandManager`,
  `NarrowPhase` and broad phase wholesale every tick (~240 KB at the old 64 m region scale; not
  re-measured at 256 m).
- Each event is one transaction (`region.rs:631`, `new_transaction`).
  - The **server** forgets each transaction immediately and never rolls back
    (`region_runner.rs:340-359`).
  - The **client** keeps every unconfirmed predicted event in `event_log`.

### 2.2 Re-sim window and frequency

- `TICK_RATE = 50` ms (`lib.rs:98`). Client lead `CLIENT_LEAD_MS = 1.5` ticks, band ±1 tick
  (`client/src/regions/clock.rs:9, 13`). `MAX_INPUT_LEAD_TICKS = 20` (`region_runner.rs:229`).
  `MAX_ALIGN_TICKS = 40` (`clock.rs:23`).
- **There is no fixed max rollback depth.** On a mispredict, `reconcile` rolls back the *whole*
  `event_log` and replays it (`region.rs:405-500`): depth = `event_log.len()`. The only hard cap is
  the `input_buffer.len() > 1000` panic (`region.rs:351-353`).
- Last live numbers, 2026-09-20 (memory `input-slot-rubber-banding`, after the tick-stamped input
  fix):
  - evlog 1–5;
  - mispredicts 0–6/s;
  - replayed events 0–118 per 5 s.
- One open item from that capture: `transfer>tick`, about **1 mispredict per tick per neighbour
  region carrying relayed ghosts**, at replay depth 12–16. Server regions tick on independent
  phases, so a relayed `GhostUpdate` never lands at the predicted id. No later document says it
  was fixed. Assume it is still true for any region next to a seam where bodies stand.
- No measurement exists since the 2026-11-02 move to client region threads and 256 m regions.
  The F3 Debug tab and `[client-budget]` (`PERF_LOG=1`) would give it.
- **Replay cost per mispredict = L × (one full region tick)**, where L is the log length. Every
  per-tick system re-runs: slimes, walkers, people, physics step, animators, voxel bodies,
  mining, piles. A water step inside `Tick` is re-run L times per mispredict, in every predicted
  region that mispredicts. The 09-22 synthesis's "≤ 10 steps per rollback" and "the step at N reads
  edits from N−D" assumptions still hold mechanically. With ghost-relay mispredicts at ~20/s
  per seam region, however, the realistic multiplier is **~20 × L ≈ 250–300 water steps/s per
  seam region**, not 5/s.

### 2.3 Cost of a block change today (16³ chunks)

`write_block_safe` (`edit.rs:284-324`) and the batched `fill_blocks_safe` / `clear_blocks_safe`
(`:494-600`) do this per changed block:

1. **Chunk data:** `set_chunk_block_safe`, one boxed undo closure. `ChunkBlocks::set` may scan all
   4096 blocks for `all_equal` (`voxel.rs:65-81`), early exit on the first mismatch.
2. **Render events:** `emit_on_undo(SetBlocks(old))` and `send(SetBlocks(new))`. A rollback
   re-emits both directions, so a replayed water step churns the client mesher.
3. **Colliders:** `collider_cells_of_block` diff, then `edit_block_colliders_safe`
   (`edit.rs:341-409`), which calls `mutate_collider_safe` (`state.rs:560-584`).
   - That clones the `Collider`. The clone shares the shape `Arc`, so `shape_mut` **copy-on-writes
     the whole chunk `Voxels` shape**: up to 4096 block keys plus half and voxel keys. With 2-block
     chunks that was 8 blocks.
   - The first edit of a genesis full-cube chunk (an all-water chunk) falls back to a full
     `rebuild_chunk_colliders_safe` (`:395-402`).
   - The modified collider then costs a broad-phase/narrow-phase update in the next step.
4. **Client:** `SetBlocks` triggers a brick re-derive and remesh. The **whole sim chunk's merged
   record list** is rebuilt from up to 512 bricks and re-sorted (`meshing/chunk_mesh.rs:1-22`),
   and the **coarse twin** is rebuilt (`meshing/coarse.rs:21-23`). The result is uploaded within
   the frame budget.
5. **Server LOD:** `terrain_fingerprint` (every `TERRAIN_HASH_INTERVAL_TICKS = 25`,
   `protocol.rs:110`; `region_runner.rs:405-419`; `state.rs:1706-1727`) changes. That builds a full
   `TerrainSnapshot` and pushes `TerrainChanged`, which invalidates and rebuilds the subscribed
   LOD keys.
6. **Piles:** the `piles_after_write_safe` hook (`item_piles.rs:294-305`).

**Conclusion: water state must live outside chunk block data**, and so outside colliders,
`SetBlocks`, the terrain fingerprint and LOD rebuilds. Two reasons:

- Each step of a CA touching k chunks would pay k whole-`Voxels`-shape clones plus k chunk
  re-merges and twin rebuilds per client. A replay pays the sim part again.
- Water is also not a collider for swimming anyway.

Static worldgen water could stay in chunks only as a seed that genesis moves into the water
layer. Leaving it there means the first breach rewrites chunk blocks. The forge's water (sea
plane, `water_top` per column) should instead become water-layer state at genesis, and the chunk
cells should become air (or a non-solid, non-meshed marker).

### 2.4 Cost formula for a per-tick water step in its own `#[undo(map)]` layer

Assume water state is `UndoMap<BlockPos, WaterCell>` (BTreeMap, like `item_piles`) plus bodies in
a small `UndoMap<BodyId, Body>`. Let *c* be the number of cells changed per step, *n* the number of
wet cells stored (CA cells only; body members are spans and levels), *s* the steps per second
(5 Hz at a 4-tick cadence), *L* the replay depth and *m* the mispredicts per second.

- **Forward step:** c × (BTreeMap get_mut + clone of `WaterCell` + Mutex lock + VecDeque push of
  one `Entry`) + c × render event. That is roughly ~100–200 ns per cell plus the solver's own
  work. No collider, no remesh of terrain.
- **Undo:** c × (BTreeMap insert/remove).
- **Client re-sim:** each region spends m × L × (step + undo) per second on water, on top of the
  forward s × step. That runs on that region's own thread
  (`client/src/regions/host.rs`; each predicted region is one OS thread).
- **Whole-state hash:** every 100 ticks per server region (`lib.rs:291`;
  `region_runner.rs:362-372`), walking `GameData` via `StateHash`. Water adds n × (key 12 B +
  value) bytes of crc32. At ~4 bytes per `u16`/`i32` field and crc32 at GB/s, 10⁵ cells add
  ~1–2 ms per 5 s. Its cost shows on the `[region …] state_hash n= sum= max=` line. It has not
  been measured at 256 m.
- **Snapshot, park and save:** the region snapshot is the whole `Rollback`, bincode + lz4
  (`protocol.rs` `encode_packet`). It is also the parked blob (`region_runner.rs:22-27`) and the
  autosave (`server/src/region_store.rs`). Water adds ~n × 16 B raw, compressing well. No current
  256 m snapshot size is recorded. The last figures (578 MiB raw at 256 m with 2-block chunks)
  predate 16³ chunks and are stale.
- **Wire:** zero beyond snapshots, because the solver is predicted (decision 1). Only seam flux
  events cross.

Scale check for a 256 m region:

- 256 × 256 columns, 16 × 16 chunk columns.
- A 100 × 100 × 5 m pond is 5 × 10⁴ cells; open sea 30 m deep is ~2 × 10⁶ cells.
- So per-cell storage of *standing* water is out of the question at this size: ~2 × 10⁶ BTreeMap
  entries is ~10⁸ B.
- The bodies abstraction from 09-22 is no longer an optimisation; it is required. Sea and lakes
  must be a level per body plus a membership rule (e.g. "column surface below `water_top`" from
  the recipe, like implied ground), never cells.

---

## 3. Cross-region plumbing today

**Scan.** `scan_boundaries` (`state.rs:1615-1700`) runs inside the region tick.

- It visits entities in key order.
- A body past a side whose neighbour is off the map is held on the seam. The check is
  `off_map(MapExtent)`, `world_map.rs:129-131`.
- Leavers are extracted undo-safely as `EntityBundle`s.
- Bodies near a margin produce `GhostData`.
- The results replace `pending_departures` / `pending_ghosts` each tick (`region.rs:964-972`).

**Server path.**

- `RegionRunner::tick` sends `Departures` / `GhostUpdates` right after the tick
  (`region_runner.rs:385-391`).
- `WorldManager::handle_region_output` (`world_manager.rs:1149-1222`) handles departures:
  - It rebases each one and calls `ensure_running(target)`, so **arrivals always wake the
    target**.
  - It sends `RegionInput::Event(EntityArrived)`.
  - A person walking into a non-running region is absorbed by the history instead
    (`person_crossed`, `:1167-1173`).
  - An off-map target is returned to the source (`:1153-1163`, logged as error).
- Ghosts never wake a region (`:1205-1213`).
- The target applies the event **on receipt, at whatever tick it is at**; no stamp maps source
  tick to target tick.
- Events for a stopping region queue in `pending_events` (`:419, 929, 1129-1139`).
- The server never rolls back (`region_runner.rs:340-348`), so a sent departure is final.

**Server timing.**

- Each region thread runs its own `recv_deadline` timer and skips missed deadlines
  (`server/src/region_threads.rs:121-161`). **Region tick counters are independent and not
  aligned.**
- Transfer latency is one manager hop: the departure is emitted at the end of source tick N and
  applied by the target at its next input poll. That is usually inside the same 50 ms period,
  but the target's tick number at application time is arbitrary.

**Client path.** On region threads `self_relay = true` (`client/src/regions/runner.rs:118-120,
266-298`).

- After each forward tick the region hands its departures and ghosts to the host. The host
  rebases them and sends sibling regions `RegionCmd::Arrival` / `Ghost`, which are applied as
  *predicted* events (`runner.rs:192-200`).
- The inline host relays at `RegionCmd::TakeTransfers` points instead (`:232-235`).
- The server's authoritative `EntityArrived` later reconciles against that prediction by identity:
  `matches_prediction` matches on `(source_region, source_key)` (`protocol.rs:637-650`).
- **A relayed transfer is never retracted when the source rolls back.**
  - Replays repopulate `pending_departures`, but they are only taken after a *forward* tick.
  - A prediction the server never confirms is an orphan. It re-applies on every rollback
    (memory `client-prediction-runaway`, scenario E), except the local player's
    (`evict_orphan_local_inputs`, `region.rs:604`).
- The client region clock is per region: each region steers its own lead (`regions/clock.rs`
  `decide`, stall/align in the runner). The tick broadcast never waits.

**Fortress 3×3.**

- The client subscribes to, and predicts, every region within `FORTRESS_NEAR_RADIUS = 1` of the
  fortress (`world_scale.rs:29`; `instance.rs:574-590`), on top of the near window
  (`DEFAULT_NEAR_RADIUS = 0` at 256 m, `world_scale.rs:18`). That is 9 predicted regions, each on
  its own thread.
- The server keeps those 9 running while subscribed. The ring outside (the 5×5 minus 3×3) is not
  running: it is parked as a blob, or never generated (`UNLOAD_GRACE_MS = 5000`,
  `world_manager.rs:29`).

**Outer map seam.**

- Past a SEA edge the world is ocean at `OUTSIDE_HEIGHT_M`; past a LAND edge the border ground
  continues (forge, CLAUDE.md).
- The sim holds bodies on the seam and the manager refuses off-map departures.
- An east-west wrap is only planned (`docs/superpowers/plans/2026-11-05-east-west-world-wrap.md`),
  not implemented.

### A "water flux across a seam" event on this path

1. The source region decides outflow from its own state and puts a `WaterFlux { source_region,
   source_seq, cells/volume }` into a third pending buffer next to departures and ghosts.
2. The server manager forwards it to the target as `RegionInput::Event(GameEventKind::WaterFlux)`.
3. The client host relays it to the sibling as a predicted event.
4. `matches_prediction` matches it on `(source_region, source_seq)`.

Failure modes:

- **Destination not running.**
  - Today an arrival wakes its target. Waking a 256 m region costs seconds of genesis and keeps
    it alive for ≥ 5 s, which is far too expensive per flux.
  - The person precedent applies instead: absorb into a manager-side ledger per region (pending
    volume, applied at that region's next start), or treat the seam as closed and hold the water
    in the source.
  - The manager already has `pending_events` for *stopping* regions, but nothing for parked
    ones.
- **Destination full / refuses.** 09-22 proposed a bounce-back event. On the server it is final
  and ordered. On the client it is one more predicted transfer that can orphan.
- **Source rolls back on the client after relaying.** The relayed flux stays in the target as a
  prediction.
  - If the server's flux differs in amount, the identity match corrects it at confirmation.
  - If the server produced **no** flux, the prediction is an orphan, re-applied forever. This is
    an existing defect that water would hit far more often than bodies do.
  - Needs: expiry of unconfirmed transfer predictions after k ticks, or water flux not predicted
    across regions at all (server-authoritative arrival only, no sibling relay).
- **Tick alignment.**
  - Regions are not tick-aligned on the server. A flux stamped "source tick N" cannot name a
    target tick, so it is applied on arrival.
  - The client predicts that arrival at a different id from the server's → mispredict and
    replay in the target. This is the same class of cost as the open `transfer>tick` ghost
    mispredicts.
  - A tick-stamped hold, like `EventAfterTick` for inputs (`region_runner.rs:36-40`), would need
    a per-region-pair tick offset, which nothing tracks today.
- **Open handoff bug.** TODO.md:25-42: the new home region is bit-inexact after a player crossing
  (`crossing.rs` red since 8e90481). Any new cross-seam event kind inherits that unresolved area.
- **Off-map seam.** The manager returns off-map departures to the source. Flux into an off-map
  side must likewise be refused in the source (closed seam), or sunk into the finite sea ledger
  of decision 2.

---

## 4. Determinism constraints and analogues

**Rules (CLAUDE.md, Vendored Forks).**

- No inherent `f32` transcendentals; use `libm`.
- `StateHash`, never std `Hash` or bytes.
- `BTreeMap`, never `HashMap`, in sim state and history.
- Stateless RNG `rng::roll(tick, subject, salt)` (`slime.rs`), so there is no RNG state to undo.
- A serialized `usize::MAX` breaks wasm32.
- After touching hashed or serialized state, run `just determinism-wasm`.
- Water should be integer-only (09-22 already decided `u16` amounts and an integer level).

**Existing per-tick patterns to copy.**

- **Item piles** (`item_piles.rs`) are the closest analogue.
  - Storage: `GameData.item_piles: #[undo(map)] BTreeMap<BlockPos, ItemPile>` plus an expiry
    index `pile_expiry: BTreeMap<(u64, BlockPos), ()>` (`state.rs:268-275`). Cells are keyed by
    region-local block position; no entities, no bodies.
  - Every write goes through one funnel, `write_piles_safe` (`item_piles.rs:431-455`). It emits
    `SetItemPiles(prior views)` on undo and the new views on do. Undo is just the `UndoMap`
    deltas, so it is exact by construction.
  - Settling is an integer column walk (`settle_cell` `:217`), run **on events, not per tick**:
    deposits, and the `piles_after_write_safe` hook on every terrain write (`:294-305`).
  - Per-tick work is gated: `if item_piles.is_empty() { return }`. Expiry touches only due
    entries via the index (`expire_piles_safe` `:393`).
  - The region cap is `PILES_PER_REGION_MAX = 4096`, with eviction.
  - Piles never cross seams; they clamp into the region.
  - A water "awake set" can copy the expiry-index trick: `BTreeMap<(wake_tick, cell)>`. Then a
    step touches only awake cells, and a settled region costs one `is_empty` check per tick.
- **Slimes** (`slime.rs:221`): a per-tick loop over owned entities in key order, plus a capped
  spawner (`SLIME_CAP_PER_REGION = 8`).
- **Voxel bodies** (`voxel_body.rs:628`): a per-tick scan of the `voxel_body` component, settling
  with `REST_TICKS` / `SETTLE_TIMEOUT_TICKS` and depositing `DropItems` into piles. This is the
  template for "a burst of activity that goes back to sleep".
- **`ColumnGround` / implied ground** (`ground.rs`): a never-mutated per-column recipe consulted
  on demand (`Rollback::implied_block`). Body membership for forged lakes and sea could be implied
  the same way: "below `water_top`, above ground, not yet disturbed". Only disturbed water would
  then be materialised as CA cells.

---

## 5. Water since September

- No water code has landed.
- The DF research (`2026-10-20-dwarf-fortress-research/08-fluids-and-environment.md`) endorses the
  09-22 model. It adds DF depth semantics (walkable ≤ 3/7, swim ≥ 4/7, drown 7/7), "diagonal gaps
  don't carry water", and the need for a supply story (springs, aquifers).
- `engine-mapping.md:96-128` sketches a per-tile `liquid: u8` (0..7 + kind) and "water blocks →
  liquid 7, sea/rivers static in the MVP".
- The 2026-11-05 adventure-survival plan rules that water is unbreakable and that "piles rest on
  the floor of water; water is not solid" (`plans/2026-11-05-adventure-survival-items.md:47, 83,
  233`). That rule is implemented for piles only; physics still treats water as solid.
- TODO.md:15 defers swimming/wading for adventure mode.
- The east-west wrap plan touches forge water routing (lakes, rivers across x = 0), not runtime
  water.

---

## Not determined

- Current replay depth and mispredict rate on region threads at 256 m, and whether ghost-relay
  `transfer>tick` mispredicts persist. These need a `PERF_LOG=1` run.
- Current region snapshot size and `state_hash` cost at 256 m / 16³ chunks.
- The size of `Delta` (undo entry) in bytes.
- The broad-phase clone size per tick at 256 m.
- How much worldgen water a typical embark region holds. A `cross-section` or `world-map` count
  per region would answer it.
