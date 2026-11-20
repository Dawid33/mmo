# AGENTS.md

Always-on repository rules and architecture map for **Errantry: Renown and Ruin** (kept under 24 KB so the full file loads cleanly). For deep subsystem protocols, runbooks, and env vars, read `CLAUDE.md` and the dated docs under `docs/superpowers/{specs,plans}/` on demand (see [Deep References](#deep-references-read-on-demand)).

## Hard Rules

- **No git worktrees.** Work directly on `develop` in this checkout — including tests, bisection, and subagent work (shared build caches and the user's concurrent edits).
- **Regression-only tests (owner's rule, 2026-11-19 — whole repo including `quest/`).** Commit ONLY:
  1. **Regression tests** reproducing a bug actually seen in play or in a log (runtime log, probe, hand check, or fuzzer crash), tagged directly above the test with `/// Regression: <the bug> (<where it was seen, the fix>)` (`//` in JS/Node, `#` in Python).
  2. **Gates** tagged `/// Gate: <what it guards>` (determinism/quality goldens and `physics-next` fork pins; adding a new gate requires owner approval).
  Temporary tests written while iterating are fine and MUST be deleted before committing. Never commit tests pre-emptively, "for coverage", or alongside a new feature. Enforced by `just test-audit` (first step of `just smoke`).
- **Use `just` and keep `justfile` in sync.** Every `just` build/test caps CPU to 80% of cores (`nice -n 10 taskset …`); raw `cargo` is uncapped. Update `justfile` in the same change whenever a build, run, or test workflow changes.
- **Ask before**: running slow `client` tests (`just test-one client …`), running `just test` / `just test-full` (announce first for wide changes), launching benchmarks (the user runs their own on this machine), or changing anything in Grafana.
- **Never commit or print secrets.** `deploy/**/*.tfvars` and `.env` are git-ignored. When reading Grafana via `GRAFANA_SERVICE_TOKEN`, source `.env` only inside the single command that uses it; never echo, log, or persist it.
- **Keep core crates engine-agnostic.** `game`, `server`, `worldgen`, `world-store`, and `slint-ui` must stay free of Bevy and windowing dependencies.
- **Keep `aws-lc-rs` out of the tree.** The workspace's only rustls crypto provider is **ring** (a second provider makes rustls panic).
- **Do not replace vendored forks with crates.io deps.** `crates/rapier`, `crates/parry`, `crates/slotmapd`, and `crates/block-mesh` carry patches required for rollback, cross-machine determinism, and `glam 0.33` alignment.
- **Keep paired artifacts in sync:**
  - `crates/panel/src/ops.rs` ↔ `world-start` / `world-stop` in `justfile`.
  - `crates/worldgen/src/pipeline.rs` (`column()`) ↔ `#[cfg(test)]` `DensityFn` (bit-exact).
  - `crates/client/src/renderer/meshing/records.rs` ↔ `assets/shaders/voxel_pull.wgsl`.
  - Bump `worldgen::tile_map::WORLD_MAP_VERSION` on any change to `build_world_map` output.
  - Exhaustive field classifiers (new fields intentionally fail compilation until classified): `carry_components` / `Client::arrive`, `projection!` (`game/src/coordinator/mod.rs`), `host_fields!` (`game/src/world_manager/succession.rs`).

## Project & Naming

A deterministic voxel MMO in Rust: client/server over QUIC (WebTransport in the browser), rollback netcode, and a multi-region world forged up front (geology, climate, biomes, history) following a Dwarf Fortress–inspired direction (`docs/game-design.md`, `TODO.md`).

- **Gameplay**: One character plays in first person. Quests (authored as content packages) move the world's story through acts; a player may claim land and grow a settlement as mayor (the commander camera is the mayor's optional view). Terrain is voxel (stud terrain was reverted 2026-11-14); embark/return was removed; editor mode is admin-only.
- **Player-facing name**: **Errantry: Renown and Ruin** ("Errantry") — window/page titles, menus, UI, website, login pages, email, and `game.errantry.renownandruin.com`.
- **Internal code name**: **Orion** — keep everywhere players do not look (code identifiers, `deploy/envs/orion`, `just orion-*`, kube context, bucket/IAM names, `orion` Keycloak theme, storage keys).

## Workspace & Repository Layout

### Root Cargo Workspace (`Cargo.toml`, 26 members, excludes `quest`)
- **Core simulation & terrain**:
  - `crates/game` — engine-neutral simulation, rollback state, physics integration, region runner, `Coordinator`, `WorldManager`, peer/cut protocols.
  - `crates/macros` — `#[rollback]` proc macro (`#[undo(cell|map|slotmap)]`).
  - `crates/state-hash`, `crates/state-hash-derive` — canonical cross-platform `StateHash` trait & derive (`f32`/`f64` normalized, `usize`/`isize` widened to 64-bit).
  - `crates/worldgen` — pure deterministic terrain pipeline and offline world forge (`forge/`).
- **Client & UI**:
  - `crates/client` — Bevy `0.19.1` renderer, input, networking (QUIC + WebTransport/wasm), predicted regions, debug launches.
  - `crates/slint-ui` — engine-free Slint software-renderer UI (`crates/slint-ui/ui/`, built on sibling `../ui/crates/widgets`), split into generated leaf crates `crates/slint-ui-game`, `crates/slint-ui-frontend`, `crates/slint-ui-tools`.
  - `crates/bbmodel` — engine-free Blockbench `.bbmodel` loader.
- **Server, storage & platform services**:
  - `crates/observability` — shared OTLP tracing, trace-correlated text logging, and optional Pyroscope CPU profiles for native `login` / `server` processes.
  - `crates/server` — Tokio/QUIC/WebTransport server supporting `VOXEL_ROLE=all|host|coordinator|fleet`.
  - `crates/world-store` — SQLite/Postgres persistence, two-phase world cuts, coordinator/host/fleet tables, and `worldctl` CLI.
  - `crates/svc-link` — authenticated QUIC service links (`ring`, pinned self-signed certs, shared service token) with crossbeam API.
  - `crates/auth` — EdDSA (Ed25519) join JWT signing and JWKS verification.
  - `crates/login`, `crates/login-api` — HTTP login service (Axum, PostgreSQL, Keycloak OIDC) and wasm-safe API/claims types.
  - `crates/kube-api` — minimal Kubernetes API client over `reqwest` (`ring`), used by `fleet` and `panel`.
  - `crates/panel` — operator web panel for world status, logs/metrics, start/stop, and forge jobs.
- **Tooling & test support**:
  - `crates/mc-import`, `tools/mc-import` — untrusted Minecraft Sponge `.schem` importer → prefabs (`prefab-imports/`, never into `assets/`).
  - `tools/prefab-tool` — prefab preview/validation and town layout renderer.
  - `crates/physics-next` — rapier/parry fork pinning and determinism harness (nothing in `game`/`client`/`server` depends on it).
  - `crates/pg-test-support` — throwaway Postgres databases (`MMO_TEST_DATABASE_URL`, dev-dep only).

### Vendored Forks, Separate Workspaces & Top-Level Directories
- **Vendored forks (`crates/`, not workspace members)**: `rapier` (0.36, `enhanced-determinism`, no `parallel`/`simd8`), `parry` (0.31), `slotmapd`, `block-mesh` (`glam 0.33`). Every rapier/parry divergence carries `// mmo-fork:` and is pinned by a gate test in `crates/physics-next`. Math uses `glamx` (`glam 0.33`), re-exported as `game::math`.
- **Quest script workspace (`quest/`)**: Own `Cargo.toml`, `Cargo.lock`, and `quest/target/` (`quest-lang`, `quest-vm`, `quest-stub`, `quest-cli`, `quest-corpus`; `unsafe_code = "forbid"`), plus `quest/fuzz/` (excluded from `quest/`). Nothing in the root workspace may depend on `quest/`; `quest/` reaches into the root only for `crates/state-hash`.
- **Sibling UI repo (`../ui`)**: `widgets = { path = "../ui/crates/widgets" }` requires a local Factorio install or `FACTORIO_DATA` (Wube sprites are not shippable).
- **Other directories**: `assets/` (blocks, models, WGSL shaders), `deploy/` (OpenTofu `modules/`, `envs/local`, `envs/orion`, container `images/`), `docs/` (design docs, `superpowers/{specs,plans}/`, `memories/`), `scripts/` (run/deploy/perf/audit helpers), `tools/` (asset/creature/texture pipelines), `website/` (static site + `orion` Keycloak theme), `worlds/` (git-ignored local worlds).

## Build, Run & Configuration Dials

- **Toolchain (`rust-toolchain.toml`, `.cargo/config.toml`)**: Dated nightly `nightly-2026-10-04` with `wasm32-unknown-unknown`, `-Zembed-metadata=no`, `profile-rustflags = true`, and workspace `feature-unification = true`. Never float to `nightly`; bump dates deliberately then run `just smoke` and `just determinism-wasm`. No Cranelift.
- **Compile-time profiles (`Cargo.toml`)**: `dev` uses `opt-level = 1`, `debug = "full"`, `split-debuginfo = "unpacked"`, dependencies at `opt-level = 3` (`line-tables-only`), `worldgen` at `opt-level = 3`, `client` with `-Clto=off`, and `slint-ui-{game,frontend,tools}` at `opt-level = 0`, `incremental = false`, `-Zthreads=8`. Do not add `-Zthreads` to incremental crates. Use `profiling` (release + full debug) for CPU profiling and `wasm-release` for browser builds.
- **Key `just` recipes (`just --list`)**:
  - Build/lint: `just check`, `just build`, `just clippy`, `just fmt`, `just fmt-check`, `just web-check`.
  - Play/dev: `just run` (debug adventure in `worlds/debug-world/`, `--reset` / `--site N`), `just run-two`, `just play` (main menu → forge → world map → adventure), `just server` + `just client` (`127.0.0.1:6466`, WT `:6467`), `just web`, `just world-map`.
  - Multi-process dev: `just pg-up|pg-reset|pg-down`, `just kc-up|kc-down`, `just dev-login`, `just dev-r2` (`static` host `h0`), `just dev-r3 [hosts] [delay]` (`claim`), `just coordinator`, `just host`.
  - Quest workspace: `just quest-check`, `just quest-test`, `just quest-run <dir>`, `just quest-wasm`, `just quest-fuzz <target> [secs]`.
- **Build-time dials (`.cargo/config.toml` `[env]`, overridden by `just`)**:
  - `VOXEL_REGION_BLOCKS` (`region_blocks=64|128|256|512`): `64` default for tests/check/clippy/raw cargo, `256` for play recipes. Server and client must match (no handshake). Never hardcode a region edge — derive from `REGION_BLOCKS` / `REGION_SIZE`.
  - `VOXEL_CHUNK_BLOCKS` (`chunk_blocks=2|16`, default `16`): a block is 1 m = 16³ voxels (`BLOCK_VOXELS`); a **sim chunk** is `CHUNK_BLOCKS`³ blocks (entity/body/collider unit); a **brick** is 32³ voxels (`BRICK_VOXELS`, 2³ blocks, meshing/LOD unit). Clarify any literal `32` (`BRICK_VOXELS` vs `CHUNK_UNITS`). Determinism goldens are kept current at 16.
  - `VOXEL_TREE_FELLING=instant|physics`, `VOXEL_ISLAND_CELLS=0`.
- **Runtime dials**: `just world=tiny|small|full`, `view=<m>`, `VOXEL_NEAR_RADIUS`, `VOXEL_LOD_RINGS`; `VOXEL_VSYNC=on|off` (on by default; every perf-measuring script must set `off`); `just adapter=<gpu>` / `VOXEL_ADAPTER` (unpinned `adapter_pick` avoids slow NVK T600 — verify the `AdapterInfo` log line); `VOXEL_SINGLE_PLAYER=1`, `VOXEL_SKIP_MENU=1`.

## Testing & Verification

- **Audit**: `just test-audit [path]` fails on any test lacking `/// Regression:` or `/// Gate:` (excluding vendored forks `crates/{rapier,parry,slotmapd}/`).
- **Focused runs**: `just test-one <crate> "<cargo test args>"`. Integration tests live in a single `tests/it.rs` binary per crate (`--test it <mod>::`); add a separate `[[test]]` target only for tests touching process-global state (`HASH_MODE`, forged world, logger, wasm32).
- **Default check**: `just smoke` (hard-limited to 60 s; runs `test-audit`, fast checks/tests, and type-checks `client` only). Ask before running `client` tests.
- **Determinism & takeover**:
  - `just determinism-wasm` — required after touching sim math, physics forks, or anything hashed/serialized (`physics_goldens::` pins physics behavior; re-record only intentionally).
  - `just test-r4` — server `takeover` target (`--features test-hooks`, Postgres via `just pg-up`, `MMO_TEST_DATABASE_URL`).
- **Full suites & fuzzing**: `just test` (all fast tests across both workspaces, vendored subset, website) and `just test-full` (adds slow tests, `determinism-wasm`, `quest-wasm`, `test-r4`) run every stage and summarize PASS/FAIL; run when asked or for wide changes (say so first). `just fuzz [secs]` replays kept crash inputs then fuzzes targets (never part of `test`/`test-full`).
- **Test constraints**:
  - Tests slower than 100 ms MUST carry `#[ignore = "slow: >100ms — run via just test-full"]`.
  - Rollback changes MUST prove bit-exact restoration (`hash(before) == hash(after undo)`).
  - Headless client harnesses MUST use a tiny world (`set_lod_table(&[])`, near radius 0) or they OOM.
  - Tests depending on block properties MUST call `common::install_real_block_props()` first.

## Core Architecture Invariants

### Rollback & Determinism (`crates/game`, `crates/macros`, `crates/state-hash`)
- Rollback bar: `hash(before) == hash(after undo)`, bit-exact. `#[rollback]` gives `#[undo(cell|map|slotmap)]` fields auto-inverted wrappers; manual `Undo<T>` closures MUST be true inverses.
- Hash state only through `StateHash` + `crc32fast`, never std `Hash` or raw serialized bytes.
- Per-entry hash checking defaults to `Off` (`HASH_MODE=delta|full` to debug undo closures). Desync is caught by `VOXEL_STATE_HASH_INTERVAL` (100 ticks) — never weaken it.
- No inherent `f32` transcendentals in simulation paths (use `libm`); use `BTreeMap` (never `HashMap`) for ordered sim state; randomness only via stateless `roll(tick, subject, salt)`; tunables as named consts.
- Use `slotmapd`/rapier exact LIFO inverses (`revert_insert`/`revert_remove`) — plain `remove(insert(x))` does not restore allocator internals.
- `crates/game/src/lib.rs` glob re-exports at crate root are required by `#[rollback]` expansion. `TICK_RATE` is a period in ms despite its name.

### Regions & Seams (`game::region`, `game::slot_order`, `game::world_view`)
- One OS thread per running region, nothing parallel inside one; regions talk strictly via `RegionInput`/`RegionOutput` over `crossbeam` channels, never shared memory.
- One world-numbered tick everywhere (`WorldClock`); entities use `EntityId { birth_region, serial }`; boundary crossings are stamped `Transfer`s applied at T+1.
- Neighbour messages flow through `SlotInbox` and are applied canonically right before the next `Tick` on server and client alike — never apply a `Transfer` on receipt. Plain `RegionInput::Event` overtakes `SlotInbox`, so ordered neighbour messages must travel as `Transfer`.
- Sim code reads neighbours via `WorldView` (own ∪ halo) and writes foreign state only as an `Intent`, never directly. Intents never wake a region; salvage paths ensure items are never lost.
- Keep simulation coordinates region-local; world offsets exist only at the render boundary.

### Server, World Store & Distributed World (`crates/server`, `crates/world-store`)
- **Hub-and-spoke**: Tokio/QUIC/WebTransport stay at the network edge; `WorldManager` and region threads communicate over `crossbeam`. Wire packets MUST use `game::encode_packet`/`decode_packet` (bincode + lz4 framing) — never raw bincode on the wire.
- **World store (`VOXEL_WORLD_DB`, SQLite or Postgres)**: A read error (`LoadFailed`) or corrupt parked blob NEVER regenerates a region (only a missing row generates). Never edit applied migrations (sqlx checksums them); a cut part writes only rows of its own regions (never add a table two hosts' parts can both write). Check lock order in `world-store/src/succession.rs` before adding any query touching `hosts`, `super_regions`, `successions`, or `coordinator`. Postgres requires `max_prepared_transactions > 0`.
- **Roles & auth**: `VOXEL_ROLE=all|host|coordinator|fleet`; split roles require Postgres and matching `VOXEL_OWNERSHIP=static|claim`. `VOXEL_AUTH=token|jwt` (Keycloak OIDC + `crates/login` + EdDSA join JWTs).
- **Coordinator in the store**: `game::Coordinator` is a cache of the `coordinator` row, fenced solely by compare-and-set on `version` (write-before-send; `StoreDecision`s execute inside the transition tx in SAVEPOINTs; store clock uses `clock_timestamp()`, never `now()`; ticker = lowest live `ReplicaId`). Note: migration to the coordinator HTTP API & host mailbox (`docs/superpowers/plans/2026-11-19-coordinator-http-api.md`) is actively landing — inspect `game/src/coordinator/` and `server/src/coordinator_rt.rs` before assuming link vs mailbox paths.
- **Multi-host (`claim`), fencing & fleet**:
  - `HostId` is issued by `register_host` and never reused (derive nothing from its numeric value). Main-loop heartbeat runs on its own DB connection (never a timer thread).
  - Every host store write/load is owner-epoch fenced (`StoreError::Fenced` → `EXIT_FENCED=3`; `EXIT_COORDINATOR_FAILED=4`; `EXIT_WORLD_ROLLBACK=5`). Never add an unfenced store write. Enforce **fence before use** (`HostClaims::add`) and **shrink before next part** (`HostClaims::shrink`).
  - Coordinated two-phase world cuts save across hosts. Under `claim`, every cut-joining path must call `PeerJournal::cut_begun`, and any new state-bearing `Reliable` message must be journalled (or explicitly argued out).
  - Elastic 3: coordinator assigns free super-regions on demand (`AssignLand`); hosts release idle land in their own cut parts.
  - Elastic 4: coordinator writes `desired_hosts`; `VOXEL_ROLE=fleet` reconciler (`server/src/fleet.rs`, `OpenMode::ReadOnly`) reconciles host Deployments/Services/PDBs without touching coordinator state.

### Client, Rendering & Slint UI (`crates/client`, `crates/slint-ui`)
- **Bevy pinned at `0.19.1`**: Upgrades are deliberate migrations. Shader tests use stubs and miss upstream WGSL changes — after a bump or shader edit, run `just pull-smoke` on a real adapter and inspect terrain via `just run` (nothing automated covers LOD rings or water on real GPUs).
- **Terrain rendering**: Near terrain meshes bricks into 8-byte quad records (`meshing/records.rs` ↔ compiled-in `voxel_pull.wgsl` / `voxel_pull_depth.wgsl` / `voxel_texture.wgsl`, rebuild required, no hot reload) merged per sim chunk (`VOXEL_NEAR_RENDER=per-chunk|direct|gpu`). Far terrain renders server-streamed LOD rings.
- **Connections (`connections.rs`)**: `ConnectionSet` holds one connection per host (home vs `Credential::Observer`). Non-loopback native QUIC verifies `cert_hash` pin or public web PKI (loopback skips verification). Home drops/refusals and `world_starting` auto-rejoin via stored refresh token.
- **Session lifecycle (`session.rs`)**: Every new per-world resource, system `Local`, or entity root MUST be registered (`add_session_reset`, `SessionLocal`, or `SessionScoped`) so nothing leaks into the next world.
- **Slint UI only**: No `bevy_ui`, no egui. `CursorPolicy` is the only cursor-grab writer. `dev_ui` (F3) is dev tooling only. Compiled with `fortress` theme only; never bind `visible`/`opacity`/shadows on a widget component's root (forces inlining and bloats codegen); every root component calls `GameTheme.apply()` in `init`.
- **Wasm**: No shared-memory threads; compute runs in Web Workers via bincode messages (`renderer::compute::JobKind`, self-contained payloads with no ECS access).

### Worldgen & Assets (`crates/worldgen`, `assets/`, `tools/`)
- `generate_region` is pure in coordinates + forged world (making park/regenerate safe). Forge tunables live in `forge/knobs.rs` (`just forge-ui`; validate at 16 m cells, pass `forge_quality`, inspect with `just world-map`). Water is recipe data (`ColumnGround::water_top`), never blocks.
- `.bbmodel` loads natively (`crates/bbmodel`; `just model-view`, `just asset-viewer`). Never hand-edit creature `.bbmodel` files — edit `tools/creatures/src/<id>.toml` and run `just creatures`. `just assets` uses paid APIs (inspect with `--picture-only` before buying models).

## Deployment, Profiling & Diagnostics

- **OpenTofu (`deploy/`, not Terraform)**: Local k3d twin (`deploy/envs/local/`, `just twin-up|twin-images|twin-apply|twin-check|twin-down`, never the inner loop). Production `orion` k3s (`deploy/envs/orion/`, S3 state, `just orion-plan|apply <root>` — always plan first; never apply `world` while stopped/mid-update; shared config changes require `world-stop` → apply → `world-start`).
- **World releases**: Stop → set images by digest → start (`just orion-world-push`, `just orion-world-update`, `just orion-world-rollback`), never rolling updates. Never run `kubectl rollout status` on host or coordinator Deployments.
- **VPN & Grafana**: `just orion-vpn-status|up|down` (WireGuard `10.99.0.1`, fallback `just tunnel`). Read-only Grafana queries via `.env` `GRAFANA_SERVICE_TOKEN` at `http://grafana.vpn.errantry.renownandruin.com`; ask before mutating Grafana (durable changes go in `deploy/modules/monitoring/`).
- **Profiling**: Measure in **release** or `profiling` profile (`dev` distorts hot loops ~2.5×), confirm GPU (`AdapterInfo`), watch laptop power clamping (`scaling_cur_freq`), compare ratios rather than raw seconds, and set `VOXEL_VSYNC=off`. Key recipes: `perf-capture`, `diag`, `gpu-measure`, `fps-steady`, `trace-chrome` / `trace-join` (close window, do not Ctrl-C), `perf-client` / `perf-server`, `fly-probe`, `shuttle-probe`, `firefox-profile`. Never `json.load` or open whole traces/folded stacks — use summary scripts in `scripts/`. Keep `[perf]` logs per process/region/batch, never per chunk/key/frame.

## Coding Style & Commit Guidelines

- Run `just fmt` (`rustfmt`, 4-space indent) and `just clippy` (warnings are errors).
- Naming: `snake_case` modules/functions/tests, `UpperCamelCase` types/traits, `SCREAMING_SNAKE_CASE` constants. Mixed editions (2021 older crates, 2024 newer crates) are intentional.
- Cross-thread messaging uses `crossbeam`.
- Commits follow scoped Conventional Commit subjects (`feat(coordinator): ...`, `fix(client): ...`, `perf(game): ...`, `docs: ...`), narrow and imperative. PRs explain behavior/architectural impact, link design docs, list verification commands, and include screenshots or profiling evidence for rendering/UI/performance changes.

## Deep References (Read On Demand)

| File / Path | Contents |
|---|---|
| `CLAUDE.md` | Detailed subsystem notes: deployment roots & runbooks, Elastic 1–4 coordinator/fleet/succession/drain protocols, link/peer rules, k8s env vars, metrics/alerts. |
| `docs/game-design.md` | Decided, proposed, and open game design. |
| `TODO.md` | Active backlog, hand checks, and pending reviews. |
| `quest/README.md` | Quest script language spec summary, VM tunables, CLI flags, corpus goldens, and fuzzing workflow. |
| `docs/superpowers/{specs,plans}/` | Dated design specs and "As built" implementation plans (grep by topic). |
| `docs/memories/` | Distilled technical investigation notes and past findings. |
| `docs/claude-md-full-reference-2026-11-08.md` | Historical pre-prune subsystem reference (not kept up to date). |
