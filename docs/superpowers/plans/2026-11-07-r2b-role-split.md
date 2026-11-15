# R2b — World coordinator / region host split, owner epochs, login push Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run the world as separate processes on one machine — a **world coordinator** (history, calendar, sites, embark / adventure claims, legends) and a **region host** (client edge, `WorldManager` over the regions it owns, region threads, LOD, save cuts) — linked by an authenticated QUIC service link, fenced by **super-region owner epochs** in Postgres, with the login service (R2a) **pushing `SpawnPlayer` / `Kick`** to the host. Single-player stays one `AllInOne` process on SQLite.

**Architecture:** The `WorldManager` stops owning a `History`. It talks to a world coordinator through typed messages (`HostMsg` / `CoordMsg`, serde) over a `WorldLink`: in-process (crossbeam / inline) for `AllInOne` and tests, a QUIC link (new crate `svc-link`, reused by R3 for host↔host) for split processes. The coordinator is a threadless core (`game::coordinator::Coordinator`) wrapped by a runtime that persists its state (`history` + request memo + claims) into the world store under its own fence. Hosts claim super-regions at start (`AllInOne` = all, `Static` = rendezvous hash over `VOXEL_HOSTS`), every region load and cut commit checks the owner epoch in the same transaction, and a fenced host self-fences (stops, exits non-zero, writes nothing more). The Postgres advisory lock is gone; SQLite keeps its file lock and runs `AllInOne` only.

**Tech Stack:** Rust; quinn 0.11 + rustls + rcgen (already in the tree) for the service link; sqlx 0.8 runtime queries on SQLite + Postgres; crossbeam between threads, tokio only at network edges.

