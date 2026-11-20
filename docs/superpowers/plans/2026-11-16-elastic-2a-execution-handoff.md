# Handoff: execute Elastic 2a (the coordinator's state in the store), subagent-driven

**Date:** 2026-11-16
**For:** a fresh session.
**Job:** execute `docs/superpowers/plans/2026-11-16-elastic-2a-coordinator-state-in-the-store.md` with `superpowers:subagent-driven-development`, one task at a time, on `develop`, in this checkout. After all 7 tasks: a whole-branch review, one fix wave, then the plan's "As built". **E2b (replicas) gets its own plan after this is built. Do not start it.**

## 1. Read first, in this order

1. `CLAUDE.md`: the rules. Its "Server / store" section describes E1 as built.
2. The plan above, whole. Its "Where this plan departs from the handoff" and "Decisions settled in planning" sections are binding.
3. `docs/superpowers/plans/2026-11-16-elastic-2-coordinator-planning-handoff.md`:
   - §6, the rules that carry over;
   - §7, how E1 was executed, which is what worked.
   - Its §4.2–§4.4 are superseded by the plan (see "How we got here").
4. `docs/superpowers/plans/2026-11-15-elastic-1-hosts-as-deployments.md`: only "As built" and "Whole-branch review and fix wave" at its end. That is what the code is today.
5. `TODO.md`: the "Elastic 1" block (known limits and known reds).

## 2. How we got here (so nobody re-litigates it)

- **Draft 1 was rejected.** It kept a stateful coordinator core with an active/standby pair fenced in Postgres ("Ruling A").
- **Dawid's direction:** the coordinator should work like a web service. Its state lives in Postgres, it keeps no state of its own, and it uses SQLite the same way in single-player.
- **What the code showed:**
  - The coordinator saves `coordinator.state` only every 60 s.
  - Only claim grants and directives wait for a save.
  - Everything else is memory-only and is rebuilt from host `Attach` messages: presence, region owners, cut and succession runs, the calendar anchor, and history's `#[serde(skip)]` fields.
- **So the work is split:**
  - **E2a (this plan):** every change is one transition — apply the inputs, write the state with a compare-and-set on `version`, then send. It runs with ONE coordinator process.
  - **E2b (later):** N replicas over the same row.
- **History stays a blob.** Measured in local worlds (`worlds/debug-embarked`, `worlds/world-72e2`), the whole lz4 state is 43–52 KB. Position reports come once per region per world cut (`region_runner.rs:412`). All history rules are deterministic with stateless rolls. There are two blobs: `core` (every transition) and `history` (only when its content changed).
- **Dawid asked for this handoff for subagent-driven execution.** He did not answer the plan's "To confirm" items. Proceed on the plan's defaults and note each as a ruling in the ledger:
  1. The residual: a host that claims land during an open succession and dies before the settle still rolls the world back.
  2. Offline `worldctl retire-host` is refused.
  3. `History::after_load`'s blanket return of embodied people to the abstract history is removed.

## 3. Order and gates

Run the tasks in plan order: T1 → T2 → T3 → T4 → T5 → T6 → T7.

| Task | What | Gate |
|---|---|---|
| T1 | `go-nogo-r3` probe repair | Touches client code. Compile only (`just test-one client "--lib --no-run"`). **Never run client tests without asking Dawid.** Ends with the probe command for Dawid, not run. |
| T2 | The crash-loop rule | Store, game and seeded tests, `test-r4`. Independent of T3–T6. |
| T3 | The store clock | — |
| T4 | History v4, a load is not a restart | **The risk gate.** The existing coordinator-restart crash tests in `test-r2`, `test-r3` and `test-r4` must pass UNCHANGED. If one fails, stop: it is a defect of decision 4. Report to Dawid; never weaken a test. |
| T5 | The whole core as one projection, plus migration `0016` | The big rewrite. The reload-after-every-input identity test is the proof. Keep the runtime working through a thin adapter. |
| T6 | Write before send | Ends with `just twin-check` (allowed without asking). Record the write counters in the ledger; they are E2b's first measurement. |
| T7 | Docs | — |

## 4. Facts already established (save the next session the research)

The code as of `a92144da`; line numbers approximate.

### The coordinator core: `crates/game/src/coordinator/`

**State**
- `mod.rs:102-178`: the `Coordinator` fields.
- `state.rs`: `CoordinatorState { history: Vec<u8>, claims, memo }`, v2.
- `from_state` `:223`, `state()` `:566`, `take_save` `:526`, `saved` `:536`, `commit_unstored` `:557`.

**Inputs and the clock**
- `handle(host, msg, now_ms)` `:277`.
- `maintain` `:452`. `calendar_now` `:271` runs from a process clock: `started.elapsed()` in `crates/server/src/coordinator_rt.rs:1074`.

**Outputs and gating**
- `push` / `gated` `:593`.
- `CoordMsg::is_gated` `msg.rs:353`: Directive, and a Reply that grants a claim.
- `push_directive` `:610`.

