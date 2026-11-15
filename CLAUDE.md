# CLAUDE.md

Guidance for Claude Code in this repository. It holds the rules and the map; the detail lives in the
code and in `docs/superpowers/{specs,plans}/` (dated files — grep them by topic). The full pre-prune
version (every subsystem in detail, as of 2026-11-08) is `docs/claude-md-full-reference-2026-11-08.md` —
grep it when a subsystem's detail is needed; it is not kept up to date.

**Do not create git worktrees.** Work directly on `develop` in this checkout — including test runs,
bisection and subagent dispatches (shared build caches + the user's concurrent edits).

## Project

A voxel game in Rust: client/server over QUIC (WebTransport in the browser), deterministic simulation,
rollback netcode, a multi-region world. The client is a Bevy app; `game`, `server` and `worldgen` stay
engine-agnostic — **no Bevy, no windowing library** there. The terrain is voxel again since 2026-11-14 (the
stud terrain was reverted): `docs/superpowers/plans/2026-11-13-voxel-terrain-return.md`.

**Direction (2026-10-20):** a **Dwarf Fortress–style** game on the multi-region engine — a huge world
forged up front (geology, climate, biomes, history), players play regions from a top-down commander
camera (fortress mode; adventure mode exists too). No phased plan: features land piece by piece,
roughly modelling DF. Research: `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/00-synthesis.md`.
Old RTS pivot is on branch `ds/rtsPivot`. `TODO.md` tracks the current direction.

## Building and running

The root `justfile` is canonical — `just --list`. Key recipes: `build`, `run` (debug launch straight into
an embarked world), `run-two` (two clients, one fortress), `play` (real start: main menu → generator →
world map → embark), `server` + `client` (dev server on 127.0.0.1:6466), `web`, `smoke`, `test-one`,
`test-threads`, `determinism-wasm`, `clippy`, `world-map`. **Keep the justfile in sync**: any new or
changed build/run/test command updates its recipe in the same change.

- **Toolchain**: one DATED nightly pinned in `rust-toolchain.toml` (used only to keep `target/` small —
  `-Zembed-metadata=no`, nightly build-dir layout). Bump the date deliberately (then `just smoke` +
  `just determinism-wasm`), never to a floating `nightly`. No Cranelift.
- **Compile time**: workspace feature unification is on; Slint screens are generated into three crates;
  `client` builds with `-Clto=off`. Don't add `-Zthreads` to incremental crates. Test builds keep their
  own rapier→game→… copies ON PURPOSE (tests build at region 64, play at 256).
- **CPU cap**: every `just` build/test is pinned to 80% of cores (`cap` = `nice -n 10 taskset …`;
  `cpu_percent=`, `jobs=`). Raw `cargo` is NOT pinned — go through `just` or prefix it yourself.
- **GPU**: `just adapter=6700 …` / `$VOXEL_ADAPTER` pins wgpu; unpinned, `client::adapter_pick` picks the
  best adapter (never the slow NVK T600 if anything better exists). Check the `AdapterInfo` log line.
- **Debug launches** (`crates/client/src/debug_launch.rs`): `client debug embarked [--reset]` forges
  `worlds/debug-embarked/` once and reuses it; add presets there. Worlds live in `worlds/<slug>/`
  (git-ignored; `terrain.sqlite` store, `world.json`, `server.log`). Menu worlds spawn their own
  single-player server (`VOXEL_SINGLE_PLAYER=1`, free port). `VOXEL_SKIP_MENU=1` connects to the dev server.

### Build/run dials

- **World scale** (client, runtime): `just world=tiny|small|full` (`world_scale.rs`), `view=<m>` view
  distance, `VOXEL_NEAR_RADIUS`, `VOXEL_LOD_RINGS`, `VOXEL_FORTRESS_RADIUS`. Use small worlds on weak GPUs;
  compare perf numbers only within one scale.
- **Region size** (BUILD time, blocks): `region_blocks=64|128|256|512` (`VOXEL_REGION_BLOCKS`). Play
  recipes default to 256, tests/check/clippy/raw cargo to 64. Server and client must match (no handshake).
  Never write a region edge as a literal — derive from `REGION_BLOCKS` / `REGION_SIZE`. Costs scale with
  area; compare numbers within one size.
- **Chunk size** (BUILD time, blocks): `chunk_blocks=2|16` (`VOXEL_CHUNK_BLOCKS`, default 16 in
  `.cargo/config.toml` `[env]` and the justfile; anything else fails the build — `game::CHUNK_BLOCKS`,
  `game/src/block.rs`). A block is 1 m = 16³ voxels (`BLOCK_VOXELS`). The **sim chunk** is `CHUNK_BLOCKS`³
  blocks (16 m = 256 voxels, `CHUNK_UNITS`) and owns one entity/body/collider set; the 32³-voxel **brick**
  (`BRICK_VOXELS`, 2³ blocks) is the meshing/LOD unit — the client fans each chunk out to
  `BRICKS_PER_CHUNK_EDGE`³ bricks (`renderer/bricks.rs`). A literal 32 must say which it is (`BRICK_VOXELS`
  or `CHUNK_UNITS`). `wasm_determinism` / `scene_hash_golden` pin hashes per size, kept current at 16
  only — re-record (`just chunk_blocks=2 …`) before relying on 2. Server and client must match.

### Deployment

OpenTofu (not Terraform) in `deploy/`: local single-node k0s (`just deploy-local`, never the inner loop);
the `orion` k3s box (`deploy/envs/orion/`, S3 remote state, `just orion-plan|apply <root>` — always plan
first; roots `platform` → `garage` (+ `garage-buckets`) = https://s3.factoriosurvivalworld.com, the S3-compatible
object store on the box itself (one copy, no off-box backup; telemetry and Zulip files live there) → `monitoring`, `picoshare` = https://share.factoriosurvivalworld.com, `zulip` = https://chat.factoriosurvivalworld.com,
`keycloak` (+ `keycloak-realm`) = https://auth.factoriosurvivalworld.com, the identity provider → `registry` (the ECR repository `mmo/login` and the cluster's pull-only IAM user) → `login` = https://login.factoriosurvivalworld.com, the game's login service (two pods, its own PostgreSQL, metrics scraped; its image is `mmo/login` in ECR, pulled with a pull secret the CronJob `ecr-refresh` renews every 6 h; one tag, `latest`, and an image `latest` has moved past expires about a day later; a release is `just orion-login-push` — pushed as `latest` (git revision in an image label), set by digest with `kubectl set image`, never an apply — and the way back is `kubectl rollout undo` (works while the previous image is still in ECR or the node's cache); `just orion-login …` runs its operator commands in a pod, `orion-login-logs` follows its log; apply `registry` and push BEFORE the first apply) → `site` = https://factoriosurvivalworld.com, the static website from `website/` with the `orion`
Keycloak login theme beside it in `website/theme/` (`just site-dev|site-test|site-theme-check`; after a Keycloak image
bump re-diff `website/theme/orion/login/template.ftl` against the new `base` theme and run `just site-theme-check`); monitoring stack
handoff `docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md`);
the public web build `just deploy` (`scripts/web_release.sh` — use the pinned binaryen under
`target/tools`, never the distro's). `deploy/**/*.tfvars` and `.env` are git-ignored — never commit or
print secrets.

**Grafana (orion) for debugging and live data:** `.env` holds `GRAFANA_SERVICE_TOKEN`, a service-account
token with the Admin role. Claude may use it to read dashboards, metrics, logs and traces through Grafana's
HTTP API at https://grafana.factoriosurvivalworld.com (`Authorization: Bearer $GRAFANA_SERVICE_TOKEN`;
data sources: Prometheus (local), Thanos, Loki, Tempo, Pyroscope, Alertmanager; `just orion-grafana`
gives a local port-forward). Load it with `set -a; . ./.env; set +a` inside the one command that needs it; never echo it,
pass it on a command line that gets logged, or write it anywhere else. Reading is fine without asking;
**ask before changing anything in Grafana** (dashboards, alerts, data sources, users, tokens) — durable
changes belong in `deploy/modules/monitoring/`, not in the UI.

## Testing

**Never run whole game/server/client suites as a routine check.** Verify with:

- `just test-one <crate> "<cargo test args>"` for exactly the touched tests. Integration tests are ONE
  `it` binary per crate (`--test it <file>::`); files that touch process-global state (`HASH_MODE`, the
  forged world, a logger, wasm32, perf gates) keep their own `[[test]]` target — a new such file needs one.
  Server test targets `roles`, `coordinator_rt`, `hosts` and `takeover` need `--features test-hooks`
  (`just test-r2` / `test-r3` / `test-r4` / `test-server` pass it; `observer` does not); Postgres-backed
  tests take their throwaway database from `crates/pg-test-support` (`MMO_TEST_DATABASE_URL`). Multi-host:
  `just test-r3` (real hosts over real links, serial), `just test-r4` (the takeover crash table, serial)
  plus the seeded layer `game` `peer_schedule::` / `peer_journal::` (part of `just test-threads`); the
  browser's frame codec on wasm32: `just client-wasm`.
- After a wide change run each touched crate's whole non-client suite once — filtered runs let two broken
  world-store tests sit unnoticed.
- `just smoke` — the default sanity check, hard-limited to 60 s. It only type-checks the client; running
  client tests (`just test-one client "--lib <filter>"`) is a slow opt-in — **ask first**.
- `just test-threads` after touching handoff, `WorldManager`, `region_threads` or anything crossing a seam
  (seeded scheduling jitter in `game/tests/handoff_schedule.rs` + real threads in `server/tests/threaded_world.rs`;
  scheduling-sensitive correctness goes in the seeded layer).
- `just determinism-wasm` after touching sim math, the physics forks, or anything hashed/serialized.
  `physics_goldens::` pins physics behaviour (re-record only on purpose).
- `just test` / `just test-full` only when the user asks or a change is wide — say so first.
- Tag tests slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`.
- Headless client harnesses must use a tiny world (`set_lod_table(&[])`, near radius 0) or they OOM.
- A test whose outcome reads block props calls `common::install_real_block_props()` first.

## Architecture rules (the things that break if forgotten)

**Rollback & determinism**
- Rollback bar: `hash(before) == hash(after undo)`, bit-exact. `#[rollback]` (`crates/macros`, expands in
  `game`) gives `#[undo(cell|map|slotmap)]` fields auto-inverted wrappers; plain `Undo<T>` closures MUST be
  true inverses. Spec: `docs/superpowers/specs/2026-07-03-undo-api-redesign-design.md`.
