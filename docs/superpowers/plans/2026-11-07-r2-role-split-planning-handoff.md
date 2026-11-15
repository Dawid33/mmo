# Handoff: write the R2 implementation plan — split the roles (login · coordinator · region host), still one machine

**Date:** 2026-11-07
**For:** a fresh session whose job is to **write** (not implement) the R2 plan of the multi-instance
roadmap, with the superpowers `writing-plans` skill. Brainstorm the open design questions with the
user first (`superpowers:brainstorming`), then write the plan.
**Output:** `docs/superpowers/plans/<date>-r2-role-split.md` (one plan; split into two only if it
clearly exceeds ~12 tasks — e.g. "R2a login + accounts" and "R2b coordinator / host split + owner
epochs" — and say why).

## 1. Where the roadmap stands

The roadmap is `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0.5,
amended by §0.9 and §0.10 (§0.10 is the final state of the design pass).

| Phase | Status |
|---|---|
| Seam-transparent regions (prerequisite) | done 2026-11-06 |
| R0 — dev Postgres | done (`f31d5ee`: `deploy/dev/compose.yml`, `just pg-up/pg-down/pg-reset/pg-psql/test-pg`) |
| R1 — world store, one host | done 2026-11-07, `e038093..d4c27ac` (Plan A `2026-11-07-world-store-r0-r1.md` + Plan B `2026-11-07-forged-world-in-db.md`) |
| **R2 — split the roles, one machine** | **this plan** |
| R3 — many hosts locally + seamless go/no-go | later |
| R4 — takeover / failover / drain | later |
| R5 — the single k3s server (Orion) | partly started: k3s + cert-manager + monitoring applied (`deploy/envs/orion`) |
| R6 — region-host pod autoscaling | separate plan, later |

R2 as specified (§0.5): *a login service (auth, sessions, `SpawnPlayer`), the world coordinator
and the region host as separate processes; the `AllInOne` and `Static` ownership policies;
super-region owner epochs in Postgres; client login over HTTPS.* Plus, from §0.9/§0.10: the
**management panel** (axum + htmx, one Rust binary with a `forge` subcommand run as a k8s `Job`
later) — decide with the user whether the panel is R2 or its own plan (it is not on R2's critical
path; `worldctl forge` already exists as its back end).

## 2. Read first (in this order)

1. The design doc §0.7 (whole: (a) auth + JWT, (b) StatefulSet + deterministic assignment +
   fencing `super_region_owner(sx, sz, ordinal, epoch)`, (c) networking, (d) S = 16 regions),
   §0.8 (open questions 8–12 are R2's), §0.9 (b) accounts across worlds, (e) panel, (f)
   LISTEN/NOTIFY, §0.10 (decisions 4–5: sessions keyed `(account, world)`, newest-session-wins
   per world; the htmx panel), §7 (ownership, liveness, fencing), §9 (front door, identity,
   transfer protocol). Skip §0.11 (rate limiting): the user DEFERRED it out of R0–R5 — R2 does
   not build it (at most: don't make it harder later).
2. CLAUDE.md: build/test rules; `crates/server` (world store, cut autosave, `VOXEL_WORLD_DB`,
   single-player server spawned by the client); the "Adventure survival" player-identity
   paragraph (`PlayerId` / `PlayerToken`, `Hello` handshake, `PlayerStore`, takeover);
   `crates/game` world_manager / region_runner (the `RegionInput`/`RegionOutput` message pair
   is "the future network seam"); `crates/client` session / netcode / web_endpoint.
3. Plan A (`2026-11-07-world-store-r0-r1.md`) "Decisions settled" — R2 replaces its Decision 9
   (single-writer lock: SQLite `flock`, Postgres `pg_try_advisory_lock(WORLD_LOCK_KEY)`, called
   "an R1 stand-in; R2's owner epochs replace it").
4. The two R1 SDD ledgers for the deferred items that touch R2 (§5 below lists them):
   `.superpowers/sdd/2026-11-07-world-store-r0-r1/progress.md`,
   `.superpowers/sdd/2026-11-07-forged-world-in-db/progress.md` (git-ignored scratch; may be
   gone — the list in §5 is self-contained).
5. Background only: `docs/superpowers/specs/2026-11-06-agones-review/`,
   `docs/superpowers/specs/2026-11-07-factorio-netcode-learnings.md` (§6 ranked recs, for the
   transfer/reconnect parts), `docs/superpowers/specs/2026-11-06-udp-network-monitoring-research.md`.

## 3. Decisions already made (do not re-open)

- Seamless crossing only; no static-border fallback (§0.9.1).
- One world per deployment, one logical database per world; accounts are global (shared login
  database), players are per world (§0.9.2, §0.9(b)).
- One account may play several worlds at once, once per world; sessions keyed
  `(account, world)`; newest session wins per `(account, world)` and parks the old one (§0.10.4).
- JWT checked only at connect / reconnect / cross-host handover; revocation = login → host kick;
  keys rotated via a JWKS endpoint (§0.8 q9 recommendation, §0.10 "left for specs" default).
- Internal call security default: k8s NetworkPolicy + a shared service token (§0.10 default).
- Coordinator downtime default: hosts keep simulating; world events (embark, history, journeys)
  queue (§0.10 default).
- Management panel = server-side-rendered htmx served by Rust (axum), using our crates directly;
  forge as a `Job` running the same binary's `forge` subcommand (§0.10.5).
- Ownership: deterministic assignment of super-regions (S = 16 regions per side) to host
  ordinals, fenced by owner epochs in Postgres (§0.7(b)); StatefulSet in R5 (§0.7(b′) verdict).
- Store: Postgres for servers, SQLite for single-player, same code (`world-store` crate, R1).
- No Postgres backups yet (§0.9.3). No node autoscaling (§0.10.1).
- The native single-player path must keep working: the client spawns its own server on a
  `.sqlite` world (R1). Whether single-player needs login at all is a question (§4).

## 4. Questions for the user (brainstorm before planning)

Open in the design doc with no default — must be answered:
1. **Email provider** for verification and password reset (SMTP / API, which?), and are
   verification and reset required at launch? (§0.8 q8)
2. **GDPR / account deletion:** privacy notice, deletion and data export; what deleting an
   account does to the player's bodies and records in each world. (§0.8 q12)

Confirm the defaults (one line each): JWT lifetime/checking (above); internal call security
(NetworkPolicy + shared token — in R2 there is no k8s, so: a shared token over localhost?);
coordinator downtime (hosts keep simulating).

Scope questions the plan must settle with the user:
3. **Single-player and login:** does the client-spawned single-player server skip login
   (local `PlayerToken` as today) — recommended — or go through a local login?
4. **Browser login:** the wasm client today joins with a `localStorage` token. R2's HTTPS login
   → JWT → `Hello(jwt)` must work in the browser (Slint login screen native + wasm, §0.8 q19).
5. **Panel in R2 or its own plan?** (Recommend its own plan; R2 needs only enough admin to
   create a world — `worldctl forge` + a login-database world registry.)
6. **What "coordinator" owns in R2:** the `WorldManager`'s world-level duties (sessions/homes,
   history, world map, embark, cut orchestration, LOD builder?) vs what moves to hosts (region
   threads, halo, intents). With `AllInOne` the coordinator and one host may still share a
   process — decide whether R2 ships them as separate processes talking over a real link
   (localhost) or as separable roles in one binary with an in-process transport first.
7. **The host↔coordinator link** (transport, framing, auth) — R3 adds host↔host; R2 should
   pick a transport that R3 reuses (QUIC/quinn is already in the tree; `encode_packet` framing
   rules apply).
8. **Owner epochs vs the R1 lock:** owner epochs replace `pg_try_advisory_lock(WORLD_LOCK_KEY)`
   on Postgres; SQLite keeps the file lock. The cut commit must check the epoch (a fenced host
   must fail its commit), and region loads must be epoch-guarded.
9. **World registry:** where the list of worlds lives (login database), what `world.json` /
   `preview.png` become for server worlds (R1 kept them as single-player menu files; §0.10 says
   Postgres worlds get their listing from the login registry in R2).

## 5. The code today (verified 2026-11-07 — re-verify before quoting lines)

- **One process does everything:** `crates/server/src/lib.rs` `run()` — QUIC + WebTransport
  edges (tokio) → manager thread (`game::WorldManager<ThreadRegionSpawner>`) → region threads
  (`region_threads.rs`) over crossbeam; LOD workers (`lod_workers.rs`, retry with backoff);
  store via `world_store::StoreHandle` (one store thread, current-thread tokio runtime);
  `cuts.rs` `CutWriter` (cut autosave every `WORLD_SAVE_SECS`, exit park `EXIT_PARK_TIMEOUT`,
  `world_save_stuck` report); `world_file.rs` (open/import/forge/re-tile, legacy refusal,
  `ensure_history`).
- **Identity today:** `game/src/player.rs` `PlayerToken` / `PlayerId`; the client sends
  `Hello(token)` first on every connection (native: token file; browser: `localStorage`);
  `server/src/player_store.rs` `PlayerStore` trait (`StorePlayers(StoreHandle)` — token → id
  via `resolve_player`, records only in cuts; `MemoryPlayerStore` for procedural); "newest
  session for a token takes over" is implemented in the manager (`WorldManager` takeover path).
  No accounts, no passwords, no JWT, no HTTPS login.
- **The network seam already exists in-process:** `game/src/region_runner.rs`
  `RegionInput` / `RegionOutput` (stamped `Transfer`, `Intents`, `Forward`, `SaveAt`/`CutSnapshot`,
  `LoadFailed`, halo deltas, …) between the manager and region threads; the `WorldManager` is
  threadless and IO-free, parameterised by a `RegionSpawner` (OS threads on the server,
  `InlineSpawner` for wasm/tests). R2/R3 put a real link where crossbeam is today.
- **Store-side facts R2 must respect:**
  - single-writer lock (`crates/world-store/src/open.rs`, `WORLD_LOCK_KEY`) — to be replaced by
    owner epochs on Postgres; schema v1 left a comment where R2's `super_regions(owner, epoch)`
    table goes (`migrations/{sqlite,pg}/0001_schema_v1.sql`); migrations are numbered
    `0001`/`0002`; Plan A's `schemas_match` test compares both engines + a checked-in list;
  - the forged world is tiled in the store (`forged_meta`, `forged_tiles`, `forged_world_map`,
    `forged_archive`); a host pins its 3×3 tiles per region; multi-host tile reads need a
    read-only store handle that skips the writer lock (R-B3, deferred to R2);
  - `world.content_crc` = the tiled crc (keys the worldgen cache only).
- **Client:** `crates/client/src/session.rs` (AppState, `PendingServer`, web flow),
  `netcode.rs` / `netcode_web.rs` (one connection; QUIC / WebTransport), `web_endpoint.rs`,
  `local_host.rs` (spawns the single-player server with `VOXEL_WORLD_FILE` / `VOXEL_WORLD_DB`),
  `player_token.rs`. The client never links sqlx (keep it so).

## 6. Deferred R1 items that land in R2's area (carry or explicitly defer)

- **R-B3:** a lock-free, read-only `StoreHandle` mode so several hosts can read tiles / stored
  terrain from one Postgres database (blocked today by the advisory lock).
- **Exit-save decision (user's, still open):** when a region never parks at exit, the exit cut
  commits the rest with that region at its last snapshot (possible duplication/loss across its
  seams, same rule as the periodic `silent_stopping` path). Alternative: skip the exit cut when
  any region is given up. Ask the user; with owner epochs a fenced/dead host's regions are the
  same problem at larger scale (R4).
- Store-thread decode runs inline on the one current-thread runtime (region loads, halo seeds of
  stored neighbours and LOD `Tile::stored` builds serialise there) — matters once a coordinator
  and hosts share or split a store; consider `spawn_blocking` / per-host handles.
- `resolve_player` is the token → id allocation; R2's account model changes it to
  `(account, world)` → `PlayerId` (players per world, accounts global).
- Single-player first start (import + tile + world map) vs the client's 2-min
  `CONNECT_DEADLINE` — unmeasured at 100 km.

## 7. Constraints the plan must carry (into its Global Constraints)

- Work on `develop` in the main checkout. **Never create a git worktree** (CLAUDE.md), including
  subagents. The tree holds the user's uncommitted work (CLAUDE.md, justfile, several client
  files, `crates/game/src/ui.rs`, slint, deploy/monitoring): `git add` only a task's files;
  for CLAUDE.md / justfile hunks use index-only staging (HEAD copy + hunk → `git update-index`).
- `game` stays Bevy-free, IO-free, sync, wasm-safe. The client never depends on `world-store` /
  sqlx; the browser build keeps working (WebTransport, `JOIN_ONLY` menu).
- Wire framing stays `game::encode_packet` / `decode_packet` on every edge (incl. any new
  host↔coordinator link). Async (tokio) only at network edges; crossbeam between threads.
- Never weaken the server state-hash check; rollback correctness bar unchanged.
- Conservation (exactly-once entities/items/players across seams and saves) is the R1 invariant
  — any new link or ownership change must keep it, with seeded-jitter tests like
  `handoff_schedule` / `save_cut`.
- No compile-time database (runtime `sqlx::query()`), shared schema subset, both engines'
  migrations + `schemas_match`.
- Secrets never committed (JWT signing keys, service tokens, SMTP credentials); dev defaults
  only via env / git-ignored files.
- Verification: `just test-one …`, `just smoke`, `just test-threads` after manager/region/handoff
  changes, `just test-pg` for store work, `just determinism-wasm` if a hashed/serialized sim type
  changes. Never whole suites; client test binaries are an explicit opt-in. Tests >100 ms get the
  slow ignore tag. Ask before live runs (one client, `adapter=Intel`), benchmarks, 100 km forges.
- Keep the justfile in sync (new recipes for each new process: e.g. `just login`,
  `just coordinator`, `just host ordinal=N`, `just dev-r2` to run them together).

## 8. What the plan's Review Focus must cover (at least)

- A login → `SpawnPlayer` → host connect with an expired / revoked / wrong-world JWT.
- Two sessions of one account in the same world (newest wins, old parked once) and in two
  worlds (both live).
- A fenced host (stale owner epoch) trying to commit a cut or load a region — refused, nothing
  written.
- Coordinator restart while hosts simulate: world events queue and replay exactly once.
- Single-player still works with no login service running.
- The browser client logging in and joining over WebTransport.

## 9. How to work

- Start with `superpowers:brainstorming` on §4's questions (lay out trade-offs in prose before
  offering options — user preference), then `superpowers:writing-plans`.
- Use the Explore agent for code surveys (every caller of `PlayerStore` / `resolve_player`, the
  manager's world-level duties vs region-level ones, the client's connect/session flow); verify
  any subagent claim before it goes into the plan.
- Plan format: header, "Decisions settled in planning", Global Constraints, Review Focus, tasks
  with Files / Interfaces / steps (TDD), self-review — like the two R1 plans.
- Don't write code. Commit the plan file only if the user asks. Then offer the execution choice
  (subagent-driven recommended).
