# Main menu, map generator and Load world — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The native client opens on a main menu (New world / Load world / Exit). New world is a Factorio-style map generator over the world forge; both paths start a local server child on the chosen world and land on its world map. In game, Esc → Main menu leaves the world cleanly, in-process, and another world can be started.

**Architecture:** worldgen gains forge progress/cancel, presets, preview rendering and an on-disk world store (`worlds/<slug>/`). The server gains `ClientPacket::Leave` and a single-player mode that exits when its last client leaves. The client gains a Bevy `AppState`, a session reset registry + `SessionScoped` marker so a world session can be torn down and re-made in one process, disconnects as events instead of panics, a child-server supervisor, and three new Slint screens on a separate `FrontendUi` layer.

**Tech Stack:** Rust stable 1.99.1, Bevy 0.19.1 (`States`, run conditions), Slint 1.18.1 software renderer on the `../ui` widgets, `std::process::Command` for the child server, `serde_json` + `image` (PNG) for world metadata/previews.

**Spec:** `docs/superpowers/specs/2026-10-30-main-menu-and-map-generator-design.md` (read it first — this plan argues from it, especially "Session lifecycle" and "Decisions").

## Global Constraints

- One world per server process; the `forged::install` `OnceLock` is the intended invariant — never make it swappable.
- Back to the main menu is a clean **in-process** exit; no client relaunch.
- v1 keeps `WorldManager`'s spawn-on-connect (`SPAWN_REGION`) unchanged.
- Worlds live in `worlds/<slug>/` under the repo root (`worldgen::forged::from_repo_root`); `worlds/` is already git-ignored.
- Every screen is Slint (`crates/slint-ui`); no new `bevy_ui`. Do not modify `../ui`.
- The generator UI is built from `worldgen::forge::knobs::SPECS` / `CHOICES` at runtime; no second copy of knob labels, ranges or defaults.
- wasm keeps today's flow (no menu); everything new that spawns processes or touches the filesystem is `#[cfg(not(target_arch = "wasm32"))]`.
- Dev bypass: `VOXEL_SKIP_MENU=1`, a set `VOXEL_WORLD_FILE`, `RENDER_BENCH` or `VOXEL_AUTOPILOT` → connect to `127.0.0.1:6466` at startup exactly as today.
- Verify with `just test-one <crate> "<target> <filter>"` and `just smoke`; never whole suites. Client test binaries are a slow opt-in — the tasks that need one say so; tag any test >100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Another session may have uncommitted edits in the tree (check `git status` before you start). Never revert, reformat or stage them. `git add -p` is NOT available (no interactive git here): stage only whole files that contain nothing but your changes (`git add <path>`), and if a file you must edit already has foreign uncommitted hunks, STOP and report BLOCKED instead of editing it.
- Every `crates/slint-ui` test is tagged slow, so slint-ui test commands need `-- --include-ignored` (e.g. `just test-one slint-ui "--test controls -- --include-ignored"`).
- The justfile always exports `VOXEL_WORLD_FILE` (empty by default): the dev bypass means *non-empty* `VOXEL_WORLD_FILE`.
- Work on `develop` in the main checkout — no worktrees (CLAUDE.md).

## Revision 2026-10-31

The plan was written at 99244f0; 59 commits later (Slint-only UI composited by `renderer/slint_composite.rs`, cell mode removed, 512 m play regions, adventure mode, trees/voxel bodies, tool wheel) it was re-audited. The audit with current file:line references is `/home/dawids/Projects/mmo/.superpowers/sdd/2026-10-30-main-menu-and-map-generator/staleness.md` — each task's **Revision** block below names what changed; read the matching "Task N" section of the audit for exact lines.

Rulings:
- **UI camera (Tasks 7, 11, 14).** Slint layers composite only onto an active `Camera3d` marked `UiCamera` (`slint_composite.rs`). Task 7 adds a `FrontendCamera` (Camera3d, dark `ClearColorConfig::Custom`, no skybox/fog, `RenderLayers` that match no world entity) which carries `UiCamera` and is the only active camera while the frontend owns the screen: every `AppState` except `InWorld`, and `InWorld` while not `Embarked` (Task 14). Otherwise the existing cameras and `bridge::enforce_single_active_camera` own `UiCamera` as today; that system must not fight the frontend camera.
- **Leave is handled in `WorldManager` (Task 5)**, as the same path as `ClientDisconnected`, so QUIC, WebTransport and the offline `LocalServer` all honour it. Session count = `manager.depths().sessions`.
- **World files carry a format version (Task 4)**: `loadable` reads the `VXWF` header and refuses a version other than `forged::ARTIFACT_VERSION`.
- **Order of execution:** Tasks 1–5 and 10 first (no overlap with in-flight work). Tasks 6–9, 11–15 touch `renderer/*` files another session had uncommitted (x-ray) on 2026-10-31; they run only once those files are clean.

## Review Focus

1. **Leaving while the world is still loading** (Esc → Main menu during `Starting`, or seconds after `InWorld` with meshing/LOD tasks in flight): expect a clean menu, no panic from a task completing into a despawned entity, no stale region in the next world. → Task 8 test `leave_during_join_then_rejoin`.
2. **Server dies under the player** (child crashes, port 6466 already taken by a dev server, connect times out): expect the main menu with the reason and the log tail, never a frozen world or a dead game thread. → Task 6 test `connect_failure_reports_disconnected_and_thread_survives`, Task 9 test `port_in_use_is_reported`.
3. **Moving sliders faster than the preview forges**: expect only the latest settings' preview to appear, no queue of stale previews, UI never blocks. → Task 12 test `preview_worker_is_latest_wins`.
4. **Bad world on disk** (half-written `.tmp` dir, `world.json` missing or unparsable, world made with another `REGION_BLOCKS`, two worlds named the same): list skips temp dirs, shows unreadable/incompatible worlds disabled with a reason, slugs never collide. → Task 4 tests.
5. **Second session after the first** (play → menu → load a different world): expect the second world to render alone, its own `[perf]` join timeline, and the far-voxel buffers empty at its start. → Task 8 test `second_session_starts_clean`.

---

## File structure

