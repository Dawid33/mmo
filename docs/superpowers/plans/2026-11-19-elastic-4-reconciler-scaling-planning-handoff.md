# Handoff: Elastic 4 — the reconciler and scaling; scale to zero

**Date:** 2026-11-19
**For:** a fresh session, **after Elastic 3's two open findings are fixed** (§2). Check `TODO.md`'s "Elastic 3" block: if the bullet "OPEN after the scoped re-review of the fix wave" is still there, stop and say so. E4 creates and removes hosts on purpose, and both findings are about a host dying or a host that dropped its land.
**Job:** write the E4 implementation plan with `superpowers:writing-plans` as `docs/superpowers/plans/<date>-elastic-4-reconciler-scaling.md`, have Dawid look at it, then execute it subagent-driven. One plan. E5 and E6 get their own handoffs after it.

The decisions in §5 are defaults this handoff takes so the plan can proceed. They come from the design and from Elastic 3 as built at `bd49b08c`. **Verify each against the code.** Where the code contradicts one, re-rule it in the plan under "Where this plan departs from the handoff" and keep going: do not stall. The ones marked **(Dawid)** are listed again in §11.

## 1. Read first, in this order

1. `CLAUDE.md`: "Testing" (the rules changed on 2026-11-19, §8), "Deployment" (the world module, the operator recipes, the twin, the world panel), and under "Server / store": "Many hosts" (Ownership, Lease, Leave and drain, Registration), "Owner epochs & fencing".
2. The design spec, `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md`:
   - decisions 1, 3, 6 and 9;
   - §4 "Who creates and destroys host Deployments", with its "Scaling signals" paragraph;
   - §5 step 3 (a host that holds nothing exits and is deleted);
   - §6, the rows "Reconciler down", "Kubernetes API down", "New host registers, nothing to give it", "Login for a world with zero hosts";
   - §9, the open questions on client UDP and scale-to-zero cold start;
   - §10 (everything on the world side must be movable to a region cluster).
3. The parent handoff, `docs/superpowers/plans/2026-11-14-elastic-region-hosts-planning-handoff.md`: §3 "E4", §4 and §5.
4. The Elastic 3 plan, `docs/superpowers/plans/2026-11-18-elastic-3-land-assignment.md`: "Decisions settled in planning" items 1, 2 and 5, "Known limits", "As built" and "Final review fix wave". That is the machinery E4 drives: load in the beat, assignment by load, release of idle land, the clean leave that releases.
5. The Elastic 2b plan's "Decisions settled in planning" items 1–4: a coordinator decision is a store decision inside the transition, and timed work belongs to the ticker. E4's scaling decisions are of that kind.
6. `TODO.md`: the Elastic 3 block (open items, known limits, hand checks) and the Elastic 1 block ("a host added after a release starts on `:latest`").
7. `docs/superpowers/plans/2026-11-15-r5b-world-panel.md` and `crates/panel/src/kube.rs`: the repository already speaks to the Kubernetes API with reqwest and ring, not kube-rs.

## 2. Before E4: what Elastic 3 left open

Both are in `TODO.md` with their fixes. They are small. Either fix them first as their own short change, or make them Task 1 of the E4 plan.

1. **The keeper's open-run check rests on a read order the store does not give.** `DirectoryRead::start` (`crates/server/src/claims.rs`) issues the owners read and the `hosts` read as two requests on pooled connections. A declaration landing between a fast `hosts` read and a slow owners read makes the heal fence an heir's inherited land before its succession order (entities twice). Fix: start the `hosts` read only after the owners read answered, or read both in one snapshot.
2. **A stale `departed` pointer at a live host after `drop_stored_parked`.** A login bounces between two hosts until `SPAWN_RELAY_MAX_HOPS` and is refused, on every retry. Fix: drop `departed[P]` when a spawn order for P arrives from the host it names.

Also relevant to E4, from the same re-review:
- A `Leave` succession now waits for a cut later than the leave cut (`LeaveGate`), up to one save period. A drained host that kept land (it had in-flight rows) is unreachable for that long.
- A landless leaver's `hosts` row can be pruned by a registration between its leave cut's decision and that part's `COMMIT PREPARED`. E4 makes registrations and leaves frequent and concurrent: **the plan must close this** (for example, `prune_hosts` skips a row that has a staged part).
- A survivor that registered during the last committed cut has a journal base one cut behind; a succession order then waits to its deadline and the world rolls back. The `LeaveGate` covers only the heir. Scale-up during a failure is E4's normal case: **the plan must rule on it.**

