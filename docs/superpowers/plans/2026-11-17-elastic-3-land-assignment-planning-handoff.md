# Handoff: Elastic 3 — the coordinator assigns land; idle land is released

**Date:** 2026-11-17
**For:** a fresh session, **after Elastic 2b is built**. Check that `docs/superpowers/plans/2026-11-17-elastic-2b-coordinator-replicas.md` has an "As built" section. If it has none, stop and say so: E3 is planned against E2b as built, not as planned.
**Job:** write the E3 implementation plan with `superpowers:writing-plans` as `docs/superpowers/plans/<date>-elastic-3-land-assignment.md`, have Dawid look at it, then execute it subagent-driven. One plan. E4–E6 get their own handoffs after it.

The decisions in §4 are defaults this handoff takes so the plan can proceed. They come from reading the code at `5765b7d4` (E2b Tasks 1–2 landed, Task 3 in the working tree). **Verify each against E2b as built.** Where the code contradicts one, re-rule it in the plan under "What the handoff got wrong" and keep going: do not stall. The ones marked **(Dawid)** are listed again in §9.

## 1. Read first, in this order

1. `CLAUDE.md`, the rules. Its "Server / store" section describes E2a (the coordinator's state in the store) and, once E2b's docs task has run, E2b.
2. The design spec, `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md`:
   - decisions 4 (the coordinator assigns land), 5 (lazy assignment, idle land released) and 9;
   - §5 step 1, "empty land";
   - §6, the failure table.
3. The parent handoff, `docs/superpowers/plans/2026-11-14-elastic-region-hosts-planning-handoff.md`: §3 "E3", §4 and §5.
4. The E2b plan, `docs/superpowers/plans/2026-11-17-elastic-2b-coordinator-replicas.md`: "Where this plan departs", "Decisions settled", "As built". That is the machinery E3's assignment rides on: transitions, decisions inside the core's transaction, the ticker, the outbox.
5. The E2a plan: "Decisions settled in planning" and "As built". The E1 plan: "As built", fix wave.
6. `TODO.md`: the Elastic 1, 2a and 2b blocks (known limits, hand checks).
7. `docs/game-design.md` §5 and its glossary (`:230`). A player's land is a **holding** in code, because "claim" already means a host's super-regions. E3 must not add a new "claim" name for anything.

## 2. Where things stand (code map at `5765b7d4`, working tree; line numbers approximate — re-check after E2b)

**Ownership** (`crates/game/src/ownership.rs`)
- `super_region_of` (:15).
- `Owner { Me, Host, Free, Unknown }` (:216).
- `trait Ownership` (:228); the implementations `AllInOne` (:269), `Static { me, layout }` (:301) and `PgClaim { id, directory }` (:467).
- `rendezvous_among` (:176), `heir_of` (:204).
- `NO_OWNER = -1` (:49).
- `Directory` (:356):
  - `rows`, `live`, `draining`;
  - `preferred(sr)` (:457) = a rendezvous over live, non-draining hosts. **This is the claim preference E3 removes.**
- The fence is `world_store::Fence { owner, epochs, all_hosts }` (`world-store/src/fence.rs:21`).
- `commit_check_set` (:70) is every super-region the cut touches **plus every super-region in the fence**. A released super-region left in the fence therefore fences the host's next cut.

**How a host gets land today, on its own** (the coordinator is not involved)
- A need is recorded by `note_unowned(rc)` (`world_manager/peers.rs:1197`). It is called from a move, a waking crossing, a join or restore, placement, `subscribe_elsewhere` and `wait_for_owner`.
- `ask_for_claims` (peers.rs:1290):
  - sends `Volatile::ClaimWanted(sr)` to the preferred host;
  - re-asks after `CLAIM_UNANSWERED_REREAD_MS`.
- `take_claims_wanted` (peers.rs:1234) feeds `DirectoryKeeper::pump` (`server/src/claims.rs:611`).
- The keeper calls `claim_free_later` (store `rows.rs:412`). `claim_free` is insert-or-nothing, then `UPDATE … WHERE owner = -1`, then `check_live` last. It is first-writer-wins and never locks a row another host holds.
- On success:
  - `HostClaims::add` (claims.rs:296, fence first);
  - `claimed` (peers.rs:1388): directory set → `Reliable::Owns { rows }` to every peer → `flush_unowned`.
- Pacing: `CLAIM_RETRY_MS` / `CLAIM_NOTHING_WON_RETRY_MS` (claims.rs:435/439), `CLAIM_WANTED_EVERY_MS` (mod.rs:97).
- Departed land: `ask_for_land` (peers.rs:1337) → `HostMsg::LandWanted { sr }` → the coordinator's `land_asked` / `land_wanted` / `leave_wanted` (`coordinator/succession.rs:1262–1335`) → a `Leave` succession giving ALL of the leaver's land to one heir.

**Release**
- `release` (`rows.rs:489`, handle.rs:907) exists but is **called only by tests** (`world-store/tests/directory.rs`). It refuses while the handle holds an undecided staged part (`CutPending`).
- **No fence ever shrinks.** `HostClaims::add` and `grow_to` only grow; `replace` is used only by `reclaim_own` at boot.
- `Directory::replace` never deletes a row.
- **No peer message says land is free.**
- The seeded rig has a rig-only release: `free_land_of(h)` (`multi_rig.rs:1310`).

**Idleness**
- Idleness exists only per region:
  - `RegionLink.subscribers` and `empty_since_ms` (`refresh_keepalive`, mod.rs:4316);
  - parking after `UNLOAD_GRACE_MS` (5 s) in `maintain` (:2373);
  - `RegionOutput::Stopped` → `parked` (:947, in memory).
- A parked snapshot stays in `uncommitted` (:1152) until a committed cut carries it (`begin_cut_at` :2512, `cut_committed` :2584).
- Other per-place holders a released super-region must be empty of:
  - `pending_arrivals` and `in_flight` targets;
  - `unowned_waiting` and `intent_mailbox`;
  - sessions' `homes` and `asked_elsewhere`;
  - `succession_pinned` and `inheriting`.
- There is no per-super-region idle aggregate anywhere.

**Load**
- **None anywhere.** The beat (`heartbeat_sql`, rows.rs:178) only updates `seen_ms` and returns the drain mark.
- `HostRow` (handle.rs:48) adds the derived `owns_land` and `succeeding`.
- The coordinator's `HostSeen` (succession.rs:104) has no load.

**The coordinator after E2b** (verify in its "As built")
- A transition is: apply the inputs → `take_transition` (`core`, `history`, `out`, `outbox`, `decisions`) → one transaction. That transaction runs:
  1. the compare-and-set on `version`;
  2. each store decision in a savepoint;
  3. the outbox writes;
  4. `pg_notify`.
- A store decision is committed with the core that decided it. Its answer is the next pass's input.
- The **ticker** (the lowest live replica) alone does timed work, the `hosts` read and the land lookups (`LandOwners`).
- An outbox row reaches a host linked to another replica, at most once.
- **E3's assignment is one more decision of this kind.**

**Tests that pin claiming today** (all to be revisited; a test that asserted the rendezvous preference changes meaning, it is not weakened)
- Store: `world-store/tests/directory.rs` (claims, drain, release).
- Server: `server/src/claims.rs` unit tests (:954–1142); `server/tests/hosts.rs` `fresh_world_under_claim_lays_out_like_static` (:1953); `takeover.rs` `drained_host_is_inherited_on_demand_and_nothing_is_lost` (:1666) and `a_leavers_restart_…` (:1957).
- Seeded rig: `with_land_on_demand` / `drive_land` (:683/:696), `with_claims` / `drive_claims` (:1224/:1263). Tests in `peer_schedule.rs`:

  | Test | Line |
  |---|---|
  | `a_free_super_region_is_claimed_by_its_preferred_host_when_a_player_walks_to_it` | 5867 |
  | `claim_wanted_reaches_the_preferred_host` | 5905 |
  | `a_draining_hosts_share_of_free_land_is_claimed_by_another` | 5950 |
  | `every_host_draining_claims_nothing_and_waits` | 6026 |
  | `a_late_peer_learns_every_claim_at_its_hello` | 6103 |
  | `player_logging_in_on_a_departed_hosts_land_is_placed_on_the_heir` | 8442 |
  | `an_ask_a_draining_host_drops_makes_the_asker_reread_within_its_bound` | 10430 |
  | `transfer_into_a_free_super_region_waits_and_is_a_row_of_the_senders_cut` | 5728 |

## 3. What E3 delivers

- **The coordinator decides who owns free land.** A host that needs a free super-region asks the coordinator, and the coordinator assigns it to the least-loaded live host that takes logins.
- **New hosts fill from new activity.** Nothing running is ever moved.
- **Idle land is released** inside a committed world cut. The host's fence shrinks for the first time, peers learn the land is free, and a later need assigns it afresh.
- **Hosts report load** in their heartbeat row: players, running regions, tick headroom.

What this closes:
- **E1:** "after a total crash the first host back holds all claimed land", because its idle land drains back to free.
- **E1:** "a `Leave` succession gives ALL of a leaver's land to the first host that asks". See §4.6.
- **E4's prerequisites:** load reports and empty hosts that can be removed.

## 4. Decisions taken (defaults; verify against E2b as built)

### 4.1 Assignment is a coordinator decision inside its transaction

- A host's need becomes `HostMsg::LandNeeded { sr }`.
  - It replaces `Volatile::ClaimWanted`, `claim_asks`, `Directory::preferred` and the host-side `claim_free` path.
  - It also replaces `HostMsg::LandWanted`: one ask, and the coordinator looks at the owner. **Verify the merge is clean against `land_asked`.**
- The core queues the ask. The ticker's `LandOwners` read tells it the owner:
  - **Free (absent or `NO_OWNER`)** → a decision `AssignLand { sr, host }`. It runs in the transition's savepoint as one statement pair: insert-or-nothing, then `UPDATE super_regions SET owner = $h, epoch = epoch + 1 WHERE … AND owner = -1 RETURNING epoch`, plus the assignee's live, dirty, not-draining check, under the lock order of the claim it replaces.
    - The answer is `Assigned { sr, host, epoch }` or `Refused`.
    - On `Assigned` the core sends `CoordMsg::Assigned { rows: [(sr, epoch)] }` to the assignee: direct, or through the outbox.
  - **Owned by a live host** → `CoordMsg::LandIs { sr, owner, epoch }` back to the asker, so it re-reads less.
  - **Owned by a dead or departed host** → the existing succession paths.
- The assignee:
  - grows its fence (`HostClaims::add`) **before** it uses the land;
  - announces `Reliable::Owns`, as `claimed` does today;
  - flushes its waits.
- A message lost after the commit is healed by the host's next ask or its directory read (at most once, as E2a decision 3).
- **Who is chosen (Dawid):**
  - Among attached live hosts that take logins (`takes_logins`: not leaving, not draining), with load from the latest `hosts` read, pick the fewest players, then the fewest running regions, then a rendezvous on `sr` as the tie-break, so equal loads spread deterministically.
  - A host whose reported headroom is below `ASSIGN_HEADROOM_MIN_PERMILLE` is skipped while another qualifies.
  - The core counts super-regions assigned since a host's last beat as provisional load, so a burst does not all land on one host.
  - **Crash-loop guard:** a host attached for less than `ASSIGN_MIN_ATTACHED_MS` is skipped while an older one qualifies. A fresh pod is the likeliest to die, and assigning it land turns its death from a landless end into a succession.
- **Static and all-in-one** are unchanged: one host owns everything, and the coordinator assigns nothing. Only `claim` worlds assign.
- **With every replica down, no land is assigned.** Hosts already holding land run on. This is new: today a host can claim free land with no coordinator. It is acceptable after E2b (replicas), and stated in the docs.
- **`claim_free`** stays in the store only if a tool uses it; otherwise it and its tests go, and its lock-walk note becomes `AssignLand`'s.

### 4.2 Load in the heartbeat row

- Migration `00NN` (the next after E2b's `0017`) adds to `hosts`:
  - `load_players BIGINT`
  - `load_regions BIGINT`
  - `load_tick_permille BIGINT`: the region threads' worst recent tick time over the tick period, ×1000.

  All default 0, on both engines.
- The beat statement sets them. It stays one row, wants nothing else, and stays on the beat connection.
- `Beat` is unchanged. `HostRow` and `HostSeen` gain a `load`.
- The values are the host's own counters, sampled when the beat is sent. No new thread.

### 4.3 Release inside the host's own cut part

- At `BeginCut`, a host lists its **idle** super-regions: owned, and for at least `LAND_IDLE_MS`:
  - no running or stopping region there;
  - no `uncommitted` snapshot there, except one this very cut carries;
  - nothing in the §2 holders aimed there;
  - no session home or `asked_elsewhere` there;
  - not pinned, not inheriting.
- **Its prepared part frees them itself:** `UPDATE super_regions SET owner = -1, epoch = epoch + 1 WHERE (sx, sz) = … AND owner = $me AND epoch = $e`. The release therefore commits exactly when the part commits (`COMMIT PREPARED`) and rolls back with it.
  - `decide_cut` needs no new statement. It cannot take a row a prepared part holds anyway: it runs before `finish_staged`.
  - **Check this against the part's fence check:** the released rows are written, not only `FOR SHARE`-checked. Write the lock walk.
- **From `BeginCut` until `CutDone`, a releasing super-region is frozen on that host:** no region loads there, and a need there waits as `Unknown`.
  - Committed → the fence shrinks (`HostClaims::shrink`, new), the directory marks the rows free, and the host tells its peers (§4.4).
  - Aborted → the land stays its own, and the freeze lifts.
- The part carries the released list (`CutPrepared { cut, released }`, or in the part rows), so the coordinator knows without a read.
- **Must settle in the plan:** what arrives for a released super-region after the cut's tick but before `CutDone`, and after `CutDone`. A transfer, a player hand-off, a halo or intent message all have to become a wait for the new owner, and a row of the holder's next cut, exactly once. The existing `unowned_waiting` / `wait_for_owner` path is the model.

### 4.4 Peers learn land is free

- `Reliable::Released { rows: Vec<(SuperRegion, u64)> }` is appended to the peer enum. Like `Owns`, it is not a state message and never `InCut`.
- It is sent to every peer after the commit, and on each peer hello the releaser no longer lists those rows. A peer sets the rows `Free` unless it already knows a newer epoch.
- The store stays the truth: a lost `Released` is healed by the next directory read.
- **The link protocol is bumped once for the plan:** `LinkHello` and `PeerHello` versions, and append-only enums.

### 4.5 A total crash's land drains back

- The first host back still takes every dirty row's land (E1 decision 4, unchanged).
- Its idle super-regions are released by §4.3 after `LAND_IDLE_MS`.
- No special path.

### 4.6 A clean leave releases instead of being succeeded (Dawid)

- A leave cut already parks every region of the leaver. Its part releases ALL of the leaver's land, idle by construction, so a cleanly departed host owns nothing, and `LandWanted` → `Leave` succession is never needed.
- **Must settle:** the leaver's own `in_flight` rows (waits for unowned land, keyed to its host id). Today a `Leave` succession re-keys them to the heir.
  - Either the leave part re-keys them to "whoever owns the target", which needs an `in_flight` row without a host: E5's re-key, too big.
  - Or the `Leave` succession is kept only for a leaver that still has `in_flight` rows.
  - Default: keep it for that case.
- Remove the rest of the `Leave` path only if the plan proves it is dead.

### 4.7 Tunables (named consts; defaults to argue in the plan)

| Constant | Default | Note |
|---|---|---|
| `LAND_IDLE_MS` | 300 000 (5 min) | So a player's short walk away does not churn ownership. |
| `ASSIGN_HEADROOM_MIN_PERMILLE` | 200 | |
| `ASSIGN_MIN_ATTACHED_MS` | 30 000 | |
| `LAND_NEEDED_EVERY_MS` | 1 000 | Takes `CLAIM_WANTED_EVERY_MS`'s place. |

### 4.8 What does not change

- Fencing in the store is the only safety. An assignment and a release are both epoch bumps in one transaction, and the old holder of an epoch is fenced at its next write.
- `static` and `allinone` are unchanged.
- The client: a released super-region has no running region and no subscriber, so no client sees the change. An assignment looks to a client like a claim does today. **Verify**: the client re-maps regions whose owner changed (`connections.rs`).
- Successions, the crash-loop rule and the drain are unchanged, except §4.6.

## 5. Order of work (the plan decides; this is a suggestion)

1. **The `go-nogo-r3` baseline.** E3 changes who owns land under a moving player, so the parent handoff's rule applies: rerun the seam probe and compare pooled runs. The probe has never had a valid run since E1. Its first valid run (E2a Task 1 left the command for Dawid in `TODO.md`) must exist BEFORE E3 changes ownership. If `TODO.md` shows it has not run, the plan's first step is to ask Dawid for it.
2. Load in the heartbeat row (migration, beat, `HostRow` / `HostSeen`). Small; nothing uses it yet.
3. The seeded rig: assignment by a coordinator decision in place of `drive_claims` / `claim_store`, and a release in the rig's cut store, so every later task has a seeded layer. E1 and E2b both started with their rig.
4. The coordinator assigns: `LandNeeded`, `AssignLand`, `Assigned` / `LandIs`, the host side, and the preference removed. The tests listed in §2 are re-pinned to the new rule.
5. Release in the cut part: the idle aggregate, the freeze, the part's update, the fence shrink.
6. `Released` to peers, and everything arriving for released land.
7. The clean leave releases (§4.6).
8. Metrics and logs:
   - `mmo_land_assigned_total`, `mmo_land_released_total`;
   - per-host load gauges on the host's `/metrics`;
   - an assignment count on the `[coordinator] window=` line;
   - the twin check gains an "assigned, idle, released, re-assigned" step.
9. Docs and closing. The rerun command for `go-nogo-r3` and what it must match.

**Sizing:** 7–9 tasks, against the design's 4–6. The design did not count the clean leave, the rig or the arrivals into released land.

## 6. Rules that carry over

- **Conservation beats latency; wait, never guess.** A task that changes who holds what is not done without a conservation test. E3's tables must include:
  - a player walking into land being released at the cut;
  - a transfer into land released a tick earlier;
  - a host that dies after its release committed but before its peers heard;
  - a coordinator replica that dies mid-assignment;
  - two hosts asking for one super-region (exactly one assignment);
  - an assignee that dies before learning its land (a landed death: a succession, not a landless end);
  - release and assignment racing a succession.
- **Fencing in the store is the only safety.** Never add a store write that skips the fence. **A cut part writes only rows of its own land**, and the released `super_regions` rows are its own until its commit.
- **Lock walks:** every new statement gets its walk in `world-store/src/succession.rs`'s notes. This covers `AssignLand`, the part's release update and the load columns in the beat. A prepared transaction is not a waiting backend.
- **Migrations** are immutable and identical in shape on both engines. Worlds are wiped, never migrated (Dawid, 2026-11-15).
- **Wire enums are append-only.** Bump the link protocol once per plan. A new `WorldManager` field goes in `host_fields!`, and a new core field in E2a's `projection!`. A new state-bearing `Reliable` is journalled or argued out (`Released` is not state-bearing: argue it).
- **E2b's replay rules:**
  - Classify every new input as replayed or ticker-only, as E2b did. `LandOwners` and the `hosts` read with load are ticker-only.
  - Classify every new decision's answer with its `CallStamp`.
- Scheduling-sensitive correctness goes in the seeded layer (`peer_schedule.rs`, `multi_rig.rs`). Real hosts over real links go in `server/tests/{hosts,takeover}.rs`.
- **Ask Dawid first** before client tests, live runs, measurements (`go-nogo-r3` included) and anything against orion. `just twin-check` is allowed.
- After a wide change, run each touched crate's whole non-client suite once. Run `just test-threads` (seams and `WorldManager`) and `just smoke` after every Rust task.

## 7. How the last plans were executed — keep what worked

- **Subagent-driven:** one implementer and one reviewer per task. Keep a ledger in `.superpowers/sdd/<plan>/progress.md`, with rulings as `Ruling: what — why — cost if wrong`. End with an area-split whole-branch review, one fix wave and a scoped re-review.
- **Plan defects in execution were the expensive part every time.** For E3 the risks are:
  - §4.3: the part's own release update against its fence check and `finish_staged`;
  - §4.3: arrivals into land released mid-cut;
  - §4.6: the leaver's `in_flight` rows.

  Write the lock walk and the "what arrives where, before and after the commit" table INTO the plan, before the tasks.
- **Dawid works in this checkout at the same time,** and other sessions do too (quest work, the client):
  - Commit only your own paths (`git diff --cached --name-only`, commit with a pathspec — memory `commit-only-own-paths`).
  - Filtered staging around his hunks: `-U3`, `--recount`, never `--unidiff-zero`, `git add -A` or `git stash`.
  - Never run `rustfmt` or `cargo fmt` from the root.
  - When the tree does not build, check `git status` for foreign changes before blaming the task.
- Subagents edit with Edit / Write. A refused command is reported, never worked around.
- `just pg-up` must be up for `test-pg` and `test-r2|r3|r4`. Run Postgres suites one at a time.

## 8. What E3 leaves

- **Moving occupied land** (E5). E3 never moves a running region: an assignment is only of free land, and a release only of idle land.
- **Creating and removing hosts, scale to zero** (E4). E4 consumes E3's load reports and its empty hosts.
- **Nested successions:** the residual that a host made landed during an open run dies and rolls back. E3 makes landing more frequent, and §4.1's attached-age guard softens it. Count it in the plan's known limits.
- **Re-keying `in_flight` from host to region** (E5).
- Rebalancing between live hosts (R6 / E5's move in place).

## 9. Open, for Dawid (the plan proceeds on the defaults)

- **The assignment rule (§4.1):** fewest players → fewest regions → rendezvous, skipping low headroom and freshly attached hosts. Is "players" the right first key, or regions?
- **`LAND_IDLE_MS` = 5 minutes:** how long empty land stays with its host before it is released. Shorter frees capacity sooner; longer avoids churn for a player who steps away and returns.
- **A clean leave releases all its land (§4.6)** instead of handing it to one heir. Drained hosts then leave nothing behind, and the land is re-assigned by load.
- **Assignment stops while every coordinator replica is down (§4.1).** Hosts keep what they hold; a player walking into free land waits at the seam until a replica returns.
