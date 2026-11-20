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
forged up front (geology, climate, biomes, history). No phased plan: features land piece by piece,
roughly modelling DF. Research: `docs/superpowers/specs/2026-10-20-dwarf-fortress-research/00-synthesis.md`.
**Since 2026-11-16** one character plays in first person: quests (authored as content packages) move the
world's story through acts; a player may claim land and grow it into a settlement they run as mayor (the
commander camera is only the mayor's optional view); embark/return was deleted (2026-11-17, `docs/superpowers/plans/2026-11-17-embark-removal.md`); editor mode is
admin-only. Game design (what is decided, proposed and open): `docs/game-design.md`.
Old RTS pivot is on branch `ds/rtsPivot`. `TODO.md` tracks the current direction.

## Building and running

The root `justfile` is canonical — `just --list`. Key recipes: `build`, `run` (debug launch straight into
an adventure), `run-two` (two players adventuring at one site), `play` (real start: main menu → generator →
world map → Adventure tab), `server` + `client` (dev server on 127.0.0.1:6466), `web`, `smoke`, `test-one`,
`test`, `test-full`, `test-audit`, `fuzz`, `determinism-wasm`, `clippy`, `world-map`. **Keep the justfile in sync**: any new or
changed build/run/test command updates its recipe in the same change.

- **Toolchain**: one DATED nightly pinned in `rust-toolchain.toml` (used only to keep `target/` small —
  `-Zembed-metadata=no`, nightly build-dir layout). Bump the date deliberately (then `just smoke` +
  `just determinism-wasm`), never to a floating `nightly`. No Cranelift.
- **Compile time**: workspace feature unification is on; Slint screens are generated into three crates;
  `client` builds with `-Clto=off`. Don't add `-Zthreads` to incremental crates. Test builds keep their
  own rapier→game→… copies ON PURPOSE (tests build at region 64, play at 256).
- **CPU cap**: every `just` build/test is pinned to 80% of cores (`cap` = `nice -n 10 taskset …`;
  `cpu_percent=`, `jobs=`). Raw `cargo` is NOT pinned — go through `just` or prefix it yourself.
- **Vsync** is a player setting (Settings → Graphics, on by default, `graphics_settings.rs`); `VOXEL_VSYNC=on|off|…`
  pins it at start, and every measuring recipe/script sets `off` — a new one that reads frame times must too.
- **Interface scale** is a player setting (Settings → General, `graphics_settings.rs` → `slint_overlay::OverlayScale`;
  scale 0.75–3 in steps of 0.25, shown as a percentage of the default `UI_SCALE` 1.5 = "100 %"): the widgets are
  pixel-exact only at a total scale of 1 and 2 (the 67 % and 133 % stops), hence the steps; a
  layer draws at the wish fitted to the window (`fitted_ui_scale`). `VOXEL_UI_SCALE=<n>` pins it at start. A new
  Bevy-side use of UI coordinates divides window pixels by the layer's `ui_scale` (the minimap alone stays in window pixels).
- **GPU**: `just adapter=6700 …` / `$VOXEL_ADAPTER` pins wgpu; unpinned, `client::adapter_pick` picks the
  best adapter (never the slow NVK T600 if anything better exists). Check the `AdapterInfo` log line.
- **Debug launches** (`crates/client/src/debug_launch.rs`): `client debug world [--reset] [--site N]` forges
  `worlds/debug-world/` (the dev Planet, `ForgeParams::default()`) once and reuses it, then starts an adventure at the first populated site (or site N); add presets there. Worlds live in `worlds/<slug>/`
  (git-ignored; `terrain.sqlite` store, `world.json`, `server.log`). Menu worlds spawn their own
  single-player server (`VOXEL_SINGLE_PLAYER=1`, free port). `VOXEL_SKIP_MENU=1` connects to the dev server.

### Build/run dials

