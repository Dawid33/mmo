# Handoff: elastic region hosts (E1–E6) — the design plan

**Date:** 2026-11-14
**For:** a fresh session, **after R5 is built** (R4 is built; R5 is in progress as of 2026-11-15).
**Job:** turn the design `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md` into
implementation plans (steps E1–E6; E6 is the PostgreSQL consolidation) (`superpowers:writing-plans`), one per step below, have Dawid review each, then
execute it subagent-driven. No step has an implementation plan yet, on purpose: a plan names exact
files, signatures and tests, and R4 and R5 are still rewriting the code those would name.

**Outputs:** one plan per step under `docs/superpowers/plans/` (`<date>-elastic-<n>-<name>.md`), each
shipping working software on its own; then its implementation, committed task by task on `develop`.

## 1. Where things stand (2026-11-14)

- **R3** (many hosts, one world) is built and accepted. **R4** (takeover) is being implemented by
  another session: Tasks 1–5 are committed (host leases, claiming free land, `Ownership` as a trait,
  `PgClaim`, `Owns`); succession, the peer journal, drain and the client's rejoin are not built.
- **R5** (k8s) is being implemented (2026-11-15; plan `2026-11-15-r5-k8s.md`): a StatefulSet of hosts with
  per-pod Services, the coordinator as one replica, the login Ingress, CloudNativePG.
- **The login service is live on orion** (`deploy/modules/login`, image in ECR, two pods). The
  deployed game does not use it yet (Task 4 of `2026-11-14-login-service-on-orion.md`, deferred).
- This work **comes after R5 and replaces its StatefulSet** (Dawid, 2026-11-14).

## 2. Read first

1. `CLAUDE.md` — the rules; its server / store paragraphs describe what is built.
2. `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md` — the design: decisions 1–9,
   the pieces, who creates hosts, removing a host (steps A and B), the failure table, open questions.
3. The R4 plan as amended at its end ("Amendments made during execution"), and R5's plan once it
   exists: what was really built is the starting point, not their first text.
