# Client / Server Performance — Findings From Captures

**Started:** 2026-09-19. Living document: add a section per capture batch, keep the ranking at
the top current. Produced by `just perf-capture` (see `CLAUDE.md` → Profiling and
`docs/superpowers/plans/2026-09-19-client-performance-data-handoff.md`).

**Rules for reading this:** numbers compare only within one adapter + world scale. Every figure
below is **n = 1** until a capture is repeated. Captures were taken with `DIAG=1` (wgpu timestamp
queries on) and a dirty tree (the instrumentation itself, uncommitted at the time).

## READ FIRST: this machine was power-clamped for every number in this document

Found 2026-09-19 while chasing why region genesis took 10 s live but 2 s headless. The laptop
(i7-12700H) was running from a **USB-C PD source: 19.5 V × 4.5 A ≈ 88 W**
(`/sys/class/power_supply/ucsi-source-psy-*`), not its own adapter. Under sustained multi-core
load the firmware clamps the CPU hard: **16 busy threads ⇒ every core at ~400–700 MHz** (at 57 °C,
so not thermal), and even one busy P-core only reaches 2.0–3.1 GHz of its 4.6 GHz. A short burst
runs fast, then the clamp bites — a region build measured 2.3 s right after a compile and 0.6 s
after a pause, same binary.

Consequences: (1) every multi-threaded phase here — builds, the 9-region join, LOD workers vs
region threads vs the client — ran several times slower than this hardware can do, and
*contention* effects are exaggerated; (2) single-capture comparisons are noisier than they look,
especially right after a rebuild (`perf-capture` rebuilds first); (3) ratios between runs at the
same scale are still meaningful, absolute seconds are not. Re-baseline on the real adapter
before quoting any absolute number. Quick check: `for c in 0 4 8 12; do cat
/sys/devices/system/cpu/cpu$c/cpufreq/scaling_cur_freq; done` while something multi-threaded runs.

## 2026-09-20: first capture on an UNCLAMPED machine (and with the region-start work)

`target/perf-capture/20260920T121828Z-Intel-full/` — `full`, Intel iGPU, release, 120 s. The power
source negotiated **19.5 V × 6.66 A ≈ 130 W** this day and 16 busy threads held ~3.5 GHz (vs
0.4–0.7 GHz on 2026-09-19), so this is **not a clean A/B** against the previous day's captures:
the numbers below mix the region-start code with a machine running several times faster.

| | 2026-09-19 (clamped, before region-start work) | 2026-09-20 (unclamped, with it) |
|---|---|---|
| Manager-thread `region_generate` | 9 calls, 6.2–6.9 s sequential | **none** (deferred to region threads) |
| `region_genesis` per region | 11–16 s | **≤ 0.58 s** (`generate_ms` ≤ 143, `build_ms` ≤ 433) |
| All nine `snapshot_sent` | 12–20 s | **0.5–1.7 s** |
| `first_near_mesh` | 12.0–12.4 s | **1.1 s** |
| `snapshot_voxels_applied` (23 372 chunks) | 31–37 s | 10.4 s |
| `lod_all_received` (570 keys) | 65.1 s | 14.3 s (server `build avg` 68–157 ms) |
| `near_meshing_drained` / `world_ready` | 64.8 s / 66.1 s | **20.0 s / 20.5 s** |
| Steady frame | 25–45 ms, CPU-bound | **16.67 ms — vsync-capped at 60 fps**, GPU 7.2 ms |
| Client / server peak RSS | 2358 / 978 MB | 2441 / 985 MB |
| WARN lines / KEEP-UP / send timeouts | 14 / 5 / 0 | **0 / 0 / 0** |