**Spec:** `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0.4(a)(b)(d), §0.5 R2, §0.7(b)(c)(d), §0.8 q10/q11, §0.10, §7; handoff `docs/superpowers/plans/2026-11-07-r2-role-split-planning-handoff.md`; prerequisite plan `docs/superpowers/plans/2026-11-07-r2a-login-accounts.md` (must be done first: `Credential`, `auth`, `login`, `VOXEL_AUTH=jwt`).

**R2a as built (done 2026-11-08, develop `0d19491f..15c9a4d6`) — read before starting:**
- `crates/server/src/auth_edge.rs`: `AuthMode::resolve(single_player, lookup)` (a single-player server is ALWAYS token mode, whatever `VOXEL_AUTH` says); `Authenticator::authenticate` → `check_jwt` (size cap `MAX_JWT_BYTES`, verify, unknown-kid refetch, `player_token` validated, then the in-memory `epochs` map under one lock). `JwksCache` keeps the published verifier in a `std::sync::RwLock<Arc<Verifier>>` and serialises refetches on a tokio mutex; `verifier_for(kid, now_secs) -> Arc<Verifier>`. Task 11's "pending spawns replace the epoch map" replaces that `epochs` map.
- `crates/server/Cargo.toml` has `autotests = false`: a new server test file needs its own `[[test]]` entry (or a module in `tests/it.rs`).
- The workspace's only rustls crypto provider is **ring** (feature unification makes a second one panic every `builder()`): SES uses `aws-smithy-http-client` `rustls-ring`; the server's QUIC config names ring explicitly. `svc-link` must do the same (`builder_with_provider(ring::default_provider())`) and must not pull in `aws-lc-rs` (`cargo tree -i aws-lc-rs` must match nothing).
- Login `join` rotates the refresh token (`LoginStore::rotate_refresh`, one transaction) and validates the world first; `bump_session` → `session_epoch` in the JWT. The client re-reads the stored refresh token before every refresh / join / logout.
- Open items R2a left for R2b/R5 are in `TODO.md` (native QUIC skips certificate verification while sending the JWT; the browser's `{origin}/login` fallback was dropped until R5).

## Decisions settled in planning (user, 2026-11-07)

1. **Split shape B — world services move out of the host.** A host is today's server minus the history: client edge, sessions/homes, subscriptions and fan-out, the directory and `in_flight`, region threads, LOD, save cuts of its regions. The coordinator owns what does not partition by region: `History` + calendar, site claims (fortress / adventure exclusivity per player), embark / return / adventure validation, embark options, legends, person info, daily calendar / site broadcasts and notices. The world map stays with the host (it reads `forged_world_map` from the store itself); the host rebuilds the `SocietyGrid` from the map for landings.
2. **Internal transport: QUIC (quinn)**, one bidirectional stream per link, frames = `u32` length prefix + `game::encode_packet` bytes. Auth: a shared service token (`VOXEL_SERVICE_TOKEN` or `VOXEL_SERVICE_TOKEN_FILE`, constant-time compare) in the first frame; the listener's self-signed certificate (rcgen) is pinned by SHA-256 (written by the listener to a file, read by the dialer). No k8s yet, so no NetworkPolicy; localhost binds by default.
3. **Exit save:** if any region was given up at exit (`give_up_unparked`), **no exit cut is committed** — the world rolls back to the last periodic cut (≤ `WORLD_SAVE_SECS`), conservation holds. A fenced host never commits anything again.
4. **Two plans:** R2a (login) before R2b (this).
5. **Single-player:** `AllInOne` in one process, no login, SQLite, today's token `Hello` — unchanged behaviour.
6. **Coordinator downtime (§0.10 default):** hosts keep simulating; region lifecycle notifications are dropped while it is down (superseded by the `Attach` snapshot on reconnect); client world requests (embark, return, adventure, legends, person info, embark options) queue in the host's outbox and replay on reconnect, exactly once (request ids + the coordinator's persisted answer memo); a request older than `WORLD_REQUEST_TIMEOUT_SECS = 120` is answered to the client with a new `…::WorldUnavailable` refusal and forgotten.
7. **Coordinator persistence (planning decision):** the coordinator is the **only writer** of history state; the host's save cut no longer carries the history. Rule *durable before visible*: a directive batch or a claim-changing reply (embark, return, adventure start, release) is sent only after the state that produced it is committed. Plus a periodic save every `COORDINATOR_SAVE_SECS = 60` and one at orderly exit. Why it is safe for the history to be ahead of the regions: region saves never contain person bodies (`portable.rs`: "Persons: the history respawns them") and `History::after_load` returns everyone embodied to the abstract history — exactly today's "history captured at cut assembly time", only more often.
8. **Coordinator restart = reattach:** the restarted coordinator loads its last saved state (`after_load`), and each host's first message is `Attach { running regions with the people bodies in them, online players with their claims }`; `History::reattach_region` re-embodies the listed people without spawning, and answers a `DespawnPerson` directive for a body whose person the restored history has dead or elsewhere ("bodies are truth" for the living, the history for the dead). Residents the history places in a running region but whose bodies are absent stay abstract until the region next starts.
9. **Owner epochs (Postgres) replace `pg_try_advisory_lock(WORLD_LOCK_KEY)`:** `super_regions(sx, sz, owner, epoch)` and `coordinator(id = 1, epoch, …)`. R2 claims unconditionally at process start (bump epoch, set owner) — there is no lease or liveness yet (R4's `PgClaim`); a second process claiming the same super-region fences the first. Writes check the epoch in the same transaction (`FOR SHARE` on Postgres). SQLite keeps the `flock` and runs only `AllInOne` (the split roles refuse a SQLite store with a message).
10. **Ownership policy:** `VOXEL_OWNERSHIP=all` (default) | `static` with `VOXEL_HOST_ORDINAL` + `VOXEL_HOSTS`; `owner(sx, sz) = argmax over ordinals of splitmix64(sx, sz, ordinal)` (rendezvous, no world seed — stable for a world's life). Super-region size `VOXEL_SUPER_REGION_REGIONS` (default 16) must equal `world.super_region_regions` (already in the world row; mismatch refuses start). In R2 a host refuses to run a region it does not own (treated as unavailable: `RegionUnavailable` refusals, arrivals wait in the mailbox); crossings into another host's region are R3.
11. **Global rows a cut wrote under the lock:** `in_flight` gets a `host` column and a commit replaces only its own host's rows; `world.world_tick` becomes `MAX(world_tick, $t)`; player ids come from the database (`players.player_id` auto-assigned: SQLite rowid, Postgres identity) instead of `MAX+1` under an in-process mutex.
12. **Read-only store handle (R-B3):** `OpenMode::ReadOnly` takes no lock, runs no migrations (refuses a schema version it does not know) and refuses writes — for tools and, in R3, for hosts' tile reads.
13. **Login push (§0.4d):** `/v1/join` → login bumps the session epoch → `SpawnPlayer { player_token, session_epoch }` to the world's host over the service link (ack within `SPAWN_ACK_TIMEOUT_MS = 3000`, else 503 `WorldUnavailable`) → ticket. The host parks any older session of that player at once (cross-process newest-wins) and accepts a JWT `Hello` only if it matches a pending spawn's epoch within `SPAWN_PENDING_TTL_SECS = 120`; this replaces R2a's in-memory epoch maximum. **Revocation** = `Kick { player_token, reason }` sent by login on logout-everywhere and password reset to every world host the account has a session in. Placement: R2 has one host per world (registry columns); per-ordinal placement by the player's record region is R3.

## Global Constraints

- Work on `develop` in the main checkout. **Never create a git worktree** (CLAUDE.md), including subagents. The tree holds the user's uncommitted work: `git add` only a task's own files; CLAUDE.md / justfile hunks via index-only staging (HEAD copy + hunk → `git update-index --cacheinfo`).
- `game` stays Bevy-free, IO-free, sync, wasm-safe: the coordinator core, messages, ownership and the inline link live in `game`; sockets, stores and threads live in `server` / `svc-link`. The client never depends on `world-store`, `sqlx`, `svc-link`.
- Every network edge (client edges, the service link) frames with `game::encode_packet` / `decode_packet`. Tokio only at network edges; crossbeam between threads.
- **Conservation** (exactly-once entities, items, players, people across seams, saves, and now the coordinator link) is the invariant: every new message path gets a seeded-jitter test in the `handoff_schedule` / `save_cut` style (`game::InlineSchedule`, failures name a replayable seed).
- Never weaken the server state-hash check; rollback correctness bar unchanged. If a hashed / serialized **sim** type changes, run `just determinism-wasm` (the messages here are not sim state; `EmbarkRefusal` / `AdventureRefusal` gain a variant — protocol only).
- No compile-time database; both engines' migrations kept column-for-column + `schemas_match`; Postgres halves via `just test-pg`.
- Secrets never committed (service token, link cert keys); dev values under git-ignored `target/dev-r2/`.
- Verification: `just test-one …`, `just smoke`, **`just test-threads` after every task touching the manager / region threads / handoff**, `just test-pg` for store work. Never whole suites. Client test binaries are an explicit opt-in (this plan needs none). Tests >100 ms get the slow tag. Ask before live runs (one client, `adapter=Intel`) and benchmarks.
- Keep the justfile in sync: `coordinator`, `host` (`ordinal=`), `dev-r2` (pg + login + coordinator + host), `test-r2` (the split end-to-end tests).
- Tunables are named constants.

## Review Focus

1. **A fenced host** (a second process claimed its super-regions) tries to commit a cut and to load a region: both refused with `StoreError::Fenced`, nothing written, the host self-fences (stops ticking, closes clients with `Refused("server moved")`, exits non-zero) — Task 2 (store) + Task 3 (host).
2. **Coordinator restart while the host simulates:** an embark requested during the downtime completes exactly once after reconnect (one site founded, one `EmbarkResult(Ok)`), a directive issued before the crash is not applied twice, and no person is duplicated or lost — Task 10.
3. **Login → `SpawnPlayer` → connect with a revoked / superseded / unspawned JWT:** refused at the edge; a `Kick` parks the online session once — Task 11.
4. **Two sessions of one account in one world across the push:** the second `SpawnPlayer` parks the first session before the new `Hello`; the old ticket connecting late is refused; one body, one record — Task 11.
5. **Single-player with nothing else running:** `AllInOne` on SQLite, no login, no service link, identical behaviour (all existing world_manager / store_world / threaded_world tests pass through the inline link) — Tasks 5–7, 9.

---

## File structure

```
crates/world-store/migrations/{sqlite,pg}/0003_owner_epochs.sql   NEW
crates/world-store/src/fence.rs        NEW  Fence, claim/check helpers, StoreError::Fenced
crates/world-store/src/open.rs         MOD  OpenMode, no pg advisory lock
crates/world-store/src/rows.rs, handle.rs, thread.rs   MOD  fenced commit/load, coordinator state rows, player ids
crates/game/src/ownership.rs           NEW  super_region_of, rendezvous_owner, Ownership
crates/game/src/coordinator/           NEW
  mod.rs        Coordinator core (History + claims + memo + gating)
  msg.rs        HostMsg, CoordMsg, WorldRequest, WorldReply, RequestId, Attach…
  link.rs       WorldLink trait, InlineWorldLink (+ jitter from InlineSchedule)
  state.rs      CoordinatorState (persisted blob: history + memo + claims)