- Hashing is the `StateHash` trait (`crates/state-hash`), never std `Hash` or serialized bytes.
- Per-entry hash mode defaults to `Off` (`HASH_MODE=delta|full` for debugging undo closures). Desync is
  caught by the server's state-hash interval (100 ticks, `VOXEL_STATE_HASH_INTERVAL`) — never weaken it.
- No inherent `f32` transcendentals in sim paths (use `libm`); `BTreeMap`, never `HashMap`, for ordered sim
  state; randomness only via stateless hash rolls (`roll(tick, subject, salt)`); tunables as named consts.
- `slotmapd`/rapier forks provide exact LIFO inverses (`revert_insert`/`revert_remove`) — plain
  remove(insert(x)) does not restore allocator state.
- `game/src/lib.rs` glob re-exports at the crate root — the macro expansion depends on it. `TICK_RATE` is
  a period in ms despite its name.

**Regions & seams** (plan `docs/superpowers/plans/2026-11-06-seam-transparent-regions.md`)
- The region is the threading unit: one OS thread per running region, nothing parallel inside one;
  regions talk only via `RegionInput`/`RegionOutput` messages (the future network seam) — never shared memory.
- Goal: a seam changes nothing observable. One world-numbered tick in every region (`WorldClock`); entities
  are `EntityId { birth_region, serial }`; crossings are stamped transfers applied at T+1.
