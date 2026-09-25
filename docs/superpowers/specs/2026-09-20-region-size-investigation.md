# Region size: bigger regions, and a build-time dial to try them

2026-09-20. Question: regions are small; with one OS thread per region, would bigger ones
be better, and would they make fast travel between regions smoother? Can the size be made
configurable for testing?

**Short answer.** A region is 1024 units = **64 m** (`UNITS_PER_METER` = 16), so yes, it is
small: walking crosses one in 15 s, sprint-flying (21.6 m/s) in 3 s. Doubling the edge is
*structurally* cheap — no packed format, shader, wire type or narrow integer knows the region
size — and it is now a build-time dial (`just region_chunks=64 run`). But every per-region cost
scales with **area** (measured ×3.6–4.0), the thread count per player does not change (a 3×3
window is nine threads at any size), and the same land starts ~3× slower as one big region than
as four small ones in parallel. Bigger regions pay off only together with a **distance-based
near window** (§5), which turns "9 regions per player" into "4" at 2048 units. **2026-09-22
addendum:** 128 (4096 units, 256 m) is now legal too — see §7; the "blockers" were fixtures and a
cap, not the tiling — but it is the practical ceiling.

## 1. What is baked in, and what is not

Two sweeps (sim/server/worldgen, client/shaders) looked for every literal `1024`, `32`-as-
chunks-per-region, shift/mask, narrow type and packed field.