4. `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0.4, §0.7 — the
   settled multi-host design this extends.
5. `TODO.md` — known limits and residuals of R3 / R4 / R5.

## 3. The steps

Each step is its own plan and leaves the game playable. Sizes are estimates from reading the code on
2026-11-14; re-estimate against the code you find.

### E1 — hosts as individual Deployments (a fixed set)

**Delivers:** N region hosts, each its own Deployment and Service, declared in OpenTofu in place of
R5's StatefulSet. Scaling is changing a number and applying. A warned drain (design §5 step A) works.

**Scope:**
- A host's identity is an id issued at registration, not an ordinal. Re-key what is keyed by ordinal:
  the `hosts` table, peer sessions and their boot-scoped resume, observer tickets, `departed`, the
  configured host count, in-flight cut rows, R4's journal and succession rows.
- A dead host never returns as itself: a restarted pod registers as a new host.
- Discovery through `hosts` only (no address derived from a number); each host advertises its client
  and peer addresses from its environment.
- The login service links to the coordinator (a stable Service), which relays spawn orders and kicks
  to the host that holds the player. Decide where placement lives (today: the entry host,
  `world_manager/placement.rs`).
- The warning: an in-game notice and countdown before a host drains (`DRAIN_WARNING_SECS`), then R4's
  leave cut and the clients' own rejoin.
- OpenTofu: one Deployment + Service per host from a list; a PodDisruptionBudget per host; client UDP
  ports per host (settle the open question: node ports per Service, or host networking).

**Must settle in the plan:** the migration of an existing world's `hosts` / ownership rows from
ordinals to ids (immutable migrations, both engines); whether `Static` ownership survives as the
escape hatch; what R4's "one heir per predecessor" becomes.

**Size:** large — comparable to R3's peer work, because the ordinal is everywhere.

### E2 — the coordinator without a leader

**Delivers:** several coordinator replicas, any of which serves any request; losing one loses nothing.

**Scope:**
- Every coordinator decision is a compare-and-set transaction in PostgreSQL that bumps an epoch the
  store checks (cut decisions and host registration already lock the world row; history, claims and
  the attached-hosts view are in memory today).
- Timed work (start a cut, watch leases, the calendar) is "claim it if it is due": any replica tries,
  one wins.
- Hosts and the login service reach "a coordinator" through a Service; a request may land on any
  replica (requests are already memoised by `RequestId` — that memo must move to the database or be
  made unnecessary).
- The world id travels on every link (bump `LINK_PROTOCOL_VERSION`), so one deployment can serve
  several worlds later.

**Must settle in the plan:** which in-memory state becomes rows and which is rebuilt per request; the
lock matrix for every new statement (a prepared transaction is not a waiting backend — R3's lesson);
what a host does when two replicas answer the same request.

**Size:** medium to large.

### E3 — the coordinator assigns land; idle land is released

**Delivers:** land gets an owner when it becomes active and loses it when idle; new hosts fill from
new activity; nothing running is ever moved. Includes the empty-land move.

**Scope:**
- "Who should own this super-region?" is answered by the coordinator (least-loaded live host),
  replacing the rendezvous preference in `PgClaim`.
- Release in a cut decision: a host whose super-region has no running region, no subscriber and no
  uncommitted parked snapshot gives it up as part of a committed cut; **its fence shrinks** (today a
  fence only grows); peers learn the land is free (a new peer message beside `Owns`).
- Hosts report load in their heartbeat row (players, running regions, tick headroom).

**Size:** 4–6 tasks.

### E4 — the reconciler and scaling

**Delivers:** the world adds and removes hosts by itself, down to zero when nobody is online.

**Scope:**
- Table `desired_hosts`; the coordinator writes "should exist" / "draining" from the load reports,
  with hysteresis and a minimum host lifetime.
- The reconciler (`VOXEL_ROLE=fleet`, its own Deployment and ServiceAccount): one Deployment +
  Service per desired host, ports read back from the Service and passed to the host as its advertised
  address; deletion when the row is gone; labels on everything; idempotent.
- A draining host gets no new land; it is deleted only when it holds nothing (E3 releases empty land;
  occupied land waits for E5, or uses the warned drain).
- Scale to zero: a login for a world with no host asks for one; the join waits, bounded, and says so.

**Must settle in the plan:** the Kubernetes client for Rust and its TLS stack (the workspace allows
only ring — check before choosing `kube`); what the reconciler does when the API is down; RBAC as
narrow as it can be.

**Size:** medium.

### E5 — move occupied land in place

**Delivers:** an occupied host can be removed, and a hotspot relieved, by moving one super-region to
another host with a warning and a pause of seconds, without a reconnect (design §5 step B).

**Scope:**
- Freeze the super-region's regions at a tick and commit them in a cut; pass ownership to the taker
  inside that cut's decision under a new epoch; the taker loads them.
- A re-home in place: a player's session hops to the taker keeping the exact pose (today
  `move_to_peer` starts from a parked record and lands at `landing_in`).
- Clients re-map the regions they observe to the new owner (shared with R4 Task 16), and open their
  connection to the taker during the warning period.
- Messages for the land during the gap are held and delivered to the new owner; a player hand-off
  addressed to the old owner must not be parked.
- Gating against an in-progress cut; the warning and countdown UI; the failure table of the design
  (§6) as tests — taker dies mid-move, giver dies mid-move, coordinator replica dies mid-decision.

**Size:** 10–14 tasks.

### E6 — consolidate PostgreSQL into two CloudNativePG clusters (the final step)

**Delivers:** exactly two PostgreSQL clusters on orion, both run by CloudNativePG: the **region**
instance (R5's `pg`, a database per world — unchanged) and a new **management** instance holding
Keycloak's, the login service's and chat's databases. The three StatefulSet databases are gone. This
is the database half of the split in design §10 (a management cluster and per-region clusters).

**Scope:**
- A second `Cluster` (its own namespace, storage, roles and nightly dump — never the region
  instance's namespace, role, volume or backup target), one database and one role per service.
- Move each service in its own maintenance window, smallest blast radius first: the login service
  (two pods, a short stop: players already in a world are not affected, new logins and rejoins wait),
  then Keycloak (sign-in is down while it moves), then chat. Per service: stop writers → dump →
  restore into the management instance → point the service at it (the URL from the operator's
  secret, never a password in state where it can be avoided) → verify → keep the old volume,
  untouched, until the owner says it can go.
- Remove the per-service PostgreSQL from the modules (`deploy/modules/login/postgres.tf`, Keycloak's
  `postgres.tf`, chat's) once each move is verified, and their variables and outputs.
- Connection budget: every service's pool × replicas against the management instance's
  `max_connections` (the login service: 8 per pod, three pods during a rollout).
- Backups: a nightly dump for the management instance like the region instance's, to its own target.
  The account → character table in the login database is one of the two things that cannot be
  rebuilt (the other is the world).
- Docs: `CLAUDE.md` Deployment paragraph, the roots' apply order, the justfile's recipes.

**Must settle in the plan:**
- Whether chat's database can move at all: Zulip ships its own PostgreSQL image with full-text search
  extras, and CloudNativePG runs its own image — check what the deployed chart needs before promising
  it; "chat stays on its own instance" is an acceptable outcome, said plainly.
- PostgreSQL major versions on both sides of each move (dump / restore across a major version is
  fine; an in-place volume is not an option).
- The rollback for each service (point back at the old StatefulSet, which was left running read-only
  or stopped with its volume intact) and how long the old volumes are kept.
- Whether the operator on orion needs anything beyond what R5 installed.

**Does not include:** moving anything into or out of the region instance; a second Kubernetes
cluster; high availability for either instance (one node has nowhere to put a replica).

**Size:** medium — infrastructure and three careful moves; no game code.

### Later — one coordinator deployment for all worlds

Needs E2's world id on the links and the process-global forged world (`worldgen::forged`, a
`OnceLock`) untangled if the coordinator depends on it. Not sized.

## 4. Rules that carry over (learned in R3 and R4)

- **Conservation beats latency; wait, never guess.** Every entity, item and player exists exactly
  once across live state and the store, through any crash point. A task without a conservation test
  is not done.
- **Fencing in PostgreSQL is the only safety.** Nothing a Deployment, a Lease or the reconciler does
  may be what prevents two writers. Never add a store write that skips the fence.
- **A cut part writes only rows of its own regions**; migrations are immutable and identical in both
  engines; walk the lock matrix for every new statement.
- **Wire enums are append-only**; link enums change only with a `LINK_PROTOCOL_VERSION` bump; client
  and server have no version handshake (an upgrade is a stop of the world and a rejoin — by decision).
- **Filtered test runs hide breakage**: after a wide change run each touched crate's whole
  non-client suite once. Scheduling-sensitive correctness goes in the seeded layer
  (`crates/game/tests/peer_schedule.rs`, `multi_rig.rs`); real processes in `crates/server/tests/hosts.rs`.
- **Ask Dawid first** before client tests, live runs, measurements, and every apply on orion.
- **Rerun the seam probe** (`just go-nogo-r3`) after any step that changes who owns land under a
  moving player, and compare pooled runs.

## 5. Decisions already taken (do not reopen without Dawid)

- One Deployment and Service per host; not a StatefulSet, not replica scaling, not Agones or
  OpenKruise Game.
- The coordinator decides; a separate reconciler acts (coordinator-reconciles-itself stays open only
  if the extra process proves a nuisance).
- No leader in the coordinator.
- Land is assigned lazily and released when idle.
- An occupied host is removed only after its land has been moved, with ample warning to its players.
- No truly live move (no cut, under a second): the warning makes a pause of seconds acceptable.
- Upgrades are a stop and a rejoin.
- **PostgreSQL: two CloudNativePG clusters; the consolidation is this plan's final step, E6** (Dawid,
  2026-11-14/15). The target is one *management* instance (Keycloak, login, chat) and one *region*
  instance (a database per world) — the split of a future management cluster and per-region clusters
  (design §10). R5's `pg` cluster is the region instance; until E6, login's, Keycloak's and chat's
  databases stay their own StatefulSets. No step may put management data into the region instance,
  read across the two, or assume they share a cluster.
- **Everything on the world side must be movable to another cluster later** (design §10): the
  coordinator, the reconciler and the hosts reach the management side only over the login service's
  public HTTPS and the service link; a world's addresses in the login registry are names that can be
  public.

## 6. Open, for Dawid

- Client UDP per host: node ports or host networking; and the shape on several nodes.
- The warning period, and what a player may do during it.
- How long the first login of a quiet world may wait (scale to zero).
