# Elastic 3 — The Coordinator Assigns Land; Idle Land Is Released Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A free super-region gets its owner from the coordinator (the least-loaded live host that takes logins), a host gives idle super-regions back inside a committed world cut, and every host reports its load in its heartbeat row.

**Architecture:**
- A host that needs land it does not know an owner for sends `HostMsg::LandNeeded`. The ticker replica reads the owners and, for a free row, issues the store decision `AssignLand` inside its transition's transaction (Elastic 2b's machinery). The assignee learns by `CoordMsg::Assigned`, grows its fence, then announces `Reliable::Owns` as a claim does today.
- A host lists its idle super-regions at `BeginCut`, masks them, and **its own prepared cut part frees the rows** (`owner = -1`, `epoch + 1`). The release commits and aborts with the part. After the commit the fence shrinks, the parked blobs of that land are dropped and peers are told (`Reliable::Released`).
- The beat statement also writes three load columns. The coordinator's `hosts` read returns them, and the choice of an assignee uses them.
- The host-side claim path (`claim_free`, `Volatile::ClaimWanted`, the rendezvous preference) is deleted. A clean leave releases all its land instead of leaving it for a `Leave` succession, except when the leaver still has in-flight rows.

**Tech Stack:** Rust (sqlx on PostgreSQL and SQLite, crossbeam, quinn via `svc-link`), OpenTofu (`deploy/modules/world`), bash (`scripts/twin_check.sh`), the justfile.

**Spec:**
- `docs/superpowers/plans/2026-11-17-elastic-3-land-assignment-planning-handoff.md` (the decisions; §4 is re-ruled below where the code disagreed).
- `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md`: decisions 4, 5 and 9; §5 step 1; §6.
- `docs/superpowers/plans/2026-11-17-elastic-2b-coordinator-replicas.md`: "Decisions settled in planning" items 1–5, "As built".
- `docs/superpowers/plans/2026-11-16-elastic-2a-coordinator-state-in-the-store.md`: "Decisions settled in planning", "As built".

## Before Task 1: the `go-nogo-r3` baseline (Dawid)

E3 changes who owns land under a moving player, so the seam probe must be compared before and after. `TODO.md` (Elastic 2a block, hand check 1) shows the probe has **never had a valid run** since Elastic 1.

- [ ] Ask Dawid to run `just go-nogo-r3 1 60`, then `just go-nogo-r3 0|1|2|5` pooled, and to record the tables in `TODO.md`. Tasks 1–3 change no ownership and may proceed meanwhile. **Task 4 does not start until the baseline is recorded, or Dawid says to go on without it** (then say so in "As built").

## Where this plan departs from the handoff (verified against the code at `5b5e83e9`, 2026-11-18)

1. **There is no peer protocol version.** Peer links, the coordinator link and the login link share `svc_link::LINK_PROTOCOL_VERSION` (9). It goes 9 → 10 once, in Task 3. Task 7 appends `Reliable::Released` under 10.
2. **Wire variants cannot be removed** (append-only). `Volatile::ClaimWanted` and `HostMsg::LandWanted` stay in their enums, are no longer sent, and are handled as before (`ClaimWanted` ignored; `LandWanted` treated as `LandNeeded`).
3. **`CutPrepared` does not carry the released list (§4.3).** The coordinator keeps no ownership view (no field holds super-region owners; `HostSeen.landed` comes from the `hosts` read). It needs no notice of a release.
4. **`AssignLand` checks the assignee FIRST, not last (§4.1 "under the lock order of the claim it replaces").** `claim_free` locked `super_regions` then `hosts`. A coordinator decision that did the same could deadlock with a total-crash registration (`hosts` then `super_regions`). Hosts-first matches the declaration's and the registration's order. See the lock walk.
5. **`AssignLand` needs no mark (§6 "classify every new decision's answer with its `CallStamp`").** The store's `WHERE owner = -1` makes a second decision for the same row a refusal, the answer is idempotent to apply, and a lost answer is healed by the next owners read. The answer still carries its stamp, for uniformity.
6. **The part's release is a lock upgrade unless the row is locked for update up front (§4.3 "check this against the part's fence check").** Today a part only `FOR SHARE`-locks its `super_regions` rows and writes none. A waiting declaration would sit between the share lock and the upgrade. The released rows are therefore locked `FOR NO KEY UPDATE` before the fence check. See the lock walk.
7. **A lost `Assigned` must not turn into an unfenced load.** `Directory::get` answers `Me` for any row whose owner is this host. A directory read showing a row owned by this host that is not in the fence would make the host load regions the fence refuses (exit 3). The keeper adds such rows to the fence before it hands the read to the manager, guarded by the epoch (Task 4).
8. **Parked blobs are never dropped today** (`WorldManager::parked`, removed only when the region restarts). After a release they must go, or a later assignment of the same land to the same host would restart from a stale blob another host has since overwritten in the store (Task 6).
9. **No tick time reaches the manager.** `TickMetrics` lives on each region thread and is log-only. Task 1 adds one process-wide atomic the region threads raise and the beat samples.
10. **The real owner-lookup handler is `on_land_owner`** (`land_wanted` is test support), and `Input::LandOwners` carries owners only. It is reshaped to carry who asked and the rows read (Task 3). `Input` is process-local, so this is no format change.
11. **The rig task is folded into the tasks that need it (§5.3).** The rig executes real `StoreDecision` values and real `WorldCut` parts, which do not exist before Tasks 2 and 5.
12. **§4.6: the rule is exact.** A leave part releases all the leaver's land when the part carries no in-flight row and no succession is open on that host. Otherwise it releases nothing and today's `Leave` succession applies. The `Leave` path is therefore not dead and stays.
13. **A host that released all its land and then dies with unsaved state traffic rolls the world back** (the existing `unaccounted_dead` rule: it is landless, so there is no succession to carry the reports). The window is one stale peer message between its release and the next committed cut. A rollback, never a loss. Counted under "Known limits".

## Decisions settled in planning

### 1. Assignment

**The ask.** `HostMsg::LandNeeded { sr }` replaces both the peer ask and `LandWanted`. A host sends it for every super-region it has a need in and knows no reachable owner for (`Free`, `Unknown`, or owned by a host that left), at most once per `LAND_NEEDED_EVERY_MS` per super-region, only while its coordinator link is attached. A draining host asks too: it is assigned nothing, but what waits on it needs an owner somewhere.

**The read.** The core queues `land_lookups: BTreeMap<SuperRegion, BTreeSet<HostId>>` (durable, as today's set). The ticker takes them, reads `owners_later()`, and applies `Input::LandOwners { asked, rows }` (ticker-only, never logged, never replayed: a rebased replica re-derives it from the hosts' next ask).

**Per asked super-region, with the row read:**

| The row | The core does |
|---|---|
| absent, or `owner = NO_OWNER` | `pick_assignee`; with a host → `StoreDecision::AssignLand { sr, host, boot }`; with none → nothing (logged, throttled) |
| owned by an attached live host `h`, an asker is `h` itself | `CoordMsg::Assigned { rows: [(sr, epoch)] }` to `h` (the heal of a lost `Assigned`) |
| owned by an attached live host `h`, other askers | `CoordMsg::LandIs { sr, owner: h, epoch }` to each |
| owned by anyone else (left, lapsed, unknown) | `on_land_owner(owner, now)`, unchanged: a `Leave` succession for a departed owner, nothing for a host a declaration will take |

**The answer.** `DecisionAnswer::Assigned { stamp, assign, outcome }`, a logged input. `Done { epoch }` → `Assigned` to the assignee and `LandIs` to the other askers of that pass, and one provisional-load entry. Any other outcome changes nothing: the hosts ask again within a second. Applying it twice (a replay) sends the messages twice, which the host tolerates (below).

**Who is chosen (`pick_assignee`).** Candidates are the hosts in `attached_live()` that `takes_logins` and have a lease row. Two filters apply in order, each only if it leaves at least one candidate:
1. drop hosts this ticker's `LeaseWatch` has known for less than `ASSIGN_MIN_ATTACHED_MS` (the crash-loop guard);
2. drop hosts whose `load.tick_permille > 1000 - ASSIGN_HEADROOM_MIN_PERMILLE`.

Among the rest: the least `players + provisional`, then the least `regions + provisional`, then `rendezvous_among(sr, tied)`. `provisional` is the number of assignments this ticker made to that host in the last `ASSIGN_PROVISIONAL_MS`. It counts toward both keys because a beat sent just after an assignment does not show the player or the region yet. It is a process field (`kept`): a new ticker starts from zero, which only spreads a burst less evenly once.

**The assignee.** `Assigned` reaches the manager, which drops a row the directory already knows at that epoch or newer and queues the rest. The server's keeper takes them, calls `HostClaims::add` (**fence first**), then `manager.claimed(rows)`: directory set, `Reliable::Owns` to every peer, `flush_unowned`. This is the order a claim has today.

**The heals.** At most once after commit (E2a decision 3), so `Assigned` can be lost:
- The assignee's own need → it asks → the read shows it as owner → `Assigned` again.
- Another host's need → `LandIs` → it sends to the assignee → the assignee, which thinks the land free, holds the message and asks → as above.
- The assignee's directory read (30 s backstop, or wanted) shows a row it owns outside its fence → the keeper treats it as an `Assigned`.

**Static and all-in-one** assign nothing: the paths above run only where successions do (`roles::coordinator_crash_policy`).

**With every replica down nothing is assigned.** Hosts keep what they hold; a need for free land waits (`Unknown`-style, never guessed). Today a host could claim alone. Stated in the docs.

### 2. Release

**Idle.** The manager scans its owned super-regions every `LAND_IDLE_SCAN_MS`. A super-region is **busy** when any of these holds:

