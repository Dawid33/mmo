# Elastic 2b — Several Coordinator Replicas over One Row Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Any number of coordinator replicas serve one world from the one `coordinator` row. Any replica serves any host and any login. Losing one loses nothing, and stops nothing for longer than a reconnect.

**Architecture:**
- Each replica keeps a cache of the row and logs the inputs it applied since its last commit.
- A transition commits with a compare-and-set on `version`. On a conflict the replica rebases: it reloads the newer row and re-applies its logged inputs.
- Every store decision (decide a cut, record its begin, declare, end, roll back) runs **inside the transaction that writes the core that decided it**. The store therefore never holds a decision that no committed core knows.
- Replicas register in `coordinator_replicas` and beat a lease. The core keeps a durable set of live replicas, and an attachment counts only while its replica is in that set.
- Messages for a host linked to another replica go through an `outbox` table, written in the same transaction and relayed in order by the replica holding the link.
- One replica, the **ticker** (the lowest live id), alone does the timed and lease-driven work.
- Hosts dial one replica and rotate on a drop. The login service holds a link per replica.

**Tech Stack:** Rust (sqlx on PostgreSQL and SQLite, crossbeam, quinn via `svc-link`, tokio at the edges), OpenTofu (`deploy/modules/world`, `deploy/modules/monitoring`), bash (`scripts/twin_check.sh`, `scripts/world_update.sh`, the justfile).

**Spec:**
- `docs/superpowers/plans/2026-11-17-elastic-2b-coordinator-replicas-planning-handoff.md` (the decisions; §4 is re-ruled below where the code disagreed).
- `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md`: decisions 7 and 9, §6 ("a coordinator replica dies mid-decision"), §10.
- `docs/superpowers/plans/2026-11-16-elastic-2a-coordinator-state-in-the-store.md`: "Decisions settled in planning", "E2b — what the next plan does", "As built".
- `docs/superpowers/plans/2026-11-16-elastic-2-coordinator-planning-handoff.md` §4.4, §4.8, §4.9.

## Where this plan departs from the handoff (verified against the code at `03da3335`, 2026-11-17)

1. **§4.2: store decisions run inside the core's transaction, not after it.**
   - The handoff's ruling was "released after commit, fenced on the row", with three candidate fences.
   - Every decision already opens with `coordinator FOR SHARE` and then `world` (`world-store/src/succession.rs` notes; `two_phase.rs:200`, `:345`; `succession.rs:292`). The CAS `UPDATE` takes that same row first, in a stronger mode.
   - So the decision runs as a **savepoint after the CAS `UPDATE`, in the same transaction**. The core and its decision then commit together or not at all, and a stale replica cannot issue anything: issuing *is* committing.
   - No new fence is needed, and the epoch's `WHERE` goes. Candidates (a)–(c) are unnecessary.
   - Post-commit effects run after the commit, by the runtime, and are recovered by the existing lookups if the replica dies first. They are `finish_staged` (`COMMIT PREPARED` of the parts) and `notify_cut`.