## 3. Where things stand (code map at `bd49b08c`; line numbers approximate)

**Load (Elastic 3 Task 1)**
- `hosts.load_players`, `load_regions`, `load_tick_permille` (migration `0018_host_load`), written by the beat.
- `game::HostLoad { players, regions, tick_permille }`; `HostRow.load`; `HostSeen.load` (`crates/game/src/coordinator/succession.rs`).
- The sampler: `crates/server/src/load.rs` (`TickLoad`), the main loop's `pump_or_fence`.
- Known minor: the load window resets when a beat is sent, not when it succeeds.
- Gauges on a host: `mmo_host_load_players|regions|tick_permille`, `mmo_host_land`, `mmo_land_released_total`. On a coordinator: `mmo_land_assigned_total`, `mmo_hosts_live`.

**Assignment and release (Elastic 3)**
- `crates/game/src/coordinator/assign.rs`: `on_land_read`, `pick_assignee` (attached, live, `takes_logins`; skips hosts seen for less than `ASSIGN_MIN_ATTACHED_MS` and hosts over the headroom limit while another qualifies), the provisional load.
- `takes_logins` = not leaving and not in the durable `draining` set (`coordinator/mod.rs`).
- A host releases idle land in its own cut part after `LAND_IDLE_MS` (300 000; dial `VOXEL_LAND_IDLE_MS`, module variable `land_idle_ms`): `crates/game/src/world_manager/land.rs`.
- A clean leave releases all its land unless its part carries in-flight rows (then a `Leave` succession, declared by the coordinator on its own after a later cut).
- A placed spawn order pins its land for `SPAWN_PIN_MS` (120 s) (`world_manager/placement.rs`).

**Drain (Elastic 1)**
- `worldctl drain <url> <host id> [--in <secs>]` sets `hosts.drain_at_ms` (`world-store/src/ctl.rs` `drain_host`; default `DRAIN_WARNING_SECS` 300, at most `DRAIN_MAX_SECS`). A mark only moves earlier; there is no un-drain.
- A `claim` host reads the mark at its beat, tells its home sessions (`ServerPacket::HostDraining`), stops being a gate, and at the deadline leaves through its leave cut and stays up **drained** (`/readyz` 503 `drained`, no beat, no store call) until its Deployment is removed.
- `worldctl drain` warns when it marks the last host not draining.
- There is **no coordinator-side drain decision**: the mark is an operator's write.

**Hosts on Kubernetes (Elastic 1, `deploy/modules/world/hosts.tf`)**
- `var.hosts` is a map `name → { quic_node_port, wt_node_port }`, at least one entry, names `^[a-z][a-z0-9]{0,11}$`.
- Per key: `kubernetes_deployment_v1.host` (one replica, `Recreate`), `kubernetes_service_v1.host_endpoint` (NodePort, both UDP ports fixed by the map), `kubernetes_pod_disruption_budget_v1.host` (no voluntary disruption).
- The pod's advertised addresses are `VOXEL_ADVERTISE_QUIC = <public_host>:<quic_node_port>` and `VOXEL_ADVERTISE_WT`; its peer address is `$(POD_IP):6500`; its name is `VOXEL_HOST_NAME = <key>`.
- A host removed today: `world-drain <name>` → remove its key from `var.hosts` → apply.
- A host added after a release starts on `var.images.server` (`:latest`) beside the released digest (`TODO.md`, Elastic 1).
- `world-stop` / `world-start` / `scripts/world_update.sh` find hosts by the label `app.kubernetes.io/component=host` and poll each Deployment's `status.readyReplicas`. `crates/panel/src/ops.rs` mirrors them step for step ("change one, change the other"). `crates/panel` is still untracked.

**Zero hosts today**
- A coordinator replica with no gate host answers `HostToLogin::NoGate`; every link `NoGate` → the join answers 503 (`crates/login/src/host_link.rs`).
- The client retries a join on 503 (`REJOIN_RETRY_MS`, gives up after `REJOIN_MAX_MS`, 60 s).
- Cuts begin on the save period whether or not any host owns land.