crates/game/src/history/handover.rs    MOD  reattach_region
crates/game/src/world_manager.rs       MOD  history → world link (lifecycle, requests, outbox, Attach)
crates/svc-link/                       NEW  QUIC service link (listener, dialer, hello, reconnect)
crates/server/src/roles.rs             NEW  VOXEL_ROLE: all | host | coordinator
crates/server/src/coordinator_rt.rs    NEW  coordinator runtime (core + store + link)
crates/server/src/login_link.rs        NEW  host side of SpawnPlayer / Kick
crates/server/src/auth_edge.rs         MOD  pending spawns replace the epoch map
crates/server/src/lib.rs, cuts.rs, region_threads.rs, world_file.rs   MOD
crates/login/src/host_link.rs          NEW  login side of SpawnPlayer / Kick
crates/login/migrations/{sqlite,pg}/0002_host_link.sql            NEW
```

---

### Task 1: Store schema 0003 — owner tables, per-host in-flight, database-assigned player ids

**Files:**
- Create: `crates/world-store/migrations/sqlite/0003_owner_epochs.sql`, `crates/world-store/migrations/pg/0003_owner_epochs.sql`
- Modify: `crates/world-store/tests/schema.rs:78-90` (`EXPECTED`), `crates/world-store/src/rows.rs:351-408` (`find_player` / `resolve_player`), `:509-541` (in_flight replace, world_tick), `crates/world-store/src/thread.rs:165-175`
- Test: `crates/world-store/tests/{schema.rs, players.rs (new or existing module), cut.rs}`

**Interfaces:**
- Schema: `super_regions(sx INTEGER, sz INTEGER, owner INTEGER NOT NULL, epoch INTEGER NOT NULL, PK(sx, sz))`; `coordinator(id INTEGER PK CHECK (id = 1), epoch INTEGER NOT NULL, state BLOB NULL, saved_utc TEXT NULL)`; `ALTER TABLE in_flight ADD COLUMN host INTEGER NOT NULL DEFAULT 0` (both engines support it; the PK stays `seq`, the replace is scoped by `host`); players: Postgres `ALTER TABLE players ALTER COLUMN player_id ADD GENERATED BY DEFAULT AS IDENTITY` + `setval` to `MAX+1`; SQLite `INTEGER PRIMARY KEY` already assigns. Migrations are immutable: 0001's `-- R2:` comment stays, 0003's header says it fulfils it.
- `resolve_player`: `INSERT INTO players (token) VALUES ($1) ON CONFLICT (token) DO NOTHING`, then `SELECT player_id, record …` — no writer mutex, safe across processes.
- `commit_cut` gains `host: u32` in `WorldCut` (default 0): `DELETE FROM in_flight WHERE host = $h` then insert with `host`; `UPDATE world SET world_tick = $t WHERE world_tick < $t`.

- [ ] **Step 1: Failing tests:** `schema::schemas_match` (EXPECTED gains the two tables and `in_flight.host`); `players::concurrent_resolve_assigns_distinct_ids` (two `StoreHandle`s on one Postgres DB resolve 50 new tokens each concurrently → 100 distinct ids, each token stable; SQLite variant with one handle); `cut::in_flight_replace_is_per_host` (commit host 0 rows, then host 1 rows → both present; host 0 commit with none → host 1's remain); `cut::world_tick_never_goes_back`.
- [ ] **Step 2: Run** `just test-one world-store "--test it schema:: players:: cut::"` and `just test-pg "--test it schema:: players:: cut::"` — fail.
- [ ] **Step 3: Implement** migrations + row changes.
- [ ] **Step 4: Run** both commands — pass; then `just test-one server "--test store_world -- --include-ignored"` — pass.
- [ ] **Step 5: Commit** `feat(world-store): owner-epoch tables, per-host in-flight rows, database-assigned player ids`.

### Task 2: Fenced writes, open modes, no advisory lock

**Files:**
- Create: `crates/world-store/src/fence.rs`; Test: `crates/world-store/tests/fence.rs`
- Modify: `crates/world-store/src/open.rs:24-105` (`OpenMode`, drop `LockGuard::Pg`), `crates/world-store/src/lib.rs:29,97-100` (`WORLD_LOCK_KEY` removed; `StoreError::Fenced`, `StoreError::ReadOnly`), `handle.rs:48,94,119` , `rows.rs:411-544`, `thread.rs`, `ctl.rs` (worldctl `export` opens `ReadOnly`)

**Interfaces:**
- `pub enum OpenMode { Writer, ReadOnly }`; `StoreHandle::open(url, dials, mode)`. SQLite `Writer` keeps the `flock`; Postgres `Writer` takes no lock; `ReadOnly` (both) takes none, skips `migrate!`, and refuses unless `_sqlx_migrations`' max version == the build's newest migration (`StoreError::OtherFormat`); every write request on a `ReadOnly` handle → `StoreError::ReadOnly`.
- `pub type SuperRegion = (i32, i32)`; `pub struct Fence { pub owner: u32, pub epochs: BTreeMap<SuperRegion, u64> }`; `pub struct CoordinatorFence { pub epoch: u64 }`.
- `StoreHandle::claim_super_regions(owner: u32, srs: Vec<SuperRegion>) -> Result<Fence, StoreError>` (upsert each row with `owner`, `epoch = epoch + 1` / 1, one transaction, returns the new epochs); `StoreHandle::claim_coordinator() -> Result<CoordinatorFence, StoreError>`.
- `commit_cut(cut: WorldCut, fence: Fence)`: first statement in the transaction locks + checks every super-region touched (cut regions, entity rows' regions, player records' regions) — Postgres `SELECT sx, sz, owner, epoch FROM super_regions WHERE … FOR SHARE`; any row missing / other owner / other epoch → roll back, `StoreError::Fenced { sr }`.
- `load_region(rc, fence: &Fence)` and `load_terrain(rc)` (unfenced read — LOD / halo only read) — the region load checks its super-region in the same read transaction.
- `save_coordinator(state: Vec<u8>, fence: CoordinatorFence) -> Receiver<Result<(), StoreError>>`, `load_coordinator() -> Result<Option<Vec<u8>>, StoreError>`; the fence check is `UPDATE coordinator SET state = $s … WHERE id = 1 AND epoch = $e` → 0 rows = `Fenced`.
- `super_region_of(rc, s)` is in `game::ownership` (Task 3); the store takes `s` from `world.super_region_regions`.

- [ ] **Step 1: Failing tests** (`fence.rs`, both engines where the scenario allows): `fence::stale_owner_commit_is_refused_and_writes_nothing` (claim as 0 → fence A; claim again as 1 → fence B; commit with A → `Fenced`, row counts unchanged; commit with B → ok); `fence::stale_owner_region_load_is_refused`; `fence::stale_coordinator_save_is_refused`; `fence::two_pg_writers_open_without_lock` (Postgres: two `Writer` opens succeed — the old `Locked` test is rewritten for SQLite only); `fence::read_only_refuses_writes_and_unknown_schema`; `fence::concurrent_claim_and_commit_serialise` (Postgres: a commit holding `FOR SHARE` delays a claim; afterwards the old fence is refused).
- [ ] **Step 2: Run** `just test-one world-store "--test it fence::"`, `just test-pg "--test it fence::"` — fail.
- [ ] **Step 3: Implement.** Update `crates/world-store/tests/open.rs:51,76` (SQLite `Locked` stays; the Postgres advisory test becomes the fence test).
- [ ] **Step 4: Run** both + `just test-one world-store "--test it"` + `just test-pg "--test it"` — pass.
- [ ] **Step 5: Commit** `feat(world-store): owner-epoch fencing for cuts, region loads and coordinator saves; read-only handles`.

### Task 3: Ownership policy, claiming at start, self-fencing, exit-save rule

**Files:**
- Create: `crates/game/src/ownership.rs`; Test: `crates/game/tests/ownership.rs` (in `it.rs`), `crates/server/tests/store_world.rs` (new cases)
- Modify: `crates/game/src/lib.rs` (re-export), `crates/game/src/world_manager.rs:2943` (`ensure_running`: a non-owned region is unavailable), `crates/server/src/lib.rs` (claim after `open_world`, pass the `Fence`), `crates/server/src/cuts.rs:62-140,149` (fence on commits; `Fenced` → self-fence; exit: skip the last cut when `give_up_unparked` gave any region up), `crates/server/src/region_threads.rs:110-170` (`load_region(rc, &fence)`; `Fenced` → `RegionOutput::LoadFailed("fenced")` + a process-wide fenced flag)

**Interfaces:**
- `pub fn super_region_of(rc: RegionCoords, s: u32) -> (i32, i32)` (floor division); `pub fn rendezvous_owner(sr: (i32, i32), hosts: u32) -> u32` (argmax of `splitmix64(sx as u64 ^ (sz as u64).rotate_left(32) ^ (ordinal as u64) << 48)`, ties to the lower ordinal); `pub enum Ownership { AllInOne, Static { ordinal: u32, hosts: u32 } }` with `owns(&self, rc, s) -> bool` and `from_env() -> Result<Ownership, String>` (`VOXEL_OWNERSHIP`, `VOXEL_HOST_ORDINAL`, `VOXEL_HOSTS`); `pub const DEFAULT_SUPER_REGION_REGIONS: u32 = 16`.
- `WorldManager::set_ownership(Ownership, s: u32)`; non-owned target = `is_unavailable` true (no retry timer) with refusal `REGION_UNAVAILABLE_REFUSAL`.
- Claim set at start: `AllInOne` → every super-region of the map extent (procedural worlds: none persisted — procedural keeps nothing); `Static` → those it owns. Store `world.super_region_regions` ≠ `VOXEL_SUPER_REGION_REGIONS` → refuse start naming both.
- `pub struct Fenced` flag (`Arc<AtomicBool>`) in `server`: set by `cuts.rs` / region threads; the main loop checks it each pass → closes every client with `Refused("this server was replaced; reconnect")`, skips the exit park and cut, exits with code 3 (`EXIT_FENCED`).

- [ ] **Step 1: Failing tests:** `ownership::rendezvous_is_stable_and_balanced` (100×100 super-regions over 4 hosts: each gets 20–30 %; going 4→5 hosts moves ≤ 25 % of them; same input → same owner); `ownership::super_region_floor_div` (negative coords: `(-1, -1)` at s = 16 is in `(-1, -1)`, `(-16, 0)` in `(-1, 0)`, `(-17, 0)` in `(-2, 0)`); `world_manager::non_owned_region_is_unavailable` (Static ordinal 0 of 2: Play into a region owned by 1 → `PlayRefused(RegionUnavailable)`, nothing spawned); `store_world::fenced_host_stops_and_writes_nothing` (slow: server A on a Postgres world; a second claim as the same ordinal; A's next cut → fenced → A exits 3, the store's `world_tick` unchanged since the claim); `store_world::exit_with_given_up_region_skips_exit_cut` (a region whose thread never answers the exit park → no cut committed at exit, store tick == last periodic cut's).
- [ ] **Step 2: Run** `just test-one game "--test it ownership:: world_manager::non_owned"`, `just test-one server "--test store_world -- --include-ignored fenced exit_with_given_up"` — fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests, `just test-threads`, `just smoke` — pass.
- [ ] **Step 5: Commit** `feat(server): AllInOne / Static ownership, super-region claims at start, self-fencing, no exit cut after a given-up region`.

### Task 4: Coordinator messages, core and inline link

**Files:**
- Create: `crates/game/src/coordinator/{mod.rs,msg.rs,link.rs,state.rs}`; Test: `crates/game/tests/coordinator.rs` (in `it.rs`)
- Modify: `crates/game/src/history/handover.rs` (add `reattach_region`), `crates/game/src/history/chain.rs:14,42` (`EmbarkRefusal::WorldUnavailable`, `AdventureRefusal::WorldUnavailable`), `crates/game/src/lib.rs`, `crates/game/src/path_plan.rs:55` only if a message must carry a `RouteMap` (it must not — the host builds its own)

**Interfaces:**
- `msg.rs` (all `Serialize, Deserialize, Debug, Clone`):
  - `pub struct RequestId { pub host: u32, pub boot: u64, pub seq: u64 }` (`boot` = random per host process start).
  - `pub enum WorldRequest { EmbarkOptions, Embark(EmbarkRequest), ReturnTo(SiteId), StartAdventure(SiteId), Legends(LegendsQuery), PersonInfo(PersonId) }`
  - `pub enum WorldReply { EmbarkOptions(EmbarkOptions), Embarked { site: SiteId, region: RegionCoords }, EmbarkRefused(EmbarkRefusal), Returned { site, region }, AdventureStarted { site, region }, AdventureRefused(AdventureRefusal), Legends(LegendsPage), PersonInfo(Option<PersonInfo>) }`
  - `pub enum Claim { Fortress(SiteId), Adventure(SiteId) }`
  - `pub struct RunningRegion { pub rc: RegionCoords, pub incarnation: u64, pub people: Vec<(PersonId, [f32; 3])> }`
  - `pub enum HostMsg { Attach { host: u32, running: Vec<RunningRegion>, online: Vec<(PlayerId, Option<Claim>)> }, RegionStarted { rc, incarnation: u64 }, RegionStopping { rc }, RegionParked { rc, people: Vec<(PersonId, [f32; 3])> }, PersonCrossed { person: PersonId, to: RegionCoords, delivered: bool }, Positions(Vec<(PersonId, [f32; 3])>), DirectiveBounced(Directive), Request { id: RequestId, player: PlayerId, req: WorldRequest }, PlayerOnline { player: PlayerId, claim: Option<Claim> }, PlayerOffline { player: PlayerId }, ReleaseClaim { player: PlayerId } }`
  - `pub enum CoordMsg { View { civs: Vec<CivMarker>, sites: Vec<SiteMarker>, calendar: CalendarTick }, SpawnPeople { rc, incarnation: u64, events: Vec<GameEventKind> }, Directive(Directive), Reply { id: RequestId, reply: WorldReply }, Calendar(CalendarTick), Sites(Vec<SiteMarker>), Notice { region: RegionCoords, line: String } }`
- `mod.rs`: `pub struct Coordinator { history: History, claims: BTreeMap<SiteId, (PlayerId, bool /*online*/)>, player_claims: BTreeMap<PlayerId, Claim>, memo: BTreeMap<RequestId, WorldReply> /* ≤ ANSWERED_MEMO_MAX = 4096, oldest by insertion evicted */, gated: Vec<CoordMsg>, out: Vec<CoordMsg>, dirty_since_save: bool, calendar_anchor, … }`:
  - `new(history: History, now_ms) -> Self`, `from_state(CoordinatorState, grid, now_ms) -> Result<Self, HistoryLoadError>`
  - `handle(&mut self, msg: HostMsg, now_ms: u64)`; `maintain(&mut self, now_ms: u64)` (calendar advance, directives, daily broadcasts, notices — moved from `WorldManager::maintain` / `deliver_directives` / `broadcast_daily` / `route_notices`)
  - `take_out(&mut self) -> Vec<CoordMsg>` (ungated messages, in order); `take_save(&mut self) -> Option<CoordinatorState>` (Some when gated messages wait, the periodic `COORDINATOR_SAVE_SECS` elapsed, or `force`); `saved(&mut self)` moves the gated messages, in order, to `out`. Gated = `Directive`, and `Reply` of `Embarked` / `Returned` / `AdventureStarted`. Everything else is ungated (`ReleaseClaim` only marks the state dirty).
  - `Request` with an id in `memo` → re-sends the memoised reply (no re-execution). `PersonCrossed { delivered }`: calls `history.person_crossed(person, to)` and logs an error if its answer ≠ `delivered` (the host's view of "running" wins; with FIFO they agree). `DirectiveBounced` → `History::defer_directive(Directive)` (new): queued like a directive for a stopping region, emitted again by `region_started` of that region.
  - Validation moved verbatim from `WorldManager::{embark, return_to, start_adventure}` (`world_manager.rs:1117-1240`): history checks + claim exclusivity (`AlreadyHasFortress` / `AlreadyPlaying` from `player_claims`, `InUse` from `claims` with `online`). Host-local checks (`is_unavailable`, region ownership) stay on the host before sending.
- `state.rs`: `pub struct CoordinatorState { pub history: Vec<u8> /* history::persist::encode */, pub claims: Vec<(PlayerId, Claim)>, pub memo: Vec<(RequestId, WorldReply)> }` with `encode / decode` (bincode + a format byte `COORDINATOR_STATE_VERSION = 1`).
- `history::handover`: `pub fn reattach_region(&mut self, rc: RegionCoords, present: &[(PersonId, [f32; 3])]) -> Vec<Directive>` — marks `rc` running; each present living person → `InRegion(rc)` with its abstract home kept, no spawn; a present person dead / unknown / placed elsewhere → `Directive { region: rc, event: DespawnPerson(p) }`.
- `link.rs`: `pub trait WorldLink { fn send(&mut self, msg: HostMsg); fn recv(&mut self) -> Option<CoordMsg>; fn is_up(&self) -> bool; }`; `pub struct InlineWorldLink { coord: Coordinator, in_q, out_q, delay: Option<(u64 seed, u32 max_steps)> }` — `pump(now_ms)` delivers host messages to the core, runs `maintain`, commits saves immediately (calls `saved()`), and releases outputs, each delayed 0..=`max_steps` pumps by the seed (order kept); `InlineSchedule` gains a `coordinator_delay(max_steps)` option.

- [ ] **Step 1: Failing tests** (`coordinator::`): `embark_validates_and_claims_once` (two players embark the same tile → one `Embarked`, one `Occupied`/`TooClose`); `embark_reply_waits_for_save` (`take_out` has no `Reply` until `take_save` + `saved`); `request_replay_answers_from_memo` (same `RequestId` twice → identical reply, one site founded); `return_in_use_only_while_online` (claimant online → `InUse`, after `PlayerOffline` → ok); `directive_bounce_is_redelivered_once` (bounce → after the region restarts the event is delivered exactly once); `reattach_keeps_bodies_and_despawns_the_dead` (state saved with person P alive in R; P dies in the live history after the save; restart from state; `Attach` lists P and Q (Q alive) in R → Q embodied without a spawn event, P gets a `DespawnPerson` directive); `state_round_trips`.
- [ ] **Step 2: Run** `just test-one game "--test it coordinator::"` — fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests and `just test-one game "--test it history::"` — pass.
- [ ] **Step 5: Commit** `feat(game): world coordinator core, host↔coordinator messages, inline link`.

### Task 5: The host's region lifecycle goes through the world link

**Files:**
- Modify: `crates/game/src/world_manager.rs` — fields `history`, `calendar_anchor`, `directive_stamps`, `calendar_day`, `sites_key` (`:622-705`) replaced by `world: Option<Box<dyn WorldLink + Send>>`, `world_view: Option<WorldViewCache>` (civs, sites by id, calendar), `spawn_waiting: BTreeMap<RegionCoords, u64 /*incarnation*/>`; `set_history` (`:893`) → `set_world_link`; `ensure_running` (`:2943-3017`); `handle_region_output` `PeopleParked` / `Stopped` / `PeoplePositions` / `Departures` (`:1506-1690`); `maintain` (`:1759-1820`); `history_region_lost` (`:961`); `deliver_directives` (`:934`)
- Test: existing `crates/game/tests/{world_manager.rs, history_play.rs, handoff_schedule.rs}` + new `crates/game/tests/coordinator_lifecycle.rs` (in `it.rs`)

**Interfaces:**
- Consumes: Task 4's `WorldLink`, `HostMsg`, `CoordMsg`, `InlineWorldLink`.
- `WorldManager::set_world_link(link: Box<dyn WorldLink + Send>)`; `WorldManager::pump_world(&mut self, now_ms)` — drains `link.recv()` and applies: `SpawnPeople` → deliver as `RegionInput::Event`s iff the region runs with that incarnation, else drop it (debug log): that incarnation's `RegionParked` is already ahead of it on the FIFO link and `History::region_parked` restores everyone still placed in the region (downgrade its "returned … not in it" warning to debug for this case only if it fires); `Directive` → today's `deliver_directives` body for one directive, a non-running target → `HostMsg::DirectiveBounced`; `View` / `Calendar` / `Sites` / `Notice` → cache + fan-out (Task 6 adds requests).
- `ensure_running` sends `RegionStarted { rc, incarnation }` where it called `history.region_started`; region stop paths send `RegionStopping` / `RegionParked`; a lost region sends `RegionParked { people: [] }`; `PeoplePositions` → `Positions`; a person `Departure` decides `delivered = target running && !stopping` on the host and sends `PersonCrossed`, routing the body exactly as today's `true` / `false` branches.
- `WorldManager::history()` / `history_mut()` are removed; tests and tools that used them go through `InlineWorldLink::coordinator_mut()`.
- The server's `run()` installs an `InlineWorldLink` for now (Task 9 replaces it per role), so behaviour is unchanged.

- [ ] **Step 1: Failing tests** (`coordinator_lifecycle::`, seeded via `InlineSchedule` with `coordinator_delay(3)`, 32 seeds each, failures name the seed): `people_spawn_once_per_start_under_delay` (start, stop, restart a region while replies lag → each resident has exactly one body while it runs, none after it parks); `spawn_for_stopped_incarnation_is_returned` (the region stops before `SpawnPeople` lands → no body, the history has the people abstract); `person_crossing_under_delay_conserves_people` (people walk across a seam between a running and a stopping region; count bodies + abstract = population every tick); `directive_for_stopping_region_bounces_and_lands_once`.
- [ ] **Step 2: Run** `just test-one game "--test it coordinator_lifecycle::"` — fail.
- [ ] **Step 3: Implement**; port `history_play::` / `world_manager::` tests to `InlineWorldLink` (lockstep: `coordinator_delay(0)` reproduces today's ordering).
- [ ] **Step 4: Run** `just test-one game "--test it coordinator_lifecycle:: history_play:: world_manager::"`, `just test-threads`, `just smoke` — pass.
- [ ] **Step 5: Commit** `refactor(game): WorldManager drives region lifecycle through the world link`.

### Task 6: Player-facing world requests, the outbox and `Attach`

**Files:**
- Modify: `crates/game/src/region_runner.rs:32-88` (`RegionInput::ListPeople`, answered with `RegionOutput::PeoplePositions`), `crates/game/src/world_manager.rs` — `embark` / `retire` / `return_to` / `start_adventure` / `end_adventure` (`:1110-1258`), `RequestEmbarkOptions` / `RequestLegends` / `RequestPersonInfo` handling, connect-time `Civs` / `Sites` / `Calendar` sends, `client_connected` / `client_disconnected` (`:2520-2624`), `send_fortress` (`:1102`), `landing_in` (`:1092`, grid from `SocietyGrid::from_world_map`)
- Test: `crates/game/tests/coordinator_requests.rs` (in `it.rs`); existing `world_manager::` embark / adventure / retire tests

**Interfaces:**
- Host state: `pending_requests: BTreeMap<RequestId, PendingRequest { client: ClientId, player: PlayerId, req: WorldRequest, sent_ms: u64 }>`, `outbox: VecDeque<HostMsg>` (requests and claim messages only — lifecycle messages are not queued while the link is down), `boot: u64` (`set_world_link` takes it), `next_request_seq`.
- Flow: a client world request → host-local pre-checks (as today: already holding, `is_unavailable`, ownership) → `HostMsg::Request` (or into the outbox when `!is_up()`); `WorldReply::Embarked { site, region }` → today's success tail (`move_client`, `fortress_of`, `embark_result`, `send_fortress`, perf event); refusals → the result packet; a reply for a client that has gone → log, and for `Embarked` / `Returned` / `AdventureStarted` send `ReleaseClaim` (the site stays, returnable). `retire` / `end_adventure` → local move as today + `ReleaseClaim`. Connect → `PlayerOnline { claim }`; disconnect / park → `PlayerOffline`.
- `WorldManager::world_link_up(now_ms)` (called when the link reports up after being down, and once at start): asks every running region for its people (new `RegionInput::ListPeople`, answered with the existing `RegionOutput::PeoplePositions`), and once all running regions answered (or after `ATTACH_WAIT_MS = 2000`, sending what it has and logging the missing ones) sends `Attach { running regions with those people, online players + claims }`, then the outbox in order. Lifecycle messages produced while the `Attach` is being gathered are held and sent after it.
- `expire_world_requests(now_ms)` in `maintain`: requests older than `WORLD_REQUEST_TIMEOUT_SECS` → `EmbarkResult(Err(WorldUnavailable))` / `AdventureResult(Err(WorldUnavailable))` / nothing for queries; removed from `pending_requests` and the outbox.

- [ ] **Step 1: Failing tests** (`coordinator_requests::`, seeded delay): `embark_round_trip_moves_player_once`; `embark_reply_after_disconnect_releases_claim`; `requests_queue_while_link_down_and_replay_once` (link down → embark + legends queued → link up → `Attach` first, then both answered once); `request_times_out_with_world_unavailable`; `return_in_use_across_reconnect` (A holds site S online → B's return refused `InUse`; A disconnects → B ok).
- [ ] **Step 2: Run** `just test-one game "--test it coordinator_requests::"` — fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the new tests + `just test-one game "--test it world_manager:: history_play::"`, `just test-threads`, `just smoke` — pass.
- [ ] **Step 5: Commit** `refactor(game): embark, adventure, legends and person info as coordinator requests with an outbox`.

### Task 7: Coordinator runtime and persistence (in-process)

**Files:**
- Create: `crates/server/src/coordinator_rt.rs`; Test: `crates/server/tests/coordinator_rt.rs` (`[[test]]`, slow-tagged)
- Modify: `crates/server/src/world_file.rs` (`ensure_history` → `ensure_coordinator_state`: coordinator row, else migrate from the `history` row, else bootstrap), `crates/server/src/cuts.rs` (`WorldCut.history` no longer filled; `world_manager.rs:2284` sets `None`), `crates/server/src/lib.rs` (`AllInOne` runs the runtime on a `coordinator` thread joined by a crossbeam `ChannelWorldLink`)

**Interfaces:**
- `pub struct ChannelWorldLink { tx: Sender<HostMsg>, rx: Receiver<CoordMsg>, up: Arc<AtomicBool> }` implements `WorldLink` (server crate; the inline link stays for tests).
- `pub struct CoordinatorRuntime { core: Coordinator, store: StoreHandle, fence: CoordinatorFence, pending_save: Option<Receiver<Result<(), StoreError>>> }` — `spawn(store, fence, history_or_state, links: LinkSet) -> JoinHandle`; loop: recv host messages (≤ `COORDINATOR_TICK_MS = 50` timeout) → `core.handle`; `core.maintain(now)`; if no save in flight and `take_save()` is Some → `store.save_coordinator(state.encode(), fence)`; on Ok → `core.saved()`; on `Fenced` → log and exit the process with `EXIT_FENCED`; send `take_out()` to the host(s); `Shutdown` → final forced save, wait ≤ `EXIT_SAVE_TIMEOUT`.
- Migration of an R1 world: no `coordinator` row but a `history` row → decode with the world map's grid, wrap in `CoordinatorState { claims: [], memo: [] }`; the `history` row is no longer written (left as is).

- [ ] **Step 1: Failing tests:** `coordinator_rt::state_survives_restart` (AllInOne server on a SQLite world: embark, orderly exit, restart → the site exists, the player's fortress claim comes back as `Fortress(Some)`); `coordinator_rt::r1_world_migrates_history_row` (a store with only the `history` row starts, then has a `coordinator` row with the same history log length); `coordinator_rt::directive_not_sent_before_save` (a store whose saves are made to fail (test hook `StoreHandle::fail_next_writes(n)`) → no `Directive` reaches the host until a save succeeds).
- [ ] **Step 2: Run** `just test-one server "--test coordinator_rt -- --include-ignored"` — fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests + `just test-one server "--test store_world -- --include-ignored"` + `just test-one server "--test threaded_world -- --include-ignored"` + `just test-threads` — pass.
- [ ] **Step 5: Commit** `feat(server): coordinator runtime — sole writer of history state, durable before visible`.

### Task 8: `svc-link` — the QUIC service link

**Files:**
- Create: `crates/svc-link/{Cargo.toml,src/lib.rs,src/frame.rs,src/cert.rs}`, `crates/svc-link/tests/{it.rs,link.rs}`
- Modify: root `Cargo.toml` (member)

**Interfaces:**
- `pub struct LinkHello { pub token: String, pub role: PeerRole, pub region_blocks: u32, pub chunk_blocks: u32, pub protocol: u32 /* LINK_PROTOCOL_VERSION = 1 */ }`, `pub enum PeerRole { Host { ordinal: u32, boot: u64 }, Login, Coordinator }`.
- `pub enum LinkEvent<In> { Up(LinkHello), Msg(In), Down(String) }`.
- `pub struct LinkListener` — `bind<In, Out>(addr, token: String, cert_hash_file: Option<PathBuf>) -> io::Result<(LinkListener, Receiver<(PeerId, LinkEvent<In>)>, PeerSenders<Out>)>`; generates an rcgen cert, writes its SHA-256 hex to the file; refuses a hello with a wrong token / dials / protocol (closes with a reason, logged both sides); `PeerSenders::send(peer, Out)`, `broadcast(Out)`.
- `pub struct LinkDialer` — `connect<In, Out>(addr, cert_sha256: String, hello: LinkHello) -> (Sender<Out>, Receiver<LinkEvent<In>>)`; reconnects forever with backoff `LINK_RETRY_MIN_MS = 250` doubling to `LINK_RETRY_MAX_MS = 5000`; messages sent while down are dropped (`Down` tells the owner; the host's outbox is the queue, not the link).
- Framing: one bidirectional stream; `u32` big-endian length + `game::encode_packet(&msg)`; `LINK_MAX_FRAME_BYTES = 64 MiB`.
- `pub fn read_cert_hash(env_var: &str, file: &Path) -> io::Result<String>`; `pub fn service_token_from_env() -> Result<String, String>` (`VOXEL_SERVICE_TOKEN`, else `VOXEL_SERVICE_TOKEN_FILE`; refuses an empty token); constant-time compare.

- [ ] **Step 1: Failing tests** (`link::`, localhost, slow-tagged): `round_trip_both_directions`; `wrong_token_refused`; `wrong_cert_hash_refused`; `mismatched_dials_refused`; `dialer_reconnects_after_listener_restart` (`Down` then `Up` again; messages after `Up` arrive in order); `large_frame_ok_and_oversize_refused`.
- [ ] **Step 2: Run** `just test-one svc-link "--test it -- --include-ignored"` — fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — pass.
- [ ] **Step 5: Commit** `feat(svc-link): authenticated QUIC service link with cert pinning and reconnect`.

### Task 9: Process roles — `VOXEL_ROLE=all|host|coordinator`

**Files:**
- Create: `crates/server/src/roles.rs`; Test: `crates/server/tests/roles.rs` (`[[test]]`, slow)
- Modify: `crates/server/src/lib.rs` (`run()` dispatch), `crates/server/src/coordinator_rt.rs` (listener side), `crates/server/src/bin/*` if the binary entry needs the role, `justfile` (`coordinator`, `host`, `dev-r2`, `test-r2`; index-only staging)

**Interfaces:**
- `pub enum Role { All, Host, Coordinator }` — `Role::from_env()` (`VOXEL_ROLE`, default `all`). `Host` and `Coordinator` on a SQLite store → refuse start: "split roles need a Postgres world (VOXEL_WORLD_DB=postgres://…)".
- `Coordinator`: opens the store `Writer`, `claim_coordinator`, loads state, listens on `VOXEL_COORDINATOR_BIND` (default `127.0.0.1:6480`), cert hash file `VOXEL_COORDINATOR_CERT_HASH_FILE` (default `target/dev-r2/coordinator.cert-sha256`); no client edge, no regions.
- `Host`: today's server without the coordinator thread; `QuicWorldLink` (implements `WorldLink` over `LinkDialer`) dials `VOXEL_COORDINATOR_ADDR` (default `127.0.0.1:6480`); claims its super-regions; no history writes.
- `All`: Task 7's in-process wiring (single-player, `just run`, `just server`).
- Recipes: `coordinator` (`VOXEL_ROLE=coordinator`, play dials, `VOXEL_WORLD_DB={{dev_database_url}}`), `host ordinal="0" hosts="1"`, `dev-r2` (pg-up; generates `target/dev-r2/service-token` if missing; starts coordinator, waits for its cert-hash file, starts host 0 with `VOXEL_AUTH=jwt` and login from R2a's `dev-login`; SIGTERM all on exit), `test-r2` (the slow split tests).

- [ ] **Step 1: Failing tests** (`roles::`, Postgres via `MMO_TEST_DATABASE_URL`, slow): `split_world_plays_like_all_in_one` (coordinator + host in-process threads over a real `svc-link` on localhost; a test client connects to the host (token auth for the test), embarks, retires, requests legends — same packets as the AllInOne run of the same script); `sqlite_refuses_split_roles`; `host_survives_coordinator_absence_at_start` (host starts first, simulates, the coordinator comes up later → `Attach` sent, requests answered).
- [ ] **Step 2: Run** `just test-r2` (= `just test-one server "--test roles -- --include-ignored"` with the pg URL) — fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-r2`, `just test-threads`, `just smoke` — pass.
- [ ] **Step 5: Commit** `feat(server): coordinator and host roles over the service link; just coordinator / host / dev-r2`.

### Task 10: Coordinator restart and downtime, end to end

**Files:**
- Test: `crates/server/tests/roles.rs` (new cases); fixes wherever they fail (`coordinator_rt.rs`, `world_manager.rs` outbox / `Attach`)

**Interfaces:**
- Consumes everything above; adds only test hooks: `CoordinatorRuntime::kill()` (drop without saving), `StoreHandle::fail_next_writes(n)` (Task 7).

- [ ] **Step 1: Failing tests** (slow, Postgres): `coordinator_crash_mid_play_conserves_people` (host runs a populated fortress region; kill the coordinator after a save and after further crossings / a birth directive; restart it → after `Attach`, every living person of the site has exactly one body or is abstract, no dead person has a body, no birth applied twice (population count matches the history)); `embark_during_downtime_completes_once` (coordinator down → client embarks → coordinator up within 10 s → exactly one site founded, one `EmbarkResult(Ok)`); `embark_answered_then_crash_is_not_refounded` (the reply's save committed, the coordinator killed before the reply left → after restart the replayed request is answered from the memo, one site); `request_timeout_answers_world_unavailable` (coordinator down > `WORLD_REQUEST_TIMEOUT_SECS` with the timeout overridden small in the test).
- [ ] **Step 2: Run** `just test-r2` — the new cases fail (or pass: then they pin behaviour; record which).
- [ ] **Step 3: Fix** what fails.
- [ ] **Step 4: Run** `just test-r2`, `just test-threads` — pass.
- [ ] **Step 5: Commit** `test(server): coordinator restart and downtime conserve people and answer requests once`.

### Task 11: Login → host push (`SpawnPlayer`, `Kick`, `SessionEnded`)

**Files:**
- Create: `crates/server/src/login_link.rs`, `crates/login/src/host_link.rs`, `crates/login/migrations/{sqlite,pg}/0002_host_link.sql`; Test: `crates/server/tests/login_link.rs` (slow), `crates/login/tests/api.rs` (new cases)
- Modify: `crates/server/src/auth_edge.rs` (pending spawns replace the epoch map), `crates/game/src/world_manager.rs` (`ServerEvent::Supersede { player: PlayerId }` → today's takeover: park the online session, `Refused(LOGGED_IN_ELSEWHERE)`), `crates/login/src/{api.rs,ctl.rs,store.rs}` (`worlds.host_link`, `worlds.host_link_cert`; `world add --link host:port --link-cert <sha256>`; join awaits the ack; logout-everywhere `POST /v1/logout {all: true}` and reset → `Kick`)

**Interfaces:**
- Messages (in `login-api`, serde, wasm-irrelevant but harmless): `pub enum LoginToHost { SpawnPlayer { player_token: String, session_epoch: u64 }, Kick { player_token: String, reason: String } }`, `pub enum HostToLogin { SpawnAck { player_token: String, session_epoch: u64, ok: bool }, SessionEnded { player_token: String, session_epoch: u64 } }`.
- Host: listens with `svc-link` on `VOXEL_HOST_LINK_BIND` (default `127.0.0.1:6490`), cert hash file `VOXEL_HOST_LINK_CERT_HASH_FILE` (default `target/dev-r2/host-0.cert-sha256`), accepts `PeerRole::Login` only. `PendingSpawns { by_token: BTreeMap<String, (u64 epoch, u64 expires_secs)> }`: `SpawnPlayer` → record (replacing an older epoch), resolve the token to a `PlayerId` (existing store), if that player is online → `ServerEvent::Supersede`; ack. `Authenticator::authenticate` (jwt mode) requires `claims.session_epoch == pending.epoch` and not expired, else `Refused("no spawn order for this login — log in again")` / `"superseded by a newer login"`; consumes the pending entry. `Kick` → `Supersede` with the reason as the `Refused` text, and drops any pending spawn. Disconnect of a JWT session → `SessionEnded`.
- Login: `HostLinks` (one `LinkDialer` per world with `host_link`), `join` sends `SpawnPlayer` and awaits `SpawnAck` ≤ `SPAWN_ACK_TIMEOUT_MS`; no ack / link down → 503 `ApiErrorCode::WorldUnavailable` (new code); worlds without `host_link` keep R2a behaviour (no push) so the R2a dev flow still works.
- Login migration 0002: `ALTER TABLE worlds ADD COLUMN host_link TEXT NULL; … host_link_cert TEXT NULL; ALTER TABLE sessions ADD COLUMN ended_at INTEGER NULL`.

- [ ] **Step 1: Failing tests:** `login_link::jwt_without_spawn_order_refused`; `…spawn_then_connect_accepted_once` (second connect with the same ticket refused — consumed); `…second_spawn_parks_first_session_before_new_hello` (client A online; `SpawnPlayer` epoch 2 → A gets `Refused(logged in elsewhere)` and its record is parked once; the epoch-1 ticket is refused; the epoch-2 ticket connects and restores the parked body — one body); `…kick_parks_and_refuses`; `api::join_fails_503_when_host_link_down`; `api::logout_all_sends_kick_to_each_world_with_a_session`.
- [ ] **Step 2: Run** `just test-one server "--test login_link -- --include-ignored"`, `just test-one login "--test it api::"` — fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** both, `just test-r2`, `just test-threads`, `just smoke` — pass.
- [ ] **Step 5: Commit** `feat(login,server): login pushes SpawnPlayer and Kick to the world host`.

### Task 12: Docs, recipes, live check

**Files:**
- Modify: `CLAUDE.md` (index-only: `crates/server` roles, ownership, fencing, coordinator, service link, env vars; world store: owner tables, read-only mode, no advisory lock; the coordinator in `crates/game`), `justfile` (final check of `dev-r2`), `TODO.md` (R2 done; R3 next: host↔host link over `svc-link`, multi-host cuts coordinated by the coordinator, LISTEN/NOTIFY, per-ordinal placement), the design doc §0.5 R2 line → "done" with the plan names.

- [ ] **Step 1:** Write the docs.
- [ ] **Step 2: Run** `just smoke`, `just test-threads`, `just test-r2`, `just test-pg "--test it"`, `just test-one game "--test it coordinator:: coordinator_lifecycle:: coordinator_requests::"` — all pass.
- [ ] **Step 3: Ask the user, then live check** (one client, `adapter=Intel`): `just dev-r2`; log in, join, embark, see people spawn; kill and restart the coordinator (`kill -9`), keep playing, embark again → works once it is back; restart the host → the world and the fortress come back; `just run` (single-player) unchanged.
- [ ] **Step 4: Commit** `docs: R2 role split — coordinator, host, owner epochs, login push`.

---

## Self-review notes

- Spec coverage: login / coordinator / host as separate processes (§0.5 R2) → Tasks 7, 9, 11; `AllInOne` + `Static` (§0.4b) → Task 3; owner epochs in Postgres, fenced writes (§0.4b, §0.7b, §7) → Tasks 1–3; `SpawnPlayer` push and cross-process newest-wins (§0.4d) → Task 11; revocation kick (§0.8 q9) → Task 11; internal call security (§0.8 q10) → Task 8; coordinator downtime (§0.8 q11) → Tasks 6, 10; S = 16 configurable + world-row check (§0.7d) → Task 3; R-B3 read-only store → Task 2; exit-save decision → Task 3; R1 global-row hazards (`in_flight`, `world_tick`, player ids) → Task 1.
- Explicitly not here: host↔host link and crossings between hosts, client multi-connection, multi-host cuts, LISTEN/NOTIFY, per-ordinal placement (R3); `PgClaim` leases, failover, drain (R4); k8s (R5); panel (own plan); GDPR (R5); rate limiting (deferred).
- Known open item: the store thread decodes inline on one current-thread runtime (R1 deferred item). The coordinator runtime and the host each open their own `StoreHandle`, so they no longer share that thread; within a host it is unchanged — measure in R3 before changing.
- Types across tasks: `Fence` / `CoordinatorFence` / `OpenMode` (T2) → T3, T7, T9; `Ownership` / `super_region_of` (T3) → T2's callers, T9; `HostMsg` / `CoordMsg` / `WorldLink` / `InlineWorldLink` / `Coordinator` / `CoordinatorState` (T4) → T5–T7, T9, T10; `LinkListener` / `LinkDialer` / `LinkHello` / `PeerRole` (T8) → T9, T11; `LoginToHost` / `HostToLogin` (T11).
