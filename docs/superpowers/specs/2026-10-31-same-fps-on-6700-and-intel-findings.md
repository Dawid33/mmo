# Same FPS on the RX 6700 and the Intel iGPU — findings (2026-10-31)

Question: `just adapter=6700 …` and `adapter=Intel` give the same frame rate — is the
pin broken, or is something else the bottleneck?

## Answer

The pin works; the client is **CPU-bound on the main thread**, so the GPU does not matter.

| capture (release, `full`, 512 m region, 60 s) | selected adapter | steady frame | GPU work / frame |
|---|---|---|---|
| `target/perf-capture/20261031T110516Z-6700-full` | AMD Radeon RX 6700 XT (RADV NAVI22), 790 MB VRAM used | 34.0 ms (29.7 fps) | 1.60 ms (5 %) |
| `target/perf-capture/20261031T105917Z-Intel-full` | Intel Iris Xe (ADL GT2) | 35.3 ms (28.6 fps) | 3.54 ms (10 %) |

Frame time rises with entity count in both runs (16.7 ms at 23k entities → 35–40 ms at
343k). The 512 m region fans out to ~331k render-brick entities
(`snapshot_voxels_applied bricks=331260`).

Display: every connected monitor (eDP-1, DP-3, DP-4) is on card1 = the Intel iGPU, and
all three run at 60 Hz with vsync (Fifo). Frames rendered on the 6700 are PRIME-copied over
Thunderbolt to Intel for presentation. That copy, and the 60 fps vsync cap, are the next
ceilings once the CPU problem is fixed — neither is the limit today.

## Where the CPU frame goes

`trace-chrome` on the 6700 (`target/trace/client.json`, 40–60 s window, 545 frames in 20 s):

| per frame | system |
|---|---|
| ~14.7 ms | `bevy_transform::propagate_parent_transforms` (+ ~0.5 ms `mark_dirty_trees`) |
| ~13.4 ms | `client::renderer::meshing::queue::spawn_mesh_tasks` |
| ~3 ms    | visibility propagation / CPU culling over the same entities |
| ~1.3 ms  | `commander::level::level_input` (surprisingly large; not investigated) |

### 1. Transform propagation walks every brick every frame (steady state)

Every chunk *and every brick* is a direct `ChildOf(region root)` (`bricks.rs`: bricks are
siblings of their chunk under the root), and so are the moving entities (bodies, slimes,
rig models below them). Bevy 0.19's static-scene optimisation (`bevy_transform`
`systems.rs`):

- `mark_dirty_trees` marks the root's tree changed whenever ANY descendant's `Transform`
  changes;
- for a dirty root, `*parent_transform = GlobalTransform::from(*transform)` writes through
  `Mut` — the root's `GlobalTransform` is flagged changed even though the value is equal;
- each child is skipped only if `!tree.is_changed() && !p_global_transform.is_changed()`, so
  ALL ~345k direct children get visited and `set_if_neq`'d.

So one animated rig or hopping slime anywhere in the region costs a ~345k-entity walk per
frame. This does not go away when loading finishes.

**Fix:** put terrain under an intermediate static entity per region
(region root → `terrain` group → chunks + bricks; bodies stay directly under the root).
Children ARE written with `set_if_neq`, so the group's `GlobalTransform` stays unchanged and
its subtree is skipped. Bricks already take their chunk's parent (`spawn_chunk_brick`), so
the change is where chunks are spawned (`drain_region_updates` `AddTerrainChunk`,
`spawn_region_snapshot`) plus `RegionRoots` gaining the group entity; bridge/bricks tests
that assert `ChildOf == root` need updating. Region despawn still works (recursive).

### 2. `spawn_mesh_tasks` sorts the whole remesh queue every frame (join phase)

With 100k–200k bricks queued, each frame runs `queue.dirty.retain(...)` over the whole
map, ranks every entry with `chunk_rank` (camera/frustum), collects them and
`sort_unstable`s them — only to take `capacity` entries. Meanwhile `in_flight` sat at 60–90
(the worker pool was idle), and the queue drained at ~3,700 bricks/s: at 58 s, 110k were
still waiting (~90 s to load the region; `world_ready` never fired in 60 s). The slow
frames throttle the spawning, which slows the drain.

**Fix:** `select_nth_unstable_by_key(capacity)` instead of a full sort (O(n)), and/or
rank only every N frames or when the camera moves; drop the per-frame whole-map `retain`
(check `chunks.contains` lazily for the taken entries). Larger: keep the queue bucketed by
distance ring so selection is O(capacity).

## Next ceilings after both fixes

- Visibility propagation + `check_visibility_cpu_culling` over 343k entities (~3 ms).
- Present: vsync at 60 Hz (`PresentMode::Fifo`; the DP-1-1 monitor supports 165 Hz but is
  set to 60) and the 6700→Intel PRIME copy. Plug a monitor into the 6700 to take the copy
  out.

## Result (both fixes implemented, same day)

- Transforms: each region now has a separate **bodies root** (`bridge::RegionRoots::bodies`,
  `BodiesOf` / `RegionBodies` relationship with `linked_spawn`, so it despawns with the region
  root) at the same fixed offset; chunks and bricks stay under the terrain root, every other
  sim entity goes under the bodies root. The terrain tree is now walked only on frames where
  terrain itself changed (brick spawns during the join, edits).
- Remesh queue: `meshing::queue::pick_nearest` — queues ≤ `RANK_CACHE_MIN_QUEUE` (4096) are
  ranked exactly every frame (partial select, no full sort); longer ones rank once into a
  `RankCache` of the nearest `capacity × RANK_CACHE_FRAMES × 2` entries, used for
  `RANK_CACHE_FRAMES` (8) frames, with stale entries purged on each re-rank.

| capture (release, `full`, 512 m) | steady frame | world_ready |
|---|---|---|
| 6700 before (`20261031T110516Z-6700-full`) | 34.0 ms (29.7 fps), CPU-bound | not in 60 s (~90 s projected) |
| 6700 after (`20261031T114245Z-6700-full`) | **16.7 ms (60 fps), vsync-capped**, GPU 1.5 ms | 58.5 s |
| Intel before (`20261031T105917Z-Intel-full`) | 35.3 ms (28.6 fps) | not in 60 s |
| Intel after (`20261031T115000Z-Intel-full`, 120 s) | **16.3–20 ms (50–60 fps)**, GPU ~3 ms | 60.4 s |

Both adapters are now at or near the 60 Hz vsync cap; to see real headroom, run with
`PresentMode::AutoNoVsync`. Still open: join frames peak ~24 ms while bricks spawn (the
terrain tree is dirty those frames, a full ~345k walk), and the join itself is ~60 s at 512 m.