**The Kubernetes client**
- `crates/panel/src/kube.rs` speaks to the API server with reqwest over rustls with ring. kube-rs is not in the tree, and the workspace allows only ring (`aws-lc-rs` must stay out).

**Nothing exists yet for:** a `desired_hosts` table, a `fleet` role, any scaling decision, a way to add a host without an OpenTofu apply, a join that waits for a host to start.

## 4. What E4 delivers

- **The world adds and removes hosts by itself.** The coordinator decides how many hosts should exist and which one drains; a separate reconciler makes Kubernetes match.
- **A draining host gets no new land and is deleted only when it holds nothing.** Elastic 3 releases its empty land; occupied land waits for the warned drain (Elastic 1) until Elastic 5 can move it.
- **Scale to zero.** With nobody online every host may go. A login for a world with no host asks for one, and the join waits, bounded, and says so.

What this closes: the design's decision 6, and the scale-down half of "capacity follows load".

## 5. Decisions taken (defaults; verify against the code)

### 5.1 `desired_hosts` is a table the coordinator writes and the reconciler reads

- Migration `0019_desired_hosts` (the next after `0018`), both engines, identical in shape. Columns to settle in the plan; the default: `name TEXT PRIMARY KEY`, `state BIGINT` (wanted / draining), `created_ms BIGINT`, plus what the reconciler writes back (the ports the Service was given).
- A row is written only by a coordinator **store decision inside its transition** (`AddHost`, `DrainHost`, `RemoveHost`), as `AssignLand` is. Its lock walk goes into `world-store/src/succession.rs`'s notes.
- **Must settle:** who writes the port columns. A cut part writes only rows of its own land, and nothing but the coordinator writes coordinator-owned rows; a table two roles write needs its columns split by writer, or two tables.
- **Must settle:** how a `desired_hosts` name relates to a `hosts` row. A host id is issued at registration and never reused; a name is a label. The link is `hosts.name`. A pod that crash-loops registers many ids under one name.

### 5.2 The reconciler is a role of the server binary, speaking to the API with reqwest

- `VOXEL_ROLE=fleet`, its own Deployment and ServiceAccount (design §4). It reads `desired_hosts`, and for each wanted row creates the Service, reads back the node ports, then creates the Deployment with those ports as the advertised address; for a row that is gone it deletes both. Everything it makes carries a label, so strays are found. It is idempotent: a crash half-way is repaired on the next pass.
- **The client is reqwest with ring, as `crates/panel/src/kube.rs`** (Dawid). Move that module where both can use it, or depend on it; do not add kube-rs without checking its TLS stack.
- RBAC as narrow as it can be: Deployments, Services and PodDisruptionBudgets in the world's own namespace, nothing cluster-wide.
- The reconciler never talks to the coordinator. Correctness never depends on it: with it down, no host is created or deleted and everything running keeps running (design §6).
- **Must settle:** OpenTofu's `var.hosts` and the reconciler cannot both own host Deployments. The default: the module keeps the coordinator, toolbox, web and the reconciler; `var.hosts` becomes the **seed** written into `desired_hosts` at forge (or goes away), and host Deployments are no longer in the state. Say what happens to a world deployed before E4 (worlds are wiped and re-forged, never migrated).
- **Must settle:** the image a new host starts on. It must be the coordinator's own digest, never `:latest` (the Elastic 1 open item). The reconciler copies it from the coordinator Deployment, or the release writes it where the reconciler reads it.
- **Must settle:** `world-stop`, `world-start`, `world_update.sh`, `world-drain` and the panel's `ops.rs` find hosts by label today and scale Deployments directly. With a reconciler they would fight it. Rule on each: either they write `desired_hosts`, or the reconciler is paused during a stop.

### 5.3 Scaling decisions are ticker work, from the load in the `hosts` read