**Memo, attach, presence**
- Memo: `remember` `:988`, `forget_settled` `:1011`. It is keyed by `RequestId { host, boot, seq }`.
- Attach: `attach` `:678`, `cut_attach` (`world_cut.rs:423`), `succession_attach` (`succession.rs:606`).
- Presence: `player_online` `:755`, `player_offline` `:799`, `player_handed` `:838`, `request` `:941`, `settle` `:911`.

**Cuts, leases, successions, the gate**
- `WorldCuts` (`world_cut.rs:172`) and `CutRun` (`:139`): `maintain_cuts` `:481`, `take_decision` `:339`, `decided` `:356`.
- `LeaseWatch` (`succession.rs:115`) and `HostSeen` (`:81`).
- `SuccessionRuns` (`:245`), `Run` (`:198`): `declare_wanted` `:413`, `maintain_successions` `:678`, `heir_candidates` `:904`, `leave_wanted` `:915`.
- `gate_host` (`mod.rs:478`).

### History: `crates/game/src/history/`

- `History` (`mod.rs:42-87`):
  - Saved: seed, now, people, sites, civs, journeys, queue, next_seq, next_ids, log, positions.
  - Skipped: grid, running, stopping, directives, bounced, stray_candidates, held_since, notices.
- `persist.rs`: `encode` / `decode` `:63`, `HISTORY_FILE_VERSION = 3`.
- `after_load` (`handover.rs:575`) returns every embodied person to the abstract history.
- `reattach_regions` (`handover.rs:399`) is exact per region. `region_parked` `:513`, `person_crossed` `:657`, `adopt_body` `:461`, `note_positions` `:530`.
- `advance` (`mod.rs:283`).
- Calendar: `TICK_RATE` = 50 ms, `DAY_MS` = 10 000.
- The old `history(id, bytes)` table is read once to migrate, in `server/src/world_file.rs:401` `ensure_coordinator_state` (claim, then `load_coordinator`, then the legacy `load_history`, else bootstrap).

### The runtime: `crates/server/src/coordinator_rt.rs`

- `run` `:1078` (one thread, `COORDINATOR_TICK_MS` = 50).
- `pump_save` `:1725`, `finish` `:1802`, `fence_out` `:1846`.
- `feed_cuts` `:1340`: a `hosts` read every 1 s, which feeds the lease watch.
- Store pumps: `pump_cuts` `:1414`, `pump_successions` `:1701`.
- Role `all`: `start_all_in_one` `:794`, the same runtime on SQLite.
- Exit codes are in `server/src/claims.rs`: `EXIT_FENCED` 3, `EXIT_COORDINATOR_FAILED` 4, `EXIT_WORLD_ROLLBACK` 5.

### The store: `crates/world-store/src/`

**The coordinator row**
- `rows.rs`, inside `engine_rows!` (both engines):
  - `claim_coordinator` `:985`: unconditional `epoch + 1`.
  - `save_coordinator` `:998`: `WHERE epoch = $e`, otherwise `Fenced { sr: None }`.
  - `load_coordinator` `:1015`.
- `hosts()` `:833`, `register_host` `:593`, `heartbeat_sql` `:178`.

**Successions** (`succession.rs`)
- `declare` `:252` and `declare_in` `:317`.
- Survivors: every dirty live row, at `:389-400`.
- `lock_coordinator_and_world` `:219`, `rollback_in` `:503`, `settle` `:545`, `PG_NOW_MS` `:244`.
- The module notes (`:1-147`) hold every lock walk: add new ones there.

**Cuts and the operator**
- `two_phase.rs`: `decide_cut` `:188`, `record_cut_begin` `:337`, `resolve_prepared` `:545`, `abort_cut` `:372` (unfenced). Nothing is ever pruned today.
- `operator.rs`: `retire_offline` `:190`, `vouch_host` `:337`; `ctl.rs` `retire_on` `:445`.

**Engines and migrations**
- No trait: `Pool::{Sqlite, Pg}`, and `engine_rows!` is instantiated at `rows.rs:1835-1836`.
- Two-phase and successions are Postgres-only; SQLite answers `Unsupported` (`thread.rs:390-521`).
- Migrations `0001`–`0015`; the next is `0016`. `tests/schema.rs` pins the columns.

### Tests and rigs

**The seeded rig** (`crates/game/tests/common/multi_rig.rs`)
- ONE real `Coordinator` in `SharedCore` (`coordinator/link.rs:48`), over `InlineWorldLink` (`link.rs:130`, `commit_unstored`).
- The in-memory store: `declare_in_store` `:1429`, `run_succession` `:1376`.
- `kill` `:1205`, `start` `:1269`, `assert_conserved_world` `:1934`.
- `sweep` (`peer_schedule.rs:38`): 12 seeds, 3 modes.

**Server rigs**
- `crates/server/tests/common/hosts_rig.rs`: `kill_coordinator` `:897`, `restart_coordinator_listening` `:907`.
- `takeover.rs`: `Takeover::start` `:278`.
- `roles.rs`: `Coordinator::start` `:173`.