2. **§4.5: timed work is not "any replica may attempt it".** The replay audit found it wrong in two ways.
   - **Several inputs read process-local observations:**
     - `leases`;
     - the `serde(skip)` fields `expected`, `world_tick` and `blocked_*` (`world_cut.rs:185-226`);
     - `begin_recorded`.
   - **Re-applying them onto another replica's core is unsafe:**
     - `successions_open` abandons runs it never saw (`succession.rs:445-463`);
     - `observe_leases` forgets hosts that attached elsewhere (`world_cut.rs:352-360`).
   - **Ruling:** the **ticker** (the lowest id in the core's durable live-replica set) alone runs:
     - `maintain`;
     - the `hosts` read → `observe_leases` / `set_expected_hosts` / `set_world_tick`;
     - every `take_*` hand-out;
     - the land lookups and the succession re-reads.

     Other replicas apply host messages, link events, their own replica lifecycle and decision answers that carry their own stamp. The CAS stays the safety: a stale cache that believes it is the ticker loses the CAS, rebases, and replays without ticker work.
3. **§4.1's per-input proof found three inputs that must not be replayed, and four that need a stamp nonce.**
   - Not replayed: `HostMsg::Login`, `login_order` and the re-read answers. The table is under "Decisions" item 3.
   - The answer handlers `declared`, `landless_ended`, `declare_failed`, `end_failed` and `rolled_back` must accept only the answer of the call they marked. Today some clear marks with no stamp check (`succession.rs:622`, `:1263`, `:682`).
4. **§4.3: `ReplicaId` replaces `ProcessBoot`; it is not added beside it.** A replica id is issued per process start and never reused, which is exactly what `ProcessBoot` was. An attachment stays `(host boot, ReplicaId)`.
5. **Query answers leave without a write (`take_out`). With several replicas they can come from a stale cache** (audit H8: `HopIs`, `ClaimIs`, `CutDone` from `outcomes`, `CutPeerCount`).
   - **Ruling:** with more than one live replica, a pass that releases outputs with nothing durable changed first reads `SELECT version`. If the row moved, the replica rebases and replays first.
   - With one replica this is skipped, so it costs nothing where E2a measured.
6. **A replica's cache is refreshed by `NOTIFY`, not only by conflicts.**
   - The row write sends `pg_notify('coordinator_row', '<version>')`.
   - Each replica listens with `listen.rs`'s machinery, generalised to a channel name, with a polling backstop.
   - **An outbox row is relayed only from a cache at least as new as the row's version.** Without this, a host's `CutPrepared` would meet a cache that has not seen its cut, and a lookup would abort the cut (audit H3).
7. **`record_cut_begin` moves into the transition that starts the cut** (audit H11). Today the runtime polls `world_cut_running` and records it from a process-local `begin_recorded`.
8. **The cut id's high half becomes durable.** `WorldCuts.epoch` is `serde(skip)` and set from the per-process claim epoch (`world_cut.rs:183-185`, `:288`).
   - It is now set once, at bootstrap, from the row's epoch (1), and stored.
   - `next` is already durable, and a cut id leaves only after its transition commits, so ids never repeat within a row's life.
9. **The calendar's day turn only moves forward** (audit H1). `broadcast_daily` compares with `!=` (`mod.rs:833`), so two replicas a tick apart would flap the day. `push_view` takes `max(calendar_now, history.now())`.
10. **The host's pin is fixed for its dialer's lifetime** (`roles.rs:295`). All replicas therefore share ONE identity: the module's one Secret, and in dev one `VOXEL_COORDINATOR_CERT_HASH_FILE`.
11. **Several addresses also come from a comma-separated target** (`127.0.0.1:6480,127.0.0.1:6481`), not only from DNS. Dev and the server tests run replicas on one machine on different ports.
12. **Migration `0017` is ONE migration** (Task 4): `coordinator_replicas`, `outbox`, `coordinator.history_version`, `coordinator.next_replica`.
    - Tasks 2–3 change no schema: the `epoch` column stays, read by nothing, to avoid a SQLite `DROP COLUMN`.
    - `COORDINATOR_STATE_VERSION` goes 4 → 5 once, in Task 2.

## Decisions settled in planning

### 1. The transition, with replicas

**Each replica holds:**
- its links;
- the cached core at `version`;
- the **input log**: every input applied since its last committed version, each with the `now_ms` it was applied at;
- its watches (`LeaseWatch` for hosts, `ReplicaWatch` for replicas, on the monotonic clock);
- the login relay queues;
- the held `SessionEnded`s.

Nothing else.

**A pass:**
1. Apply the inputs drained this pass through `Coordinator::apply`, which logs them.
2. If the replica is the ticker: the hosts read, then `maintain(now)`.
3. If no write is in flight, `take_transition()`. It carries `core`, `history`, `out` (direct), `outbox` and `decisions`.
4. `write_transition`, in one transaction:
   1. the CAS `UPDATE`;
   2. each decision in its own savepoint;
   3. the outbox inserts;
   4. the outbox take for my linked hosts;
   5. the outbox discard for the hosts whose `Attach` this transition applied;
   6. `pg_notify`.
5. On `Ok(written)`:
   1. `transition_written(version)` truncates the log up to this transition's boundary;
   2. send the taken outbox rows, then `out`;
   3. run the post-commit effects;
   4. log the decision answers as inputs of the next pass.

**On a conflict:**
- First, lost-commit recognition: the row is at `mine + 1` and its core hash is among `unacked`. That is our own commit; take it as `Ok` (its decisions' answers are lost: item 4).
- Otherwise `core.rebase(row)`: the projection is replaced, `out` is cleared and the whole log is re-applied; then write again.
- `COORDINATOR_CONFLICT_RETRIES = 8` in a row, then wait `COORDINATOR_SAVE_RETRY_MS` and start again. Never an exit.

**A replica not in the live set of the core it applies to** (a reload shows it removed) exits `EXIT_FENCED` without writing.

### 2. The ticker and the live set

- `replicas: BTreeSet<ReplicaId>` is durable in the projection.
- `Input::ReplicaJoined { me, alone }` is the first input of every replica.
  - It adds `me`.
  - With `alone` (the store found no other replica row beaten within `REPLICA_LEASE_MS`), it drops every other id. This is the total replica crash.
- `Input::ReplicaLapsed(r)` drops `r`. In the same transaction, the decision `EndReplica(r)` marks its row dead.
  - It is attempted by any replica that is the lowest of those its OWN `ReplicaWatch` shows live, after `REPLICA_LEASE_MS` (10 000) of unchanged `seen_ms` reads on its monotonic clock.
  - It is idempotent, and the CAS picks one winner.
- `r`'s next beat finds its row dead and exits fenced. Its attachments count as detached from that commit on, on every replica.
- `ticker()` = `replicas.first()`. A dead ticker stalls timed work for at most `REPLICA_LEASE_MS` plus one read. That is below `CALENDAR_GAP_MS` (30 s), so no calendar gap; it is below `SUCCESSION_DEADLINE_MS` (30 s); and a cut deadline only aborts a cut.
- A new ticker's `LeaseWatch` counts from zero. That delays a declaration by one lease and never hastens one. `PG_NOW_MS` stays the store's second gate.

### 3. Re-applying each input onto a core that moved on

The proof obligation of handoff §4.1. "Pure" means the handling reads only the projection and the input. Lines are at `03da3335`.

| Input | Replayed? | Why it is what applying it there first would have done | Change needed (task) |
|---|---|---|---|
| `Attach` | yes | It reconciles the host against whatever the core holds (`mod.rs:470-498`). Its stamp is the replica the link reached. `drop_unsent` covers only this replica's `out`: rows other replicas wrote for the host are discarded in the same transaction (item 5). | Stamp `(boot, ReplicaId)` (T2); `first_of_generation` detaches attachments of any replica (T4); outbox discard (T5) |
| `Settled` | yes | `retain`: idempotent (`mod.rs:1163`). | — |
| `RegionStarted` / `RegionStopping` / `RegionParked` / `PersonCrossed` / `Positions` / `DirectiveBounced` | yes | Pure on `region_host`, `incarnations` and the history (`mod.rs:500-560`). Their outputs are routed by attachment. | Routing (T5) |
| `Request` (state-changing) | yes | Memo, then claim check, then `remember`, all in the projection. A replay onto a core where another host took the claim answers `AlreadyPlaying`, as a first apply would. | — |
| `Request` (query) / `Settle` memo hit | yes | Read-only; the answer leaves after the version check (departs 5). | Version check (T4) |
| `PlayerOnline` / `PlayerOffline` / `PlayerHandedTo` | yes | Hop-ordered on the durable `hop_of` / `online_reported`. `HopIs` / `ClaimIs` answers go through the version check. | T4 |
| `ReleaseClaim` | yes | Pure. There is no hop guard, as today. Noted, not changed. | — |
| `CutPrepared` / `CutFailed` | yes, when the cut is in the core | Pure on `cuts.run`. For a cut id above this core's `cuts.next`, the cache is stale: refresh first, never a lookup that would abort (audit H3). | T5 |
| `LeaveRequest` | yes | Stamped with the replay's `now_ms` (the log keeps the original). | T2 |
| `SuccessionApplied` / `SuccessionAdopted` | yes | Pure on the durable `cuts.boots` (`succession.rs:722-741`). | — |
| `LandWanted` | yes (ticker answers) | It only queues `land_lookups`; the answer is ticker work. | T6 |
| `CutPeerCount` | yes | A relay. The run is in a refreshed cache (T5); the peer's message goes by outbox if it is linked elsewhere. | T5 |
| `HostMsg::Login` | **no** | Process-local `login_out`, forwarded at once (`mod.rs:456`, `RT:1307`). It never enters the log. | T2 |
| `login_order` | **no** | Process-local relay to a gate among this replica's own attached hosts. Not logged, not durable. | T8 |
| `LinkDown { host }` (detach) | yes | Applied only if the attachment's stamp is this replica's. A host that re-attached elsewhere keeps that attachment. | T2 |
| `ReplicaJoined` / `ReplicaLapsed` | yes | Set operations, idempotent. | T4 |
| `Maintain` | ticker only | Re-evaluated at replay: a replica that is not the ticker of the rebased core drops it from the log. | T6 |
| `HostsRead` (`observe_leases`, expected hosts, world tick) | ticker only, **never replayed** | A watch observation; its effect on the core is re-derived at the next read. `forget_absent_hosts` forgets only ids below the read's `next_host_id` that no live replica's attachment holds (audit H7). | T6 |
| `SuccessionsOpen` (re-read answer) | ticker only, **never replayed** | Runs are stamped with the version that opened them. A read issued at version `v` abandons only runs opened at or before `v` (audit H5). "A read is owed" becomes process-local ticker state, and a loaded core asks once. | T6 |
| Decision answers (`Decided`, `Declared`, `LandlessEnded`, `RolledBack`, `DeclareFailed`, `EndFailed`) | yes, when the mark matches | Each mark is a `CallStamp { replica, version }` (version = the commit that issued it). The answer carries it, and a mismatching answer changes nothing (audit H2, H9). | T3 |
| `LandOwners` (lookup answer) | ticker only | A set insert. | T6 |

**Clock.** `Coordinator::apply` uses `now = max(now_ms, self.clock_floor())`. The floor is the largest store-ms stamp in the projection: `calendar_seen_ms`, `calendar_anchor.0`, `cuts.last_ms`, each run's start. With this, a replica a few milliseconds behind another never moves a recorded time backwards (T2).

### 4. Store decisions in the transaction (the lock walk; T3 writes it into `succession.rs`'s notes)

**`write_transition`, one transaction:**
1. `UPDATE coordinator SET core, history = COALESCE(...), history_version = CASE ..., version = version + 1, saved_utc WHERE id = 1 AND version = $v RETURNING version, <now_ms>`.
   - It locks the one row `FOR NO KEY UPDATE`.
   - Zero rows → rollback → `Conflict`. Nothing below ran.
2. For each decision, in the transition's order: `SAVEPOINT d<i>`, then the decision's body WITHOUT its `coordinator FOR SHARE`. The row is already held, in a stronger mode.
   - `decide_cut` (minus `finish_staged`).
   - `record_cut_begin`.
   - `declare_in`.
   - `end_landless_host`.
   - `rollback_world` (conditional on its generation, as now).
   - `end_replica`: `UPDATE coordinator_replicas SET state = DEAD WHERE replica = $r`.

   A domain refusal or a SQL error inside → `ROLLBACK TO SAVEPOINT`, and the refusal is that decision's answer. A deadlock (`40P01`) or `lock_timeout` inside is also a refusal: the decision is retried later, the core still commits.
3. `INSERT INTO outbox (version, host, boot, msg)` for each outbox message.
4. `DELETE FROM outbox WHERE host = ANY($attached_here) RETURNING seq, version, host, boot, msg`, ordered by `seq`.
5. `DELETE FROM outbox WHERE host = ANY($attached_now)`: the hosts whose `Attach` this transition applied. It runs **before** step 4, so their rows are discarded, not delivered.
6. `SELECT pg_notify('coordinator_row', $version)`.

**Why no cycle:**
- Every statement that locks `coordinator` takes it FIRST. Hosts, claims, registrations, heartbeats and the operator never lock it.
- So nothing holds another lock and then waits for `coordinator`.
- After it, each decision keeps its existing walk (`world` → `hosts` → `super_regions`). That walk was already a suffix of `coordinator` first.
- Another replica's `write_transition` waits on the row and then finds `version` moved (READ COMMITTED re-evaluates the `WHERE`): a conflict, never a deadlock.
- The `outbox` and `coordinator_replicas` rows are locked only by coordinator statements, after `coordinator`, so they add no edge.
- A replica beat (`UPDATE coordinator_replicas SET seen_ms ... WHERE replica = $me AND state = LIVE`) locks one row and wants nothing after it. It waits at most for an `end_replica` holding it, under its own 1 s `lock_timeout`.

**Cost:** a decision holds the row for its `lock_timeout` at most (a declaration waits for a staged part). Other replicas' writes wait that long, then conflict and rebase. Decisions are rare: a cut per period, successions on failure.

### 5. The outbox

- Rows are `(seq BIGSERIAL, version, host, boot, msg BYTEA)`, keyed by host, not by replica: whoever links the host relays them.
- They are written only in `write_transition`, so `seq` order is version order.
- They are taken in two places:
  - in a replica's own `write_transition` (step 4);
  - between transitions by `take_outbox(hosts, boots)` when a `NOTIFY` names a newer version or the backstop (`COORDINATOR_POLL_MS = 1_000`, only while more than one replica is live) fires.

  The take and the write are both store calls of the coordinator, one at a time, answered in order. Each replica sends what it took in the order it took it, before any later transition's outputs. Per-host order is commit order.
- A row whose `boot` is not the boot of the link it would go to is deleted unsent.
- A row older than the host's newest applied `Attach` is discarded by step 5, because the `Attach` reconciles what it carried.
- **Delivery stays at most once after commit** (E2a decision 3). A row whose host is linked to no replica stays until the host's next `Attach` discards it, or until a transition that forgets the host deletes its rows.
- `Target::Every` is fanned out per host attached to a live replica (`Coordinator::route_out`).

### 6. Login (handoff 2026-11-16 §4.4, kept)

- Login holds a link per address of the world's `host_link`, which may name several (comma list or DNS).
- An order goes out on one link. That replica relays it to its own gate host: the lowest host attached **to it**, live, taking logins. If it has none, it answers at once with `HostToLogin::NoGate { player_token, session_epoch }`, and login tries the next link.
- Acks match by `(token, epoch)` on any link.
- **ONE `LINK_PROTOCOL_VERSION` bump, 8 → 9, in T8.**

### 7. Host readiness (handoff §4.7)

A host's `/readyz` is 200 while it is registered AND either:
- `WorldManager::world_attached()`, or
- it was attached within the last `HOST_LEASE_MS` (10 000).

503 bodies: `forge`, `drained`, `starting`, and the new `attaching`.

## To confirm with Dawid (the plan proceeds on these defaults)

- **Orion stays at 2 replicas on one node.** That survives a replica crash, not the node.
- **Decisions inside the core's transaction (departs 1).** If a store decision ever needs to run long, it holds the row that long.
- **The ticker owns all timed and lease work (departs 2).** A dead ticker stalls cuts and declarations for ≤ 10 s.
- **Commit `crates/panel` before Task 11.** It is untracked on 2026-11-17. The task stops and asks if it still is.

## Global Constraints

- No git worktrees; work on `develop` in this checkout. Dawid commits concurrently, and other sessions stage in the SHARED index.
  - A file holding foreign hunks is staged with a filtered patch: `git diff -U3`, keep your hunks, `git apply --cached --recount`.
  - Commit from the index with a plain `git commit`, never `git commit -- <paths>` on a partly staged file. Check `git diff --cached --name-only` before every commit, and after it confirm that foreign hunks are still unstaged.
  - Never `git add -A`, `git stash`, `--unidiff-zero`.
- **Conservation beats latency; wait, never guess.** Every entity, item, player and person exists exactly once through any crash or conflict point. A task that changes who holds what is not done without a conservation test.
- **Fencing in PostgreSQL is the only safety.** Nothing a Deployment, a Service, a replica count, a lease or the ticker preference does may be what keeps two writers apart. Never a store write that skips the fence: from Task 3 on, the coordinator's fence is the row's `version` CAS, and every store decision runs under it.
- **Write-before-send** for every output kind: messages, outbox rows, store decisions.
- **Migrations** are immutable and identical in shape in `migrations/pg` and `migrations/sqlite`. This plan adds exactly `0017_coordinator_replicas`. `tests/schema.rs` `EXPECTED` follows. Worlds are re-forged, never migrated: say "re-forge / `just pg-reset` / `just run --reset`".
  - SQLite (role `all`, single-player): the tables exist. The one replica registers (ids issued) and never beats, reads replicas or uses the outbox. A transition with decisions on SQLite is a bug (`Unsupported`).
- **Lock walks:** every new or changed statement on `coordinator`, `coordinator_replicas`, `outbox`, `hosts`, `super_regions`, `successions`, `world`, `cut_begins` gets its walk in `world-store/src/succession.rs`'s notes. A prepared transaction is not a waiting backend.
- **The core stays deterministic:** `game` reads no clock, no environment and no store; `BTreeMap` / `BTreeSet`; named consts. Every new core field is classified in `projection!`. Every new `WorldManager` field is classified in `host_fields!`.
- **Wire enums are append-only.** One `LINK_PROTOCOL_VERSION` bump in the whole plan (T8, 8 → 9). `COORDINATOR_STATE_VERSION` 4 → 5 once (T2).
- A replica id, like a host id, is never reused, and nothing is derived from its number.
- `game`, `server` and `worldgen` stay free of Bevy. rustls uses ring only. Tokio stays at the edges.
- **Ask Dawid first:** client tests, live runs (`dev-r3`, `go-nogo-r3`, `just run`), measurements, anything on orion (no `just orion-*` apply or push, no `kubectl --context orion` write verb). `just twin-check` is allowed.
- **Tests:**
  - `just test-one <crate> "<args>"`, filters after `--`.
  - Server targets `roles`, `coordinator_rt`, `hosts`, `takeover`, `login_link` need `--features test-hooks`.
  - Postgres suites need `just pg-up` and run one at a time; world-store at `--test-threads=4`.
  - Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
  - `just smoke` after every Rust task. `just web-check` whenever `crates/client` or a crate it compiles for wasm32 changes.
- **Known reds, not ours:** three `lod::tests`, `codec::golden_blobs_decode`, flaky SQLite `stud_refusal::…`, flaky svc-link `dialer_reconnects_after_listener_restart`. WATCH `a_clean_leave_keeps_the_generation_and_its_waiting_transfers_reach_the_heir`: capture its diagnostics if it fails.
- **The ledger** is `.superpowers/sdd/2026-11-17-elastic-2b-coordinator-replicas/progress.md`. A decision this plan did not make goes there as `Ruling: what — why — cost if wrong`.
- Subagents edit with Edit / Write, one file per call. A refused command is reported, never worked around. The justfile is canonical, secrets are never printed, player-facing text says **Errantry**, and dated documents are not edited.

## Review Focus

1. **A replica dies, or loses the CAS, between deciding a store call and its commit.**
   - Expected: the store holds no decision that no committed core knows.
   - A decision's answer lost with its replica is recovered by lookup, never re-issued blind. A re-declaration would answer `SuccessionOpen` and roll the world back.
   - Pinned by T3: `a_decision_rolls_back_with_its_conflicting_transition`, and `a_replica_killed_after_a_decision_commits_is_recovered_by_lookup` (seeded).
2. **A host's link moves to another replica while messages for it are in flight on the first.**
   - Expected: the host never applies a message decided before its newest `Attach`, nor two messages out of commit order.
   - Pinned by T5: `outbox_rows_older_than_an_attach_are_discarded`, `a_host_moving_between_replicas_sees_commit_order` (seeded).
3. **Two replicas a few milliseconds apart on the store clock both run a pass across a day boundary.**
   - Expected: one `Calendar` message per day, the day never goes backwards, and no recorded deadline moves earlier.
   - Pinned by T2: `two_replicas_a_tick_apart_turn_each_day_once`.
4. **The ticker is killed mid-cut while a host is lapsing.**
   - Expected: the next replica takes over within one replica lease plus one host lease; the cut aborts or commits as the store decided; the host is declared once; nothing is lost; the generation is unchanged.
   - Pinned by T6: `the_ticker_killed_mid_cut_hands_over_without_a_double_declaration` (seeded, and `roles.rs`).
5. **Every replica is down, then one comes back.**
   - Expected: it registers `alone`, drops the dead replicas from the live set at its first transition, every host re-attaches to it, the next cut commits, and the generation is unchanged.
   - Pinned by T4: `every_replica_down_then_one_back_resumes_from_the_row` (roles), and T10's twin phase.

## File structure

| Path | Responsibility after this plan |
|---|---|
| `crates/game/src/coordinator/input.rs` (new) | `Input`, `Coordinator::apply`, the input log, `rebase`, the clock floor |
| `crates/game/src/coordinator/replicas.rs` (new) | `ReplicaId`, the live set, `ticker()`, `ReplicaWatch`, `route_out` (direct vs outbox) |
| `crates/game/src/coordinator/{mod,world_cut,succession,state,link}.rs` | stamps by replica; `CallStamp` marks; decisions carried in `Transition`; ticker-only paths |
| `crates/game/tests/common/multi_rig.rs`, `peer_schedule.rs` | a versioned CAS row with an outbox; 1..N replicas; seeded replica schedules |
| `crates/world-store/migrations/{pg,sqlite}/0017_coordinator_replicas.sql` (new) | `coordinator_replicas`, `outbox`, `coordinator.history_version`, `coordinator.next_replica` |
| `crates/world-store/src/coordinator_tx.rs` (new) | `write_transition` (CAS + decisions in savepoints + outbox + notify), `take_outbox`, `row_version` |
| `crates/world-store/src/replicas.rs` (new) | `register_replica`, `beat_replica`, `replicas` read |
| `crates/world-store/src/{rows,two_phase,succession,listen,handle,thread}.rs` | decision bodies callable inside a transaction; the listener generalised to a channel name; lock notes |
| `crates/server/src/coordinator_rt.rs` | the pass with rebase and replay, the ticker, outbox relay, the version check, replica beats |
| `crates/server/src/world_file.rs` | bootstrap without `claim_coordinator`; `needs_save` removed |
| `crates/svc-link/src/{target,lib}.rs` | `resolve_all`, a comma list, the dial rotating over addresses |
| `crates/login/src/host_link.rs`, `crates/login-api/src/lib.rs` | several links per world; `NoGate` |
| `crates/server/src/health.rs`, `crates/game/src/world_manager/world_link.rs` | host Ready once attached; replica metrics |
| `deploy/modules/world/coordinator.tf`, `variables.tf`; `deploy/modules/monitoring/values/kube-prometheus-stack.yaml.tftpl`; `justfile`; `scripts/{world_update,twin_check}.sh` | N replicas, PDB, recipes, twin phases, alerts |
| `crates/panel/src/ops.rs`, `deploy/modules/panel` | hosts by label, replicas (after Dawid commits it) |

---

### Task 1: The seeded rig has a versioned row, an outbox, and room for several replicas

The handoff's §4.10. It comes FIRST, with no product change: every later conservation test stands on it.

**Files:**
- Modify: `crates/game/src/coordinator/link.rs`: `SharedCore` (~:46-164) becomes `RigStore` + `RigReplica`.
- Modify: `crates/game/tests/common/multi_rig.rs`: `with_coordinator` (:523), `restart_coordinator_from_store` (:593), the route path.
- Test: `crates/game/tests/peer_schedule.rs`, `crates/game/tests/coordinator*.rs`.

**Interfaces:**
- Produces (game, `#[doc(hidden)]` test support in `coordinator/link.rs`):

```rust
pub struct RigRow { pub version: u64, pub core: Vec<u8>, pub history: Vec<u8> }
pub struct RigStore { row: Option<RigRow>, outbox: Vec<RigOutboxRow>, seq: u64 /* … */ }
pub struct RigOutboxRow { pub seq: u64, pub version: u64, pub host: HostId, pub boot: u64, pub msg: CoordMsg }
impl RigStore {
    /// `Err(RigConflict)` when `version` is not the row's.
    pub fn write(&mut self, version: u64, core: Vec<u8>, history: Option<Vec<u8>>) -> Result<u64, RigConflict>;
    pub fn row(&self) -> Option<&RigRow>;
}
pub struct RigReplica { pub core: Coordinator, pub version: u64, pub hosts: BTreeSet<HostId> /* linked here */ }
```

- `MultiRig` gains `replicas: Vec<RigReplica>` (one in this task) and `store: RigStore`. Each host's `InlineWorldLink` names its replica index. `restart_coordinator_from_store` reloads replica 0 from `store.row()`.
- Until Task 2 the rig writes at the version it last read, so a conflict cannot happen with one replica.

- [ ] **Step 1:** Write `peer_schedule::the_rig_store_refuses_a_stale_version`. Two writes at the same version: the second is `Err(RigConflict)`, and the row is the first.
- [ ] **Step 2:** Run `just test-one game "--test it peer_schedule::the_rig_store_refuses"`. Expected: FAIL (does not compile).
- [ ] **Step 3:** Implement `RigStore` and `RigReplica`. Port `SharedCore`'s routing onto replica 0. Delete `SharedCore` once nothing names it.
- [ ] **Step 4:** Run `just test-one game "--no-fail-fast"`. Every existing test passes unchanged (the known `lod::tests` reds aside). Run `just test-threads`.
- [ ] **Step 5:** Commit: `test(rig): the seeded rig's store is a versioned row with an outbox`.

### Task 2: Inputs are logged; a conflict rebases and replays; `ReplicaId` stamps

**Files:**
- Create: `crates/game/src/coordinator/input.rs`.
- Modify:
  - `crates/game/src/coordinator/mod.rs`: `handle` (:437), `maintain` (:611), `take_out` (:634), `take_transition` (:690), `transition_written` (:711), `transition_failed` (:719), `broadcast_daily` (:831), `push_view` (:814), `load` (:337), `projection!` (:230).
  - `crates/game/src/coordinator/{state,world_cut,succession}.rs`: `ProcessBoot` → `ReplicaId`, `COORDINATOR_STATE_VERSION = 5`, and `WorldCuts.epoch` durable.
  - `crates/server/src/coordinator_rt.rs`: `settle` (:1998), `failed`, `finish` (:2141), the `unacked` / `conflict_retried` fields.
  - `crates/game/tests/common/multi_rig.rs`: N replicas.
- Test: `crates/game/tests/coordinator.rs`, `peer_schedule.rs`, `crates/server/tests/coordinator_rt.rs`.

**Interfaces:**

```rust
// game::coordinator::replicas (created here; T4 adds the live set)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReplicaId(pub u64);

// game::coordinator::input
pub enum Input {
    Host { host: HostId, msg: HostMsg },   // never HostMsg::Login: `apply` forwards it to login_out unlogged
    LinkDown { host: HostId },
    Maintain,
    // T3 adds Decision(DecisionAnswer); T4 ReplicaJoined/ReplicaLapsed; T6 HostsRead/SuccessionsOpen/LandOwners
}
impl Coordinator {
    pub fn apply(&mut self, input: Input, now_ms: u64);
    /// Replace the projection by a newer row's (`core`, `history`, `version`), keep the process's
    /// own fields, clear `out` and `writing`, and re-apply every logged input in order.
    pub fn rebase(&mut self, core: &[u8], history: Option<&[u8]>, version: u64) -> Result<(), HistoryLoadError>;
    /// The commit of the transition last taken: drop the log up to its boundary.
    pub fn transition_written(&mut self, version: u64);
    pub fn replica(&self) -> ReplicaId;   // was `process()`
}
pub const COORDINATOR_CONFLICT_RETRIES: u32 = 8;   // server
```

- `handle`, `maintain` and `host_detached` become private and are reached through `apply`.
- The log is process-local (in `projection!`'s `process` list: `log: Vec<(u64, Input)>`, `log_boundary: usize`).
- `rebase` with `history: None` keeps the cached history. A `history_version` arrives in T4; until then the rig and runtime always pass the history.
- The clock floor: `apply` uses `now.max(self.clock_floor())`.
- `broadcast_daily` turns only on `day > calendar_day`.
- `push_view` uses `max(calendar_now, history.now())`.
- `LinkDown` detaches only an attachment stamped with `self.replica`.
- `take_out` / `durable_pending` reuse the raw bytes encoded once per pass. This is E2a's deferred extra encode: cache `raw_core` until the next `apply`.
- `load` sets the history's `now` to the resumed calendar at once (E2a's parked "history now lags one pass").
- Runtime: on `Conflict`, first lost-commit recognition, as item 1. Then `core.rebase(row)` and write again, up to `COORDINATOR_CONFLICT_RETRIES` in a row, then `COORDINATOR_SAVE_RETRY_MS`.
  - `End::Conflict` and the second-conflict exit are deleted.
  - The re-read inside `finish` is bounded by the stop's timeout. This was deferred from E2a.
- The runtime draws `ReplicaId(random u64)` until T4 issues ids.

- [ ] **Step 1: Write the failing tests.**
  - `coordinator::a_rebase_replays_the_log_onto_the_newer_row`: core A applies inputs i1..i3 while core B, from the same row, applies j1 and commits. A's write conflicts. `A.rebase(B's row)` gives the same projection bytes and the same `out` as one core applying j1, i1, i2, i3.
  - `coordinator::a_login_message_is_never_replayed`: a `HostMsg::Login` applied, then a rebase. `take_login_out()` holds it once.
  - `coordinator::two_replicas_a_tick_apart_turn_each_day_once` (Review Focus 3): two cores alternate `apply(Maintain)` with `now` values offset by −3 ms and +3 ms across a day boundary. Each commits and rebases on the other's row. Exactly one `Calendar(d)` per day appears in the union of outputs, and `calendar_day` never decreases.
  - `coordinator::a_link_down_of_a_host_attached_elsewhere_changes_nothing`.
  - Seeded, `peer_schedule::a_rebase_at_any_round_conserves_everything`, through `sweep`, 3 hosts, one replica: at seeded rounds the rig writes a foreign no-op version under the replica (the store's row moves). Its next write conflicts and rebases. `assert_conserved_world` holds, and the claims and memo match a run without the injected versions. The handoff's two-replica world test needs the live set (T4) and the outbox (T5); it is T5's.
  - `coordinator_rt::a_conflict_rebases_instead_of_exiting` (hook `write_conflict_once`): the transition commits after one rebase, the process keeps running, `mmo_coordinator_conflicts_total` = 1.
- [ ] **Step 2:** Run each. Expected: FAIL.
- [ ] **Step 3:** Implement as above.
- [ ] **Step 4:** PASS. Then `just test-one game "--no-fail-fast"`, `just test-threads`, `just test-r2`, `just test-one server "--features test-hooks --test coordinator_rt"`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(coordinator): inputs are logged and a conflict rebases onto the newer row`.

### Task 3: Store decisions commit in the transaction of the core that decided them

**Files:**
- Create: `crates/world-store/src/coordinator_tx.rs`.
- Modify:
  - `crates/world-store/src/two_phase.rs`: `decide_cut` (:188) splits into `decide_in(tx, …)` plus the post-commit `finish_staged`; `record_cut_begin` (:341) splits into `begin_in(tx, …)`.
  - `crates/world-store/src/succession.rs`: `declare_in`, `end_landless_in` and `rollback_in` take the transaction with the coordinator row already held; `lock_coordinator_and_world` loses its epoch compare; the notes get item 4's walk.
  - `crates/world-store/src/{rows,handle,thread}.rs`: `write_coordinator_later` → `write_transition_later`; `claim_coordinator` → `bootstrap_coordinator`.
  - `crates/game/src/coordinator/{mod,world_cut,succession}.rs`: marks become `CallStamp`; `Transition.decisions`; `take_decision`, `declare_wanted`, `take_end_landless` and `rollback_wanted` feed the transition instead of the runtime.
  - `crates/server/src/coordinator_rt.rs`: `pump_cuts` (:1593) and `pump_successions` (:1750) issue no decision; the post-commit effects; `begin_recorded` deleted.
  - `crates/server/src/world_file.rs`: `ensure_coordinator_state`; `CoordinatorSource::needs_save` deleted.
  - `crates/game/tests/common/multi_rig.rs`: the rig's decisions run in its `write`.
- Test: `crates/world-store/tests/coordinator_rows.rs`, `two_phase.rs`, `succession.rs`; `crates/game/tests/{coordinator,peer_schedule}.rs`; `crates/server/tests/{roles,takeover}.rs`.

**Interfaces:**

```rust
// game
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallStamp { pub replica: ReplicaId, pub version: u64 } // version = the commit that issued it
pub enum StoreDecision {
    DecideCut(CutDecide), BeginCut { cut: CutId }, Declare(Declare), EndLandless(EndLandless),
    Rollback { from: u64 }, EndReplica(ReplicaId) /* T4 */,
}
pub enum DecisionAnswer {
    Cut { stamp: CallStamp, cut: CutId, committed: Result<bool, String> },
    Begin { cut: CutId, bound: Result<HostId, String> },
    Declared { stamp: CallStamp, outcome: Result<Declared, String> },
    Ended { stamp: CallStamp, end: EndLandless, outcome: Result<LandlessEnd, String> },
    RolledBack { stamp: CallStamp, outcome: Result<u64, String> },
    ReplicaEnded { replica: ReplicaId },
}
pub struct Transition { pub core: Vec<u8>, pub history: Option<Vec<u8>>, pub out: Vec<(Target, CoordMsg)>,
                        pub decisions: Vec<StoreDecision> }
// Input gains: Decision(DecisionAnswer)

// world-store
pub struct TransitionWrite { pub version: u64, pub core: Vec<u8>, pub history: Option<Vec<u8>>,
                             pub decisions: Vec<StoreDecision> /* + outbox fields from T5 */ }
pub struct TransitionWritten { pub version: u64, pub store_ms: u64, pub answers: Vec<DecisionAnswer>,
                               pub after_commit: Vec<AfterCommit> }
pub enum AfterCommit { FinishCut { cut: CutId, tick: Tick, committed: bool }, NotifyCut { cut: CutId, tick: Tick } }
impl StoreHandle {
    pub fn write_transition_later(&self, w: TransitionWrite) -> Receiver<Result<TransitionWritten, StoreError>>; // Err(Conflict)
    pub fn after_commit_later(&self, a: AfterCommit) -> Receiver<Result<(), StoreError>>;
    pub fn bootstrap_coordinator(&self, core: Vec<u8>, history: Vec<u8>) -> Result<Bootstrapped, StoreError>; // Written(version) | Exists
}
```

- A mark set in a transition is stamped `CallStamp { replica: self.replica, version: v + 1 }`. The core learns `v + 1` at `transition_written(v + 1)`. Until then the stamp holds `PENDING_VERSION` (`u64::MAX`), fixed up there.
- **An answer is applied only if its stamp equals the mark.** The clears in `declare_failed`, `end_failed` and `rolled_back` also check it.
- **A mark whose answer never arrived is resolved by lookup, never by re-issuing.** This covers a lost commit answer, and a mark whose replica is not live from T4 on. The lookups:
  - `DecideCut` → the existing `cut_decision` lookup;
  - `Declare` / `EndLandless` → `successions_reread` + the C1 hosts-read path;
  - `Rollback` → the generation read (`rolled_back` is conditional);
  - `BeginCut` → nothing (the run aborts at its deadline if the begin is missing).
- The cut's begin is a decision of the transition that starts the cut. `BeginCut` messages leave only after it commits.
- `WorldCuts.epoch` is set from `bootstrap_coordinator`'s row epoch, once.
- The CAS loses `AND epoch = $e`. `CoordinatorFence` leaves every coordinator path; hosts keep their own `Fence`.
- `bootstrap_coordinator` is `INSERT … (id, epoch, version, core, history) VALUES (1, 1, 1, …) ON CONFLICT DO NOTHING`. A replica that finds the row `Exists` loads it. This retires E2a's "a coordinator that dies between its claim and its first write beside saved regions is refused": there is no claim any more.
- On SQLite, a `TransitionWrite` with decisions is `StoreError::Unsupported`.

- [ ] **Step 1: Write the failing tests.**
  - Store, Postgres:
    - `coordinator_rows::a_decision_rolls_back_with_its_conflicting_transition` (Review Focus 1): a `TransitionWrite` at a stale version carrying `Declare` → `Conflict`, and no `successions` row, no dead `hosts` row, no `super_regions` move.
    - `coordinator_rows::a_refused_decision_still_commits_the_core`: a `DecideCut` for a cut with a missing part → the answer is `committed: Ok(false)` (abort recorded), and the version moved.
    - `coordinator_rows::a_decision_lock_timeout_is_its_answer_not_the_writes`: a staged part holding a predecessor's rows, a `Declare` inside the write → `Declared` is `Err(timeout)`, and the core committed.
    - `coordinator_rows::bootstrap_is_first_writer_wins`.
  - SQLite: `coordinator_rows::a_decision_on_sqlite_is_unsupported`.
  - Game:
    - `coordinator::an_answer_with_another_stamp_changes_nothing` (each of `declared`, `declare_failed`, `landless_ended`, `end_failed`, `rolled_back`).
    - `coordinator::a_mark_whose_answer_was_lost_is_looked_up_not_reissued`: after `transition_written` without answers, then a rebase, the core asks `successions_reread` and hands out no second `Declare`.
  - Seeded: `peer_schedule::a_replica_killed_after_a_decision_commits_is_recovered_by_lookup` (Review Focus 1), swept with 2 hosts:
    - The rig commits a transition carrying `Declare` (and, in a second case, `EndLandless` and `DecideCut`), drops its answers, and restarts the core from the row.
    - Assert: `assert_conserved_world`, the run settles, exactly one succession, never rolled back.
    - The existing `a_coordinator_dying_between_a_store_decision_and_its_transition_conserves_the_world` still passes, rewritten onto decisions in the write.
  - Server: `roles::a_coordinator_killed_after_the_commit_before_the_sends_loses_nothing` still passes with the cut decided inside the write.
- [ ] **Step 2:** Run each. Expected: FAIL.
- [ ] **Step 3:** Implement. Write item 4's lock walk into `succession.rs`'s notes, replacing the "coordinator's write" and "claim" paragraphs.
- [ ] **Step 4:** PASS. Then `just test-one world-store "--no-fail-fast"`, `just test-pg "--no-fail-fast -- --test-threads=4"`, `just test-one game "--no-fail-fast"`, `just test-threads`, `just test-r2`, `just test-r3`, `just test-r4`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(coordinator): a store decision commits in the transaction of the core that decided it`.

### Task 4: Replica rows, the live set, attachments by replica, the version check

**Files:**
- Create: `crates/world-store/migrations/pg/0017_coordinator_replicas.sql`, `crates/world-store/migrations/sqlite/0017_coordinator_replicas.sql`, `crates/world-store/src/replicas.rs`, `crates/game/src/coordinator/replicas.rs` (extend).
- Modify:
  - `crates/game/src/coordinator/{mod,world_cut,succession}.rs`: `is_attached` (`world_cut.rs:710`), `attached_hosts` (:699), `first_of_generation` (`mod.rs:856`), `open_run` (`succession.rs:930`).
  - `crates/server/src/coordinator_rt.rs`: register, beat, the version check; `crates/server/src/health.rs`.
  - `crates/server/tests/common/`: the `Resumed` / attach helpers move here from `tests/{coordinator_rt,roles}.rs` (E2a deferred).
- Test: `crates/world-store/tests/{replicas,schema}.rs`, `crates/game/tests/coordinator.rs`, `crates/server/tests/roles.rs`.

**Interfaces:**

```sql
-- both engines (SQLite INTEGER / BLOB; seq INTEGER PRIMARY KEY AUTOINCREMENT)
CREATE TABLE coordinator_replicas (replica BIGINT PRIMARY KEY, seen_ms BIGINT NOT NULL,
  state INTEGER NOT NULL, addr TEXT NOT NULL, registered_utc BIGINT NOT NULL);
CREATE TABLE outbox (seq BIGSERIAL PRIMARY KEY, version BIGINT NOT NULL, host BIGINT NOT NULL,
  boot BIGINT NOT NULL, msg BYTEA NOT NULL);
CREATE INDEX outbox_host ON outbox (host, seq);
ALTER TABLE coordinator ADD COLUMN next_replica BIGINT NOT NULL DEFAULT 1;
ALTER TABLE coordinator ADD COLUMN history_version BIGINT NOT NULL DEFAULT 0;
```

```rust
// world-store
pub struct ReplicaRegistered { pub replica: ReplicaId, pub alone: bool }
pub struct ReplicaRow { pub replica: ReplicaId, pub seen_ms: u64, pub live: bool, pub addr: String }
impl StoreHandle {
    /// One transaction: `next_replica` issued (`UPDATE coordinator … RETURNING`), the row inserted live;
    /// `alone` when no other live row was beaten within REPLICA_LEASE_MS (store clock) — those are marked dead;
    /// dead rows older than a day pruned.
    pub fn register_replica(&self, addr: &str) -> Result<ReplicaRegistered, StoreError>;
    pub fn beat_replica(&self, me: ReplicaId) -> Result<bool /* still live */, StoreError>; // own connection, 1 s lock_timeout
    pub fn replicas_later(&self) -> Receiver<Result<Vec<ReplicaRow>, StoreError>>;
    pub fn row_version_later(&self) -> Receiver<Result<u64, StoreError>>;
    pub fn load_coordinator_since(&self, history_version: u64) -> Result<Option<StoredCoordinator>, StoreError>; // history only if newer
}
// game
pub const REPLICA_LEASE_MS: u64 = 10_000;
pub const REPLICA_BEAT_MS: u64 = 2_000;
// Input gains: ReplicaJoined { me: ReplicaId, alone: bool }, ReplicaLapsed(ReplicaId)
impl Coordinator {
    pub fn replicas_live(&self) -> &BTreeSet<ReplicaId>;
    pub fn ticker(&self) -> Option<ReplicaId>;
    pub fn hosts_attached_to(&self, r: ReplicaId) -> BTreeSet<HostId>;
}
pub struct ReplicaWatch { /* monotonic spans per replica, as LeaseWatch */ }
impl ReplicaWatch { pub fn observe(&mut self, rows: &[ReplicaRow], mono_ms: u64); pub fn lapsed(&self, now_mono: u64) -> Vec<ReplicaId>; }
```

- An attachment counts iff its replica is in `replicas` (durable).
  - `first_of_generation` detaches attachments of every replica.
  - `open_run` orders and boots go to every attached host; the outbox (T5) carries those linked elsewhere. Until T5, with one replica, that is everyone.
- `ReplicaLapsed(r)` also carries `StoreDecision::EndReplica(r)`.
- A replica whose rebased core lacks its own id exits `EXIT_FENCED`, writing nothing. So does a `beat_replica` answering `false`.
- Beats come from the runtime's main loop, as host heartbeats do.
- A replica's `/readyz` is 200 once its `ReplicaJoined` transition committed.
- Metrics: `mmo_coordinator_replicas_live` (gauge, from the core) and `mmo_coordinator_is_ticker`.
- **The version check** (departs 5): with `replicas_live().len() > 1`, `take_out`'s free outputs leave only after `row_version_later` answers this cache's version; otherwise rebase first.
- On SQLite, `register_replica` works; `beat_replica` / `replicas_later` are never called.

- [ ] **Step 1: Write the failing tests.**
  - `replicas::ids_are_never_reused`.
  - `replicas::a_registration_beside_a_live_replica_is_not_alone`.
  - `replicas::a_registration_after_every_replica_lapsed_is_alone_and_marks_them_dead`.
  - `replicas::a_beat_of_an_ended_replica_answers_not_live`.
  - `schema::` `EXPECTED`. Both engines where applicable.
  - `coordinator::an_attachment_of_a_lapsed_replica_counts_detached`.
  - `coordinator::a_replica_missing_from_its_own_rebased_core_writes_nothing`.
  - `coordinator::free_outputs_wait_for_the_version_check_with_two_replicas`.
  - `roles::every_replica_down_then_one_back_resumes_from_the_row` (Review Focus 5): coordinator replicas A and B, two `claim` hosts, a player.
    - Kill both replicas, start C. C registers `alone`, and its first transition drops A and B.
    - Both hosts re-attach to C, the next cut commits, the generation is unchanged, and the player's claim is intact.
  - `roles::a_lapsed_replica_is_ended_and_exits_fenced`: freeze replica A's main loop (hook `hold_main_loop`) past `REPLICA_LEASE_MS`. B ends it; A, released, exits 3; A's hosts re-attach to B.
- [ ] **Step 2:** Run each. Expected: FAIL.
- [ ] **Step 3:** Implement. Add the `coordinator_replicas` walk (register: `coordinator` row first for `next_replica`, then the inserts; beat; end) to the notes.
- [ ] **Step 4:** PASS. Then the world-store suites (both), `just test-one game "--no-fail-fast"`, `just test-r2`, `just test-r3`, `just test-one server "--features test-hooks --test coordinator_rt"`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(coordinator): replicas register and beat; attachments count while their replica lives`.

### Task 5: The outbox, the row notice, and a cache never older than what it relays

**Files:**
- Modify:
  - `crates/game/src/coordinator/{replicas,mod}.rs`: `route_out`.
  - `crates/world-store/src/{coordinator_tx,listen,handle,thread}.rs`.
  - `crates/server/src/coordinator_rt.rs`: relay, notice handling.
  - `crates/game/src/coordinator/world_cut.rs`: `answer_cut` (:753); an unknown future cut → refresh.
- Test: `crates/world-store/tests/coordinator_rows.rs`, `crates/game/tests/{coordinator,peer_schedule}.rs`, `crates/server/tests/roles.rs`.

**Interfaces:**

```rust
// game
pub struct OutboxMsg { pub host: HostId, pub boot: u64, pub msg: CoordMsg }
// Transition gains: pub outbox: Vec<OutboxMsg>, pub attached_now: Vec<HostId> (hosts whose Attach it applied)
impl Coordinator {
    /// `out` split by attachment: this replica's hosts stay direct; another live replica's go to the
    /// outbox; a host attached to no live replica is dropped (as a detached host's today). `Target::Every` fans out.
    fn route_out(&mut self);
    /// True when a message for `cut` arrived that this cache cannot know yet (cut id above `cuts.next`).
    pub fn take_refresh_wanted(&mut self) -> bool;
}
// world-store
pub struct OutboxRow { pub seq: u64, pub version: u64, pub host: HostId, pub boot: u64, pub msg: Vec<u8> }
// TransitionWrite gains: outbox: Vec<(HostId, u64, Vec<u8>)>, take_for: Vec<(HostId, u64)>, discard_for: Vec<HostId>
// TransitionWritten gains: taken: Vec<OutboxRow>
impl StoreHandle {
    pub fn take_outbox_later(&self, for_hosts: Vec<(HostId, u64)>) -> Receiver<Result<Vec<OutboxRow>, StoreError>>;
    pub fn listen_channel(&self, channel: &'static str, mode: NotifyMode) -> Receiver<Notice>; // Notice::Payload(String) | Resync
}
pub const COORDINATOR_ROW_CHANNEL: &str = "coordinator_row";
pub const COORDINATOR_POLL_MS: u64 = 1_000;
```

- `listen.rs` is generalised: `CutNotice` stays for the terrain path, built on `listen_channel`.
- On a notice of version `> self.version`, or on `take_refresh_wanted`, the runtime does `load_coordinator_since` and `rebase`, then `take_outbox_later`.
- A taken row whose `version` exceeds the cache's is held until a rebase reaches it, then sent (departs 6).
- The message bytes are `game::encode_packet` of the `CoordMsg`, never raw bincode.
- The outbox take, the write and the version read are one store call at a time, answered in order (item 5).

- [ ] **Step 1: Write the failing tests.**
  - `coordinator_rows::outbox_rows_older_than_an_attach_are_discarded` (Review Focus 2).
  - `coordinator_rows::a_write_takes_only_its_own_hosts_rows_in_seq_order`.
  - `coordinator_rows::a_conflicting_write_inserts_no_outbox_row`.
  - `coordinator::every_is_fanned_out_per_attached_host`.
  - `coordinator::a_host_of_no_live_replica_gets_nothing`.
  - `coordinator::a_cut_answer_for_an_unknown_future_cut_asks_a_refresh_not_a_lookup`.
  - Seeded (handoff §4.1's test), `peer_schedule::two_replicas_replay_every_conflict_into_the_one_core_world`, through `sweep`, 3 hosts, two replicas:
    - The rig links hosts 0 and 1 to replica 0 and host 2 to replica 1. Each round each replica applies its hosts' messages and writes; conflicts rebase.
    - The reference is one core fed the same inputs in commit order, with its replica id set per input (`#[doc(hidden)] set_replica_for_test`).
    - Equal: the world (`assert_conserved_world`), the memo, the claims, and the multiset of outputs per host.
    - Timed work runs on replica 0 only; T6 makes that the product's rule.
  - Seeded: `peer_schedule::a_host_moving_between_replicas_sees_commit_order` (Review Focus 2), swept, 3 hosts, 2 replicas:
    - At seeded rounds a host's link moves to the other replica (down one round, then `Attach` there). Cuts and crossings run throughout.
    - Assert per host: the messages it applied are in non-decreasing commit version.
    - Assert: none older than its latest `Attached` epoch, `assert_conserved_world`, and every cut decided once.
  - `roles::a_cut_spans_hosts_on_two_replicas`: hosts h1 → A and h2 → B, `claim`. Cuts commit, `BeginCut` / `CutDone` reach h2 through the outbox, and a crossing h1 → h2 is conserved.
- [ ] **Step 2:** Run each. Expected: FAIL.
- [ ] **Step 3:** Implement. Add the `outbox` statements to the lock notes: item 4, steps 3–5, plus `take_outbox` (`DELETE … RETURNING`, rows only, no other lock).
- [ ] **Step 4:** PASS. Then `just test-pg "--no-fail-fast -- --test-threads=4"`, `just test-one game "--no-fail-fast"`, `just test-threads`, `just test-r2`, `just test-r3`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(coordinator): messages for a host linked to another replica go through the outbox, in commit order`.

### Task 6: The ticker alone does timed and lease work

**Files:**
- Modify:
  - `crates/game/src/coordinator/{mod,world_cut,succession}.rs`: `maintain`, `observe_leases` (`succession.rs:1113`), `successions_open` (:437), `forget_absent_hosts` (`world_cut.rs:352`), the land lookups, `take_successions_reread` (:645).
  - `crates/server/src/coordinator_rt.rs`: `feed_cuts` (:1514), `pump_successions` (:1750), the pass.
- Test: `crates/game/tests/{coordinator,peer_schedule}.rs`, `crates/server/tests/{roles,takeover}.rs`.

**Interfaces:**

```rust
// Input gains (ticker only, never logged for replay — `apply` refuses them on a non-ticker and does not log them):
//   HostsRead { rows: Vec<HostSeen>, next_host_id: u64, issued_mono: u64, received_mono: u64, world_tick: Tick, draining: … }
//   SuccessionsOpen { list: Vec<Succession>, issued_at_version: u64 }
//   LandOwners { … }
impl Coordinator { pub fn is_ticker(&self) -> bool; }
// SuccessionRun gains `opened_at: u64` (durable: the version that opened it)
```

- `rebase` drops logged `Maintain` entries when the rebased core's ticker is not this replica.
- `observe_leases`' forget: only ids `< next_host_id` that no live replica's attachment holds (audit H7).
- `successions_open` abandons only runs with `opened_at <= issued_at_version` (audit H5).
- "A read is owed" (`known` / `reread`) becomes process-local. A loaded or rebased ticker owes one read.
- A non-ticker skips the hosts read entirely.
- `LeaseWatch` lives on whichever replica is the ticker; a new ticker counts from zero.

- [ ] **Step 1: Write the failing tests.**
  - `coordinator::a_non_ticker_never_hands_out_timed_work`.
  - `coordinator::a_reread_abandons_only_runs_opened_before_it`.
  - `coordinator::a_hosts_read_forgets_no_host_attached_to_another_replica`.
  - `coordinator::a_rebase_that_loses_the_ticker_drops_its_maintain`.
  - Seeded: `peer_schedule::the_ticker_killed_mid_cut_hands_over_without_a_double_declaration` (Review Focus 4), swept, 3 hosts, 2 replicas:
    - At a seeded round inside a cut, replica 0 (the ticker) dies. At another round a landed host dies.
    - Replica 1 ends replica 0 after the rig's replica lease and becomes the ticker.
    - Assert: one succession, never rolled back, `assert_conserved_world`, the run settles.
  - `takeover::the_ticker_killed_mid_cut_hands_over_without_a_double_declaration`: the same with real processes. Kill replica A during a cut and a host 2 s later. B declares once, the generation is unchanged, and the slime is counted once.
- [ ] **Step 2:** Run each. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then `just test-one game "--no-fail-fast"`, `just test-threads`, `just test-r2`, `just test-r3`, `just test-r4`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(coordinator): one ticker replica runs the timed and lease work`.

### Task 7: Links resolve every address; a host rotates among replicas

**Files:**
- Modify: `crates/svc-link/src/{target,lib}.rs` (`resolve` :29, `dial_loop` :606); `crates/server/src/roles.rs` (`QuicWorldLink::connect` :295); `justfile` (`coordinator`, `dev-r3`: a `coordinators=` knob, one shared `VOXEL_COORDINATOR_CERT_HASH_FILE`); `scripts/dev_r3.sh`.
- Test: `crates/svc-link/tests/` (or the crate's `it`), `crates/server/tests/roles.rs`.

**Interfaces:**

```rust
impl LinkTarget {
    /// Every address: each comma-separated part, each name's every DNS answer, in answer order.
    pub async fn resolve_all(&self) -> io::Result<Vec<SocketAddr>>;
}
```

- `dial_loop` resolves all addresses at every attempt and tries them in turn, starting after the address that last failed.
- The pin stays one per dialer. All replicas share one identity: dev uses one cert file, and k8s uses the module's one Secret (T10).
- The host itself changes nothing: a re-`Attach` on another replica is a re-`Attach`.

- [ ] **Step 1:** Write `target::a_comma_list_resolves_every_part`, `lib::the_dial_rotates_to_the_next_address_when_one_refuses`, and `roles::a_host_whose_replica_dies_reattaches_to_another` (replicas A and B on two ports, one cert file; kill A; the host is `Attached` on B within `LINK_RETRY_MAX_MS` + one pass, and its next `Request` is answered once).
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement. Update the justfile recipes, the `dev_r3.sh` header and `just --list` text in the same change.
- [ ] **Step 4:** PASS. Then `just test-one svc-link "--no-fail-fast"`, `just test-r2`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(svc-link): a link target names every replica; a host dials them in turn`.

### Task 8: The login service holds a link per replica; a replica without a gate says so

**Files:**
- Modify: `crates/login-api/src/lib.rs` (`HostToLogin` :170: append `NoGate`); `crates/svc-link/src/lib.rs` (`LINK_PROTOCOL_VERSION = 9`); `crates/login/src/host_link.rs` (`HostLinks`, `HostLink`, `link()` ~:262, `spawn()` ~:305, the kick sweeper); `crates/server/src/coordinator_rt.rs` (`drain_login` :1454); `crates/game/src/coordinator/mod.rs` (`gate_host` :645 → per replica); `crates/server/src/login_link.rs`.
- Test: `crates/login/tests/`, `crates/server/tests/login_link.rs`.

**Interfaces:**

```rust
// login-api, appended
HostToLogin::NoGate { player_token: String, session_epoch: u64 }
// game
impl Coordinator { pub fn gate_host_here(&self) -> Option<HostId>; } // lowest host attached to THIS replica, live, taking logins
// login
struct WorldLinks { addrs: Vec<SocketAddr>, links: BTreeMap<SocketAddr, Arc<HostLink>>, waiters: shared by (token, epoch) }
```

- `HostLinks.links` becomes `BTreeMap<world, WorldLinks>`, re-resolved at every join and every sweep: new addresses are dialled and gone ones retired.
- `spawn()` tries each up link in turn. `NoGate` → next link. Every link `NoGate` → `Timeout` (503).
- A kick goes on one link; `NoGate` → next.
- Held `SessionEnded`s stay per replica.

- [ ] **Step 1:** Write:
  - `login_link::an_order_to_a_replica_without_a_gate_moves_to_the_next`;
  - `login_link::acks_match_on_any_link`;
  - `login_link::every_replica_without_a_gate_is_a_503`;
  - `host_link::a_new_replica_address_is_dialled_and_a_gone_one_retired` (all `login_link` tests with `--features test-hooks`).
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then `just test-one login "--no-fail-fast"`, `just test-one server "--features test-hooks --test login_link -- --include-ignored"`, `just test-r2`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(login): a link per coordinator replica; a replica with no gate host says so (link protocol 9)`.

### Task 9: A host is Ready once attached

**Files:**
- Modify: `crates/server/src/health.rs` (`readyz` :402, `set_registered` :121; add `set_attached(bool, mono_ms)`); `crates/server/src/lib.rs` (the host main loop sets it from `WorldManager::world_attached()`, `world_link.rs:270`); `scripts/twin_check.sh` and `justfile` `world-start` (they wait for host readiness; nothing to change if they already wait after the coordinator is up — verify and record).
- Test: `crates/server/tests/roles.rs`, `crates/server/src/health.rs` unit tests.

**Interfaces:** `/readyz` = 200 iff registered ∧ (attached ∨ attached within `HOST_LEASE_MS`); 503 body `attaching` otherwise. A drained host stays `drained`. Role `all` is attached at once.

- [ ] **Step 1:** Write `health::a_registered_host_is_unready_until_attached`, `health::a_detached_host_stays_ready_for_one_lease`, `roles::a_host_is_ready_only_after_its_attach_is_acknowledged`.
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then `just test-r2`, `just smoke`. Then the early twin (handoff §7): `just twin-down && just twin-up && just twin-images && just twin-apply && just twin-check` at ONE replica. Record the result and the write counters in the ledger.
- [ ] **Step 5:** Commit: `feat(host): a host is Ready once a coordinator replica acknowledged its attach`.

### Task 10: Two replicas on the twin and orion — module, recipes, twin phases, alerts

**Files:**
- Modify:
  - `deploy/modules/world/coordinator.tf`: `replicas = var.coordinator_replicas` (:15); add `kubernetes_pod_disruption_budget_v1 "coordinator"` with `min_available = 1`; the header text.
  - `deploy/modules/world/variables.tf`: `coordinator_replicas`, default 2. Dawid's hunks are there: filtered staging.
  - `deploy/envs/{local,orion}/world/main.tf`, only if they pin it. They hold Dawid's hunks.
  - `justfile`: `world-stop` (:2220, reads and restores the count), `world-start` (:2331, scales to the module's count and waits for every replica's `/readyz`, not `rollout status`), `world-status` (lists replicas, the ticker from metrics).
  - `scripts/world_update.sh`: scales every replica together; the preflight reads the count.
  - `scripts/twin_check.sh`: the header list, and the new phases.
  - `deploy/modules/monitoring/values/kube-prometheus-stack.yaml.tftpl`: the `world:` group.
- Test: `just twin-check`.

**Interfaces:**
- The count to restore at `world-start` is read from the Deployment's annotation `errantry/replicas`, which `world-stop` writes before scaling to 0. If it is absent, the module default applies.
- **Twin phase 7 becomes "kill one replica mid-play":**
  - Delete the ticker's pod. Logins (`world-register`'s join) and cuts continue: `committed_above` moves within 90 s.
  - `mmo_world_generation` is unchanged, and both hosts log `world coordinator link up` on the surviving replica.
- **New phase 7b, "kill both, restart":** scale to 0, then to 2. Every host re-attaches, a cut commits, and the generation is unchanged.
- **Alerts:**
  - `WorldCoordinatorReplicasDown`: `sum by (namespace) (mmo_registered{role="coordinator"}) < on (namespace) kube_deployment_spec_replicas{deployment="coordinator"}`, `for: 5m`, warning.
  - `WorldCoordinatorDown` keeps firing when none is registered.
  - `WorldCoordinatorConflicts`: `rate(mmo_coordinator_conflicts_total[10m]) > 0.5`, `for: 15m`, warning; the description says to split the row only after measuring.
  - `WorldCoordinatorNoTicker`: `max by (namespace) (mmo_coordinator_is_ticker) == 0`, `for: 2m`, critical.

- [ ] **Step 1:** Implement the module, the recipes and the alerts. Run `tofu fmt -check` and `tofu validate` in `deploy/envs/local/world` and `deploy/envs/orion/world` (no apply on orion), `bash -n scripts/twin_check.sh scripts/world_update.sh`, and `just --list`.
- [ ] **Step 2:** `just twin-down && just twin-up && just twin-images && just twin-apply && just twin-check` with 2 replicas. Expected: PASS, every phase. Record the transcript path, the conflicts per window and the transitions per window in the ledger.
- [ ] **Step 3:** Commit (filtered staging for `variables.tf`, the env roots and the justfile): `feat(world): two coordinator replicas with a disruption budget; recipes, twin phases and alerts`.

### Task 11: The world panel — hosts by label, replicas

**Precondition:** `git status --short crates/panel deploy/modules/panel deploy/modules/world/panel.tf deploy/envs/local/panel deploy/envs/orion/panel` shows nothing untracked. If anything is untracked, STOP, and ask Dawid to commit the panel. Do not edit untracked work.

**Files:** `crates/panel/src/ops.rs` (start :170-210, stop :243-340); the panel's views of the coordinator; `deploy/modules/panel` only if its RBAC needs `poddisruptionbudgets`.

**Interfaces:** `ops.rs` mirrors `world-start` / `world-stop` step for step as Task 10 left them:
- per-host Deployments by label `app.kubernetes.io/component=host`;
- drained hosts not waited for;
- the coordinator scaled to the recorded replica count, every replica's `/readyz` waited for;
- the views list each replica (pod, `/readyz`, ticker).

- [ ] **Step 1:** Write the panel's tests for the new start and stop sequences, in the crate's existing test style (the request sequence against a fake API server).
- [ ] **Step 2:** Run `just test-one panel "--no-fail-fast"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then `just twin-panel` start/stop by hand on the twin (allowed), and record the result.
- [ ] **Step 5:** Commit: `feat(panel): hosts by label and coordinator replicas, as the recipes do`.

### Task 12: Documentation and closing

**Files:** `CLAUDE.md` (Dawid's hunks: filtered staging), `TODO.md`, this plan's "As built".

- [ ] **Step 1: `CLAUDE.md`**
  - "Server / store": replicas; the input log and rebase; decisions in the core's transaction; the live set and the ticker; the outbox; the version check; migration `0017` (re-forge).
  - "Link rules": the comma or DNS target, the login's links per replica, `NoGate`, `LINK_PROTOCOL_VERSION` 9.
  - The deployment chain: N replicas, the PDB, `world-stop` / `world-start` / the release scaling every replica.
  - Replace "the coordinator down means no cuts, no takeover and no logins": that now means every replica down.
- [ ] **Step 2: `TODO.md`**
  - An "Elastic 2b" block: built; hand checks (`dev-r3` with two coordinators, a twin with a player); the live measurement with players still owed before any row split.
  - Strike, with the date, the E2a limits this removes.
  - Keep what E2b leaves (handoff §5).
- [ ] **Step 3:** Write "As built": commits, rulings, plan defects, final suites. Run each touched crate's whole non-client suite once, plus `test-threads`, `test-r2`, `test-r3`, `test-r4`, `determinism-wasm`, `smoke`, `web-check`.
- [ ] **Step 4:** Commit: `docs: Elastic 2b — coordinator replicas over one row, as built`.

## Not in this plan

- Splitting `core` / `history` into narrower rows. That waits for the live measurement with players; T10's alert watches conflicts.
- Nested successions (decision 9's residual), E1's "E2 leaves" list, `twin_check.sh` `row_land` blind spots, the memo keeping a pruned host's entries, the `go-nogo-r3` window.
- A replica-to-replica link (§4.4: the outbox is the only cross-replica channel).
- E3–E6.

## Self-review notes

- **Handoff → tasks:**

  | Handoff item | Covered by |
  |---|---|
  | §4.1 | T2; departs 3; item 3's table |
  | §4.2 | T3 (re-ruled, departs 1; item 4's walk) |
  | §4.3 | T4 (`ReplicaId` replaces `ProcessBoot`, departs 4) |
  | §4.4 | T5 |
  | §4.5 | T6 (re-ruled, departs 2) |
  | §4.6 hosts | T7 |
  | §4.6 login | T8 |
  | §4.7 | T9 |
  | §4.8 | T10 |
  | §4.9 | T11 |
  | §4.10 | T1 |

  §5 "takes":

  | Item | Covered by |
  |---|---|
  | `finish` bound, two-lost-answers | T2 |
  | Extra encode | T2 |
  | `needs_save` | T3 |
  | Test helpers | T4 |
  | The claim-before-first-write refusal | T3 (retired) |
  | History `now` lag | T2 |

- **Names across tasks:**

  | Name | Defined in | Used by |
  |---|---|---|
  | `ReplicaId`, `Input`, `apply`, `rebase`, `transition_written(version)`, `COORDINATOR_CONFLICT_RETRIES` | T2 | T3–T6 |
  | `CallStamp`, `StoreDecision`, `DecisionAnswer`, `TransitionWrite` / `TransitionWritten`, `AfterCommit`, `write_transition_later`, `bootstrap_coordinator` | T3 | T4–T6 |
  | `register_replica`, `beat_replica`, `replicas_later`, `row_version_later`, `load_coordinator_since`, `REPLICA_LEASE_MS`, `ReplicaWatch`, `ticker()`, `hosts_attached_to` | T4 | T5, T6, T8, T10 |
  | `OutboxMsg`, `route_out`, `take_outbox_later`, `listen_channel`, `COORDINATOR_ROW_CHANNEL` | T5 | T6 |
  | `resolve_all` | T7 | T8 |
  | `HostToLogin::NoGate`, `gate_host_here` | T8 | — |
  | `mmo_coordinator_is_ticker`, `mmo_coordinator_replicas_live` | T4 | T10's alerts |

- **One replica plays after every task:**
  - T1–T3 change nothing a single process observes beyond the write shape.
  - T4 registers one replica.
  - T5's outbox is empty with one replica.
  - T6's ticker is the only replica.
  - T7–T9 are compatible at one address.
  - Two replicas are switched on in T10.
- **Proportion:** 12 tasks; the risk is in T2–T6, and each carries a seeded conservation test; T7–T11 are edges.

## As built (2026-11-18)

Built on `develop` from `03da3335`, executed subagent-driven; the ledger and the per-task briefs and reports are in
`.superpowers/sdd/2026-11-17-elastic-2b-coordinator-replicas/`. Every task was reviewed clean (T3, T4 and T6 after one
fix round each). **Task 11 (the world panel) is NOT done**: `crates/panel` is still untracked (2026-11-18), so the
task is blocked by the plan's own gate; its `ops.rs` must still learn `world-stop` / `world-start`'s replica count
(below), and its CLAUDE.md paragraph is unchanged. Not yet accepted: the whole-branch review and Dawid's hand checks
(TODO.md, "Elastic 2b") are pending; nothing ran live or on orion.

### Commits

Foreign `quest` commits are interleaved on `develop` and not listed.

| Task | Commits |
|---|---|
| plan | `ce9d116c` docs: Elastic 2b plan — several coordinator replicas over one row |
| T1 the rig's row | `16241666` test(rig): the seeded rig's store is a versioned row with an outbox |
| T2 input log, rebase | `5765b7d4` feat(coordinator): inputs are logged and a conflict rebases onto the newer row |
| T3 decisions in the transaction | `1b148abb` feat(coordinator): a store decision commits in the transaction of the core that decided it; `88043c07` fix(coordinator): a committed cut is announced only after its parts are finished |
| T4 replicas | `0a29aa14` feat(coordinator): replicas register and beat; attachments count while their replica lives; `e47995f8` fix(coordinator): a replica a registration ended leaves the live set; a refresh never goes back |
| T5 the outbox | `68911542` feat(coordinator): messages for a host linked to another replica go through the outbox, in commit order |
| T6 the ticker | `1352f702` feat(coordinator): one ticker replica runs the timed and lease work; `0cc7fd17` fix(coordinator): a successions read out across a rebase is stale, never applied |
| T7 links | `78fbcb12` feat(svc-link): a link target names every replica; a host dials them in turn |
| T8 login | `120ade15` feat(login): a link per coordinator replica; a replica with no gate host says so (link protocol 9) |
| T9 host readiness | `564da569` feat(host): a host is Ready once a coordinator replica acknowledged its attach; `584ff54c` fix(twin): a killed landless host's old row may already be pruned when phase 5 looks |
| T10 deployment | `9b3398ff` feat(world): two coordinator replicas with a disruption budget; recipes, twin phases and alerts |
| T10b (inserted) | `b58d2379` fix(host): forget a peer whose hosts row was pruned (absent below next_host_id) |
| T11 the world panel | — blocked (`crates/panel` untracked) |
| T12 docs | `docs: Elastic 2b — coordinator replicas over one row, as built` (CLAUDE.md, TODO.md, this section) |

### "To confirm with Dawid" — defaults taken (not answered before execution)

- Orion stays at 2 replicas on one node (`coordinator_replicas` default 2): a replica crash is survived, the node is not.
- Store decisions run inside the core's transaction (departs 1): a decision holds the row up to its `lock_timeout`.
- The ticker owns all timed and lease work (departs 2): a dead ticker stalls cuts and declarations ≤ one replica lease
  plus one read.
- `crates/panel` was not committed before Task 11 → Task 11 stopped at its gate (ruling, ledger 2026-11-18).

### Rulings

Execution-wide:
- T1 may edit `game/src/coordinator/link.rs` (the rig's store lives there; test-support types only).
- Every file with Dawid's uncommitted hunks (justfile, `deploy/modules/world/variables.tf`, the env roots' `main.tf`,
  the monitoring `tftpl`, CLAUDE.md, TODO.md) was staged by filtered patch (`-U3`, `--recount`).
- **Task 10b inserted**: the twin's strict join check found a pre-existing host bug (below); a gate host refusing every
  spawn forever after a landless host's crash + prune is a production outage, and the deployment gate cannot pass.
- **Task 11 blocked**; T12 written without the panel paragraphs (cost: one small docs follow-up).

T1: `RigCluster` (an `Arc<Mutex<…>>` of replicas, store and mailboxes) replaces `SharedCore` and stands in for the
brief's `MultiRig` fields (the inline links need shared access); `InlineWorldLink::shared_at(core, replica, host)`.

T2:
- Interim: store answers were not yet logged; a core holding an unlogged change (or a failed write that may be in the
  row) did not rebase but stopped as in E2a (`End::Conflict`) — removed by T6 except for the double-apply / no-row cases.
- `set_cut_epoch` only raises; the epoch moved to bootstrap-once in T3.
- Tests keep `c.handle / maintain / host_detached` through `tests/common::CoreInputs` (every input still goes through
  `apply`); `take_out`'s release truncates the log (those inputs changed nothing durable); `LeaveRequest` on replay is
  stamped with the replay's clock.
- Process fields are split `kept` / `reset` in `projection!`; `WorldCuts` / `SuccessionRuns` carry their `serde(skip)`
  fields across a rebase by destructuring.

T3:
- Marks are stamped `version + 1` when `take_transition` takes them (the row holds the exact stamp; a replay matches).
- `TransitionWrite` carries `replica`; `Declare` gained `generation` (the store's `StaleGeneration` check survives).
- `DecideCut` and `Rollback` are explicit hand-outs (`take_decision`, `take_rollback`), so the rig executes decisions
  in the call that commits them.
- A failed write (outcome unknown) orphans its marks → lookup; one that provably did not commit (Conflict, a refusal
  before the store) re-queues its decisions (`transition_refused`). A decision's `Err` → the cut is looked up and the
  successions re-read.
- Postgres `write_transition` takes no store-thread writer mutex (SQLite keeps it).
- Obsolete coordinator-fence tests deleted (`stale_coordinator_*`, `ending_is_fenced`, `fenced_coordinator_stops`,
  `overlapping_start_claims_before_it_loads`), replaced by `coordinator_rows::a_decision_rolls_back_with_its_conflicting_transition`.
- E2a's "a coordinator that dies between its claim and its first write" refusal is retired: there is no claim; a new
  core is bootstrapped at once (`INSERT … ON CONFLICT … WHERE core IS NULL`).
- **Fix round**: the cut notice is sent by `FinishCut` after `finish_staged` (it could reach hosts before `COMMIT
  PREPARED`); `NotifyCut` is now unqueued.

T4:
- `ReplicaJoined { alone: true }` drops only LOWER replica ids (a replayed lone join must not evict a later live
  replica) — reviewer-approved, safer than the plan's "every other id".
- Dropping a replica does not detach its hosts: their attachments stop counting (filtered by the live set) until a
  re-`Attach`, a deadline or a `hosts` read — reviewer-approved.
- `register_replica` upserts an empty `coordinator` row (a replica registers before the bootstrap); Postgres
  `state BIGINT`; `ReplicaWatch::observe` takes the read's issue time.
- The version check covers a prefix (outputs decided before it was handed out) and drains the log prefix before it.
- The multi-replica server tests live in `server/tests/hosts.rs` (`just test-r3`): only `hosts_rig` has claim-cut
  hosts with switchable links (also T5, T7, T8). Restart-modelling game tests apply `ReplicaJoined { alone: true }`.
- **Fix round**: a replica ended by another's REGISTRATION (not the watch) left the live set never → `ReplicaWatch::gone`
  (dead rows and pruned ids below the read's highest); role `all` joins `alone` when it is not a listener. A refresh
  could rebase onto an OLDER row → `Coordinator::refresh` is forward only; stale receivers dropped at a hand-out.

T5:
- A host attached to a non-live replica gets nothing (its next `Attach` reconciles); one with no attachment → direct.
  `Attached` / `Generation` always go direct. `Every` stays one direct entry plus one outbox copy per host on another
  live replica. Routing happens at `take_transition` only.
- `take_for` is sent with every transition, even at one replica (an index `DELETE` + `pg_notify`; cost unmeasured).
- Notice-, held-row- and stale-cut refreshes are deferred while the core holds `unacked`.
- In the rig, every route pass refreshes the cache when the row moved (as a NOTIFY does).
- The two-replica reference core replays commits, releases and replica 0's world ticks in commit order.

T6:
- A rebased ticker owes a successions read when the rebase dropped one it applied OR one is out (final, after the fix
  round; the brief's "every rebase" made `a_rebase_at_any_round_conserves_everything` diverge).
- `CutLookedUp` (a stored decision read back) is a LOGGED, replayed input; the generation-lookup answer stays a direct,
  unlogged call (a dropped one re-hands the conditional rollback).
- `End::Conflict` stays only for an unacked write that may be in the row and a gone / undecodable row.
- Drain marks moved into a durable `draining` set (a non-ticker reads no `hosts` rows but its gate needs the marks).
- The runtime's cut configuration survives a rebase onto a row written before it was committed.
- **Fix round**: a successions read out across a rebase could re-open a run the rebased row closed → a read is owed
  when one is out, and `read_floor` drops answers issued below it.

T7: a dropped link's next pass starts after the address that dropped (one failed attempt when that replica was fine);
dev replicas via `just coordinators=N` and `DEV_R3_REPLICAS` / `DEV_R3_LOGIN_LINK` (env-driven, CLI unchanged); the
second replica shares the first's identity (`spare_replica_with_cert`).

T8:
- The cross-replica placement goes gate → holder over the hosts' PEER links (the brief assumed the outbox); "acks on
  any link" matters for a gate that re-attaches mid-order (`acks_match_on_any_link`).
- Every link `NoGate` → `SpawnError::Timeout` (503); `orders_dropped` keeps its name (now counts `NoGate` answers).
- A failed DNS resolution keeps an unchanged row's links; a first failure is `BadLink`. Kick durability unchanged.

T9: health keeps its own monotonic clock; the roles test replicates the main loop's `set_attached(world_attached())`
line (the wiring itself is covered by the twin only).

T10:
- `WorldCoordinatorNoTicker` selects `role="coordinator"` and requires a registered replica; `WorldNoCommittedCut`,
  `WorldNoFirstCut`, `WorldHostsBelowReplicas` read across replicas (each replica counts only its own decisions; only
  the ticker refreshes `hosts_live`).
- `world-start` keeps a running coordinator's own count (the annotation only when the spec is 0).
- `world_update.sh` stops the coordinator explicitly (to 0 after the hosts, back to the count after the images); a
  re-run of the recorded release may find it at 0.
- Twin phase 7 accepts a host's link on ANY live replica and force-deletes the ticker's pod; the join check is a real
  join through the login service (`issue-refresh` in the login pod, `POST /login/v1/join`).
- The coordinator PDB exists only above one replica (login's pattern).

T10b: candidates are the keys of `peers`, `peer_endpoints`, `peer_boots`; the open-run guard covers heir, survivors,
adoptions and reports; refusals on the absent path log at info; the server test (`takeover.rs`) marks the row dead by
SQL at the kill so the old dead-row path cannot win.

### Plan defects found

- **T2 before T3**: store answers could not be logged before T3's stamps existed; T2 needed interim unlogged marks and
  an `End::Conflict` stop (gone in T6).
- **T3's two post-commit effects** (`FinishCut`, `NotifyCut`) ran as separate store tasks: listeners could hear of a
  cut before its parts were committed (and fall back to the 30 s poll). Fixed in `88043c07`.
- **T4**: the plan's lone join "drops every other id" would evict a later live replica on replay; a replica ended by a
  registration (not the watch) was never dropped from the live set; a refresh could go backwards. Fixed in T4 /
  `e47995f8`. The core needed `register_replica` to create the row before the bootstrap.
- **T5**: without a NOTIFY-style refresh in the rig no cut ever began on two replicas; `with_replicas` had to commit
  replica 0 first.
- **T6**: "a rebased ticker owes a read" (every rebase) diverged from the one-core world; narrowed, then the review
  found the stale read across a rebase. A T5 runtime busy loop surfaced: notices left in their channel while a write
  waited to retry woke every wait (~1M `Maintain`s a second, `test-r4` OOM) — `pump_notices` now runs every pass.
- **T8**: the brief routed cross-replica placement through the outbox; it goes over peer links.
- **T9**: twin phase 5 assumed the killed landless host's old row is `dead`; a new pod's registration may already have
  pruned it (`584ff54c`, tightened in T10 to "dead or absent").
- **T10**: three existing alerts read one pod's counters (false fire / masked at 2 replicas). The twin's strict join
  check found a PRE-EXISTING host bug: a force-killed host's dead row pruned by another registration before the gate
  host forgot it → `peers_synced` waited for it forever → every spawn refused, every join 503 → Task 10b.
- The core format changed within `COORDINATOR_STATE_VERSION` 5 in T3, T4 and T6 (the plan bumps it once, in T2): a dev
  row written by a mid-plan build reads as `Corrupt` or misdecodes silently (bincode allows trailing bytes) — re-forge.

### Deferred minors (for the final review)

- **Flagged**: T8 — `HostToLogin::NoGate` carries no kind (spawn and kick share the `(token, epoch)` key space; a racing
  kick / join cross-feed costs at most one extra resend) — cheap to change only while protocol 9 is unreleased.
- **Flagged (stuck-world shape)**: T10b — absent ids are not added to `dead_seen` (`note_dead_rows`): a landless host
  with unsaved state traffic to this host, ended AND pruned before this host's read → the journal refuses the forget
  (correct) but every cut part is refused for `unsaved_traffic` forever: the world stops saving instead of rolling back.
- T1: `RigReplica.hosts` / outbox scaffolding (used from T5); `shared_at` index panic (named since T2).
- T2: hand-placed `unlogged` marks (deleted in T6); a Conflict-answered write must not enter `unacked` (fixed in T3).
- T3: `world_file.rs` ~:446 replace path hard-codes cut epoch 1 (use row epoch + 1); bincode trailing bytes (above);
  stale docs / dead code (`thread.rs` ~:788 doc on the wrong fn, ~:460 comment, `operator.rs` ~:33 broken intra-doc
  link, `handle.rs` ~:201 names `write_coordinator_later`, `coordinator_rt.rs` ~:851
  `WORLD_CUT_DECISION_SLOW_RETRY_MS` unused); `DecideAlone` helpers `pub #[doc(hidden)]` (a test-support feature?); two
  `warn!` lines log before the outer commit; no runtime test that a stale second coordinator process loses the CAS and
  rebases; no seeded lost-rollback-answer case; `tests/coordinator.rs` ~:4452 unused `mut`; the cut-notice regression
  test pins the ordering structurally; `AfterCommit::NotifyCut` dead; post-commit effects queued at an orderly stop are
  not run.
- T4: accessors inserted between existing doc comments and fns (`succession.rs` ~797–814, `world_cut.rs` ~833–844); dead
  `now` in `pump_version_check`; a stale `version_rx` after an intervening write costs an extra refresh;
  `a_lapsed_replica_is_ended_and_exits_fenced` does not assert "writes nothing" after release;
  `every_replica_down…` injects the claim into the core; comment the `unlogged_committed` prefix-release invariant;
  `gone` never drops an absent id above the read's highest; dead code in `a_refresh_onto_an_older_row_changes_nothing`;
  `refresh_onto` duplicates `pump_version_check`'s guard.
- T5: one-replica cost (every transition `pg_notify` + outbox `DELETE … RETURNING`, one more listener connection —
  consider skipping the take at one live replica with an empty relay); the coordinator row listener uses
  `TERRAIN_LISTENER_APP` as `application_name`; a malformed `coordinator_row` payload is ignored at debug rather than
  `Resync`; the reference core does not replay replica 0's rare unlogged hand-outs.
- T6: the generation-lookup answer is not ticker-gated on a non-ticker; no test that a new ticker waits a full lease for
  a non-vouched lapse; a needless refresh read after an own commit when the NOTIFY beats the write answer; a fresh drain
  mark dropped by a rebase vanishes until the next read; `hosts_live` metric only updated on the ticker.
- T7: "answered once" asserted only as a grant on B; `resolve_all` silently skips an unresolvable part among good ones;
  a dropped-but-healthy replica costs one failed attempt; no unit test for rotate-after-Up-then-drop; mixed v4/v6
  targets bind the first family only.
- T8: a NoGate-only sweeper wake resends every pending kick (kicks² × links per 2 s); a partly failed comma-list
  resolution retires that replica's link; `login_host_link_up{world}` is "any link up"; joins ask replicas in row order
  (+1 RTT when the first has no gate).
- T9: `ATTACH_GRACE_MS` not const-asserted equal to `HOST_LEASE_MS`; Ready does not decay on a stalled loop while
  attached (`/livez` covers it); the real wiring (`require_attach` + the loop's `set_attached`) is covered by the twin only.
- T10: `WorldCoordinatorReplicasDown`'s one-to-one match can error on duplicate KSM series and fires while no replica is
  registered (forge wait); `WorldNoCommittedCut` drops to 0 when the last committing replica dies (misleading
  `NoFirstCut` wording); twin phase 2b reads the PDB status once; phase 7 skips the re-link check silently when
  `link_ip` cannot parse; the fallback `2` is hard-coded in `world-start` and `world_update.sh`; `world_update.sh`'s real
  path ran only as `--dry-run`; the alerts were only `promtool`-checked.
- T10b: the takeover test's dead mark and prune are separate steps; the game test `absent_peer_forgotten` places no
  spawn; the run guard cannot go RED alone (the journal refuses first); a cleanly-left peer pruned before this host saw
  its clean row skips `note_clean_rows` / `note_left_peers`.

### The twin

- T9, ONE replica: run 1 FAIL at phase 5 (the script's `dead` assumption), run 2 PASS in 465 s
  (`target/twin/check-20261118T110851Z/`); conflicts 0, write failures 0 (`window=5.1s transitions=5 core_bytes=1552
  … version=74`).
- T10, TWO replicas: run 1 FAIL phase 7 (script: the token line); run 2 FAIL phase 7 (every join 503 — the host bug);
  run 3 with the join assertion temporarily record-only PASS in 652 s (`check-20261118T115325Z`).
- T10b, TWO replicas, strict join check: **PASS in 481 s** (`target/twin/check-20261118T124846Z/`), every phase 1–11 incl.
  7 (ticker's pod force-killed mid-play: new ticker after 12 s, cuts 1 s later, both host links on a live replica, join
  200 before and after, back to 2, generation 0) and 7b (0 → 2). Windows: transitions ≤ 6 per window, conflicts 0–3 per
  replica (≤ 1 per window), write failures 0; the longest-lived replica 111 transitions over 49 windows, 0 conflicts.
  In this run the gate forgot the killed host by the dead-row path (timing); the absent path is pinned by
  `takeover::a_pruned_landless_peer_is_forgotten_and_the_gate_places_again`.
- Not exercised: a player, a host owning land, a real node drain, `world_update.sh` for real, the alerts on live series.

### Final suites (T12, at `b58d2379` + docs; sequential, dev Postgres via `just pg-up`)

| Suite | Result |
|---|---|
| `just test-one game "--no-fail-fast"` | lib 159 passed, 3 failed (the known `lod::tests` reds); `it` 953 passed / 319 ignored; hash_mode 7, state_hash_check 1, voxel_body 20, wasm_determinism 7 — ok |
| `just test-one world-store "--no-fail-fast"` (SQLite) | unit 6; `it` 150 passed, 43 ignored (`codec::golden_blobs_decode` passed this time) |
| `just test-pg "--no-fail-fast -- --test-threads=4"` | unit 6; `it` 150 passed, 43 ignored |
| `just test-one server "--features test-hooks --no-fail-fast"` | ok: lib 108, it 29, login_link 7, observer 3, store_world 4, dev_structures 2, player_store 2, roles 2 (the rest slow-ignored there) |
| `just test-one login "--no-fail-fast"` | lib 3, `it` 85 (4 ignored) — ok |
| `just test-one svc-link "--no-fail-fast"` | lib 14, `it` 1 (11 ignored) — ok |
| `just test-threads` | 297 + 3 + 10 ok |
| `just test-r2` | 20/20 |
| `just test-r3` | 49/49 |
| `just test-r4` | **26/27 — `a_leavers_restart_is_a_new_host_and_its_land_is_given_at_once` FAILED** (`takeover.rs:377`, "timed out waiting: the coordinator records host 0's request for host 1's land", the `until` at ~:1974 on `core.lands_wanted() == [h(1)]`); alone 3 times: FAIL, FAIL, ok (33.7 s, 33.9 s, 10.4 s). Green in T6, T8 and T10b's full runs. Not diagnosed (no code changed in T12) — for the final review: unverified guess, the land-wanted window became ticker work in T6 (a lookup taken and answered between two observations) or a real stall of the `LandWanted` → `Leave` path. The WATCH test `a_clean_leave_…` passed. |
| `just determinism-wasm` | ok |
| `just smoke` | ok (3 s) |
| `just web-check` | ok |

Client tests were not run (Dawid's hand check). Another session ran `quest/` tests concurrently during part of the run.
