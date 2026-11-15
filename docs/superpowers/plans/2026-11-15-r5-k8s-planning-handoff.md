# Handoff: R5 — the world on k8s (the single k3s server)

**Date:** 2026-11-15
**For:** a fresh session.
**Job:** write the R5 implementation plan (`superpowers:writing-plans`), have Dawid review it, then
execute it subagent-driven (`superpowers:subagent-driven-development`). R5 has **no plan yet** — only the
scope lines below and the settled design in the research spec.

**Outputs:**
1. `docs/superpowers/plans/2026-11-xx-r5-k8s.md` (use the date you write it on).
2. After Dawid's review: the implementation, committed task by task on `develop`.

## 1. Where things stand (verified 2026-11-15 at `77f8e8a3`)

- **R4 (takeover) is built** — plan `docs/superpowers/plans/2026-11-13-r4-takeover.md`; read its status
  line and "Amendments made during execution" (what was really built and what is left open). Commits
  `b4f58b31` … `85187467`, plan `b673d8d8`, plus one fix after the first live run: `bdeb8c78` (a client
  whose body is handed into just-claimed land is told the host).
- **Live evidence so far: one `just go-nogo-r3` at 1 ms, verdict GO** (2026-11-15, 50 thread-seam and 24
  host-seam crossings, state hashes clean, nothing lost). **Not yet run by Dawid** (`TODO.md`, R4 block,
  "To run by hand"): the new client tests (written, type-checked, never run), the `kill -9` and
  `kill -STOP` checks under `just dev-r3 2`, the other delays (`go-nogo-r3 0|2|5`). R5 puts hosts in
  pods that k8s kills and restarts — **the kill checks are the behaviour R5 depends on; ask Dawid
  whether they passed before planning around them.**
- **Open observation, not explained:** in that GO run the number of corrections per crossing was far
  higher than on 2026-11-13 on BOTH seams (p95 18 thread / 20 host; then a mean near 1), correction size
  still 0.00. Not a host-seam problem by the numbers; cause not traced (claim policy, voxel terrain
  return, worldgen and client changes all landed in between). Does not block R5; do not paper over it.
- **Parked from R4's reviews** (in the R4 plan's "Not done / left open" and in the ledger): a host frozen
  inside a large Postgres read keeps its row locks until TCP gives up; a claim whose answer was lost
  leaves directory and fence disagreeing; a login can bounce with 4+ hosts and three hand-offs in one
  journal window; the settle retry has no back-off. The first matters more once a pod can be frozen by
  the kubelet (CPU throttling, node pressure) — weigh it in the plan.
- **orion (the k3s box) already runs the shared services**, all as OpenTofu roots under
  `deploy/envs/orion/` (CLAUDE.md "Deployment" is the map): `platform` (installs the CloudNativePG
  operator chart, cert-manager, Traefik config), `garage` (S3), `monitoring` (kube-prometheus-stack,
  Loki, Tempo, Pyroscope, Thanos, Alloy), `keycloak` + `keycloak-realm`, `registry` (ECR repo
  `mmo/login` + a pull-only IAM user, pull secret renewed by the `ecr-refresh` CronJob), `login` (two
  pods, its own PostgreSQL as a plain StatefulSet — NOT CNPG — at `deploy/modules/login/postgres.tf`),
  `site`, `picoshare`, `zulip`. **No game server runs on orion yet.**
- **`deploy/modules/world`** is the pre-R2 shape (a database manifest + one game manifest, from the
  Agones era); `deploy/envs/local/world` uses it. The spec says R5 replaces it.
- **Only one image exists:** `deploy/images/login`. There is no server image (`server`, `worldctl`) yet.
- **The deployed web client does not use the login service** (Task 4 of
  `docs/superpowers/plans/2026-11-14-login-service-on-orion.md`, deferred). Public build: `just deploy`.
- **After R5 comes "elastic region hosts"** (`docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md`,
  handoff `docs/superpowers/plans/2026-11-14-elastic-region-hosts-planning-handoff.md`): it REPLACES
  R5's StatefulSet (Dawid, 2026-11-14). Read its §1–2 before sizing R5: do not gold-plate what E1
  throws away (per-pod Services, ordinal-shaped config), and do not build what it will build
  (autoscaling, moving land between live hosts).