Machine-independent, from the same day's machine (region (0,0), 2791 chunks, three rounds each,
±3 ms): `Region::from_columns` **1277 ms with upstream parry → 231 ms with the patch (5.5×)**.
What now paces a `full` join: near meshing (20.0 s; the snapshot-voxel apply budget of 64/frame
finishes at 10.4 s, the mesher's 32 uploads/frame after it) — LOD is done by 14.3 s. With the
frame vsync-capped, per-frame budgets are the knob, not CPU.

## Why a bare `just run` is slow — measured 2026-09-20

`PERF_CAPTURE_PROFILE=debug … just world=full perf-capture` with no `adapter=`
(`20260920T160806Z-default-full`) reproduces `just run`: debug binaries, wgpu's own adapter choice.
- **It is the GPU choice, not the debug profile.** wgpu picked the **NVIDIA T600 on NVK**; the
  frame went 15 → 211 ms as terrain loaded (4.7 fps), GPU-bound at 86%, with
  `main_opaque_pass_3d` = **162 ms** for 921 605 fragment invocations (one per pixel — it is
  geometry/vertex-pull cost, not overdraw). The Intel iGPU draws the same scene's opaque pass in
  ~5 ms. `near_meshing_drained` 31.5 s and `world_ready` 37.6 s follow from the frame rate.
- **The debug build itself is fine now**: server genesis ≤ 1.17 s per region, all snapshots
  received by 3.3 s, LOD complete 10.6 s, client game thread ≤ 1.4 ms per tick at 8–20% util.
- The five KEEP-UP warnings are the pacing bug, not load: ticks cost ~1 ms while `drift` grows
  monotonically 100 → 1750 ms at `tick_rate=55ms` — the slow-down never converges the lead.
  (An earlier debug run, set aside at the user's request, additionally showed client tick cost
  for region (0,0) climbing 27 → 372 ms a few seconds after a region crossing; this run had no
  crossing and did not reproduce it. Worth a deliberate repro: debug build, cross a region.)
Fix for day-to-day use: pin the adapter. The justfile's `adapter` now defaults to
`$VOXEL_ADAPTER`, so `export VOXEL_ADAPTER=Intel` (or `6700`) once pins every recipe.

## First-terrain latency — confirmed 2026-09-20

`20260920T160235Z-Intel-full` vs `…155521Z…` (both with wire compression; `full`, unclamped):
`first_near_mesh` **1.91 → 1.07 s**; `snapshot_voxels_applied` 6.9 → 6.1 s (the cap did not slow
the batch pipeline — eight outstanding keeps four workers fed); first DIAG dump 40 → 27 ms;
`near_meshing_drained` 12.5 → 12.9 s and `world_ready` 13.1 → 13.5 s (noise); a region crossing
at ~14 s applied its 8370 chunks in under 2 s; 0 warnings, 0 KEEP-UP, all 20 631 LOD cells meshed.

### First-terrain latency — implementation notes

The side effect noted under wire compression below: with all nine regions arriving within 0.8 s,
`spawn_region_snapshot` put ~365 snapshot-voxel batches on the FIFO compute pool at once and the
first mesh tasks queued behind them (`first_near_mesh` 0.9 → 1.9 s). Batches are now only
*queued* (`SnapshotVoxelQueue.batches`); `apply_snapshot_voxels` keeps at most
`MAX_OUTSTANDING_SNAPSHOT_VOXEL_TASKS = 8` on the pool and always dispatches the batch whose
chunk centroid is nearest the active camera (arrival order until a camera exists — the local
camera itself arrives in the home region's snapshot). Batches of a removed or replaced region are
retired when `RegionRoots` changes, so they do not pin the ~40 MiB snapshot `Arc`. Tests: the
picker (`nearest_batch`), and an app-level test that a 700-chunk snapshot never has more than the
cap outstanding and still ends with every chunk's voxels applied; 350 renderer/harness tests
green. Expect `first_near_mesh` back under ~1 s and terrain filling outward from the player.

## Wire compression — confirmed 2026-09-20

`20260920T155521Z-Intel-full` vs `…154225Z…` (same code without the codec; `full`, unclamped):

| | bincode on the wire | framed + lz4 |
|---|---|---|
| Nine region snapshots | 380.0 MB (42.3 MB each) | **11.6 MB** (≤ 1.7 MB each, 33×) |
| Everything the server sent in 60 s | 395.6 MB | **14.2 MB** |
| Client `read_ms` (sum of nine) | 885 ms | **17 ms** |
| Server `send_ms` (sum) | 988 ms | 284 ms |
| Server `serialize_ms` (sum; now includes lz4) | 361 ms | 515 ms |
| Client `decode_ms` (sum; now includes lz4) | 475 ms | 846 ms |
| Last snapshot received | 1.83 s | **1.29 s** |
| `lod_all_received` / `near_meshing_drained` / `world_ready` | 8.9 / 13.0 / 13.5 s | 8.2 / 12.5 / 13.1 s (within noise) |
| LOD cells meshed / WARN / KEEP-UP / send timeouts | 20 631 of 20 631 / 1 / 0 / 0 | same / **0** / 0 / 0 |

On loopback the join time barely moves (the snapshots were never what paced it); the point is
the 27× less traffic, which is what a real network would feel. One side effect to know about:
`first_near_mesh` came at 1.9 s instead of 0.9 s — all nine regions now arrive within 0.8 s, so
their ~365 snapshot-voxel batches hit the FIFO compute pool together and the first mesh tasks
queue behind them. Total meshing time is unchanged; if first-pixels latency matters, cap the
outstanding voxel batches or spawn the home region's first.

### Wire compression — implementation notes

`game::encode_packet` / `decode_packet` (`crates/game/src/protocol.rs`): one format byte, then
raw bincode (`0`) or a little-endian `u32` decompressed length + an lz4 block (`1`). Compression
only above `PACKET_COMPRESS_THRESHOLD = 4096` bytes, and only if it actually shrinks the packet,
so tick traffic is untouched. The length prefix is untrusted: it is checked against
`MAX_DECOMPRESSED_PACKET_BYTES` (256 MiB) before any allocation, and every malformed input is a
`PacketError`, never a panic. All seven transport sites use the pair (server writer, QUIC +
WebTransport ingress, native client send/recv, wasm client send/recv); `lz4_flex` 0.11 (block
API, safe encode/decode) builds for `wasm32`. Tests: `crates/game/tests/packet_codec.rs` (raw
small packets, compressed large ones, incompressible fallback, hostile input) and the
end-to-end `webtransport_handshake` test — whose hand-rolled raw-bincode writer failed cleanly
(`Decompress(OffsetZero)`) until it used the codec too, i.e. old and new binaries do not
silently mis-parse each other. Headless, a real `ServerPacket::Region`: **40.3 → 0.90 MiB**,
encode 38 ms, decode 46 ms (bincode included). Expect in a capture: `snapshot_sent bytes≈0.9M`,
`send_ms` / `read_ms` near zero instead of ~130 / ~110 ms, a `full` join ~9 MB instead of ~380 MB.

## Region snapshot size — measured 2026-09-20 (the analysis behind the choice)

`cargo run --release -p worldgen --example snapshot_size` (region (0,0), 2791 chunks): the
`ServerPacket::Region` payload is **40.3 MiB**, of which **20.9 MiB are physics colliders**
(derived data: the voxel shapes of the chiseled surface), **18.1 MiB chunk chisel data** (4096
chiseled blocks × 4.5 KiB, mostly a 4096-byte per-voxel material array that is nearly uniform),
1.0 MiB rigid bodies, and 0.13 MiB the actual block grid. A `full` join ships nine of them:
~380 MB. On loopback that costs ~0.35 s per region (serialize 46 + send 132 + read 110 + decode
74 ms); on a real network it is the whole join.

It is extremely redundant. Same bytes (`SNAPSHOT_DUMP=<path>` on the example):
lz4 → **0.94 MiB (43×), 7 ms compress / 25 ms decompress**; zstd -1 → 0.35 MiB (113×), 25 ms;
zlib/deflate level 1 → 1.28 MiB (31×), 70 / 46 ms.

Options, with trade-offs:
1. **Compress at the transport edge** (recommended). A shared `encode_packet` / `decode_packet`
   in `game::protocol`: one header byte (raw | compressed), compression only above a size
   threshold so tick traffic stays raw and free. Seven call sites (server writer + two ingress
   readers, native client send/recv, wasm client send/recv). With `lz4_flex` (pure Rust, builds
   for wasm32, not yet in the tree — a new dependency) it is a net *win even on loopback*
   (−240 ms of send/read for +32 ms of codec per region) and ~40× less join traffic. `flate2` /
   `miniz_oxide` are already in the tree via Bevy (no new dependency) but cost ~10× more CPU for
   a worse ratio. Cost: a wire-format break between old and new binaries (fine pre-release),
   and the wasm client must take the same change.
2. **Stop shipping derived colliders** and rebuild them client-side (genesis is ~0.23 s per
   region now). Halves the payload with no codec — but rollback needs bit-exact state, and a
   region whose colliders were edited since genesis no longer matches a fresh build (slotmap
   generations / free lists), so this is only sound for never-edited terrain or with an explicit
   "pristine colliders" marker. Invasive, determinism-critical; not a tuning change.
3. **Compact `ChiselData` serialization** (elide or RLE the per-voxel material array): 18 → ~2.5
   MiB, colliders untouched. Changes the serialized state format (parked-region blobs too) for
   less than option 1 gives.
Options 2 and 3 compose with 1 later; 1 alone takes the join from ~380 MB to ~9 MB.

## LOD worker count — confirmed 2026-09-20

`20260920T154225Z-Intel-full` (5 workers) vs `…153227Z…` (2 workers), same day, unclamped, `full`:

| | 2 LOD workers | 5 LOD workers |
|---|---|---|
| `lod_all_received` (570 keys) | 17.6 s | **8.9 s** |
| `world_ready` | 18.2 s | **13.5 s** — paced by near meshing again (drained 13.0 s) |
| Server LOD latency avg, last window | 15.8 s | 6.8 s |
| Region ticks while LOD builds | max ≤ 23 ms, 0 overruns | **max ≤ 12 ms, 0 overruns** — sims unaffected |
| Server avg CPU / peak RSS | 55% / 949 MB | 89% / 1197 MB |
| Join frame times, first dumps | 23 / 20 / 22 ms | 30 / 33 / 25 ms, then 17–20 ms (LOD lands ~2× faster, more LOD meshing early) |
| LOD cells meshed / KEEP-UP / send timeouts | 20 631 of 20 631 / 0 / 0 | same |

`world_ready` history at `full` on this adapter: not within 60 s → 104.6 → 66.1 s (2026-09-19,
power-clamped) → 20.5 → 15.1 → 18.2 → **13.5 s** (2026-09-20, unclamped). Client and server now
finish at about the same time (near meshing 13.0 s, LOD 8.9 s); neither side has an obvious
single bottleneck left at this scale.

### LOD worker count — implementation notes

`world_ready` (18.2 s) was set by `lod_all_received` (17.6 s), and that by arithmetic: the
`[server]` windows of `20260920T153227Z-Intel-full` add up to ~34 worker-seconds (73 factor-2/4
keys × ~0.22 s, 118 factor-16 keys × ~0.09 s, 379 tile keys × ~0.02 s) on a fixed **two** LOD
workers ≈ 17 s — while the server averaged 55% of ONE core on a 20-thread machine. The default is
now `default_lod_build_threads(cores) = clamp(cores / 4, 2, 6)` (5 here); `LOD_BUILD_THREADS`
still overrides, the server logs the resolved count at startup and capture summaries show it
(`lod workers:`). Expect `lod_all_received` near 7–8 s, which hands the pacing of `world_ready`
back to the client's near meshing (~12.6 s). Beyond that, the remaining LOD cost is generation:
every factor-2/4/16 key for a never-run region runs a full `generate_region` (~0.1 s) — a
block-scale generator for factor 16 would cut ~120 of those, but it is a worldgen feature with
seam risk, not a tuning change.

## Packed LOD cells + tightened budget ceilings — confirmed 2026-09-20

`20260920T153227Z-Intel-full` (`full`, Intel iGPU, release, unclamped, 90 s, no region crossings)
against the same day's earlier `full` captures:

| | `…121828Z…` (per-frame budgets, dense LOD) | `…123228Z…` (rate budgets, loose ceilings) | this run (tight ceilings, packed LOD) |
|---|---|---|---|
| Client peak RSS | 2441 MB | 3194 MB (with region crossings) | **1134 MB** |
| Client GPU memory | — | 522 MB | 456 MB |
| `apply_lod_events` stalls | 27, max 46 ms | 46, max 37 ms | **none** |
| `mark_remesh_dirty` stalls | few | 105, max 31 ms (`work=512`) | 51, max 15 ms (`work=160`) |
| Frame time in the first seconds of the join | 20–27 ms | 37–49 ms | **17–23 ms** |
| `snapshot_voxels_applied` | 10.4 s | 5.9 s | 6.1 s |
| `near_meshing_drained` | 20.0 s | 10.8 s | **12.6 s** |
| `lod_all_received` / `world_ready` | 14.3 / 20.5 s | 14.6 / 15.1 s | 17.6 / 18.2 s (LOD-paced; server-side variance) |
| Steady frame | 16.67 ms (vsync) | 20.5 ms | 16.8 ms (vsync), GPU 5.6 ms |
| LOD cells meshed / KEEP-UP / send timeouts | 20 631 of 20 631 / 0 / 0 | same / 1 / 0 | same / 0 / 0 |

So the tighter ceilings kept almost all of the drain-time win (12.6 s vs 10.8 s vs 20.0 s) while
making join frames *smoother than before the budgets changed*, and packing LOD cells removed
1.3 GB of client memory and the last main-thread LOD stall. `world_ready` is now set by LOD
delivery (server: two `LOD_BUILD_THREADS`), not by anything on the client.

### Packed LOD cells — implementation notes

`LodVoxelData` held a dense `Vec<Voxel>` per LOD cell: 2 bytes × 32³ = 64 KiB, × 20 631 cells at
`full` ≈ **1.29 GB — over half the client's 2.4 GB RSS** — and `spawn_lod_root` produced it by
decoding every cell on the main thread (the last remaining `main_stall` category:
`apply_lod_events`, 37 ms unclamped, 124–886 ms clamped). It now stores the `PackedLodGrid` RLE
exactly as it arrived (the same move `bridge::VoxelData` made for near chunks). Only two
consumers ever read voxels, and both already ran in async tasks — the LOD mesh task and the
far-voxel brick task — so they clone the packed grid (a few hundred bytes) and decode there,
neighbours included. `spawn_lod_root` validates untrusted wire data with the new O(runs)
`PackedLodGrid::is_well_formed()` instead of decoding. Tests: packed round trip + footprint
(< 2 KiB for a layered terrain cell), well-formedness ⇔ decodability, and the existing 226
meshing / LOD-ring / far-voxel / bridge tests (the malformed-payload test included) unchanged
in behaviour. Expect: client peak RSS down by roughly 1 GB at `full`, `apply_lod_events` stalls
gone or tiny, LOD mesh-task time slightly up (27 decodes per task, on workers).

## Meshing budgets — confirmed 2026-09-20, then ceilings tightened

`20260920T123228Z-Intel-full` vs `…121828Z…` (same day, same unclamped machine, `full`, n = 1
each; user's verdict: "significantly improved"):

| | Per-frame budgets | Rate-based budgets (first version) |
|---|---|---|
| `snapshot_voxels_applied` (23 372 chunks) | 10.4 s | **5.9 s** |
| `near_meshing_drained` (18 876 chunks) | 20.0 s | **10.8 s** |
| `lod_all_received` / `world_ready` | 14.3 s / 20.5 s | 14.6 s / **15.1 s** (LOD-paced again) |
| Frame time in the first ~6 s of the join | 20–27 ms (37–50 fps) | **37–49 ms** — worse |
| `mark_remesh_dirty` stalls | few | 105, max 31 ms at `work=512` (the ceiling) |
| Region crossings later in the run | — | each new region loads in 8–25 ms, its voxels in 2–9 s, no stalls |

The regression is the feedback loop a delta-proportional budget creates: a long frame earns more
work, which makes the next frame longer; the 256 / 512 ceilings were far too loose to bound it.
**Fixed after this capture (not yet re-captured):** ceilings are now about twice a 60 Hz frame's
worth — `MAX_MESH_UPLOADS_PER_FRAME = 100`, `MAX_SNAPSHOT_VOXELS_PER_FRAME = 160` (~45 µs per
snapshot insert ⇒ ≤ ~7 ms). Expect join frames back near 25 ms with most of the drain-time win
kept (the rates, not the ceilings, set throughput at normal frame times).

Also seen in that run, not investigated: one `KEEP-UP WARNING` and one Bevy warning `Two or more
Entities with IsDefaultUiCamera found` during rapid back-and-forth region crossings — the
snapshot path and `AddCameraComponent` both tag the local camera, and for a frame the old and new
home regions' cameras coexist. Cosmetic, pre-existing, separate from performance.

### Meshing budgets — implementation notes

What paced near meshing in `20260920T121828Z-Intel-full`: from 11 s to 19 s the remesh queue
drained at ~1600 chunks/s while the join ran at 37–50 fps, i.e. ~36 per frame = the
`MESH_UPLOADS_PER_FRAME = 32` cap (+ empty meshes, which do not count as uploads), with the
in-flight task count pinned at its cap of 64 the whole time — the four worker threads were never
the limit. The clamped runs the day before showed the same law at the other end: ~260 chunks/s
at 8 fps. A fixed per-frame count throttles hardest exactly when frames are slow.

Change: budgets are now **rates**. `meshing::queue::per_frame_budget(per_second, dt, min, max)`
turns a chunks-per-second target into this frame's count from the real frame delta.
- `MeshBudgetPolicy` + `scale_mesh_budgets` (first in the meshing chain) re-derive `MeshBudgets`
  each frame: `MESH_UPLOADS_PER_SECOND = 3000` (≈ 50/frame at 60 Hz, 75 at 40 fps), floor = the
  old 32, ceiling `MAX_MESH_UPLOADS_PER_FRAME = 256`; `max_in_flight = max(64, 2 × uploads)` so
  finished-but-unattached tasks do not starve the lane. The far/LOD raster lane reads the same
  `MeshBudgets`, so it scales with it. Harnesses that pin `MeshBudgets` by hand do not run the
  policy and keep fixed numbers — none of the ~145 meshing/bridge/LOD tests changed.
- `apply_snapshot_voxels` uses `SNAPSHOT_VOXELS_PER_SECOND = 4800` (floor 64, ceiling 512), kept
  above the mesh rate so the remesh queue always holds everything pending and its nearest-first
  ranking decides the order.
Expect: `near_meshing_drained` well under the 20.0 s of the reference capture (the queue held
~18.9k meshable chunks: ~6–7 s at 3000/s if the workers keep up — if `in_flight` no longer sits at
its cap in `load_progress`, the workers have become the limit and
`VOXEL_CLIENT_WORKER_THREADS` (default 4) is the next knob), with join fps not much lower.

## Region start (server) — implementation notes (2026-09-19)

Attribution (`perf record` on the new `worldgen` example `genesis_profile`, reduced with
`scripts/perf_summary.py`): **88% of `Region::from_columns` was
`parry3d::shape::voxels::Voxels::new`**, 72% in `compute_voxel_neighborhood_bits` and 35% (self) in
`hashbrown` `find` — the vendored parry fork resolved every voxel *and each of its six
neighbours* through a `chunk_headers` hash lookup, per voxel slot. The project's own code was
small (`collider_cells` 1.7%, worldgen 3.9%).

1. **Parry fork patch** (`crates/parry/src/shape/voxels/voxels_neighborhood.rs`):
   `recompute_all_voxels_states` resolves each internal 8³ chunk and its six adjacent chunks
   once, then indexes directly. Identical output — emptiness is a sentinel (`0b0100_0000`)
   outside the six face bits, so in-place evaluation order cannot matter. Pinned by a randomized
   test against the upstream implementation (kept as a `#[cfg(test)]` reference) and end to end:
   region (0,0) state hash **`0x0ed71dbf` before and after**, determinism check OK. Also green:
   parry voxel tests, game `hash_restore` / `simple` / `log_model` / `region_runner` /
   `collider_genesis` / `voxels_hash_cache` (ignored ones included).
   **`Region::from_columns`: 2371 → 342 ms** (region (0,0), 2791 chunks).
2. **Worldgen off the manager thread for region starts**: `RegionSeed::Deferred(Option<SerializedRegion>)`
   + `RegionSpawner::generates_columns()` (same opt-in shape as the LOD `Pristine` job). The
   server's `ThreadRegionSpawner::with_generator` runs worldgen on the region's own thread;
   `InlineSpawner` (wasm, tests) and `ThreadRegionSpawner::default()` keep the old path. A parked
   region now generates **only if its blob is corrupt** — the eager path regenerated the columns
   on every resubscribe just to have a fallback. `region_genesis` reports `generate_ms`.
   Tests: `world_manager.rs` (manager's generator never called, deferred seeds handed out;
   deferred seed generates once for a fresh region, never for a healthy parked one).

Headless result (`cargo run --release -p server --example genesis_probe -- 9`, the production
spawn path, same machine state): nine regions to first tick **6.0–8.4 s → 1.2–1.7 s**; one region
3.3 s → ~0.6 s. Live before: 11–16 s of genesis after 6–7 s of sequential manager-thread worldgen.
Expect `snapshot_sent` for all nine regions within a few seconds of connecting.

## Headline after two captures (Intel iGPU, release; `tiny` then `small`)

Nothing measured so far points at the GPU. In cost order, with what settles or fixes each:

1. **Server region genesis, ~10 s per region, all of it chunk/collider build** (`build_ms=10033.8`,
   `terrain_hash_ms=67.2`; 10.67 s and 10.10 s in the two runs). 73% of the `tiny` join.
2. **LOD serving runs worldgen synchronously on the server's manager thread.** One factor-2 ring =
   24 pristine regions = 24 `generate` calls, **8.7 s total, up to 522 ms each**, during which the
   hub routes nothing: backlog reached `in=22 region=174`. The client then received the held-up
   events in a burst and tripped **KEEP-UP WARNING twice** (prediction log 0 → 89 → 185; one
   window with `recon total=987ms replayed=17658`). A join-time death-spiral signature caused by
   the *server*, at the second-smallest world.
3. **LOD builds cost ~1 s per factor-2 key on a worker** (`build avg=866–1011 ms, max 1419 ms`), two
   workers, so submit→result latency is 1.5–2.4 s and 24 keys took 10.5 s (`lod_first_received`
   12.0 s → `lod_all_received` 22.5 s). `full` asks for ~570 keys.
4. **Memory explodes with the first LOD ring.** Server RSS 125 MB → **1158 MB** (24 pristine
   `TerrainSnapshot`s ≈ 43 MB each in `terrain_cache`; the LRU cap is 64 ⇒ ~2.7 GB at `full`).
   Client RSS 456 MB → **1208 MB**, entities 2832 → 14 868 (≈ 31 MB per LOD key; gap 4's "dense
   32 768-voxel `Vec` per LOD cell" is the likely owner — unconfirmed).
5. **Main-thread stalls on the client, all unbudgeted:** per region `drain_client_updates` 411 / 544 ms
   and `mark_remesh_dirty` 173 / 338 ms; per LOD payload `apply_lod_events` **59–131 ms each**
   (22 stalls, 1.9 s total for 24 keys ⇒ ~50 s of hitching for `full`'s 570).
6. **Steady-state frame ≈ 19 ms and CPU-bound outside the game thread** — and it did not move
   between 2832 entities (19.05 ms) and 14 868 entities (18.88 ms), while the same client ran
   2.5 ms frames before any terrain existed. GPU passes 3–4 ms (17–22%). So the cost is something
   that appears with terrain but does not scale with the LOD ring: needs `trace-chrome`.
7. **Server tick spikes are the cadence terrain hash — confirmed.** Every window:
   `max=111.44ms … terrain_hash n=4 … max=107.5ms`, `max=50.51ms … max=48.2ms`, and so on. A full
   `terrain_snapshot()` + hash every 25 ticks costs 45–110 ms against a 50 ms tick period: 1–4
   overruns per 5 s, forever, on an idle region.
8. **42.3 MB per region snapshot** (~0.5 s end to end on loopback).

Time to `world_ready`: **14.7 s** at `tiny`, **23.2 s** at `small`.

## Capture 1 ranking (Intel iGPU, `tiny`, release)

Time from client process start to `world_ready`: **14.7 s**. Where it went:

| # | Cost | Measured | Where | Evidence |
|---|---|---|---|---|
| 1 | **Server region genesis** | **10.67 s** of the 14.7 s join (73%) | region thread: `RegionSeed::into_region` → `Region::from_columns` → `seed_terrain_genesis` (collider genesis), plus the baseline `terrain_hash` | `region_genesis region=0,0 ms=10670.1`. Client sat idle at 300–420 fps for ~10 s waiting (`load_progress … regions=0/1`). Worldgen itself was only 82 ms (`region_generate`). |
| 2 | **Near meshing after the snapshot lands** | 2.1 s (`first_near_mesh` 12.04 s → `near_meshing_drained` 14.14 s), 2163 meshed chunks | budgeted mesh queue (64 in flight, 32 uploads/frame) | timeline; `load_progress … remesh_queue=1141 in_flight=33` mid-way |
| 3 | **Steady-state frame is CPU-bound on the render/main side** | 19.0 ms/frame (52.9 fps); GPU passes sum to 4.3 ms (22%); game thread 1.5–6% utilised | not the sim, not the GPU → ECS/extract/render-graph CPU work | `diag.txt` verdict; `[client-budget] util=1.5–5.9%`; iGPU clock averaged 461 of 1400 MHz (never ramped); before the region arrived the same client ran 2.5 ms frames, so vsync is not what holds it at 19 ms |
| 4 | **Main-thread stall when a region arrives** | one 411 ms frame + one 173 ms frame per region | `drain_client_updates` → `spawn_region_snapshot` (409 ms for 2792 entities / 2791 chunks: `derive_voxels` per chunk); `mark_remesh_dirty` (173 ms for 2791 chunks) | `main_stall[drain_client_updates] ms=411.1`, `main_stall[mark_remesh_dirty] ms=172.7 work=2791` |
| 5 | **Snapshot size: 42.3 MB for ONE region** | serialize 96 ms + send 262 ms (server), read 263 ms + decode 189 ms + game-thread load 64 ms (client) ≈ 0.6 s end to end on loopback | `ServerPacket::Region` carries the whole `Rollback` | `snapshot_sent bytes=42297599`, `snapshot_received`, `region_loaded` |
| 6 | **Server tick spikes** | avg 2.3–5.9 ms but max 48–95 ms against a 50 ms tick period; 0–4 overruns per 5 s window, every window | prime suspect: the cadence terrain check — `TERRAIN_HASH_INTERVAL_TICKS = 25` × 50 ms = a full `terrain_snapshot()` + hash every 1.25 s = 4 per window, which is exactly the overrun count (`RegionRunner::tick`). Not yet measured. | `[region (0,0)] … max=94.60ms … overruns=4` in all 9 windows; the `[region]` line now carries `terrain_hash n= sum= max=` to settle it |

Not costs, but noted:

- **Memory at `tiny`:** client peak RSS 456 MB (69 → 275 MB at startup, → 455 MB when the region
  lands), GPU memory 361 MB resident; server peak 185 MB, 125 MB steady.
- **Server logs two INFO lines per player input** (`recv PlayerInput…`, `processed PlayerInput…`):
  1618 of the 1648 lines in a 60 s `server.log`. Cheap to silence, and it buries everything else.
- **Mispredictions at near-idle:** 4–20 mispredicts/s, avg replay depth ~2.3, with one player
  mostly moving the mouse. Cheap at this scale (recon ≤ 150 ms per 5 s window) but it is work
  that scales with sim cost per tick.
- Startup is not the problem: `first_update` at 1.36 s, first frame submitted at 1.43 s.
  Connect took 3.4 ms on the first attempt. No send timeouts, no WARN/ERROR lines on either side.

## Capture 2 (`small`: 1 region + one factor-2 ring, 24 LOD keys)

`target/perf-capture/20260919T151146Z-Intel-small/`. First run with the split genesis timing,
`region_first_tick` (16.7 ms — the old first-tick spike is gone) and `terrain_hash` on the
`[region]` line.

| t (s) | What |
|---|---|
| 0.09 | server `region_generate` 85 ms |
| 1.3 | first frame |
| 10.2 | server `region_genesis` done: `build_ms=10033.8 terrain_hash_ms=67.2` |
| 10.5–10.7 | snapshot sent / received / loaded (42.3 MB; 83 + 196 ms out, 189 + 130 + 46 ms in) |
| 10.7 | `lod_requests_done keys=24` — all 24 requested in one tick |
| 11.2–11.6 | `drain_client_updates` 544 ms, `mark_remesh_dirty` 338 ms |
| 12.0 → 22.5 | LOD payloads trickle in; 22 `apply_lod_events` stalls of 59–201 ms |
| 15.3 | near meshing drained (3.6 s, contended with LOD this time vs 2.1 s at `tiny`) |
| ~12–22 | server `[server]` windows #2–#4: `gen n=11 sum=3424ms`, `gen n=12 sum=4915ms max=522ms`, backlog `region=71` then `174`; client KEEP-UP WARNING ×2 |
| 23.2 | `world_ready` |

Reading notes: `gen` in the `[server]` line is manager-thread worldgen (`WorldManager::start_lod_build`
→ `(self.generator)(rc)` for every pristine region a LOD key needs). `lod latency` (submit → result)
is ~2.4× `lod build` (worker time), i.e. keys queue behind two workers.

## Hypotheses from the handoff — status

| Hypothesis | Status after capture 1 |
|---|---|
| iGPU client is GPU-bound in steady state at `tiny` | **Refuted.** GPU passes 4.3 ms of a 19 ms frame; render engine ~43% busy by fdinfo; GPU clock never ramps. CPU-bound outside the game thread. Next: `just adapter=Intel world=tiny trace-chrome`. |
| The load failure is a main-thread stall during join, not the GPU | **Partly.** Stalls are real (411 ms + 173 ms per region) but at `tiny`/release they are 4% of the join; the join is dominated by *server* genesis. No load failure occurred at `tiny`/release. At `full` the stalls multiply by 9 regions and LOD is added — unmeasured. |
| First-frame pipeline/shader compilation dominates time-to-first-frame | **Refuted** on this adapter: first frame at 1.4 s. |
| Server silently drops join packets to a slow client | **Not observed** (`timeouts=0` in both). Still possible at `full`: nine ~42 MB snapshots go through one writer task with a 2 s per-send deadline. |
| *(new)* The server can push the client into the death-spiral signature | **Observed at `small`.** Manager-thread worldgen for LOD stalls event routing; the burst afterwards drove prediction-log depth to 185 and one window to 20% game-thread util with 17 658 replayed ticks. It recovered — at `small`. |
| Client sim cannot keep up in release at `full` | Untested at `full`. At `tiny`: util ≤ 6%, no KEEP-UP WARNING. |
| Physics dominates client sim time | Consistent (`ctrl` is most of `sim`), but the sim is ≤ 6% of the game thread at `tiny`, so it is not what limits the client here. |
| Cost scales ~linearly with world size | **No — LOD dominates, not region count.** `tiny` → `small` adds zero simulated regions yet +8.5 s to ready, +1 GB server RSS, +750 MB client RSS. Steady-state frame time did not change. `full` still unmeasured. |

## What this changes

The original symptom ("client did not load on the iGPU", `just run`, debug, `full`) now has a
leading suspect that is **not the GPU and not the client**: region genesis at 10.7 s per region
in *release*. In a debug build (opt-level 1, ~2.5× on hot loops, likely worse on collider
genesis) that is plausibly 25–60 s per region before the first snapshot can even be sent, with
nine regions building concurrently at `full`. That is inside the range of "looks like it never
loads". Treat this as a hypothesis until a debug or `full` capture shows the same shape.

Memory note `collider-perf-followup` recorded region build at ~1 s after the cuboid-collapse
work. It is 10.7 s now, with 2791 chunks per region instead of 1024 (vertical chunk stacking
landed since). Either the per-chunk cost regressed or the chunk count and chunk content
(non-full chunks that cannot collapse to a cuboid) grew it; the next capture splits the number
(`build_ms` vs `terrain_hash_ms`, plus `region_first_tick`), and `just collider-bench` /
`just bench-region` attribute inside `build_ms`.

## First `full` capture (Intel iGPU, release, 60 s, all fixes in) — does NOT finish loading

`target/perf-capture/20260919T155459Z-Intel-full/` (3×3 regions, five rings, 570 LOD keys). After
60 s: `regions=9/9 lod_roots=55/570 remesh_queue=13756 lod_build_queue=4814`, **7.5 fps**. This
is the original "client does not load on the iGPU" symptom, now measured — and none of it is the
GPU: GPU passes 9.6 ms of a 171 ms frame (6%), render engine 11% busy, GPU clock 165 of 1400 MHz,
GPU memory 431 MB (the same as `small`'s 426 MB; Iris Xe is UMA — fdinfo shows it all in ordinary
system RAM, `stolen` = 0 — so a ~500 MB "VRAM" figure is not a limit in play here).

What stops it loading, in order of how much wall time it costs:

1. **LOD throughput: ~1.2 keys/s ⇒ ~8 minutes for 570 keys.** All 570 are submitted in one tick
   (`sub=570 inflight=570`); two workers build at 1.0–1.8 s per key (and slowing: they compete
   with nine region threads), so submit→result latency is already 44 s after 60 s and climbing.
   55 roots in 45 s. Knobs/fixes: `LOD_BUILD_THREADS`, cheaper factor-2/4 builds (generation +
   downsample of a full region for every key), coarser sources for far rings.
2. **Near meshing drains at the frame rate, and the frame rate has collapsed.** `remesh_queue`
   22 530 → 13 756 in 33.5 s = ~260 chunks/s = `MESH_UPLOADS_PER_FRAME` (32) × ~8 fps. The budget
   is per frame, not per unit time, so slow frames throttle the very work that would end the
   slow phase. ~50 s more just for the near window at this rate.
3. **Frame time 85 → 171 ms and still rising, 94% CPU, owner unknown.** It rises while the
   remesh queue *falls*, so it tracks something that accumulates (meshed near chunks: 23 372
   chunk entities at `full` vs 2791; `ChunkQuads` extracted 3k → 17k). `small` draws the same
   ~16k `ChunkQuads` in 23–26 ms, so it is not the draw count alone — suspects are per-frame work
   over the 9-region near set (cave-culling BFS, extract, per-view queueing ×5 views with four
   shadow cascades). **Needs a trace:** `just join_ms=60000 adapter=Intel world=full trace-join`.
4. **One 6.3 s frame**: `drain_client_updates ms=6332.6` — all nine region snapshots were spawned
   inside a single frame (`spawn_ms` sum 6277, max 924 each); then `mark_remesh_dirty` 2.6 s for
   23 372 chunks, and `apply_lod_events` up to 674 ms.
5. **Server genesis: 9.2–13.1 s per region, nine in parallel** (92.7 s of CPU) — nothing reaches
   the client before 12.6 s. Plus **6.9 s of manager-thread worldgen for the region starts**
   (`region_generate` ×9, up to 973 ms each, sequential, in `ensure_running`) — the same routing
   stall the LOD fix removed, still present for region starts.
6. Server RSS peaks at 2.7 GB and saws between 1.3 and 2.5 GB — suspect the queued tile jobs,
   each carrying cloned chunk overlays for the authoritative regions (unconfirmed).
7. Client prediction sits at `drift=-300ms tick_rate=45ms avg_replay≈10.5 evlog=10` for the whole
   run and the HUD state falls back to `Connecting` — the known join-behind pacing issue
   (`client-prediction-runaway` memory), visible at `full`. No KEEP-UP warning, game thread ≤ 21%.

Server-side fixes hold at `full`: `gen n=0` during LOD, backlog 0, no send timeouts, all nine
regions tick at avg 0.5–0.8 ms / max 9–14 ms with 0 overruns (`terrain_hash max` 9–14 ms).

### `full` frame cost, first attribution (`trace-join`, 5.7 GB trace, 2026-09-19)

Reduced with the now-streaming `scripts/trace_summary.py` (`--skip-ms 25000 --until-ms 60000
--skip-cat rapier3d --skip-cat parry3d`; 481 frames in 35 s — a tracing build, so absolute
numbers are inflated, proportions are the point):

| Per frame (heavy phase, 25–60 s) | ms |
|---|---|
| render world `schedule: Render` (prepare / queue / phase sort / render graph) | **68** (max 264) |
| main world `PostUpdate` (transform propagation, visibility, cave-culling BFS live here) | **28** (max 134) |
| main world `Update` (meshing queue, LOD apply, bridge) | **27** (max 119) |
| `RenderExtractApp` incl. `ExtractSchedule` | 11.5 (extract itself 2.9) |
| `submit_graph_commands` | 1.9 |
| GPU render graph encode (`run_graph Core3d`) | 1.6 |

So the frame has three CPU owners, the largest in the **render world**, and
`extract_chunk_quads` is *not* it (extract ≈ 3 ms) — that suspicion is dead. Which systems inside
`Render` / `PostUpdate` / `Update` is still unknown: release Bevy strips system names
(`system: name="<Enable the debug feature to see the name>"`). Fixed for next time — the
`trace_chrome` and `tracy` client features now pull in `bevy/debug`. Re-run
`just join_ms=60000 adapter=Intel world=full trace-join`; ~70 s then close the window is enough
(the rapier fork's solver spans make the file grow ~25 MB/s; the recipes now skip those
categories when summarising). Over the whole first 60 s the one-off stalls show as
`PreUpdate max 6.9 s` (region spawn) and `Render max 6.8 s`.

The KEEP-UP warnings in that traced run (prediction log depth 356 → 740 after the user crossed
regions at ~225 s) are **not** evidence yet: every rapier solver call was being traced on the game
thread. Reproduce it under `perf-capture` (no tracing) before treating it as real.

### `full` frame cost, NAMED (second `trace-join`, `bevy/debug` on, 2026-09-19)

That run also **reached `world_ready` at 113.5 s** (570/570 LOD roots, 18 876 meshed near chunks)
— `full` does load on the iGPU, in about two minutes, in a tracing build.

| Per frame (tracing build) | join, 25–60 s (208 frames) | steady, 120–150 s (478 frames ≈ 16 fps) |
|---|---|---|
| **`meshing::visibility::cull_chunks`** (cave-culling BFS, `PostUpdate`) | **115 ms** (max 327) | **33.8 ms** (max 98) |
| `schedule: Render` self time (own systems are small ⇒ the render thread waiting on the main world) | 114 ms | 45.7 ms |
| `meshing::lod::spawn_lod_mesh_tasks` | 15.6 ms (max 50) | ~0 |
| `voxel_pull::extract_chunk_quads` (system + its per-entity commands) | 1.1 + 2.7 ms | 3.4 + 9.2 ms |
| `voxel_pull::prepare::prepare_view_chunk_draws` | 3.5 ms | 10.0 ms |
| `propagate_parent_transforms` | 2.7 ms | 2.7 ms |
| `lod_rings::apply_lod_events` / `queue::spawn_mesh_tasks` | 3.4 / 3.4 ms | ~0 |

**`cull_chunks` owns the frame, during the join and after it.** Why (`visibility.rs`): every
frame it BFS-floods the whole bounding box of the chunk grid ±1 (at `full` ≈ 98 × height × 98
cells, open air included), with nodes keyed `(cell, dirs_used, entry_face)` — several visits per
cell — through `HashSet` inserts plus a frustum sphere test per step, and it recomputes from
scratch even when neither the camera nor any connectivity mask changed. It was sized for "the
near 3×3 window alone, ≈24 chunks across" (its own doc comment); the window is 96 across.

Options, cheapest first (not started — this is the render path under active benchmark work):
1. **Do not recompute when nothing changed**: key the result on camera cell + frustum + a grid/
   mask revision. Free while standing still; no help while turning or while chunks stream in.
2. **Dense storage instead of hash sets**: index `visited` / `reached` by cell offset inside the
   domain box (bitsets). Same algorithm, typically ~10× cheaper; helps every frame.
3. **Drop the frustum gate from the BFS and cache per camera cell**: Bevy already frustum-culls
   per entity; the BFS result then only changes when the camera crosses a chunk boundary or a
   mask changes. Bigger single BFS, but rare — and time-sliceable.
4. Throttle (every N frames / ms budget) as a stop-gap: visibility lags by a few frames.
Recommendation: 2 then 1 — mechanical, behaviour-preserving, testable against the existing
`visibility.rs` tests; consider 3 only if 2 + 1 leave it above ~2 ms.

**Before / after, `full`, Intel iGPU, release, untraced** (`20260919T155459Z-Intel-full` →
`20260919T165432Z-Intel-full`, n = 1 each; the first ran 60 s, the second 120 s):

| | Before | After the `cull_chunks` rewrite |
|---|---|---|
| Frame time while the join streams (same ~36k entities) | 138–171 ms and rising | **47–70 ms** |
| Frames rendered by t ≈ 59 s | 963 | **2483** |
| `remesh_queue` at t ≈ 59 s | 13 756 (draining ~260 chunks/s) | **1434** (~590 chunks/s) |
| `near_meshing_drained` | not within 60 s | **62.2 s** (18 876 chunks) |
| `world_ready` | not within 60 s | **104.6 s** (570/570 LOD roots, 20 631/20 631 cells meshed) |
| Steady frame once loaded | — | **39.6 ms (26 fps)**, GPU 12.1 ms (31%) — still CPU-bound |
| KEEP-UP warnings / send timeouts / WARN lines | 0 / 0 / 0 | 0 / 0 / 0 |

The join is now paced by LOD delivery, not by the client: near meshing is done at 62 s, but only
68 of 570 LOD roots have arrived by 67 s (server: ~1 key/s for the expensive factor-2/4 keys);
the cheap tile keys then land in a burst (82 → 570 roots between 75 s and 103 s). Unchanged and
next in line: the one-frame stalls (`drain_client_updates` 4.7 s, `mark_remesh_dirty` 2.9 s,
`apply_lod_events` up to 738 ms), server genesis 9–13 s per region, client RSS 2.46 GB.

**Steady state after the rewrite, NAMED** (`trace-chrome`, `full`, 120–182 s, 1201 frames ≈ 19 fps
in the tracing build; per frame):

| System | Before the rewrite | After |
|---|---|---|
| `meshing::visibility::cull_chunks` | 33.8 ms | **1.7 ms** |
| `voxel_pull::prepare::prepare_view_chunk_draws` | 10.0 ms | **18.4 ms** — now #1 |
| `voxel_pull::extract_chunk_quads` (system + commands) | 3.4 + 9.2 ms | 4.5 + **13.1 ms** — now #2 |
| `propagate_parent_transforms` | 2.7 ms | 3.9 ms |
| `check_visibility` | 1.0 ms | 1.8 ms |
| `schedule: Render` self (waiting on the main world / extract) | 45.7 ms | 27.2 ms |

(The two pull-path rows grew because this run has the full LOD ring meshed — ~57k chunk
entities — where the earlier trace was taken mid-way.) Both are per-chunk-entity, per-frame work
in the `GroupedDirect` near mode:
- `prepare_view_chunk_draws` walks **every** `ExtractedChunkQuads` once **per view** — the camera
  plus four shadow cascades = 5 passes — doing an `arena.allocations` map lookup and a frustum
  test per chunk per pass. Cheap cuts: resolve the allocation once per frame instead of once per
  view; give the shadow cascades a coarser or cached list.
- `extract_chunk_quads` re-inserts `ExtractedChunkQuads` (Arc clone + matrix + bounds) for every
  chunk entity every frame; 13 ms of that is just applying the commands. It already computes a
  `changed` flag — extracting only changed chunks (the render world is retained) would remove
  almost all of it, but `prepare_chunk_quads` reads that flag, so the two must change together.
- These are exactly what the GPU-driven near mode (`VOXEL_NEAR_RENDER=gpu`) is meant to remove,
  so measure that mode at `full` before optimising `GroupedDirect` by hand.

**`GroupedDirect` vs GPU-driven near mode at `full`** (Intel iGPU, release, untraced, 120 s, n = 1
each: `20260919T165432Z-Intel-full` vs `20260919T170442Z-Intel-full`, the latter with
`VOXEL_NEAR_RENDER=gpu`, log-confirmed `GpuDriven { indirect_count: true }`):

| | `GroupedDirect` (default) | `GpuDriven` |
|---|---|---|
| Steady frame once loaded | 39.6 ms (26.4 fps) | **34.4 ms (29.8 fps)** |
| GPU pass total | 12.1 ms (31%) | 9.7 ms (28%) |
| Frame time during the join | 47–70 ms | 51–65 ms |
| `near_meshing_drained` / `world_ready` | 62.2 s / 104.6 s | 63.8 s / 101.1 s |
| Client peak RSS / GPU memory | 2462 MB / — | 2470 MB / 465 MB |

A ~5 ms (13%) steady-state gain — real but modest, and still CPU-bound (24.6 ms of the frame is
not GPU). It removes the per-view draw-list cost but not `extract_chunk_quads`, which both
modes share; that (re-inserting a component per chunk entity per frame, ~57k entities) is the
next client-side frame cost worth fixing, in either mode. The join itself is unchanged: it is
paced by server LOD delivery.

**Pull-path per-entity costs — before / after** (`full`, Intel iGPU, release, untraced, 120 s,
`GroupedDirect`, n = 1 each: `20260919T165432Z-Intel-full` → `20260919T171550Z-Intel-full`):

| | Before | After |
|---|---|---|
| Steady frame once loaded | 39.6 ms (26.4 fps) | **24.8 ms (40.7 fps)** |
| GPU pass total / share | 12.1 ms / 31% | 9.3 ms / 37% — still CPU-bound (15.5 ms not GPU) |
| Frame time while the join streams | 47–70 ms | **30–50 ms** |
| `near_meshing_drained` | 62.2 s | **51.3 s** |
| `world_ready` | 104.6 s | **85.2 s** |
| Chunks re-extracted per frame (`[pull-cull]`) | all (~38k) | **0 when idle, tens while streaming** |
| LOD cells meshed / WARN lines / KEEP-UP / send timeouts | 20 631 of 20 631 / 0 / 0 / 0 | same |

This beats the GPU-driven near mode measured earlier the same day (34.4 ms) while staying on
`GroupedDirect`; that mode should be re-measured on top of this change. Visual correctness
(terrain draws, updates on edits and on cave-culling flips while turning) was NOT checkable from
the capture — confirm by eye.

**One-frame join stalls — confirmed** (`20260919T174643Z-Intel-full` vs `…171550Z…`): region
spawn in `drain_client_updates` 8.9 s → **34 ms total, 5 ms max** (no `main_stall` line at all);
`mark_remesh_dirty` one 3.5 s frame → slices of ≤ 28 ms; `apply_lod_events` max 886 → 124 ms;
`first_near_mesh` 25.7 s → **12.0 s**; all 23 372 snapshot voxels applied by 31 s;
`near_meshing_drained` unchanged at 51 s. `world_ready` was 107 s vs 85 s, but the client's LOD
backlog was 0 throughout both runs — the difference is server LOD build time that run
(1.4–2.7 s per key vs 1.0–2.0 s), i.e. the next item:

**Server LOD build cost — confirmed** (`20260919T175838Z-Intel-full` vs `…174643Z…`, `full`, Intel
iGPU, release, n = 1 each):

| | Before | After |
|---|---|---|
| Server LOD `build avg` for the expensive keys | 1.4–2.7 s | **0.26–0.31 s** (0.6–1.0 s while nine regions run genesis) |
| Keys built per 5 s window (factor-2/4 phase) | 3–9 | **14–37** |
| `lod_all_received` | 105.9 s | **65.1 s** |
| `world_ready` | 107.1 s | **66.1 s** |
| Server peak / final RSS | 2767 / 1401 MB | **978 / 789 MB** (no dense 217 MB-per-job grids on the workers) |
| `near_meshing_drained` | 51.4 s | 64.8 s — LOD now arrives ~4× faster and shares the client's compute pool; near and far finish together |
| LOD cells meshed / send timeouts / server WARNs | 20 631 of 20 631 / 0 / 0 | same |

`world_ready` history at `full` on this adapter: not within 60 s (morning) → 104.6 s (cull
rewrite) → 85–107 s (pull path + stall fixes, LOD-paced) → **66.1 s**. What remains in those
66 s: server genesis 11–16 s before anything arrives, then near meshing and LOD in parallel.

**Known netcode issue visible in that run (not caused by this work):** 5 KEEP-UP warnings with no
region crossing — `drift=+1100ms tick_rate=55ms`, 20 mispredicts/s (= every server tick),
`avg_replay≈36`, prediction log plateauing at ~39 (stable, not diverging), game thread 20–43%
utilised. The client joined ~1.1 s ahead of the server and the ±10% tick-rate pacing never
converges it; earlier the same day it joined ~0.3 s behind with `avg_replay≈10`. This is the
`client-prediction-runaway` problem (join/pacing by confirmed position): which side of the
server the client lands on depends on how busy the join was. Worth its own session; the
`[client-budget]` + `[client] mispredicts/s` lines are the measurement.

**Server LOD build cost — implementation notes.** Headless attribution
(`cargo run --release -p worldgen --example lod_build_profile`): a factor-2/4 build for a
never-run region was ~980 ms = downsample 690 (70%) + generate 190 + encode 95; factor 16 is
already cheap (block-scale path). The 690 ms was `build_far_lod_cells` expanding every chunk to a
dense 32³ grid (`derive_voxels`) before `downsample2`. It now writes each chunk straight into its
octant at half scale, per block: 2×2×2 source groups never straddle a 16-voxel block, so an
un-chiseled block is one uniform 8³ fill and only chiseled blocks run the majority rule, read
from their occupancy bitset. Bit-identical by a randomized test against the old implementation
(kept as a test-only reference; chiseled blocks, air-material chisel voxels and duplicate chunk
coordinates included). Result: downsample **694 → 45 ms** (factor 2), **687 → 68 ms** (factor
4); whole build ≈ 980 → ≈ 200 ms, now dominated by `generate_region`. Expect the ~72 expensive
keys that paced `world_ready` (~60 s on two workers) to take ~15 s.

**One-frame join stalls — implementation notes.** Measured before: all nine region
snapshots spawned inside one `drain_client_updates` call (6.3-8.9 s frame), then
`mark_remesh_dirty` fingerprinting all 23 372 chunks at once (2.6-3.5 s), and `apply_lod_events`
stacking payload bursts (0.7-0.9 s).
- Region spawn: entities still spawn synchronously (cheap), but each chunk's
  `derive_voxels` + RLE encode moved to `AsyncComputeTaskPool` batches
  (`SNAPSHOT_VOXEL_TASK_CHUNKS = 64`) sharing the snapshot through an `Arc`;
  `bridge::apply_snapshot_voxels` inserts results at `SNAPSHOT_VOXELS_PER_FRAME = 64`, which
  also caps `mark_remesh_dirty` at ~10 ms/frame. Chunk entities carry `PendingSnapshotVoxels`
  until then; an incremental `SetVoxelComponent` that arrives first clears the marker and wins
  (test proven red with the guard removed). Task entities are children of the region root, so
  removing/replacing a region cancels them. Pending voxels count as in-flight work in both
  readiness gates (`join_timeline`, `render_bench`), and the headless harness runs the same chain.
- LOD apply: `apply_lod_events` defers messages beyond `LOD_APPLY_CELLS_PER_FRAME = 512` decoded
  cells per frame, in arrival order (at least one message always progresses). A stop-gap: one
  factor-2 payload is still ~100 ms; the real fix is to keep LOD cells packed and decode in the
  LOD mesh task (which would also cut the client's LOD memory).
Verified: 59 + 33 client tests across `renderer::bridge::`, `lod_rings::`, `join_timeline::`,
`voxel_pull::`; `just smoke`. Expect in the next `full` capture: no multi-second `main_stall`
lines, a `snapshot_voxels_applied` event, first terrain earlier than 25 s.

**Pull-path per-entity costs — implementation notes.** (1) `extract_chunk_quads`
now extracts only chunks whose `ChunkQuads`, `GlobalTransform` or `InheritedVisibility` changed
(`Or<(Changed<…>,…)>` on the `Extract` query); the stored `changed` flag is gone and
`prepare_chunk_quads` filters on `Changed<ExtractedChunkQuads>` instead, because a flag would go
stale the first frame a chunk is not re-extracted. (2) `extract_removed_chunk_quads` returns early
when nothing was removed instead of walking every render entity each frame. (3)
`prepare_view_chunk_draws` resolves each chunk's arena allocation once per frame rather than once
per view (5 views), and drops cave-hidden chunks up front. Verified: type-check, `just smoke`, 33
`voxel_pull::` unit tests. **Not verifiable headlessly:** that terrain still draws correctly and
updates on edits / cave-culling flips — check visually, then compare a `full` capture against
`20260919T165432Z-Intel-full` (39.6 ms) and a `trace-chrome` against 18.4 + 17.6 ms.

**Implemented (options 2 + 1), 2026-09-19.**
`CullingScratch::flood` is now dense: one `u128` per cell inside the expansion box holds the
visited bit of every legal BFS state (`state_bit`: 27 direction codes × 3 entry axes — the entry
face is always the opposite of a used direction, so its axis identifies it; 55 of 81 bits are
reachable), plus dense per-cell connectivity masks (one grid pass per flood instead of a
`BTreeMap` + ECS lookup per popped node) and a lazily filled per-cell frustum verdict (instead of
a sphere test per step). `cull_chunks` is change-gated on `CullInputs { raw camera cell, frustum
planes }` + `grid.is_changed()` + `Changed`/`RemovedComponents<ChunkConnectivity>` +
`config.is_changed()`, and counts `CullingStats { runs, skipped }`. Behaviour is pinned by a
randomized differential test against the old `HashSet` flood kept as a test-only reference
(`dense_flood_reaches_exactly_what_the_reference_bfs_reaches`), a state-bit injectivity test, a
Chebyshev-bound test and a gate test (`flood_is_skipped_until_camera_masks_or_grid_change`).
What the gate does NOT help: turning the camera (the frustum changes every frame) and the join
(masks change every frame) — there only the dense storage helps. If a capture still shows
`cull_chunks` high while turning, option 3 (drop the frustum gate, cache per camera cell) is next.

Secondary: `spawn_lod_mesh_tasks` at 15.6 ms/frame during the join clones up to 26 neighbour
grids (64 KiB each) per task on the main thread and re-filters the whole queue every frame;
`extract_chunk_quads` re-inserts a component for every chunk entity every frame (12.6 ms at
`full` steady state — the earlier suspicion was right at this scale, just not the biggest).

## Correctness bug found on the way: neighbouring LOD rendered as strips (FIXED, confirmed live)

User report + screenshot: regions around the home region showed thin strips instead of terrain.
The `[perf-lod]` dump (capture `20260919T154601Z-Intel-small`) made it unambiguous — **every LOD
root had ~450–590 cells but exactly `raster_ready=64 with_quads=64`**: one task batch
(`MAX_IN_FLIGHT_MESH_TASKS = 64`) per root, in x-sorted order, i.e. a strip.

Cause: `far_voxel::lifecycle::queue_far_brick_tasks` runs right after
`meshing::lod::spawn_lod_mesh_tasks` in the production chain. In raster mode it called
`LodBuildQueue::remove(entity)` for every `Changed<LodVoxelData>` cell — and `remove` clears
**both** lanes, so the ~400 cells still waiting on the raster lane were dropped in the same frame
they were enqueued. Fixed by leaving the queue alone in raster mode (`retain_brick` already
empties the brick lane). Regression test:
`raster_mode_meshes_every_cell_of_a_root_larger_than_the_task_budget` (compile-checked; running it
needs the client test build: `just test-one client "--lib raster_mode_meshes"`). The meshing
pipeline harness (`meshing/queue.rs::pipeline_app`) never caught it because it chains
`spawn_lod_mesh_tasks` straight into `apply_lod_meshes`, without the brick system in between.
The server side was ruled out first with `cargo run --release -p worldgen --example lod_coverage`.

**Confirmed** by capture `20260919T155052Z-Intel-small`: all 24 roots `raster_ready == cells`
(12 012 / 12 012; 11 939 with quads, 887 916 quads), full 16×16 coverage maps, user-visible
terrain. This is the first honest client-side `small` measurement:

| Client metric (`small`, Intel iGPU, release) | Strip bug (13% of ring) | Fixed (full ring) |
|---|---|---|
| Steady frame time | 16.2–18.9 ms | **22.8–26.0 ms (39–45 fps)** |
| GPU pass total / share | 3.1–4.6 ms / 17–28% | 6.9–7.7 ms / 29–31% — still **CPU-bound** |
| `ChunkQuads` entities extracted per frame | 3 635 | **16 394** |
| Costliest GPU pass | smaa ≈ 1 ms | main_opaque_pass_3d 2.2–2.6 ms, early prepass 1.2 ms |
| `lod_all_received` → `world_ready` | 0.6 s | **4.9 s** (LOD meshing now really happens; queue peaked ≥ 1680) |
| `world_ready` | 25.9 s | 27.8 s |
| Client peak RSS / GPU memory | 1498 MB / 359 MB | 1503 MB / 426 MB |

Frame CPU cost now visibly scales with the number of drawn chunk entities (~+6 ms for +12.8k).
A concrete suspect for `trace-chrome` to confirm or kill: `voxel_pull::extract_chunk_quads`
re-inserts an `ExtractedChunkQuads` component (Arc clone + matrix + bounds) for **every**
`ChunkQuads` entity **every frame**, changed or not.

**Consequence for every earlier `small` number:** only ~13% of LOD cells ever had geometry. The
client frame time, GPU share, quad counts and part of the client RSS at `small` were measured on
a mostly empty ring — which also explains why frame time "did not move" between 2.8k and 14.9k
entities (headline #6). Re-capture `small` after this fix before using any client-side `small`
figure; expect more GPU work and a longer LOD meshing phase. Server-side figures are unaffected.

## Server batch — before / after (Intel iGPU, `small`, release, n = 1 each)

Before: `20260919T151146Z-Intel-small`. After: `20260919T153659Z-Intel-small` (in the after run
the player also crossed into region (0,-1) at ~57 s, so compare the first ~45 s only).

| Metric | Before | After |
|---|---|---|
| Manager-thread worldgen during the LOD phase (`[server] gen`) | 24 calls, 8.7 s, max 522 ms | **0 calls** (only region starts: 1 × 193 ms) |
| Routing backlog while LOD builds (`backlog region=`) | up to 174 | **0–1** |
| Client KEEP-UP WARNING | 2 (log depth 185, 17 658 replayed ticks) | **0** |
| Server peak RSS / steady | 1158 MB / 1158 MB | **582 MB / 359 MB** (472 MB while 18 builds were in flight) |
| Terrain check cost (`terrain_hash max`) | 45–110 ms | **4.7–22.7 ms** |
| Region tick `max` / overruns per 5 s | 50–111 ms / 1–4 every window | **4.9–23.2 ms / 0 in every window** |
| Region tick `avg` | 2.0–4.3 ms | 0.4–1.2 ms |
| `server.log` lines in 60 s | ~1650 | **39** |
| LOD worker build time per key (`build avg`) | 0.87–1.01 s | 1.08 s (generation moved in, as predicted) |
| LOD submit → result latency | avg 1.5–2.4 s, max 2.8 s | avg 2.0 s, max 3.3 s |
| **Time for all 24 LOD keys** (`lod_first_received` → `lod_all_received`) | 10.5 s | **14.1 s — slower** |
| **`world_ready`** | 23.2 s | **25.9 s — slower by 2.7 s** |

The one regression is honest arithmetic: before, the manager thread was an accidental third
worldgen worker running in parallel with the two LOD workers; now generation + downsample are
serialised on the two workers (`LOD_BUILD_THREADS = 2`). The routing stall and the death-spiral
trigger are gone, at the price of ~3 s more LOD fill at `small`. The knob is
`LOD_BUILD_THREADS` — a capture with `LOD_BUILD_THREADS=4` is the next comparison; changing the
default is a call about how many cores background LOD fill may take from region sim threads.

New observations from the after run (the player crossed a region boundary):

- **Input is dropped for seconds after a region crossing at `near_radius=0`.** 115 client WARNs
  `dropping PlayerInput: home region … not loaded`: the home flips to (0,-1) at once, but that
  region still has to run genesis (2–10 s) and ship a 42 MB snapshot. At `full` the neighbour
  is normally preloaded, so this is mostly a small-world artefact — but it is the same genesis
  cost (#1) showing up as a gameplay freeze rather than a join delay.
- Genesis varies by region: 9.97 s for (0,0) but ~2 s each for the two regions entered later.
- The steady frame was 16.2 ms / vsync-capped in this run vs 18.9 ms before. Different camera
  position and n = 1 — do not read it as a client-side win.

## Fixes landed (server batch, 2026-09-19)

Code + tests are in and the before/after capture above confirms every expectation in this
table (the one regression, LOD fill time, is the predicted "it moved, it did not vanish").

| Fix | Change | Expect in the next capture |
|---|---|---|
| Terrain check (headline #7) | `Rollback::terrain_fingerprint()`: per-chunk crc of coords + content, summed — no chunk clone, no bincode. `RegionRunner` polls it; the snapshot is built only when it changed. Tests: `terrain_snapshot.rs` (stable, content- and coordinate-sensitive, insertion-order-insensitive, changes on add); the existing `region_runner.rs` push/no-push tests still pass. | `[region] … terrain_hash … max=` far below the old 45–110 ms, tick `max=` well under the 50 ms period, `overruns=0` on an idle region |
| LOD worldgen off the manager (#2) | New `LodBuildJob::Pristine { key, region }` + `LodBuilder::generates_pristine()`. `ThreadLodBuilder` opts in and generates through its own `WorldgenCache` handle on the worker; `InlineLodBuilder` (wasm, tests) keeps the old manager-side path. Tests: `lod_streaming.rs` (manager never calls its generator for pristine LOD, submits `Pristine`, retains no terrain; `Pristine` payload is bit-identical to generate-then-`Region`). Cache temp files are now unique per thread. | `[server] gen n=` ≈ 0 in the LOD windows (only region starts), backlog `region=` stays small, **no KEEP-UP WARNING**, `lod build avg` rises by roughly the old `gen` cost (it moved, it did not vanish) |
| Pristine snapshots (#4) | Falls out of the above: the threaded path never inserts pristine entries into `terrain_cache`. | server peak RSS far below 1158 MB at `small` |
| Per-input log lines | `recv PlayerInput…` / `processed PlayerInput…` are DEBUG now. | `server.log` shrinks from ~1650 lines/min to tens |

Still open from the server side: genesis (#1, measure with `just collider-bench` first) and the
~1 s LOD build per key (#3, now including generation — `just perf-server` + `perf-summary`).

## Optimisation candidates the data now supports (largest first)

1. Collider/chunk genesis (`seed_terrain_genesis`): 10 s per region. Attribute with `just collider-bench`.
2. Move LOD-source worldgen off the manager thread (it already has worker threads for the build
   itself) — removes the routing stall and the induced KEEP-UP burst.
3. Stop hashing a full terrain snapshot every 25 ticks; mark terrain dirty on edit instead.
4. Do not retain 43 MB full-resolution `TerrainSnapshot`s for pristine LOD sources (downsample
   straight from generation, or cache the coarse result only).
5. Budget `apply_lod_events` / `spawn_region_snapshot` / `mark_remesh_dirty` per frame, and avoid
   the dense per-cell decode on the main thread.
6. Frame CPU cost (~15 ms): unknown owner until `trace-chrome`.

## Next measurements (in order)

1. ~~Re-capture with the split genesis timing~~ — done by capture 2 (`small`).
2. **`just collider-bench`** (headless, release) to attribute inside genesis — no GUI needed.
3. **`just adapter=Intel world=tiny trace-chrome`** for ranking #3: which ECS system / extract step /
   render node owns the ~15 ms of CPU per frame.
4. **`full` capture** on the same adapter (`PERF_CAPTURE_SECONDS=300` — expect minutes: ~570 LOD
   keys at ~1 s each over two workers, and watch server RSS); then all three on the RX 6700 once
   the eGPU is attached.
5. ~~Tick spikes~~ — settled by capture 2: it is the terrain hash.

## Capture log

| Capture directory | Adapter (selected) | World | Rev | Notes |
|---|---|---|---|---|
| `target/perf-capture/20260920T160806Z-default-full/` | **NVK T600 (unpinned)**, debug build | full | same as below | reproduces `just run`: GPU-bound 4.7 fps, opaque pass 162 ms; `world_ready` 37.6 s |
| `target/perf-capture/20260920T160235Z-Intel-full/` | same, unclamped | full | `9b778d6` + nearest-first capped voxel batches (uncommitted) | 45 s incl. one region crossing; first terrain 1.07 s, `world_ready` 13.5 s |
| `target/perf-capture/20260920T155521Z-Intel-full/` | same, unclamped | full | `4a57a97` + lz4 wire framing (uncommitted) | 60 s; snapshots 380 → 11.6 MB, server tx 396 → 14 MB, `world_ready` 13.1 s |
| `target/perf-capture/20260920T154225Z-Intel-full/` | same, unclamped | full | `34f355c` + core-scaled LOD workers = 5 (uncommitted) | 60 s; LOD complete 8.9 s, `world_ready` 13.5 s, region ticks ≤ 12 ms |
| `target/perf-capture/20260920T153227Z-Intel-full/` | same, unclamped | full | `880bd85` + packed LOD cells (uncommitted) | 90 s; client RSS 1134 MB, join frames 17–23 ms, `world_ready` 18.2 s |
| `target/perf-capture/20260920T123228Z-Intel-full/` | same, unclamped | full | rate budgets, loose ceilings (pre-`880bd85`) | 120 s with rapid region crossings; near drained 10.8 s, join frames 37–49 ms |
| `target/perf-capture/20260920T121828Z-Intel-full/` | same, **~130 W power source, unclamped** | full | + parry voxel patch, deferred region worldgen (uncommitted) | 120 s; `world_ready` 20.5 s, 60 fps vsync-capped; not comparable in seconds with 2026-09-19 |
| `target/perf-capture/20260919T175838Z-Intel-full/` | same | full | + block-wise LOD downsample (uncommitted) | 120 s; `world_ready` 66.1 s, server RSS < 1 GB; 5 KEEP-UP warnings (pacing, drift +1100 ms) |
| `target/perf-capture/20260919T174643Z-Intel-full/` | same | full | + async snapshot voxels, LOD apply budget (uncommitted) | 120 s; no multi-second stalls, first terrain 12.0 s |
| `target/perf-capture/20260919T171550Z-Intel-full/` | same | full | `43d5ddb` + extract-only-changed (uncommitted) | 120 s; steady 24.8 ms, `world_ready` 85.2 s |
| `target/perf-capture/20260919T170442Z-Intel-full/` | same | full | same, `VOXEL_NEAR_RENDER=gpu` | 120 s; GpuDriven near mode: steady 34.4 ms vs 39.6 ms |
| `target/perf-capture/20260919T165432Z-Intel-full/` | same | full | same + dense/gated `cull_chunks` | 120 s; `world_ready` at 104.6 s, steady 26 fps |
| `target/perf-capture/20260919T155459Z-Intel-full/` | same | full (9 regions, 570 LOD keys) | same | first `full` capture; not loaded after 60 s (55/570 LOD roots, 7.5 fps) |
| `target/perf-capture/20260919T155052Z-Intel-small/` | same | small | same + LOD strip fix | first capture with the LOD ring fully meshed; `VOXEL_PULL_DEBUG=1` |
| `target/perf-capture/20260919T154601Z-Intel-small/` | same | small | same + `[perf-lod]` dump | the capture that exposed `raster_ready=64` per root |
| `target/perf-capture/20260919T153659Z-Intel-small/` | same | small | same + server batch fixes | 60 s hands-off; player crossed into region (0,-1) at ~57 s; 0 KEEP-UP warnings |
| `target/perf-capture/20260919T151146Z-Intel-small/` | same | small (1 region, ring `(2,2)`, 24 LOD keys) | same + genesis/terrain-hash probes | 60 s hands-off; 2 KEEP-UP warnings during the LOD phase |
| `target/perf-capture/20260919T150349Z-Intel-tiny/` | Intel Iris Xe (ADL GT2), Mesa 25.2.8, Vulkan | tiny | `8bd9ac8` + uncommitted instrumentation | 60 s hands-off, first capture ever; `world:` line missing from the manifest (fixed afterwards) |
