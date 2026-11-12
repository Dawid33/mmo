# R3 — Many region hosts in one world Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run one world on N region-host processes on one machine (`Static` ownership) so that a host seam changes nothing observable: entities, players, halo, intents and inputs cross between hosts exactly as they cross between region threads; the world is saved as one cut at one world tick across all hosts; the login service places a player on the host that holds them; the client holds one connection per host in its window. It ends with the go / no-go measurement of §0.5.

**Architecture:** Hosts form a full mesh of `svc-link` QUIC links (the higher ordinal dials the lower) and run the existing seam protocol over them: a foreign region is "a region thread that happens to be far away", reached through a per-peer session that numbers reliable messages (transfers, intents and replies, player hand-offs) and drops volatile ones (halo, forwarded input) when the link is down. All hosts tick on one wall-anchored world clock stored in the world row. The coordinator stops being single-host, starts a world cut at a tick T, and commits it with Postgres two-phase commit (each host `PREPARE`s its fenced part; one decision row; `COMMIT PREPARED`) — in-band cut marks on the peer links decide which side of a seam owns a message that is on the wire at T. A host that restarts without a clean leave bumps a world generation, and every other host restarts from the same cut. The client gets a `ConnectionSet`: region traffic by a `region → connection` table, input only to the server-confirmed home.

**Tech Stack:** Rust (dated nightly); quinn 0.11 + rustls (ring) via `svc-link`; sqlx 0.8 runtime queries, Postgres 17 (`PREPARE TRANSACTION`, `LISTEN` / `NOTIFY`); crossbeam between threads, tokio only at network edges; Bevy 0.19.1 client, WebTransport in the browser.

