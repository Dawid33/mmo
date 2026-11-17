# Handoff: Elastic 2b — several coordinator replicas over one row

**Date:** 2026-11-17
**For:** a fresh session. Elastic 2a is built on `develop` (`7af6af89..e6f1bfd3`, as built in its plan). The embark
removal followed it (`e66d3995..21dbe0f8`).
**Job:**
1. Write the E2b implementation plan (`superpowers:writing-plans`) as
   `docs/superpowers/plans/<date>-elastic-2b-coordinator-replicas.md`.
2. Have Dawid look at it.
3. Execute it subagent-driven. E3–E6 get their own handoffs afterwards.

**The decisions in §4 are taken. They are drawn from E2a's execution record and a code map made at `21dbe0f8`.**
Verify each one against the code while planning. Where the code contradicts one, re-rule it in the plan under
"Where this plan departs from the handoff". Do not stall on it.

## 1. Read first, in this order

1. `CLAUDE.md` — the rules. Its "Server / store" section describes E2a as built and is current.
2. `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md` — decisions 7 and 9, §6 ("a coordinator
   replica dies mid-decision"), §10.
3. `docs/superpowers/plans/2026-11-16-elastic-2a-coordinator-state-in-the-store.md`:
   - "Decisions settled in planning" (what E2a's transitions are).
   - "E2b — what the next plan does" (the outline this handoff turns into decisions).
   - "As built", including "Final review fix wave": every ruling, every plan defect, every deferred minor.
4. `docs/superpowers/plans/2026-11-16-elastic-2-coordinator-planning-handoff.md`:
   - §4.4 (the login link), §4.8 (the panel) and §4.9 (deployment shape) still hold, except where §4 below
     changes them.
   - §4.2–§4.3 were for a design E2a replaced. Read them for the questions, not the answers.
5. `TODO.md` — the "Elastic 2a" block: hand checks still owed, the twin's write counters, known limits, and
   E2b's line.
6. The E2a workspace, while it exists (git-ignored): `.superpowers/sdd/2026-11-16-elastic-2a-coordinator-state-in-the-store/`.
   It holds `progress.md` (the ledger), `final-findings-*.md` (the three area reviews) and the per-task reports.
   `task-5-report.md` has the 17 projection rulings. `task-6-report.md` has the conflict policy.

## 2. Where things stand (code map at `21dbe0f8`; line numbers approximate)

**The coordinator is a cache of one row.**
- The `coordinator` row holds `epoch`, `version`, `core` and `history` (migration `0016`; the next migration is
  `0017`). `COORDINATOR_STATE_VERSION` = 4 (`game/src/coordinator/state.rs:32`). `HISTORY_FILE_VERSION` = 5
  (`history/persist.rs:13`). `LINK_PROTOCOL_VERSION` = 8 (`crates/svc-link/src/lib.rs:56`).
- `projection!` (`coordinator/mod.rs` ~:205) lists every durable field:
  - claims, memo + `memo_order`, `boots_seen`;
  - `region_host`, `online_on`, `hop_of`, `generation_seen`, `attached_generation`;
  - presence tallies, the calendar anchor / seen / day, `sites_key`;
  - `cuts: WorldCuts` and `successions: SuccessionRuns`;
  - `rolled_back_to`.
- Process-only fields: `memo_warned_ms`, `out`, `clock_ms`, `leases: LeaseWatch`, `login_out`, `process`,
  `committed_raw`, `unstored`, `writing`.

**One transition per pass** (`crates/server/src/coordinator_rt.rs`).
- The sequence is:
  1. `drain_hosts` (~:1341), `drain_login` (~:1454) and `feed_cuts` (~:1514, the `hosts` read: it feeds
     `observe_leases` with monotonic stamps).
  2. `maintain`.
  3. `pump_transition` (~:1949) → `write_next` (~:1975).
  4. `settle` (~:1998). Outputs are sent only after `Ok`.
- One write is in flight at a time. `take_out` (`mod.rs` ~:634) releases nothing while `writing` or
  `durable_pending()`.
- A `Conflict` is overwritten only when it is this process's own lost commit (≤ `COORDINATOR_UNACKED_MAX` = 64);
  anything else exits fenced.

**Fencing today is per process.**
- `claim_coordinator` (`world-store/src/rows.rs` ~:1009) is an unconditional `epoch + 1` at start.
- The row write (`rows.rs` ~:1028) compares the epoch AND the version.
- The store decisions are `decide_cut`, `record_cut_begin` (`two_phase.rs` ~:188, ~:341), `declare` /
  `declare_in`, `end_landless_host` and `rollback_world` (`world-store/src/succession.rs` ~:325, ~:390, ~:558,
  ~:651). Each takes a `CoordinatorFence` (the epoch) and runs its own transaction.
- **They are handed to the runtime in the pass that decided them, BEFORE that pass's transition is written.**
  That is safe with one process, because each is first-writer-wins, rediscovered after a restart, and the rollback
  is generation-conditional. It is not safe with two (§4.2).

**`ProcessBoot` stamps.**
- These carry the stamp: attachments (`world_cut.rs` ~:199, `BTreeMap<HostId, (u64, ProcessBoot)>`), a cut
  decision's `taken`, `declaring` and `ending` (`succession.rs` ~:309, ~:347).
- A stamp of another boot counts as detached / not in flight, so a restart needs no "forget" step.

**Timed work lives in `maintain`** (`mod.rs` ~:611):
- the calendar (gap rule, `calendar_resume`);
- `broadcast_daily`;
- `maintain_successions` (deadlines);
- `maintain_cuts` (starting a cut).
The lease watch is fed by the runtime's `hosts` read. `land_wanted` is input-driven.

**Links.**
- `svc_link::LinkTarget::resolve` (`crates/svc-link/src/target.rs` ~:31) returns ONE address (the first DNS
  answer), re-resolved at every dial.
- A host holds one `LinkDialer` to the coordinator (`server/src/roles.rs` ~:324) and re-`Attach`es after every
  `Up` (`game/src/world_manager/world_link.rs` `world_link_up` ~:307).
- The login service holds ONE link per world (`crates/login/src/host_link.rs` `HostLinks`, keyed by world, dialling
  `worlds.host_link`).
- The coordinator relays login orders to the gate host (`gate_host`, `mod.rs` ~:645) and keeps the held
  `SessionEnded`s (`LOGIN_ENDS_HELD_MAX` = 4096).

**Deployment** (`deploy/modules/world/coordinator.tf`).
- `replicas = 1`, `Recreate`, a **headless** Service (`cluster_ip = "None"`), `/livez` and `/readyz`.
- There is **no PodDisruptionBudget for the coordinator**.
- `world-stop` / `world-start` (justfile ~:2220, ~:2330) scale it 0 ↔ 1. `twin_check.sh` phase 7 restarts it
  (~:455).

**The seeded rig.**
- `game/tests/common/multi_rig.rs` holds ONE `SharedCore` (`coordinator/link.rs` ~:54).
- Its "store" is the last `(core, history)` byte pair, committed immediately: there is no version, no CAS and no
  second writer.
- `restart_coordinator_from_store` (~:593) loads it under a new `ProcessBoot`.

**Measured** (the twin, idle, 2 hosts, no players): about 0.7 transitions/s, about 395 B of lz4 core per
transition, history written in 6 of 48 windows, 0 conflicts. **A measurement with players is still owed** (TODO).

**The panel** (`crates/panel`, `deploy/modules/panel`, `deploy/modules/world/panel.tf`, `deploy/envs/*/panel`) is
**still untracked** on 2026-11-17. Its `ops.rs` still scales a StatefulSet `hosts` (~:194, ~:289), which is stale
since E1's per-host Deployments.

## 3. What E2b delivers

Several coordinator replicas, any of which serves any host and any login. Losing one replica loses nothing and
stops nothing for longer than a reconnect.

It removes these E2a limits:
- "while the coordinator is down: no cuts, no takeover, no logins";
- "the coordinator, toolbox, web and login have no PodDisruptionBudget" (for the coordinator).

The world stays playable with ONE replica after every task. The second replica is switched on (module default 2)
only in the deployment task, after the last correctness task.

## 4. Decisions taken

### 4.1 One row, contended; a conflict is normal and replays

- Keep E2a's one row (`core` + `history`). Do not split it before the live measurement (TODO) shows it is needed.
- With N writers, `Conflict` is the normal case.
- **Replace E2a's T6 R1** ("overwrite only your own lost commit, else exit fenced") with decision 2's original
  rule:
  - On a conflict, reload the row and re-apply the transition's inputs to it.
  - Write again.
  - Repeat a bounded number of times (`COORDINATOR_CONFLICT_RETRIES`), then back off.
  - Keep T6's lost-commit recognition: a row that already holds our own commit is not re-applied onto.
- **This needs the inputs to be keepable and re-appliable.** The runtime keeps each pass's drained host and login
  messages and store answers until its transition commits, and re-applies them onto the newer core.
- The plan must prove, per input kind, that re-applying onto a core which has moved on is what applying it there
  first would have done. Check at least these:
  - a `Request` (the memo answers a replay);
  - `Attach` (it reconciles);
  - presence and hand-off (hop ordering);
  - `RegionStarted` / `RegionParked`;
  - cut answers (`CutPrepared` / `CutFailed`);
  - succession answers;
  - store answers.
- **Seeded test:** two cores over one rig store, inputs interleaved, every conflict replayed. The world, the
  memo and the outputs must equal those of one core given the same inputs in commit order.

### 4.2 A store decision is an output: issued only after its transition commits, fenced on the row

- **Today a store call leaves in the pass that decided it, before that pass's write.** With two replicas that is
  wrong. Replica A decides cut K and records its begin; A's transition then loses the CAS to B; K exists in the
  store but in no core. The same applies to a declaration, an end and a rollback.
- **Ruling.** `decide_cut`, `record_cut_begin`, `declare`, `end_landless_host` and `rollback_world` become
  outputs of the transition that decided them. They are released only after that transition commits, like any
  message.
- **The fence moves from the per-process epoch to the row.** Each store decision compares that the decision is
  still the core's under its locks. The planner chooses the form; candidates:
  - (a) `coordinator.version >= v_committed` plus the decision's own id recorded in the core;
  - (b) a monotonic `decision_seq` column the decision carries;
  - (c) keep first-writer-wins per id and drop the epoch.
- Whichever form is chosen, give the plan a lock walk for it in `world-store/src/succession.rs`'s notes. It must
  stay safe against a decision handed out by a replica that has since lost its row.
- `claim_coordinator` is no longer "one process at a time". Rule what the epoch still means: likely only a
  generation-of-deployment fence for a release, or gone.
- **Review Focus item:** the coordinator dies, or loses the CAS, between deciding a store call and its commit. The
  store must hold no decision that no committed core knows.

### 4.3 Replicas and attachments

- Add `coordinator_replicas (replica, boot, seen_ms, addr)`, a lease beaten from each replica's MAIN loop on its
  own connection (`clock_timestamp()`, as `hosts.seen_ms`), registered at start with a never-reused id.
- Nothing is fenced on it. It decides which replica's attachments count.
- Attachments are stamped `(boot, ReplicaId)` instead of `ProcessBoot`.
  - An attachment whose replica's lease lapsed counts as detached, exactly as another `ProcessBoot` does today.
  - Attachments stay in the core projection: every replica sees every attachment.
- The gate host, a cut's participants (`expected` vs `attached`), heir candidates and `first_of_generation` all
  read the projection. They are therefore the same on every replica.

### 4.4 Outputs to a host linked to another replica: an outbox

- A transition's output whose target host is attached to ANOTHER replica is written into an `outbox` table in the
  same transaction as the row write, as `(seq, host, boot, message)`.
- The owning replica relays the rows for its own hosts, then deletes them.
- Delivery is driven by `LISTEN/NOTIFY` with the polling backstop `world-store/src/listen.rs` already has
  (`CutNotice::Resync` on reconnect).
- Delivery stays at most once after commit. Every message still heals on re-`Attach` (E2a decision 3); an outbox
  row for a boot that is no longer attached is dropped.
- `Target::Every` is fanned out per attached host.
- **This is the only cross-replica channel. No replica-to-replica link.**

### 4.5 Timed work by compare-and-set

- `maintain` (calendar, deadlines, cut start, succession deadlines) and the lease watch's declarations stay
  transitions. Any replica may attempt one when it is due, and the CAS picks one winner.
- To keep churn low, a replica attempts timed work only when it holds the lowest live replica id. That is a
  preference, not a safety: the CAS is the safety.
- **The lease watch is per replica and process-local** (E2a's S1 ruling: monotonic spans).
  - A replica declares only on its own observation span.
  - A new lowest replica re-counts from zero, which is conservative: it delays one lease and never hastens one.
  - The store's `PG_NOW_MS` lapse check stays the second gate.

### 4.6 Links

- `LinkTarget` gains `resolve_all`.
- **Hosts:** a host keeps ONE link to ONE replica: whichever address the dial reached. It re-dials, and may reach
  another replica, on a drop.
  - A host's re-`Attach` on another replica must be exactly a re-`Attach` after a coordinator restart. That is
    already true by construction once the attachments are in the projection.
  - No host-side change beyond resolving all addresses and rotating among them.
- **Login:** keeps the 2026-11-16 handoff §4.4 shape.
  - The login service holds a link per resolved replica address.
  - An order goes out on one link. That replica relays it to a gate host among the hosts attached to IT, or
    answers "none here" at once, and login tries the next link.
  - Acks come up the answering host's own replica, and login matches acks by `(token, epoch)`.
  - This changes `crates/login/src/host_link.rs` (one link per world → several), not the login database.
- **The held `SessionEnded`s** stay per replica (relay state, E2a decision 1).
- **ONE `LINK_PROTOCOL_VERSION` bump** (8 → 9) if a link enum must change (e.g. a "no gate here" answer).

### 4.7 A host is Ready once attached

`/readyz` returns 200 for a host once its `Attach` is acknowledged by some replica, not at registration (E1 §5
item). Rule what a drop and re-dial does to readiness: proposed, it stays Ready for one lease, then 503.

### 4.8 Deployment, recipes, twin, alerts

- **Coordinator module:**
  - `replicas = var.coordinator_replicas` (default 2 on the twin and on orion), behind the existing headless
    Service.
  - A PodDisruptionBudget with `min_available = 1`.
  - One identity Secret for all replicas.
  - The strategy stays `Recreate`. A mixed-build pair cannot be allowed (no version handshake), so a release is
    still stop → replace images → start, with every replica scaled together.
- **Recipes:** `world-stop` / `world-start` / `world_update.sh` scale every replica, and start waits for all of
  them.
- **`twin_check.sh`:** new phases.
  - Kill one replica mid-play. Logins and cuts continue, nothing is lost, and the generation is unchanged.
  - Kill both and restart. Everything resumes from the row.
  - Update the header list.
- **Alerts** (`deploy/modules/monitoring`): replicas live < N; conflicts per second above a threshold. Dawid's
  rule: durable Grafana changes live in that module, never the UI.

### 4.9 The world panel — last task before docs

Same as the 2026-11-16 handoff §4.8: hosts by label, plus replicas. `panel/src/ops.rs` mirrors the recipes step
for step. **It is still untracked: ask Dawid to commit it before that task. Do not edit untracked work.** Its
StatefulSet `hosts` must become E1's per-host Deployments.

### 4.10 The seeded rig — the first task

- The rig's store becomes a versioned row with CAS: `(version, core, history)`, a write refused on a stale version,
  and an outbox.
- The rig runs 1..N `Coordinator`s, each with its own `ReplicaId` / `ProcessBoot`, each host linked to one of
  them.
- Add a seeded schedule that interleaves replicas, kills one, and moves a host to another.
- Like E1's Task 3 rig rebuild, this goes FIRST, before any product change. Every later task's conservation test
  stands on it.

## 5. What E2b takes from E2a's leftovers, and what it leaves

**E2b takes** (each sits in code it rewrites):
- the conflict re-read inside `finish` not bounded by the stop timeout, and "two lost answers exit as another
  writer" (both are replaced by §4.1);
- the extra core encode per pass in `take_out` → `durable_pending` (§4.1 touches it: cache the raw bytes);
- `CoordinatorSource::needs_save` (a test-only leftover);
- the duplicated `Resumed` / attach helpers in `server/tests/{coordinator_rt,roles}.rs` (move them to
  `tests/common` when the replica tests are written);
- "a coordinator that dies between its claim and its first write beside saved regions is refused at the next
  start" (§4.2 changes what a claim is: re-rule it);
- the parked "history `now` lags for one pass after a load in a gap" (one line in `load`, if `load` is touched).

**E2b leaves** (documented in TODO, small and independent):
- `twin_check.sh` `row_land` blind spots;
- the memo keeping a pruned host's entries until its cap;
- E1's "E2 leaves" list (2026-11-16 handoff §5);
- nested successions (decision 9's residual);
- the `go-nogo-r3` window covering only the host run.

## 6. Rules that carry over

- **Conservation beats latency; wait, never guess.** A task that changes who holds what is not done without a
  conservation test.
- **Fencing in PostgreSQL is the only safety.**
  - Nothing a Deployment, a Service, a replica count, a lease or a "lowest replica" preference does may be what
    prevents two writers.
  - Never a store write that skips the fence.
- **Migrations** are immutable and identical in shape in `migrations/pg` and `migrations/sqlite`. The next is
  `0017`. Worlds are re-forged, never migrated: say "re-forge / `just pg-reset`".
  - SQLite (role `all`, single-player) stays one replica. New tables exist there too, unused, or SQLite answers
    `Unsupported` as for successions.
  - Rule which.
- **Wire enums:** append-only. One `LINK_PROTOCOL_VERSION` bump per plan.
- **The core stays deterministic:** no clock, no environment and no store in `game`; `BTreeMap` / `BTreeSet`; named
  consts. Every new core field is classified in `projection!`. Every new `WorldManager` field is classified in
  `host_fields!`.
- **Write-before-send** holds for every new output kind: outbox rows and store decisions.
- **Lock walks:** every new or changed statement on `coordinator`, `coordinator_replicas`, `outbox`, `hosts`,
  `super_regions`, `successions`, `world` and `cut_begins` gets its walk in `world-store/src/succession.rs`'s
  notes. A prepared transaction is not a waiting backend.
- **Ask Dawid first:** client tests, live runs (`dev-r3`, `go-nogo-r3`, `just run`), measurements, anything on
  orion. `just twin-check` is allowed.
- **Known reds, not ours:** three `lod::tests` (game lib), `codec::golden_blobs_decode`, flaky SQLite
  `stud_refusal::…`, flaky svc-link `dialer_reconnects_after_listener_restart`.
- **Watch:** the takeover test `a_clean_leave_keeps_the_generation_and_its_waiting_transfers_reach_the_heir` failed
  once under load in E2a. It now prints full diagnostics on failure. Capture them if it fails again.

## 7. How E2a was executed — keep what worked, fix what bit

- **Subagent-driven:**
  - one implementer and one reviewer per task;
  - a ledger in `.superpowers/sdd/<plan>/`, with rulings recorded as `Ruling: what — why — cost if wrong`;
  - a whole-branch review split into three areas: store+server, game, and client/scripts/docs;
  - one fix wave, one scoped re-review.
  The three-area final review found the worst bugs, a conservation hole (C1) and the calendar running backwards.
  Keep it.
- **Write the per-input replay table (§4.1) and the store-decision lock walk (§4.2) INTO the plan, before the
  tasks.** E2a's expensive defects were exactly of this kind: a durable flag beside a non-idempotent store call,
  and a store answer lost between commit and transition.
- **Dawid commits in this checkout concurrently, and other sessions stage in the SHARED index.**
  - Every dispatch carries filtered staging: `git diff -U3`, keep your own hunks, `git apply --cached --recount`.
  - **Commit from the index** with a plain `git commit`. NEVER `git commit -- <paths>` on a file partly staged:
    it commits the whole working-tree file, and that swept Dawid's hunks twice in two plans.
  - Check `git diff --cached --name-only` before every commit. The controller verifies after each commit that his
    hunks are still unstaged.
- **No git worktrees.** A reviewer needing an old revision uses `git show <sha>:<path>`, or a `git archive` export
  in /tmp with its own target dir. The controller once wrongly offered `git worktree add` in a review prompt; do
  not.
- **Gates:**
  - Compile the browser build (`just web-check`) whenever client code changes. E2a's T1 broke wasm32 unseen until
    the twin.
  - Run the twin at the deployment task AND once before it, at the end of the last correctness task, to find
    script assumptions early. E2a's `twin_check.sh` encoded a ruling the plan reversed.
- **Postgres suites:** one at a time; world-store at `--test-threads=4` (the default thread count hits "too many
  clients"). The machine may reboot; restart `just pg-up`.
- **Edits:** subagents edit with Edit / Write, one file per call. A refused command is reported, never worked
  around.

## 8. Sizing

Medium to large, about 10–13 tasks:
1. the rig;
2. conflict replay;
3. store decisions as outputs + the row fence;
4. replica rows + attachments;
5. the outbox;
6. timed work;
7. `LinkTarget::resolve_all` + host rotation;
8. login's several links;
9. host `/readyz`;
10. the module, recipes, twin and alerts;
11. the panel;
12. docs.

Order them so one replica plays after every task.

## 9. Open, for Dawid (the plan proceeds on these defaults)

- **Orion has one node:** two replicas there survive a coordinator crash or a release-less restart, not the node.
  Default 2 anyway; say if orion should stay at 1 until a second node exists.
- **The live measurement with players** (TODO, E2a) before anything splits `core` / `history`. E2b keeps one row
  either way. The measurement only decides whether conflict retries need a narrower row later.
- **Commit the panel** (still untracked) before E2b's panel task.
- **E2a's hand checks** are still owed (TODO): `just go-nogo-r3 1 60`, the autopilot client tests, a twin with a
  player and a landed host.