| Holder | Busy when |
|---|---|
| `regions` | a region of it is running or stopping |
| `pending_arrivals`, `intent_mailbox` | a key lies in it |
| `in_flight`, `in_flight_intents` | a target (or an intent's source) lies in it |
| `sessions` / `awaiting_session` | a connected or awaited client's `homes` entry lies in it |
| `moving` | a move's `from` or target lies in it |
| held spawns (`placement.rs`) | a held order's region lies in it |
| `succession_pinned`, `inheriting` | a pinned region lies in it, or it is masked |

`uncommitted` is not in the list: with no running region, every uncommitted snapshot of that land is carried by the next part (`begin_cut_at` carries every non-running one), which is the part that releases. `parked_players` is not in the list: a parked player is a record keyed by player, and its holder moves it to the land's next owner when the player returns (`move_to_peer`), as it does today for a record whose region another host owns.

A busy super-region has no idle stamp. A non-busy one gets `land_idle_since[sr] = now` once.

**At `BeginCut`** (a participant under `claim`): `releasing` = the owned super-regions idle for at least `land_idle_ms` and not busy now. It is empty while any succession run is open on this host (heir, survivor or adopting). From then on `owner_of_sr` answers `Unknown` for them, exactly as the `inheriting` mask does: nothing loads there, `note_unowned` records the need without asking, and every arrival takes the existing unowned path.

**The part** carries `WorldCut::releases`. The store frees those rows inside the prepared transaction.

**Committed** (`cut_committed`, after the store's answer): the server shrinks the fence (`HostClaims::shrink`) and then the manager, per released super-region, sets the directory row free at `epoch + 1`, drops the `parked` blobs and the idle stamp, remembers the row in `recently_released`, lifts the mask, sends `Reliable::Released` to every peer and runs `flush_unowned`. Needs recorded while masked now ask.

**Aborted**: the mask lifts, the land is this host's, `flush_unowned` delivers what waited.

**A total crash's land** (the first host back holds everything) drains back by this path. No special case.

**A clean leave** (the part of a host in `leaving`): `releases` is every super-region the leaver owns, when the part's in-flight list is empty and no succession is open on it. Peers already re-read the directory at `end_leaves`; the read shows the rows free. A leaver with in-flight rows releases nothing: its rows are keyed to its host id and only a succession's adopting part moves them (re-keying them is Elastic 5).

### 3. The lock walks (Tasks 2 and 5 write them into `world-store/src/succession.rs`'s notes)

**`AssignLand`, a savepoint of the coordinator's transition.** The `coordinator` row is already held (the compare-and-set). Then:

1. `SELECT drain_at_ms FROM hosts WHERE id = $h AND boot = $b AND state = LIVE AND clean = 0 FOR SHARE`. No row → `HostNotLive`. A drain mark → `HostDraining`.
2. `INSERT INTO super_regions (sx, sz, owner, epoch) VALUES ($1, $2, $h, 1) ON CONFLICT (sx, sz) DO NOTHING RETURNING epoch`.
3. If no row: `UPDATE super_regions SET owner = $h, epoch = epoch + 1 WHERE sx = $1 AND sz = $2 AND owner = -1 RETURNING epoch`. No row → `Owned`.

Why no cycle:
- Every coordinator decision runs under the `coordinator` row, so `AssignLand` never runs beside a declaration, a landless end or another assignment.
- Its walk is `coordinator` → `hosts` (one row, share) → `super_regions` (one row). The declaration's is `coordinator` → `world` → `hosts` → `super_regions`; a registration's is `world` → `hosts` → `successions` → `hosts` → `super_regions`. `hosts` before `super_regions` in all three.
- A host's re-claim of its own rows (`claim_super_regions_live`: `super_regions` then its `hosts` row) locks only rows it owns. `AssignLand` writes only a free or absent row. They share no `super_regions` row.
- Statements 2 and 3 never wait for a staged part. A row a part holds is owned in every committed snapshot: `ON CONFLICT DO NOTHING` sees it, and the `UPDATE`'s `WHERE owner = -1` does not match it, so no row lock is requested. A part that RELEASES the row is the same case until its `COMMIT PREPARED`.
- The share lock on the assignee's `hosts` row is held until the transition commits. The assignee's beat (`UPDATE hosts SET seen_ms …`) waits behind it, under its own 1 s `lock_timeout`, as it does behind a declaration today. A total-crash registration that would mark the row dead waits too, then reads `super_regions` in a later statement and sees the assignment.

Cost: at most `ASSIGN_PER_PASS_MAX` (64) assignments per transition; each is three single-row statements.

**The part's release** (`cut_rows`, before `PREPARE TRANSACTION`). Statement order of a part, with the two new steps marked:

1. `SET LOCAL lock_timeout`; the plain generation read.
2. **NEW, only when `cut.releases` is non-empty:** the fence check's statement over `cut.releases` with `FOR NO KEY UPDATE` in place of `FOR SHARE`, key order, compared in Rust as the fence check compares (owner and epoch must be the fence's). A release not in `fence.epochs` is refused before any statement (`Unsupported`).
3. `check_fence` over `commit_check_set` (`FOR SHARE`; a no-op on the rows step 2 holds).
4. The region, chunk, entity, player and in-flight writes, unchanged.
5. **NEW:** per release, `UPDATE super_regions SET owner = -1, epoch = epoch + 1 WHERE sx = $1 AND sz = $2 AND owner = $me AND epoch = $e`. `rows_affected != 1` → `Fenced`.
6. `PREPARE TRANSACTION`.

Why this is safe:
- "A cut part writes only rows of its own regions" holds: the released rows are the part's own until its commit, and no other host's part can check or write them (a part's check set is its own land).
- The only other lockers of this host's `super_regions` rows are its own region loads (`FOR SHARE` on one row: none run in masked land, and one still open from before the mask delays step 2 by its length), a declaration of this very host, and a total-crash registration (nobody live).
- **One new cycle, safe.** Steps 2 and 3 are two key-ordered passes, so a declaration of this host walking the same rows in one pass can hold a row of step 3's set while this part holds one of step 2's. Postgres aborts one side (`40P01`). The part's side maps to `Fenced` (`fence_race`), which is true: the host is being declared dead. The declaration's side is its savepoint's refusal and is retried.
- A staged releasing part holds the released rows `FOR NO KEY UPDATE` until `COMMIT PREPARED` / `ROLLBACK PREPARED`. A declaration of the releaser waits for it up to its `lock_timeout`, as it waits for a share lock today, and then moves only the rows the releaser still owns. `decide_in` touches no `super_regions` row.
- The one-phase cut (`CutEnd::Commit`: static, all-in-one, SQLite) refuses a non-empty `releases` (`Unsupported`).

**The load columns in the beat.** Same statement, same one row, nothing wanted after it. No new walk.

### 4. What arrives for a released super-region, and where it goes

`S` is a super-region host A releases in cut K. "Masked" is from A's `BeginCut` of K until A applies the store's answer. "Free" is after that. B is a peer.

| What | While masked on A | Free, B not yet told | Free, B told (`Released` or a read) |
|---|---|---|---|
| A non-player transfer from B, or a local crossing on A | A: `wait_for_owner` → `unowned_waiting[S]`, a row of A's part (K if the sender accounted it in K, else K+1). No ask yet. | The same, and A asks (`LandNeeded`). | B: `wait_for_owner` on B, a row of B's part; B asks. |
| A player walking in on A | Clamped back into the source region (`can_move` / the waking-crossing refusal); the need is recorded. | Clamped; A asks; the player crosses when the land has an owner (a hand-off if it is not A). | n/a |
| A player hand-off or `PlayerMove` from B | A: `park_handed` (the record parks on A, B is told `PlayerParked`); the client rejoins by itself; the gate's placement waits for the land. | The same. | B: `can_move` refuses and asks; no hand-off is sent. |
| An intent from B | Refused back (`OwnerNotRunning`), as for any land with no running region. | The same. | B: refused locally. |
| An intent reply | Held in A's `intent_mailbox`, a row of A's part. | The same. | The same on B if B holds it. |
| Halo | Dropped (volatile). | Dropped. | Not sent. |
| A subscribe, a spawn order | `subscribe_elsewhere` remembers it; `Placement::Wait` holds the order. | The same, and A asks. | The same on B. |

Then, exactly once:
- **K commits:** A's waits for `S` ask. `Assigned` to A → `flush_unowned` accepts them here. `LandIs` naming X → `flush_unowned` sends them to X.
- **K aborts:** the mask lifts, the owner is A again, `flush_unowned` accepts them here.
- **New in Task 7:** `flush_unowned` also forwards `intent_mailbox` mail for a region whose owner is now another host (`intent_to_peer`). Today nothing does, because land a region ran on never changed hands without a succession.
- **A dies after K committed, before B heard.** B's directory says `Host(A)`. If A still owned other land, its succession names the rows that moved; the released ones are not among them. If A was landless it is ended. In both cases B's `forget_host(A)` (and `end_leaves` for a leaver) finds directory rows still naming A and sets `directory_wanted`; the read marks them free. A message B queued to A goes through `to_forgotten` → `wait_for_owner`. Unsaved state traffic with a landless A is the rollback of departs 13.

### 5. Tunables (named consts)

| Constant | Value | Where | Why |
|---|---|---|---|
| `LAND_IDLE_MS` | 300 000 | `game::world_manager` | A player who steps away for a few minutes comes back to the same host. Dial: `VOXEL_LAND_IDLE_MS` (server env, unset = the const) for the twin and tests. |
| `LAND_IDLE_SCAN_MS` | 1 000 | `game::world_manager` | The idle stamps need no finer grain than the ask period. |
| `LAND_NEEDED_EVERY_MS` | 1 000 | `game::world_manager` | Takes the place of `CLAIM_WANTED_EVERY_MS` and `LAND_WANTED_EVERY_MS` (both 1 000 today). |
| `ASSIGN_HEADROOM_MIN_PERMILLE` | 200 | `game::coordinator` | A host whose worst tick uses over 80 % of the period gets no new land while another can take it. |
| `ASSIGN_MIN_ATTACHED_MS` | 30 000 | `game::coordinator` | Three leases: a pod that crash-loops is usually gone by then. |
| `ASSIGN_PROVISIONAL_MS` | 10 000 | `game::coordinator` | One lease: by then a beat shows the real load. |
| `ASSIGN_PER_PASS_MAX` | 64 | `game::coordinator` | Bounds how long one transition holds the row. |

## To confirm with Dawid (the plan proceeds on these defaults)

- **The assignment rule:** fewest players, then fewest running regions, then a rendezvous; low headroom and freshly seen hosts are skipped while another qualifies.
- **`LAND_IDLE_MS` = 5 minutes.**
- **A clean leave releases all its land** (unless the leaver has in-flight rows): a drained host leaves nothing behind and the land is assigned by load at its next need.
- **Assignment stops while every coordinator replica is down.** A player walking into free land waits at the seam until a replica is back.
- **A hand-off into land that is being released parks the player and makes the client rejoin** (the existing path for a hand-off into land the receiver cannot run). It happens only inside one cut's prepare-to-commit window, for land that was idle for five minutes.

## Global Constraints

- No git worktrees; work on `develop` in this checkout. Dawid and other sessions commit and stage concurrently in the SHARED index.
  - Commit only your own paths: check `git diff --cached --name-only` before every commit and commit with a pathspec.
  - A file holding foreign hunks (`justfile`, `CLAUDE.md`, `TODO.md`, `deploy/modules/world/variables.tf`, the env roots' `main.tf`) is staged with a filtered patch: `git diff -U3`, keep your hunks, `git apply --cached --recount`. Never `git add -A`, `git stash`, `--unidiff-zero`.
  - Never run `rustfmt` or `cargo fmt` from the root. When the tree does not build, check `git status` for foreign changes before blaming the task.
- **Conservation beats latency; wait, never guess.** Every entity, item, player and person exists exactly once through any crash, conflict or release point. A task that changes who holds what is not done without a conservation test (`assert_conserved_world` in the seeded layer).
- **Fencing in PostgreSQL is the only safety.** An assignment and a release are epoch bumps in one transaction. Never a store write that skips the fence. A cut part writes only rows of its own land.
- **Fence before use; shrink before the next part.** `HostClaims::add` runs before the manager learns the land; `HostClaims::shrink` runs before `cut_committed`.
- **Write-before-send** for every coordinator output, `Assigned` and `LandIs` included.
- **Migrations** are immutable and identical in shape in `migrations/pg` and `migrations/sqlite`. This plan adds exactly `0018_host_load`. `tests/schema.rs` `EXPECTED` follows. Worlds are re-forged, never migrated: say "re-forge / `just pg-reset` / `just run --reset`".
- **Lock walks:** every new or changed statement on `super_regions`, `hosts` or `coordinator` gets its walk in `world-store/src/succession.rs`'s notes. A prepared transaction is not a waiting backend.
- **Wire enums are append-only.** `LINK_PROTOCOL_VERSION` 9 → 10 once (Task 3). `COORDINATOR_STATE_VERSION` 5 → 6 once (Task 3).
- **The core stays deterministic:** `game` reads no clock, no environment and no store; `BTreeMap` / `BTreeSet`; named consts. Every new core field is classified in `projection!`, every new `WorldManager` field in `host_fields!`, every new input as replayed or ticker-only.
- A player's land is a **holding** in code. This plan adds no new "claim" name; it removes some.
- Never write a region edge or a super-region side as a literal.
- `game`, `server` and `worldgen` stay free of Bevy. rustls uses ring only. Tokio stays at the edges.
- **Ask Dawid first:** client tests, live runs (`dev-r3`, `go-nogo-r3`, `just run`), measurements, anything on orion. `just twin-check` is allowed.
- **Tests:**
  - `just test-one <crate> "<args>"`, filters after `--`. Server targets `roles`, `coordinator_rt`, `hosts`, `takeover` need `--features test-hooks` (`just test-r2|r3|r4` pass it).
  - Postgres suites need `just pg-up` and run one at a time; world-store at `--test-threads=4`.
  - Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
  - **Only regression tests are committed (CLAUDE.md, Testing; the whole repository since 2026-11-19).** This supersedes every "write the failing tests" step, every re-pin table and every named test of Tasks 7–10, and the tests Tasks 1–6 committed were deleted by the repository-wide clean-up (`2026-11-19-regression-only-tests.md`). From Task 7 on: a task commits NO test unless it reproduces a bug actually seen in play or in a log (then with a `/// Regression:` marker). Temporary tests written to drive a change — the seeded rig is still there for that — are fine and are deleted before the commit. A task's verification is `just smoke` (it runs `test-audit`), `just web-check` when `game` changed, `just determinism-wasm` for sim math or anything hashed. The scenarios the plan named as tests (Review Focus, the conservation table) are what the implementer reasons through and the reviewer checks by reading; they are listed for Dawid's hand checks in `TODO.md` (Task 10). `just test` / `just test-full` run once at the close, said first.
  - Do not run server test binaries while Dawid has `just run` up (the shared `target/debug/server` is re-linked at region 64): ask if unsure.
- **Known reds, not ours:** three `lod::tests`, `codec::golden_blobs_decode`, flaky SQLite `stud_refusal::…`, flaky svc-link `dialer_reconnects_after_listener_restart`.
- **The ledger** is `.superpowers/sdd/2026-11-18-elastic-3-land-assignment/progress.md`. A decision this plan did not make goes there as `Ruling: what — why — cost if wrong`.
- Subagents edit with Edit / Write. A refused command is reported, never worked around. The justfile is canonical, secrets are never printed, dated documents are not edited.

## Review Focus

1. **A player walks toward land while the cut that releases it is between prepare and commit.**
   - Expected: the body stays in its source region, the player is never in two places or none, and it crosses once the land has an owner, on whichever host that is.
   - Pinned by Task 6: `peer_schedule::a_player_walking_into_land_being_released_waits_at_the_seam_and_crosses_once`.
2. **A host is assigned land and dies, or its `Assigned` is lost, before it learns of it.**
   - Expected: the store's row decides. A dead assignee is succeeded as a landed host (never ended as landless). A live one learns by its next ask or read and never loads a region its fence lacks.
   - Pinned by Task 4: `peer_schedule::an_assignee_that_dies_before_learning_its_land_is_succeeded_not_ended`, `claims::tests::a_lost_assigned_is_healed_by_the_directory_read`.
3. **A directory read issued before a release is applied after it.**
   - Expected: the row is not put back into the fence at its old epoch (that would fence the host's next cut).
   - Pinned by Task 6: `claims::tests::a_directory_read_older_than_a_release_adds_nothing_to_the_fence`.
4. **Released land comes back to the host that released it.**
   - Expected: its regions start from the store, not from a blob parked before the release.
   - Pinned by Task 6: `peer_schedule::land_released_and_assigned_back_to_its_releaser_starts_from_the_store`.
5. **A host dies after its release committed and before its peers heard.**
   - Expected: nothing addressed to that land is lost or delivered twice; peers find the land free by their own read; the world is never stuck.
   - Pinned by Task 7: `peer_schedule::a_host_dying_after_its_release_committed_before_peers_heard_loses_nothing`.

## File structure

| Path | Responsibility after this plan |
|---|---|
| `crates/world-store/migrations/{pg,sqlite}/0018_host_load.sql` (new) | `hosts.load_players`, `load_regions`, `load_tick_permille` |
| `crates/world-store/src/assign.rs` (new) | `assign_land_in`: the `AssignLand` decision's statements |
| `crates/world-store/src/{rows,coordinator_tx,handle,thread,succession}.rs` | the beat with load; `AssignLand` in `run_one`; the part's release in `cut_rows`; `claim_free` and `release` deleted; the lock notes |
| `crates/game/src/ownership.rs` | `HostLoad`; `Directory::preferred` deleted |
| `crates/game/src/coordinator/assign.rs` (new) | `on_land_read`, `pick_assignee`, the provisional load, `assign_answered` |
| `crates/game/src/coordinator/{mod,msg,decisions,input,succession,state}.rs` | `LandNeeded`, `Assigned`, `LandIs`, `AssignLand`, the reshaped `LandOwners`, `land_lookups` by asker |
| `crates/game/src/world_manager/land.rs` (new) | the idle scan, `land_busy`, `releasing`, `land_released`, `assigned`, the one ask (`ask_for_land`) |
| `crates/game/src/world_manager/{mod,peers,world_cut,world_link,succession}.rs` | the claim fields and the peer ask removed; the mask in `owner_of_sr`; `releases` in the part; `Released` in and out |
| `crates/game/src/save_cut.rs`, `crates/game/src/peer/msg.rs` | `WorldCut::releases`; `Reliable::Released` |
| `crates/game/tests/common/multi_rig.rs`, `peer_schedule.rs`, `coordinator.rs`, `peer_journal.rs`, `ownership.rs` | the rig executes `AssignLand` and releasing parts; the seeded and core tests |
| `crates/server/src/load.rs` (new) | `TickLoad` |
| `crates/server/src/{claims,lease,cuts,lib,region_threads,health,perf,coordinator_rt}.rs` | the keeper without a claim path, with the `Assigned` hand-over and the heal; `HostClaims::shrink`; the load in the beat; metrics and log fields |
| `crates/server/tests/{hosts,takeover}.rs`, `crates/world-store/tests/{assign,directory,lease,two_phase,schema}.rs` | real-host and store tests |
| `deploy/modules/world/{hosts,variables}.tf`, `deploy/envs/local/world/main.tf`, `scripts/twin_check.sh`, `scripts/dev_r3.sh`, `justfile` | `VOXEL_LAND_IDLE_MS`; the twin's "assigned, idle, released, re-assigned" phase |
| `CLAUDE.md`, `TODO.md`, this plan's "As built" | docs |

---

### Task 1: Hosts report load in their heartbeat row

Nothing reads the load for a decision yet. One player and one host behave as before.

**Files:**
- Create: `crates/world-store/migrations/pg/0018_host_load.sql`, `crates/world-store/migrations/sqlite/0018_host_load.sql`, `crates/server/src/load.rs`.
- Modify: `crates/game/src/ownership.rs` (beside `HOST_HEARTBEAT_MS`, ~:33); `crates/world-store/src/rows.rs` (`heartbeat_sql` :178, `heartbeat` :898, `hosts` :847); `crates/world-store/src/handle.rs` (`HostRow` :48, `heartbeat` :1147, `heartbeat_later` :1152); `crates/world-store/src/thread.rs` (`beats` :801, `Request::Heartbeat` :691); `crates/server/src/lease.rs` (`Heartbeat::pump` :98, `pump_or_fence` :136); `crates/server/src/region_threads.rs` (`TickMetrics` :329); `crates/server/src/lib.rs` (the main loop's `pump_or_fence` call); `crates/server/src/health.rs` (`render` :292); `crates/game/src/coordinator/succession.rs` (`HostSeen` :115); `crates/server/src/coordinator_rt.rs` (the `HostRow` → `HostSeen` mapping :1788).
- Test: `crates/world-store/tests/lease.rs`, `crates/world-store/tests/schema.rs`, `crates/server/src/load.rs` (unit).

**Interfaces:**
- Produces:

```rust
// game/src/ownership.rs
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostLoad { pub players: u32, pub regions: u32, pub tick_permille: u32 }

// world-store
pub struct HostRow { /* … */ pub load: game::HostLoad }
impl StoreHandle {
    pub fn heartbeat(&self, id: HostId, boot: u64, load: game::HostLoad) -> Result<Beat, StoreError>;
    pub fn heartbeat_later(&self, id: HostId, boot: u64, load: game::HostLoad) -> Receiver<Result<Beat, StoreError>>;
}

// game/src/coordinator/succession.rs
pub struct HostSeen { /* … */ pub load: crate::HostLoad }

// server/src/load.rs
#[derive(Clone, Default)]
pub struct TickLoad(Arc<AtomicU64>);
impl TickLoad {
    /// A region thread calls this once per tick with the tick's compute time.
    pub fn note(&self, compute: Duration);
    /// The worst tick since the last call, over `period_ms`, ×1000, capped at 10_000. Resets the window.
    pub fn take_permille(&self, period_ms: u64) -> u32;
}

// server/src/lease.rs: `load` is called only when a beat is sent
pub fn pump_or_fence(heartbeat: &mut Option<Heartbeat>, fenced: &Fenced, now: Instant,
                     load: &mut dyn FnMut() -> game::HostLoad) -> bool;
```

- The migration, both engines: `ALTER TABLE hosts ADD COLUMN load_players BIGINT NOT NULL DEFAULT 0;` and the same for `load_regions` and `load_tick_permille` (SQLite: `INTEGER`).
- The beat: `UPDATE hosts SET seen_ms = {now_ms}, load_players = $3, load_regions = $4, load_tick_permille = $5 WHERE id = $1 AND boot = $2 AND state = {HOST_LIVE} RETURNING drain_at_ms - seen_ms`.
- The main loop's sampler: `players = manager.depths().sessions`, `regions = manager.depths().running`, `tick_permille = tick_load.take_permille(game::TICK_RATE as u64)` (`TICK_RATE` is the period in ms). `beat_at_boot` sends `HostLoad::default()`.
- Gauges on a host's `/metrics`: `mmo_host_load_players`, `mmo_host_load_regions`, `mmo_host_load_tick_permille`, set with what the last beat sent.

- [ ] **Step 1: Write the failing tests.**
  - `lease::a_beat_writes_the_load_and_the_hosts_read_returns_it` (`each_backend`): register, beat with `HostLoad { players: 3, regions: 7, tick_permille: 412 }`, `hosts()` → that row's `load` equals it; a fresh row's `load` is `HostLoad::default()`; a beat for another boot changes nothing.
  - `schema.rs`: `EXPECTED` gains `0018_host_load`.
  - `load::tests::the_worst_tick_of_the_window_is_reported_once`: `note(5 ms)`, `note(40 ms)`, `note(10 ms)`; `take_permille(50) == 800`; a second `take_permille(50) == 0`; `note(900 ms)` → `take_permille(50) == 10_000`.
- [ ] **Step 2:** Run `just test-one world-store "--test it -- lease::a_beat_writes"` and `just test-one server "--lib -- load::"`. Expected: FAIL (does not compile).
- [ ] **Step 3:** Implement. Add to the lock notes in `succession.rs` one line under the heartbeat: the load columns ride the same one-row statement.
- [ ] **Step 4:** PASS. Then `just test-one world-store "--no-fail-fast"`, `just test-pg "--no-fail-fast -- --test-threads=4"`, `just test-one server "--features test-hooks --no-fail-fast"`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(host): the heartbeat row carries the host's load`.

### Task 2: The store assigns a free super-region inside a coordinator transition

**Files:**
- Create: `crates/world-store/src/assign.rs`, `crates/world-store/tests/assign.rs` (a module of `tests/it.rs`).
- Modify: `crates/game/src/coordinator/decisions.rs` (`StoreDecision` :56, `DecisionAnswer` :75); `crates/world-store/src/coordinator_tx.rs` (`Decided` :116, `run_one` :281, `refused` :268, `answers` :350); `crates/world-store/src/handle.rs` (a `#[doc(hidden)]` wrapper beside `end_landless_host` :811); `crates/world-store/src/succession.rs` (the notes); `crates/game/src/coordinator/link.rs` and `crates/game/tests/common/multi_rig.rs` (`execute_committed` :1033 gains the arm; every exhaustive match on `StoreDecision` / `DecisionAnswer` compiles).
- Test: `crates/world-store/tests/assign.rs`.

**Interfaces:**
- Produces:

```rust
// game/src/coordinator/decisions.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignLand { pub sr: crate::SuperRegion, pub host: HostId, pub boot: u64 }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Assigned { Done { epoch: u64 }, Owned, HostNotLive, HostDraining }
// StoreDecision gains:   AssignLand(AssignLand)
// DecisionAnswer gains:  Assigned { stamp: CallStamp, assign: AssignLand, outcome: Result<Assigned, String> }

// world-store/src/assign.rs
pub(crate) async fn assign_land_in(tx: &mut sqlx::PgConnection, a: &AssignLand) -> Result<Assigned, StoreError>;

// world-store/src/handle.rs, test support through `decide_alone`
#[doc(hidden)] pub fn assign_land(&self, a: AssignLand) -> Result<Assigned, StoreError>;
```

- `run_one` keeps the savepoint only on `Ok(Assigned::Done { .. })`. A `40P01` or lock timeout inside is the decision's `Err`, as for the others.
- The statements and their order are "Decisions" item 3. No side check: a coordinator checks `VOXEL_SUPER_REGION_REGIONS` against the world row at its start.
- In the rig, `execute_committed` answers `AssignLand` from `claim_store`: `Done` for an absent or free row when the host is live, not left, not draining (epoch 1 or `epoch + 1`); otherwise the matching refusal. The core does not issue the decision before Task 3, so no rig test changes here.

- [ ] **Step 1: Write the failing tests** (`assign.rs`, Postgres only, each through `assign_land` unless it says a transition):
  - `a_free_or_absent_row_is_assigned_at_the_next_epoch`: an absent row → `Done { epoch: 1 }`, owner is the host; a row released at epoch 3 → `Done { epoch: 4 }`.
  - `an_owned_row_is_refused_and_untouched`: `Owned`; owner and epoch unchanged.
  - `a_dead_cleanly_left_or_draining_assignee_gets_nothing`: a dead row and a clean row → `HostNotLive`; a drain mark → `HostDraining`; another boot of the same id → `HostNotLive`; the row stays free in all four.
  - `two_assignments_of_one_row_one_wins`: two transitions, one row, two hosts → `Done` then `Owned`.
  - `an_assignment_never_waits_for_a_staged_part`: host A's part staged (its rows held `FOR SHARE`), the store's decision lock timeout set to 1 s; `AssignLand` of a row A owns → `Owned` in under 500 ms.
  - `an_assignment_rolls_back_with_its_conflicting_transition`: a `TransitionWrite` at a stale version carrying `AssignLand` → `Conflict`; the row is still free.
  - `assigned_then_declared_in_one_transition_moves_with_the_rest`: one transition with `AssignLand(h)` then `Declare` of a lapsed `h` → the row's owner is the heir, its epoch is 2 (assigned at 1, moved at 2), and `successions.moved` lists it.
  - SQLite: `coordinator_rows::a_decision_on_sqlite_is_unsupported` gains `AssignLand`.
- [ ] **Step 2:** Run `just test-pg "--test it -- assign::"`. Expected: FAIL (does not compile).
- [ ] **Step 3:** Implement. Write the `AssignLand` walk of "Decisions" item 3 into `succession.rs`'s notes, after "ending a landless host".
- [ ] **Step 4:** PASS. Then `just test-pg "--no-fail-fast -- --test-threads=4"`, `just test-one world-store "--no-fail-fast"`, `just test-one game "--no-fail-fast"`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(store): AssignLand gives a free super-region to a live host inside a coordinator transition`.

### Task 3: The coordinator answers a need for land

The core and the wire. Hosts do not send `LandNeeded` yet (Task 4), so a running world behaves as before; the tests inject it.

**Files:**
- Create: `crates/game/src/coordinator/assign.rs`.
- Modify: `crates/game/src/coordinator/msg.rs` (`HostMsg` :106, `CoordMsg` :251, appended at the end of each); `crates/svc-link/src/lib.rs` (`LINK_PROTOCOL_VERSION` :58 → 10, its history comment); `crates/game/src/coordinator/state.rs` (`COORDINATOR_STATE_VERSION` :36 → 6); `crates/game/src/coordinator/mod.rs` (`on_host` ~:794, `on_decision` :1374, `projection!` :354, `new_for` :432); `crates/game/src/coordinator/input.rs` (`Input::LandOwners` :47–93, `apply_logged` :183–209); `crates/game/src/coordinator/succession.rs` (`SuccessionRuns.land_lookups` :377, `land_asked` :1396, `take_land_lookups` :1408, `LeaseWatch` :161, `Watched` :135); `crates/server/src/coordinator_rt.rs` (`pump_successions` :2022–2051, `describe` :387, `answered` :2112); `crates/game/src/world_manager/world_link.rs` (`apply_world_msg` :591: the two new arms log and drop until Task 4); `crates/game/tests/common/multi_rig.rs` (`drive_land` :865).
- Test: `crates/game/tests/coordinator.rs`.

**Interfaces:**
- Consumes: `AssignLand`, `Assigned`, `DecisionAnswer::Assigned` (Task 2); `HostSeen.load` (Task 1).
- Produces:

```rust
// msg.rs (appended)
HostMsg::LandNeeded { sr: SuperRegion }
CoordMsg::Assigned { rows: Vec<(SuperRegion, u64)> }
CoordMsg::LandIs { sr: SuperRegion, owner: HostId, epoch: u64 }

// input.rs
Input::LandOwners { asked: Vec<(SuperRegion, Vec<HostId>)>, rows: Vec<(SuperRegion, i64, u64)> }

// succession.rs
pub(super) land_lookups: BTreeMap<SuperRegion, BTreeSet<HostId>>,       // durable
pub fn take_land_lookups(&mut self) -> Vec<(SuperRegion, Vec<HostId>)>; // ticker only
impl LeaseWatch { pub fn known_for(&self, host: HostId, now_ms: u64) -> u64; } // ms since this watch first saw the row's boot

// assign.rs
pub const ASSIGN_HEADROOM_MIN_PERMILLE: u32 = 200;
pub const ASSIGN_MIN_ATTACHED_MS: u64 = 30_000;
pub const ASSIGN_PROVISIONAL_MS: u64 = 10_000;
pub const ASSIGN_PER_PASS_MAX: usize = 64;
impl Coordinator {
    pub(super) fn on_land_read(&mut self, asked: Vec<(SuperRegion, Vec<HostId>)>, rows: &[(SuperRegion, i64, u64)], now_ms: u64);
    pub(super) fn pick_assignee(&self, sr: SuperRegion, now_ms: u64) -> Option<(HostId, u64)>; // (host, boot)
    pub(super) fn assign_answered(&mut self, assign: AssignLand, outcome: Result<Assigned, String>, now_ms: u64);
    /// Test support: the assignments decided and not yet expired, per host.
    #[doc(hidden)] pub fn provisional_load(&self, host: HostId, now_ms: u64) -> u32;
}
```

- New process fields in `projection!`, both `kept`: `provisional: Vec<(HostId, u64)>` (host, the store-ms of the decision) and `assign_askers: BTreeMap<SuperRegion, Vec<HostId>>` (who to tell `LandIs` when the answer comes; dropped with the answer).
- `HostMsg::LandNeeded` and `HostMsg::LandWanted` both call `land_asked(host, sr)`, which inserts the asker. `LandNeeded` is **replayed** (it only queues, as `LandWanted` in E2b's table). `LandOwners` stays ticker-only, never logged. `Decision(Assigned)` is **replayed**; its effect is two messages and one provisional entry.
- `on_land_read` follows the table of "Decisions" item 1. More free rows than `ASSIGN_PER_PASS_MAX` in one read: the rest go back into `land_lookups`.
- The runtime keeps the asked list beside `land_rx` and applies `Input::LandOwners { asked, rows }` with the rows of the asked super-regions only (an asked super-region with no row is absent: free).
- The rig's `drive_land` feeds `Input::LandOwners` from `claim_store` in the new shape and executes the committed decisions.

- [ ] **Step 1: Write the failing tests** (`coordinator.rs`; a helper `read_hosts(&mut c, &[(host, HostLoad, draining)])` applies a `HostsRead`, and `read_land(&mut c, asked, rows)` applies `LandOwners`):
  - `free_land_is_assigned_to_the_least_loaded_host_that_takes_logins`: hosts 1 `{players: 5}`, 2 `{players: 1, regions: 9}`, 3 `{players: 1, regions: 2}` → the transition carries `AssignLand { host: HostId(3), .. }`; after `Done { epoch: 1 }` the outputs are `Assigned { rows: [(sr, 1)] }` to 3 and `LandIs { owner: 3, epoch: 1 }` to the asker.
  - `a_burst_spreads_by_provisional_load`: two idle hosts, eight free super-regions in one read → four decisions each.
  - `provisional_load_expires`: after `ASSIGN_PROVISIONAL_MS` `provisional_load` is 0.
  - `a_low_headroom_host_is_skipped_while_another_qualifies`: `tick_permille: 801` vs `800`; and chosen when it is the only candidate.
  - `a_freshly_seen_host_is_skipped_while_an_older_one_qualifies`: one known 29 999 ms, one 30 000 ms; and chosen when alone.
  - `a_draining_or_leaving_host_is_assigned_nothing`; `no_host_taking_logins_assigns_nothing` (no decision, the next ask is answered again).
  - `land_of_a_live_host_is_named_to_the_asker`: `LandIs` to the asker, no decision.
  - `a_host_asking_for_land_the_store_says_is_its_own_is_told_assigned`.
  - `land_of_a_departed_owner_still_opens_a_leave_succession`: the existing `land_of_a_host_that_left_is_given_at_once` passes with `LandNeeded` in place of `LandWanted`.
  - `a_refused_assignment_changes_nothing_and_the_next_ask_decides_again`: `Owned`, `HostNotLive`, `HostDraining`, `Err`.
  - `a_non_ticker_queues_the_ask_and_decides_nothing`.
  - `an_assigned_answer_replayed_after_a_rebase_only_sends_again`: the projection after the replay equals the one before.
  - `a_static_coordinator_assigns_nothing`.
  - `more_free_rows_than_the_pass_bound_wait_for_the_next_read`: 70 asked → 64 decisions, 6 back in the lookups.
- [ ] **Step 2:** Run `just test-one game "--test it -- coordinator::free_land_is_assigned"`. Expected: FAIL (does not compile).
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then `just test-one game "--no-fail-fast"`, `just test-threads`, `just test-one server "--features test-hooks --test coordinator_rt"`, `just test-r2`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(coordinator): a need for free land is answered by an assignment to the least-loaded host (link protocol 10)`.

### Task 4: Hosts ask the coordinator; the host-side claim path is removed

**Gate:** the `go-nogo-r3` baseline ("Before Task 1").

**Files:**
- Create: `crates/game/src/world_manager/land.rs`.
- Modify:
  - `crates/game/src/world_manager/mod.rs`: consts :97–106 (`CLAIM_WANTED_EVERY_MS`, `CLAIM_UNANSWERED_REREAD_MS`, `LAND_WANTED_EVERY_MS` → `LAND_NEEDED_EVERY_MS`); fields :1241–1261 and their init :1589–1594.
  - `crates/game/src/world_manager/peers.rs`: `note_unowned` :1197, `unowned_needs` :1216, `take_claims_wanted` :1234, `preferred_claimer` :1262, `forget_claim_need` :1276, `ask_for_claims` :1290, `ask_for_land` :1337, `note_land_wanted` :1381, `claimed` :1388, the `ClaimWanted` arm of `peer_volatile` :1727, `pump_peers` :136–138.
  - `crates/game/src/world_manager/world_link.rs`: `apply_world_msg` :591 (the `Assigned` and `LandIs` arms).
  - `crates/game/src/world_manager/succession.rs`: `host_fields!` :316.
  - `crates/game/src/ownership.rs`: `Directory::preferred` :457 deleted.
  - `crates/server/src/claims.rs`: `DirectoryKeeper` :497, `ClaimingManager` :524, `pump` :618, the pacing consts :435–439, the unit tests :967–1178.
  - `crates/world-store/src/{rows,handle,thread}.rs`: `claim_free` (rows :412, handle :931/:936, thread :596) deleted; `succession.rs` notes lines about `claim_free`.
  - `crates/game/tests/common/multi_rig.rs`: `with_claims` :1398, `drive_claims` :1444, `claims_pending`, `claims_asked`.
- Test: `crates/game/tests/peer_schedule.rs`, `crates/game/tests/ownership.rs` (:185–206), `crates/server/tests/{hosts,takeover}.rs`, `crates/world-store/tests/{directory,succession,two_phase,fence}.rs`.

**Interfaces:**
- Consumes: `HostMsg::LandNeeded`, `CoordMsg::Assigned`, `CoordMsg::LandIs` (Task 3).
- Produces:

```rust
// game/src/world_manager/land.rs
pub const LAND_NEEDED_EVERY_MS: u64 = 1_000;
impl<S> WorldManager<S> {
    /// One ask per super-region per LAND_NEEDED_EVERY_MS, while the coordinator link is attached.
    pub(super) fn ask_for_land(&mut self, now_ms: u64);
    /// A `CoordMsg::Assigned`: rows the directory does not already know at that epoch or newer are queued.
    pub(super) fn assigned(&mut self, rows: Vec<(SuperRegion, u64)>);
    /// The server takes these, grows the fence, then calls `claimed`.
    pub fn take_assigned(&mut self) -> Vec<(SuperRegion, u64)>;
    /// A `CoordMsg::LandIs`: `directory.set(sr, Some(owner), epoch)`, then `flush_unowned`.
    pub(super) fn land_is(&mut self, sr: SuperRegion, owner: HostId, epoch: u64);
}
// fields: land_needed: BTreeSet<SuperRegion>, land_needed_sent: BTreeMap<SuperRegion, u64>,
//         assigned_rows: Vec<(SuperRegion, u64)>            (all three `other` in host_fields!)
// removed: claims_wanted, claim_asks, claim_wanted_sent, land_wanted, land_wanted_sent

// server/src/claims.rs
pub trait ClaimingManager {
    fn take_assigned(&mut self) -> Vec<(SuperRegion, u64)>;   // replaces take_claims_wanted
    /* claimed, directory_read, set_draining, take_directory_wanted, the forget/note defaults: unchanged */
}
```

- The needs of the ask: `land_needed ∪ unowned_waiting.keys() ∪` today's `ask_for_land` set (held sessions and `peer_waiting` of departed peers, `asked_elsewhere`), kept only where the owner is `Free`, `Unknown` or a host in `left_peers`. A super-region the host owns or that a reachable host owns leaves `land_needed`.
- `note_unowned` keeps its call sites and its `inheriting` skip; `Free` and `Unknown` insert into `land_needed`; `Unknown` also sets `directory_wanted`.
- The keeper's `pump`, in order: (1) the directory read's answer — **first** the heal: every read row with `owner == me`, absent from the fence, whose epoch is greater than the manager's known epoch for it (`0` when unknown) is `claims.add`-ed and passed to `manager.claimed`; then `set_draining`, `directory_read` and the rest as today; (2) start a read when wanted or due; (3) `take_assigned()` → `claims.add(Fence { owner, epochs, all_hosts: false })` → `manager.claimed(rows)`. The claim call, `claiming`, `no_claim_before`, `claims_wait_for_read`, `nothing_won_told`, `CLAIM_RETRY_MS` and `CLAIM_NOTHING_WON_RETRY_MS` go. The heal needs the manager's epoch: add `fn known_epoch(&self, sr: SuperRegion) -> Option<u64>` to `ClaimingManager`.
- The rig: `with_claims` becomes `with_assignment()` (it also turns the coordinator on) and `drive_claims` becomes `drive_assignment()`: per round, the ticker's `take_land_lookups` → `Input::LandOwners` from `claim_store` → `commit` → `execute_committed` → the answers applied; a host's `take_assigned` is followed by `claimed`. `drive_land` folds into it.
- Store tests that used `claim_free` as set-up use `claim_super_regions` (the upsert a boot's re-claim uses) or `assign_land`.

**Tests re-pinned** (a test that asserted the rendezvous preference changes meaning; none is weakened):

| Today | Becomes |
|---|---|
| `a_free_super_region_is_claimed_by_its_preferred_host_when_a_player_walks_to_it` (:5867) | `a_free_super_region_is_assigned_when_a_player_walks_to_it`: the owner is the least-loaded host, the player crosses once |
| `claim_wanted_reaches_the_preferred_host` (:5905) | `a_need_for_free_land_reaches_the_coordinator_within_its_bound` |
| `a_draining_hosts_share_of_free_land_is_claimed_by_another` (:5950), `a_draining_host_asked_to_claim_claims_nothing` (:5993) | `a_draining_host_is_assigned_nothing_and_its_need_is_met_by_another` |
| `every_host_draining_claims_nothing_and_waits` (:6026) | `every_host_draining_is_assigned_nothing_and_what_waits_waits` |
| `a_late_peer_learns_every_claim_at_its_hello` (:6103) | unchanged in meaning (`Owns` at hello); set-up through an assignment |
| `an_ask_a_draining_host_drops_makes_the_asker_reread_within_its_bound` (:10803) | deleted: no peer ask exists; its bound is covered by `a_need_for_free_land_reaches_the_coordinator_within_its_bound` |
| `transfer_into_a_free_super_region_waits_and_is_a_row_of_the_senders_cut` (:5728), `owns_from_a_peer_delivers_what_waited` (:6059), `a_client_whose_body_is_handed_into_just_claimed_land_is_told_the_host` (:5859), `player_logging_in_on_a_departed_hosts_land_is_placed_on_the_heir` (:8735) | unchanged in meaning; set-up through an assignment |
| `game/tests/ownership.rs` :185–206 (`preferred`) | deleted with the method |
| `claims.rs` `a_lost_claim_waits_for_a_fresh_read_before_claiming_again`, `a_claim_by_a_replaced_boot_fences_the_host`, `a_claim_that_won_nothing_is_paced` | deleted (no claim call); replaced by the two new keeper tests below |
| `hosts.rs` `fresh_world_under_claim_lays_out_like_static` (:1953) | `fresh_world_under_claim_is_assigned_by_load`: two idle hosts, a client asks for 8 super-regions → each row has exactly one owner and the split is 4 / 4 |
| `world-store/tests/directory.rs` `claim_free_takes_absent_and_free_rows_only`, `a_draining_host_claims_nothing`, `two_claimers_of_one_free_row_one_wins`; `succession.rs` `claim_free_does_not_wait_for_a_row_another_host_holds`, `a_dead_hosts_claim_of_a_free_row_is_refused`, `a_cleanly_left_boot_claims_nothing`; `two_phase.rs` `claim_waits_for_the_prepared_part…`, `claim_behind_a_staged_part_times_out_with_the_remedy`, `claim_holding_the_world_row_does_not_block_a_decision` | deleted where Task 2's `assign.rs` covers the same rule (the first six); the three `two_phase.rs` tests are re-pointed at `claim_super_regions_live` (the re-claim), which still has those waits |

- [ ] **Step 1: Write the failing tests.**
  - Seeded (`peer_schedule.rs`, swept):
    - The re-pinned tests of the table.
    - `two_hosts_asking_for_one_super_region_get_exactly_one_assignment`: both ask in the same round; `stored_owner(sr)` is one host, exactly one host's fence holds the row, what both held for it lands once; `assert_conserved_world`.
    - `an_assignee_that_dies_before_learning_its_land_is_succeeded_not_ended` (Review Focus 2): the rig commits the `AssignLand`, drops the `Assigned` message, kills the assignee → a `Crash` succession (not `end_landless`), the heir owns the row, the waiting transfer lands once.
    - `a_replica_dying_mid_assignment_assigns_exactly_once` (two replicas): the answer is dropped and the deciding replica killed; the host asks again; the other replica's read shows the owner → `Assigned` (the asker is the owner) or `LandIs`; one owner, one fence row.
    - `a_lost_assigned_is_learned_at_the_assignees_next_need`: drop the message, no kill; another host's transfer makes the assignee ask and learn.
    - `with_every_replica_down_nothing_is_assigned_and_owned_land_runs_on`: kill every replica; a player keeps crossing between two owned super-regions; a need for free land waits; a replica restarts and the need is met.
  - Server unit (`claims.rs`, fakes of `ClaimingManager`):
    - `a_lost_assigned_is_healed_by_the_directory_read` (Review Focus 2): the read shows `(sr, me, 4)`, the fence lacks it, the manager knows nothing → the fence holds `(sr, 4)` **before** `directory_read` is called (the fake records the order), and `claimed` got the row.
    - `assigned_rows_grow_the_fence_before_the_manager_learns_them`.
  - Real hosts: `hosts::fresh_world_under_claim_is_assigned_by_load`; `takeover.rs` passes unchanged (its leave tests change in Task 8).
- [ ] **Step 2:** Run `just test-one game "--test it -- --include-ignored peer_schedule::two_hosts_asking"`. Expected: FAIL (does not compile).
- [ ] **Step 3:** Implement. Delete `claim_free` and what only it used; update the `succession.rs` notes (the "claim_free" paragraphs go; `check_live` stays for the re-claim).
- [ ] **Step 4:** PASS. Then `just test-one game "--no-fail-fast"`, `just test-one game "--test it -- --include-ignored peer_schedule::"`, `just test-threads`, `just test-one world-store "--no-fail-fast"`, `just test-pg "--no-fail-fast -- --test-threads=4"`, `just test-one server "--features test-hooks --no-fail-fast"`, `just test-r2`, `just test-r3`, `just test-r4`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(host): a host asks the coordinator for land; the claim preference and claim_free are gone`.

### Task 5: A cut part can release super-regions; a fence can shrink

Store and fence only. No host releases yet.

**Files:**
- Modify: `crates/game/src/save_cut.rs` (`WorldCut` :84, and every constructor of it: `world_manager/mod.rs` :3097, the tests' helpers); `crates/world-store/src/rows.rs` (`cut_rows` :1751, `prepare_rows` :1681, `fence_check_sql` :28, `check_fence` :522, `release` :489 deleted); `crates/world-store/src/handle.rs` (`prepare_cut` :579, `release` :952 deleted), `thread.rs` (:606); `crates/world-store/src/succession.rs` (the notes); `crates/server/src/claims.rs` (`HostClaims` :152).
- Test: `crates/world-store/tests/{two_phase,directory,fence}.rs`, `crates/server/src/claims.rs` (unit).

**Interfaces:**
- Produces:

```rust
// game/src/save_cut.rs
pub struct WorldCut { /* … */ pub releases: Vec<crate::SuperRegion> }   // empty everywhere until Task 6

// server/src/claims.rs
impl HostClaims { pub fn shrink(&self, released: &[SuperRegion]); }       // drops those keys, nothing else
```

- `cut_rows` gains steps 2 and 5 of "Decisions" item 3. `fence_check_sql` takes its lock clause as a parameter.
- Refusals before any statement: a release not in `fence.epochs` → `Unsupported`; `CutEnd::Commit` with a non-empty `releases` → `Unsupported`.
- The standalone `release` (tests only today) is deleted with its two tests; the part is the only way land becomes free.

- [ ] **Step 1: Write the failing tests** (Postgres; `two_phase.rs` unless noted):
  - `a_releasing_part_frees_its_rows_when_the_cut_commits`: host A owns `s1` (epoch 2) and `s2`; its part releases `s1`; after the decision and `finish_staged` `s1` is `(NO_OWNER, 3)`, `s2` is untouched, and the part's region rows in `s1` are saved.
  - `an_aborted_releasing_part_keeps_the_land`: `(A, 2)` after `ROLLBACK PREPARED`.
  - `a_release_outside_the_fence_is_refused_before_any_write`; `a_release_at_another_epoch_is_fenced` (the fence says epoch 2, the row is at 3).
  - `a_one_phase_cut_with_releases_is_unsupported` (`each_backend`).
  - `a_row_a_staged_part_releases_is_owned_until_the_commit_then_assignable`: `assign_land` while staged → `Owned` in under 500 ms; after the commit → `Done { epoch: 4 }`.
  - `a_declaration_behind_a_staged_releasing_part_moves_only_what_is_left`: the declaration of A waits, the cut commits, the declaration then moves `s2` to the heir and `s1` stays free.
  - `the_releasers_next_part_commits_with_the_shrunk_fence_and_is_fenced_with_the_old_one`.
  - `fence.rs`: `a_region_load_in_released_land_is_fenced` (a fence still listing the row at its old epoch → `Fenced`).
  - `directory.rs`: `released_rows_are_free_and_the_old_fence_is_refused` and `release_waits_for_no_staged_part` are deleted (the first is the first test above; the second's rule no longer exists).
  - Server unit: `claims::tests::shrinking_drops_only_the_released_rows`, and `added_super_regions_are_checked_by_the_next_commit` still passes.
- [ ] **Step 2:** Run `just test-pg "--test it -- two_phase::a_releasing_part"`. Expected: FAIL (does not compile).
- [ ] **Step 3:** Implement. Write the part's release walk into `succession.rs`'s notes, with the one new cycle.
- [ ] **Step 4:** PASS. Then `just test-pg "--no-fail-fast -- --test-threads=4"`, `just test-one world-store "--no-fail-fast"`, `just test-one game "--no-fail-fast"`, `just test-one server "--features test-hooks --no-fail-fast"`, `just test-threads`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(store): a cut part releases super-regions with its commit; a fence can shrink`.

### Task 6: Idle land is released in a committed cut

**Files:**
- Modify:
  - `crates/game/src/world_manager/land.rs`: the idle scan, `land_busy`, `releasing`, `land_released`.
  - `crates/game/src/world_manager/mod.rs`: `maintain` :2374 (calls the scan), `try_complete_cut` :2980–3121 (`releases`), `cut_committed` :2584, `cut_settled_failed` :2629, fields and init.
  - `crates/game/src/world_manager/world_cut.rs`: `begin_world_cut` :234 (after the participation checks, before the regions are asked).
  - `crates/game/src/world_manager/peers.rs`: `owner_of_sr` :148 (the mask), `note_unowned` :1197 (no ask while masked).
  - `crates/game/src/world_manager/succession.rs`: `host_fields!`.
  - `crates/server/src/cuts.rs`: `prepare` :399 (keep the part's `releases`), the settle :592 (`shrink` before `cut_committed`); `crates/server/src/lib.rs` (`VOXEL_LAND_IDLE_MS`).
  - `crates/game/tests/common/multi_rig.rs`: `prepare_part` :976, `decide_settling` :1098, `apply_part` :1171, `free_land_of` :1499.
- Test: `crates/game/tests/peer_schedule.rs`, `crates/server/src/claims.rs` (unit), `crates/server/tests/takeover.rs`.

**Interfaces:**
- Consumes: `WorldCut::releases`, `HostClaims::shrink` (Task 5); `land_needed` and the ask (Task 4).
- Produces:

```rust
// game/src/world_manager/land.rs
pub const LAND_IDLE_MS: u64 = 300_000;
pub const LAND_IDLE_SCAN_MS: u64 = 1_000;
impl<S> WorldManager<S> {
    /// The server's dial (`VOXEL_LAND_IDLE_MS`) and the tests'. Default LAND_IDLE_MS.
    pub fn set_land_idle_ms(&mut self, ms: u64);
    fn land_busy(&self, sr: SuperRegion) -> bool;           // the table of "Decisions" item 2
    pub(super) fn scan_idle_land(&mut self, now_ms: u64);   // at most once per LAND_IDLE_SCAN_MS
    /// At BeginCut: the owned super-regions idle long enough; they are masked until the cut settles.
    pub(super) fn begin_release(&mut self, cut: CutId, now_ms: u64);
    /// The cut committed: free rows at epoch + 1, parked blobs dropped, mask lifted, waits flushed.
    pub(super) fn land_released(&mut self, cut: CutId);
    /// The cut aborted: the mask lifts, the land stays.
    pub(super) fn release_aborted(&mut self, cut: CutId);
    /// Test support and the `[server]` line.
    pub fn land_releasing(&self) -> Vec<SuperRegion>;
    pub fn land_released_total(&self) -> u64;
}
// fields (`other` in host_fields!): land_idle_ms: u64, land_idle_since: BTreeMap<SuperRegion, u64>,
//   idle_scanned_ms: u64, releasing: BTreeMap<SuperRegion, CutId>,
//   recently_released: BTreeMap<SuperRegion, u64>, land_released_total: u64
```

- `begin_release` releases nothing when the ownership is not a directory (`static`, all-in-one), when this host is in the cut's `leaving` (Task 8), or when any succession run is open on this host.
- `owner_of_sr` answers `Unknown` for a key of `releasing`. `note_unowned` for such a super-region inserts into `land_needed`, and `ask_for_land` skips keys of `releasing`.
- `try_complete_cut` sets `releases` to the keys of `releasing` whose cut is this one.
- `land_released`, per super-region: `directory.set(sr, None, epoch + 1)` (the epoch the directory held), remove every `parked` key in it, remove its `land_idle_since` entry, insert `recently_released[sr] = epoch + 1`, count it; then lift the mask and `flush_unowned`. A row leaves `recently_released` when the directory next shows it owned.
- The server: `prepare` remembers the part's `releases`; on a committed settle it calls `claims.shrink(&releases)` and **then** `manager.cut_committed`. `VOXEL_LAND_IDLE_MS` (a number of ms; unset = the const; unparsable = refuse the start) calls `set_land_idle_ms`.
- The rig: `prepare_part` asserts every release is owned by that host in `claim_store` at the fence's epoch; a committed decision frees those rows at `epoch + 1`; an abort changes nothing. The rig's hosts get `set_land_idle_ms` from a builder `with_land_idle_ms(ms)`. `free_land_of` is deleted where a test can use a real release, and kept (renamed `force_free_land_of`) only for tests that model an operator's edit.

- [ ] **Step 1: Write the failing tests.**
  - Seeded (`peer_schedule.rs`, swept, `with_assignment().with_world_cuts(..).with_land_idle_ms(2_000)`):
    - `idle_land_is_released_in_a_committed_cut_and_assigned_afresh_on_the_next_need`: a player walks into `s`, walks out, time passes; after a committed cut `stored_owner(s)` is `None`, the old owner's fence lacks `s`; a new need assigns it (to the least-loaded host) and the player crosses once.
    - `land_with_anything_aimed_at_it_is_not_released`: one case per row of the busy table (a running region, a pending arrival, an in-flight crossing, held mail, a connected home, a held spawn, a pinned region, an inheriting mask) → `land_releasing()` stays empty through three cuts.
    - `land_is_not_released_before_it_was_idle_long_enough`: a cut at 1 999 ms idle releases nothing; one at 2 000 ms does.
    - `an_aborted_cut_keeps_the_land_and_what_waited_is_delivered_here`: the cut aborts (a missing part); a transfer that arrived while masked lands once on the same host.
    - `a_player_walking_into_land_being_released_waits_at_the_seam_and_crosses_once` (Review Focus 1): the walk starts between the part's prepare and the decision, in both orders of the decision and the next rounds; `assert_conserved_world` every round; the player ends in the target region exactly once.
    - `land_released_and_assigned_back_to_its_releaser_starts_from_the_store` (Review Focus 4): A releases `s`; B is assigned it, changes a region (an entity moves in), parks and releases; A is assigned it again → A's region holds B's entity (A's old blob would not).
    - `a_total_crash_takers_idle_land_drains_back_to_free`: after `rebuild_from_last_cut` with one host holding everything, every super-region with no player is free within `land_idle_ms` plus two cuts.
    - `no_land_is_released_while_a_succession_is_open`.
    - `a_static_host_releases_nothing`.
  - Server unit: `claims::tests::a_directory_read_older_than_a_release_adds_nothing_to_the_fence` (Review Focus 3): the fence was shrunk and the manager knows `(sr, None, 5)`; a read showing `(sr, me, 4)` is applied → the fence still lacks `sr`.
  - Real hosts (`takeover.rs`, Postgres, `VOXEL_LAND_IDLE_MS=1500`): `idle_land_is_released_and_the_releasers_next_cut_commits`: a host owning two super-regions, one idle → the `super_regions` row is `-1`, the host's next two cuts commit, the host is not fenced.
- [ ] **Step 2:** Run `just test-one game "--test it -- --include-ignored peer_schedule::idle_land_is_released"`. Expected: FAIL (does not compile).
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then `just test-one game "--no-fail-fast"`, `just test-one game "--test it -- --include-ignored peer_schedule::"`, `just test-threads`, `just test-one server "--features test-hooks --no-fail-fast"`, `just test-r3`, `just test-r4`, `just determinism-wasm`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(host): idle land is released in the host's own cut part; the fence shrinks`.

### Task 7: Peers learn land is free; everything arriving for released land goes on exactly once

**Files:**
- Modify: `crates/game/src/peer/msg.rs` (`Reliable` :72, appended); `crates/game/src/world_manager/peers.rs` (`peer_reliable_in` :1883, `reliable_name` :2063, the `PeerEvent::Up` arm :1759, `flush_unowned` :1449); `crates/game/src/world_manager/land.rs` (`land_released` sends); `crates/game/src/world_manager/succession.rs` (`forget_host`); `crates/game/src/world_manager/world_cut.rs` (`end_leaves` :480); `crates/game/tests/common/multi_rig.rs`.
- Test: `crates/game/tests/peer_schedule.rs`, `crates/game/tests/peer_journal.rs` (:210).

**Interfaces:**
- Consumes: `recently_released`, `land_released` (Task 6).
- Produces:

```rust
// peer/msg.rs (appended after SuccessionReport)
Reliable::Released { rows: Vec<(crate::SuperRegion, u64)> }

// peers.rs
fn peer_released(&mut self, from: HostId, rows: Vec<(SuperRegion, u64)>); // directory.set(sr, None, epoch) per row
```

- `Released` is **not state-bearing**, and this is the argument: it names no entity, player or item; the store's row is the truth and a directory read repairs a lost one; applying it twice or late is harmless because `Directory::set` ignores an epoch older than the one it holds. It is never `InCut`, not journalled (`subjects` returns nothing for it), and `is_state` stays false.
- It is sent to every peer in `land_released`, and to a peer at its hello (after `Owns`) with all of `recently_released`.
- `forget_host(h)` and `end_leaves` set `directory_wanted` when the directory still names `h` as an owner.
- `flush_unowned` also walks `intent_mailbox`: mail for a region whose owner is now `Host(h)` and reachable is sent with `intent_to_peer` and removed. **First check** whether an existing path already forwards such mail (the comment at `mod.rs` ~:4437 says "its owner gets it with the mailbox"); if one does, keep it, add no second path, and record the ruling.

- [ ] **Step 1: Write the failing tests** (`peer_schedule.rs`, swept, unless noted):
  - `a_transfer_into_land_released_a_tick_earlier_waits_and_lands_once`: B sends a non-player transfer the tick after A's release committed, before `Released` is delivered (the rig holds the peer message) → it waits on A, is a row of A's next part, and lands once on whoever is assigned the land. A second case: B was already told → it waits on B and is a row of B's part.
  - `a_host_dying_after_its_release_committed_before_peers_heard_loses_nothing` (Review Focus 5), three cases:
    1. A still owns other land → a `Crash` succession moves that, the released land is free, B's directory shows it free after its read, a transfer B queued to A lands once.
    2. A is landless with no traffic → ended as landless, generation unchanged.
    3. A is landless and holds a transfer B sent after the cut → the world stops for a rollback within a bounded number of cut rounds (never an endless refusal); after `rebuild_from_last_cut` the entity exists once.
  - `a_late_peer_learns_released_land_at_its_hello`.
  - `a_lost_released_is_healed_by_the_directory_read`: the rig drops `Released`; B's need for that land asks after B's next read.
  - `a_handoff_into_land_being_released_parks_the_player_once_and_the_rejoin_places_it`: B hands a player to A while `s` is masked on A → the record is parked on A exactly once, B was told `PlayerParked`, and a spawn order for the player is held until the land has an owner, then placed.
  - `a_reply_held_for_released_land_reaches_its_new_owner`.
  - `release_and_assignment_racing_a_succession_conserve_the_world`: three hosts; A lapses while B's releasing part is staged and C is being assigned other land, over every order the sweep's seed gives → `assert_conserved_world`, one succession, the generation unchanged.
  - `peer_journal::intents_and_orders_are_never_journalled` gains `Released`.
- [ ] **Step 2:** Run `just test-one game "--test it -- --include-ignored peer_schedule::a_transfer_into_land_released"`. Expected: FAIL (does not compile).
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then `just test-one game "--no-fail-fast"`, `just test-one game "--test it -- --include-ignored peer_schedule::"`, `just test-one game "--test it -- --include-ignored peer_journal::"`, `just test-threads`, `just test-r3`, `just test-r4`, `just smoke`, `just web-check`.
- [ ] **Step 5:** Commit: `feat(peer): Released tells peers land is free; what arrives for released land waits for its next owner`.

### Task 8: A clean leave releases its land

**Files:**
- Modify: `crates/game/src/world_manager/mod.rs` (`try_complete_cut` :3097); `crates/game/src/world_manager/land.rs`; `crates/game/src/world_manager/world_cut.rs` (`end_leaves` :480: nothing new beyond Task 7's `directory_wanted`); `crates/game/tests/common/multi_rig.rs` (`declare_leave` :2021).
- Test: `crates/game/tests/peer_schedule.rs`, `crates/server/tests/takeover.rs`, `crates/game/tests/coordinator.rs`.

**Interfaces:**
- Consumes: `WorldCut::releases` (Task 5), the mask and `land_released` (Task 6).
- Produces: in `try_complete_cut`, when this host is in the cut's `leaving`: `releases` = every super-region the directory says this host owns, **if** the part's in-flight list is empty **and** no succession run is open on this host; otherwise empty. No change to the coordinator: a departed host that owns nothing is never an owner in a read, so no `Leave` declaration is asked for; one that kept its land is handled by `on_land_owner` as today.

**Tests re-pinned:**

| Today | Becomes |
|---|---|
| `takeover::drained_host_is_inherited_on_demand_and_nothing_is_lost` (:1666) | `a_drained_hosts_land_is_released_and_assigned_on_demand_and_nothing_is_lost`: after the leave cut the leaver owns no row and no succession exists; the slime and the re-login are served by the assignee |
| `takeover::a_leavers_restart_is_a_new_host_and_its_land_is_given_at_once` (:1957) | `a_leavers_restart_is_a_new_host_and_its_old_land_is_free`: the restart owns nothing; the waiting slime lands once on the assignee within 5 s |
| `takeover::both_hosts_stopped_together_leave_cleanly_and_new_hosts_inherit_by_leave_successions` (:1891) | `both_hosts_stopped_together_leave_cleanly_and_new_hosts_are_assigned_the_land`: no succession rows |
| `takeover::a_clean_leave_keeps_the_generation_and_its_waiting_transfers_reach_the_heir` (:1756) | unchanged: the leaver has in-flight rows, so it keeps its land and the `Leave` succession carries them. Assert that explicitly (a `Leave` succession row exists) |
| `peer_schedule::transfer_held_for_a_leaver_is_delivered_once_to_its_heir` (:8661), `leave_kind_succession_delivers_what_was_held_and_drops_nothing` (:8142) | the transfers are held by PEERS, so the leaver's part has no in-flight row and releases: delivered once to the assignee, no succession. Keep one variant of each where the leaver itself holds a waiting transfer → the `Leave` succession, as today |
| `world-store/tests/hosts.rs` `a_clean_stop_keeps_the_leavers_land` (:225) | unchanged (the store keeps what a part does not release) |

- [ ] **Step 1: Write the failing tests.**
  - Seeded: `a_clean_leave_releases_its_land_and_the_next_need_is_assigned_by_load`; `a_leaver_holding_a_waiting_transfer_keeps_its_land_for_a_leave_succession`; `an_aborted_leave_cut_keeps_the_land_and_the_retry_releases_it`; `both_hosts_leave_in_one_cut_and_all_land_is_free` (:5128's body, the new end state); the re-pinned two.
  - Core: `coordinator::a_departed_host_that_owns_nothing_is_never_declared`.
  - Real hosts: the re-pinned `takeover.rs` tests.
- [ ] **Step 2:** Run `just test-one game "--test it -- --include-ignored peer_schedule::a_clean_leave_releases"`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS. Then `just test-one game "--no-fail-fast"`, `just test-one game "--test it -- --include-ignored peer_schedule::"`, `just test-threads`, `just test-r3`, `just test-r4`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(host): a clean leave releases its land; a Leave succession only carries a leaver's in-flight rows`.

### Task 9: Metrics, logs, the twin's dial and its phase

**Files:**
- Modify: `crates/server/src/health.rs` (`render` :292, the names list :612); `crates/server/src/coordinator_rt.rs` (`answered` :2112, `report_writes` :2932); `crates/server/src/perf.rs` (`ServerReport::maybe_report` :134–173); `deploy/modules/world/variables.tf` and the host Deployment's env (`hosts.tf`); `deploy/envs/local/world/main.tf`; `scripts/twin_check.sh` (header :13–50 and a new phase); `scripts/dev_r3.sh` (header); `justfile` (`world-status` prints each host's land count).
- Test: `crates/server/src/health.rs` (unit), `just twin-check`.

**Interfaces:**
- Counters: `mmo_land_assigned_total` (a coordinator replica counts the `Assigned::Done` answers it applied), `mmo_land_released_total` (a host; from `land_released_total()`). The gauge `mmo_host_land` (a host: the rows of its fence).
- `[coordinator] window=` gains `assigned=`. The `[server]` line gains `land=` and `land_released=` after `unowned_waiting=` (per window: within the volume contract).
- Module `world`: `variable "land_idle_ms" { type = number, default = null }` → `VOXEL_LAND_IDLE_MS` on every host when set. The twin root sets `20000`. Orion's root does not set it.
- `twin_check.sh` phase **7c**, after 7b: (1) a join through the login service (phase 7's helper) → within 30 s exactly one `super_regions` row has an owner that is a live host id (`assigned`); (2) no client connects, so within 120 s that row's owner is `-1` at a higher epoch and the host's `mmo_land_released_total` grew (`idle, released`); (3) a second join → the row has an owner again at a higher epoch (`re-assigned`); the generation is unchanged and no succession row exists. The header's list gains the phase.

- [ ] **Step 1:** Write `health::tests::land_metrics_render_with_the_role_label` (the three names appear with `role="host"` / `role="coordinator"` as stated). Run: FAIL.
- [ ] **Step 2:** Implement the counters, the log fields, the module variable, the script phase.
- [ ] **Step 3:** `just test-one server "--lib -- health::"` PASS. `bash -n scripts/twin_check.sh scripts/dev_r3.sh`. `just --list` parses. `tofu -chdir=deploy/envs/local/world validate`.
- [ ] **Step 4:** `just twin-down`, `twin-up`, `twin-images`, `twin-apply`, `twin-check`. Expected: PASS including 7c. Record the run folder and each replica's `window=` lines (`assigned=`) in the ledger.
- [ ] **Step 5:** Commit: `feat(world): land metrics, the idle dial, and the twin's assigned-idle-released-re-assigned phase`.

### Task 10: Documentation and closing

**Files:** `CLAUDE.md`, `TODO.md`, this plan ("As built").

- [ ] **Step 1: `CLAUDE.md`** (filtered staging; Dawid has uncommitted hunks). Rewrite, in "Many hosts": the **Ownership** bullet (`claim` = super-regions assigned by the coordinator on demand; idle land released in the host's own cut part after `LAND_IDLE_MS`; a draining host is assigned nothing; with every replica down nothing is assigned); the **Leave and drain** bullet (a clean leave releases its land; the `Leave` succession remains only for a leaver with in-flight rows); the **Lease** bullet (the beat carries the load). In "Owner epochs & fencing": the part's release, `HostClaims::shrink`, "fence before use; shrink before the next part". In "Link rules": `LINK_PROTOCOL_VERSION` 10. In "The coordinator's state…": `COORDINATOR_STATE_VERSION` 6 (re-forge), `AssignLand` among the store decisions, `LandOwners` ticker-only. In "Peer link rules": `Released` (not state-bearing). Add `VOXEL_LAND_IDLE_MS` to the k8s variables paragraph and the twin's phase to the twin sentence. Migration `0018_host_load`.
- [ ] **Step 2: `TODO.md`**: an "Elastic 3" block: built, not accepted; the hand checks — (1) **`just go-nogo-r3 0|1|2|5` pooled, compared with the baseline taken before Task 4**: it must match on `correction_max` (0.00 u), `confirm_ms` p95 and the share of crossings with ≥ 5 corrections, per delay, within the run-to-run spread the baseline shows; a run is invalid if a release or an assignment happened inside the probe window on the seam's two super-regions (look for `land_released=` > 0 on the `[server]` lines of the window); (2) live, `just dev-r3 2` + `just run-online` with `VOXEL_LAND_IDLE_MS=30000`: walk into free land (assigned, no stall beyond a second), walk away and wait (released), walk back (assigned afresh, possibly to the other host); (3) the client tests still owed. The known limits: departs 13; nested successions made likelier by frequent landing (softened by `ASSIGN_MIN_ATTACHED_MS`); the rejoin of a player handed into land being released; a wandering creature at a border can make idle land churn every `LAND_IDLE_MS`; one replica's `provisional` load is lost when the ticker moves. What E3 leaves: moving occupied land (E5), `desired_hosts` and scaling (E4), re-keying `in_flight` from host to region (E5), rebalancing.
- [ ] **Step 3:** Write "As built": commits, rulings, plan defects, the twin run, final suites. Run each touched crate's whole non-client suite once: `just test-one game "--no-fail-fast"`, `just test-one game "--test it -- --include-ignored peer_schedule::"`, `just test-one world-store "--no-fail-fast"`, `just test-pg "--no-fail-fast -- --test-threads=4"`, `just test-one server "--features test-hooks --no-fail-fast"`, `just test-one svc-link "--no-fail-fast"`, `just test-one login "--no-fail-fast"`, `just test-threads`, `just test-r2`, `just test-r3`, `just test-r4`, `just determinism-wasm`, `just smoke`, `just web-check`.
- [ ] **Step 4:** Commit: `docs: Elastic 3 — the coordinator assigns land, idle land is released, as built`.

Then the whole-branch review split by area (store + server, game, ops + docs), one fix wave, a scoped re-review, as Elastic 2b closed.

## Not in this plan

- Moving occupied land, and a re-home in place (E5). An assignment is only of free land; a release only of idle land.
- `desired_hosts`, the reconciler, scaling decisions, scale to zero (E4). E4 reads this plan's load columns and its empty hosts.
- Re-keying `in_flight` from host to region (E5), which would let every leave release.
- Rebalancing between live hosts.
- Nested successions (decision 9's residual).
- The world panel (Elastic 2b Task 11 is still blocked on `crates/panel` being committed).

## Known limits (to carry into `TODO.md`)

- A host that released all its land, received a stale peer transfer and died before the next committed cut rolls the world back (departs 13).
- A host made landed by an assignment during an open succession run that dies before the settle rolls the world back (the existing residual). Assignments make landing more frequent; the freshness filter softens it.
- A player handed across a host seam into land that is inside its release window is parked and rejoins (seconds), instead of waiting at the seam.
- With every coordinator replica down, free land stays free.

## Self-review notes

- **Handoff → tasks:**

  | Handoff item | Covered by |
  |---|---|
  | §4.1 assignment as a decision | T2 (store), T3 (core), T4 (hosts); re-ruled in departs 4, 5, 7, 10 |
  | §4.1 who is chosen, the crash-loop guard | T3 (`pick_assignee`) |
  | §4.1 `claim_free` goes | T4 |
  | §4.2 load in the beat | T1; departs 9 |
  | §4.3 release in the part, the freeze, the shrink | T5 (store, fence), T6 (manager); departs 3, 6, 8 |
  | §4.3 "what arrives" | "Decisions" item 4; T6, T7 |
  | §4.4 `Released` | T7; departs 1 |
  | §4.5 a total crash drains back | T6 (`a_total_crash_takers_idle_land_drains_back_to_free`) |
  | §4.6 the clean leave | T8; departs 12 |
  | §4.7 tunables | "Decisions" item 5 |
  | §5.1 the baseline | "Before Task 1"; T4's gate; T10's rerun rule |
  | §5.3 the rig | folded into T2–T6 (departs 11) |
  | §5.8 metrics, logs, twin | T1 (load gauges), T9 |
  | §6 the conservation table | player into releasing land T6; transfer a tick later T7; host dies after release T7; replica dies mid-assignment T4; two askers T4; assignee dies T4; racing a succession T7 |

- **Names across tasks:**

  | Name | Defined in | Used by |
  |---|---|---|
  | `HostLoad`, `HostSeen.load`, `TickLoad` | T1 | T3, T9 |
  | `AssignLand`, `Assigned`, `DecisionAnswer::Assigned`, `assign_land_in`, `assign_land` | T2 | T3, T4, T5 |
  | `HostMsg::LandNeeded`, `CoordMsg::Assigned`, `CoordMsg::LandIs`, `Input::LandOwners { asked, rows }`, `take_land_lookups`, `pick_assignee`, `LeaseWatch::known_for` | T3 | T4 |
  | `LAND_NEEDED_EVERY_MS`, `land_needed`, `ask_for_land`, `assigned`, `take_assigned`, `land_is`, `ClaimingManager::known_epoch`, `with_assignment`, `drive_assignment` | T4 | T6–T8 |
  | `WorldCut::releases`, `HostClaims::shrink` | T5 | T6, T8 |
  | `LAND_IDLE_MS`, `set_land_idle_ms`, `land_busy`, `begin_release`, `land_released`, `release_aborted`, `releasing`, `recently_released`, `land_released_total`, `with_land_idle_ms`, `VOXEL_LAND_IDLE_MS` | T6 | T7–T9 |
  | `Reliable::Released`, `peer_released` | T7 | T8 |

- **A world plays after every task:** T1–T3 change nothing a running world observes (T3's messages are not sent yet). T4 switches the ask in one commit, with the keeper's heal. T5 adds an unused part field. T6 turns release on behind a five-minute idle time. T7 makes peers learn sooner what a read already tells them. T8 changes only a leave's end state.
- **Proportion:** ten tasks against the handoff's 7–9; the risk is in T4, T6 and T7, and each carries seeded conservation tests; T1, T2, T5 and T9 are narrow.

## As built (2026-11-19)

Built on `develop` from `94d292ce`, executed subagent-driven; the ledger and the per-task briefs, reports and review
findings are in `.superpowers/sdd/2026-11-18-elastic-3-land-assignment/`. Every task was reviewed clean (Task 6 after
two fix rounds; Tasks 6–9 by reading only, see "Tests" below). Not yet accepted: the whole-branch review follows this
section and Dawid's hand checks (TODO.md, "Elastic 3") are pending; nothing ran live or on orion. The `go-nogo-r3`
baseline was NOT taken before Task 4 (below).

### Commits

Foreign commits are interleaved on `develop` and not listed (the repository-wide test clean-up `6906f295..a450b3df`,
`904f3ea0`, `a9165c9f`, worldgen `bca55929`, parry `22c9006c`, docs `6c40b65c`, and others' docs commits).

| Task | Commits |
|---|---|
| plan | `94d292ce` docs: Elastic 3 plan — the coordinator assigns land, idle land is released |
| T1 load in the beat | `bccd2c67` feat(host): the heartbeat row carries the host's load |
| T2 `AssignLand` | `05bd3606` feat(store): AssignLand gives a free super-region to a live host inside a coordinator transition |
| T3 the coordinator answers | `939ee28d` feat(coordinator): a need for free land is answered by an assignment to the least-loaded host (link protocol 10) — **the last commit before Task 4: the `go-nogo-r3` "before" baseline is measured from it** |
| T4 hosts ask | `d9d4a426` feat(host): a host asks the coordinator for land; the claim preference and claim_free are gone |
| plan amendment | `ff137989` docs(plan): Elastic 3 — no whole crate suites per task; they run once at the closing task |
| T5 the part releases | `6fe4af64` feat(store): a cut part releases super-regions with its commit; a fence can shrink |
| T6 idle release | `7baf5c81` feat(host): idle land is released in the host's own cut part; the fence shrinks; fix round 1 `32208681` fix(host): a record parked on released land is placed by its land's owner, never refused; fix round 2 `64aece2d` fix(host): a parked record handed to its land's new owner keeps its adventure claim |
| plan amendment | `b4955c80` docs(plan): Elastic 3 — Tasks 7-10 under the regression-only test rule |
| T7 `Released` | `db6e0ab4` feat(peer): Released tells peers land is free; what arrives for released land waits for its next owner |
| T8 the clean leave | `f12e5293` feat(host): a clean leave releases its land; a Leave succession only carries a leaver's in-flight rows |
| T9 metrics, twin | `959f1f3c` feat(world): land metrics, the idle dial, and the twin's assigned-idle-released-re-assigned phase |
| T10 docs | `docs: Elastic 3 — the coordinator assigns land, idle land is released, as built` (CLAUDE.md, TODO.md, this section) |

### "To confirm with Dawid" — defaults taken (not answered before execution)

All five were taken as written: the assignment rule (fewest players, then fewest running regions, then a rendezvous;
low headroom and freshly seen hosts skipped while another qualifies); `LAND_IDLE_MS` = 5 minutes; a clean leave
releases all its land unless the leaver has `in_flight` rows (or an open succession run); assignment stops while every
coordinator replica is down; a hand-off into land being released parks the player and makes the client rejoin.

"Before Task 1" (the `go-nogo-r3` baseline): Task 4 started without it. Ruling (controller): Dawid said "execute it"
after being told of the gate, a measurement is his to run, and the baseline is not lost — it is measured from
`939ee28d`, the commit before Task 4 — cost if wrong: Dawid checks out that commit to measure the "before". TODO.md's
hand check 1 says so.

### Two rule changes by the owner during execution (2026-11-19)

1. **No whole crate suites per task** (during Task 5): CLAUDE.md's testing rule; the plan's per-task Step 4 lists were
   over-broad. Each task verified with its touched tests + `smoke`; whole suites were to run once in Task 10
   (`ff137989`).
2. **Only regression tests are committed** (mid-Task 6, then repository-wide): implementers write only regression
   tests for bugs actually seen and run no tests (compile checks only: `smoke`, `web-check`). Dawid's clean-up
   (`6906f295..a450b3df`) then deleted EVERY test Tasks 1–6 had committed (world-store's `tests/` emptied,
   `peer_schedule.rs` down to 432 lines, `takeover.rs` to 467). From Task 7 the plan's named tests became reasoning
   obligations for the implementer and the reviewer and hand checks in TODO.md (`b4955c80`); Task 6 was reviewed on its
   source diff only. Execution paused while the clean-up landed (Task 6 implemented, not reviewed) and resumed after it.

### Tests

Tasks 1–6 wrote the plan's tests (TDD; RED / GREEN evidence in each `task-N-report.md`) and ran them green at their
commits: T1 world-store + `test-pg` 153, game `coordinator::` 135; T2 `test-pg` `it` 160, game `it` 954,
`test-threads` 299; T3 game `it` 969 (`coordinator::` 150), `test-threads` 299, `coordinator_rt` 10/10, `test-r2`
20/20, login 85 + 3, svc-link 14 + 1; T4 `peer_schedule::` 188/188, game `it` 967, `test-threads` 302, world-store
157 on both engines, server lib 109, `test-r2` 20/20, `test-r3` 52/52, `test-r4` 27/27; T5 `test-pg` 163, world-store
163, game `it` 967, server lib 110, `test-threads` 302; T6 `peer_schedule::` 197/197, server lib 112, the new
`takeover` test 1/1, `test-threads` 311 (all before its fix rounds). The clean-up deleted all of them. Task 6's two fix
rounds and Tasks 7–9 ran `smoke` and `web-check` only (no tests written). **Nothing exercises Elastic 3 now** except
the twin's phase 7c: the release path, `Released`, `forward_mailbox`, the clean leave's release, the peers' re-route
and the parked-record fix rest on review by reading plus one twin run. The scenarios are TODO.md's hand check 3.

### Rulings

Execution-wide:
- No git worktree; `develop` in place (CLAUDE.md). Files with foreign uncommitted hunks (justfile, CLAUDE.md, TODO.md,
  `deploy/modules/world/variables.tf`, `deploy/envs/local/world/main.tf`) staged by filtered patch.
- Commit trailers name the session's model (`Claude Opus 5.5`), as the session's attribution reminder says, not the
  contract's.
- No todo list was kept beside the ledger (`progress.md`): the ledger is the one record of what is done, pending and
  ruled — cost if wrong: none (a second list would only have had to be kept in step).
- Task 10's documentation was not reviewed on its own: its review was folded into the whole-branch review (area C, ops
  + docs) — cost if wrong: a documentation error lives one review longer (area C found seven, all in the fix wave).

T1: `TickLoad` is threaded through `ThreadRegionSpawner::with_tick_load` (not a global); the boot beat sends the
default load and sets no gauge; `tests/schema.rs` `EXPECTED` is a column list (it gained the three columns).

T2: SQL errors of `assign_land_in` go through the generic `From<sqlx::Error>` (40P01 / 55P03 → `LockConflict`); the
lock notes gained the drain-mark sentence (a drain `UPDATE` waits on the assignee's share lock like a beat);
`answer_awaited` returns `true` for `Assigned` (no mark exists).

T3:
- The provisional load entry is added when the assignment is DECIDED (removed on a refusal), not at `Done`: eight asks
  in one read must spread 4 / 4 — cost: an overcount ≤ 10 s after a rebase drops an uncommitted decision.
- `assign_askers` is dropped at the answer's first apply; a replayed `Done` re-sends only `Assigned` (the other askers
  re-ask within a second); an owned row in a later read drops a stale entry.
- `ASSIGN_WARN_EVERY_MS` (60 s) and the kept field `assign_warned_ms` throttle "no host takes logins".
- `pick_assignee` also requires a live, dirty lease row (the store refuses `HostNotLive` otherwise).
- `LeaseWatch::known_for` per row `boot_received_ms`; `latest_received_ms()` is the watch's own "now".

T4:
- `with_assignment()` sets the `Succession` policy and creates no coordinator (tests that call `with_claims` then
  `world_cuts` would make two).
- Land of a departed host is asked only at the paced scan (once per period), `Free` / `Unknown` at once: asking it the
  moment a placement needed it got the `Leave` succession declared before the next cut, exposing the pre-existing
  hazard of an heir registered during the leave cut (FLAGGED, below).
- `DIRECTORY_READ_RETRY_MS` (1 s): a failed directory read is retried after it (the old retry lived under
  `claims_wait_for_read`, gone).
- `told_where_its_body_went` accepts the body parked on its owner (the home naming the owner is the test's point);
  `a_cleanly_left_boot_claims_nothing` kept, re-pointed at `assign_land`; the three `two_phase.rs` claim tests already
  used `claim_super_regions`.

T5: **`assign_land_in` reads the row plainly first** (absent → `INSERT`; free → `UPDATE … WHERE owner = -1`; owned →
`Owned`, nothing locked) — the plan's `INSERT … ON CONFLICT DO NOTHING` WAITS for an uncommitted updater of the
conflicting row (a staged releasing part): an assignment timed out (~10 s, `55P03`) behind it. Reviewer-confirmed
safe under READ COMMITTED. The `succession.rs` notes were rewritten. `release_refusal` refuses a release outside the
fence and a one-phase cut with releases before any statement; the standalone `release` is deleted.

T6:
- The busy table is one pass, `busy_land() -> BTreeSet<SuperRegion>` (not a per-row `land_busy(sr)`: thousands of rows
  per host).
- The mask lifts at `cut_failed` (a decided abort, or "nothing of it staged here"), not at `world_cut_failed`: a failed
  part may still be staged, and a region load in that land would wait for its decision.
- The server shrinks by the part's releases ∪ `manager.releases_of(id)`: a `CutPending` part this writer did not stage
  has no `releases` here.
- At `land_released`: `parked`, `terrain_cache`, `pristine_terrain` and `unavailable` entries of that land dropped;
  `stored_regions`, `locations`, `parked_bodies`, `incarnation`, `lod_cache` kept.
- `begin_release` refuses while `leave_parked` or for the leaving cut (Task 8 decides a leaver's release).
- Fix round 1 (review Important 1): a player held here ONLY as a parked record is placed by the owner of its record's
  region (`Me` → `Here`; another host → `Here`, then `move_to_peer` hands the record there; masked / `Free` / `Unknown`
  → `Wait`, the land asked for); a refused `place_player` puts the record back unless another host owns its land;
  `busy_land` counts unexpired spawn pins' regions; a committed release drops the parked records its part wrote
  UNCHANGED (`released_parked`; else two writers of one `players` row); `release_aborted` on a failed `begin_cut_at`;
  masked-land arrivals log below error; `VOXEL_LAND_IDLE_MS=0` refused. A player restored through `move_to_peer`
  lands at the region's landing spot, not its parked pose.
- Fix round 2 (Important A): `move_to_peer`'s restore lost the adventure claim → `restored_adventure` shared with
  `restore_player`, `adventurer_of` set before the move.

T7:
- No existing live path forwarded `intent_mailbox` mail when its land changed hands (the comment meant cut rows /
  `restore_in_flight`) → the new `forward_mailbox` at the end of `flush_unowned`; it also forwards a reply held because
  two directories disagree (no loss, no duplicate; converges on the store), and a host sent an intent / reply for land
  it neither owns nor masked / released sets `directory_wanted`.
- `peer_released` runs no flush (free land makes nothing deliverable); `finish_move` unchanged (a move whose target
  went free mid-move already parks on the sender via `is_unavailable`).

T8:
- **`reroute_held_for_landless_leavers`** (on PEERS, before `forget_landless_leavers`): the plan said `end_leaves`
  needs nothing new, but nothing delivered a leaver's held session without a succession, and `forget_host` would drop
  it. Transfers → `wait_for_owner`, intents refused back, replies → mailbox, the leaver's journal entries dropped;
  exactly once even when a directory read beats a `Leave` order (what a peer held is its own state). A re-routed
  leaver is forgotten only when its row is pruned (`left_peers` keeps it out of `outside`).
- `DirectoryKeeper::new` lost its unused `_boot`.

T9:
- Phase 7c waits 210 s for the release, not 120 s: a placed order pins its region for `SPAWN_PIN_MS` (120 s) while it
  waits for a session that never comes, then the 20 s dial, then the next cut (~150 s measured).
- 7c first waits until no `super_regions` row is owned (phase 7's land must go back); "no succession row" = the
  `successions` count unchanged across 7c; the coordinators' `mmo_land_assigned_total` must grow by ≥ 2.
- `mmo_host_land` / `mmo_land_released_total` refresh once per `METRICS_REPORT_INTERVAL` (5 s), from the `[server]`
  report.

### Plan defects found

- **`INSERT … ON CONFLICT DO NOTHING` waits behind a staged releasing part** (T5): the lock walk said statements 2 and
  3 never wait; Postgres' unique check waits for an uncommitted UPDATER of the conflicting tuple. Fixed by the plain
  read first.
- **A region load in land a staged part releases WAITS for the decision** (`FOR SHARE` vs the part's `FOR NO KEY
  UPDATE`; before, two share locks coexisted) — the mask must be up before the part reaches the store and stay until
  after the shrink (T5 found, T6 built it so).
- **The shrink by the part's `releases` alone** misses a `CutPending` part this writer did not stage (T6: union with
  `releases_of`).
- **`place_spawn` placed a parked record `Here` on released (or masked) land** (T6 review Important 1): the session was
  then refused (`REGION_UNAVAILABLE_REFUSAL`) and the record was not put back — every login after a committed release
  refused, and on an aborted cut the newest record dropped. The plan's "a parked player moves with `move_to_peer`"
  held only for land another host owned. The busy table also missed spawn pins.
- **`move_to_peer` restoring a parked record lost the adventure claim** (T6 fix round 2).
- **No live path forwarded held intent mail** (T7): the plan suspected one; there was none.
- **Peers' held messages for a released leaver** were never delivered and `forget_host` would drop them (T8): the plan
  said `end_leaves` needed nothing beyond Task 7.
- **The twin's 120 s release wait vs the 120 s spawn pin** (T9: run 1 failed 7c).
- **Per-task whole suites** (the Step 4 lists) were against CLAUDE.md's testing rule (owner, during T5).
- **The test strategy** (seeded conservation tests per task, "a task that changes who holds what is not done without a
  conservation test") was overtaken by the regression-only rule: Tasks 7–8, the riskiest after T4 / T6, have no test.
- The plan's provisional load at `Done` cannot spread a burst decided in one read (T3: at the decision).
- Asking for a departed host's land the moment a need appears exposes a pre-existing `Leave`-heir hazard (T4, below).

### Deferred minors (for the whole-branch review)

**FLAGGED (conservation / correctness):**
- T6: a record parked on land A does NOT own after the releasing part was built (`park_handed` from a peer not yet
  told, or the re-park in `place_player`) stays on A and A writes it in every part; with ≥ 3 hosts a gate that knows
  neither copy may place the player from the store on X → the player twice, two parts writing one `players` row (those
  cuts abort). Suggested: at every committed part drop unchanged parked records on land not `Me`. T7's `Released`
  narrows the window to its flight time; not closed.
- T8 (b): `on_land_owner` / `leave_wanted` has no guard against declaring a `Leave` for a clean row that owns nothing
  (a stale owners lookup applied after the leaver released and detached), and the store's declaration does not refuse
  an empty `moved` → an empty `Leave` succession could open (it must settle; contradicts "never declared").
- T4 / T8, pre-existing: a `Leave` succession declared before the next world cut whose heir registered DURING the leave
  cut never applies its order (its journal counts from the cut before) — rarer now, not closed.
- T8 (a), pre-existing: an heir whose directory read lands before its succession order sees the moved rows as `Me`
  with no mask.

By task:
- T1: the load window resets when a beat is SENT, not when it succeeds (a failed beat loses that window's worst tick);
  `HostLoad` re-export out of alphabetical order; a missing blank line in `ownership.rs`.
- T2: `coordinator_tx.rs` long doc line (the other T2 minors were fixed in T3 / T4).
- T3: a super-region pending in `assign_askers` can be decided again by the next read (a second provisional entry,
  overcount ≤ 10 s); provisional removal by `rposition(host)`, not by `(host, sr)`; `ASSIGN_PER_PASS_MAX`'s doc says
  "transition", it bounds a read; a new ticker before its first `hosts` read warns "no attached host takes logins"
  (the pre-existing `unused_mut` in `tests/coordinator.rs` went with that file in the clean-up).
- T4: a row both queued and healed in one pump is told twice (harmless); the rig's `assign_answers_lost` drops every
  decision's answer of that transition; `drive_land` survives as lease read + `Leave`; stale docs naming `LandWanted`
  as live (`coordinator/succession.rs` ~:401, ~:1420, `placement.rs` ~:25).
- T5: an orphaned doc line at `thread.rs` ~:109 ("Free the fence's super-regions." heads `BootstrapCoordinator`'s doc).
- T6: an idle stamp survives a busy spell shorter than one scan (doc line only); `land_released` returns early with no
  directory and leaves that cut's `released_parked` entry.
- T7: `directory_wanted` from a disagreeing intent re-reads the RIGHT side while a stale sender keeps sending per-tick
  intents → back-to-back directory reads (one in flight, no minimum spacing; space them ≥ `DIRECTORY_READ_RETRY_MS`
  or make the sender read); `intent_to_peer` from `forward_mailbox` does not raise `hops` (no cap on bounces while
  directories stay apart); mail keyed by a foreign region whose own target is this host's / free stays until restart
  (liveness, pre-existing shape; the debug line miscounts); `Released` goes to every key of `peers`, possibly
  forgotten hosts.
- T8: the re-route's `journal.take` drops `Received` entries too, unguarded (rests on leave pairs never receiving a
  wrapped message from the leaver) → guard / `debug_assert`; `Owns` / `Released` broadcasts go to kept left peers, so
  each `forget_landless_leavers` call re-fires the re-route (inflates `peer_dropped_for_dead`, re-arms `taken`, a
  misleading WARN at the prune); a new warn for a landless-at-leave leaver with held entries.
- T9: `twin_check.sh` `sql()` discards psql's stderr (a failing query reads as "no row" → a timeout without the SQL
  error); the run folder keeps no phase transcript (7c's epochs could not be re-read there) and captured no `[server]`
  line with `land=`.

### The twin

- T9, run 1 `target/twin/check-20261119T123900Z`: phases 1–7b PASS, 7c FAIL at the 120 s release wait (the spawn pin).
- T9, run 2 `target/twin/check-20261119T125023Z`: **PASS in 802 s**, every phase 1, 1b, 2, 2b, 3, 4, 5, 6, 7, 7b, 7c,
  8, 9, 10, 11. 7c: no owned row after 123 s; a join → super-region (0, 0) assigned to host b (id 3) at epoch 3 at once;
  released after 151 s (free at epoch 4; host b's `mmo_land_released_total` 0 → 2); a second join → re-assigned to host
  b at epoch 5 at once; generation 0 unchanged, successions 0 → 0, land assigned 0 → 2. `[coordinator] window=` lines
  with `assigned=1` at 12:51:21 (`version=36`), 12:54:31 (117), 12:57:01 (191); per replica (phase 11) one with 1, one
  with 2, five with 0.
- The images were built from the working tree, which held other sessions' uncommitted worldgen edits at the time.
- Environment: the disk at 90 % made kubelet's image GC delete the freshly imported images before the pods used them
  (`ImagePullBackOff`, an interrupted apply's orphan `login` Deployment, deleted); Task 9 kept them with a temporary
  re-import loop (not committed). `twin-images` / `twin-apply` keep failing until disk is freed or the GC threshold
  raised (TODO.md).
- Not exercised: a connected player, a host owning land that dies (a `Crash` succession), two hosts asking for one
  row, a release racing anything. Not checked: whether phase 9's drain and phase 10's stop released land (host b may
  still have held 7c's re-assigned row; those phases assert no land counts and no `[server]` line was kept).

### Final checks (T10, at `959f1f3c` + docs)

Task 10 changed documentation only. Both checks were blocked by other sessions' uncommitted work in the tree, not by
Elastic 3:

| Check | Result |
|---|---|
| `just smoke` | FAIL at its first step, `test-audit`: one unmarked test, `crates/game/tests/tmp_dwarf_perf.rs` (untracked, "TEMPORARY profiling harness — delete before commit", another session's). Its other four steps run by hand with the same CPU cap: `game --test it -- rollback_restore:: region_runner::` 4 passed, `server --lib` 3 passed, `server --tests --features test-hooks` and `client --lib --bins --tests` checked with no error (12 s). |
| `just determinism-wasm` | NOT RUN: `game` does not compile for wasm32 (`E0425` / `E0433`: `Instant`, `tick_start`, `TMPPROF` in `crates/game/src/region.rs`, uncommitted profiling edits of another session). No Elastic 3 task ran it (Task 6's Step 4 listed it; Tasks 7 and 8 judged it not needed: no sim math, nothing state-hashed — the serialized forms that changed are the coordinator core, the link and peer messages and `WorldCut`). Owed once those edits are gone. |

The plan's Step 3 suite list is void: `test-threads`, `test-r2`, `test-r3` and `test-pg` no longer exist, and the
suites it named hold no Elastic 3 test. `just test` / `just test-full` were NOT run — they are Dawid's to ask for
(CLAUDE.md, Testing). Client tests were not run.

### Not done

- **The whole-branch review** (store + server, game, ops + docs), its fix wave and a scoped re-review — next, as
  Elastic 2b closed. The FLAGGED items above are its first input.
- Dawid's hand checks (TODO.md, "Elastic 3"): the `go-nogo-r3` before / after, the live walk with a short idle dial, the
  scenarios no test exercises, the client tests.

### Final review fix wave (2026-11-19)

The whole-branch review ran in three areas at `aa13107e` (A: world store + server, B: `game`, C: ops + docs), by
reading; its findings are `final-findings-{A,B,C}.md` in the ledger's folder, the controller's list and rulings
`final-fix-wave.md`, the fixer's report `final-fix-wave-report.md`. ONE fix wave, one fixer:

| Commit | What |
|---|---|
| `bd735340` | fix(host): inherited land is never healed, releases carry their epoch, one holder per parked record |
| `f04919d8` | fix(coordinator): a landless leaver's in-flight rows get an heir; a Leave waits for a later cut; never an empty Leave |
| its docs commit | docs: Elastic 3 final review fix wave (this section, CLAUDE.md, TODO.md, the stale docs in code, `twin_check.sh`) |

No test was committed (the regression-only rule; none of these was seen in play or in a log). No migration, no new
wire message, no new input, no durable core field: link protocol 10 and `COORDINATOR_STATE_VERSION` 6 stand, a dev
world of `aa13107e` still loads.

**Must fix**

1. **The heal must not take an heir's inherited land** (A-F1 = B-I1). `DirectoryKeeper::pump`
   (`server/src/claims.rs`): no heal while the read's `hosts` rows show an open run (any row `succeeding`); every row
   this host owns in the store that its fence does not hold at that epoch (`unfenced`) and that was not healed is
   WITHHELD from the rows handed to `directory_read` — the manager never sees an unfenced row as `Me`, which also
   closes the older hazard of an heir's read before its order (FLAGGED T8 (a)). Beyond the list: the COORDINATOR's
   heal (`on_land_read`, an asker that is the owner → `Assigned`) had the same shape for a `Leave` heir asking for
   the leaver's land before its order: it now skips land an open run moved to the asker, and everything while a
   declaration is in flight or the open successions are not known (`inherits_or_may`). The boot's blocking read is
   unchanged (it runs after `reclaim_own`: a boot holds exactly what its fence holds).
2. **The player-record double holder** (B-I2), closure (a) + (b): `released_parked` became `parked_written` (per
   pending world cut, under a directory only). (a) `try_complete_cut` notes every record parked here on land that is
   not plainly this host's (`parks_on_foreign_land`: another host's, free, masked for a release; inherited land
   counts as own) that the part writes unchanged; (b) `park_handed` notes a record handed here wrapped in the cut
   pending here (its sender's part writes it). `cut_committed` → `land_released` → `drop_stored_parked`: each noted
   record still parked here unchanged, on land not this host's now, with no spawn order waiting on it
   (`spawn_awaited`), is dropped — the store is its only holder. An abort drops the notes (`release_aborted`).
3. **Pointers at a leaver that released its land** (B-I3): `reroute_held_for_landless_leavers` now runs for every
   landless leaver (not only one with held messages): its `departed` entries are dropped, its `redirected` regions
   answered again (`reanswer_redirected_to`) — not while a player-bearing message is held for it (nothing of that
   host moves).
4. **A landless clean leaver with in-flight rows** (A-F3 = B-I4), the coordinator fix (the controller's ruling):
   (i) `note_clean_rows` (the ticker's `hosts` read): a clean, live, landed, undeclared, unattached row goes into
   `land_wanted` without an ask; (ii) `leave_wanted` drops a wanted owner whose row shows `!landed`, and
   `declare_in` refuses a `Leave` whose predecessor has no `super_regions` row left to move and no `in_flight` row
   (`Declared::Gone`, nothing written); (iii) `LeaveGate` (the process's own, `kept` in `projection!`): a `Leave` is
   declared only once this replica applied the commit of a cut later than the newest cut made when it first read the
   row clean, to an heir among that last committed cut's staying participants. `leave_release` logs the
   landless-with-rows case (WARN). No new input: `note_clean_rows` is part of the unlogged `HostsRead`;
   `LeaveGate::committed` is set where the (logged, replayed) cut decision is applied.
5. **Release epochs** (A-F2 = B-I5): `releasing` is `sr → (cut, epoch)`; `release_epochs_of`; `land_released` frees
   at the released epoch + 1 and keeps a row the directory knows at a newer epoch (no `Released`, not in
   `recently_released`); `HostClaims::shrink` takes `(sr, epoch)` and removes a key only at that epoch; the server's
   settle passes the part's fence epochs (∪ `release_epochs_of` for a part it did not stage). `WorldCut::releases`
   is unchanged. Beyond the list: the keeper's heal compares fence EPOCHS (`unfenced`), so the read path of the
   same race (the row released at `e`, assigned back at `e + 2`, read before the settle) fences `e + 2` first instead
   of handing the manager `Me@e+2` over a fence that then shrinks.
6. **A committed `CutDone` with no part here** (A-S2): with masked releases for that cut it is queued
   (`committed_untracked`) and settled through `cut_decided` as a part is; without, as before.

**Should fix**: (7) wanted directory reads are spaced by `DIRECTORY_READ_RETRY_MS` (the want kept), and
`directory_read` no longer clears `directory_wanted` (a want raised while the read was in flight asks for the next);
(8) `Owns` / `Released` skip `left_peers`, and the re-route keeps everything of a host with a journalled `Received`
entry (`log::error!`, `debug_assert!`); (9) lock notes: the empty-`Leave` read, the safe cycle of `AssignLand` +
a `world`-locking decision in one transition against a registration lapsing its name's rows (A-S3); (10) the stale
docs in code (`thread.rs`, `svc-link`, `placement.rs`, `coordinator/succession.rs`, `coordinator_rt.rs`); (11)
CLAUDE.md ("Leave and drain" rewritten, the stale "claim" wordings, the heal and release sentences), TODO.md (hand
checks 1, 2 and the new 7, the FLAGGED items struck), `twin_check.sh` `sql()` → `$out/sql.err`, the module
variable's description, the two rulings above (C7).

**Rulings (the fixer's; the list did not make them)**

- Eager declaration for EVERY clean, landed, undeclared leaver, not only one holding in-flight rows alone —
  `HostSeen.landed` is one `EXISTS` over both tables, and a leaver that kept `super_regions` rows kept them because
  its part carried in-flight rows or a run was open on it, so nothing is gained by waiting for an ask — cost if
  wrong: a leaver's land moves to an heir nobody needed yet (it is released again when idle); the split is one more
  subquery in `rows.rs` `hosts`.
- The store refuses an empty `Leave` as well as the core (the list asked only for the core's guard) — a `hosts` read
  taken between a leave cut's decision (the row turns clean) and its parts' `COMMIT PREPARED` (the release becomes
  visible) shows the leaver clean AND landed; the core cannot tell — cost if wrong: one refused declaration per stale
  read (`Gone`, retried at the next read).
- `LeaveGate` is process-local, not durable — no core format change after `COORDINATOR_STATE_VERSION` 6; a restart
  or a new ticker waits for the next committed cut — cost if wrong: a `Leave` delayed by one save period after a
  ticker change.
- The heir of a `Leave` must have taken part in the LAST committed cut (stricter than the list's "after a later
  cut"): a host that registered during that later cut has the same stale journal base — cost if wrong: no heir until
  the next cut when every candidate is new.
- A parked record is not dropped while a spawn order for its player is held, taken or pinned here — its session is
  about to be placed from that record — cost if wrong: the record is written once more by this host and dropped at
  the next commit.
- Inherited (masked, not adopted) land counts as this host's for `parks_on_foreign_land` — a record a survivor
  handed the heir is the heir's to place after adoption — cost if wrong: none seen (no cut begins before adoption).
- An untracked committed cut is settled with tick 0 in its `WorldPart` — only an abort would record the tick, and
  the cut is decided — cost if wrong: an abort row with tick 0 for a cut the coordinator called committed (already a
  contradiction).

**Verification**: `just smoke` still fails at `test-audit` on another session's untracked
`crates/game/tests/tmp_dwarf_perf.rs`; its four other steps run by hand with the cap — `game --test it --
rollback_restore:: region_runner::` 4 passed, `server --lib` 3 passed, `server --tests --features test-hooks` and
`client --lib --bins --tests` checked clean. Also: the whole `game` `it` binary 20 passed / 2 ignored, `just test-r4`
(the takeover regression, dev Postgres) 1/1, `world-store --tests` checked. `just web-check` / `determinism-wasm`:
see the report (`final-fix-wave-report.md`). Nothing ran live, on the twin or on orion: every sequence is fixed by
reading — TODO.md's hand check 7.

**Still open**: the "Not in this wave" minors (provisional-load accounting, the load window, `forward_mailbox` hops,
mail keyed by a foreign region, the new-ticker warning, `on_land_owner`'s log, the twin's transcript); a scoped
re-review of this wave; Dawid's hand checks.