| File | Responsibility |
|---|---|
| `crates/worldgen/src/forge/mod.rs` | `forge_with` (progress + cancel); `forge` wraps it |
| `crates/worldgen/src/forge/presets.rs` (new) | `Preset` table + apply/match |
| `crates/worldgen/src/forge/render.rs` (new, moved from `examples/forge_proto/render.rs`) | preview layers → RGBA / PNG |
| `crates/worldgen/src/world_store.rs` (new) | `worlds/` dir: list, create (atomic), delete, slug, compatibility |
| `crates/game/src/protocol.rs`, `crates/game/src/lib.rs` | `ClientPacket::Leave`, `ClientUpdateEvent::Disconnected` |
| `crates/server/src/lib.rs` | Leave → `ClientDisconnected`; `VOXEL_SINGLE_PLAYER` exit |
| `crates/client/src/instance.rs`, `netcode.rs`, `main.rs` | disconnect-as-event; game thread survives; per-session channels |
| `crates/client/src/session.rs` (new) | `AppState`, `SessionScoped`, `SessionResetExt`, leave/start systems |
| `crates/client/src/local_host.rs` (new, native) | child server supervisor |
| `crates/client/src/frontend/` (new: `mod.rs`, `generator.rs`, `load_world.rs`) | menu-side Bevy glue for `FrontendUi` |
| `crates/slint-ui/ui/controls.slint` (new) | interactive `Slider`, `Dropdown`, `Tabs` over the `../ui` faces |
| `crates/slint-ui/ui/frontend.slint` + `main-menu.slint`, `map-generator.slint`, `load-world.slint`, `starting.slint` (new) | the `FrontendUi` window and its screens |
| `justfile`, `CLAUDE.md` | `play` recipe, `VOXEL_SKIP_MENU` on dev recipes, docs |

---

### Task 1: Forge progress and cancellation

- Revision: OK as written (`forge` now at `forge/mod.rs:119`). Set `knobs.iters` directly (its spec min is 50; do not go through a clamping path).


**Files:**
- Modify: `crates/worldgen/src/forge/mod.rs` (`forge` at ~117)
- Test: `crates/worldgen/src/forge/mod.rs` `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `pub struct ForgeProgress { pub stage: ForgeStage, pub iter: usize, pub iters: usize }`
  - `pub enum ForgeStage { Setup, Eroding, Shaping, Hydrology }` (`Copy`, with `fn label(self) -> &'static str` = "Preparing", "Eroding", "Shaping valleys", "Rivers and lakes")
  - `pub fn forge_with(p: &ForgeParams, on_progress: &mut dyn FnMut(ForgeProgress) -> std::ops::ControlFlow<()>) -> Option<Forged>` — `None` when the callback broke.
  - `pub fn forge(p: &ForgeParams) -> Forged` unchanged in behaviour (calls `forge_with` with a callback that always continues).

- [ ] **Step 1: Write the failing tests** — use a tiny world so they stay <100 ms: `ForgeParams { size_m: 4_096.0, cell_m: 128.0, ..Default::default() }` with `knobs.iters = 20.0`.
  - `forge_with_reports_every_erosion_iteration`: collect progress; assert the `Eroding` entries have `iter` = 1..=20 in order and `iters == 20`, and the last entry's stage is `Hydrology`.
  - `forge_with_cancels_between_iterations`: break when `iter == 3`; assert result `is_none()` and no progress entry after the break.
  - `forge_matches_forge_with`: `forge(&p).artifact(&p)` heights equal `forge_with(&p, continue).unwrap().artifact(&p)` heights.
- [ ] **Step 2:** `just test-one worldgen "--lib forge::tests::forge_with"` → FAIL (not defined).
- [ ] **Step 3:** Implement; the callback is checked once per stage boundary and once per erosion iteration, nowhere finer.
- [ ] **Step 4:** Same command → PASS; then `just test-one worldgen "--lib forge::"` → PASS.
- [ ] **Step 5:** Commit `feat(worldgen): forge progress callback and cancellation`.

### Task 2: Presets

- Revision: OK. `Knobs` has no `PartialEq` — `matching` compares through `get` over `SPECS`.


**Files:**
- Create: `crates/worldgen/src/forge/presets.rs`; register in `forge/mod.rs`
- Test: same file

**Interfaces:**
- Produces:
  - `pub struct Preset { pub name: &'static str, pub description: &'static str, pub overrides: &'static [(&'static str, f64)] }`
  - `pub const PRESETS: &[Preset]` — order and names exactly: `Default` ("Normal settings: one island, ranges inland, rivers to the sea."), `Archipelago` (`layout`=1), `Island arc` (`layout`=2), `Highlands`, `Lowlands`, `Dry`, `Wet`. Values for the last four are the builder's (spec "Left to the builder"), each within its knob's `[min, max]`, chosen by looking at the preview.
  - `pub const CUSTOM: &str = "Custom";`
  - `pub fn apply(preset: &Preset) -> Knobs` — `Knobs::default()` then each override via `Knobs::set`.
  - `pub fn matching(knobs: &Knobs) -> Option<&'static Preset>` — the first preset whose `apply` equals `knobs` on every `SPECS` key (exact f64 compare).

