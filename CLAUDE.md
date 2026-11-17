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

**Name:** the game is **Errantry: Renown and Ruin** ("Errantry" for short) — that is what players read
(window and page titles, the menu, the website, login pages and mail). **Orion** is the project's code name:
keep it wherever players do not look (code identifiers, `deploy/envs/orion`, `just orion-*`, the kube
context, bucket and IAM names, the `orion` login theme folder, storage keys). The game is served at
`game.errantry.renownandruin.com`.

**Direction (2026-10-20):** a **Dwarf Fortress–style** game on the multi-region engine — a huge world
forged up front (geology, climate, biomes, history), players play regions from a top-down commander
camera (fortress mode; adventure mode exists too). No phased plan: features land piece by piece,
roughly modelling DF. Research: `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/00-synthesis.md`.
Game design (the two loops, adventure and fortress; what is decided, proposed and open): `docs/game-design.md`.
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
  `worlds/debug-embarked/` once and reuses it (`--adventure`: adventure mode instead of the fortress); add presets there. Worlds live in `worlds/<slug>/`
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

**Quest script language** (`quest/` — its OWN Cargo workspace: own `Cargo.lock` and `quest/target/`, excluded
from the root workspace; nothing in the root may depend on it, and it reaches into the game tree only for
`crates/state-hash`): the standalone compiler (`quest-lang`), bytecode VM (`quest-vm`), stub host and CLI for
player-written quest blocks. `just quest-check | quest-test | quest-run <dir> | quest-wasm`. Spec
`docs/superpowers/specs/2026-11-16-quest-script-language-design.md`, plan
`docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md`.
**Tests in `quest/` (owner's rule, 2026-11-16):** the MVP plan's tests are the initial implementation's. Once it is
built, a test is written ONLY as a regression for a bug that was actually found — never pre-emptively, not "for
coverage", not alongside a new feature by default. Broad assurance comes from fuzzing (planned later), not from
more hand-written tests.

### Deployment

**Reaching orion:** the operator VPN (WireGuard `wg0` on the box, `10.99.0.1`, UDP 51820; this machine is the
NetworkManager connection `orion-vpn`) routes k3s's pod and Service networks with split DNS, so any Service answers
at `<svc>.<ns>.svc.cluster.local` with no port-forward, and the kube context `orion` is `https://10.99.0.1:6443`
(`just orion-vpn-status|up|down`, a new device `just orion-vpn-peer <name> <octet>`; VPN down → `just tunnel` +
`orion_context=orion-ssh`). VPN-only HTTPS names `<name>.vpn.errantry.renownandruin.com` (root `vpn-names`: a CoreDNS
rewrite to the Service + DNS-01 issuer `letsencrypt-dns`): Keycloak's admin console/API is
`https://keycloak.vpn.errantry.renownandruin.com/admin/`; Garage's admin API is `garage.garage.svc.cluster.local:3903`.
As built: `docs/superpowers/plans/2026-11-15-orion-vpn.md`.

OpenTofu (not Terraform) in `deploy/`: the k3d twin of orion (`deploy/envs/local/`, cluster `mmo`, context `k3d-mmo`:
`just twin-up`, `twin-images`, `twin-apply`, `twin-check`, `twin-down`; never the inner loop);
the `orion` k3s box (`deploy/envs/orion/`, S3 remote state, `just orion-plan|apply <root>` — always plan
first; roots `platform` → `dns` (the `renownandruin.com` records no module makes) → `garage` (+ `garage-buckets`) = https://s3.errantry.renownandruin.com, the S3-compatible
object store on the box itself (one copy, no off-box backup; telemetry and Zulip files live there) → `monitoring`, `picoshare` = https://share.errantry.renownandruin.com, `zulip` = https://chat.errantry.renownandruin.com,
`keycloak` (+ `keycloak-realm`) = https://auth.errantry.renownandruin.com, the identity provider → `registry` (the ECR repositories `mmo/login`, `mmo/server`, `mmo/web` and the cluster's pull-only IAM user) → `login` = https://login.errantry.renownandruin.com, the game's login service (two pods, its own PostgreSQL, metrics scraped; its image is `mmo/login` in ECR, pulled with a pull secret the CronJob `ecr-refresh` renews every 6 h; one tag, `latest`, and an image `latest` has moved past expires about a day later; a release is `just orion-login-push` — pushed as `latest` (git revision in an image label), set by digest with `kubectl set image`, never an apply — and the way back is `kubectl rollout undo` (works while the previous image is still in ECR or the node's cache); `just orion-login …` runs its operator commands in a pod, `orion-login-logs` follows its log; apply `registry` and push BEFORE the first apply) → `site` = https://www.errantry.renownandruin.com, the static website from `website/` with the `orion`
Keycloak login theme beside it in `website/theme/` (`just site-dev|site-test|site-theme-check`; after a Keycloak image
bump re-diff `website/theme/orion/login/template.ftl` against the new `base` theme and run `just site-theme-check`); monitoring stack
handoff `docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md`);
the OLD compose game on the box, `just deploy` (`scripts/web_release.sh` — use the pinned binaryen under
`target/tools`, never the distro's; the new game is the `world` root below, and a page from this tree shows "Play online" against a `/login` the compose game does not serve). `deploy/**/*.tfvars` and `.env` are git-ignored — never commit or
print secrets.

**The world on k8s (R5, built 2026-11-15; plan `docs/superpowers/plans/2026-11-15-r5-k8s.md`, "As built"; the
hand steps are its Phase D) — the deployment chain**: apply order `registry` (+ `just orion-world-push`) →
`platform` (carries the CloudNativePG operator) → `garage-buckets` (bucket `world-backups`) → `postgres` (cluster
`pg`, a database and role per world, a nightly dump CronJob to `world-backups` — same disk as the data, no off-box
copy) → `dns` → `login` → `monitoring` → `world` (`deploy/modules/world`, namespace `world-<id>`: Deployment
`coordinator` (it holds the login link, Secret `login-link-identity`), per key `<name>` of the MAP `var.hosts` a
Deployment, a UDP NodePort Service and a PodDisruptionBudget `host-<name>` (label `errantry/host=<name>`, all hosts
`app.kubernetes.io/component=host`), Deployment `toolbox` holding `worldctl`, Deployment
`web` = page + a `/login` proxy that forwards ONLY login's `/v1/` and `/.well-known/jwks.json` (never its `/healthz`
or `/metrics`), the cert-manager Certificate `client-tls` for `game.errantry.renownandruin.com`, the `ecr-pull`
secret). Pods never forge (`VOXEL_EMPTY_STORE=wait`): the forge runs in the toolbox, then the pods turn Ready. Each
host Deployment is 1 replica, `Recreate`; never `kubectl rollout status` a host — poll each Deployment's
`status.readyReplicas` and skip a drained one (unready for good), as the recipes do. A host is removed by
`world-drain <name>` → remove its key from `var.hosts` → apply. **Emptying a node**: a PodDisruptionBudget of 0
holds each host pod, but the coordinator, toolbox, web and login have none and go at once — on ONE node `world-stop`
first, then `kubectl drain`; with more nodes drain and remove that node's hosts first. **An apply that changes
anything the coordinator and the hosts share** (`database_url`, `service_token`, the coordinator certificate,
`save_secs`, `super_region_regions`, `ownership`) restarts them ALL at once — the hosts likely leave without a cut,
a total crash, the generation raised: such a change is `world-stop` → apply (which starts it); only a host-only
change (a `var.hosts` key, host resources) goes through cuts on a running world. **A host added after a release**
starts on `var.images.server` (`:latest`) beside the released digest: right after the apply, `kubectl set image
deploy/host-<k> host=<the coordinator's image>` (TODO.md, Elastic 1) — else the next release finds mixed images and
records no previous release. A host pod that crash-loops AFTER registering no longer rolls the world back while it
holds no land (a landless incarnation is ended, not succeeded); one that claims land during an open run, or is made
heir, and dies before the settle still does (the residual, Succession below); every check that needs no id still runs
before the registration (`roles::check_before_registration`). A release is **stop → replace images → start, never a
rolling update**; it (and `world-stop` / `world-start`) brings a drained or marked host back as a NEW host. An `orion-apply world`
owns the replica counts: applied while the world is stopped or mid-update it STARTS the world on whatever images are
set — apply only with the world running and no update under way (images are never changed by an apply).

**The world's operator recipes** (context `world_ctx`, namespace `world_ns`, default orion / `world-errantry`; the
twin reuses them with `world_ctx=k3d-mmo world_ns=world-twin`): `world-ctl <worldctl args>` (e.g. `forge --preset …`),
`world-status` (pods, host Deployments, node ports, each `/readyz` body), `world-logs|world-metrics <target>`,
`world-hosts` (the `hosts` rows), `world-drain <name> [secs]` (`claim` worlds only; marks that name's live row,
follows its log to the leave line), `world-stop` / `world-start` (hosts by label; drained and marked hosts warned
about before acting; every host leaves through its leave cut, a host that left WITHOUT one is warned about; start
waits per Deployment, never on a drained host), `world-register` (prints the login service's `world add …` line;
the link is `coordinator.<ns>.svc.cluster.local:6490`, pin from Secret `login-link-identity`). Releases: `just
orion-world-push` builds and pushes both images and prints digests;
`orion-world-update <server> <web>` = `scripts/world_update.sh` (records the refs it replaces in the world's ConfigMap
`release-history` first, prints the leave lines, refuses a world whose coordinator is at 0 (`world-start` first), and
after a failure once images began to change offers only a re-run with the same two refs); `orion-world-rollback` =
`world_update.sh --rollback` (back to the recorded previous release,
every workload — coordinator, toolbox, web, each host Deployment — to one build, same stop → set → start; refused when none is recorded or a ref is a tag; a second
rollback goes forward again; a migration is NOT undone). Backups: `orion-pg-backup-now`; `orion-pg-restore <world>
<object>` PRINTS a drill into a scratch database and the live restore (world stopped, database renamed aside and
recreated owned by `world_<id>`, `pg_restore --no-owner --role=world_<id>`, owners checked) — it runs nothing. The
twin (`twin-up|twin-images|twin-apply|twin-check|twin-down`) runs the same module on k3d: `twin-check` proves the
start, failure, leave, drain and stop/start paths (`scripts/twin_check.sh`'s header lists them); NOT owned land, a
player, a browser pin, cert-manager, a real node drain, the nightly dump or load.

**Grafana (orion) for debugging and live data:** `.env` holds `GRAFANA_SERVICE_TOKEN`, a service-account
token with the Admin role. Claude may use it to read dashboards, metrics, logs and traces through Grafana's
HTTP API at https://grafana.errantry.renownandruin.com (`Authorization: Bearer $GRAFANA_SERVICE_TOKEN`;
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
  (127.0.0.1:6480), `VOXEL_COORDINATOR_CERT_HASH_FILE`/`_CERT_SHA256`, `VOXEL_LOGIN_LINK_BIND` (the
  coordinator's listener for the login service, 127.0.0.1:6490) + `_CERT_HASH_FILE`, `VOXEL_HOST_LINK_BIND` (role
  `all` only: login→process push, 127.0.0.1:6490) + `_CERT_HASH_FILE`, `VOXEL_OWNERSHIP=allinone|static|claim`,
  `VOXEL_HOST_NAME`, `VOXEL_WORLD_ID` (in every link hello), `VOXEL_SUPER_REGION_REGIONS` (16; checked against the
  world row). `just dev-r2` runs login + coordinator + one `static` host `h0` (state in git-ignored
  `target/dev-r2/`); `just coordinator|host`, `just test-r2`.
  **The coordinator takes the same `VOXEL_OWNERSHIP` as its hosts**: its own environment decides whether it
  declares successions (`roles::coordinator_crash_policy`, from that word alone); a host's `Attach` carries its
  crash policy, and the coordinator refuses one that is not its own (the error names `VOXEL_OWNERSHIP`).
- **The coordinator's state is in the store (Elastic 2a; plan
  `docs/superpowers/plans/2026-11-16-elastic-2a-coordinator-state-in-the-store.md`, "As built")**: the process holds
  none of its own. `game::Coordinator` is a cache of the `coordinator` row (migration `0016_coordinator_rows`:
  `version`, `core`, `history`; `state` dropped): `core` = the whole durable projection (claims, memo, presence,
  `region_host`, the cut and succession runs, the calendar anchor — every field classified in `projection!`,
  `coordinator/mod.rs`: a new one does not compile until listed); `history` = History v4 (every field but `grid`;
  a load is not a restart, no blanket return), written only when `content_dirty` (a day turn with nothing due
  writes nothing); both bincode + lz4. Each runtime pass is a **transition**: apply the inputs → `take_transition`
  → `write_coordinator_later`, a compare-and-set (`WHERE epoch = $e AND version = $v`) → only after the commit
  are its outputs sent (**write-before-send**; one write in flight, `take_out` releases nothing meanwhile; an
  answer that changed nothing durable leaves without a write). No periodic save, no `gated` messages. A failed
  write keeps its outputs and retries (`COORDINATOR_SAVE_RETRY_MS`); another epoch → `Fenced`; the same epoch
  at another version → `StoreError::Conflict`: the row is re-read and written over once only if it is this
  process's own lost commit (≤ `COORDINATOR_UNACKED_MAX` remembered), else exit `EXIT_FENCED`. A process may
  hold only its links, the decoded cache, observation timers (`LeaseWatch`, retry pacing, log limits) and the
  login relay queues. Attachments, a handed-out cut decision, a declaration and an end in flight carry a random
  `ProcessBoot`: another boot's count as detached / not in flight (a restart needs no forget step). The first
  `Attach` at a newer generation parks every older-generation host's regions and detaches those still attached
  (`first_of_generation`). A store decision whose answer died with its process (a succession, a landless end)
  still takes the dead host out of the view: a run new to the start read (`successions_open`), or a dead row
  nobody here declared at the next `hosts` read (`observe_leases` → `host_ended`).
  **Store time**: the core's `now_ms` is the store's clock (`StoreClock`: sampled from each commit's `RETURNING`
  and each `hosts` read, extrapolated by the monotonic clock, never backwards). **Downtime is not caught up**:
  after `CALENDAR_GAP_MS` (30 s) without a sign of the calendar's life (a day turn, a view) it re-anchors where it
  last stood (`calendar_resume`: never earlier than a host was told; the stored history's `now` lags), and a stored cut
  / succession deadline older than the gap restarts its count at load. An older core format, another world's
  row, or no projection beside saved regions is refused at start — re-forge (`just pg-reset`, `just run
  --reset`). Counters `mmo_coordinator_transitions_total`, `mmo_coordinator_written_bytes_total{part}`,
  `mmo_coordinator_conflicts_total` and the `[coordinator] window=` line.
- **Many hosts (R3 + R4 takeover + Elastic 1 hosts by id; plans `docs/superpowers/plans/2026-11-12-r3-multi-host.md`,
  `2026-11-13-r4-takeover.md`, `2026-11-15-elastic-1-hosts-as-deployments.md` ("As built") — built; whole-branch
  reviews and the owner's hand checks pending, see `TODO.md`)**: `claim` is any number of hosts and needs role
  `host`, Postgres and the login link (`jwt` + a service token), else refused (`roles::check_claim_needs`);
  `static` is ONE host (a `static` registration beside a live row is refused, `StoreError::NotAlone`); a host under
  `allinone` or no `VOXEL_OWNERSHIP` is refused.
  - **Identity**: a host is a `game::HostId`, issued by `register_host` from `world.next_host_id` (migration
    `0014_host_ids`) when the process registers — before it claims or links — and **never reused**; nothing may
    be derived from its number (ports, file names, counts; `HOST_ID_MAX` bounds it). `HostId::SOLO` (0) is role
    `all` / SQLite. A restarted process is a NEW host that owns nothing; its old id is ended by a succession if
    it owned land, else by `end_landless_host` (row dead, no succession, generation unchanged). The coordinator
    forgets an id a `hosts` read no longer shows and nothing attached holds (`forget_absent_hosts`); a host
    forgets dead rows no open succession names (`forget_dead_hosts`).
    `VOXEL_HOST_NAME` (default empty) labels a process (logs, dev files, `hosts.name`, the Deployment), never a
    key: a registration lapses the other dirty live rows of its non-empty name at once (`seen_ms = 0`).
    `VOXEL_HOST_ORDINAL`, `VOXEL_HOSTS`, `VOXEL_ENTRY_HOST` are gone (ignored if set).
  - Hosts talk directly over peer links (`game::peer`: one `PeerSession` per other host id, `Reliable` /
    `Volatile` messages; `server/src/peer_link.rs`, QUIC over `svc-link`): crossings, halo, intents,
    forwarded input, player hand-offs. Peers are discovered from the `hosts` table (addresses, pins, `clean`):
    the higher id dials every lower live row (`PeerLinks::start`, then `refresh` from the main loop's directory
    reads). A dead host, or a departed one owning nothing, is forgotten once settled
    (`WorldManager::forget_host`); a state-bearing `Reliable` to a forgotten host is never dropped silently
    (`to_forgotten`: a transfer waits for the land's owner, anything else is an error + `peer_state_to_forgotten`).
  - One tick numbering for all hosts from the world clock anchor (`world_clock.rs`; `worldctl clock-reset`).
  - **Ownership** (`VOXEL_OWNERSHIP`, `game::ownership`): `static` = a fixed table (`Static { me, layout }`; one
    host outside the seeded tests), a crash raises the world generation (`EXIT_WORLD_ROLLBACK`, 5); `claim`
    (default of `just host` / `just coordinator` / `dev-r3` / `go-nogo-r3`; `dev-r2` is `static`) = super-regions
    claimed on demand, a dead host's land given to one heir while the others run on. A claim needs the
    claimer's live, dirty `hosts` row (a declared-dead or cleanly-left boot wins nothing); a draining host is in
    no host's claim preference and `claim_free` wins it nothing, while its own rows stay its.
  - **Lease**: a host stamps `hosts.seen_ms` every `HOST_HEARTBEAT_MS` (2 s) from its MAIN loop — never a
    timer thread (a hung loop must stop beating) — on a connection of its own (never the store's shared
    pool), each beat under a client-side deadline (`HEARTBEAT_DEADLINE_MS`), and once after each long boot
    step (`lease::beat_at_boot`); the coordinator declares it dead after `HOST_LEASE_MS`
    (10 s) of its own unchanged reads (`LeaseWatch`, on the process's MONOTONIC clock — a step of the store's clock
    lapses nobody; the store's `PG_NOW_MS` check at the declaration is the second gate; a database outage
    declares nobody). The Postgres lease
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
    save interval are not guaranteed (`succession_seam_events=` on the `[server]` line); a leave has no item
    window. **Recipients own land** (Elastic 2a): survivors are the dirty live hosts with a `super_regions` or
    `in_flight` row, plus the heirs; heirs are tried landed ∧ takes logins → takes logins → attached live. A
    lapsed landless host that is neither heir nor survivor of an open run is ended by `end_landless_host` (one
    fenced transaction: row dead, no succession, generation untouched — even while a run is open). A host
    refused a cut part for unsaved traffic with a host a directory read showed dead (land won after the
    declaration, beside the dead host) stops for a rollback (`unaccounted_dead`, exit 5): a rollback, never a
    loss, never a stuck world.
  - **The world generation is the last resort**: `static` / `allinone`, a total crash (below), a second
    LANDED (or recipient's) failure before the settle — the residual: a host that claims land during an open
    run and dies before its settle rolls the world back — / `SUCCESSION_DEADLINE_MS` (30 s) without every
    answer / a recipient's reboot in a declared run, `worldctl rollback <url> --yes` (the first process back then
    raises it once more). The coordinator's `rollback_world` is conditional on the generation it was decided
    from: asked again after its commit (a restart before its transition was written) it moves it once.
  - **Registration** (`register_host`, `world-store/src/rows.rs`; `server/src/lease.rs`): a boot never waits.
    One transaction issues the id, lapses its name's old rows, refuses a `static` host beside a live row, and —
    with nobody else live and a dirty row that is live, holds land or is an open run's predecessor (a total
    crash; an ended row is none) — abandons the open successions, takes every dirty
    row's `super_regions` and `in_flight` rows at `epoch + 1` and raises the generation (the first host back
    holds all claimed land); otherwise the new host owns nothing. Rows owning nothing are pruned there. A
    registration stuck behind a lock exits 1 after `CLAIM_LOCK_TIMEOUT_MS` (60 s).
  - Saves are coordinated world cuts in two phases (each host `prepare_cut`s its part, the coordinator
    decides; Postgres needs `max_prepared_transactions > 0`, the dev compose sets it); parts a crash left
    staged are resolved at start before anything else (`resolve_staged_cuts`; a start finishes only cuts no
    live host has a part in). An exiting host asks for a leave cut and exits 0 either way — not committed → its
    `hosts` row stays dirty (a crash to the others). A host that registers during a cut is not in it and is
    in the next (`cut_begins`, below).
  - **Leave and drain**: a cleanly departed host never returns; `LandWanted` for its land → a `Leave`
    succession at once, ALL its land to the first host that asks. `worldctl drain <url> <host id> [--in <secs>]`
    (default `DRAIN_WARNING_SECS` 300, at most `DRAIN_MAX_SECS` 7 days; a mark only moves earlier, there is no
    un-drain) sets `hosts.drain_at_ms`, which a `claim` host reads at its heartbeat: it tells its home sessions
    the deadline (`ServerPacket::HostDraining { at_tick }`), stops being the gate, and at the deadline (its
    coordinator attached) leaves through its leave cut and stays up **drained** (`/readyz` 503 `drained`, no
    heartbeat, NO store call — it must survive its row being pruned) until removed; SIGTERM still means leave
    now; a drain leave that does not commit ends the process (exit 0, dirty row). A `static` host warns once and
    ignores a mark; `worldctl drain` refuses a row not beaten within the lease and warns when it marks the last
    host not draining (no gate host: joins 503; no claimer). A stopping host tells its
    sessions why on every exit branch (`LEAVING_REFUSAL`, `FENCED_REFUSAL`, `ROLLBACK_REFUSAL`).
  - `worldctl hosts <url>` lists the rows; `worldctl retire-host <url> <host id>` (`world-store/src/operator.rs`)
    only vouches a row dead while hosts run; with no host running it is refused (the first registration takes
    the land, a total crash). `claim` only — the store does not record the policy, so it cannot refuse a
    `static` world.
  - Login links to the COORDINATOR, which relays each order (`CoordMsg::Login(LoginOrder)`) to the **gate
    host** — the lowest attached, live, not leaving, not draining id (`Coordinator::gate_host`) — and each
    answer (`HostMsg::Login`) back, keeping no per-order state (a gate that dies holding an order costs a 503
    and the client's retry). The gate resolves the player — a store read its main loop never waits on
    (`login_link::OrderResolver`, at most `LOGIN_ORDERS_IN_FLIGHT_MAX` waiting, one more is dropped) — and places them on
    the host that holds them (`world_manager/placement.rs`, relayed `SpawnOrder`s); a world request carries its
    player's hop (`HostMsg::Request { hop }`, an older placement's is refused). Regions of another host reach a
    client through an observer session: `RegionElsewhere { endpoint, ticket }` → a second connection with
    `Credential::Observer` — no body, input only as its own `origin_client`.
  - Saved terrain of other hosts arrives by LISTEN/NOTIFY per cut with a polling backstop
    (`VOXEL_TERRAIN_NOTIFY=listen|poll`; `VOXEL_WORLD_DB_DIRECT` = a direct, unpooled URL for the listener);
    a changed version drops that region's cached LOD (`stored_terrain_changed` → `invalidate_region_lod`).
  - Env: `VOXEL_PEER_BIND` (127.0.0.1:6500), `VOXEL_PEER_CERT_HASH_FILE`,
    `VOXEL_ADVERTISE_QUIC`/`_WT` (what clients are told), `VOXEL_WT_PORT`, `VOXEL_PEER_LINK_DELAY_MS`
    (measurement only); `worldctl super-region <url> <n>` (only before the first claim) + hosts run with
    `VOXEL_SUPER_REGION_REGIONS=n`.
  - `just dev-r3 <hosts> [delay]`: login + coordinator + every host under `scripts/dev_r3.sh` (restarts a host as
    a NEW host on exit 5, 137, and 3 under `claim`; knobs in its header; state `target/dev-r3/`); `just host`,
    `coordinator`, `world-hosts-dev`, `drain-dev` (arguments: `just --list`; `host` and `coordinator` take the
    same ownership word). `just go-nogo-r3` re-forges each run and seams from the client's own region→host map
    (Elastic 2a Task 1); its first valid run is a hand check (TODO.md).
- **Owner epochs & fencing**: a host claims super-regions (`super_region_of`, `game::ownership`) in Postgres
  with an epoch; every cut / region load / coordinator write is fenced on it (`StoreError::Fenced`) — a fenced
  process stops writing and exits `EXIT_FENCED` (3, also when fenced at `register_host`); a dead coordinator
  thread exits `EXIT_COORDINATOR_FAILED` (4); a world rollback exits `EXIT_WORLD_ROLLBACK` (5). No advisory
  lock any more (SQLite keeps its flock). `OpenMode::ReadOnly` handles (export, tools) write nothing and need
  the newest schema. Never add a store write that skips the fence. A world cut's commit decision must list
  every host with a non-clean `hosts` row (`two_phase::decide_cut` records an abort otherwise) — EXCEPT one
  registered after the cut began: the fenced coordinator records each cut's begin with the `next_host_id` of
  that moment (`record_cut_begin`, `cut_begins`, migration `0015_cut_begins`), and only ids at or above it may
  be missing; never relax that check without the host side (Peer link rules). Each decision prunes the older
  `cut_begins`; `cut_decisions` is never pruned (a re-attaching host's `pending_cut` looks one up). A succession
  moves rows at `epoch + 1`: the replaced process is fenced at its next heartbeat (`lease.rs`
  `pump_or_fence`) or write. A new `hosts` / `super_regions` / `successions` statement needs its lock walk
  (lock order and the one safe deadlock cycle: the module notes of `world-store/src/succession.rs`; the
  operator's in `operator.rs`; unfenced `successions` writers are the operator's rollback and a
  total-crash registration). The settles↔adopts pairing (a settled
  succession's heir part carried `adopts`) is a coordinator-side invariant the store does not enforce — it
  only detects a breach after the commit (`two_phase::report_settled_unadopted`, `cuts_settled_unadopted`).
- **Link rules**: a re-`Attach` after any reconnect reconciles the coordinator's (durable) view; requests are memoised by
  `RequestId` (idempotent resend); the Attach waits for exact people lists ("wait, never guess" — conservation
  beats latency). Login pushes `SpawnPlayer`/`Kick` to the coordinator's login listener (role `all`: to the
  process); kicks are durable until acked (login migration 0003); the coordinator relays, holding at most the
  `SessionEnded`s no login service is connected for (`LOGIN_ENDS_HELD_MAX`). It serves any number of hosts
  by id. Every link hello names the world (`LinkHello.world` = `VOXEL_WORLD_ID`; a mismatch is refused, `""`
  matches only `""`); `LINK_PROTOCOL_VERSION` is 7 — older hosts and clients do not mix with this build
  (stop, upgrade, rejoin).
- **Peer link rules** (`game::peer`, `world_manager/peers.rs`, `world_cut.rs`): a `PeerSession` is never
  replaced — `set_peer` on a known host id swaps the link and resets the connection. Resume is boot-scoped
  (`PeerHello.received_from_boot`: a count made against another boot renumbers). `Reliable` is numbered,
  kept until acked and re-sent after a drop; `Volatile` is sent only while the peer is reachable, never
  replayed. A state message the sender already accounts for in cut K travels as `Reliable::InCut { cut,
  body }`; a host marks `CutMark { K }` only once all its regions answered and its tick reached T. From a
  leave cut's `BeginCut` until `CutDone` each side holds back what it would send the other. A link that
  drops mid-cut puts that pair in count mode (`PeerSession::freeze` / `thaw`, `HostMsg::CutPeerCount`
  relayed by the coordinator) and the cut can still commit; one that drops after a side's part is complete
  by marks still aborts it. From `BeginCut` until `CutDone` a participant FREEZES its session with every host
  outside the cut (nothing sent or read), and refuses its part if it exchanged a state message with such a
  host that no committed cut of both saved (`unsaved_traffic`) — what lets `decide_cut` omit a newcomer. The
  coordinator down means no cuts, no takeover and no logins until it is back (one process until Elastic 2b) —
  but nothing it decided is lost: a restart resumes from the row.
- **Peer journal** (`game/src/peer/journal.rs`): every host under `claim` (a `static` host records nothing)
  journals the state messages it exchanged with each peer since the last committed cut — a succession's
  reports; an entity's events are ordered by its transfer stamp, a player's by the hand-off's hop, never by a
  host's own tick. Truncation rests on every
  world-cut participant calling `PeerJournal::cut_begun`: any new path that joins a cut must call it. A new
  state-bearing `Reliable` must be journalled or argued out. Every `WorldManager` field is classified in
  `host_fields!` (`world_manager/succession.rs`; per-host kind or `other`): `forget_host` and `mentions`
  destructure the manager with no `..`, so a new field does not compile until it is listed.
- **World on k8s variables (R5; each unset = today's behaviour)**: `VOXEL_COORDINATOR_ADDR` and login's `--link` accept
  a NAME (`svc_link::LinkTarget`, resolved at EVERY dial — a restarted pod's new IP is found within the DNS TTL);
  peers stay literal `ip:port`: `VOXEL_ADVERTISE_PEER` (a literal `ip:port`; required when the peer bind is a wildcard,
  a pod passes `$(POD_IP):6500`). A link identity (`VOXEL_*_CERT_HASH_FILE`'s `<file>.cert` / `.key`) may sit on a
  read-only mount (the pin file is then not written, a WARN), and a dialer's pin is derived from `<file>.cert` when
  there is no pin file (`svc_link::read_cert_hash` / `pin_beside`). `VOXEL_EMPTY_STORE=forge|wait`: `wait` = a pod
  never forges, it waits for `worldctl forge` (`mmo_waiting{why="forge"}`) and exits 0 on SIGTERM. `VOXEL_HEALTH_BIND`
  (`ip:port`, unset = no listener; `server/src/health.rs`): `/livez` fails only when a REGISTERED (or drained) process's main loop
  has not passed for `LIVEZ_STALL_MS` (30 s = 3 × the lease — the lease always acts first; it never replaces or feeds
  the lease heartbeat), `/readyz` is 200 once registered (503 body: `forge` / `drained` / `starting`),
  `/metrics` is Prometheus text with a `role` label (`host` or `coordinator`; coordinator-only series are constant on a
  host — alerts select `role="coordinator"`). `VOXEL_CLIENT_CERT` / `VOXEL_CLIENT_KEY` (PEM chain + key; the old
  `VOXEL_WT_CERT` / `_KEY` are aliases): ONE certificate for the QUIC listener and WebTransport, files WAITED for at
  start (a WARN at most every 60 s) and re-read every 60 s (a cert-manager renewal needs no restart); unset = a
  self-signed P-256 certificate valid 14 days whose hash `HostEndpoint.cert_hash` carries (single-player too).

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
  follows `PlayerRegion` from another connection only after a higher `ServerPacket::HomeHop`.
  `ServerPacket::HostDraining` counts only from the home (an observer's is ignored): the game thread sends
  `ClientUpdateEvent::HostDraining` once a second and the countdown line (`renderer/drain_notice.rs`) only
  formats it; a home change clears it. In the
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
- **Native QUIC trust** (`quic_verify.rs`, `policy_for`): a host that advertises a `cert_hash` is pinned to it; with no
  hash a non-loopback address must present a certificate the public web PKI vouches for (a typed-in "Join server"
  address of a self-signed host now fails — it retries until the 120 s deadline); verification is skipped only for
  loopback. The browser page's login is `VOXEL_LOGIN_URL`, else `{origin}/login` on an `https` non-loopback origin
  (the web image proxies it); otherwise no "Play online".
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