- A slot's neighbour messages go through `SlotInbox` (`game::slot_order`) and are applied right before the
  next `Tick` in canonical order, on server and client alike — never apply a `Transfer` on receipt.
  `Shutdown` drains the inbox too. A plain `RegionInput::Event` overtakes it: anything that must follow a
  neighbour message travels as a `Transfer`.
- `carry_components` / `Client::arrive` destructure with no `..` on purpose: a new `Ecs`/`Client` field must
  be classified as travelling or not.
- Sim code reads neighbours through `WorldView` (own ∪ halo) and writes anything it does not own as an
  `Intent`, never directly. Intents never wake a region; items are never lost (salvage path).
- Sims stay region-local; the world offset exists only at the render boundary.

**Server / store**
- Hub-and-spoke: netcode edge (tokio/quinn, WebTransport) → manager thread (`WorldManager`) → region
  threads over crossbeam channels. Tokio stays at the networking edge.
- World store (`crates/world-store`, SQLite or Postgres via `VOXEL_WORLD_DB`) holds the forged world as
  tiles, regions, players, history; saved by periodic save cuts at one world tick. A store read ERROR never
  regenerates a region (`LoadFailed`, retried later), nor does a corrupt parked blob; only a missing row generates.
- A cut part writes only rows of its own regions — **never add a table two hosts' parts can both write**.
  Entity rows are keyed per region (migrations 0009/0010), so the database no longer refuses a duplicate
  entity: the store does (a duplicate inside a part, or a stale row in another region the cut cannot be
  deleting, is refused; a cross-host one is reported after the commit, `cuts_with_entities_twice`).
  Postgres migrations run under a lock timeout (`MIGRATE_LOCK_TIMEOUT_MS`) that names any
  `pg_prepared_xacts` they wait behind.