**Existing coordinator-restart tests — T4 and T6 must keep them passing unchanged**

| File | Tests |
|---|---|
| `hosts.rs` | `:848`, `:878` (`coordinator_killed_*`) |
| `takeover.rs` | `:1020`, `:1072` |
| `roles.rs` | `:792` `host_survives_coordinator_absence_at_start`, `:1319` `coordinator_crash_mid_play_conserves_people`, `:1428` `embark_during_downtime_completes_once`, `:1487` `embark_answered_then_crash_is_not_refounded` |

**The `go-nogo-r3` probe (T1)**
- `crates/client/src/renderer/autopilot.rs`: `pick_loaded_seam` `:569`, `pick_seam` `:601`, `rendezvous_owner` `:693`. It assumes host ids `0..hosts`.
- `ConnectionSet` (`crates/client/src/connections.rs:121`) has the real map: `connection_of` `:186`, `ordinal_of` `:171`.
- `scripts/crossing_summary.py` `invalid_reasons` `:312` scans whole logs for `SUCCESSION_MARKS`.
- The recipe (`justfile` ~`:1495-1666`) keeps its database between runs.

**Embark is to be deleted** (CLAUDE.md since 2026-11-16). Tests named `embark_*` stay as conservation tests of claims and requests; build nothing new on embark.

## 5. Execution rules (E1's lessons; carry into every dispatch)

**One implementer and one reviewer per task**
- Reviews cover the spec and the code quality.
- Ledger: `.superpowers/sdd/2026-11-16-elastic-2a-coordinator-state/progress.md` (create it).
- Rulings are written as `Ruling: what — why — cost if wrong`.
- Per-task reports sit beside the ledger.
- The whole-branch review at the end is split by area (store and server; the `game` crate; client, scripts and docs), followed by one fix wave and one scoped re-review.

**Dawid edits this checkout at the same time**
- He has uncommitted hunks in `CLAUDE.md`, `TODO.md`, `justfile` and `deploy/**`, plus untracked `crates/panel/`, `deploy/modules/panel/` and others. **Never touch the panel.**
- Every dispatch carries the filtered-staging rule:
  - `git diff -U3`, keep your own hunks, `git apply --cached --recount`;
  - never `git add -A`, `git stash` or `--unidiff-zero`.
- The controller verifies each commit's `--stat` and that Dawid's hunks are still uncommitted.

**Other sessions run here too**
- One once ran `rustfmt` over the whole workspace.
- When a subagent reports the tree does not build, check `git status` for foreign changes before blaming the task. Suites may run on a `git archive` export of the commit.
- Never run `rustfmt` / `cargo fmt` from the root or with `--all` (memory: `quest/` formatting only).

**Edits**
- Subagents edit with Edit / Write, one file per call.
- A refused command is reported, never worked around.

**Tests**
- Go through `just`. Filters go after `--` (memory: `just-test-one-filter-syntax`).
- `roles`, `coordinator_rt`, `hosts`, `takeover` and `login_link` need `--features test-hooks`.
- `just pg-up` must be running for `test-pg` and `test-r2|r3|r4`. Run Postgres suites one at a time.
- `just smoke` after every Rust task.
- After a wide change, run each touched crate's whole non-client suite once.
- `just determinism-wasm` after T4 (history is serialised and hashed).

**Ask Dawid first**
- Client tests, live runs (`dev-r3`, `go-nogo-r3`, `just run`), measurements, and anything against orion.
- `just twin-check` is allowed.

**Known reds, not ours**
- The three `lod::tests` in the game lib.
- `codec::golden_blobs_decode`.
- Flaky SQLite `stud_refusal::…`.
- Flaky `svc-link` `dialer_reconnects_after_listener_restart`.

**Commit trailers:** each subagent's own `Co-Authored-By` for its model, plus the session line the harness gives.

**Worlds are re-forged, never migrated** (Dawid, 2026-11-15). Migration `0016` has no data path; the docs say `just pg-reset` / `just run --reset`.

## 6. Where the plan is most likely wrong (watch these)

E1 lost the most time to plan defects. These are the places this plan guessed.

| Task | What may not hold |
|---|---|
| T4 | Removing `after_load` assumes every restart path re-attaches each host and that `attach` parks unlisted live regions. Role `all` restarts host and coordinator together: check that path first (`single_player_restart_…` in T6, the `roles.rs` crash tests in T4). |
| T5 | `region_host` "gives way to the history's `running`". This was not verified. If `region_host` carries the host for regions history does not track as running, keep both, and record it as a ruling. |
| T5 | Attachments stamped by `ProcessBoot`: under role `all` the inline link must count as attached at start. |
| T3 | Deadlines re-based by `CALENDAR_GAP_MS`: a run's deadline must never move EARLIER. |
| T6 | Re-applying kept inputs after a failed write must not double anything. The memo makes `Request`s idempotent, but check presence and lifecycle messages (`PlayerHandedTo` hop ordering) under a retry. |
| T2 | The seeded rig's in-memory store must mirror the new survivor query exactly, or the seeded tests prove the wrong rule. |