- **Worlds must be re-forged:** `77f8e8a3` changed the forged-tile format (tiles v2, artifact v8). The
  dev Postgres world behind `just dev-r3` is refused until re-forged (`just pg-reset` deletes ALL dev
  Postgres data — Dawid's call). `go-nogo-r3`'s own database was re-forged on 2026-11-15; its recipe does
  not detect a stale tile format by itself (it dropped the stack with exit 3).
- **Working tree:** Dawid has uncommitted work of his own in `CLAUDE.md`, `justfile`, `deploy/**`
  (monitoring, platform), many `crates/client/**` files, `crates/game/src/{edit,lod,region}.rs`, `.slint`
  files and docs — and he commits concurrently. Never `git add -A`; never `git stash`; no worktrees.
  For a file that also holds his hunks, stage yours with a filtered patch using three lines of context
  and `--recount` — never `--unidiff-zero` (it mis-placed two hunks during R4) — and check the index
  before committing.

## 2. Read first

1. `CLAUDE.md` — the rules and the map. "Deployment" (orion roots, how a login release is pushed and
   rolled back, the Grafana token rules), "Server / store" (role split, many hosts, owner epochs &
   fencing, link rules), "Testing".
2. `TODO.md` — the R3 and R4 blocks: "Known limits", "To run by hand", and every line tagged R5
   (grep `R5`).
3. `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0 — the settled design.
   For R5: §0.7 (b / b′ the StatefulSet and the Agones review, c the network shape, e scaling — its
   node half is dropped), §0.9 (accounts global / players per world, the panel, `NOTIFY`), **§0.10 (the
   final decisions and the R5 line: one Hetzner server, fixed replica count per world, CNPG with one
   logical database per world, per-pod NodePort Services, k3d as the local twin, the panel)**, §0.11
   (rate limiting — deferred, not R5's core). §10 / §10a are the old k8s shape and the Agones test;
   mostly superseded.
4. `docs/superpowers/plans/2026-11-13-r4-takeover.md` — "Global Constraints", the amendments section,
   "Extension points left for later plans" (the R5 list).
5. `docs/superpowers/plans/2026-11-06-k3s-observability-stack-handoff.md` and
   `docs/superpowers/plans/2026-11-14-login-service-on-orion.md` — how a service was put on orion end
   to end (image, registry, pull secret, release by digest, metrics scrape, the operator recipes). R5
   should look like these, not invent a second way.
6. `deploy/modules/login/` and `deploy/envs/orion/login/` — the working pattern to copy.
7. R4's execution ledger, if still present: `.superpowers/sdd/2026-11-13-r4-takeover/progress.md`
   (git-ignored): every ruling, the three final reviews, the fix wave and its re-review.

## 3. R5 scope (from the spec, `TODO.md` and R4's extension points — verify each in code)

**The goal in one line:** one world — coordinator + N region hosts + its database — runs on orion's
k3s, players reach it through the live login service, and killing a host pod is survived by a
succession, not noticed as an outage.

1. **Images and releases.** A server image (`server` in roles `coordinator` and `host`, `worldctl`)
   beside `mmo/login`; the registry root gains the repository; the release path follows login's
   (`latest` + digest pinned with `kubectl set image`, rollback by `rollout undo`) unless the plan
   argues for something else. Server and client must match in `VOXEL_REGION_BLOCKS` and
   `VOXEL_CHUNK_BLOCKS` (build time, no handshake) — the image build pins them, and the web/native
   client release must be built with the same values.
2. **The world's database.** CNPG is installed (`platform`) but no `Cluster` exists. The spec wants one
   logical database per world. Needs: `max_prepared_transactions > 0` (two-phase cuts refuse to work
   without it), a **direct, unpooled** URL for LISTEN/NOTIFY (`VOXEL_WORLD_DB_DIRECT`) if a pooler sits
   in front, and the fence check's `SET LOCAL idle_in_transaction_session_timeout` must survive
   whatever pooling is chosen. Backups were deferred in the spec; the box holds one copy of everything
   (garage has no off-box backup either) — the plan must say what happens to a world if the volume dies.
3. **The coordinator** — one replica per world (Deployment or a one-pod StatefulSet), `VOXEL_ROLE=coordinator`,
   **with the same `VOXEL_OWNERSHIP` as its hosts** (a mismatch is refused at `Attach` since the R4 fix
   wave). While it is down there are no cuts and no takeover: its restart time is the world's save and
   failover gap.
4. **The hosts** — a StatefulSet + headless Service, fixed replica count, `VOXEL_HOST_ORDINAL` from the
   pod ordinal, `VOXEL_HOSTS` = replicas, `VOXEL_OWNERSHIP=claim`. What must be designed, not assumed:
   - **Addresses.** `hosts.peer_addr` is the BOUND address today; off one machine a host must advertise
     a reachable one (a `VOXEL_ADVERTISE_PEER`, the pod's DNS name). Clients are told
     `VOXEL_ADVERTISE_QUIC` / `_WT`: per-pod NodePort Services (UDP), each host advertising its own
     public port.
   - **Certificates.** Service links pin self-signed certs through files (`*_CERT_HASH_FILE`) that the
     dev scripts write side by side; pods need another way to exchange pins (the `hosts` table already
     carries peer pins; the coordinator's and the login link's do not come from there). Browser
     WebTransport pins `HostEndpoint.cert_hash` — short-lived self-signed certs have a validity limit in
     browsers; check what the host generates and how often a pod restart changes it.
   - **Native QUIC still skips certificate verification** (`SkipServerVerification`,
     `crates/client/src/netcode.rs`) while sending a bearer join JWT: pin from `HostEndpoint.cert_hash`
     or use a real certificate before any `jwt` host is public (TODO line "R2b/R5").
   - **Probes and lifecycle against the lease.** A liveness probe that kills a host the lease would
     have replaced anyway, or a `terminationGracePeriodSeconds` shorter than a leave cut, both turn a
     clean drain into a crash. SIGTERM → leave cut → exit 0 already exists; exit codes 3 (fenced),
     4 (coordinator thread died), 5 (world rollback) must map to "restart the pod", and a restarted
     pod of a replaced ordinal WAITS in `register_host` until its succession settles — a readiness
     probe must not flap on that. `scripts/dev_r3.sh` is the reference supervisor: every rule in it is
     a rule the StatefulSet must reproduce or consciously drop.
   - **Clocks.** The lease uses Postgres' clock (`clock_timestamp()`); the world clock anchor does not.
     One node today, so node clock skew is zero — say so, and say what breaks on a second node.
   - **Rolling updates.** A new image restarts pods one by one: each is a leave (or a crash) and a
     succession, and server and client builds must match. The spec's "blue/green by `build_id`" has no
     code behind it — decide what an update of a running world is in R5 (stop the world, update,
     start is an acceptable first answer if stated).
5. **Login.** The login service links to the ENTRY host only (`VOXEL_ENTRY_HOST`, the host link on
   `VOXEL_HOST_LINK_BIND`) — a dead entry host stops logins until it is back. "Several entry hosts" is
   on R4's R5 list: decide whether R5 does it or accepts the limit with one stated mitigation. Worlds
   are registered with login by an operator command today (`just orion-login …`); `jwt` hosts need
   `VOXEL_OIDC_*` and the service token as k8s Secrets (never in tfvars that get committed, never
   printed).
6. **The client's way in.** The deployed web build must use the login service (the deferred Task 4)
   and restore the `{origin}/login` fallback of `login::api::resolve_login_url` (TODO line "R5"); the
   player-facing name is **Errantry**, `orion` stays the code name.
7. **Observability.** Host and coordinator metrics scraped like login's, logs to Loki through Alloy;
   the `[server]` counters line and `[perf] event=…` lines are the existing signal — check whether the
   server exposes any HTTP metrics or health endpoint at all today (none was found by a quick grep on
   2026-11-15) before writing probes and dashboards against one. Alerts worth having on day one: no
   committed cut in N periods, a succession declared, a world rollback, a host restart loop.
8. **The local twin.** The spec says k3d replaces k0s-in-docker locally (`deploy/envs/local`,
   `just deploy-local`). Decide whether R5 ports it or only targets orion, and what proves a manifest
   change before it reaches the box.
9. **The management panel** (htmx, creates worlds, sets the replica count, launches the forge as a Job)
   is in the spec's R5 line. It is a product of its own; recommend to Dawid whether it is R5, R5b or
   later — `worldctl` covers forge / super-region / clock-reset / retire-host / rollback today.
10. **Not R5** (extension points only): autoscaling and moving land between live hosts (the elastic
    plan), rate limiting (§0.11, deferred), GDPR delete/export (listed for R5 in `TODO.md` R2a — ask),
    exact item conservation across a takeover, several nodes.

## 4. Questions to settle with Dawid before or in the plan

1. Did the R4 kill checks pass by hand, and should R5 wait for the unexplained correction counts?
2. Scope: is the panel in R5? Several entry hosts? k3d? GDPR?
3. How many hosts for the first world, and which region size (`region_blocks`) is the deployed build?
4. What is an update of a running world in R5 — stop/start, or rolling with successions?
5. The world database: CNPG `Cluster` per world or one cluster with a database per world (the spec
   says the latter); backups now or stated as absent.
6. Public exposure: which UDP ports on the box, and behind which hostname (all hosts live under
   `errantry.renownandruin.com` per the standing note; the deploy still uses
   `factoriosurvivalworld.com` names — confirm which is current).

## 5. Rules that are easy to break

- **Deployment:** OpenTofu, not Terraform. `just orion-plan <root>` before every `orion-apply`, and
  **an apply to the live box, a pushed image, a DNS or firewall change are Dawid's to approve each
  time** — a plan may prepare them, an implementer never runs them unasked. Ask before changing
  anything in Grafana; durable changes go in `deploy/modules/monitoring/`.
- **Secrets:** `.env` and `deploy/**/*.tfvars` are git-ignored; never commit, echo or log them. Do not
  put a token on a command line that gets logged.
- **Store:** never a store write that skips the fence; never a table two hosts' parts can both write;
  migrations `0001`–`0013` are never edited; a new `hosts` / `super_regions` / `successions` statement
  needs its lock walk (`world-store/src/succession.rs` module notes).
- **Lease:** the heartbeat beats from the host's MAIN loop on its own connection — never a timer
  thread, never a sidecar. A k8s probe is not a substitute for it and must not fight it.
- **Coordinator and hosts share `VOXEL_OWNERSHIP`.** `just dev-r3` silently ran without successions for
  the whole of R4's build because the coordinator's recipe lacked it; manifests have the same trap.
- **The justfile is canonical:** every new build / deploy / test command gets its recipe in the same
  change. Go through `just` (CPU cap); raw `cargo` is not pinned.
- **Testing:** never whole suites as a routine check; `just test-one` for the touched tests,
  `just smoke` as the sanity check; **ask before running client tests, any live run, any measurement
  or benchmark** (Dawid runs his own on this box). `just test-r4` is the takeover crash table.
- **rustls:** ring is the workspace's only crypto provider — keep `aws-lc-rs` out (a new k8s client
  crate such as kube-rs must be checked for it).

## 6. Tools you have

- `just dev-r3 <hosts> [delay]` / `scripts/dev_r3.sh` — the whole stack on one machine and the
  reference for restart rules; `just coordinator`, `just host`, `just dev-r2`.
- `just go-nogo-r3 <delay> [seconds]` — the seam probe with a verdict (exit 0 GO, 1 NO-GO, 2 INVALID,
  3 none); leave its client windows open; its world lives in its own database.
- `just test-r2 | test-r3 | test-r4 | test-pg | test-threads`, `just smoke`.
- `worldctl` (`forge`, `super-region`, `clock-reset`, `retire-host`, `rollback --yes`).
- `just orion-plan|apply <root>`, `orion-login …`, `orion-login-logs`, `orion-login-push`,
  `orion-grafana`; read-only Grafana API access through `GRAFANA_SERVICE_TOKEN` in `.env`.
- `crates/pg-test-support` (`MMO_TEST_DATABASE_URL`) for throwaway Postgres databases.

## 7. How to work

1. Read §2, then verify §1 and §3 against the code and the cluster definitions — line numbers and
   even facts drift (the tree moves under you). Correct this handoff's claims in the plan where they
   are wrong.
2. Settle §4 with Dawid (one round of questions, defaults proposed), then write the plan: decisions
   first, then tasks small enough for one implementer each, each naming its files, its tests and how
   it is verified WITHOUT touching the live box. Separate what an agent can do (manifests, images,
   server changes, tests, a local twin) from what only Dawid does (applies, pushes, DNS, firewall,
   live checks) — the second list is the plan's hand-check section.
3. The server-side changes (advertised peer address, pins without shared files, probes or health
   endpoint, QUIC pinning) are ordinary code tasks with seeded or real-host tests and can land before
   any manifest.
4. Execution: subagent-driven, a ledger under `.superpowers/sdd/<plan>/`, one whole-branch review, one
   fix wave. Record every decision the plan did not make as a ruling with its cost if wrong.