- The ticker alone decides, from the same `hosts` read that feeds leases and assignment.
- **Add a host** when the least-loaded host that takes logins is above a threshold for a while (design §4). **Drain one** when the total load would fit in fewer hosts with room to spare.
- Hysteresis and a minimum host lifetime, so it does not flap. Tunables as named consts (§5.6).
- The drain is the existing mark (`hosts.drain_at_ms`), now set by a coordinator decision. It keeps its warning period and its one-way rule.
- **Which host drains (Dawid):** the default is the one holding the fewest players, then the least land, never the last host that takes logins while anyone is online.
- A host that registers and is given nothing idles with its lease; it is removed again if still unused after the minimum lifetime (design §6).
- **Must settle:** the bounds. A minimum and a maximum host count per world, from the module, so a bug cannot create a hundred pods.
- **Must settle:** what "load" is. The beat reports players, running regions and the worst tick permille. Players may be the wrong key for scale-up: one crowded super-region is one thread and more hosts do not help it (design §6).

### 5.4 Scale to zero, and the first login of a quiet world

- With nobody online and no land busy, every host may drain. Elastic 3's clean leave releases its land, so a drained host leaves nothing behind.
- A login with no gate host: the coordinator asks for a host (a `desired_hosts` row) and the join waits, bounded, with an answer that says "world starting" instead of a bare 503.
- **Must settle:** where the wait lives. Today the client retries a 503 for at most `REJOIN_MAX_MS` (60 s). A cold host start is a pod schedule, an image pull, a registration and an attach. Measure it on the twin before choosing the bound (ask Dawid first: it is a measurement).
- **Must settle:** what keeps cuts and the calendar honest with zero hosts. Cuts begin on the save period regardless; with no participant there is nothing to decide.
- **The default (Dawid):** the minimum is one host, and zero is an opt-in per world, until the cold start is measured.

### 5.5 What does not change