- [ ] **Step 1: Failing tests:** `every_override_names_a_knob_in_range` (each key: `Knobs::default().get(key).is_some()`, value within that `Spec`'s min..=max); `default_preset_has_no_overrides`; `apply_then_matching_round_trips` (for every preset `matching(&apply(p)) == Some(p)` by name); `edited_knobs_match_no_preset` (Default + `uplift` nudged → `None`).
- [ ] **Step 2:** `just test-one worldgen "--lib forge::presets"` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Same → PASS.
- [ ] **Step 5:** Commit `feat(worldgen): map generator presets`.

### Task 3: Preview rendering in the library

- Revision: the example's renderers are `relief_image`, `geology_image`, `rainfall_image`, `png_bytes`; `PreviewStats` must wrap the existing `forged.analysis.stats` (+ `analysis.rivers.len()`, peak from heights), not recompute. Also add `PreviewLayer::BeforeErosion` (the spec's 'before erosion' view = forge_proto's `initial`) — `ALL` becomes 5 entries.


**Files:**
- Create: `crates/worldgen/src/forge/render.rs` (moved logic from `crates/worldgen/examples/forge_proto/render.rs`)
- Modify: `crates/worldgen/examples/forge_proto/{main.rs,render.rs,serve.rs}` to call the library
- Test: `crates/worldgen/src/forge/render.rs`

**Interfaces:**
- Produces:
  - `pub enum PreviewLayer { Relief, Rivers, Geology, Rain }` with `pub const ALL: [PreviewLayer; 4]` and `fn label(self) -> &'static str` ("Relief", "Rivers", "Geology", "Rain").
  - `pub struct Rgba { pub width: u32, pub height: u32, pub pixels: Vec<u8> }` (RGBA8, row-major, top row = north/min z — the same orientation the example's PNGs use today).
  - `pub fn render(forged: &Forged, layer: PreviewLayer) -> Rgba`
  - `pub fn encode_png(img: &Rgba) -> Vec<u8>` (via `image`).
  - `pub struct PreviewStats { pub land_share: f64, pub islands: usize, pub largest_island_km2: f64, pub highest_peak_m: f64, pub lakes: usize, pub river_reaches: usize }` and `pub fn stats(forged: &Forged) -> PreviewStats` (same numbers `serve.rs` puts in its JSON).

- [ ] **Step 1: Failing tests:** `render_is_n_by_n_rgba` (tiny forge as in Task 1: every layer gives `width == height == world.n` and `pixels.len() == n*n*4`); `relief_sea_and_land_differ` (a sea pixel at a corner ≠ the spawn pixel); `png_round_trips` (decode `encode_png` with `image` → same dims).
- [ ] **Step 2:** `just test-one worldgen "--lib forge::render"` → FAIL.
- [ ] **Step 3:** Move the colour code; the example's PNG writers become thin `encode_png(&render(..))` + `fs::write`. Byte-identical example PNGs are not required.
- [ ] **Step 4:** Tests PASS; `just forge-proto --cell 64` still writes its PNGs + `.world`.
- [ ] **Step 5:** Commit `refactor(worldgen): preview rendering moves from forge_proto into the library`.

### Task 4: World store

- Revision: `forged::ARTIFACT_VERSION` is 2. `loadable` reads the 8-byte `VXWF`+version header of the terrain file (also the legacy `default.world`) and returns `Err("world file version N, this build reads 2 — re-forge it")` on mismatch; add test `old_artifact_version_is_not_loadable`. Record `artifact_version: u32` in `WorldMeta`. `REGION_BLOCKS`/`CHUNK_BLOCKS` are `usize` (store as u32); tests build at 64 m, so the message example reads "made with 128 m regions, this build uses 64 m".


**Files:**
- Create: `crates/worldgen/src/world_store.rs`; `pub mod world_store;` in `lib.rs`; add `serde_json = { workspace = true }` to `crates/worldgen/Cargo.toml`
- Test: same file (use a scratch dir under `target/` like `server::world_file` tests)

**Interfaces:**
- Consumes: `forged::{WorldArtifact, from_repo_root}`, `game::{REGION_BLOCKS, CHUNK_BLOCKS}`.
- Produces:
  - `pub const WORLDS_DIR: &str = "worlds";` `pub const TERRAIN_FILE: &str = "terrain.world";` `pub const META_FILE: &str = "world.json";` `pub const PREVIEW_FILE: &str = "preview.png";` `pub const LEGACY_WORLD: &str = "worlds/default.world";` `pub const META_FORMAT: u32 = 1;`
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct WorldMeta { pub format_version: u32, pub name: String, pub slug: String, pub created_utc: String /* RFC 3339 */, pub seed: u64, pub size_m: f64, pub cell_m: f64, pub preset: String, pub knobs: BTreeMap<String, f64>, pub forge_ms: u64, pub region_blocks: u32, pub chunk_blocks: u32 }`
  - `pub struct WorldEntry { pub dir: PathBuf, pub terrain: PathBuf, pub preview: Option<PathBuf>, pub meta: Option<WorldMeta>, pub legacy: bool, pub loadable: Result<(), String> }`
  - `pub fn list(root: &Path) -> Vec<WorldEntry>` — newest `created_utc` first, legacy last; skips dirs starting with `.`.
  - `pub fn slug_for(root: &Path, name: &str) -> String`
  - `pub fn create(root: &Path, meta: &WorldMeta, artifact: &WorldArtifact, preview_png: &[u8]) -> io::Result<PathBuf>` — writes into `root/.<slug>.tmp/` then renames to `root/<slug>/`; returns the final dir.
  - `pub fn delete(root: &Path, entry: &WorldEntry) -> io::Result<()>` — refuses legacy (`ErrorKind::PermissionDenied`).
  - `pub fn knobs_map(knobs: &Knobs) -> BTreeMap<String, f64>` (every `SPECS` key).

- [ ] **Step 1: Failing tests:**
  - `slug_lowercases_and_dedups`: `slug_for(root, "My Island!")` == `"my-island"`; after creating it, `slug_for(root, "my island")` == `"my-island-2"`; `slug_for(root, "")` == `"world"`.
  - `create_then_list_round_trips`: list returns one entry with `meta == Some(meta)`, `loadable == Ok(())`, `terrain` exists, `preview` is `Some`.
  - `list_skips_temp_dirs`: a leftover `root/.x.tmp/` is not listed.
  - `unreadable_meta_is_listed_not_loadable`: dir with `terrain.world` + garbage `world.json` → entry with `meta: None`, `loadable` is `Err` containing "world.json".
  - `other_region_size_is_not_loadable`: meta with `region_blocks = REGION_BLOCKS * 2` → `Err` whose text names both sizes in metres (e.g. "made with 512 m regions, this build uses 256 m").
  - `legacy_default_world_listed_last_and_undeletable`: a `root/default.world` file → last entry, `legacy`, `loadable == Ok`, `delete` → `PermissionDenied`.
  - `list_orders_newest_first`.
- [ ] **Step 2:** `just test-one worldgen "--lib world_store"` → FAIL.
- [ ] **Step 3:** Implement. Use a tiny `WorldArtifact` from a 128 m-cell forge built once per test file (`OnceLock`) or a hand-built minimal artifact if `WorldArtifact` allows it.
- [ ] **Step 4:** Same → PASS.
- [ ] **Step 5:** Commit `feat(worldgen): on-disk world store for the main menu`.

### Task 5: Server — Leave packet and single-player exit

- Revision (overrides the dispatch details above): handle `ClientPacket::Leave` in `WorldManager::handle_server_event`'s exhaustive `ClientPacket` match (`crates/game/src/world_manager.rs`), running the same code as `ServerEvent::ClientDisconnected(id)`; do NOT add a server-only `ingress_event`. Replace the test `leave_packet_disconnects_client` with a game-crate test at the `WorldManager` level (InlineSpawner, as the existing world_manager tests do): `leave_packet_removes_the_session` — after `Leave`, `depths().sessions` drops by one and a later `ClientDisconnected` for the same id is a no-op. The single-player exit uses `manager.depths().sessions` in the server's manager loop. `local_server.rs` needs no change if the manager handles it.


**Files:**
- Modify: `crates/game/src/protocol.rs` (`ClientPacket`, add `Leave` as the LAST variant); `crates/server/src/lib.rs` (packet dispatch near the `ServerEvent::ClientDisconnected` send at ~248, and the manager loop); `crates/client/src/local_server.rs` (treat `Leave` like disconnect)
- Test: `crates/server/src/lib.rs` `#[cfg(test)]`

**Interfaces:**
- Produces:
  - `ClientPacket::Leave` — "the client is leaving this world; treat as disconnected now".
  - Env `VOXEL_SINGLE_PLAYER=1` (const `SINGLE_PLAYER_ENV` in `server/src/lib.rs`): once at least one client has connected and the session count returns to 0, the manager logs `[server] single-player: last client left, exiting` and `run()` returns (process exits 0).
  - `fn should_exit_single_player(enabled: bool, ever_connected: bool, sessions: usize) -> bool` (pure, in `lib.rs`).

- [ ] **Step 1: Failing tests:** `single_player_exits_only_after_last_client_leaves` (table over the pure fn: (false,true,0)→false, (true,false,0)→false, (true,true,1)→false, (true,true,0)→true); `leave_packet_disconnects_client` — at whichever function turns a decoded `ClientPacket` into a `ServerEvent` (extract one named `fn ingress_event(id: ClientId, packet: ClientPacket) -> Option<ServerEvent>` if none exists), assert `Leave` → `ServerEvent::ClientDisconnected(id)`.
- [ ] **Step 2:** `just test-one server "--lib single_player leave_packet"` → FAIL.
- [ ] **Step 3:** Implement. A `Leave` must also stop that connection's reader task so the later QUIC close does not send a second `ClientDisconnected` (or make the manager's handler idempotent — `sessions.remove` already returns `None` on the second call; confirm and keep it so).
- [ ] **Step 4:** PASS; `just smoke` green.
- [ ] **Step 5:** Commit `feat(server): Leave packet and single-player exit`.

### Task 6: Client disconnects become events

- Revision: lines moved (`connect_and_run` ≈ instance.rs:1448, `server_msg.unwrap()` ≈1191, netcode thread unwraps ≈1471-1472, plus an `Err(e) => panic!` ≈1495 to turn into `Err`; bridge `GameCrash` arm ≈686). `ConnectPolicy` already exists (`netcode.rs`, fields `attempt_timeout`, `retry_interval`, `deadline`); express the test's short retry as e.g. `attempt_timeout 50 ms, retry_interval 10 ms, deadline 150 ms`. `connect_gives_up_with_timed_out_instead_of_panicking` in netcode.rs is the template.


**Files:**
- Modify: `crates/game/src/lib.rs` (`ClientUpdateEvent`), `crates/client/src/instance.rs` (`connect_and_run` ~1498-1563, `handle_server` `server_msg.unwrap()` ~1217, netcode thread `.unwrap()` ~1522, ~1545), `crates/client/src/netcode.rs` (unwraps ~143-195), `crates/client/src/main.rs` (`start_game_thread` 105-146), `crates/client/src/renderer/bridge.rs` (`drain_client_updates` ~553, `GameCrash` arm ~647)
- Test: `crates/client/src/instance.rs` tests (slow opt-in client lib test — say so to the user before running)

**Interfaces:**
- Produces:
  - `ClientUpdateEvent::Disconnected { reason: String }` — sent by the game thread exactly once per session when the session ends for any reason other than a requested Quit (connect gave up, connection lost, server closed).
  - `connect_and_run` returns `Err(GameError)` on those; the `start_game_thread` loop sends `Disconnected` and waits for the next `Command`.
  - Bevy: `bridge.rs` turns `Disconnected` (and `GameCrash`) into the Bevy message `SessionEnded { reason: Some(reason) }` (type defined in Task 7's `session.rs`; until Task 7 lands, define it in `bridge.rs` with that exact shape and move it).
  - `Command::ConnectToServerAndScene` gains a trailing `ConnectPolicy` field (`netcode::ConnectPolicy`, `Default` in production) so tests can shorten the retry.

- [ ] **Step 1: Failing test** `connect_failure_reports_disconnected_and_thread_survives`: start the game thread and send a connect whose `ConnectPolicy` allows 2 × 50 ms attempts to an unused port; assert `client_recv` yields `Disconnected { reason }` with `reason` containing "could not connect" within 2 s; then send a second `ConnectToServerAndScene` (also to a dead port) and assert a second `Disconnected` arrives (the thread survived). Tag slow.
- [ ] **Step 2:** `just test-one client "--lib instance::tests::connect_failure -- --include-ignored"` → FAIL (panic / no event). Ask the user before the first client test build.
- [ ] **Step 3:** Implement: replace the unwraps with error propagation; the netcode thread's `JoinHandle` result and a closed `server_recv` both become `Err`.
- [ ] **Step 4:** Same → PASS; `just smoke` green.
- [ ] **Step 5:** Commit `fix(client): a lost server ends the session instead of panicking the game thread`.

### Task 7: Session mechanism — AppState, SessionScoped, reset registry

- Revision: verify `StatesPlugin` is in `DefaultPlugins` (Bevy feature `bevy_state`); add the feature to the workspace bevy features if not. Implement the **UI camera ruling** above here: `#[derive(Component)] pub struct FrontendCamera;` spawned once at startup, plus the system that makes it the active `UiCamera` camera whenever `frontend_owns_screen()` (`!in_state(InWorld) || !Embarked` — define `Embarked` here as a session-reset resource, Task 14 sets it). Test: `frontend_camera_is_ui_camera_outside_world`.


**Files:**
- Create: `crates/client/src/session.rs` (`pub mod session;` in `lib.rs`)
- Modify: `crates/client/src/main.rs` (`init_state`), `crates/client/src/renderer/mod.rs` (`SimBridgePlugin` registers itself under `InWorld`)
- Test: `crates/client/src/session.rs` (headless `MinimalPlugins` + `StatesPlugin`; slow opt-in client lib test)

**Interfaces:**
- Produces:
  - `#[derive(States, Default, Clone, Copy, PartialEq, Eq, Hash, Debug)] pub enum AppState { #[default] MainMenu, MapGenerator, LoadWorld, Starting, InWorld }`
  - `#[derive(Component)] pub struct SessionScoped;`
  - `pub trait SessionResetExt { fn add_session_reset<R: Resource + FromWorld>(&mut self) -> &mut Self; fn add_session_reset_with(&mut self, f: fn(&mut World)) -> &mut Self; }` on `App`.
  - `#[derive(Message)] pub struct SessionEnded { pub reason: Option<String> }` (`None` = the player chose to leave).
  - `pub fn end_session(world: &mut World)` — exclusive system/fn: despawns every `SessionScoped` entity (recursively), runs every registered reset in registration order, clears `Messages<T>` for every type registered with `add_session_message_clear::<T>()` (also on `SessionResetExt`).
  - `pub fn world_systems_enabled() -> impl Condition` = `in_state(AppState::InWorld).or(in_state(AppState::Starting))` — the one run condition every per-world plugin uses.
  - `#[derive(Resource)] pub struct SkipMenu(pub bool)` — computed once in `main` from the dev-bypass env (Global Constraints); when true the initial state is `Starting` with the fixed `127.0.0.1:6466` address and no child.

- [ ] **Step 1: Failing tests:** `end_session_despawns_scoped_trees` (a `SessionScoped` parent with children + an unscoped entity → after `end_session` only the unscoped one remains); `end_session_resets_registered_resources` (a test resource mutated → back to `Default`; an unregistered one untouched); `end_session_clears_registered_messages`; `world_systems_do_not_run_in_menu` (a counter system under `world_systems_enabled()` runs 0 times in `MainMenu`, ≥1 in `InWorld`).
- [ ] **Step 2:** `just test-one client "--lib session:: -- --include-ignored"` → FAIL.
- [ ] **Step 3:** Implement; registrations are stored in a `SessionResets(Vec<fn(&mut World)>)` resource.
- [ ] **Step 4:** PASS; `just smoke` green (type-checks the client).
- [ ] **Step 5:** Commit `feat(client): session state, SessionScoped and the reset registry`.

### Task 8: Make every per-world plugin session-safe

- Revision: the inventory above is stale — rebuild it from the audit's "Task 8" section (ui_bridge and `HomeCellMask` are gone; one lod_rings Local; `WorldReady` already a resource; minimap is `MinimapPixels`; NEW: adventure `ClientPlayMode`/`Follow`/`RightPress`/`PathPreview`/`SentPath`/`ToggleModeRequest`, `world_map::PendingPlay`, `DebugColliderLines`, `SliceState`, `OverlayUpload`/`FortressViewUniform`, `CullingStats`, ~15 Locals, voxel-body / rig / body-root entities, and the per-world properties held inside the `GameUi` and `HudUi` Slint components, which `end_session` must reset). Preferences (`XraySettings`, debug toggles) are not reset.


**Files (modify; the inventory in the spec's "Session lifecycle" is the checklist):**
- `renderer/bridge.rs` — tag region roots / retired roots / `SnapshotVoxelTask`s `SessionScoped`; register `Regions`, `RegionRoots`, `RetiredRegionRoots`, `SimEntityMap`, `SnapshotVoxelQueue`, `LocalPlayer`, `HudStatus`.
- `renderer/ui_bridge.rs` — ui-root `SessionScoped`; `UiWidgetMap`, `UiRoot`, `UiUpdates`.
- `renderer/lod_rings.rs` — LOD roots + retired `SessionScoped`; `LodRings`, `HomeCellMask`; `Local`s at ~162 and ~464 → resources, registered.
- `renderer/meshing/*`, `renderer/bricks.rs` — `ChunkGrid`, `RemeshQueue`, `LodBuildQueue`, `FrameUploads`, `FarFrameUploads`, `PullMemStats`, `SkippedBricks` (now inserted in `build`, not lazily), `visibility.rs` `GridCache` `Local` → resource.
- `renderer/far_voxel/*` — `add_session_reset_with(reset_far_voxel)`: re-create `FarVoxelResidency::new(limits)`, `FarBrickRevisions`, `FarDeferredRemovals`, and send a `FarReset` through `FarUploadChannel` that makes the render world clear `GpuFarBuffers` (no per-entity drain).
- `renderer/commander/*` — `CommanderCamera`, `HeightGrid`, `TileMirror`, `Selection`, `GroundPick`, `WorkingLevel`, `DesignateState`, `DragReadout`, `MinimapPointer`; minimap `built` and designate `GhostCache` `Local`s → resources; minimap images cleared.
- `renderer/world_map.rs` (`WorldMapState`; clear `WorldMapMsg`), `tiles` (`TileMirrorMsg` clear), `slint_overlay.rs` (`PlayPending`), `fortress_view.rs` (`OverlayGrid`, `DragPreview`, `Local<OverlayBuilt>` → resource), `boundaries.rs` (`BoundaryUniform` home rect), `join_timeline.rs` (`WorldReady` + `JoinTimelineState` → resources), `motion_probe.rs` `Local`s.
- every plugin above: its `Update`/`PostUpdate` systems gated by `world_systems_enabled()`.
- Test: `crates/client/tests/session_reset.rs` (new; slow opt-in)

**Interfaces:**
- Consumes: Task 7's `SessionScoped`, `SessionResetExt`, `end_session`, `world_systems_enabled`.
- Produces: the invariant "after `end_session`, the client's world state equals a fresh app's".

- [ ] **Step 1: Failing tests** (headless harness like the convergence tests — offline `LocalServer`, `set_lod_table(&[])` + `set_near_radius(0)` per the headless-world-size rule):
  - `second_session_starts_clean`: join, reach world-ready, `end_session`, then assert: zero `SessionScoped` entities; `ChunkGrid`, `LodRings`, `SimEntityMap`, `TileMirror`, `WorldMapState` empty; `FarVoxelResidency` has 0 occupied slots; `WorldReady` false. Join again → world-ready again and a second `[perf] event=world_ready` line is emitted.
  - `leave_during_join_then_rejoin`: `end_session` one update after `NewRegion` arrives (meshing tasks in flight), run 30 updates (no panic), rejoin → world-ready, and no entity from the first session's region appears.
- [ ] **Step 2:** `just test-one client "--test session_reset -- --include-ignored"` → FAIL.
- [ ] **Step 3:** Work through the file list. Any new `Resource` you find that holds per-world data and is not in the list: register it too and add it to the spec's inventory.
- [ ] **Step 4:** PASS; `just smoke` green.
- [ ] **Step 5:** Commit per plugin group if large (`refactor(client): <module> state is session-scoped`), final `test(client): session reset leaves no world state behind`.

### Task 9: Local server supervisor and the session start/leave flow

- Revision: also gate/swap the `DevUiPlugin`'s use of `GameEvents`; the child server also binds 6467 (WebTransport) — the 6466 pre-check is enough. `ClientRequest` currently has only `PlayRegion` (instance.rs).


**Files:**
- Create: `crates/client/src/local_host.rs` (native only)
- Modify: `crates/client/src/main.rs` (channels per session instead of once; no connect at startup unless `SkipMenu`), `crates/client/src/session.rs` (`OnEnter(Starting)` / leave systems), `crates/slint-ui/ui/menu.slint` (+ "Main menu" button and `callback main-menu()`), `crates/client/src/renderer/slint_overlay.rs` (wire it; your hunks only)
- Test: `crates/client/src/local_host.rs` (slow opt-in)

**Interfaces:**
- Produces:
  - `pub struct LocalHost { child: std::process::Child, log: PathBuf, pub addr: SocketAddr }` (`Resource` when present)
  - `pub fn server_binary() -> PathBuf` — `VOXEL_SERVER_BIN` if set, else `current_exe().parent()/server` (`server.exe` on Windows).
  - `impl LocalHost { pub fn start(terrain: &Path, log: &Path) -> io::Result<LocalHost>; pub fn try_exited(&mut self) -> Option<ExitStatus>; pub fn stop(self, grace: Duration) -> Option<ExitStatus>; pub fn log_tail(&self, lines: usize) -> String }` — `start` first binds a UDP socket on `127.0.0.1:6466` (QUIC is UDP) and drops it; if that bind fails it returns `ErrorKind::AddrInUse` with the message "port 6466 is already in use (is a dev server running?)" without spawning anything. It then sets `VOXEL_WORLD_FILE=<terrain>` and `VOXEL_SINGLE_PLAYER=1`; stdout+stderr → `log`.
  - `pub const STOP_GRACE: Duration = Duration::from_secs(5);`
  - `#[derive(Resource)] pub struct PendingWorld { pub entry_dir: PathBuf, pub terrain: PathBuf, pub name: String }` — set by the menu screens before `AppState::Starting`.
  - `#[derive(Resource, Default)] pub struct LastSessionError(pub Option<String>)` — shown by the main menu.
  - `#[derive(Resource)] pub struct GameThread(Sender<Command>)`; `Command::ConnectToServerAndScene` as extended in Task 6.
  - Flow: `OnEnter(Starting)` → (unless `SkipMenu`) `LocalHost::start` → new channels → swap `ClientUpdates`/`GameEvents`/`ClientRequests` → send connect. First `SetPlayer` → `InWorld`. Leave (menu button or `SessionEnded`): send `ClientPacket::Leave` via a new `ClientRequest::Leave`, then `GameEventKind::Quit`; swap channels to fresh dead ones; `end_session`; `LocalHost::stop(STOP_GRACE)` (on a background thread so the frame doesn't wait); `AppState::MainMenu`. App exit (window close / Exit button) runs the same leave first.
- [ ] **Step 1: Failing tests** (fake server = `VOXEL_SERVER_BIN` pointing at a small shell script written to a scratch dir): `start_passes_world_and_single_player_env` (script dumps its env to the log; assert both vars); `stop_kills_after_grace` (script ignores SIGTERM / `sleep 60`; `stop(100ms)` returns within 1 s and the process is gone); `port_in_use_is_reported` (bind 6466 in the test, `start` → `AddrInUse` with the message above). Native only.
- [ ] **Step 2:** `just test-one client "--lib local_host:: -- --include-ignored"` → FAIL.
- [ ] **Step 3:** Implement `LocalHost`, then the state systems.
- [ ] **Step 4:** PASS; `just smoke` green. Manual check (Task 15 recipe): `VOXEL_SKIP_MENU=1 just run` behaves exactly as before.
- [ ] **Step 5:** Commit `feat(client): local server per world and in-process leave to the main menu`.

### Task 10: Interactive Slint controls

- Revision: an interactive slider and list exist privately in `crates/slint-ui/ui/asset-viewer.slint` (`Scrubber`, `PickList`). Promote them into `controls.slint` as `Slider` / `PickList` (keeping the asset viewer working and its test `asset_viewer` green) instead of writing new ones; `Dropdown` and `Tabs` are new. `../ui` has `TextField` and `ProgressbarFace` for later tasks; do not modify `../ui` (it has its own uncommitted work).


**Files:**
- Create: `crates/slint-ui/ui/controls.slint`
- Test: `crates/slint-ui/tests/controls.rs` (headless `Overlay`, like `tests/world_map.rs`); a test-only component exported from `index.slint` if needed (`ControlsHarness`)

**Interfaces:**
- Produces (Slint):
  - `export component Slider { in property <float> minimum; in property <float> maximum; in property <float> step: 0; in-out property <float> value; in property <bool> disabled; callback edited(float); }` — press on the track jumps the thumb there, drag follows, wheel steps ±`(maximum-minimum)/100` (or `step`); `edited` fires on every change by the user, never on a property set from Rust. Draws `SliderFace`.
  - `export component Dropdown { in property <[string]> items; in-out property <int> selected; callback chosen(int); }` — click opens a list popup under it (`PopupWindow` over `ListBoxFace`), click an item → `selected` + `chosen`, Esc / click outside closes. Draws `DropdownFace`.
  - `export component Tabs { in property <[string]> tabs; in-out property <int> selected: 0; callback changed(int); }` — click a tab; draws `TabbedPaneFace` and hosts `@children` as the page.
- [ ] **Step 1: Failing tests:** `slider_click_jumps_and_drag_follows` (click at 75 % of the track → value ≈ min + 0.75·range ± one pixel's worth; drag to the left end → minimum; `edited` count ≥ 2); `slider_set_from_rust_does_not_fire_edited`; `dropdown_opens_and_selects` (click → popup; click item 2 → `selected == 2`, `chosen(2)`, popup closed); `tabs_click_selects`.
- [ ] **Step 2:** `just test-one slint-ui "--test controls"` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(slint-ui): interactive slider, dropdown and tabs`.

### Task 11: FrontendUi layer and the main menu

- Revision: a Slint layer has no enable switch (always drawn and fed input; pointer via `SlintPointer`). `FrontendUi` gets its own order constant `FRONTEND_LAYER_ORDER = 1500` next to `HUD/GAME/DEV_LAYER_ORDER` and a `shown: bool` property (draws nothing when false). Outside `InWorld`, `GameUi`, `HudUi` and `DevPanelUi` must draw nothing and take no input. Background = the `FrontendCamera` clear colour (Task 7).


**Files:**
- Create: `crates/slint-ui/ui/frontend.slint` (`export component FrontendUi inherits Window` with `in property <int> screen` = 0 menu, 1 generator, 2 load, 3 starting), `main-menu.slint`; export `FrontendUi` from `index.slint`
- Create: `crates/client/src/frontend/mod.rs` (`FrontendPlugin`: a `SlintLayerPlugin<FrontendUi>`, visible when `AppState != InWorld`; plain dark clear colour behind it)
- Test: `crates/slint-ui/tests/frontend.rs`

**Interfaces:**
- Produces (Slint `FrontendUi`): `in property <string> title: "Orion"`; `in property <bool> can-load`; `in property <string> menu-error`; callbacks `new-world()`, `load-world()`, `exit()`.
- Bevy: `new-world` → `AppState::MapGenerator`; `load-world` → `AppState::LoadWorld`; `exit` → `AppExit::Success` (after the leave path, a no-op in the menu). `can-load` = `!world_store::list(root).is_empty()`, refreshed on `OnEnter(MainMenu)`. `menu-error` = `LastSessionError`.
- [ ] **Step 1: Failing tests:** `main_menu_buttons_fire` (click each → its callback); `load_disabled_without_worlds` (`can-load: false` → click does not fire); `menu_error_is_shown` (property set → text visible via the overlay's accessible tree or a pixel probe, whichever `world_map.rs` already uses).
- [ ] **Step 2:** `just test-one slint-ui "--test frontend"` → FAIL.
- [ ] **Step 3:** Implement Slint + plugin.
- [ ] **Step 4:** PASS; `just smoke`.
- [ ] **Step 5:** Commit `feat: main menu`.

### Task 12: Map generator screen

- Revision: the knob groups are Island, Uplift, Erosion, Geology, Climate, Shaping, Rivers; every Shaping key is in `TAB_KEY_OVERRIDES` — add an assertion that no knob falls through to Advanced except the Erosion group. Use `../ui` `TextField` for seed/name and `ProgressbarFace` for the forge bar; the `Slider`/`Dropdown`/`Tabs` from Task 10.


**Files:**
- Create: `crates/slint-ui/ui/map-generator.slint`; `crates/client/src/frontend/generator.rs`
- Test: `crates/slint-ui/tests/frontend.rs` (layout/hit tests); `crates/client/src/frontend/generator.rs` (worker, slow opt-in)

**Interfaces:**
- Consumes: Tasks 1–4, 10, 11.
- Produces:
  - Slint `MapGenerator` (inside `FrontendUi`, screen 1): properties `presets: [string]`, `preset: int`, `preset-description: string`, `seed-text: string`, `name-text: string`, `tabs: [string]`, `tab: int`, `rows: [KnobRow]` (`struct KnobRow { key: string, label: string, help: string, min: float, max: float, value: float, default: float, is-choice: bool, choices: [string] }` — rows of the current tab only), `preview: image`, `preview-busy: bool`, `stats-lines: [string]`, `layers: [string]`, `layer: int`, `auto-refresh: bool`, `forging: bool`, `forge-progress: float`, `forge-label: string`; callbacks `preset-chosen(int)`, `seed-edited(string)`, `shuffle-seed()`, `knob-edited(string, float)`, `knob-reset(string)`, `tab-changed(int)`, `layer-chosen(int)`, `refresh()`, `back()`, `play()`, `cancel-forge()`. Layout per the spec's Screens section and the Factorio screenshot.
  - Rust: `pub const TABS: [(&str, &[&str]); 5]` mapping tab name → knob GROUPS: `("Land", &["Island"])`, `("Mountains", &["Uplift"])`, `("Water", &["Rivers", "Climate"])`, `("Geology", &["Geology"])`, `("Advanced", &["Erosion"])`, plus `const TAB_KEY_OVERRIDES: &[(&str, &str)]` moving `max_peak_m`, `floodplain_m_per_sqrt_km2`, `floodplain_max_m` → Mountains and `lake_basins`, `lake_spacing_m`, `lake_radius_max_m`, `lake_depth_max_m` → Water. `pub fn tab_of(spec: &Spec) -> usize` — override, else group, else Advanced (4).
  - Advanced also gets two non-knob rows: preview cell (choices 64 m / 32 m, default 64) and world cell (choices 16 m / 8 m "slow", default 16).
  - `pub struct PreviewWorker` — one background thread; `request(params: ForgeParams, layer: PreviewLayer, generation: u64)`; results via a channel `(generation, Rgba, PreviewStats)`; a newer `request` cancels the running forge through `forge_with`'s callback (an `AtomicU64` generation compare). Debounce: requests are coalesced; the worker waits `PREVIEW_DEBOUNCE = 250 ms` after the last one before forging.
  - Play: full forge on its own thread with `forge_with` → progress to the UI (`forge-label` = `ForgeStage::label`, `"Eroding 312 / 500"` during erosion) → `world_store::create(..)` with `preset` = `presets::matching(..).map_or(CUSTOM, |p| p.name)`, `forge_ms`, `REGION_BLOCKS`, `CHUNK_BLOCKS`, preview = Relief render → `PendingWorld` → `AppState::Starting`. Cancel stops it and leaves no directory.
  - Seed: shown as `{:#018X}`, parsed like forge_proto `--seed` (hex, `0x` and `_` allowed); invalid text keeps the old seed and tints the box red. Shuffle = a random u64 from the system clock hash. Name default: `"Island {last 4 hex digits of seed}"`.
- [ ] **Step 1: Failing tests:** `every_knob_lands_in_exactly_one_tab` (all `SPECS` → `tab_of` < 5; with the overrides as above); `preview_worker_is_latest_wins` (three requests within 10 ms with distinct generations → exactly one result, generation 3); `play_cancel_leaves_no_world_dir`; Slint: `knob_row_drag_fires_knob_edited`, `preset_dropdown_fires_preset_chosen`, `play_and_back_fire`.
- [ ] **Step 2:** `just test-one slint-ui "--test frontend"` and `just test-one client "--lib frontend::generator -- --include-ignored"` → FAIL.
- [ ] **Step 3:** Implement. Editing a knob sets the preset dropdown to "Custom" (`matching` → `None`); choosing a preset re-applies it.
- [ ] **Step 4:** PASS; `just smoke`.
- [ ] **Step 5:** Commit `feat: map generator screen`.

### Task 13: Load world screen

- Revision: show Task 4's version-mismatch reason. Base the list on Task 10's `PickList`; add double-click.


**Files:**
- Create: `crates/slint-ui/ui/load-world.slint`; `crates/client/src/frontend/load_world.rs`
- Test: `crates/slint-ui/tests/frontend.rs`

**Interfaces:**
- Consumes: `world_store::{list, delete}`, `PendingWorld`.
- Produces: Slint `LoadWorld` (screen 2): `worlds: [WorldRow]` (`struct WorldRow { name: string, detail: string /* "seed 0x…, 16 km, 2026-10-30" */, thumb: image, loadable: bool, reason: string }`), `selected: int`, `big-preview: image`, `settings-lines: [string]` (preset + knobs that differ from that preset), `confirm-delete: bool`; callbacks `select(int)`, `load()`, `delete()`, `confirm-delete-yes()`, `confirm-delete-no()`, `back()`. Double-click a row = `load`. Load/Delete disabled for unloadable rows (reason shown); Delete disabled for legacy.
- [ ] **Step 1: Failing tests:** `unloadable_row_disables_load`; `double_click_loads`; `delete_asks_first` (delete → `confirm-delete` true, nothing deleted until yes).
- [ ] **Step 2:** `just test-one slint-ui "--test frontend"` → FAIL.
- [ ] **Step 3:** Implement; the list is re-read on `OnEnter(LoadWorld)` and after a delete.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat: load world screen`.

### Task 14: Starting screen and the modal world map on arrival

- Revision: do NOT deactivate the commander camera (it is the `UiCamera`; that would hide the map too) — use the Task 7 `FrontendCamera` ruling while not embarked. Esc/M are Bevy-side (`slint_overlay.rs` `menu_keys` / `Screens::escape()` / `toggle_map()`): gate the modal map there and in `UiAction::CloseMap`; the Slint test covers only the Close button. Two pending-Play resources exist (`slint_overlay::PlayPending`, `world_map::PendingPlay`): `EmbarkHere` sets neither, but must still let `commander::track_home_region` place the camera over the body. While not embarked, the adventure-mode and follow buttons are hidden.


**Files:**
- Create: `crates/slint-ui/ui/starting.slint`
- Modify: `crates/client/src/frontend/mod.rs`, `crates/client/src/renderer/world_map.rs`, `crates/client/src/renderer/slint_overlay.rs` (your hunks), `crates/slint-ui/ui/game.slint` / `world-map.slint` (add `in property <bool> map-modal`; when true the Close button is hidden and `close-map` is not raised by M/Esc)
- Test: `crates/slint-ui/tests/world_map.rs` (add to the existing file; leave foreign hunks alone)

**Interfaces:**
- Produces:
  - Starting (screen 3): `starting-lines: [string]` ("Starting server…" → "Connecting…" → "Receiving world map…"), `starting-error: string`, `starting-log: string`, callback `back()`. Error comes from `SessionEnded { reason: Some(..) }` while in `Starting` plus `LocalHost::log_tail(20)`; Back → leave path → `MainMenu`.
  - `#[derive(Resource, Default)] pub struct Embarked(pub bool)` (session-reset): false until the first Play. Becomes true when the existing `PlayPending` resolves successfully (the "Loading region…" clear in `slint_overlay.rs`), OR — because the player already stands in `SPAWN_REGION` before embarking and `tile_map::play_blocker` refuses the home tile ("You are here") — immediately, with no packet, when the player presses Play on the current home tile while not embarked. While not embarked the client calls `play_blocker(map, rc, None)` so the home tile is playable. While false: the map opens automatically on the first `WorldMap`, `map-modal` = true, Esc opens the menu with only **Main menu** visible, and the 3D view is hidden (`Camera::is_active = false` on the commander camera) so the spawn region loading behind the map is never shown. After a menu start, `Embarked` starts false; with `SkipMenu` it starts true (today's behaviour).
- [ ] **Step 1: Failing tests:** `modal_map_cannot_be_closed` (`map-modal: true` → Esc / M / Close produce no `close-map`); `non_modal_map_closes` (regression: today's behaviour). Plus a client unit test `play_on_home_tile_embarks_without_packet` for the pure decision fn `fn play_action(embarked: bool, home: Option<RegionCoords>, rc: RegionCoords) -> PlayAction` (`enum PlayAction { EmbarkHere, SendPlay, Refuse }`): (false, Some(a), a) → `EmbarkHere`; (true, Some(a), a) → `Refuse`; (false, Some(a), b) → `SendPlay`.
- [ ] **Step 2:** `just test-one slint-ui "--test world_map modal"` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS; `just smoke`.
- [ ] **Step 5:** Commit `feat: starting screen and world map as the embark step`.

### Task 15: Recipes, docs, end-to-end check

- Revision: `play_blocks` defaults to 512. Also export `VOXEL_SKIP_MENU=1` in `scripts/run.sh`. `CLAUDE.md` may carry another session's uncommitted hunks — if so, report BLOCKED for that file rather than staging it.


**Files:**
- Modify: `justfile` (your hunks only), `CLAUDE.md`
- [ ] **Step 0:** flip the menu default (Task 7 left it opt-in behind `VOXEL_MENU=1`, see `TODO(Task 15)` in `session.rs`): the menu becomes the default, `VOXEL_SKIP_MENU=1` skips it; `just play` must not need `VOXEL_MENU`.
- [ ] **Step 1:** `justfile`: add `play` (builds `client` + `server` with `{{play_blocks}}`, runs only `./target/debug/client`; `play-release` likewise); export `VOXEL_SKIP_MENU=1` in `run`, `run-release`, `client`, `client-release`, `diag`, `perf-capture`, `fly-probe`, `shuttle-probe`, `play-test`, and the `trace-*`/`perf*`/`profile-*`/`tracy-client` recipes that launch a client against a separately started server.
- [ ] **Step 2:** `CLAUDE.md`: "Building and Running" — `just play` = main menu; `VOXEL_SKIP_MENU`, `VOXEL_SERVER_BIN`, `VOXEL_SINGLE_PLAYER`; `worlds/<slug>/` layout; one paragraph under `crates/client` on `AppState` / `SessionScoped` / `add_session_reset` ("any new per-world resource must be registered").
- [ ] **Step 3:** `just smoke` → green. `just test-one worldgen "--lib forge:: world_store"` → green.
- [ ] **Step 4:** Manual end-to-end on `adapter=Intel` (ask the user first — it is a live run): `just adapter=Intel play` → New world → move a slider (preview updates) → Play (progress bar) → world map modal → Play a tile → world shows → Esc → Main menu → Load world → load the same world → Play another tile → Exit. Then `just adapter=Intel run` still boots straight into the world. Record the result in the commit message.
- [ ] **Step 5:** Commit `docs: main menu recipes and session rules`.

---

## Self-review notes

- Spec coverage: goals 1–6 → Tasks 11, 12, 13, 9+14, 9 (Exit), 9 (Esc → Main menu); non-goals untouched; anti-goals: UI thread never forges (12), knobs from `SPECS` (12), dev bypass (7, 9, 15), Slint only. Session lifecycle → 5–9. Build-settings compatibility → 4, 13. Modal map → 14.
- Names used across tasks: `forge_with`/`ForgeProgress`/`ForgeStage` (1, 12), `PRESETS`/`matching`/`CUSTOM` (2, 12, 13), `render`/`PreviewLayer`/`Rgba`/`PreviewStats` (3, 12, 13), `world_store::*` (4, 11–13), `ClientPacket::Leave`/`VOXEL_SINGLE_PLAYER` (5, 9), `Disconnected`/`SessionEnded` (6, 7, 9, 14), `AppState`/`SessionScoped`/`SessionResetExt`/`end_session`/`world_systems_enabled`/`SkipMenu` (7–9, 11, 14), `LocalHost`/`PendingWorld`/`LastSessionError` (9, 11–14), `Embarked` (14).
