# Elastic 2a — The Coordinator's State in the Store Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The world coordinator keeps no state of its own. Every decision it makes is in the world's store (PostgreSQL, or SQLite in single-player) before anything it decided leaves the process. A coordinator restart loses nothing: no 60 s save window, and no history rebuilt by guessing. A crash-looping host pod no longer rolls the world back.

**Architecture:** `game::Coordinator` stays the logic: engine-agnostic, deterministic given its inputs. Its WHOLE state becomes serializable:
- the history, including the parts rebuilt at load today;
- presence, regions, the cut run, the succession runs, the calendar anchor;
- claims and the request memo.

The server runtime runs it as a sequence of **transitions**:
1. Apply a batch of inputs (host messages, a timed step, store answers) to the state.
2. Write the state back to the `coordinator` row with a compare-and-set on a `version`.
3. Only then send the outputs.

The state is two blobs in the same row: `core` (small, written by every transition) and `history` (written only when its content changed). The process may hold only:
- its links;
- caches tagged with the `version` they were read at;
- timers whose loss only delays (the lease watch, retry pacing).

The core's clock becomes the store's clock, so a state written by one process means the same thing to the next. E2b (a plan written after this one is built) then runs N replicas over the same row.

**Tech Stack:** Rust (sqlx on SQLite and PostgreSQL, bincode + lz4, crossbeam), Python (the probe's summary).

**Spec:**
- `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md` (decisions 7 and 9; §6 "a coordinator replica dies mid-decision")
- `docs/superpowers/plans/2026-11-16-elastic-2-coordinator-planning-handoff.md` (§4.1, §4.2, §4.5, §5, §6)
- Dawid, 2026-11-16: "the coordinator should work almost like a web service: its state in Postgres, no state of its own; on SQLite in single-player too."

## Where this plan departs from the handoff (verified against the code, 2026-11-16, `a92144da`)

1. **E2 is split in two.**
   - **E2a (this plan):** the state goes into the store, with one coordinator process running.
   - **E2b (next plan):** N replicas over the same row. That plan covers:
     - fencing decisions on the row's `version` instead of a per-process epoch;
     - attachments stamped by replica;
     - an outbox for messages to hosts linked to another replica;
     - timed work taken by compare-and-set;
     - the rotating dial and standby-free links;
     - the module, the recipes and the panel.

   Every task here leaves one coordinator, and a world that plays.
2. **The state was not in the store.** `coordinator.state` is saved every `COORDINATOR_SAVE_SECS` (60 s), not per change. Only claim grants and directives wait for a save (`CoordMsg::is_gated`). Everything else lives only in memory and is rebuilt from each host's `Attach`:

   | Kind | What |
   |---|---|
   | Presence | `online_on`, `hop_of`, `online_reported`, `displaced_told` |
   | Regions | `region_host`, `incarnations` |
   | Runs | the world cut run, the succession runs' answers |
   | Calendar | the calendar anchor, `sites_key` |
   | History | `running`, `stopping`, `directives`, `bounced`, `stray_candidates`, `held_since`, `notices` (`#[serde(skip)]`), plus `History::after_load`, which turns every embodied person back into the abstract history |
3. **History stays a blob, not rows.**
   - Measured in local worlds: the whole coordinator state is 43–52 KB lz4.
   - Position reports arrive once per region per world cut (`region_runner.rs:412`), not continuously.
   - Every history rule is deterministic given (state, inputs, calendar time), with stateless rolls.
   - So read–modify–write of one blob per transition is affordable, as the handoff's "keep one row; split only on measurement" asks.
   - The one split made up front is size-driven: `history` is written only when its content changed. Advancing `now` alone does not count, so a day turn does not rewrite 50 KB.
   - Task 6 adds the counters that decide whether to split further.
4. **The memo stays inside `core`.** It is written in the same transaction as the change it answers, which is what the handoff's memo table was for. A separate table is E2b's to decide, if a replica needs to read one row without the blob.
5. **The core's clock.** `now_ms` is `started.elapsed()` of the process (`coordinator_rt.rs:1074`), so the calendar and every cut and succession deadline restart from 0 with the process.
   - It becomes the store's clock (`clock_timestamp()` / SQLite `unixepoch('subsec')`), sampled at each transaction and each `hosts` read and extrapolated in between with the process's monotonic clock.
   - Downtime is not caught up: if no transition stamped the calendar for `CALENDAR_GAP_MS`, the calendar re-anchors at its stored tick.
6. **Store decisions stay their own transactions in E2a.** `decide_cut`, `declare_succession`, `record_cut_begin`, `rollback_world` and Task 2's `end_landless_host` are already fenced, first-writer-wins transactions whose answers are fed back to the core.
   - What changes is that the core's state "waiting for decision X" is now durable. A restart finds it and resumes through the existing lookups: `cut_decision_later`, `successions_reread`.
   - E2b moves their fence from the per-process epoch to the row's `version`.
7. **§4.1 "recipients = attached when declared" does not stop the flapping pod.**
   - A restarted pod's registration lapses its old row by name, and the coordinator declares at its next `hosts` read (≤ 1 s), while the new process can attach sooner.
   - Separately, while a succession is open no second one may be declared (`Declared::SuccessionOpen`), so a dead recipient blocks the settle cut for good.
   - Re-ruled in Task 2: recipients own land.
8. **`cut_decisions` must not be pruned.** A re-attaching host looks up its `pending_cut`, and an undecided-looking cut gets an abort recorded. Only `cut_begins` is pruned (Task 5).
9. **Names:** `resolve_staged_cuts` is `resolve_prepared`; `LeaseWatch` is in `game/src/coordinator/succession.rs:115`. The legacy `history(id, bytes)` table is read once to migrate (`world_file.rs:401`) and is otherwise dead.
10. **Single-player.** Role `all` runs the same `CoordinatorRuntime` on SQLite (`start_all_in_one`, `coordinator_rt.rs:794`), so it gets the same transitions. `InlineWorldLink`'s `commit_unstored` has no non-test caller. It stays the seeded rig's in-memory store.

## Decisions settled in planning

1. **A process may hold only these, and nothing else counts as state:**
   - its links and their routes;
   - caches tagged with the row `version` they were read at (the decoded `Coordinator`);
   - observation timers whose loss only delays a decision: `LeaseWatch` (re-counted from zero, as today), store-retry pacing (`WORLD_CUT_*_RETRY_MS`), log rate limits (`memo_warned_ms`);
   - the `login_out` relay queue and the held `SessionEnded`s. These relay the login service's own state, and on a loss the login service re-sends its kicks; nothing of the world is in them.
2. **A transition:**
   - **Inputs:** every host message drained this pass, the timed step (`maintain`) and every store answer that arrived.
   - **Steps:**
     1. The cached core applies them.
     2. If the durable projection changed, it is encoded and written with `UPDATE coordinator SET core = $c, history = COALESCE($h, history), version = version + 1, saved_utc = $t WHERE id = 1 AND epoch = $e AND version = $v`.
     3. Only after the commit are the transition's outputs sent.
   - **On a failed write:**
     - 0 rows with the epoch matching → `StoreError::Conflict`. E2a has one process, so this is a bug or a second writer: reload, re-apply the same inputs, retry once, then exit `EXIT_FENCED`.
     - Epoch mismatch → `Fenced`, as today.
     - Any other error → the outputs are held, the inputs are kept, and the transition is retried (`COORDINATOR_SAVE_RETRY_MS`, 250 ms).
   - **Concurrency:** at most one transition is in flight. Inputs that arrive meanwhile wait for the next.
3. **Delivery is at most once after the commit.** This is today's guarantee, and every message heals on the host's re-`Attach`, which a coordinator restart always causes:
   - replies are replayed from the memo;
   - `CutDone` is looked up through `pending_cut`;
   - succession orders are re-sent by `succession_attach`;
   - directives and spawns are rebuilt by `reattach_regions`.

   No outbox in E2a. `gated` and `is_gated` go: every output is durable before it leaves.
4. **History v4 saves everything.** `HISTORY_FILE_VERSION = 4` serialises every field except `grid` (pure of the map). `after_load`'s blanket return of embodied people is removed: a load is no longer a restart.
   - What a restart must undo is undone where it always was: on each host's `Attach` (`attach` parks every live region the host does not list; `reattach_regions` is exact per region), and for a host that never returns, by its succession.
   - v2 and v3 decode paths are deleted: worlds are re-forged (Dawid, 2026-11-15).
5. **`CoordinatorState` v3 is the whole durable projection:**
   - history and its `content_dirty` flag;
   - claims, memo;
   - presence, `region_host`, `incarnations`, `generation_seen`;
   - the calendar anchor `(store_ms, tick)` and `calendar_seen_ms`, `calendar_day`, `sites_key`;
   - `WorldCuts` without its config;
   - `SuccessionRuns`;
   - `rolled_back_to`;
   - attachments as `BTreeMap<HostId, (boot, ProcessBoot)>`.

   `ProcessBoot` is a random `u64` drawn at process start. An attachment stamped with another process boot counts as detached, so a restart needs no "forget every link" step.
6. **The core's clock** is `StoreClock`: an offset between the store's clock and the process's monotonic clock. It is re-sampled from each transition's commit (`RETURNING` the store time) and each `hosts` read, and never moves backwards.
   - `CALENDAR_GAP_MS = 30_000`: three day turns (`DAY_MS = 10_000`), so a running coordinator always stamps within it.
7. **The two blobs** are bincode + lz4 (`lz4_flex`, as history). `history` is written only when `History::content_dirty`. Advancing `now` with no step due does not set it.
8. **Counters:** `mmo_coordinator_transitions_total`, `mmo_coordinator_written_bytes_total{part="core|history"}`, `mmo_coordinator_conflicts_total`, and the same on the periodic coordinator log line. They decide E2b's split; nothing in this plan acts on them.
9. **Recipients own land (re-ruled §4.1):**
   - The store's survivors are the dirty live hosts with a `super_regions` or `in_flight` row, plus the heirs.
   - Heir candidates are tried in order: landed ∧ takes logins, then takes logins, then attached live.
   - A lapsed landless host that is neither heir nor survivor of an open run is ended by `end_landless_host`: its row is marked dead, with no succession and no generation change, even while a run is open.
   - A lapsed landed host while a run is open is still a second failure. That residual is documented.
   - Offline `worldctl retire-host` is refused; the online vouch stays (handoff §5, §9).
10. **`LINK_PROTOCOL_VERSION` stays 7.** No link enum changes here.

## To confirm with Dawid (the plan proceeds on the defaults above)

- The residual of decision 9: a host that **claims land while a succession is open** and dies before its settle still rolls the world back.
- Refusing offline `retire-host`.
- Removing `after_load`'s blanket return (decision 4). It changes crash recovery. Task 4 proves it on the existing crash tables before anything else relies on it.

## Global Constraints

- No git worktrees; work on `develop`. Dawid commits concurrently and has uncommitted hunks in `CLAUDE.md`, `TODO.md`, `justfile`, `deploy/**` and untracked `crates/panel/`.
  - Never `git add -A`, `git stash` or `--unidiff-zero`.
  - A file holding his hunks is staged with a filtered patch: `git diff -U3`, keep your own hunks, `git apply --cached --recount`.
  - Check `git diff --cached --stat` before every commit, and confirm his hunks are still uncommitted after it.
- **Conservation beats latency; wait, never guess.** Every entity, item, player and person exists exactly once across live state and the store through any crash point. A task that changes who holds what is not done without a conservation test.
- **Fencing in the store is the only safety.** Never add a store write that skips the fence. Every coordinator write stays fenced on `coordinator.epoch`, and from Task 5 on also on `version`.
- **Migrations:** immutable, identical in shape in `migrations/pg` and `migrations/sqlite`; the next is `0016`, and `0001`–`0015` are never edited. Worlds are wiped, never migrated: the docs say "re-forge / `just pg-reset`". `tests/schema.rs` `EXPECTED` follows every column.
- **Lock walks:** every new or changed statement on `coordinator`, `hosts`, `super_regions`, `successions`, `world` or `cut_begins` gets its walk in the module notes of `world-store/src/succession.rs` (the operator's in `operator.rs`). A prepared transaction is not a waiting backend.
- The lease clock is `clock_timestamp()`. Beats come from a main loop, never a timer thread.
- **The core stays deterministic:** `game` reads no clock, no environment and no store; the runtime passes `now_ms` in. Ordered state is `BTreeMap` / `BTreeSet`. Randomness only via stateless rolls. Tunables are named consts.
- Wire enums are append-only. `LINK_PROTOCOL_VERSION` stays 7; if a task must change a link enum, that is ONE bump, recorded as a ruling.
- A new per-host `WorldManager` field is classified in `host_fields!`. A host id is never reused, and nothing is derived from its number.
- `game`, `server`, `worldgen` stay free of Bevy. rustls uses ring only.
- **Implementers never run** `just orion-*` apply or push, or `kubectl --context orion` with a write verb. Ask Dawid before client tests, live runs (`dev-r3`, `go-nogo-r3`, `just run`) and measurements. `just twin-check` is allowed.
- **Tests:**
  - Run them with `just test-one <crate> "<cargo test args>"`, filters after `--`.
  - Server targets `roles`, `coordinator_rt`, `hosts`, `takeover`, `login_link` need `--features test-hooks`.
  - PostgreSQL suites need `just pg-up` and run one at a time.
  - Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
  - `just smoke` after every Rust task.
  - Seeded scheduling lives in `crates/game/tests/peer_schedule.rs` / `common/multi_rig.rs`.
  - Real hosts over real links live in `crates/server/tests/{roles,hosts,takeover}.rs`.
- The justfile is canonical. Secrets are never printed or committed. Player-facing text says **Errantry**. Dated documents are not edited.
- A decision this plan did not make goes in the ledger `.superpowers/sdd/2026-11-16-elastic-2a-coordinator-state/progress.md` as `Ruling: what — why — cost if wrong`.
- Subagents edit with Edit / Write, one file per call. A refused command is reported, never worked around. When the tree does not build, check `git status` for foreign changes first; suites may run on a `git archive` export.

## Review Focus

1. **The coordinator dies between a transition's commit and its sends.**
   - Expected: nothing is lost or done twice. Replies come back from the memo, a cut resolves by lookup, and spawns are rebuilt at re-attach.
   - Pinned by Task 6, `a_coordinator_killed_after_the_commit_before_the_sends_loses_nothing`.
2. **A write fails (the store is unreachable, or the lock times out) while hosts keep sending.**
   - Expected: no output of the failed transition leaves. The inputs are kept and applied once when the store returns. No reply is sent twice.
   - Pinned by Task 6, `no_output_leaves_before_its_state_is_written`.
3. **The world is stopped for an hour and started again.**
   - Expected: the calendar resumes at its stored tick (no hour of history in one burst). Every cut and succession deadline counts from the restart, not from before the stop.
   - Pinned by Task 3, `downtime_is_not_caught_up`.
4. **A history reloaded between two inputs behaves as one never reloaded.**
   - Expected: the same outputs, the same log and the same state, bit for bit.
   - Pinned by Task 4, `a_history_reloaded_after_every_input_is_identical`, and Task 5, `a_core_reloaded_after_every_input_is_identical`.
5. **A host pod crash-loops beside a healthy, landed host while its first incarnation's succession is open.**
   - Expected: each landless incarnation is ended; the generation never moves; the run settles.
   - Pinned by Task 2 (seeded and `takeover.rs`).

## File structure

| Path | Responsibility after this plan |
|---|---|
| `crates/client/src/renderer/autopilot.rs`, `scripts/crossing_summary.py`, `justfile` (`go-nogo-r3`) | the probe seams from the client's own host map, re-forges each run, judges successions inside its window |
| `crates/world-store/src/succession.rs`, `operator.rs`, `ctl.rs` | survivors own land; `end_landless_host`; offline retire refused; lock notes |
| `crates/game/src/coordinator/succession.rs` | `HostSeen.landed`; heir order; a landless lapse is ended, not a second failure; pruned ids forgotten |
| `crates/game/src/coordinator/clock.rs` (new) | `StoreClock`; the calendar anchor and gap rule |
| `crates/game/src/history/{mod,persist,handover}.rs` | v4 saves every field; no blanket return at load; `content_dirty` |
| `crates/game/src/coordinator/state.rs` | `CoordinatorState` v3 = the whole durable projection; `ProcessBoot` |
| `crates/game/src/coordinator/{mod,world_cut,succession}.rs` | state held in serialisable parts; attachments stamped by process boot; no `gated` |
| `crates/world-store/migrations/{pg,sqlite}/0016_coordinator_rows.sql` (new) | `coordinator.version`, `core`, `history`; `state` dropped |
| `crates/world-store/src/{rows,handle,thread,two_phase}.rs` | `load_coordinator` / `write_coordinator` (CAS); `cut_begins` pruned at each decision |
| `crates/server/src/coordinator_rt.rs`, `world_file.rs`, `health.rs` | the transition loop; no periodic save; counters |
| `crates/game/tests/common/multi_rig.rs`, `peer_schedule.rs` | the rig's store applies the survivor rule and `end_landless_host`; restarts the core from its stored state |

---

### Task 1: `go-nogo-r3` measures what it says (handoff §4.5)

**Files:**
- Modify: `crates/client/src/renderer/autopilot.rs`:
  - `seam_shuttle` (~`:377`)
  - `pick_loaded_seam` (`:569`)
  - `pick_seam` (`:601`)
  - `rendezvous_owner` (`:693`)
- Modify: `justfile` (`go-nogo-r3`, ~`:1495-1666`)
- Modify: `scripts/crossing_summary.py` (`invalid_reasons`, `:312`)

**Interfaces:**
- `pub fn pick_loaded_seam(home: RegionCoords, loaded: &[(RegionCoords, Option<HostId>)], want_host_seam: bool) -> Option<(RegionCoords, RegionCoords)>`.
  - Uses only the client's own map: region → `ConnectionSet::ordinal_of(connection_of(region))`.
  - `pick_seam`, `rendezvous_owner` and the `VOXEL_HOSTS` read (`HOSTS_ENV`) are deleted.
  - With no seam of the wanted kind loaded, the autopilot keeps walking outward and logs `event=autopilot_seam_wait` at most once per 5 s. It never falls back to a computed owner.
- `crossing_summary.py`: a succession line makes the run INVALID only if its timestamp lies between the host run's first `event=autopilot_seam` and its `event=autopilot_done`. It uses the `SERVER_CLOCK` / `CLIENT_CLOCK` parsing of `silent_hosts`.

- [ ] **Step 1: Write the failing tests.**
  - `autopilot::tests::a_host_seam_is_where_the_map_changes_host`:
    - Map: (0,0)→h1, (1,0)→h2, (0,1)→h1.
    - With `true`: `Some(((0,0),(1,0)))`.
    - With `false`: `((0,0),(0,1))`.
  - `autopilot::tests::no_seam_is_invented_from_ids`: every region on h7, `true` → `None`.
  - `scripts/tests/`:
    - `test_succession_outside_the_window_is_not_invalid`
    - `test_succession_inside_the_window_is_invalid`
- [ ] **Step 2:** Run `python3 -I -m pytest scripts/tests -k succession`. Expected: FAIL. Then `just test-one client "--lib --no-run"`: it must compile. **Ask Dawid** before running the client tests.
- [ ] **Step 3: Implement.**
  - The autopilot system reads the `ConnectionSet` resource that `instance.rs:623` reads for `from_host=`.
  - The recipe:
    - drops and re-creates `{{go_nogo_db}}` (through the dev compose's `psql`), then forges, runs `super-region … 1` and `clock-reset`, on EVERY run;
    - drops `VOXEL_HOSTS=2` from the client env;
    - globs `host-*.log`.
- [ ] **Step 4:** The Python tests pass; `just smoke`; the client compiles.
- [ ] **Step 5:** Commit: `fix(go-nogo): the probe seams from the client's own host map, re-forges each run, judges successions in its window` (filtered staging for `justfile`).
- [ ] **Step 6:** Hand-off line, recorded in the ledger and NOT run:
  - Command: `just go-nogo-r3 1 60`.
  - A valid run prints a table with a `thread` and a `host` group of at least 20 crossings each, no `INVALID:` line, and `GO` / `NO-GO` with `correction_max` / `corrections` p95.
  - Then pooled `0|1|2|5` against 2026-11-13.

### Task 2: The crash-loop rule — recipients own land; a landless death is ended (handoff §4.1, re-ruled)

**Files:**
- Modify: `crates/world-store/src/succession.rs`:
  - `declare_in` survivors (`:389-400`)
  - new `end_landless_host`
  - the module notes
- Modify: `crates/world-store/src/rows.rs`:
  - `hosts()` (`:833`) adds `owns_land`
- Modify: `crates/world-store/src/handle.rs`, `thread.rs`:
  - SQLite answers `Unsupported`
- Modify: `crates/world-store/src/operator.rs`, `ctl.rs`:
  - `retire_on` (`:445`)
- Modify: `crates/game/src/coordinator/succession.rs`:
  - `HostSeen` (`:81`)
  - `heir_candidates` (`:904`)
  - `maintain_successions` (`:678`)
  - `observe_leases`
- Modify: `crates/game/src/coordinator/world_cut.rs`
- Modify: `crates/server/src/coordinator_rt.rs`
- Modify: `crates/game/src/world_manager/{succession,peers}.rs`
- Test:
  - `crates/world-store/tests/{succession,worldctl}.rs`
  - `crates/game/tests/coordinator.rs`
  - `crates/game/tests/peer_schedule.rs` + `common/multi_rig.rs`
  - `crates/server/tests/takeover.rs`

**Interfaces:**

```rust
pub enum LandlessEnd { Ended, NotLapsed, OwnsLand, Recipient, Gone }
impl StoreHandle {
    pub fn end_landless_host_later(&self, host: HostId, boot: u64, fence: CoordinatorFence)
        -> crossbeam::channel::Receiver<Result<LandlessEnd, StoreError>>;
}
pub struct HostRow { /* … */ pub owns_land: bool }   // EXISTS super_regions owner OR EXISTS in_flight host
pub struct HostSeen { /* … */ pub landed: bool }     // game
```

**`end_landless_host`** runs as one transaction under `CUT_LOCK_TIMEOUT_MS`.
- Lock order: `coordinator` `FOR SHARE` (epoch compared) → `world` `FOR NO KEY UPDATE` → the host's row `FOR NO KEY UPDATE`.
- Its prefix is `declare_in`'s, so it adds no cycle. A zombie's `claim_free` waits at its `check_live` and is refused.
- Outcomes:

  | Outcome | When |
  |---|---|
  | `Gone` | the row is missing, of another boot, or not live |
  | `NotLapsed` | the lease has not lapsed by `PG_NOW_MS` |
  | `OwnsLand` | the host has a `super_regions` or `in_flight` row |
  | `Recipient` | an open succession names it heir or survivor |
  | `Ended` | none of the above: `state = DEAD`, generation untouched |

**Survivors in `declare_in`:**

```sql
SELECT id FROM hosts h
WHERE clean = 0 AND state = LIVE
  AND (EXISTS (SELECT 1 FROM super_regions WHERE owner = h.id)
       OR EXISTS (SELECT 1 FROM in_flight WHERE host = h.id)
       OR h.id = ANY($heirs))
ORDER BY id
```

minus the predecessors.

**The core:**
- A lapsed landless host that is in no open run's survivors or heir yields `EndLandless { host, boot }` (`take_end_landless()` beside `take_declare`). With no run open, a landless lapse is also ended rather than succeeded.
- What the answers lead to:
  - `Ended`: `host_ended`.
  - `OwnsLand`: decide again at the next read.
  - `Recipient` while a run is open: rollback.
  - `NotLapsed` / `Gone`: `refused_at_read`.
- An id absent from the latest `hosts` read is removed from `cuts.left`, `cuts.succeeded`, `cuts.boots`, `successions.declared` and `successions.reread_for` (B-M5).
- Hosts: `forget_host` also accepts a dead row named by no open succession.

**`worldctl retire-host`** with no running host refuses: `no host is running: start one — the first registration takes the land (a total crash); retire-host only vouches for a dead row while hosts run`. `operator::retire_offline`, `Retired` and their tests are deleted.

- [ ] **Step 1: Write the failing tests.**
  - Store:
    - `succession::a_landless_live_host_is_not_a_survivor`
    - `succession::the_heir_is_a_survivor_even_landless`
    - `succession::a_landless_lapsed_host_is_ended_without_a_generation_change`
    - `succession::a_landed_host_is_not_ended`
    - `succession::a_recipient_is_not_ended`
    - `succession::ending_is_fenced`
    - `succession::a_landless_host_ends_while_a_succession_is_open`
    - `worldctl::offline_retire_host_is_refused`
  - Game:
    - `coordinator::a_lapsed_landless_non_recipient_is_ended_not_a_second_failure`
    - `coordinator::a_lapsed_landed_host_during_a_run_still_rolls_back`
    - `coordinator::the_heir_prefers_landed_hosts_that_take_logins`
    - `coordinator::an_id_pruned_from_the_read_is_forgotten`
  - Seeded, in `peer_schedule.rs` (the rig's `declare_in_store`, `:1429`, applies the survivor rule; add `end_landless_in_store`):
    - `a_flapping_landless_host_never_rolls_the_world_back`: two landed hosts; kill h1, then `start("h1")` and kill it before the settle, three times. Generation unchanged, the run settles, `assert_conserved_world()`.
    - `a_host_landed_after_the_declaration_is_not_a_recipient_and_the_settle_commits`
    - `a_host_landed_during_a_run_that_dies_rolls_back_whole`: pins the residual — a rollback, never a loss.
  - Real store:
    - `takeover::a_crash_looping_pod_beside_a_healthy_host_never_raises_the_generation`
- [ ] **Step 2:** Run and expect FAIL:
  - `just test-pg "--test it -- succession:: worldctl::"`
  - `just test-one game "--test it -- coordinator::"`
  - the seeded names
  - `just test-r4`
- [ ] **Step 3:** Implement. Write the lock walks into `succession.rs`'s notes.
- [ ] **Step 4:** All pass. Then run once each: `just test-pg "--no-fail-fast"`, `just test-one game "--no-fail-fast"`, `just test-threads`, `just test-r3`, `just test-r4`, `just smoke`. Record the counts; the known reds are listed in `TODO.md`.
- [ ] **Step 5:** Commit: `feat(succession): recipients own land; a landless death is ended, not succeeded — a flapping pod no longer rolls the world back`.

### Task 3: The core's clock is the store's clock

**Files:**
- Create: `crates/game/src/coordinator/clock.rs`
- Modify: `crates/game/src/coordinator/mod.rs`:
  - `calendar_now` (`:271`)
  - `maintain` (`:452`)
  - `from_state` (`:223`)
- Modify: `crates/server/src/coordinator_rt.rs`:
  - `now` (`:1074`)
  - the `hosts` read
- Modify: `crates/world-store/src/rows.rs` / `handle.rs`:
  - `hosts_later` returns the store time with the rows: `HostsRead { rows, store_ms }`
- Test: `crates/game/tests/coordinator.rs`, `crates/server/tests/coordinator_rt.rs`

**Interfaces:**

```rust
pub const CALENDAR_GAP_MS: u64 = 30_000;
/// Store time from the process's monotonic clock: re-sampled, never backwards.
pub struct StoreClock { /* offset, last */ }
impl StoreClock {
    pub fn sample(&mut self, store_ms: u64, mono_ms: u64);
    pub fn now(&self, mono_ms: u64) -> u64;
}
```

- The core is fed store milliseconds. The runtime passes `clock.now(mono)` wherever it passes `now_ms` today; `game` reads no clock itself.
- The calendar anchor `(store_ms, tick)` and `calendar_seen_ms` live in the core.
- `calendar_now(now)`:
  - If `now - calendar_seen_ms > CALENDAR_GAP_MS`, re-anchor at `(now, history.now())` first: downtime is not caught up.
  - Every day turn sets `calendar_seen_ms = now`.
- Every deadline the core keeps (`WORLD_CUT_DEADLINE_MS`, `SUCCESSION_DEADLINE_MS`, the cut period's `last_ms`) is in store milliseconds.
- `from_state` re-bases any stored deadline that lies more than `CALENDAR_GAP_MS` in the past. The run restarts its count, which is conservative and matches today's restart.

- [ ] **Step 1: Write the failing tests.**
  - `coordinator::downtime_is_not_caught_up`: run a core to day 3. Save its state, rebuild with `now + 3_600_000`, `maintain`. `history.now()` has moved by at most one `maintain` step, not an hour.
  - `coordinator::the_calendar_continues_across_a_quick_restart`: rebuild at `now + 2_000`. The calendar continues from the anchor, with no skip and no repeat of a day turn.
  - `coordinator::a_cut_deadline_older_than_the_gap_restarts_its_count`
  - `clock::tests::the_store_clock_never_runs_backwards`
- [ ] **Step 2:** Run `just test-one game "--test it -- coordinator::"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS, then `just test-r2`, `just test-r4`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(coordinator): the core keeps store time; downtime is not caught up`.

### Task 4: History saves every field and loads without guessing

**Files:**
- Modify: `crates/game/src/history/mod.rs` (`:42-87`, the `#[serde(skip)]` fields; `content_dirty`)
- Modify: `crates/game/src/history/persist.rs` (`HISTORY_FILE_VERSION = 4`; `OLDEST_LOADABLE_VERSION = 4`; v2/v3 paths deleted)
- Modify: `crates/game/src/history/handover.rs` (`after_load` `:575`: only `set_grid` work remains)
- Test: `crates/game/tests/history*.rs`, `crates/server/tests/roles.rs`

**Interfaces:**
- `History` serialises every field but `grid`:
  - `running`, `stopping`
  - `directives`, `bounced`
  - `stray_candidates`, `held_since`
  - `notices`
- `pub fn content_dirty(&self) -> bool; pub fn clear_content_dirty(&mut self);`
  - Every mutation sets it except `advance` when no step was due.
  - `take_directives` / `take_notices` set it when they drained something.
- `persist::decode` no longer returns embodied people to the abstract history.

- [ ] **Step 1: Write the failing tests.**
  - `history::a_history_reloaded_after_every_input_is_identical`:
    - Bootstrap a small world.
    - Drive the same scripted inputs into A and B. The inputs are 200 calendar steps plus `region_started`, `person_crossed`, `region_parked`, `note_positions`, `defer_directive`, `reattach_regions`, and an embark.
    - B is `decode(encode(B))` before every input.
    - Assert equal `encode` bytes, equal `take_directives` / `take_notices` outputs, and an equal legends page.
  - `history::advancing_with_nothing_due_is_not_a_content_change`
  - `history::a_v3_file_is_refused` (re-forge).
- [ ] **Step 2:** Run `just test-one game "--test it -- history"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then the crash tables that exercise a coordinator restart must still pass unchanged:
  - `just test-r2` (`coordinator_crash_mid_play_conserves_people`, `embark_during_downtime_completes_once`, `embark_answered_then_crash_is_not_refounded`, `host_survives_coordinator_absence_at_start`)
  - `just test-r3` (`coordinator_killed_*`)
  - `just test-r4`

  A failure here is a defect of decision 4: record it as a ruling and stop for the controller, do not weaken a test. Then `just test-threads`, `just determinism-wasm` (history is hashed and serialised), `just smoke`.
- [ ] **Step 5:** Commit: `feat(history): v4 saves every field; a load is not a restart`.

### Task 5: The whole core is one durable projection, stored with a compare-and-set

**Files:**
- Modify: `crates/game/src/coordinator/state.rs`: `CoordinatorState` v3, `ProcessBoot`.
- Modify: `crates/game/src/coordinator/mod.rs`: fields grouped into the projection; `gated`, `is_gated` and `take_save` / `saved` / `commit_unstored` replaced by `take_transition`.
- Modify: `crates/game/src/coordinator/world_cut.rs`, `succession.rs`: the durable parts serialisable; attachments stamped by process boot.
- Modify: `crates/game/src/coordinator/msg.rs`: `is_gated` removed.
- Create: `crates/world-store/migrations/pg/0016_coordinator_rows.sql`, `crates/world-store/migrations/sqlite/0016_coordinator_rows.sql`.
- Modify: `crates/world-store/src/rows.rs` (`claim_coordinator` `:985`, `save_coordinator` `:998`, `load_coordinator` `:1015`), `handle.rs`, `thread.rs`, `two_phase.rs` (the `cut_begins` prune).
- Test:
  - `crates/game/tests/coordinator*.rs`
  - `crates/world-store/tests/{fence,schema,two_phase}.rs` + a new `coordinator_rows.rs` module of `it.rs`
  - `crates/game/tests/common/multi_rig.rs` + `peer_schedule.rs`

**Interfaces:**

The migration (both engines; SQLite `INTEGER` / `BLOB`):

```sql
ALTER TABLE coordinator ADD COLUMN version BIGINT NOT NULL DEFAULT 0;
ALTER TABLE coordinator ADD COLUMN core BYTEA;
ALTER TABLE coordinator ADD COLUMN history BYTEA;
ALTER TABLE coordinator DROP COLUMN state;
```

The game side:

```rust
// game
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)] pub struct ProcessBoot(pub u64);
pub struct Transition { pub core: Vec<u8>, pub history: Option<Vec<u8>>, pub out: Vec<(Target, CoordMsg)> }
impl Coordinator {
    pub fn load(core: &[u8], history: &[u8], grid: SocietyGrid, process: ProcessBoot) -> Result<Self, HistoryLoadError>;
    /// The pending change as bytes plus the messages it releases; None when nothing durable changed.
    pub fn take_transition(&mut self) -> Option<Transition>;
}
```

The store side:

```rust
// world-store
pub struct StoredCoordinator { pub version: u64, pub core: Vec<u8>, pub history: Vec<u8> }
impl StoreHandle {
    pub fn load_coordinator(&self) -> Result<Option<StoredCoordinator>, StoreError>;
    pub fn write_coordinator_later(&self, fence: CoordinatorFence, version: u64, core: Vec<u8>, history: Option<Vec<u8>>)
        -> Receiver<Result<CoordinatorWritten, StoreError>>;   // Err(Conflict) when version moved
}
pub struct CoordinatorWritten { pub version: u64, pub store_ms: u64 }
pub enum StoreError { /* … */ Conflict }
```

The SQL, and the lock walk to add to `succession.rs`'s notes:
- **Write:** `UPDATE coordinator SET core = $1, history = COALESCE($2, history), version = version + 1, saved_utc = $3 WHERE id = 1 AND epoch = $4 AND version = $5 RETURNING version, <now_ms>`. It is one row and wants nothing else.
- **No row:** re-read `epoch`. A different epoch → `Fenced`; the same epoch → `Conflict`.
- **`claim_coordinator`:** unchanged (epoch + 1, the one-process fence of E2a).
- **The prune:** in `decide_cut`, after its decision row is inserted, `DELETE FROM cut_begins WHERE cut_id < $cut`. A begin below the cut being decided is never read again: `resolve_prepared` does not read `cut_begins`. `cut_decisions` is kept.

What changes in the core:
- `attached` becomes `BTreeMap<HostId, (u64 boot, ProcessBoot)>`. "Attached" means stamped with this process's boot.
- `push` no longer gates. Every output waits for its transition.
- `cuts.run`, `outcomes` and `lookups` are in the projection. So are the successions' `open`, `declared`, `rollback`, `land_lookups` and `land_wanted`.
- `LeaseWatch`, retry timers and `memo_warned_ms` are not in the projection.
- `region_host` gives way to the history's `running`, with the host kept beside it.

The rig:
- `MultiRig::restart_coordinator_from_store()` rebuilds the core with `Coordinator::load(last transition, …, new ProcessBoot)`. It sets every host's world link down for one round, then up.
- The rig's link drives `take_transition` in place of `commit_unstored`.

- [ ] **Step 1: Write the failing tests.**
  - `coordinator::a_core_reloaded_after_every_input_is_identical`: the coordinator test inputs (attach, requests, presence, a cut, a succession, `maintain` across day turns) run into A, and into B, which is `load(take_transition())` after every input. The outputs are equal, in order, and so are the final bytes.
  - `coordinator::an_attachment_of_another_process_boot_is_detached`
  - `coordinator::no_output_is_released_without_its_transition`
  - Store:
    - `coordinator_rows::a_write_with_a_stale_version_conflicts`
    - `coordinator_rows::a_write_with_a_stale_epoch_is_fenced`
    - `coordinator_rows::history_is_kept_when_not_written`
    - `two_phase::a_decision_prunes_older_begins_and_keeps_every_decision`
    - `schema::` `EXPECTED` updated
  - Both engines.
  - Seeded: `peer_schedule::a_coordinator_restart_at_any_round_conserves_everything`, through `sweep`:
    - 3 hosts, crossings and cuts.
    - `restart_coordinator_from_store()` at a seeded round, then a landed host killed at another.
    - `assert_conserved_world()`, `assert_journals_explain_the_cut()`, each claim granted once.
- [ ] **Step 2:** Run the above. Expected: FAIL.
- [ ] **Step 3:** Implement. The runtime keeps working through a thin adapter in this task: it calls `take_transition` and writes at its current cadence. Task 6 makes the write the gate.
- [ ] **Step 4:** PASS. Then `just test-one game "--no-fail-fast"`, `just test-pg "--no-fail-fast"`, `just test-one world-store "--no-fail-fast"`, `just test-threads`, `just test-r2`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(coordinator): the whole core is one durable projection, written with a compare-and-set`.

### Task 6: The runtime makes every change durable before it is seen

**Files:**
- Modify: `crates/server/src/coordinator_rt.rs`:
  - `run` (`:1078`)
  - `pump_save` (`:1725`) becomes `pump_transition`
  - `finish` (`:1802`)
  - `fence_out` (`:1846`)
  - the `COORDINATOR_SAVE_SECS` and `COORDINATOR_FAILURES_BEFORE_STUCK` paths
- Modify: `crates/server/src/world_file.rs`:
  - `ensure_coordinator_state` (`:401`) loads `StoredCoordinator`
  - bootstrap writes version 1
  - the legacy `history` table is still read once to migrate a forged-but-unstarted world
- Modify: `crates/server/src/health.rs`: the counters
- Test: `crates/server/tests/{roles,coordinator_rt,hosts,takeover,login_link}.rs`

**Interfaces:**
- **Each pass:**
  1. Drain hosts, login and store answers.
  2. `maintain(clock.now())`.
  3. If no write is in flight and `take_transition()` gives one, send `write_coordinator_later`.
  4. On `Ok(w)`: `clock.sample(w.store_ms)`, then send `out`, then `forward_login`.
  5. On `Conflict`: reload, re-apply the kept inputs once, else exit `EXIT_FENCED`.
  6. On `Fenced`: `fence_out()`, as today.
  7. On any other error: hold `out`, retry after `COORDINATOR_SAVE_RETRY_MS`, and log stuck after `COORDINATOR_FAILURES_BEFORE_STUCK`. The core does not advance the inputs past an unwritten transition: it is re-applied, never doubled.
- **A stop:** the last transition is written (bounded by the stop's timeout). There is no separate "final save".
- **Counters** (§8): `mmo_coordinator_transitions_total`, `mmo_coordinator_written_bytes_total{part}`, `mmo_coordinator_conflicts_total`, and `coordinator transitions=… core_bytes=… history_bytes=…` on the periodic line.
- **Test hooks:**
  - `fail_coordinator_writes(bool)` (the store answers an error);
  - `kill_after_commit_before_send()` (the thread exits right after a successful write, before sending).

- [ ] **Step 1: Write the failing tests.**
  - `roles::no_output_leaves_before_its_state_is_written`: with writes failing, a host's `Request` gets no `Reply` and a `RegionStarted` gets no `SpawnPeople`. When writes resume, exactly one `Reply` arrives and the memo holds it.
  - `roles::a_coordinator_killed_after_the_commit_before_the_sends_loses_nothing`: kill after commit, before send, then restart. The host re-attaches; its resent `Request` is answered from the memo, identical, with no second grant. A cut whose `CutDone` was lost is resolved by lookup. People are conserved.
  - `roles::a_restart_resumes_exactly_where_the_last_transition_left`: after the restart, the calendar tick, site markers and legends length equal those of the last committed transition, not of a 60 s save.
  - `coordinator_rt::single_player_restart_resumes_history_where_it_was`: role `all` on SQLite, the same assertion.
- [ ] **Step 2:** Run `just test-r2` and `just test-one server "--features test-hooks --test coordinator_rt"`. Expected: FAIL.
- [ ] **Step 3:** Implement. Delete `COORDINATOR_SAVE_SECS` and `request_save` / `force_save`.
- [ ] **Step 4:** PASS. Run every server target once:
  - `just test-one server "--features test-hooks --no-fail-fast"`
  - `just test-r2`, `test-r3`, `test-r4`
  - `login_link` with `--include-ignored`
  - `just test-threads`
  - `just smoke`
  - `just twin-down && just twin-up && just twin-images && just twin-apply && just twin-check` — record the coordinator line's write counters from the twin in the ledger. That is the first number for E2b.
- [ ] **Step 5:** Commit: `feat(coordinator): every change is written before anything it decided is sent`.

### Task 7: Documentation and closing

**Files:** `CLAUDE.md` (Dawid's hunks: filtered staging), `TODO.md`, this plan's "As built".

- [ ] **Step 1: `CLAUDE.md`**
  - **"Server / store":**
    - The coordinator holds no state; a transition is apply → write (CAS on `version`) → send.
    - Two blobs, `core` and `history`; what a process may hold.
    - Store time, and that downtime is not caught up.
    - History v4.
  - **The succession rules:** recipients own land, `end_landless_host`, offline retire refused, `cut_begins` pruned while `cut_decisions` is kept.
  - **Migration `0016`:** re-forge (`just pg-reset`, `just run --reset`).
  - **"Link rules":** "the coordinator down means no cuts, no takeover and no logins" — still true until E2b, but nothing it decided is lost.
- [ ] **Step 2: `TODO.md`**
  - An "Elastic 2a" block: built, the hand checks (Task 1's probe run and the client tests), the residual of decision 9, and the twin's write counters.
  - Strike through, with the date, the E1 limits removed:
    - "a crash-looping host pod can roll the world back";
    - "the coordinator never forgets a host id";
    - "`cut_begins` never pruned";
    - "offline retire-host's succession";
    - "the 60 s coordinator save window".
  - Keep what E2b takes: replicas, the outbox, the login link and dial, the module, the panel, host `/readyz` after attach.
- [ ] **Step 3:** Write "As built": commits, rulings, plan defects, and the final suite table. Run each touched crate's whole non-client suite once, plus `test-threads`, `test-r2|r3|r4`, `determinism-wasm` and `smoke`.
- [ ] **Step 4:** Commit: `docs: Elastic 2a — the coordinator's state in the store, as built`.

## E2b — what the next plan does (outline, written after this one is built)

- **Decisions fenced on `version`, not on a per-process epoch:**
  - `claim_coordinator` is gone for the split role;
  - `decide_cut`, `declare`, `record_cut_begin`, `rollback_world` and `end_landless_host` compare the core `version` they were decided from (`FOR SHARE`), as they compare the epoch today.
- **Replica rows** (`coordinator_replicas`, a lease from each replica's main loop):
  - attachments are stamped by replica instead of process boot;
  - a dead replica's attachments count as detached.
- **An outbox table** for outputs whose host is linked to another replica. Each replica relays the rows for its own hosts, driven by LISTEN/NOTIFY with the polling backstop `listen.rs` already has.
- **Timed work** (`maintain`'s calendar step, starting a cut, the lease watch, `land_wanted`) is a transition any replica may attempt when due. The compare-and-set picks one.
- **Links:**
  - `svc-link` resolves every address of a name and holds a link per replica, or rotates;
  - the login service reaches any replica, and the replica relays to a gate host among those attached to it, or answers "none" at once (handoff §4.4).
- **A host is Ready once attached** (handoff §5).
- **Deployment:** N replicas behind the headless Service (already headless), a PodDisruptionBudget, the recipes, `twin_check.sh`, the alerts, and the world panel (hosts by label from E1, plus replicas — after Dawid commits `crates/panel`).
- The split of `core` / `history` into narrower rows, if Task 6's counters on the twin (and a live measurement Dawid runs) show it is needed.

## Not in this plan

- Several coordinator processes (E2b).
- Nested successions: a landed host dying during an open run still rolls back.
- E3–E6.
- Handoff §5 "E2 leaves". Each item is done only if its file is already open for another reason, and recorded as a ruling.

## Self-review notes

- **Handoff → tasks:**

  | Handoff item | Covered by |
  |---|---|
  | §4.1 | T2 (re-ruled) |
  | §4.2: history, claims and memo as state in the row | T4, T5, T6 (no memo table: decision 5, "departs" 4) |
  | §4.2: what Attach rebuilds | Now durable in the projection (T5); E2b decides per-replica views |
  | §4.2: replica liveness | E2b |
  | §4.3 | E2b, on top of T5's projection |
  | §4.4 | E2b |
  | §4.5 | T1 |
  | §4.8 | E2b |
  | §4.9 | E2b |

  §5's "E2 takes":

  | Item | Covered by |
  |---|---|
  | Crash loop | T2 |
  | Forgotten ids | T2 |
  | `cut_begins` | T5 |
  | Offline retire | T2 |
  | The gate without a token | E2b, with replicas |
  | `/readyz` | E2b |
- **Dawid's ask → tasks:**
  - "State in Postgres, none of its own": T3–T6, with decision 1 listing the only things a process may hold.
  - "SQLite in single-player too": T5's migration on both engines and T6's `single_player_restart_…`.
- **Names across tasks:**

  | Name | Defined in | Used by |
  |---|---|---|
  | `LandlessEnd`, `end_landless_host_later`, `HostRow.owns_land`, `HostSeen.landed` | T2 | T5's rig |
  | `StoreClock`, `CALENDAR_GAP_MS`, `HostsRead { rows, store_ms }` | T3 | T5, T6 |
  | `content_dirty`, `HISTORY_FILE_VERSION = 4` | T4 | T5's `take_transition` |
  | `ProcessBoot`, `Transition`, `Coordinator::load`, `take_transition`, `StoredCoordinator`, `write_coordinator_later`, `CoordinatorWritten`, `StoreError::Conflict` | T5 | T6 |
- **Proportion:** seven tasks. The code rewrite is T5 and T6. The rest is either independent (T1, T2) or prepares them (T3, T4).

## As built (2026-11-16)

Built on `develop` from `0985ab69`, executed subagent-driven; the ledger and the per-task reports are in
`.superpowers/sdd/2026-11-16-elastic-2a-coordinator-state-in-the-store/` (one workspace per plan — not the
`…-coordinator-state/` path the Global Constraints name). Every task was reviewed clean. Not yet accepted: the
whole-branch review and Dawid's hand checks (TODO.md, "Elastic 2a") are pending; nothing ran live or on orion.

### Commits

Foreign `quest` commits are interleaved on `develop` and not listed. One of them, `837c9131`
(`feat(quest): frame layout and code generation …`), also carries T5's deletion of
`crates/game/tests/fixtures/coordinator_state_v1.bin` (T5's early `git rm --cached` was swept into that concurrent
commit; history not rewritten).

| Task | Commits |
|---|---|
| T1 the probe | `7af6af89` fix(go-nogo): the probe seams from the client's own host map, re-forges each run, judges successions in its window; `0f13de5b` fix(go-nogo): the region-host map is native-only — the browser client builds again |
| T2 recipients own land | `711e97af` feat(succession): recipients own land; a landless death is ended, not succeeded; `1c5ebe18` fix(succession): a host landed after the declaration that traded state with the dead host stops for a rollback |
| T3 store time | `70bfdb7a` feat(coordinator): the core keeps store time; downtime is not caught up; `50285583` fix(coordinator): sample the store clock at receipt; sample it after the load |
| T4 History v4 | `d45129f6` feat(history): v4 saves every field; a load is not a restart |
| T5 the projection | `f691fa57` feat(coordinator): the whole core is one durable projection, written with a compare-and-set; `00f4e975` fix(coordinator): a rollback is recorded once, nothing leaves while a write is in flight |
| T6 write-before-send | `0b4da0e7` feat(coordinator): every change is written before anything it decided is sent; `cf7c4d8f` fix(twin): a killed landless host is ended, not succeeded — the twin checks decision 9 |
| T7 docs | `docs: Elastic 2a — the coordinator's state in the store, as built` (CLAUDE.md, TODO.md, this section) |

### "To confirm with Dawid" — defaults taken (he did not answer before execution)

- The residual of decision 9 stands: a host that claims land while a succession is open and dies before its
  settle still rolls the world back. Cost if wrong: a later plan for nested successions.
- Offline `worldctl retire-host` is refused (`retire-host: no host is running: start one — the first
  registration takes the land …`). Cost if wrong: re-add an offline path (the deleted `retire_offline` is in git).
- `History::after_load`'s blanket return is removed (decision 4), gated by T4's unchanged crash tables, which
  passed (`test-r2` 16/16, `test-r3` 44/44, `test-r4` both `coordinator_killed_*`).

### Rulings

Execution-wide:
- **R-T3**: T3 did not bump the state format; the anchor and `calendar_seen_ms` stayed in memory until T5's
  projection made them durable. The two tests needing a stored anchor/deadline
  (`the_calendar_continues_across_a_quick_restart`, `a_cut_deadline_older_than_the_gap_restarts_its_count`) moved to T5.
- **R-T5**: T5's thin adapter already wrote every transition (CAS) and sent its outputs only after `Ok` — removing
  `gated` beside a 60 s save would have held every reply 60 s. T6 then added the conflict policy, counters and hooks.

T1: `ClientUpdateEvent::RegionHosts` (a client-internal event, appended) carries the `ConnectionSet`'s
region→host map from the game thread to the Bevy autopilot.

T2:
- `LandlessEnd` lives in `game` (as `Declared`) and is re-exported by `world-store`; the core's hand-out is
  `take_end_landless(now)` → `landless_ended` / `end_failed`.
- `HostRow.succeeding` added beside `owns_land`, so a host tells an ended row from a predecessor whose run is open
  (`forget_dead_hosts`).
- The coordinator's forget of ids pruned from the `hosts` read (B-M5) keeps ids still attached.
- An end is handed out only between cuts (a running cut it is in is aborted first) and never while a declaration is
  in flight. `Recipient` with no run open in the core → a re-read of the open successions, not a rollback.
- The core's offline-run paths (runs with no survivors) are kept although nothing opens one any more.
- `ask_for_land` also asks for a directory read (a landless gate learns a Leave's land move without an order).
- **I1 (review), handled as fail-stop**: a host refused a cut part for unsaved traffic with a host a directory read
  showed dead (`dead_seen` → `unaccounted_dead`) stops for a rollback (exit 5, no save) — rollback, never a loss,
  never a stuck world; each such case costs one rollback.
- The rig's survivor rule mirrors the store's `declare_in` exactly (review S2).

T3:
- The first `now` is one store-clock sample (`db_now_ms`) taken after the load/bootstrap, extrapolated monotonically.
- A `hosts` read samples the clock at receipt with no `rtt/2` correction (review I1: the correction inflated lease
  spans); the clock lags the store by a return trip and never leads it.
- `calendar_now` stays `&self` (applies the gap virtually); the re-anchor happens in `maintain` and also sets
  `calendar_seen_ms`. `rebase_deadline` lets a deadline past by ≤ the gap stand (fires at once).
- The blocking `hosts()` keeps `Vec<HostRow>`; only `hosts_later` answers `HostsRead`.
- `history_handover::World::wait` steps a day at a time (a half-year jump is downtime under the gap rule).

T4:
- `content_dirty` is not in the history blob (decoded = clean, new = dirty); dirty-marking is conservative.
- Tests that pinned the removed normalisation were replaced (`a_load_keeps_embodied_state`,
  `a_reload_then_a_park_normalises_embodied_state`, `noted_positions_survive_a_reload`); `HISTORY_DIGEST`
  re-recorded `0x48693884 → 0xF2ABCB4D`.
- **An old-version history/state beside saved regions is a HARD REFUSAL** ("re-forge: `just pg-reset` /
  `just run --reset`"), never a silent bootstrap — old bodies' `PersonId`s would collide with a fresh history's.

T5 (17 rulings in `task-5-report.md`; the load-bearing ones):
- `Coordinator::load(core, history, grid, process, now_ms)` takes the store time; `start_clock` is gone;
  `new_for(history, now, process)` bootstraps.
- `region_host` is KEPT beside the history's `running` (it routes directives, carries the host; the restart undo is
  per host).
- Store calls handed to the runtime are stamped with the `ProcessBoot` (`declaring`, `ending`, a cut decision's
  `taken`): another process's count as not in flight. The `*_at_read` counts stay out of the projection.
- `commit_unstored` kept for the storeless inline link (wasm single-player, most game tests).
- `take_out` releases what changed nothing durable (a query's answer costs no write).
- `WorldCuts.every_ms` is durable; `set_world_cuts` restarts the period only on a change.
- `content_dirty` is not in the core projection (core and history are written in one statement) — departs from
  decision 5's list.
- Old core format / another world / no projection beside saved regions → refusal naming the re-forge; with no
  saved region an old state is replaced after its history is copied to `<db>.coordinator.bak`.
- `successions_unknown()`: a loaded core holding runs re-reads them at start.
- `rolled_back` detaches every host (found by r4 `heir_killed_before_the_settle_rolls_the_world_back_once`).
- The first `Attach` at a newer generation parks every older-generation host's regions (`first_of_generation`) —
  the restart undo once `region_host` is durable.
- `COORDINATOR_SAVE_SECS`, `request_save`, `force_save` deleted in T5 (T6 listed them); the `CoordinatorState`
  v1/v2 ladder and its fixture deleted.
- **Fix round, I1 (plan-mandated defect)**: `rollback_world` is conditional on the generation the decision was made
  from; asked again after its commit it answers "already rolled back" (= `rolled_back`). A `rolled_back(g)` answered
  after hosts already attach at `g` tells and detaches nobody.
- **Fix round, I2**: a core `writing` flag set by `take_transition`, cleared by `transition_written` /
  `transition_failed`; `take_out` refuses while it is set.
- Two r3 tests made flaky by slower cuts were fixed without weakening their claims (`push_over_the_seam` puts the
  slime mid-region and both cuts are asserted; `replaced_host_exits_fenced_at_its_next_heartbeat` reads its bound at
  host 0's first refusal).

T6:
- **R1, Conflict**: the row is re-read; it is written over at the new version only if it holds this process's own
  lost commit (`version + 1`, core hash among ≤ `COORDINATOR_UNACKED_MAX` (64) failed writes); anything else, or a
  second conflict before a commit → exit as fenced. Replaces decision 2's "reload, re-apply the inputs, retry once"
  (accepted in review as equivalent or safer: no re-drive of store calls decided from another core).
- The conflict re-read is synchronous on the coordinator thread (a rare path).
- `kill_after_commit_before_send` applies to the next transition taken after arming; a third hook
  `lose_next_write_answer` exercises the lost-commit branch.
- The `[coordinator] window=5.0s …` line is per 5 s window, skipped when nothing was written or failed.
- `world_file.rs` needed no change after T5.

### Plan defects found

- **R-T3**: T3 could not store the calendar anchor or test a stored deadline before T5's projection existed.
- **R-T5**: "write at the current 60 s cadence" while removing `gated` would hold every reply 60 s.
- **T1's wasm32 break**: `bridge.rs` named `super::autopilot::RegionHosts` (the module is `not(wasm32)`); T1's gate
  compiled the client natively only. Found by T6's `twin-images`; fixed in `0f13de5b`.
- **`twin_check.sh` encoded Elastic 1's ruling** that a landless killed host gets a `Crash` succession; decision 9
  reverses it. Phase 5 now asserts the end (old row dead, no succession, generation unchanged) — `cf7c4d8f`.
- **T2's I1 stuck-world path**: decision 9's survivor rule let a host that won land beside the dead host AFTER the
  declaration route a transfer to it, then refuse every later cut part — the settle never committed. Handled as
  fail-stop (above); the store's "survivors" note corrected.
- **T5's double rollback**: the plan made the `rollback` flag durable while `rollback_world` raised the generation on
  every call — a crash between its commit and the transition's write rolled the world back twice. Fixed by the
  store-side condition (above).
- Write latency exposed two coordinator bugs that were invisible with immediate sends: old-generation hosts left
  attached after a rollback (a new succession declared at the new generation) and a newer-generation attach not
  dropping older hosts from the view; both fixed in T5 with regression tests.
- Decision 2's conflict replay (T6 R1) and decision 5's `content_dirty` in the projection (T5) were replaced;
  the plan's `region_host` "gives way to the history's running" did not hold (T5 kept both).

### Deferred minors (for the final review)

- T1: `crossing_summary.py` `"event=autopilot_seam" in line` also matches `autopilot_seam_wait` (the window can
  start at the first wait line); only the host run's window is judged (a succession during the thread run goes
  unflagged); the justfile's unmatched `host-*.log` glob passes the literal; `autopilot.rs` import order / an
  overlong doc line.
- T2: `register_in` counts ended (dirty, dead) rows as a total crash before its prune (a clean restart can raise
  the generation); the `host_succeeded` log says "0 region(s) parked" for an ended host; the core's offline-run
  paths and `world_manager/succession.rs` ~:53, ~:767 are unreachable; the unaccounted-dead stop logs "a host
  restarted dirty" (`lib.rs` ~:1334 / `cuts.rs` ~:855–866) — record a reason in `stop_for_rollback`; a landless host
  tripping that stop would be ENDED, not rolled back (no path found; backstop: a lapse after the stop counts as a
  second failure); after a coordinator restart an ended row asks once for a re-read (one warning line).
- T3: every blocking `hosts()` caller pays an extra `SELECT` for the clock; a genuine forward step of the store's
  clock between samples stretches open lease spans (consider capping a sample's forward step).
- T4: the reload test may compare empty outputs (assert a non-empty drain, the crossing, held/stray across a
  reload); mixed `content_dirty = true` vs `mark_content_dirty()`; conservative marks before early returns.
- T5: `first_of_generation` leaves old-generation hosts attached (could be gate host until their link drops —
  consider `host_detached`); an extra core encode per pass in `take_out` → `durable_pending` (cache the raw bytes);
  a coordinator that dies between its claim and its first write beside saved regions is refused at the next start;
  `CoordinatorSource::needs_save` is a test-only leftover; the "rolled back already" path pushes
  `SuccessionDone { settled: false }` to `Target::Every` while its docs say it tells nobody; the narrowed
  `replaced_host_exits_fenced…` no longer covers the host-1-exit → host-0-first-refusal window.
- T6: the conflict re-read inside `finish` is not bounded by the stop timeout; two lost answers in a row exit as
  "another writer" (a wrong diagnosis); say why a core-only hash suffices; the `Resumed` / attach / embark helpers
  are duplicated in `tests/coordinator_rt.rs` and `tests/roles.rs` (→ `tests/common`); the roles tests silently
  pass without Postgres (the existing pattern); the restart roles tests take ~20 s each (the raw link notices the
  dead coordinator late).
- **Watch**: takeover `a_clean_leave_keeps_the_generation_and_its_waiting_transfers_reach_the_heir` (a conservation
  assertion, slime exactly once) failed once in a full serial `test-r4` under load in T4; 8/8 alone, green in every
  later full run (T5 ×2, T6, T7). Hand to the final review (the Leave-succession path).

### The twin (T6, after the two fixes)

`twin-check` PASS in 481 s (down/up/images/apply/check; transcript `target/twin/check-20261116T230407Z/`). Phase 5:
the killed host's old row dead (vouched), successions declared 0 → 0, generation 0. The write counters — E2b's
first number (idle world, 2 hosts, no players, 48 windows of 5 s): 167 transitions, core 65 981 B (≈ 395 B lz4 per
transition), history 346 380 B in 6 windows only, conflicts 0, write failures 0. A live measurement with players
is owed before E2b splits anything.

### Final suites (T7, at `cf7c4d8f` + docs; sequential, dev Postgres via `just pg-up`)

| Suite | Result |
|---|---|
| `just test-one game "--no-fail-fast"` | lib 159 passed, 3 failed (the known `lod::tests` reds); `it` 940 passed / 315 ignored; hash_mode 7, state_hash_check 1, voxel_body 20, wasm_determinism 7 — ok |
| `just test-one world-store "--no-fail-fast"` (SQLite) | unit 6; `it` 140 passed, 1 failed (the known `codec::golden_blobs_decode`), 42 ignored |
| `just test-pg "--no-fail-fast -- --test-threads=4"` | unit 6; `it` 140 passed, 1 failed (the known `codec::golden_blobs_decode`), 42 ignored |
| `just test-one server "--features test-hooks --no-fail-fast"` | ok: lib 106, it 29, login_link 7, observer 3, store_world 4, dev_structures 2, player_store 2, roles 2 (the rest slow-ignored there) |
| `just test-threads` | 290 + 3 + 10 ok |
| `just test-r2` | 19/19 |
| `just test-r3` | 44/44 |
| `just test-r4` | 24/24 (the watch test `a_clean_leave_…` ok) |
| `just determinism-wasm` | ok |
| `just smoke` | ok |

No red outside the known ones. Client tests were not run (Dawid's hand check).

### Final review fix wave (2026-11-17)

Three area reviews of `0985ab69..dbc0e8b9` (findings: `.superpowers/sdd/…/final-fix-wave-findings.md`); one fixer,
base `dbc0e8b9`. Commits `1cd94ea9` (core), `e3152145` (runtime), `e578b8b5` (store) and the docs/scripts commit
after them. Every new test was RED on the unfixed code first.

- **C1 (conservation).** A coordinator that died between the store's commit of a decision and the transition
  holding its answer left the predecessor in the view (people embodied in its regions, players online on it).
  Now `successions_open` takes every run new to the core out of the view (`host_succeeded`) at a START read too,
  and `observe_leases` ends every dirty dead row nobody here declared (`host_ended`, then the re-read as before).
  Tests: `coordinator::a_store_decision_lost_with_its_transition_still_takes_the_dead_host_out` (both cases) and
  the seeded `peer_schedule::a_coordinator_dying_between_a_store_decision_and_its_transition_conserves_the_world`
  (an end, then a succession, each committed with the coordinator restarted before its answer;
  `assert_conserved_world`, the run settles, the dead region restarts on the heir). The rig gained
  `declare_unanswered` / `end_unanswered` and answers a re-read in `drive_leases` (as the runtime does).
- **I1.** The calendar re-anchors where it last stood (`calendar_resume`: the anchor's tick at `calendar_seen_ms`,
  or the history's `now` if later), at the load already; a view moves `calendar_seen_ms` (durable before it
  leaves). Test `the_calendar_never_runs_backwards_across_a_load` (the reviewer's probe: was 16800000 < 16801200).
  The history's own `now` follows at the first `maintain` (not advanced at the load: that could queue directives).
- **I2.** `first_of_generation` also detaches every older host still attached. Test
  `the_first_attach_of_a_new_generation_detaches_the_older_hosts`.
- **S1 (ruling: monotonic).** The runtime stamps `hosts` reads on the process's monotonic clock; the core asks the
  watch by its reads (`lapsed_by_reads`), never comparing the store-clock `now`. `StoreClock`'s forward step is NOT
  bounded (decided): a sample is the store's word, the existing test asserts a forward sample is taken at once, and
  what a step overtakes fails safe (a cut aborts, a succession deadline rolls back, the calendar re-anchors). Test
  `takeover::a_forward_step_of_the_store_clock_lapses_nobody` (hook `step_store_clock`; RED with store-clock stamps).
- **S-M1.** `register_in` counts a dirty row toward a total crash only if live, landed, or an open run's
  predecessor. Test `succession::an_ended_row_is_no_total_crash_after_a_clean_stop`.
- **G1** `take_transition` returns `None` while writing (four tests now acknowledge the writes they treated as
  committed — `written()` — no assertion removed); **G2** `forget_absent_hosts` prunes `cuts.attached` and
  `boots_seen`; **G3/G4** docs; **G5** `WorldGeneration::why` in the stop logs; **S-M2/S-M3** wording;
  **S-M4** WATCH test diagnostics (message only); **D1** CLAUDE.md crash-loop sentence; **D2** go-nogo-r3 note;
  **D3** `crossing_summary.py` matches `event=autopilot_seam ` (regression test) + TODO note; **D4** `RegionHosts`
  is a session reset; **D5** `twin_check.sh` phase 5 asserts host b `land=0`; **D6** TODO notes.

Gate (sequential): game lib 159 + the 3 known `lod::tests` reds, `it` 945; world-store SQLite and Postgres 141 + the
known `golden_blobs_decode` (Postgres with ignored: 183 + it); server (test-hooks) ok, `coordinator_rt` 11/11,
`login_link` 12/12; `test-threads` 291 + 3 + 10; r2 19/19; r3 44/44; r4 25/25 (WATCH test green); determinism-wasm,
smoke, web-check, client `--no-run` ok; Python 21 ok; `bash -n scripts/twin_check.sh` ok (the twin was not run).