- Bump `worldgen::tile_map::WORLD_MAP_VERSION` on ANY change to what `build_world_map` returns.
- Terrain refusal (`world-store/src/open.rs` `refuse_unreturned_world`): a store with saved regions that has
  not applied migration `0011_voxel_terrain_returns` is refused before any migration runs and left untouched
  (a stud world, or a voxel world saved before the stud switch — `0005` would drop its chunks): re-forge it.
  From `0011` on a world whose `terrain` is `Studs` is refused. Migrations `0004`/`0005` stay in the tree and
  are never edited (sqlx checksums them).
- Auth: `VOXEL_AUTH=token` (default, offline) or `jwt` (login service, `crates/login`, with Keycloak: it owns
  the passwords and the login pages, the login service trades a Keycloak access token for its own refresh
  token at `/v1/login/oidc`; `VOXEL_OIDC_ISSUER` (unset = no Keycloak login), `VOXEL_OIDC_AUDIENCE` (`mmo-login`),
  `VOXEL_OIDC_CLIENT_ID` (`game`); `just dev-login|dev-r2|dev-r3` start the dev Keycloak (`kc-up`) and set the
  issuer; a launch with no screens starts from `login account issue-refresh <email>`, `login account revoke
  <email>` ends an account's sessions; no register / password / reset routes). A `jwt` host without a
  service token refuses to start unless `VOXEL_JWT_ADMISSION=newest`, dev only. The workspace's
  only rustls crypto provider is **ring** — keep `aws-lc-rs` out of the tree.
- **Role split (R2, `VOXEL_ROLE=all|host|coordinator`, default `all` = one process as before; plan
  `docs/superpowers/plans/2026-11-07-r2b-role-split.md`)**: the coordinator (`game::coordinator`, claims,
  embark/return, history, sites, the coordinator row) and the host (`WorldManager` + regions) talk only via
  `HostMsg`/`CoordMsg` over a `WorldLink` — `InlineWorldLink`/channel in `all`, `QuicWorldLink` over
  `svc-link` (QUIC, shared `VOXEL_SERVICE_TOKEN[_FILE]`, pinned self-signed cert) when split. Split roles
  need Postgres (`VOXEL_WORLD_DB`) and refuse anything else. Env: `VOXEL_COORDINATOR_BIND`/`_ADDR`
  (127.0.0.1:6480), `VOXEL_COORDINATOR_CERT_HASH_FILE`/`_CERT_SHA256`, `VOXEL_HOST_LINK_BIND` (login→host
  push, 127.0.0.1:6490) + `_CERT_HASH_FILE`, `VOXEL_OWNERSHIP=allinone|static|claim`, `VOXEL_HOST_ORDINAL`,
  `VOXEL_HOSTS`, `VOXEL_SUPER_REGION_REGIONS` (16; checked against the world row). `just dev-r2` runs
  login + coordinator + host 0 (state in git-ignored `target/dev-r2/`); `just coordinator|host`, `just test-r2`.
- **Many hosts (R3 + R4 takeover, `VOXEL_HOSTS` > 1; plans `docs/superpowers/plans/2026-11-12-r3-multi-host.md`,
  **The coordinator takes the same `VOXEL_OWNERSHIP` as its hosts**: its own environment decides whether it
  declares successions (`roles::coordinator_crash_policy`); a host's `Attach` carries its crash policy, and a
  coordinator of several hosts refuses one that is not its own (the error names `VOXEL_OWNERSHIP`).
  `2026-11-13-r4-takeover.md` — R4 built; whole-branch review and the owner's hand checks pending, see
  `TODO.md`)**: needs role `host`, Postgres and the login link (`jwt`), else refused.
  - Hosts talk directly over peer links (`game::peer`: one `PeerSession` per other ordinal, `Reliable` /
    `Volatile` messages; `server/src/peer_link.rs`, QUIC over `svc-link`): crossings, halo, intents,
    forwarded input, player hand-offs. Each host registers once per boot in the `hosts` table
    (`register_host`: addresses, pins, `clean`); peers find each other there.
  - One tick numbering for all hosts from the world clock anchor (`world_clock.rs`; `worldctl clock-reset`).
  - **Ownership** (`VOXEL_OWNERSHIP`, `game::ownership`): `static` = R3's fixed layout, a host crash raises
    the world generation and EVERY host exits `EXIT_WORLD_ROLLBACK` (5); `claim` (default of `dev-r3` /
    `go-nogo-r3`, and of `just host` / `just coordinator` with several hosts; one host and `dev-r2` stay
    `static`) = super-regions claimed on demand, a dead host's land
    given to one heir while the others run on (`roles::coordinator_crash_policy`: only with several hosts).
    A claim needs the claimer's live, dirty `hosts` row (a declared-dead or cleanly-left boot wins nothing).
  - **Lease**: a host stamps `hosts.seen_ms` every `HOST_HEARTBEAT_MS` (2 s) from its MAIN loop — never a
    timer thread (a hung loop must stop beating) — on a connection of its own (never the store's shared
    pool), each beat under a client-side deadline (`HEARTBEAT_DEADLINE_MS`), and once after each long boot
    step (`lease::beat_at_boot`); the coordinator declares it dead after `HOST_LEASE_MS`
    (10 s) of its own unchanged reads (`LeaseWatch`: a database outage declares nobody). The Postgres lease
    clock is `clock_timestamp()`, never `now()`. A fenced transaction (a cut part, a region load) idle longer
    than the lease loses its session (`FENCED_TX_IDLE_MS`, set by the fence check): a frozen host's row
    locks do not outlive it.
  - **Succession** (`coordinator/succession.rs`, `world_manager/succession.rs`; migrations `0012_host_lease`,
    `0013_successions`): one fenced transaction marks the dead row and moves ALL its land to one heir per
    predecessor. Survivors report their peer journal against the dead host; the heir masks the inherited
    land (`inheriting`, from the order on) until it resolved every report and adopted, holds every login order
    until then, pins the affected regions, and its parts carry `adopts` until the settle. No cut begins until
    every survivor applied and the heir adopted (answers count per BOOT); then one is due at once
    (`SETTLE_CUT_RETRY_MS` after the last cut ended, not a save period), and its commit decision names the open
    successions (`settles`) — the store records any other set as an abort. Conserved: identities (entities,
    players, people) exactly once; items and terrain edits that crossed the dead host's seams within one
    save interval are not guaranteed (`succession_seam_events=` on the `[server]` line); a leave or an
    offline retirement has no item window.
  - **The world generation is the last resort**: `static` / `allinone`, a dirty boot with no other host
    alive, a second failure before the settle / `SUCCESSION_DEADLINE_MS` (30 s) without every answer / a
    recipient's reboot in a declared run, `worldctl rollback <url> --yes`; an offline `retire-host` raises it.
  - **Boots** (`register_host`, `server/src/lease.rs`): a boot WAITS while an open succession names its
    ordinal as predecessor (crash, offline retirement or leave) and registers after the settle owning
    nothing — if the heir never returns, only `worldctl rollback --yes` gets it out; a dirty boot beside a
    live host vouches its old row dead and waits; with nobody alive (a total crash) the first boot raises the
    generation and gives every other dirty row a fresh lease (the others boot seconds apart and keep their
    land). A registration stuck behind a lock exits 1 after `CLAIM_LOCK_TIMEOUT_MS` (60 s).
  - Saves are coordinated world cuts in two phases (each host `prepare_cut`s its part, the coordinator
    decides; Postgres needs `max_prepared_transactions > 0`, the dev compose sets it); parts a crash left
    staged are resolved at start before anything else (`resolve_staged_cuts`). An exiting host asks for a
    leave cut and exits 0 either way — not committed → its `hosts` row stays dirty (a crash to the others).
  - **Drain**: a cleanly departed host keeps its land; `LandWanted` from a host needing it → a `Leave`
    succession after `HEIR_WAIT_MS` (30 s; at once for an ordinal beyond the host count). A stopping host
    tells its sessions why on every exit branch (`LEAVING_REFUSAL`, `FENCED_REFUSAL`, `ROLLBACK_REFUSAL`).
  - `worldctl retire-host <url> <ordinal>` (`world-store/src/operator.rs`): hosts alive → only vouches the
    row dead; none → a succession to the lowest other live-row ordinal, generation raised. `claim` only —
    the store does not record the policy, so it cannot refuse a `static` world.
  - Login links to the entry host (`VOXEL_ENTRY_HOST`, default 0), which places each player on the host
    that holds them (`world_manager/placement.rs`, relayed `SpawnOrder`s); a world request carries its
    player's hop (`HostMsg::Request { hop }`, an older placement's is refused). Regions of another host reach a
    client through an observer session: `RegionElsewhere { endpoint, ticket }` → a second connection with
    `Credential::Observer` — no body, input only as its own `origin_client`.
  - Saved terrain of other hosts arrives by LISTEN/NOTIFY per cut with a polling backstop
    (`VOXEL_TERRAIN_NOTIFY=listen|poll`; `VOXEL_WORLD_DB_DIRECT` = a direct, unpooled URL for the listener);
    a changed version drops that region's cached LOD (`stored_terrain_changed` → `invalidate_region_lod`).
  - Env: `VOXEL_ENTRY_HOST`, `VOXEL_PEER_BIND` (127.0.0.1:{6500+ordinal}), `VOXEL_PEER_CERT_HASH_FILE`,
    `VOXEL_ADVERTISE_QUIC`/`_WT` (what clients are told), `VOXEL_WT_PORT`, `VOXEL_PEER_LINK_DELAY_MS`
    (measurement only); `worldctl super-region <url> <n>` (only before the first claim) + hosts run with
    `VOXEL_SUPER_REGION_REGIONS=n`.
  - `just dev-r3 <hosts> [delay]` (positional, 2..6): login + coordinator + every host under
    `scripts/dev_r3.sh` (restarts a host on exit 5 or 137 — and on 3, a replaced host, when it runs `claim` —
    at most 3 per 60 s; `DEV_R3_*` knobs in its header; state `target/dev-r3/`); `just host <ordinal> <hosts>
    <delay> [static]`, `just coordinator <hosts> [static]` (the same word); `just go-nogo-r3 <delay>
    [seconds]` (own database and `target/go-nogo-r3/`; exit 0 GO, 1 NO-GO, 2 INVALID, 3 no verdict).