**Spec:** `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0.4, §0.5 R3, §0.9f, §0.10, §8; handoff `docs/superpowers/plans/2026-11-11-r3-and-leftovers-planning-handoff.md`; seam protocol `docs/superpowers/plans/2026-11-06-seam-transparent-regions.md`; predecessor `docs/superpowers/plans/2026-11-07-r2b-role-split.md`. **Prerequisite:** `docs/superpowers/plans/2026-11-12-r2b-cleanups.md` (the `world_manager/` split, the `test-hooks` feature, `pg-test-support`). Command form: `just test-one <crate> "--test <target> -- <filter> <filter>"` — cargo needs the `--` before name filters and before flags such as `--include-ignored`.

## Verified against the code on 2026-11-12 (`cc728a58`) — read before starting

Where the handoff's premises differ from the code:

- **`Routes::admit` already keeps one live peer per ordinal by claim epoch** (commit `58015958`, `crates/server/src/coordinator_rt.rs`). What is left: `best_epoch` lives in memory, so after a coordinator restart the first ordinal-0 process to dial is admitted, stale or not; equal epochs flap. Task 10 checks the epoch against the store instead.
- **`Ownership` is an enum** (`crates/game/src/ownership.rs`: `AllInOne | Static { ordinal, hosts }`), not a trait. It stays an enum here; R4's `PgClaim` is the moment to make it a trait.
- **`just test-r2` starts no processes.** `crates/server/tests/roles.rs` runs the coordinator and a host on threads in one test process over a real `svc-link` on localhost. Task 9's `hosts.rs` does the same for N hosts; the only multi-process run is `just dev-r3`.
- **LOD no longer reads the store.** Since the stud cutover far terrain is the recipe's alone ("nothing ever invalidates a cached tile", `world_manager.rs` near `LOD_CACHE_MAX_ENTRIES`). §8 case 2 therefore has nothing to invalidate for LOD; the only stale cross-host read is **halo seeding** (`stored_regions`, `terrain_cache`, `SeedHaloFromStore`). Task 19 is scoped to that.
- **Terrain is written only inside save cuts**, once per changed region per cut; there is no separate debounced terrain write for §0.9f's NOTIFY to ride on.
- **The world clock is per process**: `WorldClock::starting_at(0, opened.world_tick)` with `started = Instant::now()` (`server/src/lib.rs`, `region_threads.rs`). Two hosts started a second apart disagree by twenty ticks. Nothing in the handoff lists this; Task 1 fixes it and everything else depends on it.
- **`ClientId` is a `usize` from a per-process counter** (`game/src/state.rs`, `server/src/lib.rs` `next_client_id`) and sits inside sim state (`PlayerInput(ClientId, _)`, `PortableRegion.players`). A body that crosses to another host keeps its id, so ids must be world-unique, and 64 bits wide on wasm32 too. `StateHash` already writes `usize` as `u64`, and bincode writes it as 8 bytes, so the change moves no hash and no wire byte.
- **The coordinator core has no per-host state.** `attach()` parks every live region the attaching host did not list and marks every claimant offline (`coordinator/mod.rs`, with a `TODO(R3)`); `SERVED_HOST_ORDINAL` appears in `admit` and in the routing of `FortressRefused`.
- **The login service knows one host per world** (`worlds.host_link`, one `quic` / `webtransport` / `cert_hash`), and a ticket names no host.
- **The client is single-connection throughout**: one `server_game_send` cloned into every predicted region's `RunnerCtx`, one `PredictedClock` steered by any region's `SyncClock`, one `WorkerSession` on wasm with untagged frames. The server has no session without a body, and it silently ignores a subscribe for a region it does not own.
- **Today's behaviour at a foreign region** (all in `world_manager`): a player crossing is refused and clamped back to the seam; any other entity is held in `pending_arrivals` for ever and written into every cut; an intent is refused `OwnerNotRunning`; a reply is lost through the salvage path; halo traffic is dropped; `move_client` into a foreign region parks the record locally, whose foreign `record.region` then makes every cut fail the fence check as a "stray write" (`fence.rs` `touched_super_regions`, `claims.rs` `note_refusal`) — "saving is stuck" for ever instead of fencing.
- The file line numbers quoted in `TODO.md` have drifted; tasks below name functions.

## Decisions settled in planning

Each one gives the trade-off first. The ones that are the user's to overrule are repeated under "To confirm with the user".

1. **Host↔host topology: a direct full mesh, not through the coordinator.** A relay through the coordinator would need one link per host and no discovery, but it puts a singleton on the path of every crossing: its restart would freeze every host seam, against R2b's rule that hosts keep simulating while it is down, and it adds a hop to the latency the go / no-go measures. Rendezvous hashing scatters super-regions, so every host borders every other; "mesh among neighbours" is the full mesh. One link per pair: the higher ordinal dials the lower. Hosts find each other through a `hosts` table in the world database (address and certificate pin written at start) — no address lists in the environment, and R5's per-pod DNS names drop into the same column.
2. **One world clock, anchored to wall time in the world row.** The alternative — the coordinator hands out the tick — makes a host unable to start regions without the coordinator. The anchor `(unix_ms, tick)` is written once by the first split-role process; every host and the coordinator derive the tick from it. A cold start after downtime either jumps the tick (legal: regions already handle clock jumps, seam plan Task 1.2) or is re-anchored by `worldctl clock-reset`, which `just dev-r3` runs before launching. Hosts on different machines need synchronised clocks; on one machine (R3) the clock is shared. R5 must add an NTP requirement or a link-measured offset.
3. **`ClientId` becomes a world-unique session id** (`u64`): `(player_id << 24) | (session_epoch & 0xFF_FFFF)` for a login session, today's counter for a local one. The alternative — each host renames an arriving body to its own id — would make the client's predicted transfer carry a different id than the server's, a guaranteed misprediction at every host flip. Hosts > 1 require `VOXEL_AUTH=jwt` with the login link, so ids of the two kinds never meet.
4. **Peer messages come in two classes.** *Reliable* (entity transfers, intents and replies, player hand-offs and moves, relayed login orders, cut marks): numbered per directed link, acknowledged cumulatively, resent after a reconnect from the receiver's `received_through`, and kept while the peer is away — conservation beats latency. *Volatile* (halo deltas, halo entities, running-region sets, forwarded inputs): dropped while the link is down; a link coming up re-sends the running set and re-seeds halos, a link going down is `HaloSourceGone` for every region of that peer. An inbound peer message re-enters the same routing function a local region's output goes through, so a foreign region obeys the same wake policy.
5. **While a peer is unreachable** a player cannot walk into its regions (clamped back at the seam, today's behaviour for an unavailable region), new intents to it are refused locally and salvaged, and everything already owed to it waits in the reliable queue. That is not the "static border" §0.9 dropped: the wall exists only while the neighbour host is down.
6. **Multi-host cut: same-tick snapshots + in-band marks + Postgres two-phase commit.**
   - *What must be atomic.* Every host snapshots at the same world tick T, so the union of their parts is a consistent world whatever the commit order. The danger is a mixed store — host A's part of cut K+1 committed, host B's not — because an entity that crossed between the two ticks is then stored twice or not at all.
   - *Options.* (a) Version every row by cut id and flip a "complete" pointer: correct, but a new schema for regions, chunks, entities, players and in-flight rows, plus garbage collection. (b) Ship every part to the coordinator and commit once: one transaction, but all region data flows through a singleton and its fence is no longer the host's own. (c) **Chosen:** each host runs today's `commit_cut` transaction unchanged, fenced on its own epochs, but ends it with `PREPARE TRANSACTION 'mmo-cut-<cut>-<ordinal>'`; the coordinator, once every participant reports prepared, inserts one row in `cut_decisions` and issues `COMMIT PREPARED` for each part. The database holds the staged parts; no schema is versioned.
   - *Who owns a message on the wire at T.* Each host, once all its regions have answered the cut, sends `CutMark` on every peer link, in band. Everything a host received from a peer **before** that peer's mark is the receiver's (in a region snapshot if applied by T, else an in-flight row of the receiver); everything after belongs to the next cut (it departed at or after T, so it is still in the sender's snapshot). A host's part is complete only when it holds every participant's mark — "wait, never guess". A link that is down during a cut therefore aborts that cut; the next one succeeds.
   - *Crash proof.* The decision row is the single commit point (first writer wins on its primary key; anyone may write "abort" for an undecided cut, only the coordinator writes "commit"). See the table under Task 14.
   - *Costs accepted.* Postgres needs `max_prepared_transactions > 0` (dev compose changes; R5's CNPG cluster must set it). With more than one host there are **no cuts while the coordinator is down** — the loss window grows by its downtime. With one host (`all`, `dev-r2`) the one-phase path is unchanged.
7. **A host that restarts without a clean leave rolls the whole world back to the last cut.** A crashed host resumes from the last committed cut while its peers have run on; everything that crossed since is duplicated or lost. R4 is where one host fails over alone. Until then the cheap, conserving rule: `world.generation` is bumped by a host that boots dirty; a host that sees a higher generation (on a peer hello, at the coordinator, or when preparing a cut) exits with `EXIT_WORLD_ROLLBACK` (5) without a cut and is restarted by its launcher. Every player loses at most one save interval. A clean leave (a cut in which the leaving host parks) keeps the generation, and transfers owed to it wait in its peers' queues until it returns.
8. **How the client learns about foreign hosts: a redirect that carries a host-vouched ticket.** Sending the client back to the login service for each new host would put login on the crossing path (the spec forbids it) and the join JWT lives 300 s. Proxying foreign regions through the home host would double their traffic and hide the latency the client must predict against. So: the home host answers a subscribe for a foreign region with `RegionElsewhere { region, endpoint, ticket }`, and tells the owner over the peer link to expect that ticket (`ObserverOrder`). The owner admits an **observer** session — subscriptions only, no body. When the body arrives, that session becomes the home.
9. **Input goes to the server-confirmed home only.** The client switches its input connection when the new host's `PlayerRegion` arrives, not when it predicts the flip; the old host forwards the few inputs that reach it after the departure, exactly as a region thread forwards them today (`RegionOutput::Forward`). No host ever holds inputs for a body it does not have.
10. **Placement goes through one entry host, not the coordinator.** Login keeps one link per world (the entry host, ordinal 0). The entry host decides where the player is, in this order: online or parked here → here; "I handed this player to host B" (an in-memory pointer every host keeps for players that left it) → B; otherwise the owner of the stored record's region; no record → the owner of the spawn region. It relays the spawn order over the peer link and returns the final host's client address in `SpawnAck`, which login puts in the ticket. The stored record alone would be wrong for up to a save interval after a crossing — the pointer covers that, and decision 7 guarantees pointers and store never disagree after a restart. The coordinator stays off the login path, so logins work while it is down.
11. **NOTIFY is the primary mechanism (the user's decision, §0.9), one notification per world cut, with a 30 s poll as the backstop.** A prepared transaction cannot contain `NOTIFY`, and §0.9f keeps it off the commit path anyway, so the notification is its own tiny transaction after the cut is decided. `VOXEL_TERRAIN_NOTIFY=poll` runs on the timer alone.
12. **Native first, browser inside R3 but after the go / no-go.** The measurement runs on the native client (`shuttle-probe`); the browser work (Task 23) is tagging frames with a connection id and opening N WebTransport sessions on the page — mechanical once the native routing exists, and worthless if the go / no-go sends the seam back for repair.
13. **Go / no-go threshold.** The spec says the corrections "match". As a number: at each of 1, 2 and 5 ms injected one-way delay, the p95 of `correction_max` and of `corrections` per crossing at a host flip is at most `GO_NOGO_RATIO = 1.25` × the same p95 at a thread flip measured in the same run, plus 0.05 units; and conservation holds (below). A miss means fixing the seam — there is no fallback.

## To confirm with the user (defaults above are planned; none blocks the first tasks)

- Decision 7: a host crash costs **every** player up to one save interval until R4.
- Decision 6: no saves while the coordinator is down when hosts > 1; Postgres must allow prepared transactions.
- Decision 12: browser multi-connection as the last feature task of R3 (or cut it to a follow-up).
- Decision 13: the 1.25 ratio.

## Global Constraints

- Work on `develop` in the main checkout. **Never create a git worktree** (CLAUDE.md), including subagents. The tree holds the user's uncommitted work: `git add` only a task's own files, never `git add -A`; `CLAUDE.md` / `justfile` / `TODO.md` hunks via index-only staging (HEAD copy + hunk → `git update-index --cacheinfo`). Run `git status` before every commit.
- `game` stays Bevy-free, IO-free, sync, wasm-safe: peer messages, the peer session, the coordinator core and the cut protocol's state machines live in `game`; sockets, stores, threads and the listener live in `server` / `svc-link` / `world-store`. The client never depends on `world-store`, `sqlx`, `svc-link`.
- Every network edge frames with `game::encode_packet` / `decode_packet`. Tokio only at network edges; crossbeam between threads. The only rustls provider is **ring** (`cargo tree -i aws-lc-rs` matches nothing).
- **Conservation** — every entity, item, player and person exactly once across region seams, host seams, saves and both link kinds — is the invariant. Every new message path gets a seeded-jitter test (`game::InlineSchedule`, `peer_schedule::`), and a failure names a replayable seed. **"Wait, never guess":** a host that lacks a fact (a peer's mark, a people list, an acknowledgement) waits or aborts; it never assumes.
- No region ever waits for another region, on any host. A late peer message is applied at `max(stamp + 1, the receiver's next tick)`.
- **No store write skips the fence.** Every new write names its fence (a host's `Fence`, the `CoordinatorFence`, or — for a decision row written by a host — first-writer-wins on the primary key).
- Never weaken the server state-hash check; rollback correctness bar unchanged. Sim rules: ordered containers only, `libm` for transcendentals, named constants for tunables. Run `just determinism-wasm` after Task 2 and after any task that changes a hashed or serialized sim type.
- No compile-time database; both engines' migrations stay column-for-column with `schemas_match`; migrations are immutable once committed (each task that needs schema adds its own numbered file). Postgres halves run via `just test-pg`.
- Appending is the only allowed change to a wire enum that an older peer might read (`ServerPacket`, `ClientPacket`, `Credential`): new variants go last. Service-link enums change freely, with `LINK_PROTOCOL_VERSION` bumped.
- Secrets never committed; dev values under git-ignored `target/dev-r3/`.
- Single-player and one-host runs (`just run`, `just server`, `just dev-r2`) keep their behaviour: no peer link, one-phase cuts, local client ids.
- Verification: `just test-one <crate> "<args>"` for the touched tests; `just smoke` before each commit; **`just test-threads` after every task touching the manager, region threads, handoff or the peer path**; `just test-pg` for store work; `just test-r2` and `just test-r3` after coordinator, link or cut tasks. Never whole suites. Client test binaries are a slow opt-in: **ask first** (Tasks 17, 18, 23 need them). Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`. A new test file that touches process-global state gets its own `[[test]]` target.
- **Ask before** any live run (one client, `adapter=Intel`), any benchmark, and the go / no-go measurement itself.
- Keep the justfile in sync in the task that changes a command.

## Review Focus

1. **A player stops on a host seam, or turns back within two ticks** (A→B→A before the first hand-off is acknowledged). Expected: one body, one home, no rubber band, the claim still theirs — Task 7 `ping_pong_across_host_seam`.
2. **The client cannot reach the new host although the hosts can reach each other** (a firewalled port, a wrong advertised address). Expected: the body is parked on the new host after `HANDOFF_SESSION_GRACE_MS`, exactly once, and the next login lands there — Task 7 `handoff_without_session_parks_after_grace`, Task 15 `rejoin_within_a_cut_follows_departed_pointer`.
3. **One host is killed while the others run.** Expected: every host restarts once from the same cut, no restart loop, clients are told "the world is restarting; reconnect", nothing is duplicated — Task 14 `dirty_restart_rolls_every_host_back_once`.
4. **Postgres restarts, or the coordinator dies, in the middle of a cut.** Expected: the cut is either wholly visible or wholly absent after recovery, no prepared transaction is left behind, hosts keep simulating — Task 12 `resolve_*`, Task 14 `coordinator_crash_between_commit_prepareds`.
5. **Nothing but one process is running** (`just run`, `just server`, `just dev-r2`). Expected: identical behaviour and the existing suites green — checked at Tasks 1, 2, 5, 13 by running `store_world`, `threaded_world`, `roles` and `test-threads`.

---

## File structure

```
crates/world-store/migrations/{sqlite,pg}/0006_world_clock.sql      NEW  T1
crates/world-store/migrations/{sqlite,pg}/0007_hosts.sql            NEW  T3
crates/world-store/migrations/{sqlite,pg}/0008_cut_decisions.sql    NEW  T12
crates/world-store/src/{rows.rs,handle.rs,thread.rs,fence.rs,ctl.rs}  MOD
crates/world-store/src/two_phase.rs      NEW  prepare / decide / resolve (T12)
crates/world-store/src/listen.rs         NEW  LISTEN world_cut + poll (T19)
crates/game/src/world_clock.rs           MOD  ClockAnchor (T1)
crates/game/src/state.rs, player.rs      MOD  ClientId = u64, session_client_id (T2)
crates/game/src/peer/                    NEW  T4
  mod.rs      re-exports
  msg.rs      HostEndpoint, PeerHello, PlayerHandoff, Reliable, Volatile, PeerMsg
  session.rs  PeerSession: numbering, acks, resend, absent-peer queue
  link.rs     PeerLink trait, InlinePeerLink (seeded delay, flaps)
crates/game/src/world_manager/peers.rs   NEW  the manager's peer half (T5–T8)
crates/game/src/world_manager/world_cut.rs  NEW  coordinated cut, marks (T13, T14)
crates/game/src/world_manager/{mod.rs,world_link.rs}   MOD
crates/game/src/coordinator/{mod.rs,msg.rs,link.rs}    MOD  per-host core, cuts, memo (T10, T11, T13)
crates/game/src/protocol.rs              MOD  Host, RegionElsewhere, ObserverTicket (T7)
crates/game/src/ownership.rs             MOD  owner_of (T5)
crates/server/src/peer_link.rs           NEW  QuicPeerLink, discovery, delay knob (T9)
crates/server/src/{lib.rs,claims.rs,cuts.rs,coordinator_rt.rs,roles.rs,login_link.rs,auth_edge.rs,player_store.rs,webtransport.rs}  MOD
crates/login-api/src/lib.rs, crates/login/src/{host_link.rs,api.rs}  MOD  SpawnAck endpoint (T15)
crates/client/src/connections.rs         NEW  ConnectionSet, Outgoing, routing (T17)
crates/client/src/{instance.rs,netcode.rs,regions/runner.rs,regions/clock.rs,harness.rs,renderer/autopilot.rs}  MOD
crates/client/src/{netcode_web.rs,sim_driver.rs,web_worker/sim.rs}   MOD  T23
crates/game/tests/{peer_session.rs,peer_schedule.rs,common/multi_rig.rs}  NEW
crates/server/tests/{hosts.rs,observer.rs}  NEW ([[test]] targets)
deploy/dev/compose.yml                   MOD  max_prepared_transactions (T12)
justfile, scripts/crossing_summary.py, CLAUDE.md, TODO.md   MOD
```

---

## Phase A — foundations

### Task 1: One world clock for every host

**Files:**
- Create: `crates/world-store/migrations/{sqlite,pg}/0006_world_clock.sql`
- Modify: `crates/game/src/world_clock.rs`, `crates/world-store/src/{rows.rs,handle.rs,thread.rs,ctl.rs}`, `crates/world-store/tests/schema.rs` (`EXPECTED`), `crates/server/src/lib.rs` (where `WorldClock::starting_at(0, opened.world_tick)` is built), `crates/server/src/region_threads.rs` (`ThreadSpawner`'s `started` / `clock`), `crates/server/src/roles.rs` (`run_coordinator`)
- Test: `crates/game/tests/` (new module `world_clock` in `it.rs`), `crates/world-store/tests/` (module `clock`), `crates/server/tests/store_world.rs`

**Interfaces:**
- Schema: `ALTER TABLE world ADD COLUMN clock_anchor_unix_ms BIGINT NULL; ALTER TABLE world ADD COLUMN clock_anchor_tick BIGINT NULL;`
- `game`: `pub struct ClockAnchor { pub unix_ms: u64, pub tick: Tick }` with `pub fn tick_at_unix(&self, unix_ms: u64) -> Tick` (`tick + (unix_ms - self.unix_ms) / TICK_RATE`, saturating at `tick` for an earlier time); `impl WorldClock { pub fn from_anchor(anchor: ClockAnchor, now_unix_ms: u64) -> (WorldClock, u64) }` — returns `WorldClock::starting_at(0, anchor.tick_at_unix(now_unix_ms))` and the phase `(now_unix_ms - anchor.unix_ms) % TICK_RATE` in ms by which the caller back-dates its `started` instant, so tick edges fall at the same wall instants in every process.
- `StoreHandle`: `pub fn ensure_clock_anchor(&self, proposed: ClockAnchor) -> Result<ClockAnchor, StoreError>` (`UPDATE world SET … WHERE id = 1 AND clock_anchor_unix_ms IS NULL`, then read: first writer wins); `pub fn clear_clock_anchor(&self) -> Result<(), StoreError>`. `worldctl clock-reset <url>` calls the second.
- Server: roles `host` and `coordinator` call `ensure_clock_anchor(ClockAnchor { unix_ms: now, tick: opened.world_tick })`, refuse to start when `anchor.tick_at_unix(now) < opened.world_tick` ("the world clock is behind the saved world tick; run `worldctl clock-reset`"), and build their clock with `from_anchor`. Role `all` calls `clear_clock_anchor` (Postgres only) and keeps today's clock.
- Produces for later tasks: every host's `spawner.world_tick()` and the coordinator's `world_tick_now()` agree to within one tick.

- [ ] **Step 1: Failing tests:** `world_clock::anchored_clocks_agree` (two `from_anchor` calls 1,234 ms apart: for 100 sampled wall instants the two clocks, each fed its own elapsed time plus its phase, return the same tick); `world_clock::anchor_before_now_never_goes_back`; `clock::first_anchor_wins` (two `ensure_clock_anchor` with different proposals → both return the first; after `clear_clock_anchor` the next proposal is stored; both engines); `schema::schemas_match`; `store_world::split_start_refuses_anchor_behind_world_tick` (slow, Postgres).
- [ ] **Step 2: Run** `just test-one game "--test it -- world_clock::"`, `just test-one world-store "--test it -- clock:: schema::"`, `just test-pg "--test it -- clock:: schema::"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-one server "--test store_world -- --include-ignored split_start"` (with `MMO_TEST_DATABASE_URL`), `just test-r2`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat: one wall-anchored world clock in the world row for split roles`.

### Task 2: `ClientId` is a world-unique session id

**Files:**
- Modify: `crates/game/src/state.rs` (`pub type ClientId = u64;`), `crates/game/src/player.rs`, every site the compiler then names (58 files mention `ClientId`; the fixes are `usize` ↔ `u64` conversions only), `crates/server/src/lib.rs` and `crates/server/src/webtransport.rs` (the `next_client_id` counter and the accept paths), `crates/server/src/player_store.rs` (`resolve_session`)
- Test: `crates/game/tests/player.rs`, `crates/game/tests/packet_codec.rs`, `crates/game/tests/wasm_determinism.rs`, `crates/server/tests/login_link.rs`

**Interfaces:**
- `pub const SESSION_EPOCH_BITS: u32 = 24;` `pub fn session_client_id(player: PlayerId, session_epoch: u64) -> ClientId` = `(player.0 << SESSION_EPOCH_BITS) | (session_epoch & 0xFF_FFFF)`; panics in debug if `player.0 >= 1 << 40`.
- The edge allocates the id **after** authentication: a `Credential::Jwt` session gets `session_client_id(player, claims.session_epoch)`; a `Credential::Local` session keeps the process counter (now `AtomicU64`).
- Produces: the same player in the same login session has the same `ClientId` on every host; two sessions of one player differ.

- [ ] **Step 1: Failing tests:** `player::session_client_id_is_unique_per_join` (same player, epochs 1 and 2 → different ids; players 1 and 2, same epoch → different; epoch `1 << 24` wraps onto epoch 0 of the same player only); `packet_codec::client_id_above_u32_round_trips` (`ServerPacket::PlayerRegion(None, 1 << 40)`); the same assertion added to `wasm_determinism.rs`; `login_link::jwt_session_gets_session_client_id` (the `ClientConnected` event's id equals `session_client_id(player, epoch)`).
- [ ] **Step 2: Run** `just test-one game "--test it -- player::session_client_id packet_codec::client_id"` — FAIL (does not compile).
- [ ] **Step 3: Implement.** No behaviour change beyond the id's value.
- [ ] **Step 4: Run** the Step 2 command, `just test-one server "--test login_link -- --include-ignored"`, `just determinism-wasm`, `just test-one game "--test it -- state_hash:: physics_goldens::"` (no golden changes — `StateHash` writes `usize` as `u64` already), `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `refactor: ClientId is a 64-bit session id, world-unique for login sessions`.

### Task 3: Host registry and the world generation

**Files:**
- Create: `crates/world-store/migrations/{sqlite,pg}/0007_hosts.sql`
- Modify: `crates/world-store/src/{rows.rs,handle.rs,thread.rs,lib.rs}`, `crates/world-store/tests/schema.rs`, `crates/server/src/claims.rs`, `crates/server/src/lib.rs` (after the claim; the main loop's exit check), `crates/server/src/cuts.rs` (`exit_world`)
- Test: `crates/world-store/tests/` (module `hosts`), unit tests in `claims.rs`

**Interfaces:**
- Schema: `ALTER TABLE world ADD COLUMN generation BIGINT NOT NULL DEFAULT 0;` `CREATE TABLE hosts (ordinal BIGINT PRIMARY KEY, boot BIGINT NOT NULL, generation BIGINT NOT NULL, clean BIGINT NOT NULL, peer_addr TEXT NULL, peer_cert TEXT NULL, quic TEXT NULL, webtransport TEXT NULL, wt_cert_hash TEXT NULL, updated_utc TEXT NOT NULL);`
- `pub struct HostRow { pub ordinal: u32, pub boot: u64, pub generation: u64, pub clean: bool, pub peer_addr: Option<String>, pub peer_cert: Option<String>, pub quic: Option<String>, pub webtransport: Option<String>, pub wt_cert_hash: Option<String> }`
- `pub struct HostBoot { pub generation: u64, pub bumped: bool }`
- `StoreHandle::register_host(&self, row: HostRow, fence: &Fence) -> Result<HostBoot, StoreError>` — one transaction, fence-checked, `world` row locked: **if** a row for the ordinal exists, is not clean, and its generation equals `world.generation` → `world.generation += 1` (`bumped`); the row is upserted with the new boot, `generation = world.generation`, `clean = 0`. The `generation` and `clean` fields of the argument are ignored.
- `StoreHandle::hosts(&self) -> Result<Vec<HostRow>, StoreError>`; `StoreHandle::world_generation(&self) -> Result<u64, StoreError>`; `StoreError::StaleGeneration { stored: u64, mine: u64 }`.
- Server: `pub const EXIT_WORLD_ROLLBACK: i32 = 5;` `pub const ROLLBACK_REFUSAL: &str = "the world is restarting; reconnect";` `pub struct WorldGeneration` (the host's generation plus an `Arc<AtomicBool>` flag) with `pub fn mine(&self) -> u64` and `pub fn note(&self, seen: u64)` — `seen > mine` sets the flag. The main loop treats the flag like `Fenced`: close clients with `ROLLBACK_REFUSAL`, no park, no cut, `ServerExit::WorldRolledBack` → exit 5. Role `host` calls `register_host` right after its claim; role `all` does not.
- Why this converges: after A crashes, A's reboot bumps g → g+1; each peer sees g+1, exits, reboots dirty with its row at g ≠ g+1, so it adopts without bumping.

- [ ] **Step 1: Failing tests:** `hosts::first_boot_does_not_bump`; `hosts::dirty_boot_bumps_once_and_peers_adopt` (A registers, A registers again → generation 1, `bumped`; B, whose row says 0, registers → generation 1, not bumped; B registers again → 2); `hosts::clean_row_keeps_generation` (set `clean = 1` by SQL; register → not bumped, row is `clean = 0` again); `hosts::register_is_fenced` (a stale fence → `Fenced`, no row change); `schema::schemas_match`; `claims::tests::higher_generation_sets_the_flag_lower_does_not`.
- [ ] **Step 2: Run** `just test-one world-store "--test it -- hosts:: schema::"`, `just test-pg "--test it -- hosts:: schema::"`, `just test-one server "--lib claims::"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-r2`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(world-store,server): host registry and world generation — a dirty host restart is visible to every host`.

## Phase B — the host↔host link

### Task 4: `game::peer` — messages, the session, the inline link

**Files:**
- Create: `crates/game/src/peer/{mod.rs,msg.rs,session.rs,link.rs}`, `crates/game/tests/peer_session.rs` (module in `it.rs`)
- Modify: `crates/game/src/lib.rs` (`pub mod peer; pub use peer::*;`)

**Interfaces (all `Serialize, Deserialize, Debug, Clone` where they travel):**
- `pub struct HostEndpoint { pub ordinal: u32, pub quic: Option<String>, pub webtransport: Option<String>, pub cert_hash: Option<String> }`
- `pub struct PeerHello { pub ordinal: u32, pub boot: u64, pub generation: u64, pub endpoint: HostEndpoint, pub received_through: u64 }` (`received_through`: the last reliable seq this side received from the other in this boot pair; 0 = none)
- `pub struct PlayerHandoff { pub client: ClientId, pub player: PlayerId, pub claim: Option<Claim>, pub login: Option<(String, u64)>, pub park: bool }` (`login` = player token and session epoch)
- `pub enum Reliable { Transfer { target: RegionCoords, stamp: Tick, bundle: Box<EntityBundle>, player: Option<PlayerHandoff> }, Intent(IntentMsg), PlayerMove { handoff: PlayerHandoff, record: Box<PlayerRecord>, target: RegionCoords }, ObserverOrder { client: ClientId, player: PlayerId, nonce: u64, expires_unix_secs: u64 }, SpawnOrder { origin: u32, hops: u8, player_token: String, session_epoch: u64 }, SpawnAck { player_token: String, session_epoch: u64, ok: bool, endpoint: Option<HostEndpoint> }, Kick { player_token: String, session_epoch: u64, reason: String }, KickAck { player_token: String, session_epoch: u64 }, SessionEnded { player_token: String, session_epoch: u64 }, CutMark { cut: CutId } }`
- `pub enum Volatile { Running(Vec<RegionCoords>), RegionUp(RegionCoords), RegionDown(RegionCoords), HaloRequest { from: RegionCoords, of: RegionCoords }, HaloUpdate { to: RegionCoords, update: HaloUpdate, stamp: Tick }, HaloEntity { to: RegionCoords, msg: HaloEntityMsg, stamp: Tick }, Input { client: ClientId, kind: GameEventKind, stamp: Tick } }`
- `pub enum PeerMsg { Hello(PeerHello), Reliable { seq: u64, body: Reliable }, Ack { through: u64 }, Volatile(Volatile) }`
- `pub trait PeerLink { fn send(&mut self, msg: PeerMsg); fn recv(&mut self) -> Option<PeerMsg>; fn is_up(&self) -> bool; fn connection(&self) -> u64 { 0 } fn pump(&mut self, _now_ms: u64) {} }` (`connection` counts link-ups, like `WorldLink::generation`)
- `pub enum PeerEvent { Up(PeerHello), Down, Reliable(Reliable), Volatile(Volatile), HigherGeneration(u64) }`
- `pub struct PeerSession` with:
  - `pub fn new(me: PeerHello, peer: u32) -> Self` (the `received_through` of `me` is filled by the session)
  - `pub fn queue(&mut self, body: Reliable)` — always accepted, kept until acknowledged
  - `pub fn send_volatile(&mut self, v: Volatile) -> bool` — `false` = dropped (not reachable)
  - `pub fn pump(&mut self, link: &mut dyn PeerLink, now_ms: u64) -> Vec<PeerEvent>`
  - `pub fn is_reachable(&self) -> bool` — link up, hellos exchanged, generations equal, not held
  - `pub fn unreceived(&self) -> impl Iterator<Item = &Reliable>` — queued or sent and unacknowledged, in order
  - `pub fn hold(&mut self)` / `pub fn release(&mut self)` — while held, reliable messages queue and are not sent (a leaving peer, Task 14)
  - `pub fn drop_through_mark(&mut self, cut: CutId)` — forgets every entry up to and including this side's `CutMark { cut }`
- Rules the session implements: a reconnect with the same boot pair resends from the peer's `received_through + 1`; a peer hello with a **new boot** and the same generation restarts numbering at 1 and resends everything unacknowledged in order; a peer hello with a **higher generation** yields `HigherGeneration` and delivers nothing; a lower one is ignored (that peer is about to exit). A received reliable message with `seq <= received_through` is dropped; one with a gap ends the connection (`Down`).
- `pub struct InlinePeerLink` — `pub fn pair(seed: u64, max_delay_steps: u32) -> (InlinePeerLink, InlinePeerLink)`; `set_up(bool)` on either end drops what is in transit; each message is delayed `0..=max_delay_steps` pumps by a splitmix roll, order kept.

- [ ] **Step 1: Failing tests** (`peer_session::`, 32 seeds each, a failure prints the seed): `reliable_exactly_once_in_order_across_flaps` (200 numbered payloads, the link dropped at 5 seeded points → the receiver's list is 1..=200, once each); `volatile_is_dropped_while_down_and_not_replayed`; `new_peer_boot_restarts_numbering_and_requeues` ; `higher_generation_is_reported_and_nothing_delivered`; `held_session_queues_and_release_sends_in_order`; `drop_through_mark_forgets_exactly_the_prefix`; `unreceived_lists_queued_and_unacked`.
- [ ] **Step 2: Run** `just test-one game "--test it -- peer_session::"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** it — PASS; `just smoke`.
- [ ] **Step 5: Commit** `feat(game): host↔host peer messages, the reliable/volatile peer session, an inline link`.

### Task 5: The multi-host rig; entity transfers between hosts

**Files:**
- Create: `crates/game/src/world_manager/peers.rs`, `crates/game/tests/common/multi_rig.rs`, `crates/game/tests/peer_schedule.rs` (module in `it.rs`)
- Modify: `crates/game/src/ownership.rs`, `crates/game/src/world_manager/mod.rs` (the `Departures` arm of `region_output`, `is_unavailable`, `restore_in_flight`, `try_complete_cut`, `maintain`), `justfile` (`seam_tests` gains `peer_session:: peer_schedule::`)

**Interfaces:**
- `Ownership::owner_of(&self, rc: RegionCoords, s: u32) -> u32` (`AllInOne` → 0).
- `WorldManager::set_peer(&mut self, ordinal: u32, session: PeerSession, link: Box<dyn PeerLink + Send>)`; `pump_peers(&mut self, now_ms: u64)` (called first in `maintain`); `pub fn owner_of(&self, rc) -> u32`; `pub fn is_foreign(&self, rc) -> bool`; `pub fn reaches(&self, rc) -> bool` (owned, or the owner's session `is_reachable`); `pub fn peer_unreceived(&self) -> usize` (for the `[server]` counters line and the rig).
- `fn accept_arrival(&mut self, source: Option<RegionCoords>, target: RegionCoords, stamp: Tick, bundle: EntityBundle)` — the tail of today's `Departures` arm (wake policy, `enter_in_flight`, `hold_arrival` / `settle_at_seam`, the `Transfer(EntityArrived)` send), extracted so a local departure and an inbound `Reliable::Transfer` share it. An inbound transfer has `in_source_view = false`.
- A departure of a non-player, non-person entity into a foreign region → `queue(Reliable::Transfer { player: None, .. })` on the owner's session, whether or not it is reachable; nothing goes to `pending_arrivals`. A **Person** with a world link crossing into a foreign region is sent the same way; the *receiving* host decides `delivered` and sends `HostMsg::PersonCrossed` (the sender sends none).
- `is_unavailable(rc)` keeps meaning "nobody can be put there now": load failed, or foreign and not reachable. The play / embark / return / adventure paths still refuse every foreign region in this task (`is_foreign`); Task 8 lifts that.
- Cuts (one-phase, until Task 13): `try_complete_cut` writes each `unreceived()` `Transfer` without a player as `InFlightRow::Transfer`; `restore_in_flight` queues a row whose target is foreign on that owner's session. Until Task 13, hosts > 1 is a test-only configuration.
- `multi_rig`: `pub struct MultiRig { pub hosts: Vec<Rig>, … }`; `MultiRig::new(hosts: u32, s: u32, schedule: Option<fn(u64) -> InlineSchedule>, seed: u64, link_delay_steps: u32) -> MultiRig`; `round()` (tick every host, settle, pump links, then `assert_conserved_world()`); `pub fn host_seam(hosts: u32, s: u32) -> (RegionCoords, RegionCoords)` (the adjacent pair nearest the origin with different owners); `pub fn link_down(&mut self, a: u32, b: u32)` / `link_up`. `assert_conserved_world`: every tracked `EntityId` is in exactly one of a running region of one host, one host's `in_flight` / `pending_arrivals`, one session's `unreceived()`, or a link's transit queue.

- [ ] **Step 1: Failing tests** (`peer_schedule::`, 12 seeds × lockstep / `shuffled` / `late` / `jittery`, link delay 0..=3): `slime_crosses_host_seam_once`; `transfer_to_a_parked_foreign_region_waits_in_its_mailbox` (the owner holds it in `pending_arrivals`, not the sender); `link_down_keeps_transfer_and_delivers_after_up`; `lagging_source_host_transfer_conserved`; `double_crossing_at_a_four_host_corner` (`s = 1`, 4 hosts); `one_phase_cut_holds_unreceived_transfer_once_and_restore_requeues_it`; `world_manager::foreign_region_still_refuses_play`.
- [ ] **Step 2: Run** `just test-one game "--test it -- peer_schedule:: world_manager::foreign_region"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 command, `just test-threads`, `just test-one game "--test it -- save_cut:: handoff_manager::"`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(game): entities cross between hosts over the peer session; multi-host seeded rig`.

### Task 6: Halo and intents between hosts; what a dropped link means

**Files:**
- Modify: `crates/game/src/world_manager/peers.rs`, `crates/game/src/world_manager/mod.rs` (`seed_halo`, the `HaloDeltas` / `HaloEntities` arms, `halo_source_lost`, `route_intent`, `ensure_running`, the region stop paths)
- Test: `crates/game/tests/peer_schedule.rs`

**Interfaces:**
- `foreign_running: BTreeSet<RegionCoords>` per manager, fed by `Volatile::Running` (sent on every `PeerEvent::Up`), `RegionUp`, `RegionDown` (sent from `ensure_running` and the stop paths for regions that border a foreign one).
- `seed_halo` for a foreign neighbour `n`: in `foreign_running` → `Volatile::HaloRequest { from: rc, of: n }`, which the owner turns into `RegionInput::HaloRequest(rc)`; the answer comes back as a `HaloDeltas` output targeted at `rc`, i.e. `Volatile::HaloUpdate`. Not running → today's order (terrain cache, store, worldgen).
- `HaloDeltas` / `HaloEntities` whose target is foreign are sent only when the target is in `foreign_running` (halo never wakes a region).
- `PeerEvent::Down`, `HigherGeneration` and `RegionDown(n)` → `halo_source_lost(n)` for the regions concerned and their removal from `foreign_running`. `PeerEvent::Up` → send `Running`, and re-seed the halo of every running region that borders the peer.
- `route_intent`: an intent whose owner is foreign and reachable → `queue(Reliable::Intent)`; foreign and unreachable → `Refused(OwnerNotRunning)` as today. A **reply** to a foreign owner is always queued. An inbound `Reliable::Intent` goes through `route_intent` as if a local region had sent it.
- Cuts (one-phase): an `unreceived()` `Intent` is an `InFlightRow::Intent`.

- [ ] **Step 1: Failing tests** (`peer_schedule::`, seeded): `halo_delta_crosses_host_seam` (a dig at the border on host 0 shows in host 1's halo mirror within 8 rounds); `halo_entity_crosses_host_seam`; `halo_source_gone_when_peer_link_drops_and_reseeds_on_up`; `halo_never_wakes_a_foreign_region`; `deposit_intent_across_hosts_conserves_items`; `intent_to_unreachable_host_is_refused_and_items_return`; `intent_reply_survives_a_link_flap`.
- [ ] **Step 2: Run** `just test-one game "--test it -- peer_schedule::halo peer_schedule::deposit peer_schedule::intent"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it -- peer_schedule:: halo:: intents::"`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(game): halo and intents cross host seams; a dropped peer link is HaloSourceGone`.

### Task 7: A player crosses between hosts

**Files:**
- Modify: `crates/game/src/protocol.rs`, `crates/game/src/player.rs` (`Credential`), `crates/game/src/world_manager/{peers.rs,mod.rs}` (`Session`, `ServerEvent`, the `Departures` arm's player branch, the `Forward` arm, `handle_server_event`'s `GameEventAfterTick` routing, `client_disconnected`, `subscribe`, `maintain`)
- Test: `crates/game/tests/peer_schedule.rs`, `crates/game/tests/packet_codec.rs`

**Interfaces:**
- Protocol (appended last in each enum): `Credential::Observer { client: ClientId, nonce: u64 }`; `pub struct ObserverTicket { pub client: ClientId, pub nonce: u64 }`; `ServerPacket::Host(HostEndpoint)` (the first packet of every session; `all` sends ordinal 0 with no addresses); `ServerPacket::RegionElsewhere { region: RegionId, endpoint: HostEndpoint, ticket: ObserverTicket }`.
- `pub enum SessionKind { Home, Observer }` on `Session`; `ServerEvent::ObserverConnected { client: ClientId, player: PlayerId }`.
- `pub const HANDOFF_SESSION_GRACE_MS: u64 = 5_000;` `pub const OBSERVER_ORDER_TTL_SECS: u64 = 120;`
- `WorldManager::set_endpoints(&mut self, mine: HostEndpoint)`; peers' endpoints come from their `PeerHello`.
- **Leaving (host A):** a player body departs into a foreign region. Unreachable owner → clamped back, as today. Reachable → `queue(Reliable::Transfer { player: Some(handoff), .. })`; A removes `homes[c]`, moves `fortress_of` / `adventurer_of` into the hand-off, sets the session to `Observer`, records `departed[player] = owner`, sends `HostMsg::PlayerOffline`. A body departing while it is being parked sets `handoff.park`.
- **Arriving (host B):** records `player_of`, the claim, `homes[c] = target`, clears `departed[player]`, runs `accept_arrival` (players wake the target), sends `HostMsg::PlayerOnline`. If a session for `c` exists it becomes `Home` and gets `PlayerRegion(Some(target), c)`; otherwise `awaiting_session[c] = now_ms`, and `maintain` treats the player as disconnected after `HANDOFF_SESSION_GRACE_MS` (today's park path). `handoff.park` → `Transfer(ParkPlayer(c), stamp)` follows the arrival.
- `ObserverConnected` → a `Session { kind: Observer }`; if `awaiting_session` has the client → `Home` + `PlayerRegion`.
- **Subscribe to a foreign region** (`subscribe`): reachable owner → `RegionElsewhere { region, endpoint: the owner's, ticket }` where `fn observer_ticket(&mut self, client, owner: u32, now_ms) -> ObserverTicket` mints a random nonce once per `(client, owner)` per TTL and queues `Reliable::ObserverOrder`. Unreachable → nothing, as today.
- **Forwarded input:** a `GameEventAfterTick` from a session with no home whose player is in `departed` → `Volatile::Input` to that host; likewise a `RegionOutput::Forward` for such a client. Inbound `Volatile::Input` → `EventAfterTick` to `homes[client]` if present, else dropped.
- **Disconnect:** an `Observer` session's end drops its subscriptions and nothing else; a `Home` session's end parks, as today.
- An inbound `Reliable::ObserverOrder` is handed to the server through a new manager output `pub fn take_observer_orders(&mut self) -> Vec<(ClientId, PlayerId, u64 /*nonce*/, u64 /*expires*/)>` (Task 16 consumes it).

- [ ] **Step 1: Failing tests** (`peer_schedule::`, seeded as in Task 5; the rig gives each client a session on both hosts): `player_crosses_host_seam_once_and_home_follows` (one body; host 1 sends `PlayerRegion`; host 0's session is `Observer`); `held_key_across_host_seam_settles` (inputs sent to host 0 after the departure reach the body on host 1; one crossing, no oscillation); `ping_pong_across_host_seam` (A→B→A within two ticks: one body, home is A, `departed` is empty on both); `handoff_without_session_parks_after_grace` (no session on host 1: after 5,000 ms the record is in host 1's `parked_players`, exactly once in the world); `disconnect_mid_handoff_parks_once`; `subscribe_to_foreign_region_answers_region_elsewhere_and_orders_the_owner`; `player_cannot_enter_an_unreachable_host`; `packet_codec::new_variants_round_trip_and_old_indices_are_unchanged` (the encoded tag of `ServerPacket::StudLodData` and of `Credential::Jwt` is what it was).
- [ ] **Step 2: Run** `just test-one game "--test it -- peer_schedule::player peer_schedule::held_key peer_schedule::ping_pong peer_schedule::handoff peer_schedule::disconnect peer_schedule::subscribe packet_codec::new_variants"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it -- peer_schedule:: packet_codec:: multi_client::"`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(game): players cross between hosts — hand-off, observer sessions, forwarded input`.

### Task 8: Moves and parks into foreign regions

**Files:**
- Modify: `crates/world-store/src/fence.rs` (`touched_super_regions`), `crates/game/src/world_manager/{mod.rs,world_link.rs,peers.rs}` (`move_client`, `finish_move`, `play_region`, `embark`, `return_to`, `start_adventure`, `world_reply`, `retire`, `end_adventure`, `client_disconnected`, `try_complete_cut`)
- Test: `crates/world-store/tests/fence.rs`, `crates/game/tests/peer_schedule.rs`

**Interfaces:**
- Fence: a player record no longer adds its region's super-region to `touched_super_regions`. The commit is still fenced by every super-region the host claims (`commit_check_set`), so a fenced-out host still cannot write; a host holding a record whose region is foreign can. This removes the "stray write → saving is stuck for ever" failure.
- The play / embark / return / adventure checks refuse a region when `is_unavailable` (which now means unreachable or load-failed) — a reachable foreign region is allowed.
- `move_client(c, target)` with a foreign target: park the body here as today; in `finish_move`, `queue(Reliable::PlayerMove { handoff, record, target })` — also when the owner became unreachable meanwhile (the record is conserved in the queue) — then the bookkeeping of a leaving player (Task 7) and `RegionElsewhere` for `target` to the client.
- Inbound `PlayerMove`: the arriving bookkeeping of Task 7, then `restore_player(c, record)`; a target that failed to load → `parked_players`.
- Cuts: an `unreceived()` `PlayerMove`, or `Transfer` with a player, writes that player's record into `WorldCut.players` (for a transfer through `crossing_record`); nothing is requeued for it on restore — the stored record places the player at the next login.
- `client_disconnected` for a session with no home sends no `ParkPlayer`.

- [ ] **Step 1: Failing tests:** `fence::player_record_in_a_foreign_super_region_commits` and `fence::stale_owner_still_cannot_write_a_player_record` (both engines where the scenario allows); `peer_schedule::embark_into_a_foreign_host_moves_the_player_once` (the body is on host 1, host 1 sent `PlayerRegion`, host 0 sent `RegionElsewhere`); `peer_schedule::retire_to_a_foreign_spawn_region`; `peer_schedule::move_to_unreachable_host_is_refused_before_parking` (`EmbarkResult(Err(RegionUnavailable))`, the claim released, the body untouched); `peer_schedule::peer_drops_after_the_park_record_waits_and_the_cut_holds_it_once`; `peer_schedule::observer_disconnect_parks_nothing`; `peer_schedule::cut_with_a_foreign_record_commits` (no `Fenced`, `cut_failed` never called).
- [ ] **Step 2: Run** `just test-one world-store "--test it -- fence::player_record fence::stale_owner_still"`, `just test-pg "--test it -- fence::"`, `just test-one game "--test it -- peer_schedule::embark peer_schedule::retire peer_schedule::move_to peer_schedule::peer_drops peer_schedule::observer_disconnect peer_schedule::cut_with"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-one game "--test it -- world_manager:: coordinator_requests::"`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `fix(game,world-store): moves and parks into another host's region hand the player over instead of parking a record nobody can save`.

### Task 9: `QuicPeerLink`, discovery, the latency knob

**Files:**
- Create: `crates/server/src/peer_link.rs`, `crates/server/tests/hosts.rs` (`[[test]] name = "hosts"`, `required-features = ["test-hooks"]`)
- Modify: `crates/svc-link/src/lib.rs` (`LINK_PROTOCOL_VERSION = 3`), `crates/server/src/lib.rs` (start-up checks, wiring, `set_endpoints`), `crates/server/src/roles.rs`

**Interfaces:**
- Env: `VOXEL_PEER_BIND` (default `127.0.0.1:{6500 + ordinal}`), `VOXEL_PEER_CERT_HASH_FILE` (default `target/dev-r3/host-{ordinal}.peer.cert-sha256`), `VOXEL_ADVERTISE_QUIC` (default `127.0.0.1:{VOXEL_SERVER_PORT}`), `VOXEL_ADVERTISE_WT`, `VOXEL_PEER_LINK_DELAY_MS` (default 0; a non-zero value logs a WARN at start: "peer link delay injected — measurement only").
- `pub const PEER_DISCOVERY_SECS: u64 = 2;`
- `pub struct PeerLinks` — `pub fn start(ordinal: u32, hosts: u32, boot: u64, generation: u64, endpoint: HostEndpoint, token: String, store: StoreHandle, fence: &Fence) -> io::Result<PeerLinks>`: binds one `svc-link` listener (accepting `PeerRole::Host` only), writes its address and pin into its `hosts` row (through `register_host`), and for every lower ordinal dials the `peer_addr` / `peer_cert` found in `hosts()`, re-reading the rows every `PEER_DISCOVERY_SECS` while that peer is down. `pub fn take(&mut self) -> Vec<(u32, Box<dyn PeerLink + Send>)>` hands one `QuicPeerLink` per other ordinal to the manager.
- `QuicPeerLink` implements `PeerLink`; its `send` holds each frame `VOXEL_PEER_LINK_DELAY_MS` before writing (order kept, a `VecDeque<(Instant, PeerMsg)>` drained in `pump`).
- Start-up refusals when `VOXEL_HOSTS > 1`: role must be `host`, the store Postgres, auth `jwt` with a service token ("several hosts need the login link: ids and placement come from it").
- `PeerEvent::HigherGeneration(g)` → `WorldGeneration::note(g)` (Task 3).

- [ ] **Step 1: Failing tests** (`hosts.rs`, Postgres via `pg_test_support`, real links on `127.0.0.1:0`, all slow-tagged): `two_hosts_find_each_other_through_the_hosts_table_and_exchange_running_sets`; `slime_crosses_between_real_hosts` (two `WorldManager<InlineSpawner>`s as in `roles.rs`); `peer_with_a_wrong_token_is_refused`; `restarted_listener_is_found_again` (the lower host restarts on a new port: the dialer picks the new row up within 5 s); unit `peer_link::tests::delay_knob_holds_frames_in_order` (a fake clock: 3 frames, 5 ms → none readable at 4 ms, all three in order at 5 ms); `several_hosts_refuse_local_auth`.
- [ ] **Step 2: Run** `MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres just test-one server "--features test-hooks --test hosts -- --include-ignored --test-threads=1"` and `just test-one server "--lib peer_link::"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-r2`, `just test-one svc-link "--test it -- --include-ignored"`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(server): QUIC peer links between hosts, discovery through the hosts table, an injected-delay knob`.

## Phase C — the coordinator and the world cut

### Task 10: The coordinator serves every host

**Files:**
- Modify: `crates/game/src/coordinator/{mod.rs,msg.rs,link.rs}`, `crates/game/src/world_manager/world_link.rs` (`apply_world_msg`, `try_finish_attach`, `world_player_online`), `crates/game/src/world_manager/mod.rs` (`restore_player`), `crates/server/src/coordinator_rt.rs` (`Routes`, `admit`, `SERVED_HOST_ORDINAL` removed, `region_target`), `crates/world-store/src/{rows.rs,handle.rs,thread.rs}`, `crates/game/tests/common/multi_rig.rs`
- Test: `crates/game/tests/coordinator.rs`, `crates/game/tests/peer_schedule.rs`, unit tests in `coordinator_rt.rs`, `crates/server/tests/roles.rs`

**Interfaces:**
- `pub enum Target { Host(u32), Every }` moves into `game::coordinator`. `Coordinator::handle(&mut self, host: u32, msg: HostMsg, now_ms: u64)`; `Coordinator::take_out(&mut self) -> Vec<(Target, CoordMsg)>`. New state (not persisted — every `Attach` rebuilds it): `region_host: BTreeMap<RegionCoords, u32>`, `online_on: BTreeMap<PlayerId, u32>`.
- `attach(host, …)`: parks only the live regions whose `region_host` is this host and that it did not list; marks offline only the players whose `online_on` is this host. `PlayerOffline { player }` from host `h` is ignored unless `online_on[player] == h` (a hand-off's `PlayerOnline` from the new host may arrive first). `HostMsg::Attach` gains `generation: u64`.
- `SpawnPeople` / `Directive` go to `Target::Host(region_host[rc])`; a region no host has announced is treated as bounced (`defer_directive`), never broadcast.
- `CoordMsg::ClaimIs { player: PlayerId, claim: Option<Claim> }` **replaces `FortressRefused`**: sent to the reporting host whenever the claim it reports for a player (`PlayerOnline`, or an `Attach`'s `online` entry, after `claim_pending` is resolved) differs from the coordinator's. The host sets `fortress_of` / `adventurer_of` for that player's client to exactly that and sends `ServerPacket::Fortress`. This closes the `restore_player` gap: the host's local "does another local player hold it" filter stays as an early guess, and the coordinator's answer is final.
- `InlineWorldLink::shared(core: Arc<Mutex<Coordinator>>, host: u32) -> InlineWorldLink` for the rig (`MultiRig::with_coordinator(history)`).
- `StoreHandle::owner_claim_epoch(&self, owner: u32) -> Result<Option<u64>, StoreError>` (`SELECT MAX(epoch) FROM super_regions WHERE owner = $1`).
- `Routes::admit(peer, ordinal, epoch, stored_epoch: Option<u64>, generation_ok: bool)`: any ordinal is served; a hello whose `epoch != stored_epoch` is refused ("a stale host process"), which holds across a coordinator restart; one live peer per ordinal, the newer connection replacing the older, as today. The runtime reads `store.world_generation()` at every `Attach` and refuses one whose `generation` is lower (it closes that peer, which is about to exit anyway). The coordinator itself never rolls back: it holds no region state, and R2b already lets the history run ahead of the regions.

- [ ] **Step 1: Failing tests:** `coordinator::attach_from_host_1_keeps_host_0_regions_and_players`; `coordinator::player_offline_from_the_old_host_after_online_on_the_new_is_ignored`; `coordinator::claim_is_answers_a_differing_report` (three cases: the coordinator holds none, holds another site, `claim_pending` with a different grant → the host ends with exactly the coordinator's claim); `coordinator::directive_for_an_unannounced_region_is_deferred_not_broadcast`; `peer_schedule::claim_travels_with_the_player_across_hosts` (after the crossing, `Return` by another player is refused `InUse`; after the first player disconnects on host 1 it succeeds); `coordinator_rt::tests::two_ordinals_are_served` (replaces `only_host_ordinal_zero_is_served`); `roles::stale_epoch_host_is_refused_by_a_restarted_coordinator` (Postgres: claim twice as ordinal 0; a fresh coordinator refuses a hello carrying the first epoch); `roles::second_host_ordinal_is_refused` is rewritten as `roles::two_hosts_attach_and_both_get_their_people`. The existing `roles.rs` helpers pass the real claim epoch instead of 0.
- [ ] **Step 2: Run** `just test-one game "--test it -- coordinator:: peer_schedule::claim_travels"`, `just test-one server "--lib coordinator_rt::"`, `just test-r2` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-one game "--test it -- coordinator_lifecycle:: coordinator_requests:: history_play::"`, `just test-one server "--features test-hooks --test coordinator_rt -- --include-ignored"`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(game,server): the coordinator serves every host — per-host attach, routing by region owner, authoritative claims, admission by stored epoch`.

### Task 11: The answer memo is pruned by acknowledgement, not by count

**Files:**
- Modify: `crates/game/src/coordinator/{mod.rs,msg.rs}`, `crates/game/src/world_manager/world_link.rs` (`world_reply`, `expire_world_requests`, `try_finish_attach`)
- Test: `crates/game/tests/coordinator.rs`, `crates/game/tests/coordinator_requests.rs`

**Interfaces:**
- `HostMsg::Settled { boot: u64, below: u64 }` — "every request of this boot with `seq < below` has been answered and applied here, or was never sent". The host sends it whenever its lowest outstanding sequence (over `pending_requests` and `expired_requests`) rises; `HostMsg::Attach` gains `boot: u64` and `settled_below: u64`.
- Coordinator: `Settled` (and an `Attach`'s watermark) removes memo entries `(host, boot, seq < below)`. An `Attach` from a host with a boot it has not seen removes that host's entries of every other boot (the old process is gone; its claims are what the `Attach`'s `online` list says). `remember` never evicts: above `ANSWERED_MEMO_MAX` (kept at 4096) it logs an error at most once a minute naming the host with the most entries.
- Result: a resent `Request` or `Settle` always finds its answer, so nothing is re-executed and no claim leaks.

- [ ] **Step 1: Failing tests:** `coordinator::memo_keeps_unsettled_answers_past_the_old_cap` (4,097 embark requests answered, none settled; replaying the first returns the memoised `Embarked` and founds no second site); `coordinator::settled_watermark_prunes_exactly_below`; `coordinator::new_boot_attach_drops_the_old_boots_memo`; `coordinator_requests::host_sends_settled_as_requests_complete` (after three round trips the coordinator's memo for that host is empty).
- [ ] **Step 2: Run** `just test-one game "--test it -- coordinator::memo coordinator::settled coordinator::new_boot coordinator_requests::host_sends_settled"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `just test-one game "--test it -- coordinator:: coordinator_requests::"`, `just test-r2`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `fix(game): the coordinator's answer memo is pruned by host acknowledgement, never by count`.

### Task 12: Store — a cut in two phases

**Files:**
- Create: `crates/world-store/migrations/{sqlite,pg}/0008_cut_decisions.sql`, `crates/world-store/src/two_phase.rs`, `crates/world-store/tests/two_phase.rs` (module in `it.rs`)
- Modify: `crates/world-store/src/{rows.rs,handle.rs,thread.rs,lib.rs}`, `crates/world-store/tests/schema.rs`, `deploy/dev/compose.yml`

**Interfaces:**
- Schema: `CREATE TABLE cut_decisions (cut_id BIGINT PRIMARY KEY, tick BIGINT NOT NULL, committed BIGINT NOT NULL, decided_utc TEXT NOT NULL);`
- Compose: the postgres service gains `command: ["postgres", "-c", "max_prepared_transactions=64"]` (the container is recreated by `just pg-down && just pg-up`; the volume keeps the data).
- `pub fn cut_gid(cut: CutId, host: u32) -> String` = `"mmo-cut-{cut}-{host}"`.
- `StoreHandle::two_phase_ready(&self) -> Result<(), StoreError>` — Postgres with `max_prepared_transactions > 0`, else `Unsupported("several hosts need max_prepared_transactions > 0 (Postgres)")`; SQLite → `Unsupported`.
- `StoreHandle::prepare_cut(&self, cut: WorldCut, fence: Fence, generation: u64) -> Receiver<Result<CutCommitted, StoreError>>` — the body of `commit_cut`, on one dedicated connection with an explicit `BEGIN`; first statement `SELECT generation FROM world WHERE id = 1 FOR SHARE` (≠ `generation` → `StaleGeneration`, rolled back); the `UPDATE world SET world_tick` statement is **left out** (the decision writes it, so two parts do not contend on the world row); ends with `PREPARE TRANSACTION '<cut_gid>'`. The `FOR SHARE` locks on the host's `super_regions` rows are held by the prepared transaction, so a claim by another process waits for the decision.
- `StoreHandle::decide_cut(&self, cut: CutId, tick: Tick, commit: bool, hosts: Vec<u32>, leaving: Vec<u32>, fence: CoordinatorFence) -> Result<bool, StoreError>` — transaction 1: check the coordinator epoch (`Fenced { sr: None }` otherwise); `INSERT INTO cut_decisions … ON CONFLICT (cut_id) DO NOTHING`; read the stored decision `d`; if `d`: `UPDATE world SET world_tick = $t WHERE world_tick < $t` and `UPDATE hosts SET clean = 1 WHERE ordinal IN (leaving)`. Then, outside it, for each host `COMMIT PREPARED` (if `d`) or `ROLLBACK PREPARED` — "prepared transaction … does not exist" counts as done. Returns `d`.
- `StoreHandle::abort_cut(&self, cut: CutId, tick: Tick) -> Result<bool, StoreError>` — a host's unfenced "abort unless decided": the same insert with `committed = 0`, returns the stored decision.
- `StoreHandle::resolve_prepared(&self) -> Result<Vec<(CutId, bool)>, StoreError>` — for every gid `LIKE 'mmo-cut-%'` in `pg_prepared_xacts` of this database: a decision row exists → apply it; none → `abort_cut`, then apply what is stored.
- `StoreHandle::cut_decision(&self, cut: CutId) -> Result<Option<bool>, StoreError>`.
- Test hook (`#[doc(hidden)]`, like `fail_next_writes`): `StoreHandle::stop_after_commit_prepared(&self, n: u32)` — `decide_cut` returns `Backend("test stop")` after `n` `COMMIT PREPARED`s.

- [ ] **Step 1: Failing tests** (`just test-pg "--test it -- two_phase::"`, plus one SQLite case): `prepared_part_is_invisible_until_decided`; `commit_decision_applies_every_part_and_the_world_tick`; `abort_decision_leaves_nothing_and_no_prepared_xact`; `first_decision_wins` (`abort_cut` first → a later `decide_cut(commit = true)` returns `false` and rolls the parts back); `resolve_finishes_a_decided_cut_stopped_between_commits` (two parts prepared, `stop_after_commit_prepared(1)`, `decide_cut` errors, `resolve_prepared` → both parts visible, `pg_prepared_xacts` empty); `resolve_aborts_an_undecided_cut`; `fenced_host_cannot_prepare`; `stale_generation_cannot_prepare`; `stale_coordinator_cannot_decide`; `claim_waits_for_the_prepared_part_and_proceeds_after_the_decision`; `leaving_host_is_clean_only_if_committed`; `sqlite_is_not_two_phase_ready`; `schema::schemas_match`.
- [ ] **Step 2: Run** `just pg-down && just pg-up`, then `just test-pg "--test it -- two_phase:: schema::"` and `just test-one world-store "--test it -- two_phase::sqlite schema::"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-pg "--test it -- cut:: fence::"`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(world-store): two-phase save cuts — fenced PREPARE per host, one decision row, recovery sweep`.

### Task 13: The coordinated world cut

**Files:**
- Create: `crates/game/src/world_manager/world_cut.rs`
- Modify: `crates/game/src/coordinator/{mod.rs,msg.rs}`, `crates/game/src/world_manager/{mod.rs,peers.rs,world_link.rs}` (`begin_cut`, `try_complete_cut`, `cut_committed`, `cut_failed`, `try_finish_attach`), `crates/game/src/save_cut.rs` (`PendingCut`), `crates/server/src/{cuts.rs,coordinator_rt.rs,lib.rs}`, `crates/game/tests/common/multi_rig.rs`
- Test: `crates/game/tests/peer_schedule.rs`, `crates/game/tests/coordinator.rs`

**Interfaces:**
- Messages: `CoordMsg::BeginCut { cut: CutId, tick: Tick, hosts: Vec<u32>, leaving: Vec<u32>, generation: u64 }`, `CoordMsg::CutDone { cut: CutId, committed: bool }`, `HostMsg::CutPrepared { cut: CutId }`, `HostMsg::CutFailed { cut: CutId, why: CutFailure }`, `pub enum CutFailure { Missed, Aborted, Store(String) }`; `HostMsg::Attach` gains `pending_cut: Option<CutId>`.
- Constants: `WORLD_CUT_LEAD_TICKS: Tick = 40`, `WORLD_CUT_LEAD_MAX_TICKS: Tick = 640`, `WORLD_CUT_DEADLINE_MS: u64 = 30_000`.
- Coordinator core: `set_world_cuts(every_ms: u64)`, `set_expected_hosts(BTreeSet<u32>)` (the runtime feeds it the ordinals whose `hosts` row is not clean), `set_world_tick(tick)` each pass. In `maintain`: when a period has elapsed, no cut runs, and **every expected host is attached** → `BeginCut` to each with `tick = now + lead`, `cut = CutId((coordinator_epoch << 32) | n)`. All `CutPrepared` in → `take_decision() -> Option<CutDecide { cut, tick, commit, hosts, leaving }>`; any `CutFailed`, a host detaching, or the deadline → a decision with `commit = false`; a `Missed` doubles the lead up to the maximum, a committed cut resets it. `decided(cut, committed)` → `CutDone` to every participant. An `Attach` with `pending_cut` → the runtime looks up `cut_decision` (none → it decides abort) and the core answers `CutDone`.
- Coordinator runtime: executes `CutDecide` with `store.decide_cut(…)`; a `Fenced` result exits as today.
- Host (`world_cut.rs`): `WorldManager::begin_world_cut(&mut self, cut: CutId, tick: Tick, hosts: &[u32], leaving: &[u32]) -> Result<(), CutFailure>` — `Missed` when `tick` is closer than `SAVE_CUT_LEAD_TICKS / 2` or past; otherwise today's `begin_cut` body at the given tick (shared as `begin_cut_at`). When every region has answered: `queue(Reliable::CutMark { cut })` to every participant peer. The cut completes when, in addition, a `CutMark { cut }` has been **received** from every participant peer.
  - A reliable message received from a participant peer **before** its mark is the receiver's (`in_source_view = false`: in the snapshot if applied by T, else a row). After the mark it is `in_source_view = true` (not in this cut).
  - This side's `unreceived()` entries for a **participant** are not written (they precede this side's mark, so they are the peer's); entries for an ordinal that is **not** a participant are written as rows / player records (Tasks 5, 6, 8).
  - A peer session going `Down`, or `HigherGeneration`, while the cut waits → `cut_failed` locally and `HostMsg::CutFailed { why: Aborted }`.
- Server `CutWriter`: with `VOXEL_HOSTS > 1` the periodic host-initiated `begin_cut` is off; a `BeginCut` whose `generation` differs from `WorldGeneration::mine()` is answered `CutFailed { Aborted }` (and a higher one noted); `take_cut()` → `prepare_cut(cut, fence, generation)` → `CutPrepared` or `CutFailed { Store }`; the cut is held until `CutDone` → `cut_committed(id, versions)` or `cut_failed(id)`. With one host (or `all`) nothing changes.
- Rig: `MultiRig` gets an in-memory two-phase store (`parts: BTreeMap<(CutId, u32), WorldCut>`, `decided`), and `pub fn rebuild_from_last_cut(&self) -> MultiRig` (every host restored from its committed part) for the conservation check.

- [ ] **Step 1: Failing tests** (`peer_schedule::cut_`, 12 seeds × 4 schedules × link delay 0..=3): `cut_under_crossings_restores_each_entity_once` (slimes and one player shuttle across the host seam while a cut runs; `rebuild_from_last_cut()` holds every tracked id, and the player's record, exactly once); `transfer_on_the_wire_at_t_is_in_exactly_one_part`; `intent_reply_on_the_wire_at_t_is_in_exactly_one_part`; `cut_with_a_link_down_aborts_and_the_next_cut_commits`; `missed_tick_doubles_the_lead_and_the_retry_commits`; `no_cut_while_an_expected_host_is_absent`; `host_detaching_mid_cut_aborts_it`; `coordinator::reattach_with_a_pending_cut_gets_its_outcome`; `save_cut::one_host_keeps_the_one_phase_path` (no `BeginCut` is ever sent with one host).
- [ ] **Step 2: Run** `just test-one game "--test it -- peer_schedule::cut_ peer_schedule::transfer_on_the_wire peer_schedule::intent_reply_on_the_wire peer_schedule::missed_tick peer_schedule::no_cut peer_schedule::host_detaching coordinator::reattach_with_a_pending_cut save_cut::one_host"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 command, `just test-one game "--test it -- save_cut::"`, `just test-one server "--test store_world -- --include-ignored"`, `just test-r2`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat: one world cut across hosts — the coordinator picks the tick, peers exchange marks, parts are prepared and decided together`.

### Task 14: Leaving cleanly, starting after a crash

**Files:**
- Modify: `crates/game/src/coordinator/{mod.rs,msg.rs}`, `crates/game/src/world_manager/{world_cut.rs,peers.rs}`, `crates/server/src/{cuts.rs,lib.rs,roles.rs,coordinator_rt.rs}`
- Test: `crates/server/tests/hosts.rs`, `crates/game/tests/peer_schedule.rs`

**Interfaces:**
- `HostMsg::LeaveRequest`; `pub const LEAVE_BATCH_MS: u64 = 500;` `pub const LEAVE_CUT_TIMEOUT_SECS: u64 = 30;`
- Coordinator: collects `LeaveRequest`s for `LEAVE_BATCH_MS`, then starts a cut at once with those hosts in `leaving`. After it commits they are no longer expected (their rows are clean) until they register again.
- Host exit with `VOXEL_HOSTS > 1` (`cuts::exit_world`): send `LeaveRequest`; on the `BeginCut` that names this host as leaving, park every region for exit (today's `park_for_exit`; the `Stopped` answers are the cut's answers) and prepare; `CutDone { committed: true }` → exit 0. No `BeginCut` within `LEAVE_CUT_TIMEOUT_SECS`, or an aborted cut → exit 0 without a cut, logging "left without a cut: the world rolls back to the last cut when this host returns".
- Peers of a leaving host: after queuing their own `CutMark` for it they `hold()` that session; on `CutDone { committed: true }` they `drop_through_mark(cut)` (those entries are the leaver's by the cut rule) and keep the session held until a new boot's hello (`release`); on an aborted cut they `release()`.
- Start-up, every split-role process, before any claim or load: `resolve_prepared()`; a host with `VOXEL_HOSTS > 1` also `two_phase_ready()` (fatal otherwise).
- The crash table this task's tests pin (P = a host's part prepared, D = decision row, C = `COMMIT PREPARED`):

| Crash point | State left | Recovery | Result |
|---|---|---|---|
| A host dies before P | other parts prepared, no D | the coordinator's deadline or the host's detach decides abort | cut absent; the dead host reboots dirty → world rollback to the previous cut |
| A host dies after P, before D | all parts prepared | the coordinator decides commit (it has every `CutPrepared`) | cut K whole; the host reboots dirty → every host restarts from K |
| The coordinator dies before D | parts prepared, no D | its restart (or any host's start) runs `resolve_prepared` → abort | cut absent; hosts get `CutDone(false)` on re-attach and keep everything uncommitted |
| The coordinator dies after D, between Cs | D = commit, some parts committed | `resolve_prepared` commits the rest; no process loads a region before it has run | cut K whole |
| Postgres restarts mid-cut | prepared transactions survive a restart | as the rows above | whole or absent |
| A host dies between cuts | nothing staged | reboot dirty → generation + 1 → every host exits 5 and restarts from the last cut | conserved, ≤ one interval lost |

- [ ] **Step 1: Failing tests** (`hosts.rs`, Postgres, real links, hosts as threads that can be dropped without saving; all slow): `host_killed_after_prepare_cut_commits_and_world_restarts_from_it`; `coordinator_killed_before_the_decision_cut_is_absent`; `coordinator_killed_between_commit_prepareds_cut_is_whole` (`stop_after_commit_prepared(1)`; after the restart `pg_prepared_xacts` is empty and both hosts' regions carry the cut's tick); `dirty_restart_rolls_every_host_back_once` (kill host 1 between cuts and restart it: generation is exactly + 1, host 0 ends with `ServerExit::WorldRolledBack` once, restarted hosts stay up for 5 s, every tracked entity is present once); `clean_leave_and_return_keeps_the_generation_and_delivers_waiting_transfers` (host 1 leaves; a slime sent toward it waits in host 0's queue and is in host 0's next cut as a row; host 1 returns → it arrives once); `leave_without_a_coordinator_exits_dirty`; `peer_schedule::held_session_entries_before_the_mark_are_dropped_after_a_committed_leave`.
- [ ] **Step 2: Run** `just test-r3` (the recipe lands in Task 20; until then the Task 9 command) and `just test-one game "--test it -- peer_schedule::held_session"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-r2`, `just test-pg "--test it -- two_phase::"`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(server): leave cuts, start-up recovery of staged cuts, world rollback on a dirty host restart`.

## Phase D — placement and the client

### Task 15: The login service places a player on the host that holds them

**Files:**
- Modify: `crates/login-api/src/lib.rs` (`HostToLogin::SpawnAck`), `crates/login/src/{host_link.rs,api.rs}`, `crates/server/src/login_link.rs`, `crates/game/src/world_manager/peers.rs`, `crates/game/src/world_manager/mod.rs` (`ServerEvent`)
- Test: `crates/server/tests/hosts.rs`, `crates/login/tests/api.rs`, `crates/game/tests/peer_schedule.rs`

**Interfaces:**
- `HostToLogin::SpawnAck { player_token: String, session_epoch: u64, ok: bool, quic: Option<String>, webtransport: Option<String>, cert_hash: Option<String> }`; login's `join` builds the `JoinTicket` from the acknowledgement's addresses when present, else from the world row.
- Env `VOXEL_ENTRY_HOST` (default 0): only that ordinal binds the login link (`VOXEL_HOST_LINK_BIND`).
- `pub const SPAWN_RELAY_MAX_HOPS: u8 = 4;`
- `pub enum Placement { Here, Host(u32) }`; `WorldManager::place_spawn(&self, player: PlayerId, record_region: Option<RegionCoords>) -> Placement` — in order: online, parked or parking here → `Here`; `departed[player]` → `Host`; `record_region` owned here → `Here`, else its owner; no record → the owner of `SPAWN_REGION`.
- `ServerEvent::SpawnOrder { origin: u32, hops: u8, player_token: String, session_epoch: u64, player: PlayerId, record_region: Option<RegionCoords> }` (the server resolves the token and reads the record's region before sending it). `Here` → the host records the pending spawn in its `SpawnOrders` and acknowledges with its own endpoint; `Host(o)` → `Reliable::SpawnOrder` to `o` if reachable and `hops < SPAWN_RELAY_MAX_HOPS`, else an acknowledgement with `ok: false`. `Reliable::SpawnAck` travels back to `origin`, which answers login.
- `Kick`: the entry host applies it and queues `Reliable::Kick` to every peer; it sends `HostToLogin::KickAck` once it and every **reachable** peer have acknowledged (an unreachable peer means no acknowledgement, so login resends — kicks stay durable). `SessionEnded` on another host → `Reliable::SessionEnded` to the entry host → login. **A host reports `SessionEnded` only when the session leaves the world** (its home session parks or disconnects), never when the player is handed to another host: login deletes a session's pending kick on `SessionEnded` (`end_session`, keyed by world, not by host), so a hand-off that reported it would drop a kick for a session that is still live. Test `hosts::handoff_does_not_end_the_login_session` pins it (a kick queued before the crossing still parks the player on the new host).

- [ ] **Step 1: Failing tests:** `peer_schedule::place_spawn_order_of_rules` (the four rules, each with the earlier ones absent); `hosts::join_places_the_player_on_the_host_owning_its_record_region` (the ticket's `quic` is host 1's advertised address; host 1 holds the pending spawn, host 0 none); `hosts::rejoin_within_a_cut_follows_the_departed_pointer` (the player walked 0 → 1 and disconnected; no cut since; the join lands on host 1 and there is one body); `hosts::kick_reaches_a_session_on_a_non_entry_host_and_is_acked_once`; `hosts::spawn_for_an_unreachable_owner_is_refused` (login answers 503 `world_unavailable`); `hosts::session_ended_on_a_non_entry_host_reaches_login`; `api::join_uses_the_acknowledged_endpoint`.
- [ ] **Step 2: Run** `just test-one game "--test it -- peer_schedule::place_spawn"`, `just test-one login "--test it -- api::join_uses"`, `just test-r3` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-one server "--test login_link -- --include-ignored"`, `just test-login-pg`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(login,server): the entry host relays spawn orders and kicks to the host that holds the player`.

### Task 16: Observer sessions at the edge

**Files:**
- Create: `crates/server/tests/observer.rs` (`[[test]] name = "observer"`)
- Modify: `crates/server/src/auth_edge.rs`, `crates/server/src/player_store.rs` (`resolve_session`, `announce_session`), `crates/server/src/lib.rs` and `crates/server/src/webtransport.rs` (accept paths, sinks), `crates/game/src/world_manager/mod.rs` (`handle_server_event`, `maintain`)

**Interfaces:**
- `pub struct ObserverOrders` (`auth_edge.rs`): `pub fn record(&self, client: ClientId, player: PlayerId, nonce: u64, expires_unix_secs: u64)` (fed each pass from `manager.take_observer_orders()`); `pub fn admit(&self, client: ClientId, nonce: u64, now_secs: u64) -> Result<PlayerId, &'static str>` — constant-time nonce compare; valid until it expires, any number of times.
- `Authenticator::admit` for `Credential::Observer` → the connection's id **is** `ticket.client`; the edge sends `ServerEvent::ObserverConnected { client, player }`, never `ClientConnected`. One live sink per `ClientId` per host: a newer connection replaces the older, which is closed with `Refused("replaced by a newer connection")`.
- Every session's first packet is `ServerPacket::Host(endpoint)`.
- An observer session may send `RequestRegionConnection` / `ReleaseRegionConnection` and `GameEventAfterTick` (forwarded, Task 7); every other packet is dropped with a debug line. `pub const OBSERVER_IDLE_CLOSE_MS: u64 = 30_000;` — an observer session with no subscription and no awaited body for that long is closed.
- When an observer session becomes the home (the body arrived): the host sends `PlayerRegion`, then the session state that changes over time — `Fortress`, `Calendar`, `Sites` — but not `WorldMap` / `Civs`.

- [ ] **Step 1: Failing tests** (`observer.rs`, the real connect edge as in `login_link.rs`, slow): `observer_ticket_admits_without_placing_a_body` (no `RestorePlayer`, no `WorldMap`; first packet is `Host`); `unknown_or_expired_nonce_is_refused`; `observer_cannot_play_or_embark` (`PlayRegion` and `Embark` produce nothing); `observer_subscribes_to_an_owned_region_and_gets_its_snapshot`; `second_connection_with_the_same_ticket_replaces_the_first`; `idle_observer_is_closed`; `observer_becomes_home_when_the_body_arrives` (`PlayerRegion`, `Fortress`, `Calendar`, `Sites` in that order).
- [ ] **Step 2: Run** `just test-one server "--test observer -- --include-ignored"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** it, `just test-one server "--test login_link -- --include-ignored"`, `just test-one server "--test webtransport_handshake -- --include-ignored"`, `just test-r3`, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(server): observer sessions — a host-vouched ticket subscribes regions without a body`.

### Task 17: Client — a connection per host, routed by region (native)

This and Task 18 are the largest client change since the region threads. `instance.rs` assumes one connection throughout: the `server` / `credential` / `server_game_send` / `server_game_recv` / `client_id` / `home_region` / `clock` fields, the channel clone in `new_region`, `send_unpredicted`, `handle_request`, `update_window`, `update_lod_window`, `start`, and the one netcode thread whose end is the session's end. Expect this task to touch every one of them. **Ask the user before running the client tests.**

**Files:**
- Create: `crates/client/src/connections.rs`
- Modify: `crates/client/src/instance.rs` (`GameInstanceManager`: `server_game_send` / `server_game_recv`, `connect_and_run`, `pump`, `update_window`, `update_lod_window`, `handle_request`, `send_unpredicted`, `new_region`, the `ServerPacket` dispatch), `crates/client/src/regions/runner.rs` (`RunnerCtx.server_send`), `crates/client/src/regions/clock.rs`, `crates/client/src/netcode.rs`, `crates/client/src/harness.rs`, `crates/client/src/local_server.rs`
- Test: unit tests in `connections.rs`; `crates/client/tests/` harness cases

**Interfaces:**
- `pub enum Outgoing { Home(ClientPacket), Region(RegionId, ClientPacket), Conn(u32, ClientPacket) }` — what the manager and the region runners send; `RunnerCtx.server_send: Sender<Outgoing>` (runners send `Home`).
- `pub struct ConnectionSet` (game thread, one per session):
  - `pub fn new(first: ConnHandle) -> Self` — the login connection is the home until told otherwise; its ordinal is learned from `ServerPacket::Host`.
  - `pub fn route(&mut self, out: Outgoing)` — `Home` → the home connection; `Region(rc, _)` → `region_conn[rc]`, else home; `Conn(o, _)` → that connection, dropped if absent.
  - `pub fn on_region_elsewhere(&mut self, region: RegionId, endpoint: HostEndpoint, ticket: ObserverTicket, opener: &mut dyn ConnOpener)` — opens a connection to `endpoint.ordinal` unless one is live, sets `region_conn[region]`, and sends `RequestRegionConnection(region)` there.
  - `pub fn accepts(&self, conn: u32, region: RegionId) -> bool` — region traffic counts only from the connection the table names (home when unmapped).
  - `pub fn set_home(&mut self, conn: u32)`; `pub fn home(&self) -> u32`; `pub fn on_closed(&mut self, conn: u32) -> Closed` where `pub enum Closed { Home, Observer { regions: Vec<RegionId> } }`; `pub fn release(&mut self, region: RegionId)`.
- `pub trait ConnOpener { fn open(&mut self, ordinal: u32, endpoint: &HostEndpoint, credential: Credential) -> ConnHandle; }`; `pub struct ConnHandle { pub send: Sender<ClientPacket> }`. Incoming packets from every connection arrive on one channel as `(u32, ConnEvent)`, `pub enum ConnEvent { Packet(ServerPacket), Closed(String) }`; `GameInstanceManager::pump(&mut self, incoming: &Receiver<(u32, ConnEvent)>)`.
- Native: `netcode.rs` runs every `ServerConnection` of a session on one tokio runtime thread (`NetThread` implements `ConnOpener`); a connection that has not yet seen `Host` is keyed by a provisional id and re-keyed on that packet.
- Behaviour: `RequestRegionConnection` / `ReleaseRegionConnection` go out as `Outgoing::Region`; `RequestLod` / `ReleaseLod`, UI requests and predictions as `Outgoing::Home`. `Region` snapshots and region events are dropped unless `accepts`. An observer connection closing → its regions are dropped and re-requested through the home after `OBSERVER_RETRY_MS = 1_000`; the home closing ends the session, as today. Only `SyncClock` samples from regions on the **home** connection steer `PredictedClock`.
- Session lifecycle: `ConnectionSet` lives and dies with the game thread's session; no new Bevy resource, `Local` or entity root is added (nothing to register in `session.rs`).
- Harness: `Harness::with_hosts(n: u32)` — n in-process managers joined by `InlinePeerLink`s, one fake connection per host, the same `set_latency` per connection.

- [ ] **Step 1: Failing tests** (`connections::tests`, pure): `region_elsewhere_opens_one_connection_per_host` (two regions of one host → one `open`); `subscribe_and_release_go_to_the_owning_connection`; `predictions_and_ui_requests_go_home_only`; `traffic_from_the_wrong_connection_is_not_accepted`; `observer_close_returns_its_regions_home_close_ends_the_session`; harness: `two_hosts::window_spanning_a_host_seam_loads_regions_from_both`; `two_hosts::clock_follows_the_home_connection_only`.
- [ ] **Step 2: Ask the user**, then run `just test-one client "--lib connections::"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 command and `just test-one client "--test crossing -- two_hosts::"` (opt-in), `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(client): one connection per host with a region → connection table; input only to the home`.

### Task 18: Client — flips and moves across connections

**Files:**
- Modify: `crates/client/src/instance.rs` (the `PlayerRegion` handler, `route_transfers`, `CrossingProbe`, `maybe_report_metrics`), `crates/client/src/connections.rs`, `scripts/crossing_summary.py`
- Test: `crates/client/tests/crossing.rs`

**Interfaces:**
- `PlayerRegion(Some(r), c)` arriving on connection X makes X the home (`set_home`), whatever the client predicted; inputs follow the home, so between the predicted flip and that packet they still go to the old host, which forwards them (Task 7).
- The predicted flip itself is unchanged: `route_transfers` moves the arrival between predicted regions regardless of their connections.
- An unsolicited `RegionElsewhere` for a region outside the window (a move, Task 8): connect, subscribe, and treat the following `PlayerRegion` like any Play move.
- `pub const CONN_IDLE_CLOSE_MS: u64 = 10_000;` — a connection that is not the home and has no region mapped to it for that long is closed.
- `CrossingProbe`'s `[perf] event=region_crossing` line gains `host_flip=0|1 from_host=<o> to_host=<o>`; `crossing_summary.py` gains `--by host_flip` and `--p95` (p95 of `correction_max`, `corrections`, `confirm_ms` per group).

- [ ] **Step 1: Failing tests** (`crossing.rs`, harness `with_hosts(2)`): `host_flip_matches_thread_flip_without_delay` (the same walk across a thread seam and across a host seam with link delay 0: the same number of corrections and `correction_max` within 0.01); `held_key_across_a_host_seam_no_rubber_band` (`MAX_STEP_BACK_UNITS`); `home_follows_player_region_from_the_new_connection`; `teleport_to_a_foreign_host_switches_home`; `idle_observer_connection_is_closed`; and a script test in a new `scripts/tests/test_crossing_summary.py` (`unittest`), `test_groups_by_host_flip` (three sample lines → two groups with the right p95).
- [ ] **Step 2: Ask the user**, then run `just test-one client "--test crossing -- host_flip held_key_across_a_host home_follows teleport idle_observer"` and `python3 -m unittest discover -s scripts/tests -p "test_crossing_summary.py"` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(client): home follows the body across connections; crossing probe reports host flips`.

## Phase E — notification, launch, measurement, browser

### Task 19: `LISTEN` / `NOTIFY` for stored terrain, with a polling backstop

**Files:**
- Create: `crates/world-store/src/listen.rs`, `crates/world-store/tests/notify.rs` (module in `it.rs`)
- Modify: `crates/world-store/src/{rows.rs,handle.rs,thread.rs}`, `crates/server/src/{cuts.rs,coordinator_rt.rs,lib.rs}`, `crates/game/src/world_manager/mod.rs` (`cut_committed`, `set_stored_regions`, `seed_halo`)
- Test: `crates/game/tests/halo.rs`, `crates/server/tests/hosts.rs`

**Interfaces:**
- `StoreHandle::notify_cut(&self, cut: CutId, tick: Tick) -> Result<(), StoreError>` — its own transaction: `SELECT pg_notify('world_cut', '<cut>,<tick>')`; a no-op on SQLite. Called by the coordinator runtime after a committed `decide_cut`, and by the one-phase `CutWriter` after a successful `commit_cut`. Never inside a cut's transaction (a prepared transaction cannot contain it; §0.9f keeps it off the commit path).
- `pub const TERRAIN_POLL_SECS: u64 = 30;` `pub enum CutNotice { Cut { tick: Tick }, Resync }`
- `StoreHandle::listen_cuts(&self, mode: NotifyMode) -> Receiver<CutNotice>` — `pub enum NotifyMode { Listen, Poll }` (`VOXEL_TERRAIN_NOTIFY=listen|poll`, default `listen`). A dedicated thread with **one direct, unpooled** `sqlx::postgres::PgListener` on `VOXEL_WORLD_DB_DIRECT` if set, else the store URL; it emits `Resync` on every (re)connect and every `TERRAIN_POLL_SECS`, `Cut` per notification; in `Poll` mode only the timer. SQLite: an empty receiver.
- `StoreHandle::stored_regions_since(&self, tick: Tick) -> Result<BTreeMap<RegionCoords, u64>, StoreError>` (`WHERE saved_tick >= $1`); `Resync` uses the existing `stored_regions()`.
- `WorldManager::stored_terrain_changed(&mut self, versions: BTreeMap<RegionCoords, u64>)` — for each region whose version differs from `stored_regions` and that does not run here: update the map, drop its `terrain_cache` entry if foreign, and for every running neighbour whose halo of it is not live (the region is not in `foreign_running`) queue `RegionInput::SeedHaloFromStore`.
- LOD: nothing to do (see "Verified"); the task's doc comment on `stored_terrain_changed` says so and names §8 case 2 as not built.

- [ ] **Step 1: Failing tests:** `notify::cut_notice_reaches_a_listener` (Postgres); `notify::listener_reconnect_emits_resync` (`pg_terminate_backend` on the listener's pid → a `Resync` within 5 s, later notices still arrive); `notify::poll_mode_resyncs_without_notify` (the poll period overridden to 1 s by a test setter); `notify::stored_regions_since_returns_only_newer_saves`; `halo::stored_terrain_change_reseeds_a_parked_foreign_halo` (a neighbour's version rises → the running region gets `SeedHaloFromStore` once; an unchanged version → nothing); `hosts::edit_saved_by_host_0_reaches_host_1s_halo_after_the_cut` (host 0 digs at the border, parks the region, a world cut commits → host 1's halo mirror shows the dig).
- [ ] **Step 2: Run** `just test-pg "--test it -- notify::"`, `just test-one game "--test it -- halo::stored_terrain_change"`, `just test-r3` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the Step 2 commands, `just test-threads`, `just smoke` — PASS.
- [ ] **Step 5: Commit** `feat(world-store,server): hosts learn of each other's saved terrain by LISTEN/NOTIFY per world cut, with a polling backstop`.

### Task 20: `just dev-r3`, `just test-r3`

**Files:**
- Modify: `justfile`, `crates/world-store/src/ctl.rs` (`worldctl super-region`), `CLAUDE.md` (the recipe list, index-only)

**Interfaces:**
- `dev_r3_dir := "target/dev-r3"`.
- `host ordinal="0" hosts="1" delay="0"` — sets `VOXEL_SERVER_PORT={6466 + 10·ordinal}`, the WebTransport port `{6467 + 10·ordinal}`, `VOXEL_PEER_BIND=127.0.0.1:{6500 + ordinal}`, the pin files under `dev_r3_dir` when `hosts > 1`, `VOXEL_PEER_LINK_DELAY_MS={{delay}}`, and the login link only for ordinal 0. Its "R2 runs ONE host" comment is replaced.
- `dev-r3 hosts="2" delay="0"`: `pg-up`, `_dev-login-setup`, the service token; `worldctl clock-reset`; login; the coordinator (wait for its pin); hosts `0..hosts`, each in a loop that restarts it while it exits 5 (`EXIT_WORLD_ROLLBACK`) and stops on any other code; world `dev` registered with host 0's login link once its pin exists; logs under `dev_r3_dir/host-<n>.log`. On exit: TERM the hosts, wait for them (they need the coordinator for the leave cut), then the coordinator, then login.
- `test-r3`: `MMO_TEST_DATABASE_URL=… just test-one server "--features test-hooks --test hosts -- --include-ignored --test-threads=1"`.
- `worldctl super-region <url> <n>`: sets `world.super_region_regions`; refuses unless `super_regions` is empty (a world nobody has claimed yet). The go / no-go world uses `n = 1`, so a host seam is one region from the spawn.

- [ ] **Step 1: Write** the recipes and the `worldctl` subcommand with its test `ctl::super_region_only_before_the_first_claim` (`just test-pg "--test it -- ctl::super_region"` FAIL → PASS).
- [ ] **Step 2: Run** `just --list | grep -E "dev-r3|test-r3|host "` (all three listed), `just --dry-run dev-r3 hosts=3` (prints three host launches on ports 6466, 6476, 6486), `just test-r3` — PASS.
- [ ] **Step 3: Ask the user, then live check** (one client, `adapter=Intel`): `just dev-r3`; `just run-online`; walk across a host seam and back; embark on the other host's land; Ctrl-C the stack → every host logs a committed leave cut; start again → the fortress and the body are where they were. `kill -9` one host → both restart once and the client is told to reconnect.
- [ ] **Step 4: Commit** `feat: just dev-r3 (login + coordinator + N hosts), just test-r3`.

### Task 21: The store thread — measure before changing

**Files:**
- Modify: `crates/world-store/src/thread.rs`, `crates/world-store/src/handle.rs`, `crates/server/src/lib.rs` (the periodic `[server]` counters line)
- Test: `crates/world-store/tests/` (module `counters`)

**Interfaces:**
- `pub struct StoreCounters { pub requests: u64, pub wait_ms_max: u64, pub wait_ms_sum: u64, pub cpu_ms_sum: u64, pub cpu_ms_max: u64 }`; `StoreHandle::take_counters(&self) -> StoreCounters` (resets). `wait` = from the request being sent to its task starting; `cpu` = time spent in decode / encode inside the task (measured around those calls, not around the awaits). The `[server]` line gains `store_req= store_wait_max_ms= store_cpu_max_ms=`. No behaviour change.

- [ ] **Step 1: Failing test** `counters::wait_and_cpu_are_recorded_and_reset` (ten `load_region`s of a region with entities → `requests == 10`, `cpu_ms_sum > 0`; a second `take_counters` is all zero).
- [ ] **Step 2: Run** `just test-one world-store "--test it -- counters::"` — FAIL; implement; PASS; `just smoke`.
- [ ] **Step 3: Commit** `feat(world-store): store-thread wait and decode counters on the [server] line`.
- [ ] **Step 4: Ask the user before measuring.** With their go-ahead: `just dev-r3 hosts=2` in release, one client joining at view distance; read `store_wait_max_ms` during the join burst and during a cut. **Decision rule:** a wait above 50 ms during a join, or above one tick during steady play → write a follow-up plan to move decode / encode onto `spawn_blocking`; otherwise record the numbers in `TODO.md` and close the item. Do not change the thread in this plan.

### Task 22: The go / no-go

**Files:**
- Modify: `crates/client/src/renderer/autopilot.rs`, `justfile` (`go-nogo-r3`), `scripts/crossing_summary.py`
- Test: unit tests in `autopilot.rs`

**Interfaces:**
- `VOXEL_AUTOPILOT_BOUNDARY=host|thread` (beside `region`, `chunk`): with `VOXEL_HOSTS` and `VOXEL_SUPER_REGION_REGIONS` in the client's environment (probe only), the shuttle picks the nearest region seam whose two sides have different owners (`host`) or the same owner (`thread`), through `game::rendezvous_owner`. `pub fn pick_seam(home: RegionCoords, hosts: u32, s: u32, want_host_seam: bool) -> (RegionCoords, RegionCoords)`.
- `go-nogo-r3 delay="1" seconds="60"`: a release build; a dedicated dev database forged with `worldctl super-region … 1`; `dev-r3 hosts=2 delay={{delay}}`; `client debug online` with the autopilot, once with `boundary=thread` and once with `boundary=host`; then `scripts/crossing_summary.py --by host_flip --p95` over both logs, and a verdict line `GO` / `NO-GO` from decision 13's rule (`GO_NOGO_RATIO = 1.25`, `+ 0.05`).
- Conservation half of the verdict: `just test-threads` and `just test-r3` green on the same commit; in the run's logs no `[client-state-hash]` mismatch, `items_lost=0` on every `[server]` line, and `peer_unreceived=0` at the end.

- [ ] **Step 1: Failing test** `autopilot::tests::pick_seam_finds_a_host_seam_and_a_thread_seam` (hosts 2, s 1: the two sides of the first differ in owner, of the second agree; both within 4 regions of the home) — `just test-one client "--lib autopilot::"` is a client test: **ask first**. Implement; PASS.
- [ ] **Step 2: Commit** `feat: go / no-go probe for host seams (autopilot boundary=host|thread, just go-nogo-r3)`.
- [ ] **Step 3: Ask the user before running the measurement** — it is a benchmark on their box. With their go-ahead run `just go-nogo-r3 delay=1`, `delay=2`, `delay=5` (and `delay=0` as the reference), confirm the adapter from the `AdapterInfo` line and that `scaling_cur_freq` is not clamped, and record the three tables and verdicts in `TODO.md`.
- [ ] **Step 4: Act on the verdict.** `GO` at all three delays → continue to Task 23. Any `NO-GO` → stop this plan here, write what the tables show (which metric, at which delay, with the tick gap and `confirm_ms` beside it) into `TODO.md`, and plan the seam fix — there is no fallback (§0.9).

### Task 23: Browser multi-connection

**Files:**
- Modify: `crates/client/src/netcode_web.rs`, `crates/client/src/sim_driver.rs`, `crates/client/src/web_worker/sim.rs`, `crates/client/src/web_endpoint.rs`
- Test: wasm unit tests beside the worker's frame codec

**Interfaces:**
- The transports stay on the page's main thread (the 2026-11-02 finding in `netcode_web.rs`'s header). The page implements `ConnOpener`: one `WebTransport` session per host, pinned with `WebServer::Pinned { url: endpoint.webtransport, cert_hash: endpoint.cert_hash }`.
- Worker framing: a server frame is `TAG_SERVER_FRAME`, then the connection id as `u32` little-endian, then the frame; `SimOut::Packets(Vec<(u32, Vec<u8>)>)`; new `SimOut::Open { conn: u32, endpoint: HostEndpoint, credential: Credential }` and `ToSim::Closed { conn: u32, reason: String }`. `pub fn encode_server_frame(conn: u32, frame: &[u8]) -> Vec<u8>` / `pub fn decode_server_frame(bytes: &[u8]) -> Option<(u32, &[u8])>`.
- The inline (no-worker) path uses the same `ConnectionSet` with the page as the opener.

- [ ] **Step 1: Failing tests** (`wasm-bindgen-test`, in the target `just determinism-wasm`'s runner already serves, or a new `[[test]]` if it needs its own): `server_frame_round_trips_with_its_connection`; `short_frame_is_rejected`.
- [ ] **Step 2: Run** them — FAIL; implement; PASS; `just web` builds; `just smoke`.
- [ ] **Step 3: Ask the user, then live check** in a browser against `just dev-r3`: log in, walk across a host seam, watch two WebTransport sessions in the network panel and no reconnect.
- [ ] **Step 4: Commit** `feat(client,web): one WebTransport session per host in the browser`.

### Task 24: Documentation

**Files:**
- Modify: `CLAUDE.md` (index-only: the role-split paragraph — peer links, the `hosts` table, the world clock anchor, world generation and `EXIT_WORLD_ROLLBACK`, two-phase cuts and `max_prepared_transactions`, the entry host, observer sessions; the new env vars; "Link rules" gains the peer session's; the client paragraph gains `ConnectionSet`; the testing section gains `just test-r3`), `TODO.md` (R3 done or the no-go findings; "R3 must pick up" deleted; R4 next: `PgClaim`, per-host failover replacing the world rollback, drain, sender-side cut rows so a link drop does not abort a cut; R5: `max_prepared_transactions` on CNPG, clock sync across machines, certificate pinning for native QUIC using `HostEndpoint.cert_hash`), the design doc's §0.5 R3 line → done with this plan's name.

- [ ] **Step 1:** Write the docs.
- [ ] **Step 2: Run** `just smoke`, `just test-threads`, `just test-r2`, `just test-r3`, `just test-pg "--test it"`, `just determinism-wasm` — PASS.
- [ ] **Step 3: Commit** `docs: R3 — many hosts, one world`.

---

## Extension points left for later plans (not planned here)

- **R4:** `Ownership` becomes a trait with `PgClaim`; a dead host's super-regions are claimed by another instead of rolling the world back (the `departed` pointers and the peer queues already say what is owed to whom); drain = the leave cut plus re-placement; sender-side in-flight rows so a cut survives a link drop.
- **R5:** k8s StatefulSet; `peer_addr` holds the pod's DNS name; `max_prepared_transactions` in the CNPG cluster; clock sync across nodes; certificate pinning for native QUIC (multi-connection sends the observer nonce to redirected hosts under the same unverified TLS as the join JWT today — not worse in kind, but more endpoints); several entry hosts.
- **R6:** autoscaling changes `VOXEL_HOSTS`, i.e. ownership — out of scope with `Static`.
- **Deferred as decided:** rate limiting (observer connections would get a near-zero input budget, §0.11), Postgres backups, node autoscaling.

## Self-review notes

- **Coverage of handoff §4.** (1) host↔host link: departures T5, halo and `HaloSourceGone` and intents T6, forwarded inputs and players T7, crossings into another host's region T5/T7/T8, re-delivery across a link drop T4/T5, real link T9. (2) multi-host cuts: T12–T14, with the crash table in T14. (3) placement and the guard: the ordinal guard and `admit` T10, login placement T15. (4) client multi-connection: T7 (protocol), T16 (server), T17–T18 (native), T23 (browser). (5) LISTEN / NOTIFY with polling: T19. (6) correctness gaps: `restore_player` → T10 (`ClaimIs`); memo cap → T11; parks without `owns()` → T8; store thread → T21 (measure, ask first). (7) dev launch and tests: `dev-r3` / `test-r3` T20, the seeded layer T5 (`peer_schedule::`, in `seam_tests`), threaded real-link tests T9/T14, the latency knob T9, the go / no-go T22 with conservation and p95 at 1, 2, 5 ms.
- **Added beyond the handoff, because nothing works without them:** the shared world clock (T1), world-unique client ids (T2), the world generation (T3). Each is called out under "Verified" / "Decisions".
- **Design questions of the handoff:** topology → decision 1; cut protocol → 6; how the client learns of foreign hosts → 8; NOTIFY vs polling → 11; browser now or later → 12.
- **Review Focus lines and their tests:** 1 → T7; 2 → T7, T15; 3 → T14; 4 → T12, T14; 5 → the verification steps of T1, T2, T5, T13.
- **Names across tasks:** `ClockAnchor` (T1) → T13's `set_world_tick`; `session_client_id` (T2) → T7, T16; `HostRow` / `register_host` / `WorldGeneration` / `EXIT_WORLD_ROLLBACK` (T3) → T9, T12–T14, T20; `HostEndpoint` / `PeerHello` / `PlayerHandoff` / `Reliable` / `Volatile` / `PeerSession` / `PeerLink` / `InlinePeerLink` (T4) → T5–T9, T13–T17; `accept_arrival` / `reaches` / `is_foreign` / `MultiRig` (T5) → T6–T8, T10, T13; `SessionKind` / `ObserverTicket` / `take_observer_orders` / `departed` (T7) → T8, T15, T16; `Target` / `ClaimIs` (T10); `cut_gid` / `prepare_cut` / `decide_cut` / `abort_cut` / `resolve_prepared` (T12) → T13, T14; `BeginCut` / `CutDone` / `CutPrepared` / `CutFailed` / `begin_world_cut` (T13) → T14; `Placement` / `place_spawn` (T15); `ObserverOrders` (T16); `Outgoing` / `ConnectionSet` / `ConnOpener` / `ConnEvent` (T17) → T18, T23; `notify_cut` / `listen_cuts` / `stored_terrain_changed` (T19).
- **Known limits, stated rather than hidden:** a peer link that is down aborts cuts until it is back; with hosts > 1 there are no cuts while the coordinator is down; a host crash rolls back every host; hosts on different machines need synchronised clocks; a world request (embark, legends) in flight while its player crosses to another host is answered to a client that has gone, so the claim is released and the player asks again.