- **World scale** (client, runtime): `just world=tiny|small|full` (`world_scale.rs`), `view=<m>` view
  distance, `VOXEL_NEAR_RADIUS`, `VOXEL_LOD_RINGS`. Use small worlds on weak GPUs;
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
`docs/superpowers/specs/2026-11-16-quest-script-language-design.md` (says what was built and decided since); plans
`docs/superpowers/plans/2026-11-16-quest-script-language-mvp.md` (the MVP, built) and
`docs/superpowers/plans/2026-11-17-quest-script-completion.md` (everything after it, in order: Phase 10, leftovers,
fuzzing, the game, packages).
**Tests in `quest/` (owner's rule, 2026-11-16; the whole repository since 2026-11-19 — Testing below):** the MVP
plan's tests are the initial implementation's; since it was built only regression tests are committed. Broad assurance
comes from fuzzing (completion plan Part 3), not from more hand-written tests. Phase 10 (dialogue and quests, plan
`docs/superpowers/plans/2026-11-17-quest-script-completion.md` Part 1) was initial implementation too (Dawid,
2026-11-17) and carried its own tests; it closed with its corpus (Task 49, 2026-11-18), so the regression-only rule
applies again.

### Deployment

**Reaching orion:** the operator VPN (WireGuard `wg0` on the box, `10.99.0.1`, UDP 51820; this machine is the
NetworkManager connection `orion-vpn`) routes k3s's pod and Service networks with split DNS, so any Service answers
at `<svc>.<ns>.svc.cluster.local` with no port-forward, and the kube context `orion` is `https://10.99.0.1:6443`
(`just orion-vpn-status|up|down`, a new device `just orion-vpn-peer <name> <octet>`; VPN down → `just tunnel` +
`orion_context=orion-ssh`). VPN-only HTTPS names `<name>.vpn.errantry.renownandruin.com` (root `vpn-names`: a CoreDNS
rewrite to the Service + DNS-01 issuer `letsencrypt-dns`): Keycloak's admin console/API is
`https://keycloak.vpn.errantry.renownandruin.com/admin/`; Grafana is `http://grafana.vpn.errantry.renownandruin.com` (VPN only,
monitoring module `grafana_public = false`); Garage's admin API is `garage.garage.svc.cluster.local:3903`.
As built: `docs/superpowers/plans/2026-11-15-orion-vpn.md`.

**Deploying is two commands** (`docs/superpowers/plans/2026-11-15-two-command-deploy.md`, "As built"):
`just deploy --infra` plans and applies every OpenTofu root in order (saved plans; confirmation for
changes; `--yes`, `--dry-run`, `--only <root>`), and `just deploy --game` builds, pushes and releases
login, world and panel, forges a first world and registers it with login. Both flags run infra then
game; `just target=twin deploy …` does the same on the k3d twin. The stdlib-Python orchestrator is
`scripts/deploy/` (`just deploy-test` runs its policy-compliant static checks); a local flock keeps two runs apart. `bootstrap`
(the state bucket) stays manual. The `orion-*` / `twin-*` recipes below are escape hatches for doing
one step by hand.

OpenTofu (not Terraform) in `deploy/`: the k3d twin of orion (`deploy/envs/local/`, cluster `mmo`, context `k3d-mmo`:
`just twin-up`, `twin-images`, `twin-apply`, `twin-check`, `twin-down`; never the inner loop);
the `orion` k3s box (`deploy/envs/orion/`, S3 remote state, `just orion-plan|apply <root>` — always plan
first; roots `platform` → `dns` (the `renownandruin.com` records no module makes) → `garage` (+ `garage-buckets`) = https://s3.errantry.renownandruin.com, the S3-compatible
object store on the box itself (one copy, no off-box backup; telemetry and Zulip files live there) → `monitoring`, `picoshare` = https://share.errantry.renownandruin.com, `zulip` = https://chat.errantry.renownandruin.com,
`keycloak` (+ `keycloak-realm`) = https://auth.errantry.renownandruin.com, the identity provider → `registry` (the ECR repositories `mmo/login`, `mmo/server`, `mmo/web` and the cluster's pull-only IAM user) → `login` = https://login.errantry.renownandruin.com, the game's login service (two pods, the retained old PostgreSQL until its operator move to `pg-management`, metrics scraped; its image is `mmo/login` in ECR, pulled with a pull secret the CronJob `ecr-refresh` renews every 6 h; one tag, `latest`, and an image `latest` has moved past expires about a day later; a release is `just orion-login-push` — pushed as `latest` (git revision in an image label), set by digest with `kubectl set image`, never an apply — and the way back is `kubectl rollout undo` (works while the previous image is still in ECR or the node's cache); `just orion-login …` runs its operator commands in a pod, `orion-login-logs` follows its log; apply `registry` and push BEFORE the first apply) → `site` = https://www.errantry.renownandruin.com, the static website from `website/` with the `orion`
Keycloak login theme beside it in `website/theme/` (`just site-dev|site-test|site-theme-check`; after a Keycloak image
bump re-diff `website/theme/orion/login/template.ftl` against the new `base` theme and run `just site-theme-check`); monitoring stack
handoff `docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md`). The old compose game's
deploy path is gone; any compose containers still on the box are the owner's hand step. `web-release`
(`scripts/web_release.sh`, with the pinned binaryen under `target/tools`) remains for the browser bundle.
`deploy/**/*.tfvars` and `.env` are git-ignored — never commit or print secrets.

PostgreSQL consolidation (Elastic 6): orion has two CloudNativePG clusters — `pg` in
namespace `postgres` for one region/world database per world, and `pg-management` in
namespace `pg-management` for login and Keycloak. Management has its own Garage tenant,
bucket `management-backups`, role Secrets, CronJob and alert (`PostgresBackupFailed`).
Zulip deliberately keeps its chart-bundled PostgreSQL and its full-text dictionaries.
Infrastructure order is `registry` → `platform` → `garage-buckets` (including the
management bucket) → `postgres` → `pg-management` → `dns` → `keycloak` → `login` → the
remaining roots; the deploy driver waits for the two management `Database` CRs after
`pg-management`. `orion-pg-mgmt-backup-now` runs the dump manually; `pg-move <ctx> <ns>
<svc>` only prints the stopped-writer dump/restore, ownership/count checks, saved-plan
apply, verification and rollback. `twin-pg-move-login` is the twin-only destructive
rehearsal. Production moves were completed login first, then Keycloak. The Orion login and
Keycloak roots now default to `use_management_db=true` and `own_postgres="absent"`,
so a normal `just deploy --infra` stays on `pg-management` and does not recreate
the retired StatefulSets. The old volumes and `*-db-old` Secrets were explicitly
removed by the owner; the twin remains on the rehearsal defaults until a move is
run there. Old-image/old-role cleanup is complete, but the dump restore drill and
ordinary future backups remain part of operations.

**The world on k8s (R5, built 2026-11-15; plan `docs/superpowers/plans/2026-11-15-r5-k8s.md`, "As built"; the
hand steps are its Phase D) — the deployment chain**: apply order `registry` (+ `just orion-world-push`) →
`platform` (carries the CloudNativePG operator) → `garage-buckets` (buckets `world-backups` and
`management-backups`) → `postgres` (region/world cluster `pg`) + `pg-management` (login and
Keycloak, separate roles/Secrets/volume/dump) → `dns` → `login` → `monitoring` → `world`
(`deploy/modules/world`, namespace `world-<id>`: Deployment
`coordinator` (`var.coordinator_replicas` replicas, default 2, `Recreate`, every replica on the same identity
Secret `coordinator-identity` — one pin for hosts and login — behind the Service `coordinator`: ClusterIP, TCP 6480 =
the HTTPS API (`VOXEL_COORDINATOR_BIND=0.0.0.0:6480`) + the health port, each connection to a Ready replica — a world
created before the coordinator HTTP API keeps its Service HEADLESS on a plain apply (its ports change to TCP in
place; the provider keeps `cluster_ip`) until `-replace=module.world.kubernetes_service_v1.coordinator`; no Secret `login-link-identity`; a
PodDisruptionBudget `min_available = 1` only above one replica), Deployment `fleet` (the reconciler, below; its
ServiceAccount, Role and RoleBinding `fleet`), the ConfigMap `host-template` (key `template.json`: a host's Deployment,
UDP NodePort Service and PodDisruptionBudget as one JSON template with placeholders, rendered by `hosts.tf`),
Deployment `toolbox` holding `worldctl`, Deployment
`web` = page + a `/login` proxy that forwards ONLY login's `/v1/` and `/.well-known/jwks.json` (never its `/healthz`
or `/metrics`), the cert-manager Certificate `client-tls` for `game.errantry.renownandruin.com`, the `ecr-pull`
secret). Pods never forge (`VOXEL_EMPTY_STORE=wait`): the forge runs in the toolbox, then the pods turn Ready.
**Hosts are not OpenTofu's** (Elastic 4, plan `docs/superpowers/plans/2026-11-19-elastic-4-reconciler-scaling.md`;
`var.hosts` is gone, `var.fleet` = `min` (default 1), `max`, `port_base`, the optional pace dials): the coordinator's
decisions write `desired_hosts` ("The fleet", below) and the reconciler makes one Deployment, Service and
PodDisruptionBudget `host-<name>` per row (names `h1`, `h2`, … issued by the store, never reused; labels
`errantry/fleet=<world id>`, `errantry/host=<name>`, `app.kubernetes.io/component=host`): the Deployment at 1 replica
on the image of the `coordinator` container of Deployment `coordinator` AT THAT MOMENT (the released digest — a host
added after a release never runs another build), the Service on node ports `port_base + 2·slot` (QUIC) and `+ 1`
(WebTransport) — output `client_ports`, the range `port_base … port_base + 2·max − 1`, must be open to clients.
**The reconciler** (`VOXEL_ROLE=fleet`, `server/src/fleet.rs`, a pass every `FLEET_PASS_MS`, 2 s) writes nothing to
the store (`OpenMode::ReadOnly`) and never talks to the coordinator (nor the coordinator to the Kubernetes API); it
creates and re-templates nothing while the `coordinator` Deployment's `spec.replicas` is 0 (it still deletes); it
never patches an existing Deployment's replicas or image (the recipes own those); it re-templates a host Deployment
only at 0 replicas (a `PUT` with the read `resourceVersion`, its own image kept) and never a Service or a budget; a
draining row gets nothing created; an object labelled for the world without `errantry/host` is never touched; an
EMPTY `desired_hosts` read deletes every labelled host object (each pod leaves by its leave cut: a disruption, never
a loss), a FAILED read deletes and creates nothing. ONE replica, `Recreate` — which orders a rollout, not a deleted
pod's replacement: that one can overlap the old pod for its grace period, harmlessly, because a pass reads the
desired rows AFTER its object lists (an object whose name the later read lacks was really removed). Its Role can
create Deployments, so it can mount any Secret of the namespace (the service token, the link identities' keys), and
its `delete` is not limited to host objects (the code deletes only those labelled for the world with
`errantry/host`). Each
host Deployment is 1 replica, `Recreate`; never `kubectl rollout status` a host — poll each Deployment's
`status.readyReplicas` and skip a drained one (unready for good), as the recipes do. The operator removes a host
ONLY by `world-drain <name>`, nothing to apply (the coordinator also drains hosts itself: rules 6 and 7, "The
fleet"): the coordinator marks its desired row draining, removes the row once nothing of the name runs (a leaver
that kept land or in-flight rows included: its `Leave` succession needs only its clean `hosts` row), the reconciler
deletes its objects (below the minimum a new name is added). The coordinator's replicas move together: never
`kubectl rollout status` it either — `world-stop` records the count in the Deployment annotation `errantry/replicas`
and scales it to 0, `world-start` restores it (absent: 2) and waits for every replica's `/readyz`, `world_update.sh`
stops and starts every replica together (no two builds share the row). **Emptying a node**: a PodDisruptionBudget
of 0 holds each host pod and the coordinator's keeps one replica (which cannot move on ONE node, so the drain cannot
finish); toolbox, web, login and `fleet` have none and go at once — on ONE node `world-stop` first, then `kubectl drain`;
with more nodes `kubectl cordon` the node, then `world-drain` its hosts (a replacement the coordinator adds below the
minimum is scheduled elsewhere), then `kubectl drain`. **An apply starts no host**: it restarts the
coordinator on a change of its own, and a change of the host template (env, resources, the secrets' checksum)
reaches a running host only at its next stop and start (re-templated at 0 replicas). **An apply that changes
anything the coordinator and the hosts share** (`database_url`, `service_token`, the coordinator certificate,
`save_secs`, `super_region_regions`, `ownership`) is `world-stop` → apply → `world-start`. **The FIRST apply of this
build on a world deployed before it** (before Elastic 4 or before the coordinator HTTP API; NOT RUN; ONE merged
runbook with a reason per step: `TODO.md`, "The coordinator as an HTTP service" — it supersedes Elastic 4's hand
check 1): an apply never changes an image, so the build reaches the existing workloads only by hand, and the world is
re-forged into a NEW empty database (`worldctl forge` refuses a store that holds a world; worlds are never migrated;
`COORDINATOR_STATE_VERSION` 8): `orion-world-push` → `world-stop` (no `fleet` yet: skipped) → `orion-plan` /
`orion-apply world -replace=module.world.kubernetes_service_v1.coordinator` (the Service becomes ClusterIP TCP, Secret
`login-link-identity` goes; before Elastic 4 also the stopped `var.hosts` objects go and `fleet`, its Role and
`host-template` come; it scales the coordinator back up on its OLD image and `fleet` to 1 on `var.images.server`) →
`world-stop` again → delete the old reconciled host objects (`-l errantry/fleet=<world id>`) and `kubectl set image`
the pushed digests on `coordinator`, `toolbox`, `fleet` and `web` (not `world_update.sh`: it refuses a stopped world,
and its start step waits for `/readyz` 200 that an unforged store never gives) → `orion-login-push` (link protocol
12: login calls the coordinator's HTTPS API and an old login service is refused, a new one finds no old coordinator
to call — so only now, with no coordinator running; native clients need this build too) → a new empty database (not
written yet; do not start the runbook before it is) → `world-ctl forge …` from the new toolbox → `world-start` (the
coordinator adds the minimum, the reconciler creates `h1`) → `world-register` and the `world add …` it prints (the
registry row still names the old link). A host pod that crash-loops AFTER registering no longer rolls the world back while it
holds no land (a landless incarnation is ended, not succeeded); one that is assigned land during an open run, or is
made heir, and dies before the settle still does (the residual, Succession below); every check that needs no id still
runs before the registration (`roles::check_before_registration`). A release is **stop → replace images → start, never a
rolling update**; it (and `world-stop`) stops a drained or marked host for good: the coordinator drains and removes
its desired row (rules 2 / 2b, 3) and the reconciler deletes its Deployment — usually before the start scales it; one
still there runs as a NEW host for seconds (marked at once, deleted with its row). `world-start` and a release's
start wait for the reconciler's `/readyz` 200 at most their timeout, then WARN and start the host Deployments that
exist (one failing host object never keeps the world down). An `orion-apply world`
owns the coordinator's and `fleet`'s replica counts (not the hosts'): applied while the world is stopped or mid-update
it STARTS the coordinator and the reconciler on whatever images are set (the host Deployments at 0 stay there until
`world-start`, but a wanted row with no Deployment is then created at 1 replica) — apply only with the world running
and no update under way (images are never changed by an apply).

**The world's operator recipes** (context `world_ctx`, namespace `world_ns`, default orion / `world-errantry`; the
twin reuses them with `world_ctx=k3d-mmo world_ns=world-twin`): `world-ctl <worldctl args>` (e.g. `forge --preset …`),
`world-status` (pods, host Deployments, the desired rows (`worldctl desired`), node ports, each `/readyz` body; the
coordinator's WANT / READY / RECORDED count and per replica its ticker flag, live replicas and conflicts and its fleet
counters (wanted, draining, added, drained, removed); the reconciler's (desired, host Deployments, created, deleted,
errors, pass age); per host pod `land N, released M`),
`world-logs|world-metrics <target>`,
`world-hosts` (the `hosts` rows, then `worldctl desired`), `world-drain <name> [secs]` (`claim` worlds only; marks that
name's live row, follows its log to the leave line; the coordinator and the reconciler then remove the host),
`world-stop` / `world-start` (hosts by label; drained and marked hosts warned about before acting; every host leaves
through its leave cut, a host that left WITHOUT one is warned about; the coordinator's count recorded / restored as
above; start waits per Deployment, never on a drained host; a world with no host Deployment stops and starts too).
`world-stop` scales `fleet` to 0 FIRST and waits until its pod is gone (the reconciler's own rule does not cover the
hosts' leave while the coordinator is still up), then the hosts (listed after), then the coordinator; `world-start`
the coordinator first (every replica's `/readyz`), then `fleet` to 1 and its `/readyz` 200 (a pass that saw the
coordinator up: stopped hosts re-templated, missing ones created on the coordinator's image), then every host
Deployment at 0 to 1. A failed stop leaves `fleet` at 0 (`world-start` brings it back); a world with no `fleet`
Deployment (from before Elastic 4) skips that step. `fleet <port_base>` runs the reconciler from the checkout against
`world_ctx` / `world_ns` through a `kubectl proxy` with your credentials (`VOXEL_WORLD_DB` required) — never beside
the world's own `fleet` Deployment.
`world-register` (prints the login service's `world add <id> --name … --region eu-central-1 --link
https://coordinator.<ns>.svc.cluster.local:6480 --link-cert <pin>` line, the pin = the SHA-256 of Secret
`coordinator-identity`'s certificate; it changes nothing). Releases: `just
orion-world-push` builds and pushes both images and prints digests;
`orion-world-update <server> <web>` = `scripts/world_update.sh` (records the refs it replaces in the world's ConfigMap
`release-history` first, prints the leave lines, refuses a world whose coordinator is at 0 (`world-start` first), and
after a failure once images began to change offers only a re-run with the same two refs; the steps of `world-stop`,
every image set — coordinator, toolbox, `fleet`, each host Deployment at 0 — then the steps of `world-start`; a
world with no host Deployment is updated too); `orion-world-rollback` =
`world_update.sh --rollback` (back to the recorded previous release,
every workload — coordinator, toolbox, `fleet`, web, each host Deployment — to one build, same stop → set → start; refused when none is recorded or a ref is a tag; a second
rollback goes forward again; a migration is NOT undone). Backups: `orion-pg-backup-now`; `orion-pg-restore <world>
<object>` PRINTS a drill into a scratch database and the live restore (world stopped, database renamed aside and
recreated owned by `world_<id>`, `pg_restore --no-owner --role=world_<id>`, owners checked) — it runs nothing. The
twin (`twin-up|twin-images|twin-apply|twin-check|twin-down`) runs the same module on k3d (`twin-up` creates the
cluster with kubelet's image GC at 98 / 95 % disk use — it applies only to a newly created cluster: `twin-down`
first): `twin-check` proves the
start, failure, leave, drain and stop/start paths (`scripts/twin_check.sh`'s header lists them; since the coordinator
HTTP API phase 7 — a replica killed mid-play — waits for both hosts Ready, cuts committing and a join 200, every host
re-attached through the Service, no log line); phase 7d cuts the host that does not own 7c's land from the
coordinator's TCP 6480 for 30 s (a NetworkPolicy + a conntrack flush in the k3d node): PASS = a replica logs the host
`stalled`, its pod goes not Ready, a join answers 200 with the land still on the OTHER host, and with the policy gone
it is Ready again, a cut commits, its pod untouched; `SKIP 7d` = no stall line and the pod stayed Ready (the cut took
no visible effect); INCONCLUSIVE (a SKIP too) = not Ready but no stall line (no mailbox row waited for it within 80 s,
or the coordinator does not detach: read the logs) — neither fails the run, its last line says `PASS (7d skipped)`;
7d does not prove a cut host noticed without a waiting row, a host cut from one replica only, or a browser's join; and,
since Elastic 3
(phase 7c, the twin root sets the module's `land_idle_ms` = 20 s), land assigned to a join's placement, released idle
by a committed cut part and assigned again (no succession, generation unchanged); since Elastic 4 (the twin's fleet
is `min = max = 2`: k3d maps exactly the node ports 30466–30469 of two slots) the hosts appearing by themselves after
the forge on the coordinator's image and their slots' ports (2c), a drained host removed and a new name on the freed
slot (9), stop/start with the reconciler and a stray labelled Deployment deleted (10), the reconciler's pod deleted
with nothing changed (10b), the world taken to zero hosts one at a time with `VOXEL_FLEET_MIN=0` (12) and a join that
answers 503 `world_starting`, then gets in on one new host (13; the seconds recorded in `cold-start.txt`, images
already on the node); NOT a connected player, load-driven scale-up, a host
dying while it owns land (a `Crash` succession), a browser pin, cert-manager, a real node drain, the nightly dump,
the firewall's port range, an image pull at a cold start, or load.

**The world panel (R5b first slice, `crates/panel`, plan `docs/superpowers/plans/2026-11-15-r5b-world-panel.md`)**: an
operator page over every world namespace (label `app.kubernetes.io/name=world`) — state, pods, `/readyz`, metrics,
forge Jobs, release history — that starts, stops and forges as the `world-*` recipes do. `panel/src/ops.rs` mirrors
`world-start` / `world-stop` step for step: **change one, change the other**. Port-forward only, no login
(`just orion-panel`, `twin-panel`; `just panel ctx=<ctx>` runs it from the checkout through `kubectl proxy` with your
credentials); a world grants it access with the world module's `panel` RoleBinding (ClusterRole `world-panel`). It
speaks to the API server with reqwest (ring), not kube-rs. Releases `just orion-panel-push` (by digest, as login).

**Grafana (orion) for debugging and live data:** `.env` holds `GRAFANA_SERVICE_TOKEN`, a service-account
token with the Admin role. Claude may use it to read dashboards, metrics, logs and traces through Grafana's
HTTP API at http://grafana.vpn.errantry.renownandruin.com — VPN-only since 2026-11-15, no public Ingress or record (`Authorization: Bearer $GRAFANA_SERVICE_TOKEN`;
data sources: Prometheus (local), Thanos, Loki, Tempo, Pyroscope, Alertmanager; `just orion-grafana`
gives a local port-forward). Load it with `set -a; . ./.env; set +a` inside the one command that needs it; never echo it,
pass it on a command line that gets logged, or write it anywhere else. Reading is fine without asking;
**ask before changing anything in Grafana** (dashboards, alerts, data sources, users, tokens) — durable
changes belong in `deploy/modules/monitoring/`, not in the UI.

The `Errantry telemetry` dashboard is the operator entry point for correlated traces, logs and CPU profiles
(`deploy/modules/monitoring/dashboards/telemetry.json`). Select the environment, cluster, service
(`errantry-login` / `errantry-world`), role (`login` / `coordinator` / `fleet` / `host`), world and pod before
opening a trace. The world dashboard links here for its selected `world-*` namespace while keeping the time
range. Paste the 32-digit ID from Tempo into `trace_id`; logs and the Tempo-to-Loki derived field use the exact
`trace_id=<hex>` syntax. A trace's Explore page retains trace-to-log and trace-to-profile links. Profile views
show the selected pod beside a role comparison for the same world; align the dashboard range with the trace.
Tempo's p50/p95/p99 panels use TraceQL metrics and require the backend metrics generator to be enabled.

Telemetry is non-authoritative: health/readiness, simulation, persistence and fencing never depend on an
exporter. `OTEL_EXPORTER_OTLP_ENDPOINT` / `OTEL_EXPORTER_OTLP_PROTOCOL` select the Alloy OTLP HTTP ingest path;
`OTEL_TRACES_SAMPLER` and `OTEL_TRACES_SAMPLER_ARG` select parent-based head sampling. To force one diagnostic
deployment to be sampled, set `OTEL_TRACES_SAMPLER=always_on` temporarily, restart only that workload, inspect
the bounded interval, then restore its role default. `OTEL_BSP_*` bounds the batch queue. Continuous CPU profiles
are runtime-controlled by `PYROSCOPE`, `PYROSCOPE_SERVER` and `PYROSCOPE_SAMPLE_RATE`; disabling `PYROSCOPE` or
setting the sampler to `always_off` is the first telemetry rollback, not a game-binary rollback.
`VOXEL_TELEMETRY_ENVIRONMENT` / `VOXEL_TELEMETRY_CLUSTER` and Downward-API pod/namespace values supply resource
identity; login has no world id, while coordinator, fleet and host workloads carry the world id.

The correlation allowlist is deliberately low-cardinality: service, role, environment, cluster, namespace, world,
pod, build revision, fixed route/operation/status categories. Never add player/account/entity identifiers, host
ordinal or numeric id, region coordinates, raw URLs/queries, certificate pins, service/Kubernetes tokens, SQL or
binds, request/response bodies, issuer tokens or claims. Profile labels mirror only service, role, environment,
cluster, namespace, world, pod and revision. Alloy already forwards the OTLP resource envelope unchanged and must
retain its batch path and bounded resources; no tail sampler is part of this phase.

Roll out dashboards/datasources through a separate `monitoring` plan/apply and review the resulting diff before any
workload image release. Disable telemetry at runtime first if the backend/exporter fails. A code release that
changes the tracing protocol is a coordinated login/world stop-update-start; Grafana JSON and datasource changes
can be rolled back independently through OpenTofu. Never make live Grafana configuration changes or use a
telemetry outage to bypass world release or fencing runbooks.

## Testing

**The only tests ever committed are regression tests** (owner's rule, 2026-11-19 — the whole repository, as `quest/`
since 2026-11-16): a test reproduces a bug that was actually SEEN — in play or in a log (a runtime log, a probe's or a
hand check's output) — and carries `/// Regression: <the bug> (<where it was seen, the fix>)` (`//` in node, `#` in
python) right above it. Temporary tests written while working (to drive a change, to check an idea) are fine and are
deleted before the commit; never commit one pre-emptively, "for coverage", or alongside a new feature. A bug found by
a review or by another test is not a regression of this kind; a fuzzer's crash is (its output is the log). The other committed kind is a **gate** (`/// Gate:
<what it guards>`): the determinism and quality goldens (`wasm_determinism`, `physics_goldens::`, `state_hash_wasm`,
`scene_hash_golden`, `forge_quality`, the `DensityFn` mirror) and `physics-next`'s pins of the vendored forks'
`// mmo-fork` hunks; a new gate needs the owner. `just test-audit` (first step of `smoke`) fails on any test without
one of the two markers (vendored forks excepted). Broad assurance comes from fuzzing (planned, `TODO.md`), not from
more hand-written tests. Inventory and plan: `docs/superpowers/plans/2026-11-19-regression-only-tests.md`.

Verify with:

- `just test-one <crate> "<cargo test args>"` for exactly the touched tests. Integration tests are ONE
  `it` binary per crate (`--test it <file>::`); files that touch process-global state (`HASH_MODE`, the
  forged world, a logger, wasm32) keep their own `[[test]]` target — a new such file needs one. Server's
  `takeover` target needs `--features test-hooks` and Postgres (`just test-r4`; `crates/pg-test-support`,
  `MMO_TEST_DATABASE_URL`).
- `just smoke` — the default sanity check, hard-limited to 60 s. It only type-checks the client; running
  client tests (`just test-one client "--lib <filter>"`) is a slow opt-in — **ask first**.
- `just determinism-wasm` after touching sim math, the physics forks, or anything hashed/serialized.
  `physics_goldens::` pins physics behaviour (re-record only on purpose).
- A test slower than 100 ms is tagged `#[ignore = "slow: >100ms — run via just test-full"]`. `just test` runs every
  fast test everywhere (both workspaces, the vendored forks' upstream subset, the website's node test),
  `just test-full` adds the slow ones, the wasm32 determinism runs (`determinism-wasm`,
  `quest-wasm`) and the takeover rig against the dev Postgres. Both keep going after a failing stage and end with a PASS/FAIL line per stage; run them when the user
  asks or a change is wide — say so first. Fuzzing is never part of either: `just fuzz [secs]` replays every kept crash input,
  then runs every fuzz target for `secs` seconds (default 60) each.
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
- `chunks.top` (migration `0021_chunk_tops`, `game::map_tile::chunk_top`, `FORMAT_CHUNK_TOP`) is written in the
  chunk's own `INSERT … ON CONFLICT` of the cut part — never a separate row; a NULL top is derived from `data`.
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
- **Role split (R2, `VOXEL_ROLE=all|host|coordinator|fleet` (`fleet`: Elastic 4's reconciler, Postgres and
  Kubernetes only — "The world on k8s" above), default `all` = one process as before; plan
  `docs/superpowers/plans/2026-11-07-r2b-role-split.md`)**: the coordinator (`game::coordinator`, claims,
  adventures, history, sites, the coordinator row) and the host (`WorldManager` + regions) talk only via
  `HostMsg`/`CoordMsg` over a `WorldLink` — `InlineWorldLink`/channel in `all`, `HttpWorldLink`
  (`server/src/world_http.rs`) over the coordinator's HTTPS API (`server/src/coordinator_api.rs`; shared
  `VOXEL_SERVICE_TOKEN[_FILE]`, the coordinator's pinned self-signed cert; "Link rules") when split. Split roles
  need Postgres (`VOXEL_WORLD_DB`) and refuse anything else. Env: `VOXEL_COORDINATOR_BIND` (the API's TCP bind,
  127.0.0.1:6480), `VOXEL_COORDINATOR_ADDR` (a host's: a URL `https://host:port` or a comma list of them, default
  `https://127.0.0.1:6480`; a bare `host:port` is refused), `VOXEL_COORDINATOR_CERT_HASH_FILE`/`_CERT_SHA256`,
  `VOXEL_HOST_LINK_BIND` (role `all` only: the process serves the login service's two routes itself, HTTPS,
  127.0.0.1:6490) + `_CERT_HASH_FILE` (`VOXEL_LOGIN_LINK_BIND` / `_CERT_HASH_FILE` are removed: login calls the
  coordinator's API), `VOXEL_OWNERSHIP=allinone|static|claim`, `VOXEL_HOST_NAME`, `VOXEL_WORLD_ID` (every API
  request's `X-World`, the peer hello's world), `VOXEL_SUPER_REGION_REGIONS` (16; checked against the world row). `just dev-r2` runs login + coordinator + one `static` host `h0` (state in git-ignored
  `target/dev-r2/`); `just coordinator|host`.
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
  → `write_transition_later`, one transaction: a compare-and-set (`WHERE version = $v`; no epoch any more), the
  transition's store decisions, its mailbox and login-answer rows (below) → only after the commit
  are its outputs sent (role `all`) or readable (the rows) (**write-before-send**; one write in flight, `take_out` releases nothing meanwhile; an
  answer that changed nothing durable leaves without a write). No periodic save, no `gated` messages. A failed
  write keeps its outputs and retries (`COORDINATOR_SAVE_RETRY_MS`); a `StoreError::Conflict` is this process's
  own lost commit if the row is at `version + 1` with a core hash among ≤ `COORDINATOR_UNACKED_MAX` remembered
  failed writes, else the core REBASES (below); only a failed write that may be in the row, or a gone / undecodable
  row, exits `EXIT_FENCED`. A process may hold only its API listener and the calls waiting on it (batch answers,
  login orders until their transition commits), the decoded cache, its input log and observation timers (`LeaseWatch`,
  `ReplicaWatch`, retry pacing, log limits). A replica is a `ReplicaId` (issued by the store per process start, never
  reused); an attachment names no replica (a host's boot, below); a store decision's mark is a `CallStamp { replica, version }` (an answer with another stamp
  changes nothing; a lost one is looked up, never re-issued blind). The first
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
- **Several coordinator replicas over one row (Elastic 2b; plan
  `docs/superpowers/plans/2026-11-17-elastic-2b-coordinator-replicas.md`, "As built"; the coordinator as an HTTP
  service: plan `docs/superpowers/plans/2026-11-19-coordinator-http-api.md`, "As built")**: any number of coordinator
  processes, each a cache of the same row, any of them serving any host or login request; the row's `version`
  compare-and-set is the ONLY fence — never a replica count, a budget, a lease, a sticky route or the ticker
  preference. Migrations `0017_coordinator_replicas` (`coordinator_replicas`, `outbox`, `coordinator.next_replica`,
  `coordinator.history_version`) and `0020_mailbox` (`outbox.at_ms`, `login_outbox`). `COORDINATOR_STATE_VERSION` is
  9 since Elastic 5 (the move runs, `CutRun.moves`); 8 was the coordinator HTTP API (attachments by boot, with their
  store time; 7 lived only inside that plan); a row
  of an older format beside saved regions is refused: re-forge (`just pg-reset`, `just run --reset`).
  - **Replicas**: `register_replica` issues a `ReplicaId`; a split replica beats `coordinator_replicas.seen_ms` every
    `REPLICA_BEAT_MS` (2 s) from its main loop. Its first input is `Input::ReplicaJoined { me, alone }` (`alone`: no
    other row beaten within `REPLICA_LEASE_MS`, 10 s — a total replica crash; it drops the LOWER ids). The lowest
    replica its own `ReplicaWatch` shows live applies `ReplicaLapsed(r)` after a lease of unchanged reads (or for a
    row a read shows dead / pruned, `gone`); its `EndReplica` decision marks the row dead. A replica whose beat finds
    its row dead, or that is not in the live set of the core it holds, exits `EXIT_FENCED` without writing. Its
    `/readyz` is 200 once its `ReplicaJoined` committed. SQLite (role `all`) registers its one replica and never beats.
  - **Input log and rebase** (`coordinator/input.rs`): every input goes through `Coordinator::apply` and stays in the
    log, with its `now_ms`, until its transition commits; a conflict that is not our own lost commit → `rebase(row)`:
    the projection replaced, `out` cleared, the log re-applied at `max(now, clock_floor())`; retried
    `COORDINATOR_CONFLICT_RETRIES` (8) in a row, then after `COORDINATOR_SAVE_RETRY_MS` — never an exit. NEVER logged:
    login orders (the runtime holds each until the transition carrying its row commits and issues it again after a
    rebase), the ticker's reads (`HostsRead`, `SuccessionsOpen`, `LandOwners`); `HostMsg::Login` IS logged (its only
    effect a `login_outbox` row); a new input
    is classified (replayed or not, why) as in the plan's decision 3 table. Process fields are `kept` / `reset` in
    `projection!`. The calendar's day only moves forward. **A superseded link's messages never overtake**: the
    projection keeps each host's newest applied `Attach` as `(boot, epoch)` (`WorldCuts::attach_epochs`; the host's
    `epoch` grows per link up), every logged host message carries the `(boot, epoch)` its batch was stamped with
    (`link_opened`), and an older `Attach` of the same boot — or a message of its link, or of the newest link while
    that attachment is DETACHED (a detached pair is never attached again) — is dropped at its first apply AND at a
    replay, and leaves the log (else a replica's delayed write re-stamps a host whose later batches another replica
    applied).
  - **Store decisions run inside the transition** (`world-store/src/coordinator_tx.rs`): the CAS `UPDATE` first (it
    locks the row), then each `StoreDecision` (`DecideCut`, `BeginCut` = `record_cut_begin`, `Declare`,
    `EndLandless`, `Rollback`, `EndReplica`, `AssignLand` — Elastic 3, below) in its own SAVEPOINT (a refusal is that
    decision's answer; the core still commits), the mailbox drops, the mailbox and `login_outbox` inserts, the login
    answers' prune, `pg_notify('coordinator_row', version)`: issuing a
    decision IS committing it. Answers come back as logged `Input::Decision`. Post-commit effects (`FinishCut`:
    `COMMIT PREPARED` of the parts, THEN the cut notice) run after the commit and are retried. Every statement that
    locks `coordinator` takes it FIRST (lock walk: `succession.rs`'s notes). SQLite: a transition with decisions,
    mailbox or login rows is `Unsupported`.
  - **The ticker** (`ticker()` = the lowest id of the core's durable live set `replicas`) ALONE runs `Maintain`, the
    `hosts` read (leases, expected hosts, world tick, load; with the mailbox read beside it, below), every `take_*`
    hand-out, land lookups (the owners read,
    `Input::LandOwners`, never logged — a rebased replica re-derives it from the hosts' next ask) and succession
    re-reads; the others apply host batches, their own lifecycle and answers bearing their stamp. A
    dead ticker stalls timed work ≤ one replica lease + one read; a new ticker's `LeaseWatch` counts from zero (a
    declaration is delayed, never hastened).
  - **The mailbox** (`Delivery::Mailbox`, role `coordinator`; role `all` is `Direct`, its channel link) is the only
    way anything reaches a host, and no replica holds a host: every message for one is an `outbox` row `(host, boot,
    at_ms)` inserted by the transition that decided it (`Coordinator::route_out` at `take_transition`: `Target::Host`
    → one row at the boot it is attached as, not attached → dropped; `Target::Every` → one per attached host) ONLY
    inside a transition, after its compare-and-set locked the row — so `seq` is commit order and a reader past `seq >
    after` never misses a lower one committing later. **Never insert an `outbox` or `login_outbox` row outside a
    transition.** An applied `Attach` deletes the host's older rows (`outbox_drop`) before its `CoordMsg::Attached {
    epoch }` row; a detach or a host's end deletes its rows (`MailboxDrop { boot: None }`: every row of the host). A
    host long-polls `GET /v1/hosts/{host}/mailbox?boot=&after=`
    on any replica — a store read in the HTTP handler, no core, woken by the replica's `coordinator_row` NOTIFY,
    re-read every `MAILBOX_RECHECK_MS` (1 s), at most `MAILBOX_WAIT_MS` (20 s), `MAILBOX_PAGE_MAX` (256) rows — and
    acknowledges by cursor: the read deletes its rows `seq <= after` in the same statement (delivery at least once,
    the host drops `seq <=` its cursor: exactly once, in order). A transition that changed nothing durable but carries
    rows (a memo `Reply`, `LandIs`, a relayed login order) is written WITHOUT a core (`TransitionWrite.core = None`:
    the CAS bumps `version` only) — only `core_less_kind` messages and `core_less_decision` store decisions
    (`AssignLand`, `AddHost`, `DrainHost`, `RemoveHost`: no mark, no durable field, each idempotent in the store) may
    (`check_core_less` logs and debug-asserts any other), because a failed core-less write is never remembered as
    ours: a conflict after it rebases and its rows may go twice, its decisions run twice (each kind harmless twice). A write recognised as our own lost commit: its rows, drops and login answers
    are not applied again (`Coordinator::outbox_committed`). A replica's cache also follows the row by NOTIFY
    (`load_coordinator_since`, forward only).
  - **Attachments** (`WorldCuts::attached`: host → boot, `attached_ms` = the store time its `Attach` was applied):
    attached = this boot's `Attach` was applied, on any replica, and nothing detached it since. Only `detach_host`
    removes an attachment: `first_of_generation`, a world rollback (`detach_all`), `ReplicaJoined { alone }` under
    `Direct`, and `Input::HostStalled` — from role `all`'s channel close, a refused `Attach` of that boot at least as
    new as the newest (`admit_attach`), and a **stalled mailbox**. A host's END (a succession, `end_landless_host`,
    `note_succeeded`) drops its mailbox rows but NOT its attachment: it no longer counts (`attached_live` skips a
    succeeded or left host: never gate, heir or assignee), and the next row routed to it (a `Target::Every`) stalls
    and detaches it. The stall: the ticker's `hosts` read carries each
    host's oldest unread row (`mailbox_oldest`); one of the current attachment (not older than `attached_ms` −
    `ATTACH_CLOCK_SLACK_MS`, 1 s: the `Attach` deleted the earlier ones) unread past `MAILBOX_STALL_MS` (15 s, store
    clock) → logged `Input::HostStalled { host, boot, epoch }`, applied only while the host's newest `Attach` is
    exactly that pair: detached, its rows deleted, its next POST answered `attached: false`, and it re-attaches. A
    host that beats its lease but cannot reach the coordinator is noticed only once a row waits for it — there always
    is one: the ticker's upkeep broadcasts `CoordMsg::Calendar` to every attachment at each day turn (`DAY_MS`, 10 s),
    so a host that stops reading is detached within about 10 + 15 + 1–2 ≈ 27 s (meanwhile it may still be gate, heir
    or assignee: joins 503, a run may wait for it up to `SUCCESSION_DEADLINE_MS`). **Orphan rows**: a head row
    unread past `MAILBOX_STALL_MS` whose boot is NOT the one its host is attached as (a rollback's `Generation` rows,
    pushed to every attached host right before `detach_all`, of a host dead then; a `Generation` told a refused
    `Attach` whose process is gone) → the ticker drops that boot's rows with its next transition
    (`Coordinator::drop_orphan_mailbox`, `MailboxDrop { boot: Some }`: one `DELETE … USING UNNEST` by `(host, boot)`,
    never another boot's — never a current attachment's); not logged (a ticker read's effect: a rebase drops it, the
    next read decides it again); a younger head is left (a refused host may yet read its `Generation`). `ReplicaLapsed` detaches nobody; `forget_absent_hosts` forgets only ids below the
    read's `next_host_id` that are not attached. The gate is world-wide: `Coordinator::gate_host()` = the lowest
    attached host that is live, not leaving, not draining (`Gate::Host` / `Gate::Nobody`; "Many hosts", login).
    Replicas still share ONLY the row (and its tables): each keeps its own input log, held login orders and batch
    answers, observation timers and, as the ticker, its reads.
  - **Version check**: with > 1 live replica, outputs leaving with nothing durable changed (`take_out`) wait for a
    `SELECT version` — under `Mailbox` the runtime's, before it answers batches that changed nothing durable
    (`pump_clean_batches`; an EMPTY batch too, unless the cache holds the host attached as its pair — `attached: true`
    leaves at once, `false` only from a confirmed cache, else a healthy host re-attaches and aborts a cut); a moved
    row → refresh and replay first. An evicted core answers no batch.
  - Metrics `mmo_coordinator_replicas_live`, `mmo_coordinator_is_ticker`, `mmo_coordinator_api_requests_total{route,code}`
    (routes `host_messages`, `host_mailbox`, `login_orders`, `login_answers`, `other`), `mmo_coordinator_mailbox_rows` /
    `mmo_coordinator_mailbox_oldest_ms` (the ticker's read; 0 on the others), `mmo_coordinator_stalled_total`, a host's
    `mmo_world_link_up` / `mmo_world_link_reattach_total`; `stalled=` / `mailbox_rows=` / `login_rows=` / `core_less=`
    / `refreshes=` (rebased onto another replica's row: each version bump makes every other replica reload the core)
    on the window line; alerts `WorldCoordinatorReplicasDown`, `WorldCoordinatorConflicts` (measure before splitting
    the row), `WorldCoordinatorNoTicker`, `WorldMailboxStalled` (the oldest unread row > 30 s for 2 min),
    `WorldCoordinatorApiErrors` (5xx share of the hosts' routes > 5 % for 10 min; the login orders' 503s are answers).
    Dev: `just coordinators=2 dev-r3`, `just coordinator claim <n>` (replica n on https://127.0.0.1:6480+n, the same
    cert files).
- **Many hosts (R3 + R4 takeover + Elastic 1 hosts by id + Elastic 3 land assignment + Elastic 4 the fleet; plans
  `docs/superpowers/plans/2026-11-12-r3-multi-host.md`, `2026-11-13-r4-takeover.md`,
  `2026-11-15-elastic-1-hosts-as-deployments.md`, `2026-11-18-elastic-3-land-assignment.md` ("As built"),
  `2026-11-19-elastic-4-reconciler-scaling.md` — built;
  whole-branch reviews and the owner's hand checks pending, see `TODO.md`)**: `claim` is any number of hosts and
  needs role `host`, Postgres and login through the coordinator (`jwt` + a service token), else refused (`roles::check_claim_needs`);
  `static` is ONE host (a `static` registration beside a live row is refused, `StoreError::NotAlone`); a host under
  `allinone` or no `VOXEL_OWNERSHIP` is refused.
  - **Identity**: a host is a `game::HostId`, issued by `register_host` from `world.next_host_id` (migration
    `0014_host_ids`) when the process registers — before it is assigned land or links — and **never reused**; nothing may
    be derived from its number (ports, file names, counts; `HOST_ID_MAX` bounds it). `HostId::SOLO` (0) is role
    `all` / SQLite. A restarted process is a NEW host that owns nothing; its old id is ended by a succession if
    it owned land, else by `end_landless_host` (row dead, no succession, generation unchanged). The coordinator
    forgets an id a `hosts` read no longer shows and nothing attached holds (`forget_absent_hosts`); a host
    forgets dead rows no open succession names (`forget_dead_hosts`) and, from a successful directory read, ids
    ABSENT from it below its `next_host_id` that no run it holds names (`WorldManager::forget_absent_hosts`, a
    pruned row — else a gate waits for that peer's sync forever and refuses every spawn); one the journal refuses
    to forget (unsaved state traffic) is noted dead (`dead_seen`): the next cut stops the host for a rollback
    (`take_unaccounted_dead`), never a world whose every cut part is refused.
    `VOXEL_HOST_NAME` (default empty) labels a process (logs, dev files, `hosts.name`, the Deployment), never a
    host's identity (no store row is keyed by it); under the fleet it is the `desired_hosts` name the pod was made for
    (the template's `__NAME__`) and the coordinator finds a name's `hosts` rows by it (`HostSeen.name`). A
    registration lapses the other dirty live rows of its non-empty name at once (`seen_ms = 0`).
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
    ASSIGNED by the coordinator on demand (Elastic 3; `world_manager/land.rs`, `coordinator/assign.rs`,
    `world-store/src/assign.rs`), a dead host's land given to one heir while the others run on. No host claims
    free land itself any more (`claim_free`, `Volatile::ClaimWanted`'s ask and `Directory::preferred` are gone; a
    boot's re-claim of its OWN rows, `claim_super_regions_live`, stays). A host that needs land it knows no reachable
    owner of (`Free`, `Unknown`, a host in `left_peers`) sends `HostMsg::LandNeeded` (once per `LAND_NEEDED_EVERY_MS`,
    1 s, per super-region, only while attached; a draining host asks too); the ticker reads the owners and, for a
    free or absent row, issues the store decision `AssignLand` to `pick_assignee`'s host — attached, live, takes
    logins (a draining or leaving host is assigned nothing), a live dirty lease row; skipped while another
    qualifies: known < `ASSIGN_MIN_ATTACHED_MS` (30 s, the crash-loop guard) or tick headroom <
    `ASSIGN_HEADROOM_MIN_PERMILLE` (200); then fewest players, fewest regions (each + its provisional count,
    `ASSIGN_PROVISIONAL_MS` 10 s), rendezvous; ≤ `ASSIGN_PER_PASS_MAX` (64) per read. `AssignLand` checks the
    assignee's `hosts` row FIRST (live, dirty, undrained — else `HostNotLive` / `HostDraining`), then reads the land
    row plainly (an `INSERT … ON CONFLICT` would WAIT behind a staged releasing part) and takes only an absent or
    free row (`WHERE owner = -1`, else `Owned`). No mark: a duplicate is refused by the store, a replayed answer
    only re-sends. The answer: `CoordMsg::Assigned` to the assignee, `CoordMsg::LandIs` to other askers (also for an
    owned row; an assignee asking for its own row is told `Assigned` again); the assignee QUEUES the rows, the
    server's keeper grows the fence (`HostClaims::add`) and only then `claimed` (directory, `Reliable::Owns`,
    `flush_unowned`) — **fence before use**. A lost `Assigned` heals by the next ask, or by the directory read: a
    row owned by this host that the fence does not hold at that epoch, newer than the manager's epoch, is fenced
    first (`DirectoryKeeper`; a failed read retries after `DIRECTORY_READ_RETRY_MS`, 1 s, which also spaces the reads
    the manager wants) — **never while a succession is open** (a `hosts` row `succeeding`): a declaration's moved
    rows look the same and are INHERITED, usable only behind the heir's mask after its order and adoption; and every
    row of this host the fence lacks is WITHHELD from the read the manager gets (it never sees an unfenced row as
    `Me`). The coordinator's own heal skips land an open run moved to the asker (or while one is being declared).
    With EVERY replica down
    nothing is assigned: free land stays free, a need waits (never guessed); owned land runs on.
    **Idle land is released** in the host's OWN cut part: every `LAND_IDLE_SCAN_MS` (1 s) owned super-regions nothing
    holds (`busy_land`: running/stopping regions, arrivals, mailboxes, in-flight targets, homes, moves, held spawns
    and unexpired spawn pins, `succession_pinned`, `inheriting`) are stamped idle; at `BeginCut` those idle ≥
    `land_idle_ms` (`LAND_IDLE_MS` 5 min; server dial `VOXEL_LAND_IDLE_MS`, 0 or unparsable refuses the start) are
    MASKED (`releasing`: `owner_of` answers `Unknown`, as for `inheriting` — nothing loads, a need is noted without
    an ask, arrivals take the unowned path) and the part's `WorldCut::releases` frees them with its commit. Committed:
    the server shrinks the fence (`HostClaims::shrink`, a row only at the epoch it was RELEASED at — `releasing`
    keeps it) and then `cut_committed` → `land_released` (directory free at the released epoch + 1, parked blobs
    dropped, `Reliable::Released` to every peer that has not left, mask lifted, waits ask; a row the directory
    already knows at a newer epoch was assigned again before this host settled the cut — kept, nobody told) →
    `drop_stored_parked`: **after every committed world cut the store is the only holder of a record parked on land
    that is not `Me`** (those the part wrote unchanged, and those handed here wrapped in the cut — `parked_written`;
    not one a spawn order waits on), else the land's next owner places the player from the store while this host
    keeps writing its copy; aborted (`cut_failed` → `release_aborted`): the mask
    lifts, the land stays. Nothing is released under `static` / all-in-one or while any succession run is open on the
    host. A total crash's land drains back to free this way. A player record parked on released (or another host's)
    land is placed by its land's owner: the gate's `place_spawn` waits (`Placement::Wait`, the land asked for) or
    hands the record over with `move_to_peer`, its adventure claim kept (`restored_adventure`). Metrics
    `mmo_host_land`, `mmo_land_released_total` (host), `mmo_land_assigned_total` (coordinator); `land=` /
    `land_released=` on the `[server]` line, `assigned=` on the `[coordinator] window=` line.
  - **Lease**: a host stamps `hosts.seen_ms` every `HOST_HEARTBEAT_MS` (2 s) from its MAIN loop — never a
    timer thread (a hung loop must stop beating) — on a connection of its own (never the store's shared
    pool), each beat under a client-side deadline (`HEARTBEAT_DEADLINE_MS`), and once after each long boot
    step (`lease::beat_at_boot`); the coordinator declares it dead after `HOST_LEASE_MS`
    (10 s) of its own unchanged reads (`LeaseWatch`, on the process's MONOTONIC clock — a step of the store's clock
    lapses nobody; the store's `PG_NOW_MS` check at the declaration is the second gate; a database outage
    declares nobody). The Postgres lease
    clock is `clock_timestamp()`, never `now()`. A fenced transaction (a cut part, a region load) idle longer
    than the lease loses its session (`FENCED_TX_IDLE_MS`, set by the fence check): a frozen host's row
    locks do not outlive it. The beat statement also writes the host's load (Elastic 3, migration
    `0018_host_load`: `hosts.load_players` / `load_regions` / `load_tick_permille` = sessions, running regions,
    the worst tick since the last beat in permille of the period, `server/src/load.rs` `TickLoad`; a boot beat
    sends zeros); the ticker's `hosts` read carries it (`HostSeen.load`) to `pick_assignee`. Gauges
    `mmo_host_load_{players,regions,tick_permille}`.
  - **Succession** (`coordinator/succession.rs`, `world_manager/succession.rs`; migrations `0012_host_lease`,
    `0013_successions`): one fenced transaction marks the dead row and moves ALL its land to one heir per
    predecessor. Survivors report their peer journal against the dead host; the heir masks the inherited
    land (`inheriting`, from the order on) until it resolved every report and adopted, holds every login order
    until then, pins the affected regions, and its parts carry `adopts` until the settle. No cut begins until
    every survivor applied and the heir adopted (answers count per BOOT; a survivor that took no part in the order's
    base cut — it registered during it: `cuts_joined_max` is not that cut and no cut up to it is undecided here —
    answers at once from its WHOLE journal, which only a committed cut its host took part in truncates); then one is due at once
    (`SETTLE_CUT_RETRY_MS` after the last cut ended, not a save period), and its commit decision names the open
    successions (`settles`) — the store records any other set as an abort. Conserved: identities (entities,
    players, people) exactly once; items and terrain edits that crossed the dead host's seams within one
    save interval are not guaranteed (`succession_seam_events=` on the `[server]` line); a leave has no item
    window. **Recipients own land** (Elastic 2a): survivors are the dirty live hosts with a `super_regions` or
    `in_flight` row, plus the heirs; heirs are tried landed ∧ takes logins → takes logins → attached live. A
    lapsed landless host that is neither heir nor survivor of an open run is ended by `end_landless_host` (one
    fenced transaction: row dead, no succession, generation untouched — even while a run is open). A host
    refused a cut part for unsaved traffic with a host a directory read showed dead (land assigned after the
    declaration, beside the dead host) stops for a rollback (`unaccounted_dead`, exit 5): a rollback, never a
    loss, never a stuck world.
  - **The world generation is the last resort**: `static` / `allinone`, a total crash (below), a second
    LANDED (or recipient's) failure before the settle — the residual: a host that is assigned land during an open
    run and dies before its settle rolls the world back — / `SUCCESSION_DEADLINE_MS` (30 s) without every
    answer / a recipient's reboot in a declared run, `worldctl rollback <url> --yes` (the first process back then
    raises it once more). The coordinator's `rollback_world` is conditional on the generation it was decided
    from: asked again after its commit (a restart before its transition was written) it moves it once.
  - **Registration** (`register_host`, `world-store/src/rows.rs`; `server/src/lease.rs`): a boot never waits.
    One transaction issues the id, lapses its name's old rows, refuses a `static` host beside a live row, and —
    with nobody else live and a dirty row that is live, holds land or is an open run's predecessor (a total
    crash; an ended row is none) — abandons the open successions, takes every dirty
    row's `super_regions` and `in_flight` rows at `epoch + 1` and raises the generation (the first host back
    holds all owned land, and releases what stays idle); otherwise the new host owns nothing. Rows owning nothing
    are pruned there — never a host with a staged cut part (`two_phase::staged_hosts`, read in a statement of its own
    BEFORE the `DELETE`, Postgres only: a leaver between its leave cut's decision and its part's `COMMIT PREPARED`
    keeps its row, whose in-flight rows are not visible yet). A registration stuck behind a lock exits 1 after `CLAIM_LOCK_TIMEOUT_MS` (60 s).
  - Saves are coordinated world cuts in two phases (each host `prepare_cut`s its part, the coordinator
    decides; Postgres needs `max_prepared_transactions > 0`, the dev compose sets it); parts a crash left
    staged are resolved at start before anything else (`resolve_staged_cuts`; a start finishes only cuts no
    live host has a part in). An exiting host asks for a leave cut and exits 0 either way — not committed → its
    `hosts` row stays dirty (a crash to the others). A host that registers during a cut is not in it and is
    in the next (`cut_begins`, below).
  - **Leave and drain**: a cleanly departed host never returns. **A clean leave releases its land** (Elastic 3):
    the leaver's part (`leave_release`, in `try_complete_cut`) carries every super-region its directory gives it in
    `releases` — unless the part carries an `in_flight` row or a succession run is open on it; then it releases
    nothing (its `in_flight` rows are keyed to its host id, only a succession's adopting part moves them) and a
    **`Leave` succession** gives ALL its land and rows to ONE heir — `heir_of` among `heir_candidates`, as for a
    crash, never "the asker". The leaver is wanted (`land_wanted`) from the ticker's `hosts` read — a clean, live,
    undeclared row that still `owns_land` (a `super_regions` OR an `in_flight` row: a leaver that released its last
    land earlier and left carrying only in-flight rows is found this way, no ask needed) — or from an owners lookup
    (a departed host's land is asked for only at the paced scan, once per `LAND_NEEDED_EVERY_MS`). `leave_wanted`
    declares it between cuts, only once a world cut LATER than the leave cut committed and to an heir that took part
    in the last committed cut (`LeaveGate`, the process's own observations: a host registered during a cut would
    wait for a base cut it never takes part in), and **never for a leaver that owns nothing** (dropped when the
    read shows it landless; the store's declaration refuses an empty `Leave` too, `Gone`). An aborted leave keeps
    the land, its retry decides again. Peers learn a released leave from a directory read (`end_leaves` asks for
    one); where a read first shows the leaver landless (`reroute_held_for_landless_leavers`, before
    `forget_landless_leavers`) their `departed` / `redirected` pointers at it are dropped (its committed part wrote
    every player it held: the store places them) and what they held for it goes on by the land's next owner
    (transfers wait for an owner, intents are refused back, replies go to the mailbox); such a leaver is forgotten
    only when its row is pruned. `worldctl drain <url> <host id> [--in <secs>]`
    (default `DRAIN_WARNING_SECS` 300, at most `DRAIN_MAX_SECS` 7 days; a mark only moves earlier, there is no
    un-drain) sets `hosts.drain_at_ms`, which a `claim` host reads at its heartbeat: it tells its home sessions
    the deadline (`ServerPacket::HostDraining { at_tick }`), stops being the gate, and at the deadline (its
    coordinator attached) leaves through its leave cut and stays up **drained** (`/readyz` 503 `drained`, no
    heartbeat, NO store call — it must survive its row being pruned) until removed (under the fleet: by the
    coordinator's `RemoveHost` and the reconciler); SIGTERM still means leave
    now; a drain leave that does not commit ends the process (exit 0, dirty row). A `static` host warns once and
    ignores a mark; `worldctl drain` refuses a row not beaten within the lease and warns when it marks the last
    host not draining (no gate host: joins 503; no assignee — with fleet dials the joins answer `world_starting`
    and the coordinator adds a host while a slot is free). A stopping host tells its
    sessions why on every exit branch (`LEAVING_REFUSAL`, `FENCED_REFUSAL`, `ROLLBACK_REFUSAL`).
  - `worldctl hosts <url>` lists the rows; `worldctl retire-host <url> <host id>` (`world-store/src/operator.rs`)
    only vouches a row dead while hosts run; with no host running it is refused (the first registration takes
    the land, a total crash). `claim` only — the store does not record the policy, so it cannot refuse a
    `static` world.
  - Editor access is account-scoped and operator-controlled: `worldctl editor-grant <url>
    <account-id>`, `editor-revoke` and `editor-list`. The coordinator reads a grant only when
    admitting a spawn, so a change applies to that account's next join; revoke an active session by
    kicking it and requiring a rejoin. The trusted local-owner path (inline/offline `LocalServer`
    and `AuthMode::SinglePlayer` selected by `VOXEL_SINGLE_PLAYER=1`) sets the capability directly;
    ordinary `AuthMode::Local` dev/LAN names stay deny-by-default, while observer and JWT/login
    admission keep the coordinator grant gate. Migration `0025_editor_grants` must be present.
    Editor mode uses link protocol 15.
  - **The fleet** (Elastic 4; `game/src/coordinator/fleet.rs`, `world-store/src/fleet.rs`, migration
    `0019_desired_hosts`): the table `desired_hosts` — `name` `h<n>` from `world.next_desired_host` (never reused),
    `slot` (the lowest free at its add, reused after a removal; the reconciler's node ports), `state` wanted /
    draining — is written ONLY by the coordinator's store decisions, inside its transition: `AddHost { max,
    expect_wanted }` (adds a row only while the wanted rows number `expect_wanted`, else `NotNeeded`; `Full` when ALL
    rows, draining ones included, reach `max`), `DrainHost { name, host, in_ms }` (the row draining; with a host, that
    boot's live dirty row marked `drain_at_ms = LEAST(…, now + in_ms)` → `Marked`, else `RowOnly`; no row →
    `Unknown`), `RemoveHost { name }` (`Done` / `Gone`) — each idempotent, no mark (a lost answer is repaired by the
    next read). The reconciler and `worldctl desired` only read it (SQLite: always empty). The core keeps NO durable
    fleet state (Elastic 4 left `COORDINATOR_STATE_VERSION` alone; the fleet fields are process fields in `projection!`,
    `fleet_pending` `reset`, `start_asked_ms` `kept`): the desired rows come with the ticker's `hosts` read (never
    logged; `HostSeen.name` ties a `hosts` row to its name). `maintain_fleet` runs only on the ticker, only with
    dials set (`VOXEL_FLEET_MAX` unset = no fleet decision at all: `dev-r3`, role `all`), at most one decision per
    `SCALE_DECISION_EVERY_MS` (10 s), none while one is unanswered, and after an answer none before a `hosts` read
    issued after it. The live host of a name = its live dirty `hosts` row (the newest still beating). The first rule
    that applies: (1) a draining name whose live host has no mark → `DrainHost` of that host; (2) a wanted name whose
    live host has a mark (`world-drain`) → `DrainHost` of the row only; (2b) a wanted name with no live host whose
    NEWEST row is clean WITH a mark (the drain finished before the ticker saw it) → the row only — clean without a
    mark (a SIGTERM leave: `world-stop`, a release) changes nothing; (3) a draining name none of whose rows is
    live-and-dirty → `RemoveHost` — also when its leaver kept land or in-flight rows: its `Leave` succession needs
    only the clean `hosts` row (the drained process takes no part and writes nothing, even at its SIGTERM), and the
    slot must be free for its heir to be added at all; (4) wanted < min → `AddHost`; (5) scale up: wanted < max and, for `up_after_ms`, every read showed
    the LEAST-loaded serving host at `tick_permille ≥ SCALE_UP_LOAD_PERMILLE` (600) → `AddHost`; (6) scale down:
    wanted > max(min, 1), every serving host known ≥ `min_lifetime_ms`, and for `down_after_ms` every read showed
    `Σ tick_permille ≤ (n − 1) · (1000 − SCALE_DOWN_SPARE_PERMILLE)` (700) → `DrainHost` of the victim (fewest
    players, then fewest running regions, then the highest id); (7) to zero (only `min == 0`): one wanted name, its
    serving host known ≥ `min_lifetime_ms` and at 0 players and 0 running regions for `down_after_ms` →
    `DrainHost`. A drain's notice is 0 with no players, else `DRAIN_WARNING_SECS`. Rules 1–3 and 5–7 need `claim`;
    rule 4 runs under `static` too (with dials set a `static` coordinator reads `hosts`, so its gate follows a drain
    mark the static host itself ignores). Rules 5–7 also need the scale gates: nothing draining, no succession run
    open, being declared or rolling back (and the open runs known), every wanted name with a SERVING host (live,
    attached, takes logins). Their timers move only on a NEW `hosts` read (a failed read and every
    fleet answer clear them); a new ticker counts from zero (a decision delayed, never hastened). Dials (the
    coordinator's env, from the module's `var.fleet`): `VOXEL_FLEET_MIN` (default 1), `VOXEL_FLEET_MAX`,
    `VOXEL_SCALE_UP_AFTER_MS` (`SCALE_UP_AFTER_MS`, 60 s), `VOXEL_SCALE_DOWN_AFTER_MS` (5 min, also the quiet time
    before the last host goes), `VOXEL_HOST_MIN_LIFETIME_MS` (10 min); a dial that is unparsable or 0 (but the
    minimum), or min > max, refuses the start, and min 0 needs max ≥ 2 (a conservative bound: with one slot a login
    that arrives while the last host leaves finds no slot free until rule 3 removes its row). With zero hosts no cut begins, the calendar runs on, free land stays
    free. Lock walks: `world-store/src/succession.rs`'s notes. Metrics (coordinator): `mmo_fleet_wanted`,
    `mmo_fleet_draining` (the ticker's last read; 0 on the others), `mmo_fleet_{added,drained,removed,full}_total`;
    `added=` / `drained=` / `removed=` / `scale=` on the `[coordinator] window=` line.
  - **Moving occupied land in place** (Elastic 5, plan `docs/superpowers/plans/2026-11-20-elastic-5-move-occupied-land.md`;
    `coordinator/moves.rs`, `world_manager/moves.rs`; built, NOT accepted — `TODO.md`): ONE move per world, a durable
    run in the core (`MoveRuns`, in `projection!`): the ticker opens it (`CoordMsg::MoveLand` to the giver G,
    `ExpectLand` to the taker T, rows of the opening transition; the giver takes no logins from then on), the warning
    (`VOXEL_MOVE_WARNING_SECS`, default `MOVE_WARNING_SECS` 60) ends at `at_tick`, a cut begins at once and carries it
    (`BeginCut` / `CutRun` / `CutDecide.moves`); committed → the run ends; aborted → retried (`MOVE_CUT_TRIES` 3) or
    abandoned (`MoveOff` to both; the land held out `MOVE_RETRY_AFTER_MS`); a lost party, a declaration, a rollback, a
    newer generation or the deadline (`at_tick` + `MOVE_DEADLINE_MS`) abandons a `Warning` run — a `Cutting` one is only
    doomed (the store may hold the decision): its cut's outcome decides. **The store**: G's part hands the row to T at
    `epoch + 1` (`WorldCut::moves`, locked with the releases, written after them, NO `hosts` lock); `decide_in` records an
    abort unless every taker is a live, dirty, undrained, freshly beaten listed participant not leaving at the move's
    boot, and its giver is listed; `declare_in` / `end_landless_in` answer `StoreError::Busy` while a part of a cut
    decided COMMITTED is still staged (a taker dying before `COMMIT PREPARED`). **G**: warns the home sessions on the land
    (`ServerPacket::LandMoving` with a standby ticket to T); at the `BeginCut` refuses its part (`MoveRefused`) unless it
    owns the land, T takes part, no spawn order is held or pinned there and no player there is between states; else
    masks it, parks its players KEEPING their sessions and stops its regions; at the commit (`land_moved`, before
    `drop_stored_parked`; the fence shrank by the moved rows, `release_epochs_of`) names T at `epoch + 1` (no
    `Released`) and sends each kept record as `Reliable::PlayerMove { place: MovePlace::Pose }`, the session turning
    observer; an abort restores them in place. A peer's hand-off into the land during the mask takes the release path
    (a reconnect). **T**: never asks for the land; after the committed `CutDone` its keeper reads until a read shows the
    row as T's and fences it even while a succession is open (`expected_committed`), then `moved_in` places the
    `PlayerMove`s it held (noted in `cut_transit`) at their pose, promoting the standby session (none: parked, the
    client rejoins; held past `MOVE_HOP_WAIT_MS` 30 s: the ordinary path); idle observers are spared while a move is
    expected. Other participants mask the land until a newer epoch is known. Hosts report load per super-region in the
    beat (`HostLoad::land`, `hosts.land_load`, migration `0022`). **Scale-down** (rule 6 with players): the victim
    EVACUATES (takes no logins); rule 6b moves its most occupied super-region to `pick_taker`'s host one at a time and
    drains it with no notice once its beat shows no player on its land — `MOVE_ABANDON_MAX` (2) abandons,
    `EVACUATE_MAX_MS` (30 min) or no taker → the warned drain. Dev / twin trigger `VOXEL_MOVE_PROBE_MS` (refused beside
    `VOXEL_FLEET_MAX`; `just move-probe`, a live run). Client: the move's countdown beside the drain line, a standby
    connection opened on `LandMoving`. Hotspot relief is not built.
  - Login calls the COORDINATOR's HTTP API (`login/src/host_link.rs` `WorldClients`: per world ONE pinned HTTP/2
    client and ONE standing answers poll; `host_link` = a URL or comma list, `host_link_cert` = the coordinator's pin;
    no per-replica links): a join POSTs `SpawnPlayer` to `/v1/login/orders` (a transport failure or a 503 `not_ready` /
    `fenced` is tried once more on the next URL) and waits for its `SpawnAck`, matched by `(token, epoch)`, within
    `SPAWN_ACK_TIMEOUT_MS` (3 s). The replica asks `Coordinator::login_gate()`: `Gate::Host(h)`, the world's gate host
    (`gate_host()`), gets `CoordMsg::Login(order)` as a mailbox row; the runtime HOLDS the order (never logged; at
    most `LOGIN_ORDERS_HELD_MAX`, 4096, then 503 `not_ready`) until the transition carrying the row commits (then
    202) and issues it again after a rebase or a refresh (the gate chosen again; a host takes a repeated epoch as
    the same order). `Gate::Nobody`: with fleet dials set a spawn is answered 503 `world_starting` (login:
    `SpawnError::Starting`) and the replica applies `Input::StartWanted` (logged and replayed, ANY replica, at most
    once per `WORLD_START_ASK_EVERY_MS`, 5 s, per replica) → `AddHost { expect_wanted: 0 }` (two replicas asking at
    once add one row); otherwise 503 `no_gate` (login: `SpawnError::NoGate`, 503). `HostToLogin::NoGate` / `Starting`
    are never sent (append-only enum). The hosts' answers (`HostMsg::Login`: `SpawnAck`, `KickAck`, `SessionEnded`) are
    logged inputs whose only effect is a `login_outbox` row; every login pod long-polls `GET /v1/login/answers?after=`
    (cursor in memory from 0; nothing is deleted by a read; a cursor past the newest row reads from the start — a
    re-forged world), every kind idempotent there (a restarted pod re-reads up to the TTL); the ticker prunes rows
    older than `LOGIN_ANSWER_TTL_MS` (10 min) at most once per `LOGIN_ANSWER_PRUNE_EVERY_MS` (60 s), in a transition
    written anyway. Kicks: the sweeper POSTs every pending kick each `KICK_RESEND_MS` (2 s) until its `KickAck` row.
    The client repeats a join answered `world_starting` — a first join from the world list and a rejoin
    alike — every `REJOIN_RETRY_MS` for up to `WORLD_START_WAIT_MS` (180 s, provisional until the cold start is
    measured), status "The world is starting…". A gate that dies holding an order costs a 503 and the client's retry;
    a placement on another host goes gate → holder over the peer link. The gate resolves the player — a store read its main loop never waits on
    (`login_link::OrderResolver`, at most `LOGIN_ORDERS_IN_FLIGHT_MAX` waiting, one more is dropped) — and places them on
    the host that holds them (`world_manager/placement.rs`, relayed `SpawnOrder`s; a host drops a stale `departed`
    pointer when the very order it sent on by it comes back from ANY host — same origin, token and epoch, more hops,
    its own hop of the player unchanged, within `SPAWN_SENT_ON_MS` (`departed_denied`) — so a chain of stale pointers
    ends; `SPAWN_RELAY_MAX_HOPS` is 32); a world request carries its
    player's hop (`HostMsg::Request { hop }`, an older placement's is refused). Regions of another host reach a
    client through an observer session: `RegionElsewhere { endpoint, ticket }` → a second connection with
    `Credential::Observer` — no body, input only as its own `origin_client`.
  - Saved terrain of other hosts arrives by LISTEN/NOTIFY per cut with a polling backstop
    (`VOXEL_TERRAIN_NOTIFY=listen|poll`; `VOXEL_WORLD_DB_DIRECT` = a direct, unpooled URL for the listener);
    a changed version drops that region's cached LOD (`stored_terrain_changed` → `invalidate_region_lod`).
  - Env: `VOXEL_PEER_BIND` (127.0.0.1:6500), `VOXEL_PEER_CERT_HASH_FILE`,
    `VOXEL_ADVERTISE_QUIC`/`_WT` (what clients are told), `VOXEL_WT_PORT`, `VOXEL_PEER_LINK_DELAY_MS`
    (measurement only); `worldctl super-region <url> <n>` (only before the first owned row) + hosts run with
    `VOXEL_SUPER_REGION_REGIONS=n`.
  - `just dev-r3 <hosts> [delay]`: login + coordinator + every host under `scripts/dev_r3.sh` (restarts a host as
    a NEW host on exit 5, 137, and 3 under `claim`, a coordinator replica as a new replica on 137, 3, 4; the stack
    goes down when login, a host loop or the LAST replica's loop ends; knobs in its header; state `target/dev-r3/`); `just host`,
    `coordinator`, `world-hosts-dev`, `drain-dev` (arguments: `just --list`; `host` and `coordinator` take the
    same ownership word). `just go-nogo-r3` re-forges each run and seams from the client's own region→host map
    (Elastic 2a Task 1); its first valid run is a hand check (TODO.md).
- **Owner epochs & fencing**: a host holds super-regions (`super_region_of`, `game::ownership`) in Postgres
  with an epoch — assigned (`AssignLand`), moved by a succession or taken by a total-crash registration at
  `epoch + 1`, freed by its own committed cut part (`owner = -1`, `epoch + 1`); every cut part / region load is fenced
  on it (`StoreError::Fenced`; the coordinator's writes and
  store decisions are fenced by its row's `version` compare-and-set since Elastic 2b) — a fenced
  process stops writing and exits `EXIT_FENCED` (3, also when fenced at `register_host`); a dead coordinator
  thread exits `EXIT_COORDINATOR_FAILED` (4); a world rollback exits `EXIT_WORLD_ROLLBACK` (5). No advisory
  lock any more (SQLite keeps its flock). `OpenMode::ReadOnly` handles (export, tools) write nothing and need
  the newest schema. Never add a store write that skips the fence. A world cut's commit decision must list
  every host with a non-clean `hosts` row (`two_phase::decide_cut` records an abort otherwise) — EXCEPT one
  registered after the cut began: the coordinator records each cut's begin, in the transition that starts the cut,
  with the `next_host_id` of that moment (`record_cut_begin`, `cut_begins`, migration `0015_cut_begins`), and only ids at or above it may
  be missing; never relax that check without the host side (Peer link rules). Each decision prunes the older
  `cut_begins`; `cut_decisions` is never pruned (a re-attaching host's `pending_cut` looks one up). A succession
  moves rows at `epoch + 1`: the replaced process is fenced at its next heartbeat (`lease.rs`
  `pump_or_fence`) or write. **A part's release** (Elastic 3, `rows.rs` `cut_rows`): the released rows are locked
  `FOR NO KEY UPDATE` and checked against the fence BEFORE the ordinary `FOR SHARE` fence check (no lock upgrade
  for a waiting declaration to sit in), freed by `UPDATE … WHERE owner = $me AND epoch = $e` after every other
  write (`rows_affected != 1` → `Fenced`), then `PREPARE`; a release outside the fence, or in a one-phase cut
  (`CutEnd::Commit`: static, all-in-one, SQLite), is `Unsupported` before any statement. A staged releasing part
  holds those rows until its decision: a region load there WAITS (so the mask is up before the part reaches the
  store and lifts only after the shrink), a declaration of the releaser waits and then moves only what is left.
  **Fence before use; shrink before the next part**: `HostClaims::add` runs before the manager learns assigned
  land, `HostClaims::shrink` (the part's releases at its fence's epochs ∪ `release_epochs_of(cut)`; a row leaves
  only at its released epoch) before `cut_committed`; a directory read older than a release adds nothing back (the
  keeper's epoch guard); a `CutDone` saying COMMITTED for a cut with masked releases and no part tracked here is
  settled by the store's decision, never as a failure. A new `hosts` / `super_regions` /
  `successions` statement needs its lock walk
  (lock order and the safe deadlock cycles: the module notes of `world-store/src/succession.rs`; the
  operator's in `operator.rs`; unfenced `successions` writers are the operator's rollback and a
  total-crash registration). The settles↔adopts pairing (a settled
  succession's heir part carried `adopts`) is a coordinator-side invariant the store does not enforce — it
  only detects a breach after the commit (`two_phase::report_settled_unadopted`, `cuts_settled_unadopted`).
- **Link rules**: a re-`Attach` after any reconnect reconciles the coordinator's (durable) view; requests are memoised by
  `RequestId` (idempotent resend); the Attach waits for exact people lists ("wait, never guess" — conservation
  beats latency). **The coordinator's API** (`server/src/coordinator_api.rs`; HTTPS, HTTP/2, rustls ring, the
  coordinator identity; its own thread and current-thread runtime, calls to the core over crossbeam): bodies
  `game::encode_packet`; every request carries `Authorization: Bearer <VOXEL_SERVICE_TOKEN>` (401), `X-World` =
  `VOXEL_WORLD_ID` (412 `world`; `""` matches only `""`) and `X-Link-Version` = `LINK_PROTOCOL_VERSION` (412
  `version`); a replica not Ready (its `ReplicaJoined` not committed) answers everything 503 `not_ready`, a fenced
  one 503 `fenced` (`ApiListener::set_refusal`), a store failure 503 `store`. **The host's link** (`HttpWorldLink`,
  `server/src/world_http.rs`; one thread, two loops): the send loop drains what the manager sends into ONE `HostBatch
  { boot, epoch, msgs }` (≤ `LINK_BATCH_MAX`, 256) and keeps ONE `POST /v1/hosts/{host}/messages` in flight
  (`LINK_POST_TIMEOUT_MS`, 5 s); every batch is stamped with the `(boot, epoch)` of the last `Attach` it carried (an
  `Attach` always opens its batch); idle for `LINK_IDLE_POST_MS` (2 s) it posts an empty batch (how a detached host
  learns it; the coordinator applies and writes nothing for it). Any replica applies a batch as its messages' inputs
  and answers `BatchAck { attached }` once the transition covering them committed (nothing durable changed: once
  the version check passed); `attached` = the core holds that boot attached with exactly that newest `Attach`.
  **Down**: a failed POST, a non-200, or `attached: false` for an `Attach` batch or while steady — what is queued is
  dropped (the link is not a queue), the mailbox gate closes, probes are empty batches under `LINK_RETRY_MIN_MS` …
  `LINK_RETRY_MAX_MS` (250 ms … 5 s, doubling; back at the minimum only once an `Attach` is answered attached, so a
  refused `Attach` is not retried at round-trip speed); a probe answered 200 → **up**: a new `generation()`, and the
  manager re-attaches before anything else is carried. **The mailbox gate**: the mailbox loop hands rows to the
  manager only from the row `CoordMsg::Attached { epoch }` of the current `Attach` on (earlier rows are acknowledged,
  never delivered: no row decided for an older attach reaches the manager), except `CoordMsg::Generation` (the
  answer to an `Attach` refused for a stale world generation has no `Attached` row). A down, and each `Attach` sent,
  cancel the mailbox GET in flight (and its backoff wait); it polls again at once, on a new client after a transport
  failure. If the link's thread ends (a panic, no runtime) `pump` logs an error once and the link reads down for good
  (`is_up()` false; nothing respawns it). Several URLs: a failed request moves to the next, under ONE pin (every
  replica shares one identity, `svc_link::pinned_http2_client`: `HTTP_CONNECT_TIMEOUT_MS` 5 s, an HTTP/2 PING every
  `HTTP_KEEP_ALIVE_INTERVAL_MS` 5 s, idle too, closed after `HTTP_KEEP_ALIVE_TIMEOUT_MS` 5 s unanswered — a silently
  dead connection fails within 10 s, under the 15 s stall). The login service replaces a world's client after a
  transport failure (not after an HTTP status). Behind one Service a host's (or a login pod's) requests ride ONE
  HTTP/2 connection to ONE replica until it breaks (kube-proxy balances connections, not requests). Role `all`
  (`jwt` + a service token) serves the two login routes itself on `VOXEL_HOST_LINK_BIND` (an in-memory answer queue,
  `LOGIN_ANSWERS_HELD_MAX` 4096): the login service has one transport; kicks are durable in its store until acked
  (login migration 0003). `svc-link` carries host↔host peer links only. `LINK_PROTOCOL_VERSION` is 15
  (the coordinator-authorized editor capability is carried through host spawn placement; version 14's
  `CoordMsg::LoginTraced` carries only a bounded sampled W3C carrier and opaque operation id through the durable
  mailbox; the old `CoordMsg::Login` remains decodable; 13 was Elastic 5:
  `CoordMsg::BeginCut.moves`, `MoveLand`, `ExpectLand`, `MoveOff`, `CutFailure::MoveRefused`,
  `Reliable::PlayerMove.place`, `ServerPacket::LandMoving`; 12 was the coordinator as an HTTP service: the
  `X-Link-Version` header and the peer hello; 11 was Elastic 4:
  `HostToLogin::Starting`; 10 Elastic 3: `HostMsg::LandNeeded`, `CoordMsg::Assigned`, `CoordMsg::LandIs`,
  `Reliable::Released`; the unsent `HostMsg::LandWanted` / `Volatile::ClaimWanted`, `HostToLogin::NoGate` /
  `Starting` and `PeerRole::Coordinator` / `Login` stay in their append-only enums) — older hosts, login services and
  clients do not mix with this build (stop, upgrade, rejoin).
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
  host that no committed cut of both saved (`unsaved_traffic`) — what lets `decide_cut` omit a newcomer. EVERY
  coordinator replica down means no cuts, no takeover, no logins and no assignment until one is back (it registers
  `alone`) — but nothing they decided is lost: it resumes from the row; the hosts run on, their links down, and
  re-attach when one is back. One live replica suffices; a host whose POST failed with its replica re-attaches
  through the next URL or the Service to any other. **`Reliable::Released { rows }`** (Elastic 3) tells every peer land is free:
  sent by `land_released`, and to a peer at its hello (after `Owns`) with all of `recently_released`; NOT
  state-bearing (it names no entity, player or item; the store's row is the truth, a directory read repairs a lost
  one, `Directory::set` ignores an older epoch), never `InCut`, never journalled. What arrives for released land
  waits for its next owner (the unowned path) and goes on once: `flush_unowned` also forwards `intent_mailbox` mail
  whose land another reachable host now owns (`forward_mailbox`); `forget_host` and `end_leaves` ask for a directory
  read while the directory still names that host; a host sent an intent or reply for land it neither owns, masks nor
  just released asks for one too (one of the two directories is stale).
- **Peer journal** (`game/src/peer/journal.rs`): every host under `claim` (a `static` host records nothing)
  journals the state messages it exchanged with each peer since the last committed cut — a succession's
  reports; an entity's events are ordered by its transfer stamp, a player's by the hand-off's hop, never by a
  host's own tick. Truncation rests on every
  world-cut participant calling `PeerJournal::cut_begun`: any new path that joins a cut must call it. A new
  state-bearing `Reliable` must be journalled or argued out. Every `WorldManager` field is classified in
  `host_fields!` (`world_manager/succession.rs`; per-host kind or `other`): `forget_host` and `mentions`
  destructure the manager with no `..`, so a new field does not compile until it is listed.
- **World on k8s variables (R5; each unset = today's behaviour)**: `VOXEL_COORDINATOR_ADDR` and login's `--link` are
  a URL or a comma list of URLs (`https://coordinator.<ns>.svc.cluster.local:6480` behind the ClusterIP Service, a
  name resolved at each connect; a bare `host:port` is refused); `VOXEL_COORDINATOR_BIND` is the HTTPS API's TCP
  bind (`0.0.0.0:6480` in a pod); `VOXEL_LOGIN_LINK_BIND` and `VOXEL_LOGIN_LINK_CERT_HASH_FILE` are removed;
  peers stay literal `ip:port`: `VOXEL_ADVERTISE_PEER` (a literal `ip:port`; required when the peer bind is a wildcard,
  a pod passes `$(POD_IP):6500`). A link identity (`VOXEL_*_CERT_HASH_FILE`'s `<file>.cert` / `.key`) may sit on a
  read-only mount (the pin file is then not written, a WARN), and a dialer's pin is derived from `<file>.cert` when
  there is no pin file (`svc_link::read_cert_hash` / `pin_beside`). `VOXEL_EMPTY_STORE=forge|wait`: `wait` = a pod
  never forges, it waits for `worldctl forge` (`mmo_waiting{why="forge"}`) and exits 0 on SIGTERM. `VOXEL_HEALTH_BIND`
  (`ip:port`, unset = no listener; `server/src/health.rs`): `/livez` fails only when a REGISTERED (or drained) process's main loop
  has not passed for `LIVEZ_STALL_MS` (30 s = 3 × the lease — the lease always acts first; it never replaces or feeds
  the lease heartbeat), `/readyz` is 200 once registered (503 body: `forge` / `drained` / `starting`) — a host
  also needs its attach acknowledged by a coordinator replica (503 `attaching`; Ready for `ATTACH_GRACE_MS`, one
  lease, after a detach), a coordinator replica its `ReplicaJoined` committed (its API answers 503 `not_ready` until
  then and `fenced` once fenced), the reconciler a pass without a failed
  read or action within `FLEET_READY_MS` (6 s) and the coordinator above 0 (503 `forge` / `stopped` / `failing`) —
  `/metrics` is Prometheus text with a `role` label (`host`, `coordinator` or `fleet`; each role's series are constant
  on the others — alerts select the role; the reconciler's: `mmo_fleet_desired`, `mmo_fleet_deployments`,
  `mmo_fleet_pass_age_ms`, `mmo_fleet_{created,deleted,errors}_total`; alerts `WorldFleetDown`, `WorldFleetBehind`,
  `WorldFleetSaturated` (the least-loaded registered host ≥ 600 ‰ for 15 min: scale-up could not act — at `fleet.max`
  the coordinator does not ask), `WorldFleetNoFreeSlot` (`Full` answers for 20 min: a host wanted for the minimum or a
  starting world, every slot held, draining hosts included)). `VOXEL_CLIENT_CERT` / `VOXEL_CLIENT_KEY` (PEM chain + key; the old
  `VOXEL_WT_CERT` / `_KEY` are aliases): ONE certificate for the QUIC listener and WebTransport, files WAITED for at
  start (a WARN at most every 60 s) and re-read every 60 s (a cert-manager renewal needs no restart); unset = a
  self-signed P-256 certificate valid 14 days whose hash `HostEndpoint.cert_hash` carries (single-player too).
  `VOXEL_LAND_IDLE_MS` (Elastic 3; ms, unset = `LAND_IDLE_MS` 5 min, 0 or unparsable refuses the start): how long
  owned land nothing holds stays the host's before a cut releases it — the world module's `land_idle_ms` (default
  null = unset; the twin sets 20000, orion does not). The fleet (Elastic 4): the coordinator's `VOXEL_FLEET_MIN` /
  `_MAX` and pace dials ("The fleet" above); the reconciler's `VOXEL_FLEET_NAMESPACE`, `VOXEL_FLEET_PORT_BASE` and
  `VOXEL_WORLD_ID` (its label value) — all three required — and `VOXEL_FLEET_KUBE_API` (a `kubectl proxy` URL; unset =
  the in-cluster API server).

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
  `REJOIN_CONNECT_MS` (15 s), then a new join; gives up after `REJOIN_MAX_MS` — or, once a join was answered
  `world_starting`, no earlier than `WORLD_START_WAIT_MS` after the first such answer; revoked → the sign-in screen). Native QUIC keep-alive
  3 s / idle 10 s (`netcode.rs`, as WebTransport).
- **Native QUIC trust** (`quic_verify.rs`, `policy_for`): a host that advertises a `cert_hash` is pinned to it; with no
  hash a non-loopback address must present a certificate the public web PKI vouches for; verification is skipped
  only for loopback. The Slint frontend is shared by native/web: Main Menu → Single player / Multiplayer /
  Settings / Exit (the browser disables single player and omits Exit), with Single player → New / Load. Multiplayer
  browses the public `GET /v1/worlds` by deployment region (`eu-central-1` today), including before sign-in, and
  joins only a registry world through the coordinator ticket flow. The player UI has no join-by-address field.
  Account Settings and the existing graphics Settings window are both retained; login from Join or Account Settings
  returns to the screen that opened it. `just web` alone prints a localhost debug URL with `?dev_server=` that starts
  straight at the local server; a release build or non-loopback page ignores it. The login is
  `VOXEL_LOGIN_URL` / a localhost `?login=`, else `{origin}/login` on a non-loopback `https` page, else the one
  official login service. Each registry row carries `region` (migration 0006; `0005` is `map_url`), and
  `login world add` / `world-register` record it.
- **Session lifecycle** (`session.rs`): any new per-world resource, system `Local` or entity root must be
  registered (`add_session_reset`, `SessionLocal`) or tagged `SessionScoped`, or it leaks into the next world.
- **Editor mode**: the sim owns `editor` and `editor_allowed`; the client mirrors
  `UiUpdate::Editor` in the session-reset `EditorState`. `/creative` / `/gamemode creative` and
  `/give` send `Order::SetEditor` / `UiAction::Editor*`; active editor forces body-driven first
  person and F5 cannot leave it. The inventory palette is hidden for chests, reads `BlockDef.tab`,
  and shares the chat's two-owner `TextEntry` gate. Palette clicks never forge items: every
  take/give/trash is authoritative. Ordinary chat remains `ChatLog`-local, with no packet or journal.
- **The world map** (`renderer/map/`, plan `docs/superpowers/plans/2026-11-20-block-level-world-map.md`): one tile
  pyramid (`game::map_tile`: 256² samples, level k = 2^k m per sample) behind the minimap, the full map (M) and the
  site choice. A [`MapView`] (centre, m per PHYSICAL px, rect) per surface; `map/pass.rs` draws them in `Core3d`
  right before `SlintCompositeSystems` (`assets/shaders/map_tile.wgsl`, compiled in), so the Slint layers draw over
  it: the map's rect in a layer is a hole — `#00000001`, not transparent, where the layer must keep the pointer
  (`opaque_at` needs alpha > 0), and never inside a `FrameFace` (its base is opaque). Tiles come from
  `MapSource::Local` (native single-player: `map-tiles` over the world's `terrain.sqlite` opened read-only) or
  `MapSource::Http` (`{base}/v1/map/tiles/…`: the join ticket's `map_url`, the browser's `{origin}/map/<world>`,
  `VOXEL_MAP_URL` for `just client`; native disk cache `map/disk.rs`). `MapTiles` belongs to the WORLD: a session
  end pauses it, another world clears it. Loaded chunks are painted into levels 0–3 at once (`map/paint.rs`; a
  painted tile is kept until a served one with a newer etag). The server side is `server/src/map_api.rs`:
  unauthenticated, on the coordinator's HTTPS API (merged after the token guard) and on a plain `VOXEL_MAP_BIND`
  (the pod's 6470 for the web image's rate-limited `/map/<world>/` proxy; role `all` loopback only).
- UI is **Slint only** (no `bevy_ui`, no egui). `CursorPolicy` is the only cursor-grab writer. `dev_ui`
  (F3 debug pane) is dev tooling only, never player-facing.
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

**Worldgen** (`crates/worldgen`) — `generate_region(RegionCoords, …)` (`generate_columns` for a window,
`generate_region_forged` for an explicit forged world) is a pure function of coordinates (+ the installed
forged world) yielding the region's chunk columns; that is what makes "park, or regenerate" safe. The fused
`pipeline` `column()` must mirror the `#[cfg(test)]` `DensityFn` graph bit-for-bit.
The forge makes ONE kind of world, the **Planet** (since 2026-11-20; plan
`docs/superpowers/plans/2026-11-14-planet-frame-1a.md`): 2:1, x wraps east–west (`EdgeKind::Wrap`), polar seas
north and south (`forge::world::PLANET_EDGES`), 90° N at row 0 to 90° S. Sizes (`forge::play`): Tiny 25,600 ×
12,800 m, Small 50,176 × 25,088, Medium (default) 100,352 × 50,176, Large 200,704 × 100,352, and Dev 16,384 ×
8,192 (not in the menu; `ForgeParams::default()` at 32 m) — every width a whole number of 512 m, so regions tile the
circle at every region size. Any other world — an older artifact version (< 10), another layout's edges, a width off
512 m — is refused with "re-forge this world" (file, tiles and store alike: `forged.rs` `validate_wrap`).
**Monthly climate** (Phase 1b, plan `docs/superpowers/plans/2026-11-20-monthly-climate-container-1b.md`): every world
stores `game::MonthlyClimate` — twelve months × six integer planes (sea-level temperature, diurnal range,
precipitation, wet-day share, wind east / south; 96 B per CLIMATE cell) on a coarse grid of about 256 m, never
fewer than 96 rows pole to pole (`forge::climate::climate_stride`; Dev 256 × 128 of 64 m, Large 784 × 392 of 256 m,
28 MiB). It is always RESIDENT: whole in `WorldArtifact::climate` (artifact v10) and in `ForgedMeta::climate` (tiled
format v3), never in tiles, one copy per process (the tile cache holds it, outside its budget); read through
`ForgedWorld::monthly_climate`. Filled by `forge::climate::monthly_placeholder` until Phase 3. At a world position
(exact arithmetic, no date — month interpolation is the caller's): `ForgedWorld::monthly_normals(x, z)` = the grid's
SEA-LEVEL normals at engine metres (reads no tile); `monthly_at_ground(x, z)` = at the GROUND (lapse ×
the bilinear height, each cell floored at 0; precipitation = the fine annual raster × the grid's monthly shares, so
the annual total is conserved; may load tiles), the same at MAP metres `monthly_at_ground_map` — a forge-side
reader with no `ForgedWorld` gets it bit for bit from `forged::ground_normals` at a fine cell centre.
**Canonical coordinates**: everything stored, simulated, hashed or sent is canonical (x folded into the map's width,
`canonical_region` in `worldgen/src/lib.rs`, one nearest-image helper into a region's frame); LOD keys and the client's render frame
are the only unwrapped coordinates. **The wrap is for one process** (`VOXEL_ROLE=all`): under any other role the seam
is CLOSED (`roles::seam_for_role` installs the map with `wrap_x = false`), until the host-to-host seam is built.
Forge tunables all live in `forge/knobs.rs` (`just forge-ui` to tune; check at 16 m cells before keeping a
value; the `forge_quality` gate must pass). `just forge-report --size dev --cell 64 --seeds 8` prints each seed's
numbers (`height_crc`, land share, land components, biome shares, the seam checks) and min / median / max. Water is
recipe data (`ColumnGround::water_top`), never blocks. Look at the world with `just world-map` after tuning changes.
Generated trees share the one deterministic shape/raster contract in `game::tree_shape`. Each newly
generated standing tree stamps 2–6 explicit buried log roots and a compact region-owned `TreeLife`;
roots are never inferred from height or `Block.data`. A visible trunk cut schedules six rollbackable
growth stages every 4 game days (slower with fewer surviving roots), while mining a root is an ordinary
one-block break. `PortableRegion` and world cuts carry the full `RegionTrees` in the nullable migration
0023 blob; `NULL` is an old rootless/legacy region and is never resynthesised. Growth uses normal
terrain/collider deltas and refuses to overwrite terrain, water, bodies, piles, chests or player blocks.

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
