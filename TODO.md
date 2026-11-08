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

- World map + Play follow-ups: embark (choosing a site/party, not just a tile); adventure-mode travel between map tiles (Play, not walking; walks now plan across regions with PathPlan); real biomes on the map tiles (today a summary of height/water/geology); `just clippy` is red on toolchain 1.99.1 in macros / bbmodel / slotmapd / block-mesh; the live check of the world map (Esc menu, M, tile panel, Play, camera landing, old region parking) is still pending.
- Adventure mode landed 2026-10-31 (plans/2026-10-30-adventure-mode.md: PlayMode + SetMode, MoveTo + A* Walker, seam handoff, Follow, near-window bounds, right-click orders with a path preview). Live check pending (one client, adapter=Intel: Adventure, select the dwarf, hover draws the path / red cross, right-click walks it, Follow, walk across a seam with the camera staying on it). Deferred: swimming/wading, ladders/ramps, falls over one block, following a non-player entity across a seam, a sim-emitted drawn route, orders for other entities (workers); the hover preview searches at most PREVIEW_MAX_PATH_NODES (2048) of the sim's 32 768 (a long detour shows an amber "not previewed" cross; the click then searches at the full budget).
- Chunk-16 follow-up (needs Dawid's go-ahead to measure): client brick landing per frame (a whole chunk is ~200-300 bricks/frame vs the cap of 160), client replay churn when a predicted break materialises ground (RemoveEntity + AddTerrainChunk re-spawns bricks per replay), and a solid chunk's first-edit full collider rebuild; then fix what the numbers show.
- `just pull-smoke` is red: since 9aed311 `prepare_chunk_quads` needs `OverlayTexture`, which only `FortressOverlayPlugin` creates; the probe doesn't add it (production unaffected). Fix: `VoxelPullPlugin` falls back to a 1x1x1 placeholder overlay texture.

- ~~Client per-region clock tracking (single shared tick_rate today; 9 SyncClock streams fight over it).~~ **RESOLVED 2026-11-06 (seam-transparent regions Task 5.3)**: the client keeps ONE predicted world tick (`regions::clock::PredictedClock`), steered by every region's SyncClock (all world-numbered; the newest sample per round decides); every predicted region runs to it (bounded catch-up bursts, held when ahead), so client regions close slots together like the server's.
- ~~Durable region persistence (parking lot is in-memory only).~~ RESOLVED: the world autosave (`server/src/region_store.rs`, forged worlds) writes every region and the world tick.
- ~~Cross-region event relay groundwork (deferred with handoff).~~ RESOLVED 2026-11-06 (seam-transparent regions): one world tick, stamped transfers, the terrain/entity halo and intents (`intents.rs`) carry everything across a seam.
- Stable client identity across connections: reconnects get a fresh ClientId, so the manager's no-duplicate-player reconnect path only fires for same-id sessions; the old player entity lingers in its home region.
- Cross-region interactions beyond collision: pickup, piles, chests, mining and placing work across a seam through intents (seam-transparent regions, Phase 6); combat is still a future spec.
- ~~Ghost mirrors TTL'd~~ RESOLVED 2026-11-06 (seam-transparent regions Task 5.2): the entity halo ends mirrors explicitly (`HaloEntityGone`); halos are reseeded when a region starts.
- ~~Seamless local-player crossing under latency~~ RESOLVED (Tasks 3.2/3.3, below) — the local client's own boundary crossing
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
- ~~**Adventure walk is not seam invariant**~~ FIXED (Task 6.5, `path_plan.rs`): walks plan in world
  tiles over region ∪ halo and walk on along the carried path after a hand-off, so
  `handoff_schedule::adventure_walk_is_seam_invariant` passes lockstep (Seam(X)/Seam(Z) 2.3e-5 blocks,
  was 0.17; Corner 5.3e-5). The coarse route now reads `WorldMap` tile summaries (`RouteMap`, Ruling R28): it walks around sea and mountain regions.
- **Dynamic bodies across seams (Task 7.5, 2026-11-06)**: voxel bodies (felled trees, detached
  terrain) are `EntityKind::Body`s that cross seams with their model and `DropItems` drops
  (`Carried::voxel_body` / `Carried::drops`); `TIP_EDGE_MARGIN_BLOCKS` is gone, a tree falls across a
  seam; the F3 Debug tab's "Drop body at cursor" sends `Order::DevDropBody` (a 1 m cube, drawn as a
  placeholder box). **Live check pending** (one client, `just adapter=Intel world=small run`): drop
  bodies on a slope toward the home region edge, fell a tree across the seam in Adventure — each
  crosses without a pop or a frozen frame, the orientation interpolates across the region-local frame
  change, and the logs lie where the tree lands. Known gaps: a voxel body's halo mirror whose owner the
  client does not predict draws as the placeholder box (the mirror has no model); a voxel body's
  collider material (friction 0.9) is re-set on arrival, its mirror keeps the default.