- **Owner epochs & fencing**: a host claims super-regions (`super_region_of`, `game::ownership`) in Postgres
  with an epoch; every cut / region load / coordinator save is fenced on it (`StoreError::Fenced`) — a fenced
  process stops writing and exits `EXIT_FENCED` (3, also when fenced at `register_host`); a dead coordinator
  thread exits `EXIT_COORDINATOR_FAILED` (4); a world rollback exits `EXIT_WORLD_ROLLBACK` (5). No advisory
  lock any more (SQLite keeps its flock). `OpenMode::ReadOnly` handles (export, tools) write nothing and need
  the newest schema. Never add a store write that skips the fence. A world cut's commit decision must list
  every host with a non-clean `hosts` row (`two_phase::decide_cut` records an abort otherwise). A succession
  moves rows at `epoch + 1`: the replaced process is fenced at its next heartbeat (`lease.rs`
  `pump_or_fence`) or write. A new `hosts` / `super_regions` / `successions` statement needs its lock walk
  (lock order and the two safe deadlock cycles: the module notes of `world-store/src/succession.rs`; the
  operator's in `operator.rs`, the one unfenced `successions` writer). The settles↔adopts pairing (a settled
  succession's heir part carried `adopts`) is a coordinator-side invariant the store does not enforce — it
  only detects a breach after the commit (`two_phase::report_settled_unadopted`, `cuts_settled_unadopted`).
- **Link rules**: a re-`Attach` after any reconnect rebuilds the coordinator's view; requests are memoised by
  `RequestId` (idempotent resend); the Attach waits for exact people lists ("wait, never guess" — conservation
  beats latency). Login pushes `SpawnPlayer`/`Kick` to the host's link; kicks are durable until acked
  (login migration 0003). With several hosts the coordinator serves every ordinal of `VOXEL_HOSTS`; login
  links to the entry host only.
- **Peer link rules** (`game::peer`, `world_manager/peers.rs`, `world_cut.rs`): a `PeerSession` is never
  replaced — `set_peer` on a known ordinal swaps the link and resets the connection. Resume is boot-scoped
  (`PeerHello.received_from_boot`: a count made against another boot renumbers). `Reliable` is numbered,
  kept until acked and re-sent after a drop; `Volatile` is sent only while the peer is reachable, never
  replayed. A state message the sender already accounts for in cut K travels as `Reliable::InCut { cut,
  body }`; a host marks `CutMark { K }` only once all its regions answered and its tick reached T. From a
  leave cut's `BeginCut` until `CutDone` each side holds back what it would send the other. A link that
  drops mid-cut puts that pair in count mode (`PeerSession::freeze` / `thaw`, `HostMsg::CutPeerCount`
  relayed by the coordinator) and the cut can still commit; one that drops after a side's part is complete
  by marks still aborts it. The coordinator down means no cuts and no takeover until it is back.
- **Peer journal** (`game/src/peer/journal.rs`): every host under `claim` (a `static` host records nothing)
  journals the state messages it exchanged with each peer since the last committed cut — a succession's
  reports; an entity's events are ordered by its transfer stamp, a player's by the hand-off's hop, never by a
  host's own tick. Truncation rests on every
  world-cut participant calling `PeerJournal::cut_begun`: any new path that joins a cut must call it. A new
  state-bearing `Reliable` must be journalled or argued out. A new per-ordinal `WorldManager` field must be
  handled in `world_manager/succession.rs`.

**Client** (`crates/client`)
- `renderer/` bridges sim state to Bevy (`bridge.rs`); predicted regions run like the server's (`regions/`,
  threaded on native). Near terrain: bricks (`bricks.rs`) are meshed in `meshing/` into 8-byte quad records
  (`meshing/records.rs`, layout locked to `assets/shaders/voxel_pull.wgsl`) and drawn by `voxel_pull/`
  (`TerrainRenderConfig` in `config.rs`, `VOXEL_NEAR_RENDER=per-chunk|direct|gpu`); the render unit is the
  sim chunk on every backend (`meshing/chunk_mesh.rs` merges its bricks' records). Far terrain: the
  server-streamed LOD rings (`lod_rings.rs`, `meshing/lod.rs`), always rasterized.
  `voxel_pull.wgsl`, `voxel_pull_depth.wgsl` and `voxel_texture.wgsl` are compiled in (rebuild, no hot reload).
- **Connections** (`connections.rs`): a session's `ConnectionSet` holds one connection per host, each open
  under its own key; `RegionElsewhere` opens an observer connection, and region traffic counts only from
  the connection its region maps to. Input, predictions, UI and LOD requests go to the home only; the home
  follows `PlayerRegion` from another connection only after a higher `ServerPacket::HomeHop`. In the
  browser the page opens one WebTransport session per host, pinned by `HostEndpoint.cert_hash`
  (`web_endpoint::web_server_for_host`; page → worker framing in `web_frame.rs`). A home host remembers
  subscribes it cannot answer yet and answers again after a succession; the client re-maps regions whose
  owner changed (an observer connection never redirects: a region it cannot serve is asked of the home after
  `OBSERVER_RETRY_MAX_MS`). A session whose home connection closed without a refusal (ANY such close), or was refused with
  `FENCED_REFUSAL` / `ROLLBACK_REFUSAL` / `LEAVING_REFUSAL` / `PLAYER_PARKED_ELSEWHERE`, rejoins by itself
  with its kept refresh token, re-read from the store before every join call (`login::flow::rejoinable`; one
  join call at a time, `REJOIN_RETRY_MS` apart; a 503 = not yet; a ticket's connect is bounded by
  `REJOIN_CONNECT_MS` (15 s), then a new join; gives up after `REJOIN_MAX_MS`; revoked → the sign-in screen). Native QUIC keep-alive
  3 s / idle 10 s (`netcode.rs`, as WebTransport).
- **Session lifecycle** (`session.rs`): any new per-world resource, system `Local` or entity root must be
  registered (`add_session_reset`, `SessionLocal`) or tagged `SessionScoped`, or it leaks into the next world.
- UI is **Slint only** (no `bevy_ui`, no egui). `CursorPolicy` is the only cursor-grab writer. `dev_ui`
  (F3 Debug tab) is dev tooling only, never player-facing.
- wasm: no shared-memory threads; work runs in Web Workers by bincode messages. New compute work goes in
  `renderer::compute::JobKind` and must carry everything it reads (workers have no ECS).
- Bevy pinned at `0.19.1`; upgrades are deliberate migrations. Shader tests compose against stubs and miss
  upstream WGSL signature changes, so after a bump run `just pull-smoke` on a real adapter (a self-screenshot
  of the pulled near path only; it passed on Iris Xe 2026-11-14) and look at the terrain with `just run`
  (nothing automated covers the LOD rings or water on a real adapter).

**Slint UI** (`crates/slint-ui`) — built on the sibling checkout `../ui` (`~/Projects/ui`, needs a local
Factorio install or `FACTORIO_DATA`; Wube's sprites are not shippable). Sources in `crates/slint-ui/ui/`,
generated into `slint-ui-game` / `slint-ui-frontend` / `slint-ui-tools`. Compiled with the `fortress` theme
only (`build_common.rs` `single_theme_overlay`). In widgets never bind `visible`/`opacity`/shadows on a
component's ROOT (forces inlining, bloats codegen). Every root component calls `GameTheme.apply()` in `init`.
After a Slint bump run `just slint-wasm`.

**Worldgen** (`crates/worldgen`) — `generate_region(RegionCoords, …)` (`generate_columns` for a window,
`generate_region_forged` for an explicit forged world) is a pure function of coordinates (+ the installed
forged world) yielding the region's chunk columns; that is what makes "park, or regenerate" safe. The fused
`pipeline` `column()` must mirror the `#[cfg(test)]` `DensityFn` graph bit-for-bit.
Forge tunables all live in `forge/knobs.rs` (`just forge-ui` to tune; check at 16 m cells before keeping a
value; the `forge_quality` gate must pass). Water is recipe data (`ColumnGround::water_top`), never
blocks. Look at the world with `just world-map` after tuning changes.

**Assets**
- Blockbench `.bbmodel` loads natively (`crates/bbmodel`, `renderer/model.rs`); `just model-view`,
  `just asset-viewer`.
- Creature models are never hand-written: edit `tools/creatures/src/<id>.toml` and run `just creatures`;
  carry any Blockbench touch-up back into the source. Plan:
  `docs/superpowers/plans/2026-10-29-creature-models-handoff.md`.
- Generated art: `just assets` (paid APIs — check the picture with `--picture-only` before buying a model).

## Profiling

Measure in **release**; the dev profile runs hot loops ~2.5× slow. Always confirm which GPU you got, and
watch for laptop power clamping (`scaling_cur_freq`) — compare ratios, not seconds. Ask before launching
benchmarks (the user runs their own on this box).

- `just perf-capture` — the self-describing capture to hand to analysis (`summary.txt` first).
- `[perf] event=…` join timeline in every log; volume contract: per process/region/batch, never per
  chunk/key/frame — high-rate work goes in the periodic `[server]` counters line.
- `just diag` (CPU-vs-GPU verdict), `gpu-measure` (Intel per-pass GPU time), `fps-steady`, `trace-chrome`
  / `trace-join` (per-system tables; close the window, don't Ctrl-C), `perf-client` / `perf-server`,
  `fly-probe` / `shuttle-probe` (region-crossing repros), `firefox-profile` (web build).
- Never `json.load` or open whole traces/folded stacks — use the summary scripts in `scripts/`.

## Vendored forks

`crates/rapier` (0.36), `crates/parry` (0.31), `crates/slotmapd`. Every rapier/parry
hunk differing from upstream carries a `// mmo-fork:` marker and is pinned by a test in
`crates/physics-next`; updating upstream = re-import + re-apply marked hunks. Determinism comes from
`enhanced-determinism` (libm), no `parallel`/`simd8`. After touching sim math in the forks run
`just determinism-wasm` and the physics-next suite.

## Conventions

- Wire serialization is `bincode` framed by `game::encode_packet` / `decode_packet` (lz4 above a
  threshold) at every transport edge — never raw `bincode` on the wire. State hashing uses `crc32fast`.
- Cross-thread communication uses `crossbeam` channels.
- Editions are mixed on purpose (older crates 2021, newer 2024).
