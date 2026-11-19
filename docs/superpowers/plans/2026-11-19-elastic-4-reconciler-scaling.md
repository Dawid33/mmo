# Elastic 4 — The Reconciler and Scaling; Scale to Zero Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `claim` world on Kubernetes adds and removes its own hosts: the coordinator decides how many hosts should exist and which one drains, a separate reconciler makes Kubernetes match, and a world with nobody online may go down to zero hosts and come back on the first login.

**Architecture:**
- The coordinator writes the table `desired_hosts` through three store decisions inside its transition (`AddHost`, `DrainHost`, `RemoveHost`), as `AssignLand` is. The core keeps **no durable fleet state**: the ticker reads the table with its `hosts` read and decides from that read; every decision is idempotent against the table.
- A new role of the server binary, `VOXEL_ROLE=fleet`, reads `desired_hosts` (read-only store handle) and creates or deletes one Deployment + NodePort Service + PodDisruptionBudget per row from a template the OpenTofu module renders into a ConfigMap. It never writes the store and never talks to the coordinator.
- OpenTofu no longer owns host Deployments: `var.hosts` is replaced by `var.fleet` (bounds, port base, pace dials).
- A login for a world in which no host takes logins is answered "world starting" (`HostToLogin::Starting` → 503 `world_starting`), asks for a host, and the client retries, bounded.

**Tech Stack:** Rust (sqlx on PostgreSQL and SQLite, reqwest over rustls with ring, crossbeam, quinn via `svc-link`), OpenTofu (`deploy/modules/world`), bash (`scripts/world_update.sh`, `scripts/twin_check.sh`), the justfile, Slint (one status line).

**Spec:**
- `docs/superpowers/plans/2026-11-19-elastic-4-reconciler-scaling-planning-handoff.md` (the decisions; re-ruled below where the code disagreed).
- `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md`: decisions 1, 3, 6, 9; §4; §5 step 3; §6; §9; §10.
- `docs/superpowers/plans/2026-11-18-elastic-3-land-assignment.md`: "Decisions settled in planning" 1, 2, 5; "Known limits"; "As built"; "Final review fix wave".
- `docs/superpowers/plans/2026-11-17-elastic-2b-coordinator-replicas.md`: "Decisions settled in planning" 1–4.

## Before Task 1: Elastic 3's two open findings (done 2026-11-19, in the working tree)

Both were fixed before this plan was written (Dawid chose "fix them first"):