In-engine UI (spec docs/superpowers/specs/2026-07-07-in-engine-ui-design.md,
plan docs/superpowers/plans/2026-07-07-in-engine-ui.md): inventory/hotbar landed
(engine-owned data → client-side widget projection → bevy_ui). Follow-ups: real
item icons/textures, drag-drop UX, additional screens, and the deferred diegetic
world-space panel case (needs in-engine taffy + deterministic text measurement).

Adventure survival follow-ups (landed 2026-11-05, plans/2026-11-05-adventure-survival-items.md; live run pending):
- Not built: tools, crafting, health, hunger, fall damage; authentication for player tokens (comes with the external database behind `PlayerStore`); a thrown-item arc animation; a block-crack overlay (progress is a bar under the crosshair); fortress dig orders yielding items (one `deposit_items_safe` call in `clear_tiles_safe`).
- Restore keeps position, inventory and mode only: a walk in progress and animation state of a parked player are lost.
- A parked owner's fortress claim can be taken by another player while they are away.
- Chests: opening uses the eye ray within `edit::REACH` (5 blocks), staying open `CONTAINER_REACH_BLOCKS` (6) from the feet tile, so a chest ~5 blocks above the eye silently cannot be opened; an open chest and the inventory screen are carried across a seam by design (Task 6.4: closed by reach / an unseen chest; mining and its cooldown still reset in `Client::arrive`).
- UI: the hotbar flickers empty when the view is None mid-handoff; M / menu can stack over the inventory; inventory slots are plain themed rectangles (restyle to the widgets' slot style).
- Crash-only duplication window: a player's park record is written at once but the region file only every `WORLD_SAVE_SECS` — items picked up after the last region save, then a park, then a crash, can come back in the world and stay in the record (fix: save the region when one of its players parks).
- Client re-sim double-counts `items_destroyed` (server count is exact); a refused `set_block_in` leaves a silent repeat mine cycle.

Terrain slopes follow-ups (landed 2026-11-06, spec specs/2026-11-06-terrain-slopes-design.md):
- Live look: `just run --reset` at hillsides, shores and slopes, and walking up/down them in Adventure (keyboard and `MoveTo`).
- Feel check of `movement::GROUND_SNAP` 0.6 m: small player-made ledges now snap the body down instead of a short fall.
- The tree_fall test skips cactus (needs a real scene for a single-block tree).
- Fortress-mode stair building (TBD with the building rework).
- Old saves are reset: `REGION_FILE_VERSION` 7 set pre-slope region files aside as `.bak` (superseded: see the seam-transparent regions follow-ups below — it is 21 now).

Seam-transparent regions follow-ups (program landed 2026-11-06/07, spec specs/2026-11-06-region-seam-crossing-overview-and-prior-art.md):
- **Saves reset on the live deploy**: the program bumped `REGION_FILE_VERSION` 7 → 21 (`EntityId`, halo, intents, piles/chests by world cell, world-tile walks, bodies). Every existing world's saved regions are set aside as `.bak` and regenerate from the world file (terrain edits lost); no migration was written. Player records (`PLAYER_FILE_VERSION` 5) still load.
- **Live check of the whole program** pending (one client, `just adapter=Intel world=small run`): mining / placing / chests / piles across a seam, walking along and over a seam (halo, no dip), a felled tree across a seam, one-clock prediction (no stall at a crossing), a crossing at near radius 0 into a region the client does not hold (inputs are sent unpredicted until its snapshot loads).
- Halo cost unmeasured at 256 m regions (~68 band columns, ~3.2 MiB serialized per seed, ~4× mirrored entities); halo traffic from neighbours the client does not run (HaloEntity / HaloUpdate / Intent) is one rollback + replay per server tick in edge regions — measure with `[client] by_kinds`; a lever: apply events from non-predicted sources without counting / rolling back.
- Physics findings (Task 7.3, pre-existing, not seam-specific): (1) chunk-edge ghost contacts — a dynamic body sliding over the boundary of two chunk colliders stops dead or tips 90° (each chunk's `Voxels` shape knows nothing of its neighbour); will hit felled trees and voxel bodies; fixes: internal-edge correction across neighbouring `Voxels` in the fork, or one compound terrain collider per column/region. (2) Rapier fork `debug_assert` in `NarrowPhase::debug_validate_solver_graph` (`solver_graph.rs:824`, "solver contact graph size 6 != selection size 2 (full_rebuild=false)"): a `ColliderSpec::Ball { radius: 8 }` body spawned at (513.4, 24, 509.4) in a floored region with linvel (−46, −18, −37), angvel (−4.5, 2.4, 5.6) panics on its first tick; release builds skip it and solve a wrong contact set. Nothing spawns balls yet — repro in physics-next before they are used.
- No world-wide slime cap (only the per-target mailbox cap `PENDING_ARRIVALS_PER_REGION_MAX`).
- A neighbour saved by an earlier server process stays pristine in a running region's halo until it runs (the halo seed of a never-run-this-session region is worldgen).
- The autosave round is not atomic across region files (a crash mid-round can save a crossing in neither or — in a narrow window — both); a stopping SOURCE at the round's start may leave a save gap (Task 3.4); no end-to-end manager restore test from saved transfers.
- `EntityId` serials are disjoint per incarnation (`Region::start_at`: `world tick << ENTITY_SERIAL_TICK_SHIFT`), but a region regenerated after a crash that lost the world tick's progress (`world.tick` saved every `WORLD_SAVE_SECS`) can restart below the crashed process's serials; only ids persisted elsewhere by that process (a parked record written on disconnect) could collide.
- A refused survival place's block reaching a player who is gone (not parked) is counted in `items_destroyed` instead of being put down: the reply carries no cell (`IntentReply::carried_items`).
- Designations are still clipped to the home region (`orders.rs` `clip_to_region`): move them to world tiles + intents when workers land.
- Mining / placing across a seam into a neighbour that is not running runs the full mining time, then `OwnerNotRunning` and the cooldown, in a loop (the halo looks mineable): a per-halo "owner running" bit, or show the refusal.
- People wander with 2× `MAX_PATH_NODES` on unreachable goals; a voxel body's halo mirror re-sends its whole compound collider every tick (wire/CPU).

Static water layer follow-ups (landed 2026-11-07, plan plans/2026-11-06-water-static-layer.md; code review pending):
- Static limits: dug pockets under water stay dry (the recipe is per column, a dug cell reads as air above `surface + 1`); placed blocks do not relevel water; no drowning (`DROWN_DEPTH` is unused); no currents.
- A body in static (depth-7) water cannot take a move order: `current_tile` / `nearest_standable` find no standable tile for it, so only F5 swimming gets it out — and the click gives no feedback.
- Underwater fog only inside the home region: `eye_in_water` reads the home tile mirror, so it is false in the fortress-neighbourhood regions.
- Whole-cell immersion: a player in 1-deep shoreline water swims (no jump except climbing out); people who end up in deep water cannot climb out (they never hold jump); a fall into shallow water can still hit the bed.
- LOD water wire size and the per-rebuild worker cost (`LodWater`) are unmeasured (the shipped depth is clamped at `LOD_WATER_MAX_DEPTH_BLOCKS`): cache per key if they matter.
- The water tile readiness gate scans every `PendingSnapshotVoxels` per frame while waiting.
- Live checks pending (Dawid): Tasks 6-9 visuals (water pass, cut discard, underwater fog, LOD water), swim feel, the WebGL2 browser check.
- Measure before the flow plans: (1) a `PERF_LOG=1` embarked run at 256 m, which Dawid runs himself; (2) the body-restructure budget prototype round.
- Open decision 1: is the world ledger the water cycle (recommended yes)?
- Open decision 2: should clients predict water arriving across a seam? Worth revisiting now that predicted transfers land in the server's slot, carry an identity and are conservation-checked (the research advised against it because relayed transfers orphaned).
- Flow-plan hooks (seam spec): water flux leaves like a departure (stamped `RegionOutput` -> `RegionInput::Transfer`, applied at `max(T+1, next tick)`); level deltas ride the halo's stamped deltas; water volume joins the manager's `in_flight` conservation invariant; `pending_arrivals` needs an uncapped water ledger (never dropped by its overflow rule); step water when `tick % WATER_STEP_TICKS == 0` on the shared `WorldClock`; pumps/gates/buckets on water a region does not own are `Effects` intents.

R2a login service (plans/2026-11-07-r2a-login-accounts.md) — DONE 2026-11-07: accounts + Argon2id + EdDSA join tickets (`crates/login`, `auth`, `login-api`), server `VOXEL_AUTH=jwt` (`auth_edge.rs`), native + browser login screens ("Play online"), `just dev-login`. Next: R2b role split (plans/2026-11-07-r2b-role-split.md). GDPR delete/export lands in R5; rate limiting on the login endpoints is deferred.
- `just run-online` (2026-11-08): straight into the game WITH login, no screens — ensures the verified dev account (`dev_email` / `dev_password`, `login account ensure`), starts the login service + jwt dev server, runs `client debug online` (logs in, joins world `dev`, embarks / returns to the account's fortress); a login/join failure prints the error and exits non-zero.
- R2a live check pending (Dawid, one client, adapter=Intel): first `just adapter=Intel run-online` -> in the world as `dev@localhost` with no screens (quit, run again -> back in the same fortress); then the screens: `just dev-login`; register + verify via the link in `target/dev-login/login.log`; native client with the login service in its environment (`VOXEL_LOGIN_URL=http://127.0.0.1:8790 just play` — `play` passes the env through; without it the menu has no Play online) -> Play online -> log in -> world `dev` -> Join -> in the world; a second native login of the same account takes the first over (the first shows "logged in elsewhere"); `just run` with no login service still starts single-player; browser: `just with_web=1 dev-login` (or `just web`) + `?login=http://127.0.0.1:8790` -> log in -> join over WebTransport. Watch for: Back while a login call hangs frees the screens (the browser's calls time out after `WASM_REQUEST_TIMEOUT_SECS`, native after `REQUEST_TIMEOUT`); the browser join to `dev` pins the dev hash file (the ticket has no `cert_hash`, its URL is local); a ticket's `cert_hash` must be 64-hex SHA-256 (the dev self-signed hash file format).
- R5: restore the `{origin}/login` fallback of the browser's login URL (`login::api::resolve_login_url`) once the login service is served behind `/login` on the public site — dropped in R2a because nothing serves `/login` there, so a deployed page would open on a dead login screen.
- Shared refresh token, known gaps (R2a): clients on one machine share the kept token and re-read it before every refresh / join / logout, and the native client no longer refreshes at startup — but a refresh or join whose RESPONSE is lost (timeout, crash) still signs the player out (the token was rotated server-side; the next use of the old one is reuse → the family is revoked → log in again), and two clients presenting the same token at the same instant can still race into a reuse revocation.
- R2b/R5: the native client's QUIC connect skips certificate verification (`SkipServerVerification`, `netcode.rs`) while it now sends a bearer join JWT — pin a cert hash for QUIC too (from the ticket) or use a real certificate before any `jwt` host is public.