**Not baked** (the CLAUDE.md warning "region (1024) dimensions are baked into packed records,
shaders and the LOD math" is too strong — the *chunk* edge 32 is; the region edge is not):

- Quad records (`records.rs`, `voxel_pull.wgsl`): 6-bit positions are chunk-local; a chunk is
  placed by a full `mat4x4`. GPU slot capacity grows geometrically; no "9 regions × 32×32" buffer.
- `far_voxel.wgsl` / DDA constants are LOD-grid constants.
- `ChunkCoords {usize, i32, usize}`, `ColumnCoords`, `LodKey.coords: [i32; 2]`, `ChunkKey([i32; 3])`
  — no range limit. `PackedLodGrid` u16 runs are intra-grid.
- f32: every candidate size is a power of two, so region offsets and handoff rebasing stay exact;
  ULP at 4096 is 0.0005 voxel units.
- `GHOST_MARGIN` (32) and `FLIP_HYSTERESIS` (2) are absolute world units — still correct.
- Worldgen: `REGION_BLOCKS` derives from `REGION_CHUNKS`; `PROFILE_LATTICE_BLOCKS` (4) divides
  128/256; `coarse_tile` only needs a key to be whole 32-cell grids. The LOD builder branches on
  `key.region_span() == 1`, not on a factor, so a one-region factor-64 tile takes the right path.

**Was baked, fixed in this change:**

| Where | Problem | Fix |
|---|---|---|
| `client …/meshing/visibility.rs` `CULL_BFS_MAX_CHEBYSHEV = 64` | Cave-culling flood box. Its comment said "≈24 chunks, 64 overshoots 2×"; in fact a camera on its home region's edge is 2 regions = 64 chunks from the far side of the 3×3 window, so 64 is *exactly* enough today. At 64-chunk regions two thirds of the window would end `Hidden`. `Node::pos: [u8; 3]` overflows at the 257-wide box. | `2 * REGION_CHUNKS`, `pos: [u16; 3]` (same 12-byte node). |
| `lod.rs` `LOD_RING_TABLE` | Bounds are in **regions**, the pixel budget is about world distance: unchanged, a doubled region silently doubles the horizon (33 → 65 km) and quadruples each key. | Table per region size, same 2.5 px budget, same 32 768-unit horizon: `[(2,4),(3,8),(6,16),(13,32),(16,64)]` at 64. The factor-2 ring disappears (the near window covers it). ~1 080 keys instead of ~3 300, LOD cell total about equal. `ring_table_keeps_cells_within_the_pixel_budget` passes for it. |
| `lod.rs` tiling (`tile_regions`, `cells_per_region`, `copy_region_cells_into_tile`) | Every factor's grid must be either whole regions (tile-keyed) or divide a region exactly (region-keyed, several grids). With power-of-two factors that is "`REGION_SIZE` is a power of two". | One `const` assert (power of two, ≥ 32) + `REGION_CHUNKS_CHOICES = [32, 64, 128]` for the sizes that have a ring table and checked fixtures; anything else fails the **build**. |
| `world_manager.rs` `PRISTINE_TERRAIN_CACHE_MAX = 64` | 64 × ~160 MiB ≈ 11 GB at 64 chunks (inline builder: wasm, tests). | Scales inversely with area (16 at 64): same ~2.7 GB ceiling. |
| tests in `lod.rs`, `tests/region_coords.rs`, `worldgen` sea tests | Pinned 1024 / the production ring table / a coast named by region coords `(-5, 2)` (a different, waterless place at 2048) as fixtures. | `ring_factor_in` tests use a fixed fixture table; geometry tests derive from the constants; the coast is named by world position. |

**Still open at 64** (cost or cosmetics, not breakage): stale numbers in comments (`bridge.rs`
"~2800 chunks per region", `instance.rs` "~3,300 keys", `lod_rings.rs` "a factor-2 region is ~480
cells", `justfile` "~570 LOD keys"); test fixtures `bridge.rs:1005` (`i % 32`, `i / 1024`),
`voxel_pull/queue.rs:456` (4096), `join_timeline.rs:303` (570); `MAX_NEAR_RADIUS = 3` becomes a
14 km / ~500k-chunk window; `PLAYER_SPAWN_X/Z = 128` no longer sits on the column the seed was
scanned for; `far_voxel` page budgets see 4× data per key. **No connect handshake exists**, so a
32-chunk client against a 64-chunk server is not refused — build both from one `just` call.

**128 (4096 units), corrected 2026-09-22:** the sweep's "factor-64 tile covers half its region"
was wrong — `tile_regions(64)` truncating to `max(1)` correctly makes factor 64 a *per-region*
key of 2×2 grids, exactly how factor 16/32 already work at the default size, and the LOD builder
branches on `region_span() == 1`, not on the factor, so the overlay path with its `per <= 32`
assert is never taken. The real blockers were `MAX_DECOMPRESSED_PACKET_BYTES` (now 256 MiB ×
area ratio, capped at 2 GiB) and fixtures. **16** is untested (it would need its own ring table).

**Verified at 64** (separate target dir): `game` lib `lod::`/`protocol::` 27/27, `lod_streaming`
14/14, handoff suites 36/36, the seeded-schedule seam/corner walks 4/4, the real-thread walks in
`server/tests/threaded_world.rs` 4/4, `worldgen` lib 35/35 (sea surface exact at every factor),
`server` lib 5/5, client lib + bin + tests type-check. **Not run at 64:** any client test binary,
the client itself on a GPU, wasm. At the default size: `just smoke` green, touched tests green.

## 2. Measured cost (release, 130 W adapter — not the clamped USB-C state, n=1)

`server` example `genesis_probe`, `worldgen` example `snapshot_size`, region (0,0)/(-1,-1):

| | 32 chunks (1024 u) | 64 chunks (2048 u) | ratio |
|---|---|---|---|
| chunks per region | 2 671 | 10 527 | 3.9× |
| region start, one region | 368 ms | 1 309 ms | 3.6× |
| region start, nine at once | 421–568 ms | 1 661–2 323 ms | 4.0× |
| server peak RSS, nine regions | 471 MB | 1 837 MB | 3.9× |
| snapshot, raw | 40.3 MiB | 161.3 MiB | 4.0× |
| snapshot, on the wire (lz4) | 1.15 MiB | 5.46 MiB | 4.7× |
| snapshot encode / decode | 36 / 44 ms | 149 / 183 ms | 4.1× |

Everything per region is ∝ area; nothing is super-linear. Total CPU for the same land is about
equal (4 × 368 = 1 472 ms vs 1 309 ms). **Wall time is not**: four small regions start in parallel
in ~0.4 s, the one big region covering them takes 1.3 s — one thread per region means bigger
regions *reduce* parallelism per unit area. Not measured: tick cost with entities (expected
unchanged — physics is ~99.8 % of sim time and scales with dynamic bodies; the static voxel
colliders only deepen the BVH), the every-25-ticks `terrain_fingerprint` (∝ chunks: 5–23 ms → est.
20–90 ms against a 50 ms tick — **watch `[server]` tick overruns first** when trying 64), and the
whole client side (no GPU run here).

## 3. Does it make fast traversal smoother?

Sprint-flying at 345.6 u/s. On a flip the new strip's near edge is one region ahead, so the
**lead time** is `REGION_SIZE / speed`.

| | 32 | 64 |
|---|---|---|
| lead time before reaching the new strip | 2.96 s | 5.93 s |
| sim-critical path: 3 regions start in parallel + snapshot decode | ~0.5 s | ~1.9 s |
| **margin for the sim** (colliders exist before you arrive) | ~2.4 s | **~4.0 s** |
| client fill of the strip: voxels at `SNAPSHOT_VOXELS_PER_SECOND` 4800 | 1.7 s | 6.6 s |
| client fill: meshing at `MESH_UPLOADS_PER_SECOND` 3000 | 2.7 s | 10.5 s |
| flips per km flown | ~16 | ~8 |

So the intuition is right for what matters to correctness — the region, its colliders and the
handoff target exist with more slack, and there are half as many flips, which is where the known
seam problems live (held-input oscillation, ghost-relay mispredicts, `IsDefaultUiCamera` warning).
It is wrong for the visible fill at today's budgets: load is ×4, lead is ×2, so full-res terrain
lags further behind a fast flyer (LOD keeps covering it — roots retire only once the replacement
is meshed — so it shows as late detail, not holes). Nearest-first ordering already exists for
snapshot voxels; the budgets are the dial.

## 4. The thread argument

Per isolated player the near window is `(2r+1)²` regions = **9 threads at any region size**; only
the land under them grows. What bigger regions buy on the server is sharing (players within 2 km
of each other overlap more) and fewer park/restore cycles; what they cost is 4× state per thread
(~1.8 GB per isolated player at 64) and the "one region's tick fits one core" ceiling from the
multi-instance proposal being reached with a quarter as many players per km². Thread *count* is
not the scarce resource today (region ticks ≤12 ms of 50 ms, idle regions nearly free).

## 5. Recommendation

1. **Try 64 with the dial**, tiny/small world first:
   `just region_chunks=64 world=tiny run`, then `just region_chunks=64 adapter=6700 perf-capture`.
   Watch server tick overruns (terrain fingerprint), client RSS, `world_ready`, and fly a few
   flips. Compare only within one region size.
2. **If 64 feels right, make the near window distance-based before adopting it**: subscribe to
   regions within `D` units of the player (with hysteresis) instead of 3×3 around the home region.
   At 2048-unit regions and `D = 1024` — the same guaranteed full-res distance as today's worst
   case — that is always exactly **4 regions** (home + one x neighbour + one z neighbour + the
   diagonal): 4 threads and ~42k chunks per player instead of 9 threads and ~95k (today: 9 and
   ~24k). A flip loads 2 regions, not 3. At 1024-unit regions the same rule still needs all 9,
   which is why this only pays with the bigger size. It needs: `desired_window` by distance
   (`crates/client/src/instance.rs`), a `(1, 2)` ring-table entry for d=1 regions outside the
   window, and the cull bound following the window. Not implemented — it is a design change to
   the subscription model, not a testing knob.
3. Independently of size, a **connect handshake carrying a build tag** (the multi-instance
   proposal already wants `build_id`) would make mismatched dials fail cleanly.
4. Do not go to 128 without generalising the LOD tile contract and chunking the join snapshot.

## 7. Addendum 2026-09-22: 128 chunks (4096 units, 256 m)

Asked for "double again". Made legal (`REGION_CHUNKS_CHOICES = [32, 64, 128]`), ring table
`[(2,8),(3,16),(6,32),(8,64)]` (same 2.5 px budget, same 32 768-unit horizon, ~280 keys; factor
64 is per-region here), packet cap scaled by area, `SMALL_LOD_RING_TABLE` follows the finest
production factor, fixtures in `lod.rs`, `worldgen` and client `instance.rs` pinned to the
default-size table or derived from the constants.

**Verified at 128** (same set as at 64): game lib `lod::`/`protocol::` 27/27, handoff suites
36/36 + `lod_streaming` 14/14, seeded walks 4/4, real-thread `threaded_world` 4/4, worldgen lib
35/35, server lib 5/5, client lib + bin + tests type-check clean. Default size: `just smoke` green.

**Measured** (release, same probes, n=1):

| | 32 | 64 | 128 | 128 / 32 |
|---|---|---|---|---|
| chunks per region | 2 671 | 10 527 | 49 982 | 18.7× |
| region start, one region | 368 ms | 1 309 ms | 3 856 ms | 10.5× |
| region start, nine at once | 0.42–0.57 s | 1.7–2.3 s | 5.7–7.4 s | 13× |
| server peak RSS, nine regions | 471 MB | 1 837 MB | 6 915 MB | 14.7× |
| snapshot, raw / wire | 40.3 / 1.15 MiB | 161 / 5.5 MiB | 578 / 33.5 MiB | 14.3× / 29× |
| snapshot encode / decode | 36 / 44 ms | 149 / 183 ms | 586 / 736 ms | 16× |

Chunks grow 18.7× rather than 16× because the region is tall enough to catch more of the
terrain's vertical range; genesis is slightly sub-linear per chunk. What it means for a 3×3
window per client: ~450k chunks, ~7 GB of server state, a 33 MB wire snapshot per region and a
0.7 s decode on the game thread per region (the client decodes on its net thread but applies on
the main thread — expect visible `main_stall`s). Cave-culling scratch at 128 is a 384×~20×384-cell
box (~47 MB of `u128`), fine; the flood itself is change-gated. The lead-time argument from §3
scales the same way: sim margin at sprint-fly ≈ 11.9 s − ~7.5 s (three regions in parallel +
decode) ≈ **4.4 s** — barely better than 64's 4.0 s — while the visible fill of a strip now takes
~26 s of voxels and ~42 s of meshing at today's budgets. **128 is where the 3×3 window stops
being defensible**; it only makes sense with the distance-based window of §5 (at 4096-unit
regions and D = 1024 that is 1–4 regions, usually 1).

Not run at 128: the client on a GPU, client test binaries, wasm (a 578 MiB decode buffer on
wasm32 is its own problem — `MAX_DECOMPRESSED_PACKET_BYTES` is 2 GiB there, the browser will not
be).

## 6. The dial

`game::REGION_CHUNKS` is parsed at compile time from `VOXEL_REGION_CHUNKS` (`option_env!` +
`const fn`, no build script). Build-time rather than runtime because it sizes const expressions
and client, server and wasm `LocalServer` must agree bit-exactly. The default lives in
`.cargo/config.toml` `[env]` so raw `cargo`, rust-analyzer and `just` share one fingerprint
(cargo tracks the variable; "unset" vs "32" would rebuild `game` and everything downstream on
every alternation); `just region_chunks=64 …` overrides it. Switching recompiles `game` and its
dependents, not Bevy or the vendored forks. For side-by-side experiments without thrashing the
cache: `CARGO_TARGET_DIR=$PWD/target/region64 VOXEL_REGION_CHUNKS=64 cargo …`.