1. **The keeper's open-run check.** `DirectoryRead` (`crates/server/src/claims.rs`) now issues the `hosts` read (and the clock read) only after the owners read answered; the type's and `DirectoryKeeper`'s docs say why.
2. **The stale `departed` pointer at a live host.** `WorldManager::departed_denied` (`crates/game/src/world_manager/placement.rs`): a host remembers a spawn order it sent on because of its `departed` pointer (`spawn_sent_on`, `SentOn`, `SPAWN_SENT_ON_MS`); when that very order comes back from that host (same token and epoch, one hop more, this host's hop of the player unchanged) the pointer is dropped and the order is placed by its record's region. **Not** "drop when any order arrives from that host" (the handoff's wording): an order that host sends on its own account proves nothing while a hand-off of this host is still on its way to it — the plain rule would place the player twice.

Checked with a temporary seeded-rig test (deleted): the two-host bounce on the real route (walk, logout, release, `drop_stored_parked`, the land assigned to the pointing host) is refused without the fix and placed with it, in 8 variants (link delay, gate, when the land is assigned); a hand-off in flight while the other host sends an order on its own account leaves the pointer alone and the player exactly once (32 runs).

**Left open by that test — Task 1 closes it:** a CHAIN of two stale pointers. P walked h1 → h2 → h0, h0 dropped the record, the land went to h1: the order circles h1 → h2 → h0 → h1 (h0 sends it by the record's region, not back to h2), so nobody sees its own order come back from the host it points at, and every login is refused at `SPAWN_RELAY_MAX_HOPS`. (With the land at the middle host h2 it works.)

- [ ] Confirm both are committed (`git log --oneline -- crates/server/src/claims.rs crates/game/src/world_manager/placement.rs`) and that `TODO.md`'s Elastic 3 bullet "OPEN after the scoped re-review" says so. If not, stop and ask Dawid.

## Where this plan departs from the handoff (verified against the code at `bd49b08c` + the two fixes, 2026-11-19)

1. **The reconciler does not read ports back from the Service (§5.1, §5.2; design §4).** `VOXEL_ADVERTISE_QUIC` / `_WT` embed the node port, so it must be known before the pod exists; and the twin's k3d maps exactly UDP 30466–30469 to the machine (`justfile` `twin-up`), so a port the cluster picks is not reachable there. Each `desired_hosts` row carries a **slot**; the ports are `port_base + 2·slot` and `+ 1`. The store's `AddHost` picks the lowest free slot below the maximum. So **nothing but the coordinator writes `desired_hosts`**, and the "two writers" question is gone.
2. **No seed at forge, and `var.hosts` goes away entirely (§5.2).** The coordinator's own "below the minimum" rule adds the first hosts. Nothing writes `desired_hosts` but coordinator decisions — not `worldctl`, not the recipes.
3. **Names are issued by the store and never reused** (`world.next_desired_host`, as `next_host_id`): `h1`, `h2`, … A reused name could not be told from its predecessor's Deployment still being deleted. The link to a `hosts` row is `hosts.name`; the coordinator's `HostSeen` gains `name` (the conversion in `coordinator_rt.rs` drops it today). The live host of a name is its live dirty row; a crash-looping pod is many ids under one name, at most one live (a registration lapses its name's older rows).
4. **The core keeps no durable fleet state.** `COORDINATOR_STATE_VERSION` stays 6. The desired rows come with the ticker's `hosts` read (never logged, as `LandOwners`); the decisions carry what they expect (`AddHost.expect_wanted`), so a replay, a duplicate or a second replica changes nothing.
5. **A drained host does not exit (design §5 step 3 "it exits").** It stays up drained as Elastic 1 built it (an exit would make its Deployment restart it as a new host); the coordinator removes its row once every `hosts` row of its name is clean or dead and owns nothing, and the reconciler deletes the Deployment.
6. **The pod's shape stays in the module.** The reconciler does not know env vars, mounts or resources: the module renders the host Deployment, Service and PDB as one JSON template into the ConfigMap `host-template` (placeholders `__NAME__`, `__QUIC_PORT__`, `__WT_PORT__`, `__IMAGE__`); the reconciler substitutes and posts.
7. **The image rule:** a host Deployment is created on the image of the `coordinator` container of the `coordinator` Deployment at that moment (the released digest). This closes Elastic 1's open item (`TODO.md`:260).
8. **The recipes keep working by label and do not write `desired_hosts`; the reconciler is not paused.** Instead it has one rule that makes it harmless during a stop: **it creates and re-templates nothing while the `coordinator` Deployment's `spec.replicas` is 0**, and it never patches an existing Deployment's replicas or image. So `world-stop` / `world-start` / `world_update.sh` scale and re-image host Deployments as today.
9. **`crates/panel/src/ops.rs` is already out of step with the recipes** — it still scales a StatefulSet `hosts` and reads the ConfigMap `host-endpoints` (pre-Elastic 1). Task 9 brings it level, and is gated on Dawid committing `crates/panel`.
10. **The scale-up key is the tick, not players (§5.3 "what load is").** `load_tick_permille` of the LEAST-loaded host that takes logins: one crowded super-region saturates one host and leaves the least-loaded one low, so no host is added that could not help (design §6).
11. **The join's wait lives in the client** (§5.4): the login service answers at once (503 `world_starting`), the client retries every `REJOIN_RETRY_MS` for at most `WORLD_START_WAIT_MS` — also on the FIRST join from the menu, which today shows the error and stops.
12. **Any replica may ask for the first host** (§5.4 "the coordinator asks for a host"): the replica that receives the login order issues `AddHost { expect_wanted: 0 }` itself. Login orders are never logged and may reach a replica that is not the ticker; the decision is idempotent in the store, so it needs neither.
13. **The two Elastic 3 residuals of handoff §2 are Task 1**, ruled in "Decisions" 6 and 7.

## Decisions settled in planning

### 1. Who writes which row

| Rows | Written by | Read by |
|---|---|---|
| `desired_hosts` (all columns), `world.next_desired_host` | the coordinator's `AddHost` / `DrainHost` / `RemoveHost` decisions, inside its transition — nothing else, ever | the ticker (with its `hosts` read), the reconciler, `worldctl desired`, `world-status` |
| `hosts.drain_at_ms` | `DrainHost` (new) and the operator's `worldctl drain` (as today): the same one-row `UPDATE`, a mark only moves earlier | the host's beat, the `hosts` read |
| `hosts` (everything else), `super_regions`, cut parts | unchanged | unchanged |
| Kubernetes: Deployments `host-<name>`, Services `host-<name>`, PDBs `host-<name>` (label `errantry/fleet=<world id>`) | existence and template: the reconciler. `spec.replicas` and the image of an EXISTING Deployment: the recipes (`world-stop`, `world-start`, `world_update.sh`) and the panel, never the reconciler | the recipes, the panel (by label `app.kubernetes.io/component=host`) |
| Kubernetes: Deployments `coordinator`, `toolbox`, `web`, `fleet`, the ConfigMap `host-template`, the Role | OpenTofu (images by `world_update.sh`) | the reconciler reads `coordinator` (image, replicas) and `host-template` |

### 2. The table and the three decisions

Migration `0019_desired_hosts`, both engines, identical in shape:

```sql
CREATE TABLE desired_hosts (
    name       TEXT   PRIMARY KEY,
    slot       BIGINT NOT NULL UNIQUE,
    state      BIGINT NOT NULL,   -- 0 wanted, 1 draining
    created_ms BIGINT NOT NULL
);
ALTER TABLE world ADD COLUMN next_desired_host BIGINT NOT NULL DEFAULT 1;
```

- **`AddHost { max, expect_wanted }`** → `HostAdded::Done { name, slot } | NotNeeded | Full`. Adds a row only when the number of `wanted` rows equals `expect_wanted` (else `NotNeeded`) and the number of ALL rows is below `max` (else `Full`: a draining host still holds a slot and a pod). The name is `h<next_desired_host>`, the slot the lowest unused one.
- **`DrainHost { name, host: Option<(HostId, u64)>, in_ms }`** → `HostDrained::Marked | RowOnly | Unknown`. Sets the row's `state` to draining (`Unknown`: no such row, nothing done); with a `host`, marks that `hosts` row (`drain_at_ms = LEAST(drain_at_ms, now + in_ms)`, only a live dirty row of that boot) → `Marked`, else `RowOnly`. The savepoint is kept in both.
- **`RemoveHost { name }`** → `HostRemoved::Done | Gone`. Deletes the row.
- None needs a mark: each is idempotent to issue twice and to apply twice; a lost answer is repaired by the next read.
- SQLite: a transition with decisions is `Unsupported` already; the read returns no rows there.

### 3. The lock walks (Task 2 writes them into `world-store/src/succession.rs`'s notes)

All three run in a savepoint of the coordinator's transition, so the `coordinator` row is held and none runs beside a declaration, an assignment or another fleet decision.

**`AddHost`:** `coordinator` → `world` (`SELECT next_desired_host … FOR NO KEY UPDATE`) → `desired_hosts` (a plain count and slot read, the `INSERT`, `UPDATE world SET next_desired_host`).
- The declaration's walk is `coordinator` → `world` → `hosts` → `super_regions`: the same prefix.
- A registration holds `world` and never `coordinator` or `desired_hosts`: `AddHost` waits for it under the decision's `lock_timeout`, and it waits for nothing of ours.
- The `INSERT` cannot wait on a conflicting uncommitted row (the Elastic 3 lesson): every writer of `desired_hosts` holds `coordinator`.

**`DrainHost`:** `coordinator` → `desired_hosts` (one row, `UPDATE`) → `hosts` (one row, `UPDATE`).
- It holds a `hosts` row without `world`, exactly as `AssignLand` holds its share lock: the one known safe cycle of the notes (A-S3 — a later decision of the same transition wants `world` while a registration holds `world` and lapses that name's row) applies unchanged: `40P01` or a timeout, the decision's refusal, retried.
- The host's beat and the operator's `worldctl drain` update the same one row and want nothing after it.

**`RemoveHost`:** `coordinator` → `desired_hosts` (one row, `DELETE`).

**The reads** (`desired_hosts` by the ticker, the reconciler, `worldctl`): plain, no locks.

**`prune_hosts` (Decision 6):** one more plain read (`pg_prepared_xacts`) before the `DELETE`; no row lock added.

### 4. What the ticker decides, and when

All of it lives in `crates/game/src/coordinator/fleet.rs`, runs only on the ticker from `on_maintain`, only when fleet dials are set (`VOXEL_FLEET_MAX`; unset = no fleet decision at all: `dev-r3`, role `all`, today's behaviour), at most one decision per `SCALE_DECISION_EVERY_MS`, none while one is unanswered, and after an answer none before a `hosts` read ISSUED after it was applied. Inputs: the latest read's desired rows and `HostSeen` rows (now with `name`), `attached_live()`, `takes_logins`, `leases.known_for`.

Terms: `wanted` / `draining` = the desired rows by state. The **live host of a name** = its live dirty `HostSeen`. **Serving** = attached here or to any live replica, live, `takes_logins`. In order, the first rule that applies:

1. **A draining name with a live host that carries no mark** (the pod restarted, a stop/start, a drain leave that did not commit) → `DrainHost { name, host, in_ms }`.
2. **A wanted name whose live host carries a mark** (the operator's `world-drain`) → `DrainHost { name, host: None, in_ms: 0 }`: the row follows the mark.
3. **A draining name none of whose `hosts` rows is live-and-dirty or `landed`** (pruned rows count as none) → `RemoveHost`. A leaver that kept land (in-flight rows) holds its row until its `Leave` succession moved it.
4. **Below the minimum:** `wanted < min` → `AddHost { max, expect_wanted: wanted }`. Runs under `static` too (min = max = 1); rules 5–7 need `CrashPolicy::Succession`.
5. **Scale up:** every wanted name has a serving host, `wanted < max`, and for `up_after_ms` without a break every read showed the least-loaded serving host's `tick_permille` ≥ `SCALE_UP_LOAD_PERMILLE` → `AddHost`.
6. **Scale down:** `wanted > max(min, 1)`, nothing draining, no succession run open, every wanted name has a serving host known for ≥ `min_lifetime_ms`, and for `down_after_ms` without a break every read showed `Σ tick_permille over serving hosts ≤ (n − 1) · (1000 − SCALE_DOWN_SPARE_PERMILLE)` → `DrainHost` of the victim.
7. **To zero** (only `min == 0`): one wanted name left, its host serving and known for ≥ `min_lifetime_ms`, and for `down_after_ms` without a break every read showed it with 0 players and 0 running regions → `DrainHost`.

- **The victim** (Dawid's default): fewest players, then fewest running regions, then the highest host id (the youngest).
- **`in_ms`:** 0 when the host reports 0 players, else `DRAIN_WARNING_SECS · 1000`.
- A draining host gets no new land and is no gate already (`takes_logins`, from the mark, within one `hosts` read).
- A host that registered and was given nothing reports load 0 and is the first victim once rule 6 holds — "removed again if still unused after the minimum lifetime" (design §6).
- After a scale-down the remaining hosts sit at ≤ 300 ‰ on average, half the scale-up threshold: the two rules cannot chase each other.

### 5. A login when no host takes logins

- `Coordinator::login_gate() -> Gate`: `Here(host)` (today's `gate_host_here`), `Elsewhere` (a serving host is attached to another live replica → `NoGate`, login tries the next link, as today), `Nobody`.
- `Nobody` with fleet dials set → the replica answers `HostToLogin::Starting` and applies `Input::StartWanted` (logged, replayed, any replica; at most once per `WORLD_START_ASK_EVERY_MS`), which issues `AddHost { max, expect_wanted: 0 }`. `Nobody` without dials → `NoGate`, as today.
- The login service: any link `Starting` → `SpawnError::Starting` → 503 `ApiErrorCode::WorldStarting` (it wins over `NoGate` from other links).
- The client: `WorldStarting` on a first join or a rejoin → retry every `REJOIN_RETRY_MS`, status "The world is starting…", give up after `WORLD_START_WAIT_MS`.
- This also ends Elastic 1's known limit "every live host draining → joins 503 for the whole notice": draining rows are not `wanted`, so a host is added (while a slot is free).
- **With zero hosts:** no cut begins (`maintain_cuts` returns on an empty expected set — verified), the calendar runs on (it does not depend on hosts — verified), free land stays free. A last leaver that kept land (in-flight rows) keeps its drained pod until the next host inherits it by the `Leave` succession.

### 6. `prune_hosts` and a staged leave part (handoff §2)

`two_phase::decide_in` sets `clean = 1` in the decision's transaction; the leaver's part is committed afterwards (`FinishCut`). Until then its `in_flight` rows are invisible, so a registration's prune can delete the row and leave rows keyed to an id no read shows. Fix, Postgres only: `register_in` first reads the staged parts (`two_phase::staged_parts`, a statement of its own, BEFORE the prune) and `prune_hosts` skips those host ids.
- Two statements on purpose: `pg_prepared_xacts` is not read under the statement's snapshot. Read first, a part still staged is skipped; a part committed by then has its `in_flight` rows visible to the later `DELETE`'s snapshot.
- A clean row prepares no later part (its process left).

### 7. A survivor that registered during the last committed cut (handoff §2)

Today `try_apply_succession` waits while `journal.base() != order.base_cut`; a survivor that was not in cut K (it registered while K ran) has an older base for good, the run reaches `SUCCESSION_DEADLINE_MS` and the world rolls back. With hosts added during a failure this is the normal case.

Ruling: **a survivor that never took part in `order.base_cut` answers from its whole journal, at the order's base.** The manager records the highest cut whose `BeginCut` this boot handled (`cuts_joined_max`); `base < order.base_cut && cuts_joined_max < Some(order.base_cut)` → apply with every entry, the report's `base_cut` = the order's.
- Why the whole journal is right: a journal is truncated only by a committed cut its host took part in; every participant of K froze its session with this host from K's `BeginCut` to its `CutDone`, so everything this host exchanged with a K participant is after K; what it exchanged with another non-participant is in no cut at all.
- Why `cuts_joined_max` and not just `base < order.base_cut`: a PARTICIPANT of K whose `CutDone` is still on its way also has the older base, and must keep waiting (its journal still holds what K saved).
- **Task 1's implementer verifies the two facts this rests on in the code (truncation only in `PeerJournal::committed` for participants; the freeze) and stops with a report if either does not hold.**

### 8. Tunables (named consts; an env dial where the twin needs one)

| Constant | Default | Dial | Note |
|---|---|---|---|
| `SCALE_UP_LOAD_PERMILLE` | 600 | — | The least-loaded serving host's worst tick, in permille of the period. Assignment already avoids hosts above 800 (`ASSIGN_HEADROOM_MIN_PERMILLE` 200): a host is added before none qualifies. |
| `SCALE_UP_AFTER_MS` | 60 000 | `VOXEL_SCALE_UP_AFTER_MS` | |
| `SCALE_DOWN_SPARE_PERMILLE` | 700 | — | After the removal the others average ≤ 300 ‰. |
| `SCALE_DOWN_AFTER_MS` | 300 000 | `VOXEL_SCALE_DOWN_AFTER_MS` | Also the quiet time before the last host goes. |
| `HOST_MIN_LIFETIME_MS` | 600 000 | `VOXEL_HOST_MIN_LIFETIME_MS` | `const` asserts: > `ASSIGN_MIN_ATTACHED_MS`, > `LAND_IDLE_MS`. |
| `SCALE_DECISION_EVERY_MS` | 10 000 | — | |
| `WORLD_START_ASK_EVERY_MS` | 5 000 | — | Per replica. |
| `WORLD_START_WAIT_MS` | 180 000 | — | Client. Provisional until the cold start is measured (Task 8). |
| `FLEET_PASS_MS` | 2 000 | — | The reconciler's period. |
| fleet bounds | min 1, max from the module | `VOXEL_FLEET_MIN`, `VOXEL_FLEET_MAX` | `VOXEL_FLEET_MAX` unset = no fleet decisions. min > max, or a dial that is 0 or unparsable, refuses the start. |
| ports | — | `VOXEL_FLEET_PORT_BASE` (reconciler) | Module validation: `port_base + 2·max − 1 ≤ 32767`, `port_base ≥ 30000`. |

## To confirm with Dawid (the plan proceeds on these defaults)

1. Explicit ports from a slot pool instead of ports read back (departure 1). On orion the firewall must admit UDP `port_base … port_base + 2·max − 1`.
2. `var.hosts` is removed, hosts are named `h1`, `h2`, … (departures 2, 3). The orion world is re-forged for `0019` anyway.
3. Scale-up by the least-loaded host's tick, thresholds 600 / 700 (Decision 4, 8).
4. The victim and the 0-second drain of a host with no players (Decision 4).
5. Scale to zero is `fleet.min = 0`, default 1 (handoff §11).
6. `WORLD_START_WAIT_MS` 180 s until measured; the measurement is Task 8's first step and needs your go.
7. `crates/panel` must be committed before Task 9 (handoff §11).
8. Load-driven scale-up cannot be exercised on the twin (no connected players): it is traced in review and left as a hand check.

## Global Constraints

- No git worktrees; work on `develop` in this checkout. Dawid and other sessions commit and stage concurrently in the SHARED index.
  - Commit only your own paths: check `git diff --cached --name-only` before every commit and commit with a pathspec.
  - A file holding foreign hunks (`justfile`, `CLAUDE.md`, `TODO.md`, `deploy/modules/world/*.tf`, the env roots' `main.tf`) is staged with a filtered patch: `git diff -U3`, keep your hunks, `git apply --cached --recount`. Never `git add -A`, `git stash`, `--unidiff-zero`.
  - Never run `rustfmt` or `cargo fmt` from the root. When the tree does not build or `just smoke` fails, check `git status` for foreign changes first (an untracked temporary test of another session fails `test-audit`).
- **Conservation beats latency; wait, never guess.** Every entity, item, player and person exists exactly once through any crash, conflict, abort, drain or deletion.
- **Fencing in PostgreSQL is the only safety.** Nothing the reconciler does keeps two writers apart; deleting a pod is always "SIGTERM → leave cut". Never a store write that skips the fence. A cut part writes only rows of its own land.
- **Write-before-send**; a fleet decision is a store decision in the transition: issuing it is committing it.
- **Only the coordinator's decisions write `desired_hosts`.** The reconciler opens the store `OpenMode::ReadOnly`.
- **The coordinator never talks to the Kubernetes API; the reconciler never talks to the coordinator** and reaches only its own cluster's API and the world's database (design §10).
- **Migrations** are immutable and identical in shape in `migrations/pg` and `migrations/sqlite`. This plan adds exactly `0019_desired_hosts`. Worlds are re-forged, never migrated.
- **Lock walks:** every new or changed statement on `hosts`, `world`, `coordinator`, `desired_hosts` has its walk in `world-store/src/succession.rs`'s notes (Decision 3). A prepared transaction is not a waiting backend.
- **Wire enums are append-only.** `LINK_PROTOCOL_VERSION` 10 → 11 once (Task 8, `HostToLogin::Starting`). `COORDINATOR_STATE_VERSION` does not change. Every new core field is classified in `projection!` (all of this plan's are process fields), every new `WorldManager` field in `host_fields!`, every new input as replayed or ticker-only.
- **The core stays deterministic:** `game` reads no clock, no environment, no store; `BTreeMap` / `BTreeSet`; named consts.
- `game`, `server`, `worldgen` stay free of Bevy. rustls uses **ring only**: after adding reqwest to `server`, `cargo tree -i aws-lc-rs` must print nothing. Tokio stays at the edges (the reconciler's HTTP calls run on a current-thread runtime of its own).
- **Ask Dawid first:** client tests, live runs (`dev-r3`, `go-nogo-r3`, `just run`), measurements, anything on orion. `just twin-check` is allowed; the twin needs disk (at 90 % kubelet's image GC deletes the imported images — `TODO.md`, Elastic 3 hand check 5): check `df` before a twin run.
- **Tests (CLAUDE.md, "Testing"):** only regression tests (a bug SEEN in play or in a log, `/// Regression:`) are committed. This plan has no "write the failing test" steps. Each task instead has:
  - **Scenarios**: cases the implementer traces through the code in its report, and the reviewer checks by reading.
  - Temporary tests to drive the change (the seeded rig `crates/game/tests/common/multi_rig.rs`; `just test-pg` for store statements), **deleted before the commit**.
  - `just smoke`; `just web-check` when `game` changes; a twin phase wherever one can exercise the path.
  - No whole crate suites per task. `just test` / `just test-full` once at the close, said first.
- Run every command in the FOREGROUND. Do not run server test binaries while Dawid has `just run` up (the shared `target/debug/server` is re-linked at region 64): ask if unsure.
- **The ledger** is `.superpowers/sdd/2026-11-19-elastic-4-reconciler-scaling/progress.md`. A decision this plan did not make goes there as `Ruling: what — why — cost if wrong`.
- Every review dispatch says: "there are no tests to read; review by tracing each named scenario through the code".
- Subagents edit with Edit / Write. A refused command is reported, never worked around. The justfile is canonical (a changed command updates its recipe in the same change), secrets and `.tfvars` are never printed, dated documents are not edited.

## Review Focus

Cross-task interactions no single task's scenarios cover. The whole-branch review (most capable model, split by area) traces each.

1. **A registration races a leave (Tasks 1, 3, 7).** A host is added while another's drain leave is between its cut's decision and its part's `COMMIT PREPARED`.
   - Expected: the leaver's row is not pruned; its in-flight rows get an heir by the `Leave` succession; `RemoveHost` waits until the row owns nothing.
2. **A host is added during a failure (Tasks 1, 3).** A host dies; a replacement pod (same name, new id) or a new name registers during the cut before the declaration and is a survivor or the heir.
   - Expected: the run completes without a rollback; nothing is reported twice or not at all (Decision 7).
3. **The reconciler against a stop and an update (Tasks 4, 6).** `world-stop` or `world_update.sh` runs while a wanted row has no Deployment yet, or a `RemoveHost` lands mid-stop.
   - Expected: no host is created while the coordinator Deployment is at 0; none starts on an image other than the coordinator's; after `world-start` the missing host exists on the new image; a release still records one previous build.
4. **A ticker change in the middle of a scale decision (Tasks 2, 3, 7).** The ticker dies after `AddHost` committed and before its answer was applied; the next ticker's timers start at zero.
   - Expected: one host is added, not two (`expect_wanted`); a drain already marked is not marked later; a new ticker delays a decision, never hastens one.
5. **Zero hosts and two logins on two replicas (Task 8).** Both replicas answer `Starting` and both issue `AddHost { expect_wanted: 0 }`.
   - Expected: one row; both clients get in within `WORLD_START_WAIT_MS`; the new host is not drained again by rule 7 before `min_lifetime_ms`.
6. **The reconciler reads an empty or unreachable table (Task 4).** A database outage, or a world re-forged under a running reconciler.
   - Expected: a failed read deletes nothing; an EMPTY read deletes every labelled host object (each pod leaves by its leave cut — a disruption, never a loss); this is said in the role's docs.

## File structure

| Path | Responsibility after this plan |
|---|---|
| `crates/world-store/migrations/{pg,sqlite}/0019_desired_hosts.sql` (new) | the table, `world.next_desired_host` |
| `crates/world-store/src/fleet.rs` (new) | `add_host_in`, `drain_host_in`, `remove_host_in`, `desired_hosts` (the read) |
| `crates/world-store/src/{rows,coordinator_tx,handle,thread,succession,ctl,lib}.rs` | the prune skips staged parts; the three arms in `run_one`; the `hosts` read returns the desired rows; the lock notes; `worldctl desired` |
| `crates/game/src/coordinator/fleet.rs` (new) | `DesiredHost`, `FleetDials`, the seven rules, the timers, `fleet_answered`, `login_gate`, `Input::StartWanted` |
| `crates/game/src/coordinator/{mod,decisions,input,succession}.rs` | the decision types; `HostSeen.name`; `HostsRead.desired`; the process fields in `projection!` |
| `crates/game/src/world_manager/{succession,world_cut,mod}.rs` | `cuts_joined_max`; the whole-journal answer (Decision 7) |
| `crates/kube-api/` (new crate) | `Kube` (get, list by label, post, put, delete) over reqwest/ring; copied from `crates/panel/src/kube.rs`, which Task 9 replaces by a dependency |
| `crates/server/src/fleet.rs` (new) | the `fleet` role: the pass, the template, the guards |
| `crates/server/src/{roles,lib,health,coordinator_rt,login_link}.rs` | `Role::Fleet`; the fleet dials; metrics; `Starting`; the window line |
| `crates/login-api/src/lib.rs`, `crates/login/src/{host_link,api}.rs`, `crates/svc-link/src/lib.rs` | `HostToLogin::Starting`, `ApiErrorCode::WorldStarting`, protocol 11 |
| `crates/client/src/login/{flow,api}.rs` | the waiting join |
| `deploy/modules/world/{hosts,fleet,variables,outputs,main}.tf`, `charts/world-monitor`, the env roots | `var.fleet`; the template ConfigMap; the `fleet` Deployment, ServiceAccount, Role; no host Deployments in the state |
| `deploy/modules/monitoring/values/kube-prometheus-stack.yaml.tftpl` | fleet alerts |
| `justfile`, `scripts/world_update.sh`, `scripts/twin_check.sh` | the recipes against a reconciler; the twin's fleet phases |
| `crates/panel/src/{ops,world,kube}.rs` | level with the recipes (Task 9, gated) |
| `CLAUDE.md`, `TODO.md`, this plan's "As built" | docs |

---

### Task 1: Elastic 3's residuals — a chain of stale `departed` pointers; the prune and a staged leave part; a survivor outside the last cut

**Files:**
- Modify: `crates/world-store/src/rows.rs` (`register_in` :543, `prune_hosts` :721), `crates/world-store/src/two_phase.rs` (`staged_parts` :111 reused), `crates/world-store/src/succession.rs` (notes).
- Modify: `crates/game/src/world_manager/succession.rs` (`try_apply_succession` :674, `host_fields!`), `world_cut.rs` (where `BeginCut` is handled), `mod.rs` (the field).
- Modify: `crates/game/src/world_manager/placement.rs` (`departed_denied`, `SentOn`).

**The pointer chain (ruling):** `departed_denied` drops the pointer when this host's own forwarded order comes back from ANY host (same token and epoch, `hops` greater than it left with, the other conditions unchanged), not only from the host the pointer names.
- Why: an order sent on by a pointer follows the player's walk — every host the player left points at the next, and an order travels behind any hand-off on the same ordered stream — until it reaches a host with no pointer. That host holds the player (and places it) or dropped its record after a committed cut wrote it; only then does the order travel by the record's region. So an order that left here by the pointer and returns from anywhere was refused by the end of the walk.
- **The implementer checks this argument against the code before changing the condition** (every `departed.insert` / `remove`, `accept_handoff`, `accept_move`, the succession's re-pointing, an order held in `spawn_held`) and stops with a report if a host can route an order by region while a hand-off of that player is on its way to it from a host that is not the order's sender. The fallback is the handoff's other fix: a new `Reliable` telling peers of the drop (then under protocol 11, journalled or argued out).
- An order may need a second join to get through a chain (four hops); it must not need a third.

**Interfaces:**
- Produces: `prune_hosts(tx, staged: &[HostId])` (SQLite passes `&[]`); `WorldManager.cuts_joined_max: Option<CutId>` (`other` in `host_fields!`: it names no host).

**Scenarios:**
- S1: host L's leave cut K is decided (`clean = 1`), its part carrying two `in_flight` rows is still staged; host N registers. → L's row is kept; after `COMMIT PREPARED` it is kept by the existing `in_flight` check.
- S2: as S1 but the part is committed between the staged read and the `DELETE`. → kept (the `DELETE`'s snapshot sees the `in_flight` rows).
- S3: a clean landless row with no staged part → pruned as today.
- S0a: P walked h1 → h2 → h0, h0 dropped the record at a committed cut, the land is h1's; a login at gate h1, h2 or h0 → placed on h1 by the second join at the latest; both pointers of the chain are gone or harmless.
- S0b: as S0a with the land at h2 → placed on h2 (works today).
- S0c: the two-host cases of "Before Task 1" (the bounce; the hand-off in flight) are unchanged.
- S4: hosts A, B in cut K; N registers during K; A dies after K committed; the order's `base_cut` is K, N's base is K − 1, `cuts_joined_max` is `None` → N applies with its whole journal, reports `base_cut` K; the run settles.
- S5: B was in K, its `CutDone(K)` not arrived yet when the order comes (`cuts_joined_max = Some(K)`, base K − 1) → B waits, applies after `CutDone`.
- S6: N registered during K and took part in K + 1 (aborted) → `cuts_joined_max = Some(K + 1)`, base K − 1, order base K → whole journal (an aborted cut truncated nothing); state why in a comment.

- [ ] **Step 1:** Verify Decision 7's two facts in the code (`PeerJournal::committed`, the freeze in `world_cut.rs`). If either does not hold, stop and report.
- [ ] **Step 2:** Implement the prune change; add its paragraph to the lock notes (Decision 3, Decision 6).
- [ ] **Step 3:** Implement `cuts_joined_max` and the relaxed condition; the log line says "answers from its whole journal (not in cut K)".
- [ ] **Step 4:** The pointer chain: check the argument, then change `departed_denied` (or stop).
- [ ] **Step 5:** Drive S1–S3 with a temporary `just test-pg` test and S0a–S0c, S4–S5 with temporary rig tests; delete them. `just smoke`, `just web-check`.
- [ ] **Step 6:** Commit: `fix(store,host): a registration never prunes a leaver with a staged part; a survivor outside the last cut answers from its whole journal; a chain of stale departed pointers ends`.

### Task 2: `desired_hosts` — the migration, the three store decisions, the read

**Files:**
- Create: `crates/world-store/migrations/{pg,sqlite}/0019_desired_hosts.sql`, `crates/world-store/src/fleet.rs`.
- Modify: `crates/game/src/coordinator/decisions.rs` (`StoreDecision` :57, `DecisionAnswer` :101), `crates/game/src/coordinator/mod.rs` (`answer_awaited` :1381, `on_decision` :1403 — arms that do nothing yet), `crates/world-store/src/coordinator_tx.rs` (`Decided` :116, `refused` :269, `run_one` :283, `answers` :357), `crates/world-store/src/{lib,handle,thread,rows,ctl,succession}.rs`, `crates/server/src/coordinator_rt.rs` (`answered` :2119, the exhaustive arm), `crates/game/tests/common/multi_rig.rs` (`execute_committed` :862, the arms), the schema gate's expected list if one names migrations.

**Interfaces:**
- Produces:

```rust
// game/src/coordinator/fleet.rs (types only in this task)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesiredHost { pub name: String, pub slot: u32, pub draining: bool, pub created_ms: u64 }

// game/src/coordinator/decisions.rs
pub struct AddHost { pub max: u32, pub expect_wanted: u32 }
pub enum HostAdded { Done { name: String, slot: u32 }, NotNeeded, Full }
pub struct DrainHost { pub name: String, pub host: Option<(HostId, u64)>, pub in_ms: u64 }
pub enum HostDrained { Marked, RowOnly, Unknown }
pub struct RemoveHost { pub name: String }
pub enum HostRemoved { Done, Gone }
// StoreDecision gains AddHost(AddHost), DrainHost(DrainHost), RemoveHost(RemoveHost)
// DecisionAnswer gains HostAdded { stamp, add, outcome: Result<HostAdded, String> }, HostDrained { .. }, HostRemoved { .. }

// world-store/src/fleet.rs
pub(crate) async fn add_host_in(tx: &mut sqlx::PgConnection, a: &AddHost) -> Result<HostAdded, StoreError>;
pub(crate) async fn drain_host_in(tx: &mut sqlx::PgConnection, d: &DrainHost) -> Result<HostDrained, StoreError>;
pub(crate) async fn remove_host_in(tx: &mut sqlx::PgConnection, r: &RemoveHost) -> Result<HostRemoved, StoreError>;
// world-store: HostsRead gains `desired: Vec<DesiredHost>` (read in the same request, after the rows; SQLite: empty)
// StoreHandle::desired_hosts(&self) -> Result<Vec<DesiredHost>, StoreError>   (plain read; works on a ReadOnly handle)
// worldctl desired <url>    prints name, slot, state, age
```

- If `StoreDecision` derives `Copy` today, it loses it (names are `String`); fix the call sites.
- `run_one` keeps the savepoint on `Done`, on `Marked` / `RowOnly`, on `HostRemoved::Done`.
- The statements and their order are Decision 3. `created_ms` is the store's clock (`PG_NOW_MS`, as the lease's).
- The rig answers the three from an in-memory table with the same rules.

**Scenarios:**
- S1: empty table, `AddHost { max: 2, expect_wanted: 0 }` twice in two transitions → `Done { h1, slot 0 }`, then `NotNeeded`.
- S2: rows `h1` (wanted, slot 0), `h2` (draining, slot 1), `AddHost { max: 2, expect_wanted: 1 }` → `Full`. After `RemoveHost(h2)`: `Done { h3, slot 1 }` — the slot is reused, the name is not.
- S3: `DrainHost` of a name with a live row → `Marked`, `drain_at_ms` set, never later than an existing mark; with another boot → `RowOnly`; unknown name → `Unknown`, nothing written.
- S4: a transition that conflicts (stale version) carrying `AddHost` → no row, the counter unchanged.
- S5: `AddHost` while a registration holds `world` → waits, then succeeds, or the decision's timeout refusal (retried by the next pass).

- [ ] **Step 1:** The migration (both engines) and `fleet.rs`; the lock walks into `succession.rs`'s notes after `AssignLand`.
- [ ] **Step 2:** The decision types and every exhaustive match; the read; `worldctl desired`.
- [ ] **Step 3:** Drive S1–S5 with a temporary `just test-pg` test; delete it. `just smoke`, `just web-check`.
- [ ] **Step 4:** Commit: `feat(store): desired_hosts and the AddHost / DrainHost / RemoveHost decisions`.

### Task 3: The ticker keeps the fleet at its minimum, follows marks, removes what left

**Files:**
- Modify: `crates/game/src/coordinator/fleet.rs` (the rules), `mod.rs` (`projection!` :371, `on_maintain` :858, `on_decision`), `input.rs` (`HostsRead` gains `desired`), `succession.rs` (`HostSeen` :123 gains `name`; `on_hosts_read` :1623 stores the read's desired rows).
- Modify: `crates/server/src/coordinator_rt.rs` (`feed_cuts` :1794 fills `name` and `desired`; the dials at start; `answered`; `WriteTally` + `report_writes` :2944), `crates/server/src/roles.rs` (the env dials), `crates/server/src/health.rs` (metrics).

**Interfaces:**
- Consumes: Task 2's types and decisions.
- Produces:

```rust
// game/src/coordinator/fleet.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FleetDials { pub min: u32, pub max: u32, pub up_after_ms: u64, pub down_after_ms: u64, pub min_lifetime_ms: u64 }
impl Coordinator {
    pub fn set_fleet(&mut self, dials: Option<FleetDials>);       // None (default): no fleet decision
    pub(super) fn maintain_fleet(&mut self, now_ms: u64);          // rules 1–4 here; 5–7 in Task 7
    pub(super) fn fleet_answered(&mut self, answer: &DecisionAnswer, now_ms: u64);
}
// process fields (projection!): fleet_dials (kept), fleet_desired: Option<Vec<DesiredHost>> (kept; None until a read),
// fleet_pending: bool, fleet_last_ms, fleet_read_after: Option<u64> (kept)
```

- Env (roles.rs, read by the coordinator role only): `VOXEL_FLEET_MAX` (unset → `set_fleet(None)`), `VOXEL_FLEET_MIN` (default 1), the three pace dials of Decision 8. Refusals as Decision 8 says.
- Metrics (role `coordinator`): gauges `mmo_fleet_wanted`, `mmo_fleet_draining`; counters `mmo_fleet_added_total`, `mmo_fleet_drained_total`, `mmo_fleet_removed_total`. The window line gains `added= drained= removed=`.
- No rule runs before the first read (`fleet_desired` is `None`), on a replica that is not the ticker, or while `fleet_pending`.

**Scenarios:**
- S1: a fresh `claim` world, min 1: the first pass issues `AddHost { expect_wanted: 0 }`; until a read issued after the answer shows `h1`, no second one.
- S2: the ticker dies between the commit and the answer; the new ticker reads `h1` wanted → nothing.
- S3: the operator marks `h1`'s host (`worldctl drain`) → rule 2 sets the row draining; with min 1, rule 4 adds `h2` in a later pass; `h1`'s host leaves; all its rows clean and landless → `RemoveHost(h1)`.
- S4: `h1` draining, its pod restarts (new id, no mark) → rule 1 marks the new id with `in_ms` 0 (0 players).
- S5: `h1` draining, its leaver kept land (in-flight rows) → no `RemoveHost` until the `Leave` succession moved the land and the row reads landless.
- S6: `static`, min = max = 1 → rule 4 only.
- S7: a rebase of the ticker with `Maintain` in its log → the replayed `Maintain` issues the same decision at most once (`fleet_pending` is a kept process field: say in `projection!` why).

- [ ] **Step 1:** Implement; classify the fields and `Input::HostsRead`'s new part (never logged, as the rest of it).
- [ ] **Step 2:** Drive S1–S5 with a temporary rig test (the rig's `execute_committed` from Task 2); delete it. `just smoke`, `just web-check`.
- [ ] **Step 3:** Commit: `feat(coordinator): the ticker keeps the fleet at its minimum, follows drain marks and removes hosts that left`.

### Task 4: The `fleet` role — the reconciler

**Files:**
- Create: `crates/kube-api/{Cargo.toml,src/lib.rs}` (workspace member), `crates/server/src/fleet.rs`.
- Modify: `crates/server/Cargo.toml` (kube-api, reqwest via it), `crates/server/src/roles.rs` (`Role::Fleet`, `from_value` :61, `check_store`, `run_fleet`), `crates/server/src/lib.rs` (the early branch beside :714), `crates/server/src/health.rs` (the role label doc, `set_waiting` values, the metrics), `justfile` (`fleet` recipe for a run from the checkout through `kubectl proxy`).

**Interfaces:**
- Consumes: `StoreHandle::desired_hosts` (Task 2).
- Produces:

```rust
// kube-api
pub enum KubeError { NotFound, Conflict, Forbidden(String), Status(u16, String), Transport(String) }
impl Kube {
    pub fn from_env(api_env: &str) -> Result<Kube, String>;   // the env var naming a kubectl-proxy base, else in-cluster
    pub async fn get(&self, path: &str) -> Result<Value, KubeError>;
    pub async fn list(&self, path: &str, label_selector: &str) -> Result<Vec<Value>, KubeError>;
    pub async fn post(&self, path: &str, body: &Value) -> Result<Value, KubeError>;   // 409 → Conflict
    pub async fn put(&self, path: &str, body: &Value) -> Result<Value, KubeError>;
    pub async fn delete(&self, path: &str) -> Result<(), KubeError>;                  // 404 → Ok
}
// server/src/fleet.rs
pub fn plan_pass(desired: &[DesiredHost], objects: &Objects, coordinator: &CoordinatorSeen, template: &Template) -> Vec<Action>;  // pure
pub enum Action { Delete { kind: Kind, name: String }, Create { kind: Kind, name: String, body: Value }, Replace { name: String, body: Value } }
```

- Env: `VOXEL_ROLE=fleet`, `VOXEL_WORLD_DB`, `VOXEL_WORLD_ID`, `VOXEL_FLEET_NAMESPACE`, `VOXEL_FLEET_PORT_BASE`, `VOXEL_FLEET_KUBE_API` (optional, a `kubectl proxy` base), `VOXEL_HEALTH_BIND`. Postgres only (`check_store`). The store handle is `OpenMode::ReadOnly`; while the store is not forged or not at the newest schema the role waits (`/readyz` 503 `forge`), exits 0 on SIGTERM.
- One pass every `FLEET_PASS_MS`:
  1. Read the desired rows. **A failed read ends the pass: nothing is created or deleted.**
  2. Read the `coordinator` Deployment (replicas, the `coordinator` container's image) and the ConfigMap `host-template` (key `template.json`; its sha256 is the template hash).
  3. List Deployments, Services and PDBs labelled `errantry/fleet=<world id>`.
  4. `plan_pass`: every labelled object whose `errantry/host` is in no row → `Delete` (Deployment, PDB, Service, in that order). Then, **only when the coordinator's `spec.replicas` > 0**: per row, in slot order, whatever is missing → `Create` Service, PDB, Deployment (replicas 1, the coordinator's image, annotation `errantry/template=<hash>`); a Deployment whose annotation differs **and whose `spec.replicas` is 0** → `Replace` (the new template, keeping replicas 0 and the image it has).
  5. Apply the actions in order; a failure is logged (at most one line per `10 s` per object), counted, and retried by the next pass. `Conflict` on a create is success.
- It never patches replicas or the image of an existing Deployment, and never touches an object without its label.
- `/readyz`: 200 when the last pass completed without a failed action within `3 × FLEET_PASS_MS`; 503 `stopped` while the coordinator is at 0; `/livez` from the loop beat.
- Metrics (role `fleet`): gauges `mmo_fleet_desired`, `mmo_fleet_deployments`; counters `mmo_fleet_created_total`, `mmo_fleet_deleted_total`, `mmo_fleet_errors_total`; gauge `mmo_fleet_pass_age_ms`.
- The role's module doc states Review Focus 6 (an empty read deletes every host object; a failed one deletes nothing).

**Scenarios** (traced through `plan_pass`, which is pure):
- S1: rows `h1`; no objects; coordinator at 2 → Service (ports `base`, `base + 1`), PDB, Deployment.
- S2: the pass died after the Service → the next pass creates the PDB and the Deployment only.
- S3: no row; objects of `h1` → three deletes.
- S4: rows `h3` at slot 1 while `h2`'s Service (slot 1) still exists and `h2` has no row → the delete comes first; if the create still fails (the port is not free yet) the next pass creates it.
- S5: coordinator at 0, a row without objects → nothing created; a labelled stray is still deleted.
- S6: a Deployment at replicas 0 with an old template hash, coordinator at 2 → `Replace`; the same at replicas 1 → nothing.
- S7: the API answers 500 / is unreachable → no action applied, `mmo_fleet_errors_total` grows, not ready after three periods; everything running keeps running.

- [ ] **Step 1:** `crates/kube-api`: copy `crates/panel/src/kube.rs`'s client (do not edit `crates/panel` — it is untracked and Dawid's), add `list`, `put`, `delete`, `Conflict`. `cargo tree -i aws-lc-rs` prints nothing.
- [ ] **Step 2:** `Role::Fleet`, `run_fleet`, `fleet.rs`; the pass's HTTP on a current-thread runtime.
- [ ] **Step 3:** Drive S1–S6 with a temporary unit test of `plan_pass`; delete it. `just smoke`.
- [ ] **Step 4:** Commit: `feat(server): the fleet role makes Kubernetes match desired_hosts`.

### Task 5: The module — the reconciler's Deployment and Role; host Deployments leave the OpenTofu state

**Files:**
- Create: `deploy/modules/world/fleet.tf`.
- Modify: `deploy/modules/world/{hosts,variables,outputs,main,coordinator}.tf`, `charts/world-monitor/templates/servicemonitor.yaml`, `deploy/envs/local/world/main.tf`, `deploy/envs/orion/world/main.tf`.

**Interfaces:**
- Produces (what Tasks 6 and 8 rely on): Deployment `fleet` (container `fleet`, image `var.images.server`, `ignore_changes` on the image, one replica, `Recreate`, label `app.kubernetes.io/component=fleet`); ConfigMap `host-template` (key `template.json`); host objects named `host-<name>` with the labels they have today plus `errantry/fleet=<world id>`; `var.fleet`.

```hcl
variable "fleet" {
  type = object({
    min = optional(number, 1), max = number, port_base = number,
    scale_up_after_ms = optional(number), scale_down_after_ms = optional(number), host_min_lifetime_ms = optional(number),
  })
}
```

- Validations: whole numbers; `0 ≤ min ≤ max`, `max ≥ 1`; `port_base ≥ 30000`, `port_base + 2·max − 1 ≤ 32767`; `ownership != "static" || (min == 1 && max == 1)`.
- `hosts.tf`: the three `for_each` resources become ONE `jsonencode`d template (a list of the three manifests) in the ConfigMap, with the same pod spec, env, mounts, probes, resources, labels and PDB as today; `__NAME__`, `__QUIC_PORT__`, `__WT_PORT__`, `__IMAGE__` where `each.key`, the node ports and the image stood. The headless Service `hosts` stays.
- `fleet.tf`: ServiceAccount `fleet` (token mounted only in this pod); a namespaced **Role** (no ClusterRole): `apps` deployments get, list, create, update, delete; `""` services get, list, create, delete; `policy` poddisruptionbudgets get, list, create, delete; `""` configmaps get with `resourceNames = ["host-template"]`; RoleBinding; the Deployment with the env of Task 4 (`VOXEL_FLEET_NAMESPACE` from the downward API) and the world's database secret; a headless Service `fleet` with the `health` port; the ServiceMonitor's component list gains `fleet`.
- `coordinator.tf`: `VOXEL_FLEET_MIN`, `VOXEL_FLEET_MAX` and the three optional pace dials.
- `outputs.tf`: `endpoints` and `host_names` go (find their consumers with `grep -rn "module.world\.\(endpoints\|host_names\)" deploy/` and adapt them); a `client_ports` output gives the UDP range.
- Roots: local `fleet = { min = 2, max = 2, port_base = 30466, scale_down_after_ms = 30000, host_min_lifetime_ms = 60000, scale_up_after_ms = 30000 }` (k3d maps exactly these four ports; `land_idle_ms` stays 20000); orion `fleet = { min = 1, max = 2, port_base = 30466 }`.

**Scenarios:**
- S1: `tofu validate` and a `plan` of the local root: the host Deployments, Services and PDBs are to be destroyed, `fleet`, its Role and the ConfigMap created; no secret value appears in the ConfigMap (the template references Secrets by name, as the Deployment did).
- S2: the rendered template, with the placeholders substituted for `a` / 30466 / 30467 / the server image, equals the Deployment, Service and PDB the old `hosts.tf` produced, except the new label and annotation (compare `kubectl get -o json` of an old twin, or the old plan's JSON).
- S3: an apply on a running world changes no host (the reconciler re-templates only at replicas 0).

- [ ] **Step 1:** Implement; `tofu fmt` on the touched files only; `tofu validate` in both roots (through the justfile's recipes).
- [ ] **Step 2:** Trace S2 in the report (the diff of the two JSONs).
- [ ] **Step 3:** Commit: `feat(deploy): the world module runs the fleet reconciler; hosts come from desired_hosts, not var.hosts`.

### Task 6: The recipes, `world_update.sh` and the twin against a reconciler

**Files:**
- Modify: `justfile` (`world-status`, `world-stop`, `world-start`, `world-drain`, `world-hosts`, the header comments that say "remove its key from `var.hosts`"), `scripts/world_update.sh`, `scripts/twin_check.sh`.

**Interfaces:**
- Consumes: Deployment `fleet` / container `fleet`, the labels, `worldctl desired` (Tasks 2, 4, 5).

- `world-status`: adds the desired rows (`worldctl desired`), the `fleet` pod's `/readyz` and its counters.
- `world-stop`: unchanged steps; **zero host Deployments is no longer an error** (a world at zero). The reconciler is left running (it creates nothing while the coordinator is at 0).
- `world-start`: coordinator up and every replica Ready → wait for `fleet`'s `/readyz` 200 (it re-templates stopped Deployments and creates missing ones) → scale every host Deployment that is at 0 to 1 → poll each as today, skipping drained ones.
- `world-drain <name>`: unchanged; its closing text says the coordinator removes the host and the reconciler deletes its Deployment (no apply).
- `world_update.sh`: `fleet` joins the preflight's container names and Step 3's `set image` (`deploy/fleet fleet=$server_ref`), and the "one build" check of Step 1; zero host Deployments is allowed (`hostdeps` empty: skip the host steps); the leave lines and `release-history` are unchanged.
- `twin_check.sh`: host names are discovered (`k get deploy -l app.kubernetes.io/component=host`), never `a` / `b`. The precondition becomes "the `fleet` Deployment exists". Changed and new phases (the header lists them):
  - 2c (new): after the forge, **two hosts appear by themselves** (`h1`, `h2`), each on the coordinator's image, node ports 30466–30469; `mmo_fleet_added_total` 0 → 2 summed over the replicas.
  - 5, 6: as today on the discovered names; the restarted pod keeps its name and gets a new id.
  - 9: `world-drain <second host> 5` → drained → its desired row turns draining → **its row and its Deployment, Service and PDB disappear by themselves**; a third name appears (min 2) on the freed slot; no `rollout restart`.
  - 10: `world-stop` / `world-start` as today; additionally a stray check: a hand-made labelled Deployment `host-zz` (replicas 0) is deleted within three passes.
  - 10b (new): the `fleet` pod deleted mid-play → nothing running changes; it returns Ready.

**Scenarios:**
- S1: `world-stop` while a wanted row has no Deployment yet → after the stop no host pod exists; after `world-start` the host exists on the coordinator's image.
- S2: `world_update.sh` on a world with hosts `h1`, `h3` → all four workloads and both hosts on one build; `release-history` records the previous one; a re-run with the same refs is a no-op.
- S3: `world_update.sh --rollback` → the same, back.
- S4: `world-stop` on a world at zero hosts → stops the coordinator, no error.

- [ ] **Step 1:** Implement the recipes and the script.
- [ ] **Step 2:** Rewrite the twin phases; `bash -n scripts/twin_check.sh`.
- [ ] **Step 3:** Check `df` (below 85 % used), then run `just twin-down; just twin-up; just twin-images; just twin-apply; just twin-check` in the foreground. Expected: every phase PASS. This is the first run of Tasks 2–6 as processes: a failure is fixed in the task that owns the code, and noted in the ledger.
- [ ] **Step 4:** Commit: `feat(world): the recipes, the release script and the twin run against the fleet reconciler`.

### Task 7: Scaling by load — add under load, drain when it fits in fewer

**Files:**
- Modify: `crates/game/src/coordinator/fleet.rs` (rules 5–6, the timers), `mod.rs` (`projection!`), `crates/server/src/coordinator_rt.rs` (the window line's `scale=` reason).

**Interfaces:**
- Consumes: Task 3's `maintain_fleet`, `FleetDials`.
- Produces: process fields `fleet_up_since: Option<u64>`, `fleet_down_since: Option<u64>` (kept; a new ticker starts both at `None`). Consts `SCALE_UP_LOAD_PERMILLE`, `SCALE_DOWN_SPARE_PERMILLE`, `SCALE_UP_AFTER_MS`, `SCALE_DOWN_AFTER_MS`, `HOST_MIN_LIFETIME_MS`, `SCALE_DECISION_EVERY_MS` with the `const` asserts of Decision 8.
- The timers move only on a NEW `hosts` read (a read is the evidence), are cleared by any read that breaks the condition, and by any fleet decision.
- Each decision logs one `[coordinator]` line with its evidence (the loads, the victim and why).

**Scenarios:**
- S1: two serving hosts at 700 and 650 ‰ for 60 s, max 3 → `AddHost`. One read at 500 in between → the count starts again.
- S2: one host at 1000 (a crowded super-region), one at 50 → no add (the least-loaded is low), no drain (1050 > 300).
- S3: three hosts at 100, 80, 0 ‰; players 4, 2, 0; all older than the minimum lifetime; 300 s → `DrainHost` of the third, `in_ms` 0. With players 4, 2, 1 → the third, `in_ms` = `DRAIN_WARNING_SECS · 1000`.
- S4: a draining row exists, or a succession run is open, or a wanted name has no serving host (its pod is starting or crash-looping) → no scale-up, no scale-down.
- S5: `wanted == min` → never drained by rule 6. Two hosts, min 1, the fit holds, players on both → the one with fewer players drains; the other is never the last serving host left with players unless it stays.
- S6: the ticker changes during the 300 s → the new ticker counts from zero (delayed, never hastened).
- S7: after a scale-down, the scale-up condition cannot hold at once: `(n − 1) · 300 ‰` spread over `n − 1` hosts is ≤ 300 < 600 per host.

- [ ] **Step 1:** Implement.
- [ ] **Step 2:** Drive S1–S5 with a temporary rig or core test (reads with chosen loads); delete it. `just smoke`, `just web-check`.
- [ ] **Step 3:** Commit: `feat(coordinator): a host is added under load and one drains when the load fits in fewer`.

### Task 8: Scale to zero, and the join that waits for the first host

**Files:**
- Modify: `crates/game/src/coordinator/fleet.rs` (rule 7, `login_gate`, `Input::StartWanted`), `input.rs`, `mod.rs` (`login_order` :1176).
- Modify: `crates/login-api/src/lib.rs` (`HostToLogin::Starting` + its `Debug` arm, `ApiErrorCode::WorldStarting`), `crates/svc-link/src/lib.rs` (`LINK_PROTOCOL_VERSION` 11 + the history line), `crates/server/src/{coordinator_rt,login_link}.rs` (`drain_login` :1698, `starting(&msg)` beside `no_gate` :186), `crates/login/src/{host_link,api}.rs` (`SpawnError::Starting`, `Fail::world_starting()`, the join metric's outcome).
- Modify: `crates/client/src/login/{flow,api}.rs` (the retry on a first join and a rejoin, `WORLD_START_WAIT_MS`, the status text, the `Display` arm).
- Modify: `scripts/twin_check.sh` (phases 12–13), `deploy/envs/local/world/main.tf` only if a dial is missing.

**Interfaces:**
- Produces:

```rust
// game
pub enum Gate { Here(HostId), Elsewhere, Nobody }
impl Coordinator { pub fn login_gate(&self) -> Gate; }
// Input::StartWanted — logged, replayed, any replica; issues AddHost { max, expect_wanted: 0 } at most once per WORLD_START_ASK_EVERY_MS
// login-api
HostToLogin::Starting { player_token: String, session_epoch: u64 }
ApiErrorCode::WorldStarting      // 503, message "the world is starting; this can take a minute"
// client
pub const WORLD_START_WAIT_MS: u64 = 180_000;
pub const WORLD_STARTING_STATUS: &str = "The world is starting…";
```

- `Nobody` = no attached live host that takes logins on ANY live replica. With dials: `Starting` + `StartWanted`; without: `NoGate`.
- Login: a link that answers `Starting` ends the spawn at once with `SpawnError::Starting` (no further link is tried: the world has no gate anywhere).
- Client: on `WorldStarting` the join is re-issued every `REJOIN_RETRY_MS` with the kept refresh token (re-read before every call, as a rejoin), the status line shows `WORLD_STARTING_STATUS`, the error line stays empty; after `WORLD_START_WAIT_MS` the ordinary error. A `WorldUnavailable` on a first join still shows its error at once, as today.
- Rule 7 (Decision 4) needs `min == 0`. A host added for a login is not drained before `min_lifetime_ms`.
- Twin phases (the twin's module has min 2; the phases change the dial at run time: `kubectl set env deploy/coordinator VOXEL_FLEET_MIN=0` restarts every replica together, as phase 7b does):
  - 12 (new): with min 0 and nobody online, land released (20 s), after `host_min_lifetime_ms` + `scale_down_after_ms` the hosts drain ONE AT A TIME and are removed; no host Deployment, no `hosts` row live; the coordinator replicas stay Ready; `successions` unchanged, the generation unchanged.
  - 13 (new): a join (the twin's own login service) answers 503 `world_starting`; a desired row and a pod appear; a join repeated every second succeeds; the seconds from the first 503 to the first 200 are RECORDED (the cold start, images already on the node). A second join meanwhile adds no second host.

**Scenarios:**
- S1: two replicas, zero hosts, two logins, one per replica → both `Starting`; one desired row (`expect_wanted: 0`); both joins succeed after the host attached.
- S2: one serving host attached to replica 2, a login at replica 1 → `NoGate`, login tries replica 2, as today; no `AddHost`.
- S3: every live host draining (the operator marked the only one), max 2 → `Starting`, a second host is added, the join succeeds on it; with max 1 → `Full`, the join keeps answering `world_starting` until the drained host's row is removed, then a host is added.
- S4: fleet dials unset (`dev-r3`) → `NoGate` and 503 `world_unavailable`, exactly as today.
- S5: the last host is told to drain while a login for a parked player arrives → the player is placed (the host still owns the land) and is told `HostDraining`, or the join answers `world_starting` and succeeds on the next host; never both, never neither.
- S6: the last leaver kept land (in-flight rows) → its drained pod and row stay; the first login adds a host, the `Leave` succession gives it the land, then `RemoveHost`.
- S7: an old client or login service → refused at the link hello (protocol 11), as every bump.

- [ ] **Step 1:** Ask Dawid whether to measure the cold start on the twin now (phase 13's number) or proceed on 180 s. Record the answer in the ledger.
- [ ] **Step 2:** Implement the core, the link, the login service.
- [ ] **Step 3:** Implement the client. `just smoke` type-checks it; **ask Dawid before running any client test**.
- [ ] **Step 4:** Add phases 12–13; check `df`; `just twin-images; just twin-apply; just twin-check` in the foreground. Expected: every phase PASS; note phase 13's seconds in the ledger.
- [ ] **Step 5:** Commit: `feat(world): a quiet world goes to zero hosts; a login starts one and the join waits`.

### Task 9: The world panel level with the recipes (gated)

**Gate:** `git ls-files crates/panel | head -1` prints a path. If `crates/panel` is still untracked, **stop**: ask Dawid to commit it; do not stage it for him. Task 10 does not depend on this task.

**Files:**
- Modify: `crates/panel/src/{ops,world,kube}.rs`, `crates/panel/Cargo.toml`, `deploy/modules/panel/main.tf` (the ClusterRole).

- `kube.rs` becomes a re-export of `crates/kube-api` (one client in the tree).
- `run_start` / `run_stop` mirror `world-start` / `world-stop` of Task 6 step for step: hosts are the Deployments labelled `app.kubernetes.io/component=host` (not a StatefulSet, not the ConfigMap `host-endpoints`); the coordinator's count is recorded in / restored from the annotation `errantry/replicas`; start waits for every replica's `/readyz`, then for `fleet`'s, then scales the stopped host Deployments; zero hosts is not an error.
- The world page shows the desired rows and the `fleet` pod.
- The ClusterRole `world-panel` gains `apps` deployments `patch` (the annotation) — nothing else.

**Scenarios:** Task 6's S1 and S4, run through the panel's buttons on the twin (`just twin-panel`).

- [ ] **Step 1:** Implement; `just smoke`; `just twin-images && just twin-apply`; stop and start the twin world from the panel.
- [ ] **Step 2:** Commit: `feat(panel): start and stop follow the recipes — hosts by Deployment, the fleet reconciler awaited`.

### Task 10: Alerts, documentation, closing

**Files:**
- Modify: `deploy/modules/monitoring/values/kube-prometheus-stack.yaml.tftpl`, `CLAUDE.md`, `TODO.md`, this plan ("As built").

- Alerts (durable changes belong in the module; nothing in Grafana's UI):
  - `WorldFleetDown`: no `fleet` pod Ready for 10 min (warning: nothing breaks, nothing scales).
  - `WorldFleetBehind`: `mmo_fleet_desired != mmo_fleet_deployments` for 10 min, or `mmo_fleet_errors_total` rising for 10 min.
  - `WorldFleetFull`: `mmo_fleet_wanted + mmo_fleet_draining == max` and the scale-up condition held (a counter `mmo_fleet_full_total` from the `Full` answer) — capacity, for the owner.
  - `WorldHostsBelowReplicas`: its text no longer names `var.hosts`.
- `CLAUDE.md`: "Deployment" (the world module, `var.fleet`, the reconciler's rules, the recipes, the image rule, what the twin proves and does not), "Many hosts" (the fleet rules, `login_gate`, protocol 11, the env dials), the role list (`VOXEL_ROLE=all|host|coordinator|fleet`).
- `TODO.md`: an Elastic 4 block (hand checks: a release on orion with the reconciler; load-driven scale-up with real players; the cold start on orion with an image pull; the firewall's port range), the known limits below; the Elastic 1 items it closes (`:latest`, "every live host draining → 503"); the Elastic 3 residuals Task 1 closed.

- [ ] **Step 1:** The alerts; `just orion-plan monitoring` is Dawid's to run — say so, do not run it.
- [ ] **Step 2:** The docs.
- [ ] **Step 3:** Say first, then run `just test` and `just test-full` (foreground); report PASS/FAIL per stage against the known reds.
- [ ] **Step 4:** The whole-branch review, split by area (store + lock walks; core rules; reconciler + module; recipes + twin; login + client), each tracing the Review Focus items; one fix wave; one scoped re-review. Then "As built".
- [ ] **Step 5:** Commit: `docs: Elastic 4 — the reconciler and scaling, as built`.

## Not in this plan

- Moving occupied land, re-keying `in_flight` from host to region (E5). An occupied host is removed only by the warned drain; its players rejoin.
- Several nodes, a public address per node, a load balancer per host.
- One coordinator deployment for all worlds; creating or deleting worlds from the panel (R5b-2).
- Rebalancing between live hosts.
- An operator's write to `desired_hosts` (a "pin this many hosts" command): the bounds in the module are the operator's lever.

## Known limits (to carry into `TODO.md`)

- A template change reaches a running host only at the next stop/start (the reconciler re-templates at replicas 0); a change of the Service's or the PDB's shape needs the object deleted by hand.
- With the reconciler down, a drained host's pod stays, a wanted host does not appear, a login for a world at zero waits out `WORLD_START_WAIT_MS` and fails.
- A last leaver that kept land holds its drained pod until the next host inherits it.
- Scale-up needs a free slot: with every slot held (draining hosts included) `AddHost` answers `Full`.
- `tick_permille` is the worst tick since the last beat: one slow tick per beat on every host for a minute reads as load.
- A new ticker forgets the scale timers (a decision is delayed by up to `up_after_ms` / `down_after_ms`).
- Load-driven scale-up is not exercised by the twin.

## Self-review notes

- Handoff §4 (delivers): add/remove by itself → Tasks 2–7; a draining host gets no new land, deleted only when it holds nothing → Decision 4 rules 1–3, Task 3; scale to zero and the waiting join → Task 8.
- Handoff "must settle": port writer → departure 1; name ↔ `hosts` row → departure 3; `var.hosts` → departure 2; the image → departure 7; the recipes and the panel → departure 8, Tasks 6, 9; bounds → Decision 8, Task 5; what load is → departure 10; where the wait lives → departure 11; cuts and the calendar at zero → Decision 5; the prune → Decision 6; the survivor → Decision 7.
- Design §6 rows: reconciler down / API down → Task 4 S7, known limits; "new host registers, nothing to give it" → Decision 4; "login for a world with zero hosts" → Task 8.
- Types: `DesiredHost`, `AddHost { max, expect_wanted }`, `DrainHost { name, host, in_ms }`, `RemoveHost { name }`, `FleetDials`, `Gate` are used with these shapes in Tasks 2, 3, 7, 8.
- No task commits a test; every task names its scenarios; Tasks 6 and 8 run the twin.

## As built (2026-11-19)

Executed subagent-driven on `develop` in one day: one implementer and one reviewer per task, a whole-branch review in three areas on the most capable model, one fix wave, one scoped re-review. **Task 9 (the world panel) was NOT done:** its gate stayed closed (`crates/panel` is still untracked). Everything else is built and reviewed. Nothing was applied to orion.

### Commits (base `bd49b08c`)

- `2520c3ee` Elastic 3's two open findings (before Task 1); `5f29b60f` this plan.
- Task 1: `b27b2e47`, `f117d1ca`.
- Task 2: `ee28414c`.
- Task 3: `abdd5a68`, `0d390b7c` (takes back another session's hunks swept into the first), `6474251f` (rig).
- Task 4: `d41afe1f`.
- Task 5: `a02bc0a8`.
- Task 6: `ab57b1b5`, `a37e1bb9` (the twin's image-GC thresholds, asked for by Dawid).
- Task 7: `230b6093`.
- Task 8: `4c2bf915`, `f3a3d97c`, `2c8cb7ac`.
- Task 10: `e492ec7b`, `28248390`, `a4f82f0b`.
- Fix wave: `7c9d0e84`, `45e3675c`, `5a08a8dd`, `6c3c9e93`, `4eb43c43`.

### Where the build departs from this plan's text (each a ruling in the ledger; the code and `CLAUDE.md` are right, the text above is not)

1. **Decision 4, rule 3:** a draining name is removed once none of its `hosts` rows is live-and-dirty. `landed` no longer holds the row (final review A-I1): a leaver that kept land held its slot for ever, so no heir could be added. The drained pod takes no part in its `Leave` succession.
2. **Decision 4, a rule 2b was added:** a wanted name with no live host whose newest row is clean AND marked → `DrainHost { name, None, 0 }`. An operator's drain that finished before the ticker looked matched no rule.
3. **Decision 8:** `min 0` needs `max ≥ 2` (refused at the coordinator's start and by the module). With rule 3 as built this may be liftable; it was kept.
4. **Departure 8 and Task 6:** the recipes DO pause the reconciler. `world-stop` and `world_update.sh` scale `deploy/fleet` to 0 first; `world-start` restores it after the coordinator is Ready. The reconciler's own guard does not cover the window between scaling the hosts and the coordinator to 0. A start or release waits for the reconciler for a bounded time (300 s), then warns and goes on (final review B-I1).
5. **Task 3:** `fleet_pending` is a RESET process field (kept, a decision dropped by a rebase was never issued again). Task 8's `start_asked_ms` is kept and cleared only when a decision can be lost.
6. **Task 4:** the reconciler creates nothing for a DRAINING row; it reads the desired rows LAST in a pass (after the object lists); a re-template is a PUT carrying the `resourceVersion` it read.
7. **Task 7:** a failed read clears the scale timers; the scale gates are part of the timed condition; rule 5 also needs no draining row and no open run.
8. **Task 8:** inside a first join's wait a `world_unavailable` is retried until the same bound (final review C-I3); as the first answer it still shows at once.
9. **Task 10:** there is no `WorldFleetFull`. Rule 5 never asks at the maximum, so `Full` cannot mean "at max under load". The alerts are `WorldFleetDown`, `WorldFleetBehind`, `WorldFleetSaturated` (the least-loaded host ≥ 600 ‰ for 15 min), `WorldFleetNoFreeSlot`, `WorldFleetHostNotLive`; `WorldNoCommittedCut` is silent at zero hosts.
10. **Task 1:** `departed_denied` also matches the order's `origin`; `SPAWN_RELAY_MAX_HOPS` is 32 (a chain of three or more stale pointers never passed four hops); Decision 7's condition is `cuts_joined_max != Some(K)` plus `PeerJournal::undecided_through`, not `<`.
11. **Global Constraints:** there is no `just test-pg`; Postgres store checks ran as temporary in-crate tests against the dev Postgres.
12. Under `static` with fleet dials set the coordinator reads `hosts`, so its login gate follows a drain mark a static host itself ignores.

### Plan defects found during execution

Five rules of this plan were wrong and were found by reviews: the stop window (4 above), the finished operator drain (2), `min 0 / max 1` (3), the alert on a counter that cannot move (9), the leaver that kept land (1). Four of the five are states a world could reach and never leave.

### Verification

- **Per task:** scenarios driven by temporary tests (seeded rig, in-crate Postgres), deleted before each commit; every task review passed, four after one fix round (Tasks 1, 8, 10; the final wave).
- **Twin run 2** (`f3a3d97c`, before the fix wave): `twin-check` **PASS in 932 s**, every phase, transcript `target/twin/check-20261119T182013Z/`. Two hosts appear by themselves; an operator drain → the row and the objects go → `h3` on the freed slot; `MIN=0`: `h3` drains at 62 s, `h1` at 102 s, zero hosts at 114 s, generation 0, no succession; the first join answers 503 `world_starting`, `h4` is wanted in the same second, the first 200 comes after 4 s (images already on the node: not a cold pull).
- **Twin run 3** (`4eb43c43`, after the fix wave): `twin-check` **PASS in 909 s**, every phase, on a fresh cluster with images rebuilt from the fixed code, transcript `target/twin/check-20261119T192821Z/`. `MIN=0`: `h1` drains at 60 s, `h3` at 102 s, zero hosts at 113 s; the first join answers `world_starting`, `h4` is wanted 1 s later, the first 200 comes after 4 s.
- **`just test-full`** at `28248390`: PASS test-audit, physics-next-upstream, slotmapd, takeover, quest, quest-wasm, site-test; FAIL workspace (`scene_hash_native`, `scene_hash_golden::canonical_scene_state_hash`, two `tree_fall` tests) and determinism-wasm. The failing tests pin sim state this plan did not touch; another session had uncommitted edits in `movement.rs`, `trees.rs`, `state.rs`, `mining.rs` and a chunk-hashing commit landed that day. Not confirmed on a clean tree.
- **Whole-branch review:** 0 Critical, 6 Important (all fixed in the wave and confirmed by the re-review), 29 Minor.
- **Not exercised anywhere:** a connected player; load-driven scale-up or scale-down with real load; a host dying while it owns land during a scale event; two logins on two replicas at zero hosts; the client's waiting join in a real client or the browser (type-checked only, native and wasm); the release script and `world-start`'s bounded wait against a failing reconciler; anything on orion; the first-apply runbook.

### Left open

- **Task 9:** `crates/panel/src/ops.rs` is not level with the recipes (it predates per-host Deployments, and lacks the `fleet` pause and the bounded wait). Gated on `crates/panel` being committed.
- **Stale comments** (the wave was told not to touch the module): `deploy/modules/world/variables.tf` gives the old reason for "`min 0` needs `max ≥ 2`"; `fleet.tf`'s header on overlapping reconcilers, "nothing waits on its readiness" and the Role's reach; `scripts/world_update.sh`'s header and `world-stop`'s leave summary line.
- **Minors from the re-review:** with a drained leaver's row removed at once, rules 6–7 could drain the would-be heir before its `Leave` is declared (needs cuts failing for 5 min; a delay, never a loss); `WorldNoCommittedCut` may page if the first cut after waking from zero takes over 2 min (unmeasured); `world-start` prints "the world is up" right after the reconciler warning on a world with no host Deployments.
- **Known limits:** in `TODO.md`'s Elastic 4 block (this plan's list, plus: rule 2b's pruned-row residual; the same spawn order reaching the same gate twice while a hand-off is live — needs a nonce on the wire; a 0 s drain notice on a stale 0-player count; the S5 window).
- **Hand checks for Dawid:** in `TODO.md`. The first apply on the existing orion world is a NOT RUN runbook whose database-wipe step is not written.
- `WORLD_START_WAIT_MS` stays 180 s until a cold start with an image pull is measured.