- Fencing in the store is the only safety. Nothing the reconciler does keeps two writers apart.
- `static` and `allinone` are unchanged; only `claim` worlds scale.
- Assignment, release, successions and the leave cut are Elastic 3's, unchanged, apart from §2.
- Client UDP stays node ports per host Service on one node (the design's open question, settled by Elastic 1). Several nodes are out of scope.
- The coordinator still never talks to the Kubernetes API.

### 5.6 Tunables (named consts; defaults to argue in the plan)

| Constant | Default | Note |
|---|---|---|
| `SCALE_UP_LOAD_PERMILLE` | to argue | The least-loaded host's load above which a host is added. |
| `SCALE_UP_AFTER_MS` | 60 000 | How long that must hold. |
| `SCALE_DOWN_SPARE_PERMILLE` | to argue | Room to spare required before one host drains. |
| `HOST_MIN_LIFETIME_MS` | 600 000 | No host drains younger than this. Must exceed `ASSIGN_MIN_ATTACHED_MS` (30 s) and `LAND_IDLE_MS` (5 min). |
| `SCALE_DECISION_EVERY_MS` | 10 000 | One decision per pass at most. |
| `WORLD_START_WAIT_MS` | to measure | The bound on a join that waits for the first host. |

## 6. Order of work (the plan decides; this is a suggestion)

1. Elastic 3's two open findings and the two E4-relevant residuals of §2.
2. `desired_hosts`: the migration, the store decisions, their lock walks, the seed at forge.
3. The Kubernetes client shared with the panel; the `fleet` role: create, read back ports, delete, labels, idempotence. Runs on the twin against a hand-written row.
4. The module: the reconciler's Deployment, ServiceAccount and Role; host Deployments leave the OpenTofu state; the image rule.
5. The recipes, `world_update.sh` and the panel against a reconciler.
6. The scaling decisions in the core: add, drain, remove; hysteresis; bounds.
7. Scale to zero and the waiting join.
8. Metrics, alerts and twin phases: a host added under load, a host drained and deleted, zero and back.
9. Docs and closing.

**Sizing:** the design says "medium". With the recipes, the panel and scale to zero, expect 8–10 tasks.

## 7. Rules that carry over

- **Conservation beats latency; wait, never guess.** Every entity, item, player and person exists exactly once through any crash, conflict, abort or release point.
- **Fencing in the store is the only safety.** Fence before use; shrink before the next part. Never a store write that skips the fence. A cut part writes only rows of its own land.
- **Write-before-send** for every coordinator output. A scaling decision is a store decision in the transition: issuing it is committing it.
- **Lock walks** for every new or changed statement on `hosts`, `super_regions`, `coordinator`, `desired_hosts`. A prepared transaction is not a waiting backend. Elastic 3 lesson: `INSERT … ON CONFLICT DO NOTHING` waits for an uncommitted updater of the conflicting row.
- **Migrations** are immutable and identical in shape on both engines. Worlds are wiped, never migrated.
- **Wire enums are append-only.** Bump the link protocol once per plan, and only if a message is added. Every new core field is classified in `projection!`, every new `WorldManager` field in `host_fields!`, every new input as replayed or ticker-only.
- **Everything on the world side must be movable to another cluster** (design §10): the reconciler reaches only its own cluster's API and the world's database.
- **Ask Dawid first** before client tests, live runs, measurements and anything on orion. `just twin-check` is allowed. The twin needs disk: at 90 % kubelet's image GC deleted its imported images (`TODO.md`, Elastic 3 hand check 5).
- Never run `rustfmt` or `cargo fmt` from the root. Commit only your own paths; filtered staging beside Dawid's hunks (`-U3`, `--recount`).

## 8. Testing: the rules changed on 2026-11-19

- **Only regression tests are committed**: a test reproduces a bug that was actually seen, in play or in a log, and carries a `/// Regression:` marker. `just test-audit` (the first step of `just smoke`) fails on any unmarked test. Read `CLAUDE.md`, "Testing".
- **So the plan must not contain "write the failing test" steps, re-pin tables or named tests.** Elastic 3's plan did, and its Tasks 7–10 had to be re-ruled mid-execution.
- What a task has instead:
  - the scenarios it must handle, listed as cases the implementer traces through the code in its report and the reviewer checks by reading;
  - temporary tests to drive the change, deleted before the commit (the seeded rig `crates/game/tests/common/multi_rig.rs` is still there);
  - `just smoke`, `just web-check` when `game` changes, `just determinism-wasm` for sim math or anything hashed;
  - **a twin phase wherever one can exercise the path.** E4 is mostly about processes and Kubernetes objects, which the twin can exercise directly. Elastic 3's one twin run was the only time its release path executed.
- **No whole crate suites per task.** `just test` / `just test-full` run when Dawid asks or once at the close, said first.
- Tell implementers to run every command in the foreground. Two Elastic 3 implementers stalled waiting on a background run.

## 9. How the last plan was executed — keep what worked

- Subagent-driven: one implementer and one reviewer per task, a ledger in `.superpowers/sdd/<plan>/progress.md` with rulings as `Ruling: what — why — cost if wrong`, a whole-branch review split by area, one fix wave, one scoped re-review.
- **The whole-branch review found what the task reviews could not.** In Elastic 3 it found six cross-task defects, three of them conservation breaches, all at the seams between tasks (a heal from Task 4 against R4's adoption; a release from Task 6 against a leave from Task 8). Plan for it: write the cross-task interactions into the plan's "Review Focus", and give the final review the most capable model.
- **Write the lock walks and the "who writes which row" table INTO the plan, before the tasks.** For E4 the risks are: two writers of `desired_hosts`; the recipes and the panel fighting the reconciler; a registration racing a leave (§2); the image a new host starts on.
- A reviewer without tests reviews by tracing each named scenario through the code. Say so in every review dispatch.
- Dawid and other sessions work in this checkout at the same time. Their uncommitted files can break `just smoke` and the wasm build; check `git status` before blaming the task.

## 10. What E4 leaves

- **Moving occupied land** (E5). E4 removes an occupied host only by the warned drain, which makes its players rejoin.
- **Re-keying `in_flight` from host to region** (E5), which would let every leave release.
- **Several nodes**, a public address per node or a load balancer per host.
- **One coordinator deployment for all worlds.**
- Rebalancing between live hosts.

## 11. Open, for Dawid (the plan proceeds on the defaults)

- **The Kubernetes client:** reqwest with ring, shared with the panel (§5.2), not kube-rs.
- **Who owns host Deployments:** the reconciler, with `var.hosts` reduced to a seed (§5.2).
- **Which host drains:** fewest players, then least land (§5.3).
- **The bounds:** a minimum and a maximum host count per world, from the module (§5.3).
- **Scale to zero:** opt-in per world, minimum one host by default, until the cold start is measured (§5.4).
- **`crates/panel` is still untracked.** E4 changes what `ops.rs` must do. Commit it before the task that touches it, or that task stops at its gate as Elastic 2b's Task 11 did.
