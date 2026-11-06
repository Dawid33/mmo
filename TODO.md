Direction (2026-10-20): Dwarf Fortress–style game on the multi-region engine, MVP = fortress
mode. No phased plan (rejected 2026-10-20): implement features piece by piece, roughly
modelling DF; reference research in docs/superpowers/specs/2026-10-20-dwarf-fortress-research/
(00-synthesis.md). The commander
camera (renderer/commander/) is the default view; live feel pass on this branch pending.
Fortress selection/designation UI landed 2026-10-29 (game::orders, commander level + designate
tools, cutaway, overlay, Slint tool bar; live check pending). Next step: workers/jobs that consume
designations (dwarves pathing to and digging/building them, replacing the F3 "Complete all
orders" stand-in, incl. top-down deconstruction scheduling); then stockpiles and zones.

Multi-region world follow-ups (milestone landed; spec 2026-07-05-multi-region-world):
- Cell mode removed 2026-10-30 (plans/2026-10-30-remove-cell-mode.md): whole regions + seams; a client never simulates more than its own window. Play regions are 512 m since 2026-10-30 (one-region window, single factor-16 LOD ring); live run at 512 pending.

- World map + Play follow-ups: embark (choosing a site/party, not just a tile); adventure-mode travel between map tiles (Play, not walking; walks reach only one region away, see below); real biomes on the map tiles (today a summary of height/water/geology); `just clippy` is red on toolchain 1.99.1 in macros / bbmodel / slotmapd / block-mesh; the live check of the world map (Esc menu, M, tile panel, Play, camera landing, old region parking) is still pending.
- Adventure mode landed 2026-10-31 (plans/2026-10-30-adventure-mode.md: PlayMode + SetMode, MoveTo + A* Walker, seam handoff, Follow, near-window bounds, right-click orders with a path preview). Live check pending (one client, adapter=Intel: Adventure, select the dwarf, hover draws the path / red cross, right-click walks it, Follow, walk across a seam with the camera staying on it). Deferred: swimming/wading, ladders/ramps, falls over one block, following a non-player entity across a seam, a sim-emitted drawn route, orders for other entities (workers), goals more than one region away; the hover preview searches at most PREVIEW_MAX_PATH_NODES (2048) of the sim's 32 768 (a long detour shows an amber "not previewed" cross; the click then searches at the full budget).
- Chunk-16 follow-up (needs Dawid's go-ahead to measure): client brick landing per frame (a whole chunk is ~200-300 bricks/frame vs the cap of 160), client replay churn when a predicted break materialises ground (RemoveEntity + AddTerrainChunk re-spawns bricks per replay), and a solid chunk's first-edit full collider rebuild; then fix what the numbers show.
- `just pull-smoke` is red: since 9aed311 `prepare_chunk_quads` needs `OverlayTexture`, which only `FortressOverlayPlugin` creates; the probe doesn't add it (production unaffected). Fix: `VoxelPullPlugin` falls back to a 1x1x1 placeholder overlay texture.

- ~~Client per-region clock tracking (single shared tick_rate today; 9 SyncClock streams fight over it).~~ **RESOLVED 2026-11-06 (seam-transparent regions Task 5.3)**: the client keeps ONE predicted world tick (`regions::clock::PredictedClock`), steered by every region's SyncClock (all world-numbered; the newest sample per round decides); every predicted region runs to it (bounded catch-up bursts, held when ahead), so client regions close slots together like the server's.
- Durable region persistence (parking lot is in-memory only).
- Cross-region event relay groundwork (deferred with handoff).
- Stable client identity across connections: reconnects get a fresh ClientId, so the manager's no-duplicate-player reconnect path only fires for same-id sessions; the old player entity lingers in its home region.
- Cross-region interactions beyond collision (combat, pickup) — future spec.
- Ghost mirrors for parked-region persistence are TTL'd, not persisted.
- Seamless local-player crossing under latency — the local client's own boundary crossing
  converges bit-exact but has a transient rubber-band under active input (spec-accepted
  flip-tick divergence). Proper fix needs a protocol-level design (server-side input
  routing / predicted-authority-transfer); see docs/superpowers/2026-07-06-seamless-local-crossing-followup.md.
  - **RESOLVED 2026-11-06 (seam-transparent regions Task 3.2)**: all three `crossing.rs` tests pass —
    `walking_across_a_region_boundary_keeps_input_and_converges` (red 2026-09-20 .. 2026-11-06, since
    8e90481), `crossing_is_deterministic`, and `crossing_converges_under_late_region_scheduling` over
    all 6 `InlineSchedule::late` seeds. The 2026-09-20 "new home permanently bit-inexact" was the
    client's regions not running in step with each other (the home leads its neighbours by ~3 ticks
    in this harness), so predicted transfers met the server's in other slots. Fixed by: predicted
    arrivals / mirrors stamped with the source's tick and held to the END of that slot
    (`regions/runner.rs`: released by the Tick that closes it, after the slot's predicted inputs —
    the server's order: held inputs, then held transfers); reconcile treating margin mirrors per
    slot (a server mirror never consumes a LATER slot's prediction; a server Tick drops a mirror
    predicted in the slot it closes — before, one missing mirror shifted every later one by a slot
    for good); and a predicted arrival of an entity already living in the region (the
    authoritative copy got there first) dropped instead of floating in the log re-applying its
    stale bundle. Harness: the server hashes are now SLOT-START hashes (right after the Tick event,
    as the desync check hashes; `InlineSpawner::record_slot_start_hashes`), and
    `assert_converged`'s heal steps wait for every aligned region, not only the home.
  - **RESOLVED 2026-11-06 (Task 3.3, inputs follow the body)**: input held across a seam. The server's
    source region hands a departing client's held inputs (stamp >= departure) to the target in the
    bundle, and forwards inputs that reach it after the body left (`RegionOutput::Forward`) to the
    home; an input for a held arrival waits behind it (server and client runner alike).
    `evict_orphan_local_inputs` and `Misrouted` are gone; reconcile drops a local input replayed
    where the local body is not (also heals a mispredicted crossing tick). Tests:
    `handoff_schedule::held_key_across_seam_settles` (lockstep + jittery) and
    `crossing::held_key_crossing_has_no_rubber_band` (100 ms RTT via `SimHarness::set_latency`).
  - Previous status, kept for the root-cause notes: deterministic headless coverage in the same test. Root-caused via SimHarness: the flip-tick divergence at the just-crossed home
    region is genuinely TRANSIENT — it reconciles bit-exact within ~1 tick once input stops
    (measured: diverged at the flip tick, converged the next tick, stable 240+ ticks after).
    The earlier "not self-healing" reading was the harness asserting one tick too early
    (`assert_converged` now lets the documented bounded transient heal before its strict check).
    (The held-input case deferred here was resolved by Task 3.3, above.)
  - Validation tool for that design, when it lands: `game::InlineSchedule` (seeded region-scheduling
    jitter on the `InlineSpawner`; `SimHarness::set_schedule`). Lockstep is the one schedule the
    threaded server never runs, so judge a candidate protocol by `crossing.rs` convergence across
    `InlineSchedule::late` seeds, not by the lockstep run alone. `jittery` (missed ticks) needs the
    client per-region clock tracking item above first — the harness aligns client and server by tick.
- **Adventure walk is not seam invariant** (found 2026-11-06 by the seam-translation harness,
  Task 4.1; owner: Task 6.5 pathfinding/walker): `handoff_schedule::adventure_walk_is_seam_invariant`,
  lockstep (no seed), placements `Seam(X)` and `Seam(Z)` — a `MoveTo` 8 blocks diagonally through
  the origin reaches the same goal tile in every placement, but rests 0.165 blocks off the centred
  run's point at a straight seam (Corner: 0.012, within `POSE_TOL_BLOCKS` 0.025). Cause: a walk
  split at a seam goes to `edge_toward`'s edge tile, `steer` aims its last tile a block past the
  edge along the exit axis only, and the walk re-plans on arrival (`resume_walker_safe`), so the
  body follows a different line. Pinned with `should_panic` until the walker plans across the seam;
  `adventure_walk_reaches_its_goal_in_every_placement` holds the discrete outcome meanwhile.

In-engine UI (spec docs/superpowers/specs/2026-07-07-in-engine-ui-design.md,
plan docs/superpowers/plans/2026-07-07-in-engine-ui.md): inventory/hotbar landed
(engine-owned data → client-side widget projection → bevy_ui). Follow-ups: real
item icons/textures, drag-drop UX, additional screens, and the deferred diegetic
world-space panel case (needs in-engine taffy + deterministic text measurement).

Adventure survival follow-ups (landed 2026-11-05, plans/2026-11-05-adventure-survival-items.md; live run pending):
- Not built: tools, crafting, health, hunger, fall damage; authentication for player tokens (comes with the external database behind `PlayerStore`); a thrown-item arc animation; a block-crack overlay (progress is a bar under the crosshair); fortress dig orders yielding items (one `deposit_items_safe` call in `clear_tiles_safe`); item piles crossing region seams (deposits clamp into the region).
- Restore keeps position, inventory and mode only: a walk in progress and animation state of a parked player are lost.
- A parked owner's fortress claim can be taken by another player while they are away.
- Chests: opening uses the eye ray within `edit::REACH` (5 blocks), staying open `CONTAINER_REACH_BLOCKS` (6) from the feet tile, so a chest ~5 blocks above the eye silently cannot be opened; `inventory_open` persists after a seam handoff (mining, its cooldown and the open chest are reset — `Client::reset_region_local`).
- UI: the hotbar flickers empty when the view is None mid-handoff; M / menu can stack over the inventory; inventory slots are plain themed rectangles (restyle to the widgets' slot style).
- Crash-only duplication window: a player's park record is written at once but the region file only every `WORLD_SAVE_SECS` — items picked up after the last region save, then a park, then a crash, can come back in the world and stay in the record (fix: save the region when one of its players parks).
- Client re-sim double-counts `items_destroyed` (server count is exact); a refused `set_block_in` leaves a silent repeat mine cycle.

Terrain slopes follow-ups (landed 2026-11-06, spec specs/2026-11-06-terrain-slopes-design.md):
- Live look: `just run --reset` at hillsides, shores and slopes, and walking up/down them in Adventure (keyboard and `MoveTo`).
- Feel check of `movement::GROUND_SNAP` 0.6 m: small player-made ledges now snap the body down instead of a short fall.
- The tree_fall test skips cactus (needs a real scene for a single-block tree).
- Fortress-mode stair building (TBD with the building rework).
- Old saves are reset: `REGION_FILE_VERSION` 7 sets pre-slope region files aside as `.bak` and regenerates them.
