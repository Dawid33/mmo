# Elastic 5 — Move Occupied Land in Place Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `claim` world moves one occupied super-region at a time from a live host (the giver, G) to another (the taker, T) inside an ordinary world cut, so the players standing there see a warning, then a pause of seconds, and keep playing on T at their exact pose without a reconnect; scale-down empties a host this way before it drains it.

**Architecture:**
- **The store:** a cut part that names a successor. `WorldCut::moves` makes G's part set `owner = T, epoch + 1` where a release sets `owner = -1`. The cut decision checks, in its own transaction, that every taker is live, undrained and a participant, and aborts the cut otherwise. A declaration waits while a cut decided as committed still has staged parts.
- **The coordinator:** a move is a durable run in the core, `MoveRuns`, moving from `Warning` to `Cutting` to ended. It orders G and T through mailbox rows, chooses when a cut carries the move (`BeginCut.moves`, `CutDecide.moves`) and abandons it on every failure. Hosts report their load per super-region in the beat row (migration `0022`), so the coordinator names which land moves.
- **The hosts:** at the move's `BeginCut`, G parks S's players while keeping their sessions, stops S's regions and masks S. After the commit G sends each player to T as a journalled `PlayerMove` placed at the record's pose. T fences S only once a store read shows the row as T's, then restores the players and promotes the sessions their clients opened during the warning.
- **The client:** shows the warning, opens a standby connection to T, holds input still during the pause and re-maps S's regions to its new home.

**Tech Stack:** Rust (sqlx on PostgreSQL and SQLite, quinn peer links via `svc-link`, crossbeam), the HTTPS coordinator API (unchanged), Slint (one notice line), bash (`scripts/twin_check.sh`, a probe recipe).

**Spec:**
- `docs/superpowers/plans/2026-11-19-elastic-5-move-occupied-land-planning-handoff.md`: its decisions are re-ruled below where the code disagreed.
- `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md`: decision 8; §5 step 2 "Step B"; §6, the rows "Taker dies during a move in place", "Host dies while draining, during the warning" and "A crowded super-region"; §9 "The warning period".
- `docs/superpowers/plans/2026-11-18-elastic-3-land-assignment.md`, "Decisions settled in planning" 2–4. The release, its lock walk and the "what arrives for released land" table are this plan's templates.
- `docs/superpowers/plans/2026-11-19-elastic-4-reconciler-scaling.md`, "Decisions" 4 (the ticker's rules) and "As built".
- `docs/superpowers/plans/2026-11-19-coordinator-http-api.md`, "As built" and "Final review": the mailbox, attachment by boot, and the ≈ 27 s attached window.

## Before Task 1: the state of the prerequisites (checked 2026-11-20)

1. **Elastic 4's re-review is CLOSED.** The ledger `.superpowers/sdd/2026-11-19-elastic-4-reconciler-scaling/progress.md:98` says: "Fix wave re-review: A-I1, B-I1, C-I1, C-I2, C-I3, C-I4 all ADDRESSED; 0 new Critical / Important … No second wave". Twin run 3 PASSed after it. `TODO.md`'s Elastic 4 "Open" line ("the scoped re-review is pending", ~:309) is stale; its "Twin" line is right. Task 1 Step 1 corrects the stale line.
2. **The coordinator HTTP plan is built and NOT accepted.** Its hand checks (a) two hosts and two replicas with kills, (c) joins / revoke / 503, and (d) the twin are owed (`TODO.md`, "The coordinator as an HTTP service"). **Ask Dawid to run (a), (c) and (d) before E5's first live run** (Task 10's probe). E5's evidence for the hop needs that stack working; a failure there would otherwise be blamed on E5. Writing and building this plan does not wait for them.
3. Elastic 4 Task 9, the world panel, is still gated on `crates/panel` being committed. E5 changes no recipe and no Kubernetes object, so it stays independent.

## Where this plan departs from the handoff (verified against the code at `c473653f`, 2026-11-20)

1. **`in_flight` is NOT re-keyed in E5 (handoff §6.5, §7 steps 1–2).**
   - A row has only a TARGET region, and a part also writes rows aimed at land its host does not own: messages to peers absent from the cut, and items waiting on free land (`peers.rs` `cut_peer_rows`, `unowned_waiting`).
   - Keying the table by target makes two hosts' parts write rows of one super-region. Worse, the owner's DELETE-then-INSERT would drop rows another host wrote, which is a loss. That breaks "never add a table two hosts' parts can both write". The only way through is an insert-only exception for rows aimed at land the writer does not own, which is **Dawid's** call (listed below).
   - **A move does not need the re-key.** G's in-memory copies of rows aimed at S are on its unowned / peer path after the commit and go to T exactly as they go to a release's next owner (Elastic 3's table). G's in-flight rows stay keyed to G.
   - The `Leave` succession stays in any case: `leave_release` also keeps land while a succession run is open on the leaver, or while assigned land is not yet in its fence (`land.rs:341`, cases b and c).
   - Correction: `in_flight.host` was added by `0003_owner_epochs.sql:20`, not `0014`.
2. **The coordinator names the super-region; hosts report load per super-region in the beat (handoff §6.7).**
   - The coordinator knows only `HostSeen.landed: bool` and which regions each host runs. It knows no player positions (`OnlinePlayer` carries no region). Scale-down needs "which of the victim's super-regions hold players", and hotspot relief needs load per super-region.
   - Migration `0022` adds `hosts.land_load`, an encoded `Vec<SrLoad>`, written by the same beat statement as Elastic 3's load columns and read with the ticker's `hosts` read (never logged).
   - **Not** a new `HostMsg`: every `HostMsg` is a logged input and a core transition, while the beat costs nothing new.
3. **A cut carries its moves; there is no separate order to mark them (handoff §6.1, §6.4).**
   - `CoordMsg::BeginCut`, `CutRun` and `CutDecide` gain `moves: Vec<LandMove>`. Every participant learns of a move from its `BeginCut`: G masks and parks, T expects, and peers mask S. These are the same moment and the same mailbox row.
   - Changing `BeginCut`'s fields is a wire break, covered by the protocol bump. "Append-only" is the variant-order rule.
4. **The taker grows its fence ONLY from a store read that shows the row as its own (handoff §5 step 4).**
   - The decision's transition writes the `CutDone` row, and `COMMIT PREPARED` of the parts runs after it as a retried post-commit effect (`coordinator_rt.rs:1873`, `coordinator_tx.rs:382`). So `CutDone` can reach T while the row is still G's.
   - A region load before `COMMIT PREPARED` is refused as fenced (the old epoch is committed, or its `FOR SHARE` meets the prepared lock and fails `40001` under REPEATABLE READ), and that ends the process.
   - So T fences S when it holds an expectation for S from a cut it knows committed, and its directory read shows `(S, T, e')`.
5. **The decision checks the taker; the part does not (handoff §6.1 "must settle").**
   - A prepared transaction must hold no `hosts` row: it would block the taker's beats until `COMMIT PREPARED`.
   - `two_phase::decide_in` takes each taker's `hosts` row `FOR SHARE` after its `world` lock and records an abort unless the taker is live, dirty, undrained, beaten within the lease, listed and not leaving. The walk `coordinator → world → hosts` is a prefix of the declaration's, so it adds no new edge.
6. **A store hole the handoff did not name: a taker that dies between the decision's commit and `COMMIT PREPARED`.**
   - `declare_in` reads `WHERE owner = $taker` against the COMMITTED version, which is still `owner = G`. The moved row would then commit to a dead host no succession covers. (A release has no such case: its successor is "free".)
   - Fix: `declare_in` and `end_landless_in` refuse with a retryable answer (`StoreError::Busy`) while any part of a cut decided as committed is still staged. That part shows in the lockless `pg_prepared_xacts` read beside `cut_decisions`, so the check adds no new lock edge.
7. **G stops S's regions at `BeginCut`; it does not snapshot them (handoff §5 step 3).**
   - Cuts never stop regions today: they snapshot running regions with `SaveAt(T)`. Only a leave stops anything (`park_all_players`, then `shutdown_all` at `BeginCut`, `world_cut.rs:~385`). A region that stops before T counts as parked at T (`save_cut.rs:12`).
   - A snapshot-then-stop would run S past T: its later state never reaches the store, and its post-T departures duplicate entities the cut holds.
   - So G does for S what a leave does for all its land, except that sessions are kept.
8. **The hop's carrier is `PlayerMove`, sent after the commit, placed at the record's pose (handoff §6.3 "must settle").**
   - `Reliable::PlayerMove` is already state-bearing, journalled and handled by every cut, report and re-route path. It gains a field `place: MovePlace { Landing, Pose }`; `accept_move` places a `Pose` move with `restore_player` instead of `CreateClientAt(landing_in(..))`.
   - **Not** an `InCut`-wrapped hand-off before the commit: on an abort T would hold a player whose land stays with G.
   - Momentum and look direction are not in `PlayerRecord`. The client holds input still during the pause (Task 7), so nothing of value is lost.
9. **G must not send `Released` for moved land.** `Directory::set` overwrites on an EQUAL epoch (`ownership.rs:381`), so a late `Released { (S, e+1) }` arriving after T's `Owns { (S, e+1) }` makes peers think S is free. G writes `(Some(T), e+1)` into its own directory instead, and peers learn the new owner from T's `Owns` or a directory read.
10. **The coordinator needs no "the taker asked for S" check.** `Assigned` and `LandIs` are answered from the owners read, which reads committed rows: a taker that asks after the commit sees `(S, T, e+1)` only once `COMMIT PREPARED` ran, and `Assigned` fences at that epoch. T does not ask for S while it expects it (Task 5). **Verify** that `CoordMsg::Assigned` carries the read's epoch before relying on this; if it does not, add the mirror of `succession_moved_to` to `on_land_read`.
11. **A dev / twin trigger is needed (handoff §6.7 "an operator's move is out unless a hand check needs one").** The twin has no connected player and `dev-r3` has no fleet, so neither can reach rule 6 with players. Two coordinator dials make that possible:
    - `VOXEL_MOVE_PROBE_MS` (unset = off; refused together with `VOXEL_FLEET_MAX`): with no move open, every that many ms the ticker moves the most occupied super-region of the most loaded host to the best taker.
    - `VOXEL_MOVE_WARNING_SECS`: the warning, so a run takes seconds.

    They are what Task 10's twin phase and probe use. They are not an operator interface.

## Decisions settled in planning

### 1. Types (game crate, shared by every task)

```rust
// crates/game/src/ownership.rs (beside HostLoad)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MoveId(pub u64);

/// One move carried by a world cut: `sr` passes from `giver` to `taker` (its boot when chosen).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandMove { pub id: MoveId, pub sr: SuperRegion, pub giver: HostId, pub taker: HostId, pub taker_boot: u64 }

/// A super-region's load in a host's beat row (`hosts.land_load`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SrLoad { pub sr: SuperRegion, pub players: u32, pub regions: u32, pub tick_permille: u32 }
```

- `WorldCut::moves: Vec<(SuperRegion, HostId)>` (`save_cut.rs`). The epoch comes from the part's fence, as for `releases`.
- `CutDecide::moves: Vec<LandMove>`, `CutRun::moves`, `CoordMsg::BeginCut { …, moves: Vec<LandMove> }`.
- New `CoordMsg` variants, appended in this order:
  - `MoveLand { id: MoveId, sr: SuperRegion, taker: HostId, at_tick: Tick }` (to G);
  - `ExpectLand { id: MoveId, sr: SuperRegion, giver: HostId, at_tick: Tick }` (to T);
  - `MoveOff { id: MoveId, sr: SuperRegion }` (to both).

  None of them is in `core_less_kind`: each is decided together with a `MoveRuns` change.
- `CutFailure` gains `MoveRefused` (appended): G's part cannot carry a move the cut names (S not owned at the fence's epoch, T not a participant, a spawn pin or held order on S).
- `Reliable::PlayerMove { handoff, record, target, place: MovePlace }`, with `enum MovePlace { Landing, Pose }`. Every existing sender uses `Landing`.
- `ServerPacket::LandMoving { super_region: SuperRegion, at_tick: Option<u64>, standby: Option<(HostEndpoint, ObserverTicket)> }`, appended after `HostDraining`. `at_tick: None` calls the move off.
- Versions: `LINK_PROTOCOL_VERSION` 12 → **13** and `COORDINATOR_STATE_VERSION` 8 → **9**, each bumped once, in Task 3. Migration **`0022_host_land_load`** in both engines; recheck the next free number at Task 2.

### 2. The coordinator's run (`crates/game/src/coordinator/moves.rs`, new)

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
enum MoveState { Warning, Cutting { cut: CutId, tries: u32 } }

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MoveRun {
    mv: LandMove,             // id, sr, giver, taker, taker_boot
    giver_boot: u64,
    why: MoveWhy,             // ScaleDown | Hotspot | Probe
    state: MoveState,
    at_tick: Tick,            // the warning's end; the cut's tick is max(at_tick, world_tick + lead)
    deadline_ms: u64,         // store clock: at_tick's time + MOVE_DEADLINE_MS; rebased at load, in clock_floor
    opened_at: u64,           // row version that opened it
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct MoveRuns {
    next: u64,
    open: Option<MoveRun>,                          // ONE move per world
    evacuating: BTreeMap<HostId, (u64 /*boot*/, String /*desired name*/, u64 /*since ms*/)>,
    abandoned: BTreeMap<SuperRegion, (u32, u64 /*retry after ms*/)>,
    evac_abandoned: BTreeMap<HostId, u32>,
}
```

- `moves: MoveRuns` is durable in `projection!` and named in `Coordinator::new_for`'s literal. Its deadline joins `rebase_deadlines` and `clock_floor`, as `successions` does (`input.rs:340`, `clock.rs:57`).
- **Open** (ticker only, one transition carrying the core):
  - Preconditions: no open move, no succession run open / being declared / rolling back, `cuts` not blocked, S not in `abandoned` before its retry time.
  - It pushes `MoveLand` to G and `ExpectLand` to T, both as mailbox rows of that transition, with `at_tick = world_tick + warning_secs·1000 / TICK_RATE`.
- **Cut due** (`move_due` beside `settle_due` in `maintain_cuts`): `Warning` and `world_tick + lead ≥ at_tick`. The cut that begins carries `moves = [mv]`, and the run becomes `Cutting { cut, tries + 1 }`.
- **Ends**, in each case removing the run in the transition that learns of it:
  - **Committed:** the cut's stored decision is committed with the move listed. The run is removed, and nothing more is sent: `CutDone` tells G and T.
  - **Retry:** the cut is aborted and `tries < MOVE_CUT_TRIES` and now < `deadline_ms` → back to `Warning` with `at_tick` = now (due at the next pass).
  - **Abandon:** any of the following → `MoveOff` to G and T (each attached), `abandoned[sr]` counted with retry after `MOVE_RETRY_AFTER_MS`, and the reason on the `[coordinator] window=` line:
    - the cut aborted with `tries` exhausted or past `deadline_ms`;
    - G or T detached, succeeded, ended or lapsed;
    - a succession declared;
    - a rollback (`rolled_back_to_generation`);
    - `first_of_generation`;
    - the deadline passed in `Warning`.
- **On an `Attach`** (in `on_host`'s Attach branch, after `cut_attach`), when the run names the attaching host at the boot recorded in the run: re-send its order (`MoveLand` or `ExpectLand`). The `Attach`'s own transition drops older rows before its inserts, so the re-sent row survives (as `succession_attach`'s does).
  - Another boot of G or T means a new process: abandon.
  - In `Cutting`, the participant's re-attach has already aborted the cut (`cut_attach`, `world_cut.rs:765`), so the run takes the retry branch.
- **The giver stops taking logins:** `takes_logins` (`mod.rs:1291`) is false for an open move's giver and for every `evacuating` host. That removes it at once as gate, as assignee and as a "takes logins" heir.
- **Taker choice** (`pick_taker`, in `moves.rs`):
  - Candidates: `pick_assignee`'s candidate set minus the giver and every `evacuating` or draining host, with its two narrows as HARD filters: `known_for ≥ ASSIGN_MIN_ATTACHED_MS`, and `tick_permille ≤ 1000 − ASSIGN_HEADROOM_MIN_PERMILLE`. The lease row must have moved in the ticker's last read.
  - Key: `(tick_permille, players, regions)` ascending, then `rendezvous_among(sr, tied)`. No candidate means no move.

### 3. The store (Task 1)

**The part.** `release_refusal` becomes `land_refusal(cut, fence, end)`. It answers `Unsupported` before any statement for:
- a release or a move in a one-phase cut (`CutEnd::Commit`: static, all-in-one, SQLite);
- a move whose super-region is not in `fence.epochs`, is also in `releases`, or is listed twice;
- a move whose taker is the part's own host.

The statement order is Elastic 3's, with moves beside releases:
1. `SET LOCAL lock_timeout`; the generation read.
2. `check_fence(releases ∪ moves, RELEASE_LOCK)`: one key-ordered pass, `FOR NO KEY UPDATE`.
3. `check_fence(commit_check_set, FOR SHARE)`.
4. Every data write, unchanged (chunks with `top`, entities, players, `in_flight` — still keyed to G).
5. Per release, `UPDATE … SET owner = -1, epoch = epoch + 1 …`. **Per move**, `UPDATE super_regions SET owner = $taker, epoch = epoch + 1 WHERE sx = $1 AND sz = $2 AND owner = $me AND epoch = $e`. `rows_affected != 1` → `Fenced`.
6. `PREPARE TRANSACTION`.

It locks no `hosts` row. The safe cycle with a declaration of G is the release's: `40P01` makes the part `Fenced`, which is true.

**The decision** (`decide_in`, commit branch, after the `world` lock and the staged-vs-listed check): for each `CutDecide.moves` taker in id order,

```sql
SELECT drain_at_ms, <lease fresh> FROM hosts
 WHERE id = $t AND boot = $b AND state = LIVE AND clean = 0 FOR SHARE
```

It records an abort (`commit = false`, logged with the reason) on any of these:
- no row, a drain mark, or a stale lease;
- `taker ∉ hosts` (the listed participants);
- `taker ∈ leaving`;
- `giver ∉ hosts`.

An abort rolls back every part, so the land stays with G.

**The declaration's wait.** `declare_in` and `end_landless_in` first read `staged_parts` for cuts whose `cut_decisions` row says committed (lockless). Any found → `StoreError::Busy`, which the coordinator treats as "try again after `COORDINATOR_SAVE_RETRY_MS`", exactly as a lock timeout. A start's `resolve_staged_cuts` finishes those parts, so a total-crash registration needs no change (it runs after `resolve_before_claim`).

**Lock walks**, written into `world-store/src/succession.rs`'s module notes:
- (a) **The part's move:** `super_regions` (releases ∪ moves, NO KEY UPDATE, key order) → `super_regions` (check set, SHARE) → data → `in_flight` → the moved rows' UPDATE. Same as a release.
- (b) **The decision:** `coordinator` → `world` (NO KEY UPDATE) → `hosts` (takers, SHARE, id order) → `cut_decisions`, `cut_begins`, `hosts` (leaving, via UPDATE), `successions`.
  - It is a prefix of the declaration's walk; a registration serializes on `world`.
  - A beat, a drain mark or a vouch waits (one row each, wanting nothing after).
  - A re-claim's share lock does not conflict.
  - A staged part locks no `hosts` row.
- (c) **The declaration's staged read** takes no lock.

### 4. The host (Tasks 4 and 5): who holds what, when

**`MoveLand` received (on G):**
- `moving_out[sr] = MoveOut { id, taker, at_tick, state: Warned }`.
- Each home session whose body is in S (now, or when it walks in later) gets `LandMoving { sr, at_tick, standby }`. `standby` holds T's endpoint and a ticket from `observer_ticket(client, T, now)` (an `ObserverOrder` goes to T).
- G re-sends with a fresh ticket while the ticket is more than half its TTL old (the existing re-mint rule). A body that walks off S gets `LandMoving { at_tick: None }`.
- From `at_tick − SPAWN_PIN_MS` G answers a placement onto S with `Placement::Wait`; the client's retry lands after the move.

**`ExpectLand` received (on T):**
- `expecting[sr] = MoveIn { id, giver, cut: None }`.
- T does not ask the coordinator for S (`ask_for_land` skips it).
- `close_idle_observers` spares an observer session whose client holds a standby ticket for an expected move until `at_tick + MOVE_DEADLINE_MS / TICK_RATE`.

**`BeginCut` with `moves`:**
- **On G**, for each move it gives:
  - It must hold the matching `moving_out` (id), own S at the fence's epoch, find T among the cut's participants, and find no spawn pin, `spawn_here` or held order targeting S. Otherwise it fails the part (`CutFailure::MoveRefused`).
  - Otherwise it records `moving_out[sr].state = Masked { cut, epoch }` and masks S (`owner_of_sr` answers `Unknown`, as `releasing` does).
  - Every session whose body is in S is parked KEEPING its session: a new park reason; `parked_players` holds the record; the session, `player_of` and `homes` are kept.
  - S's regions are shut down. Their `Stopped` answers count as parked at T.
  - The part carries `moves`.
- **On T:** `expecting[sr].cut = Some(cut)`.
- **On every other participant:** `peer_moving[sr] = (cut, epoch_known)`, a mask that makes `owner_of_sr` answer `Unknown` (players clamp at the seam, items wait on the unowned path).

**`CutDone { cut, committed: true }`:**
- **On G**, in `cut_committed`, before `drop_stored_parked`:
  - The server shrinks the fence by the moved rows at their masked epochs (`release_epochs_of` grows a `moved` half).
  - `land_moved(cut)` sets the directory to `(Some(T), e+1)`, drops `parked` blobs / terrain caches / idle stamp for S, and lifts the mask. It sends NO `Released`.
  - It TAKES each kept session's record out of `parked_players`, sets `departed[p] = T`, turns the session into an observer, and sends `PlayerMove { place: Pose, handoff (hop + 1, login kept), record, target }` to T.
  - It forwards to T every hand-off it held for S while masked (below).
  - It runs `flush_unowned`.
- **On T:** `expecting[sr]` becomes `Committed { cut }`, and the keeper is asked for a directory read (`directory_wanted`).
- **On the others:** nothing yet. `peer_moving[sr]` lifts when the directory shows S at an epoch above `epoch_known` (T's `Owns` or a read).

**`CutDone { committed: false }`, a failed `begin_cut_at`, or `cut_failed`:**
- **On G:** the mask lifts, and each kept session's player is restored at its record's pose (`restore_player`). Held hand-offs are delivered here. `moving_out[sr].state` goes back to `Warned` (the run retries or sends `MoveOff`).
- **On T:** `expecting[sr].cut = None`.
- **On the others:** `peer_moving[sr]` lifts.

**`MoveOff`:**
- **On G:** `moving_out` is removed, and sessions in S get `LandMoving { at_tick: None }`.
- **On T:** `expecting` is removed.

**`Attached { epoch }`** (a re-attach): G and T drop `moving_out` / `expecting` entries that are `Warned` / uncut. The re-sent orders in the same transition re-create them. A masked or committed entry follows its cut's outcome (`pending_cut` is answered at attach).

**The taker's fence** (`server/src/claims.rs` `DirectoryKeeper`):
- A row `(S, T, e')` in a directory read, while the manager's `expecting[S]` is `Committed`, is fenced (`HostClaims::add`) and then `claimed`. This applies **even while a succession is open**: the expectation came from the mailbox in commit order, so it is not a declaration's moved row.
- A read that still shows G is retried after `DIRECTORY_READ_RETRY_MS`.

**On T after `claimed(S)`:**
- `expecting` is removed.
- Held `PlayerMove`s and hand-offs into S are delivered:
  - A `Pose` move restores the record (`restore_player`).
  - The session is promoted. Its client's standby observer session already exists, so it gets `HomeHop` plus `PlayerRegion` at once. With none, `awaiting_session` for `HANDOFF_SESSION_GRACE_MS`, then the existing park, a rejoin, counted in `mmo_move_hop_parked_total`.
  - T sends `RegionElsewhere` naming itself for each of S's regions that client has subscribed, so the client re-maps them.
- Other homes re-answer `redirected` entries pointing at G for S's regions (`reanswer_redirected_to`, filtered to S) when their directory learns T.

**Hand-offs into S during G's mask:**
- **G** holds `PlayerHandoff` transfers and `PlayerMove`s that arrive for S in `move_held[sr]` instead of `park_handed`. G's part writes each held player's record, by `park_handed`'s `parked_written` rule. They are forwarded to T at commit or delivered locally at abort.
- **T** holds what arrives for S before `claimed` in `move_held[sr]`, then delivers it. If T never claims within `MOVE_HOP_WAIT_MS` of the first held player, the player is parked (the existing reconnect).

**New `WorldManager` fields:** `moving_out`, `expecting`, `peer_moving` and `move_held`. Each is classified in `host_fields!`:
- `moving_out` (the taker in a struct value) and `expecting` (the giver): `other`, with the written reason "a move names a live peer only while its run is open; `forget_host` of either ends the run by `MoveOff` or a cut abort".
- `move_held`: `value_sets` if it is keyed per sender, else `other` with the same reason.
- `peer_moving`: `other` (no host).

### 5. Conservation of a moved player (the proof each implementer re-checks against the code before building on it)

1. **Until G's park** (at `BeginCut`, before the cut's tick): the body is in S's region on G, and the region's `portable.players` hold it.
2. **The park:** `PlayerParked` → `parked_players[p]` + `cut_transit`. G is the only holder, and S's stopped regions hold no player.
3. **The part:** cut K's part writes `players[p]` and moves S's row, in one prepared transaction.
   - Abort → G alone holds `p`, and restores it at its pose.
   - Commit → the store holds `p` on T's land.
4. **After the commit:** G takes `parked_players[p]` before `drop_stored_parked` (which would otherwise drop it as "on foreign land") and sends `PlayerMove` to T. From here the message is the holder:
   - It is journalled `Sent` / `Received` with `Subject::Player` at the hop.
   - It is sent bare after K. In K+1 it belongs to T's part if received before G's mark, else to G's (`cut_peer_players`, `in_transit`).
5. **On T:** held until `claimed(S)`, then `restore_player` → `creating` + `cut_transit` → the body is in T's region.

**Crashes:**
- **G dies before sending:** the record is in the store on T's land, and no journal entry names it. T (or the gate) places it from the store at the client's rejoin, after every peer synced: "wait, never guess".
- **G dies after sending:** T's received copy holds the player (`holds_player` → the login places it Here).
- **T dies after `COMMIT PREPARED`:** its land, S included, goes to its heir. G's journal report against T names the `PlayerMove`, which is sent again to the heir by the existing report resolution.
- **T dies between the decision and `COMMIT PREPARED`:** the declaration waits (decision 3), then it is the case above.

Spawn pins and held orders on S refuse the part (`MoveRefused`), so no login places a second copy during the move. `departed[p] = T` on G chains other hosts' pointers by one hop.

### 6. Windows in which a party may be dead, detached or stalled while the run still counts it

For the whole-branch review's timing seam. Named consts: `HOST_LEASE_MS` 10 s, `DAY_MS` 10 s, `MAILBOX_STALL_MS` 15 s, `WORLD_CUT_DEADLINE_MS` 30 s, `REPLICA_LEASE_MS` 10 s, `MOVE_DEADLINE_MS` 120 s, `MOVE_HOP_WAIT_MS` 30 s.

| Run state | Who may be gone | Worst case before noticed | What ends it |
|---|---|---|---|
| `Warning` | G dead | lease + one read ≈ 11 s | G's declaration / end → abandon (`MoveOff` to T) |
| `Warning` | G or T cut off, still beating | `DAY_MS` + `MAILBOX_STALL_MS` + 1–2 reads ≈ 27 s | `HostStalled` → detach → abandon |
| `Warning` | T dead | ≈ 11 s | T's declaration / end → abandon; G's sessions get `at_tick: None` |
| `Warning` | the ticker replica | `REPLICA_LEASE_MS` + one read | a new ticker resumes the durable run; a deadline older than `CALENDAR_GAP_MS` restarts its count |
| `Warning`, cuts blocked (expected ≠ attached, a run open) | — | until `deadline_ms` (`at_tick` + 120 s) | abandon by deadline |
| `Cutting` | any participant | `WORLD_CUT_DEADLINE_MS` 30 s | the cut aborts → retry (`MOVE_CUT_TRIES` 3) or abandon |
| decided committed, before `COMMIT PREPARED` | T dies | the `FinishCut` retry (normally ms) | the declaration waits (`Busy`), then a crash succession of T |
| committed, T not yet fenced | T alive but its read keeps failing | `DIRECTORY_READ_RETRY_MS` per try | T parks held players after `MOVE_HOP_WAIT_MS` (a reconnect) |
| committed | G dies before sending the `PlayerMove`s | lease | clients rejoin; T places them from the store |
| peers' `peer_moving` mask | the `CutDone` row dropped by a re-attach | until the attach | `pending_cut` answered at the attach lifts or confirms it; T's `Owns` raises the epoch |
| standby connection | the move abandoned while G is dead | `at_tick + MOVE_DEADLINE_MS` | T's idle exemption ends and the client's standby closes on its own deadline |

**States a world can reach that need an explicit end** (E4's lesson):
- An evacuating host that never reaches 0 players (players walk in from neighbours) → `EVACUATE_MAX_MS` (30 min) after `since`, then the warned drain.
- A super-region abandoned again and again → `abandoned` holds it out for `MOVE_RETRY_AFTER_MS`; an evacuation stops moving after `MOVE_ABANDON_MAX` abandons and falls back to the warned drain.
- An expectation on T whose `MoveOff` was dropped → cleared at the next `Attached` unless re-sent.
- A held hand-off on T → `MOVE_HOP_WAIT_MS`.
- A `Cutting` run whose cut was never begun because the ticker changed → the new ticker's `maintain_cuts` sees the run without a running cut, sets it back to `Warning` and retries.

### 7. Scale-down and hotspot relief (Tasks 8 and 9)

- **Rule 6, with players** (`fleet.rs:328-358`): when the victim reports players > 0, instead of a 300 s `DrainHost`:
  - `evacuating.insert(victim)`, in a transition with a core, so not core-less;
  - the drain decision is NOT issued.
- **Rule 6b** (new, ahead of rules 4–7, no scale gates), for an evacuating host:
  - its live row's `land_load` shows no super-region with players → `DrainHost { in_ms: 0 }`, and it leaves `evacuating`;
  - else, with no move open → open a move of its super-region with the most players to `pick_taker`;
  - no taker, `evac_abandoned ≥ MOVE_ABANDON_MAX`, or `EVACUATE_MAX_MS` passed → `DrainHost { in_ms: DRAIN_WARNING_SECS·1000 }` (the warned drain, today's behaviour), and it leaves `evacuating`.
- An evacuating host that is ended or succeeded leaves the set.
- The scale gates of rules 5–7 also need "no move open and no host evacuating".
- **Rule 9, hotspot (Task 9, droppable):** for `HOTSPOT_AFTER_MS`, the most loaded serving host H has `tick_permille ≥ SCALE_UP_LOAD_PERMILLE`, and `pick_taker` finds a host at least `HOTSPOT_GAP_PERMILLE` below it. Then move H's super-region with the highest `SrLoad.tick_permille`, provided H holds more than one super-region with running regions (moving its only one relieves nothing).

### 8. Tunables (named consts; env dials only where a test or dev run needs them)

| Constant | Default | Where | Why |
|---|---|---|---|
| `MOVE_WARNING_SECS` | `DRAIN_WARNING_SECS` (300) | `ownership.rs`; dial `VOXEL_MOVE_WARNING_SECS` | design §5; **(Dawid)** |
| `MOVE_DEADLINE_MS` | 120 000 | `ownership.rs` | four cut deadlines past `at_tick`; then abandon |
| `MOVE_CUT_TRIES` | 3 | `coordinator/moves.rs` | a missed lead or a transient abort retries at once |
| `MOVE_RETRY_AFTER_MS` | 600 000 | `moves.rs` | the same land is not warned again for 10 min |
| `MOVE_ABANDON_MAX` | 2 | `moves.rs` | per evacuation; then the warned drain |
| `EVACUATE_MAX_MS` | 1 800 000 | `moves.rs` | an evacuation always ends |
| `MOVE_HOP_WAIT_MS` | 30 000 | `world_manager` | T parks a held player it cannot place |
| `MOVE_STANDBY_LEAD_SECS` | 60 | `game` (client reads it) | the client opens the standby connection this long before `at_tick` |
| `HOTSPOT_GAP_PERMILLE` | 300 | `moves.rs` | giver's minus taker's tick load |
| `HOTSPOT_AFTER_MS` | 300 000 | `moves.rs` | how long the gap must hold |
| `VOXEL_MOVE_PROBE_MS` | unset | coordinator env | dev / twin trigger (departure 11) |

## To confirm with Dawid (the plan proceeds on these defaults)

1. **`in_flight` re-key: out of E5** (departure 1). Re-keying needs an exception to "a cut part writes only rows of its own regions" (insert-only rows into land the writer does not own, deleted only by that land's owner). Yes / no, and if yes, when (its own plan, after E5).
2. **The warning:** `DRAIN_WARNING_SECS` (300 s) for scale-down. During it the player may walk off the land, and then nothing happens to them.
3. **Hotspot relief in or out** (Task 9, last; droppable without touching the rest).
4. **The live probe for the hop** (Task 10, `just move-probe` on `dev-r3`): each run asked for, after the coordinator HTTP plan's hand checks (a), (c) and (d).
5. **The pause's bound:** measured by that probe; no bound is coded before it.

**Recommended and adopted for execution (2026-11-20, native execution; Dawid may overturn any):**
1. `in_flight` re-key: **out of E5**, its own plan after E5 if ever — the move does not need it, and the insert-only exception weakens the one rule that keeps cut parts disjoint.
2. The warning: **`MOVE_WARNING_SECS` = 60**, not 300. A move costs the player a pause of seconds, not a reconnect, so the drain's five minutes buys nothing; at 300 s an evacuation of N occupied super-regions takes N × 5 min and holds every scale gate shut that long. 60 s also equals `MOVE_STANDBY_LEAD_SECS`, so the standby connection opens with the notice. The dial `VOXEL_MOVE_WARNING_SECS` stays.
3. Hotspot relief: **out (Task 9 not built)**. No measured pause yet, no evidence a hotspot happens, and a move it opens shuts the scale gates; build it once the probe has measured the pause.
4. The live probe: **written, not run** (Task 10 Step 3); owed after the coordinator HTTP plan's hand checks (a), (c), (d).
5. The pause's bound: **none coded**; the probe measures it.

## Global Constraints

- Only regression tests and gates are committed (`CLAUDE.md`, Testing). Every task's tests are TEMPORARY: written to drive or check the change, run, then deleted before the commit. No step commits a test.
- Wire enums are append-only (variant order). `LINK_PROTOCOL_VERSION` 12 → 13 and `COORDINATOR_STATE_VERSION` 8 → 9 exactly once (Task 3). Migrations are immutable and identical in shape on pg and sqlite. Worlds are re-forged, never migrated.
- Every new core field goes in `projection!`, every new `WorldManager` field in `host_fields!`, every new input is classified (Elastic 2b decision 3 table), and every new or changed state-bearing `Reliable` is journalled or argued out.
- **Fence before use; shrink before the next part.** The store's fencing is the only safety. Never add a store write that skips the fence. Lock walks go into `world-store/src/succession.rs`'s notes in the task that adds the statement.
- **Never insert an `outbox` or `login_outbox` row outside a transition.** Move orders are rows of the transition that decided them.
- `static` and `allinone` never move land. A one-phase cut refuses `moves`.
- No Bevy in `game`, `server` or `worldgen`. Use `libm`, `BTreeMap` and named consts in sim paths.
- Verification per task: the touched code's temporary tests (`just test-one …`), `just smoke`, `just web-check` when `game` or the client's connection code changes. `just determinism-wasm` only if something hashed in the sim changes (nothing planned does).
- **Ask Dawid first:** client tests (`just test-one client …`), live runs (`dev-r3`, `go-nogo-r3`, `just run`, the probe), measurements, anything on orion. `just twin-check` is allowed.
- Never `rustfmt` / `cargo fmt` from the root. **Commit only your own hunks.** A file that already shows ` M` is staged by a filtered patch (`git diff -U3 <file>`, keep yours, `git apply --cached --recount`), checked with `git diff --cached`, and committed from the index with NO pathspec. Never amend. Other sessions commit on `develop` during the run: build review packages from this plan's commit list, not a range.
- Reports and reviews go to files under `.superpowers/sdd/2026-11-20-elastic-5-move-occupied-land/`; messages carry a short verdict.

## Review Focus

1. **A player who walks onto S during the last seconds of the warning** (after the giver's `LandMoving` round, before the park): they must get the notice and a standby ticket on entry, and be moved at their pose, not parked. Task 4 sends `LandMoving` on entry into S; Task 5's check walks one in at `at_tick − 1 s`.
2. **A player whose client has no standby connection to T** (opened late, or failed to connect): one reconnect at most, never a lost or doubled player. Task 5's check runs the hop with no observer session on T and asserts one park, one rejoin and one copy.
3. **A player standing exactly on S's edge, next to land of a third host P:** P's hand-off racing G's mask must be held and forwarded, never parked. Task 4's check fires a P→G hand-off between G's mask and P's `BeginCut`.
4. **A login for a player whose record is on S during the warning's last `SPAWN_PIN_MS`:** it must wait and land on T after the move, never be pinned to G and refused `PLAYER_ELSEWHERE`. Task 4's check logs in at `at_tick − 10 s`.
5. **The move abandoned after the clients were warned** (taker died): every warned client's notice clears (`at_tick: None`), the standby connection closes, and the player keeps playing on G. Task 7 (client) and Task 3's check (abandon → `MoveOff`).

---

## File structure

| File | Responsibility | Task |
|---|---|---|
| `crates/world-store/migrations/{pg,sqlite}/0022_host_land_load.sql` | `hosts.land_load` | 2 |
| `crates/world-store/src/rows.rs` | the part's moves; the beat writes `land_load`; the `hosts` read returns it | 1, 2 |
| `crates/world-store/src/two_phase.rs` | `decide_in`'s taker check; the staged-committed read | 1 |
| `crates/world-store/src/succession.rs` | `declare_in` / `end_landless_in` wait; lock-walk notes | 1 |
| `crates/game/src/save_cut.rs` | `WorldCut::moves` | 1 |
| `crates/game/src/ownership.rs` | `MoveId`, `LandMove`, `SrLoad`, move consts | 1, 2 |
| `crates/game/src/coordinator/moves.rs` (new) | `MoveRuns`, open / due / end / attach / `pick_taker` | 3 |
| `crates/game/src/coordinator/{mod,world_cut,input,state,msg}.rs` | `projection!`, `BeginCut` / `CutRun` / `CutDecide` moves, `takes_logins`, versions, new `CoordMsg` | 3 |
| `crates/game/src/coordinator/fleet.rs` | rule 6 evacuation, rule 6b, gates | 8 |
| `crates/game/src/world_manager/moves.rs` (new) | giver and taker host state; `land_moved`; holds | 4, 5 |
| `crates/game/src/world_manager/{world_cut,land,peers,placement,succession,mod}.rs` | hooks: `BeginCut`, park keeping the session, `drop_stored_parked` ordering, `accept_move` at pose, `host_fields!`, idle-observer exemption | 4, 5 |
| `crates/game/src/peer/msg.rs`, `peer/journal.rs` | `PlayerMove.place` | 4 |
| `crates/server/src/{claims,cuts,load}.rs` | taker's fence rule; giver's shrink; per-super-region `TickLoad` | 2, 4, 5 |
| `crates/svc-link/src/lib.rs` | `LINK_PROTOCOL_VERSION` 13 | 3 |
| `crates/game/src/protocol.rs` | `ServerPacket::LandMoving` | 4 |
| `crates/client/src/{connections,instance}.rs`, `renderer/drain_notice.rs`, `renderer/autopilot.rs` | standby, notice, input hold, re-map, `stand` autopilot | 7, 10 |
| `crates/server/src/{lib,roles}.rs`, coordinator dials | `VOXEL_MOVE_PROBE_MS`, `VOXEL_MOVE_WARNING_SECS` | 3, 10 |
| `scripts/twin_check.sh`, `justfile` | twin phase 14, `move-probe` recipe | 10 |
| `deploy/modules/monitoring/…` | alerts | 10 |
| `CLAUDE.md`, `TODO.md` | docs | 10 |

---

### Task 1: The store — a part names a successor; the decision checks the taker; a declaration waits for a committed cut's staged parts

**Files:**
- Modify: `crates/game/src/save_cut.rs` (`WorldCut::moves`), `crates/game/src/ownership.rs` (`MoveId`, `LandMove`), `crates/game/src/coordinator/world_cut.rs` (`CutDecide::moves` only; filled by Task 3)
- Modify: `crates/world-store/src/rows.rs` (`land_refusal`, the lock pass, the move loop)
- Modify: `crates/world-store/src/two_phase.rs` (`decide_in`, `staged_committed`)
- Modify: `crates/world-store/src/succession.rs` (`declare_in`, `end_landless_in`, module notes), `crates/world-store/src/lib.rs` (`StoreError::Busy`, if absent)
- Modify: `TODO.md` (the stale Elastic 4 "Open" line only)

**Interfaces:**
- Produces:
  - `WorldCut::moves: Vec<(SuperRegion, HostId)>`.
  - `CutDecide::moves: Vec<LandMove>`.
  - `StoreError::Busy`: retryable, mapped by the coordinator like a lock timeout.
  - `two_phase::staged_committed(tx, world_tag) -> Result<bool, StoreError>`.

- [ ] **Step 1: Correct `TODO.md`'s Elastic 4 "Open" line.** It should say the scoped re-review is closed (ledger :98), matching the "Twin" line.
- [ ] **Step 2: Verify decision 3 against the code before building on it.** Read `cut_rows`, `release_refusal`, `decide_in`, `declare_in` and `end_landless_in`. If any claim there fails (the statement order, the absence of a `hosts` lock in the part, the committed-version read in `declare_in`), stop and report.
- [ ] **Step 3: Implement the part's moves** as decision 3: `land_refusal` (rename of `release_refusal`; every refusal listed), one lock pass over `releases ∪ moves`, the per-move UPDATE after the release loop. `rows_affected != 1` → `Fenced`; `40001` / `40P01` → `Fenced` (`fence_race`).
- [ ] **Step 4: Implement the decision's taker check** in `decide_in`'s commit branch, exactly as decision 3, with each abort reason logged at `warn` (`cut {cut}: move {id} aborted: taker {t} {why}`).
- [ ] **Step 5: Implement the declaration's wait.** `staged_committed` is a lockless `pg_prepared_xacts` read joined in Rust to `cut_decisions.committed`. `declare_in` and `end_landless_in` call it first and answer `Busy`. SQLite: always false.
- [ ] **Step 6: Write the three lock walks** (a), (b) and (c) into `succession.rs`'s module notes.
- [ ] **Step 7: Temporary tests against the dev Postgres** (in-crate `#[cfg(test)]`, `MMO_TEST_DATABASE_URL`; deleted before the commit):
  - `move_part_sets_taker_at_next_epoch`: a prepared part with `moves = [(S, T)]`, then `COMMIT PREPARED` → the row is `(T, e+1)`; `ROLLBACK PREPARED` → `(G, e)`.
  - `move_refused_in_one_phase` (→ `Unsupported`), and `move_not_in_fence` / `move_and_release_same_sr` / `move_to_self` (→ `Unsupported`, no statement run).
  - `decide_aborts_for_drained_taker`, `…_dead_taker`, `…_unlisted_taker`, `…_leaving_taker` → stored decision `committed = false`.
  - `declare_waits_for_committed_staged_part`: a decision stored committed with one part still prepared → `declare_in` of the taker answers `Busy`; after `COMMIT PREPARED` it declares and moves S.

  Run: `just test-one world-store "--lib move_ decide_aborts declare_waits"` (with `MMO_TEST_DATABASE_URL` set). Expected: all pass.
- [ ] **Step 8:** `just smoke`. Expected: PASS within 60 s.
- [ ] **Step 9: Delete the temporary tests; commit** (own hunks only):
  ```bash
  git commit -m "feat(store): a cut part names its land's successor; the decision checks the taker; a declaration waits for a committed cut's staged parts"
  ```

### Task 2: Load per super-region in the beat (migration `0022`)

**Files:**
- Create: `crates/world-store/migrations/pg/0022_host_land_load.sql`, `crates/world-store/migrations/sqlite/0022_host_land_load.sql` (`ALTER TABLE hosts ADD COLUMN land_load BYTEA` / `BLOB`, nullable)
- Modify: `crates/world-store/src/rows.rs` (the beat statement `:180`, the `hosts` read `:784`), `crates/world-store/src/handle.rs` (`HostRow.land_load: Vec<SrLoad>`)
- Modify: `crates/game/src/ownership.rs` (`SrLoad`, `HostSeen.land_load`), `crates/game/src/coordinator/succession.rs` (the `HostSeen` conversion)
- Modify: `crates/server/src/load.rs` (a per-super-region tick maximum), the beat's caller, `crates/game/src/world_manager/` (players and regions per super-region)

**Interfaces:**
- Consumes: `SrLoad` (decision 1).
- Produces:
  - `HostSeen.land_load: Vec<SrLoad>`, from the ticker's `hosts` read, never logged.
  - `WorldManager::land_load() -> Vec<SrLoad>`: owned super-regions with a running region or a home session.

- [ ] **Step 1: Re-check the next free migration number** (`ls crates/world-store/migrations/{pg,sqlite}`). Use it if `0022` is taken, and say so in the report.
- [ ] **Step 2: Implement.**
  - `TickLoad` becomes keyed by super-region (`BTreeMap<SuperRegion, AtomicU64>` behind the existing handle; each region thread notes into its super-region's slot; the host-wide value = the max over them, so `load_tick_permille` is unchanged).
  - The beat encodes `land_load` with `game::encode_packet`. A boot beat writes `NULL`, and a `NULL` reads as empty.
  - Bound: at most `LAND_LOAD_MAX` (256) entries, the most loaded first.
- [ ] **Step 3: Temporary test** `beat_carries_land_load`: a beat with two `SrLoad`s → the `hosts` read returns them in order; `NULL` → empty. Run: `just test-one world-store "--lib beat_carries_land_load"`. Expected: PASS.
- [ ] **Step 4:** `just smoke`.
- [ ] **Step 5: Delete the test; commit** `feat(load): hosts report load per super-region in their beat (migration 0022)`.

### Task 3: The coordinator's move run

**Files:**
- Create: `crates/game/src/coordinator/moves.rs`
- Modify:
  - `crates/game/src/coordinator/mod.rs` (`projection!`, `new_for`, `takes_logins`, the Attach branch, `on_maintain`);
  - `world_cut.rs` (`CutRun.moves`, `BeginCut.moves`, `move_due`, `decided_as_stored` → the run's end, `cut_attach` → retry);
  - `succession.rs` (`open_run` / rollback / `first_of_generation` → abandon);
  - `input.rs` (`clock_floor`), `state.rs` (version 9 and its doc line), `msg.rs` (`MoveLand`, `ExpectLand`, `MoveOff`, `CutFailure::MoveRefused`, `BeginCut.moves`);
  - `assign.rs` (`pick_taker` beside `pick_assignee`).
- Modify: `crates/svc-link/src/lib.rs` (13 and its history line), `crates/server/src/coordinator_rt.rs` (`Busy` → retry; the `VOXEL_MOVE_WARNING_SECS` / `VOXEL_MOVE_PROBE_MS` dials parsed with the fleet dials, both refused together with `VOXEL_FLEET_MAX`; the window line's `moves=` / `move_abandoned=`).

**Interfaces:**
- Consumes: Task 1's `CutDecide::moves` and `StoreError::Busy`; Task 2's `HostSeen.land_load`.
- Produces:
  - `Coordinator::open_move(sr: SuperRegion, giver: HostId, taker: HostId, why: MoveWhy) -> bool` (ticker only; false if a precondition fails).
  - `Coordinator::move_open() -> Option<&LandMove>`.
  - `Coordinator::evacuating(h: HostId) -> bool`.
  - `pick_taker(&self, sr: SuperRegion, giver: HostId) -> Option<(HostId, u64)>`.
  - The probe rule: with `VOXEL_MOVE_PROBE_MS`, `maintain_moves` opens one move per that period: the most loaded host's super-region with the most players, else (none with players) the one with the most running regions.

- [ ] **Step 1: Verify decision 2 against the code.** Check `SuccessionRuns`' re-send on Attach, `rebase_deadlines`, `clock_floor`, `cut_attach`'s abort and `maintain_cuts`' due rules; stop if one fails.
- [ ] **Step 2: Implement `MoveRuns` and its life** exactly as decision 2: open, due, every end, attach re-send, `takes_logins`, `pick_taker`.
  - Classify the new field in `projection!` (durable).
  - Add an input-table row only if a new `Input` variant is introduced. None is planned: orders are outputs, answers are `CutPrepared` / `CutFailed` / decisions already logged.
- [ ] **Step 3: Bump the versions** (`COORDINATOR_STATE_VERSION` 9: "move runs"; `LINK_PROTOCOL_VERSION` 13: "moves: `BeginCut.moves`, `MoveLand`, `ExpectLand`, `MoveOff`, `PlayerMove.place`, `ServerPacket::LandMoving`").
- [ ] **Step 4: Temporary core tests** in `coordinator/moves.rs` (`#[cfg(test)]`, deleted before the commit):
  - `move_orders_rows_then_cut_carries_it`: open → `MoveLand` to G and `ExpectLand` to T in one transition; world tick past `at_tick` → `BeginCut.moves == [mv]`, state `Cutting`.
  - `move_retries_then_abandons`: three aborted cuts → `MoveOff` to both, `abandoned[sr]` set.
  - `move_abandons_on_taker_detach`, `…_on_succession_open`, `…_on_rollback`, `…_on_deadline`.
  - `move_resends_order_on_attach`: a G `Attach` (same boot) during `Warning` re-pushes `MoveLand` after `Attached`; another boot → abandon.
  - `giver_takes_no_logins`: `gate_host()` skips the giver; `pick_assignee` skips it.
  - `move_deadline_rebased_at_load` and `move_survives_rebase`: encode → decode the core with an open run; the deadline restarts if older than `CALENDAR_GAP_MS`; a rebase replays to the same run.

  Run: `just test-one game "--lib coordinator::moves"`. Expected: all pass.
- [ ] **Step 5:** `just smoke` and `just web-check`.
- [ ] **Step 6: Delete the tests; commit** `feat(coordinator): a durable move run orders the giver and the taker and carries the move in a world cut`.

### Task 4: The giver — warn, park keeping the sessions, stop and mask S, write it, hand it over at the commit

**Files:**
- Create: `crates/game/src/world_manager/moves.rs`
- Modify: `crates/game/src/world_manager/world_cut.rs` (`begin_world_cut` → `begin_move_out`; `try_complete_cut` → `WorldCut::moves`; `cut_committed` → `land_moved` BEFORE `drop_stored_parked`; `cut_failed` → `move_aborted`)
- Modify: `crates/game/src/world_manager/peers.rs` (`owner_of_sr` reads the move mask; `accept_handoff` / `accept_move` → `move_held` on G; `peer_moving` from `BeginCut`)
- Modify: `crates/game/src/world_manager/placement.rs` (`Placement::Wait` for S from `at_tick − SPAWN_PIN_MS`)
- Modify: `crates/game/src/world_manager/succession.rs` (`host_fields!` per decision 4)
- Modify: `crates/game/src/world_manager/mod.rs` (a park reason that keeps the session; per-super-region shutdown)
- Modify: `crates/game/src/peer/msg.rs` (`MovePlace`, `PlayerMove.place`; every sender `Landing`), `crates/game/src/protocol.rs` (`ServerPacket::LandMoving`), `crates/game/src/world_manager/drain.rs` (send `LandMoving` beside `HostDraining`)
- Modify: `crates/server/src/cuts.rs` (shrink by moved rows), `crates/server/src/lib.rs` (route `MoveLand` / `MoveOff` to the manager)

**Interfaces:**
- Consumes: Task 3's `CoordMsg::MoveLand`, `MoveOff`, `BeginCut.moves`, `CutFailure::MoveRefused`; Task 1's `WorldCut::moves`.
- Produces:
  - `WorldManager::move_land(id: MoveId, sr: SuperRegion, taker: HostId, at_tick: Tick)` and `move_off(id: MoveId, sr: SuperRegion)`.
  - `WorldManager::move_epochs_of(cut: CutId) -> Vec<(SuperRegion, u64)>` (for the server's shrink).
  - `PlayerMove { place: MovePlace::Pose, .. }` sent to T after the commit.
  - `move_held` delivery on commit and abort.

- [ ] **Step 1: Verify decision 5's steps 1–4 and the "who holds what" list for G against the code** (`park_all_players`, `shutdown_all`, `drop_stored_parked`, `cut_transit`, `parked_written`, `world_cut_wrap`). Stop and report if one fails.
- [ ] **Step 2: Implement the warning, the `BeginCut` path, the commit and the abort** on G exactly as decision 4. Keep `off_map` checks ahead of every new owner check (the closed x-wrap seam reads as off the map).
- [ ] **Step 3: Temporary seeded-rig tests** in `crates/game/tests/` (`MultiRig`, three hosts; deleted before the commit):
  - `move_keeps_player_once_and_at_pose`: a player standing on S → move committed → the player exists exactly once across all hosts and the store at every step; T restores it at the cut's pose (bit-equal `pose`).
  - `move_abort_restores_on_giver`: T drained before the decision → abort → the player is on G at the same pose, and S is G's at epoch `e`.
  - `move_holds_peer_handoff_in_gap` (Review Focus 3): P hands a body into S after G masks and before P applies `BeginCut` → G holds it, forwards it to T at the commit; no `PlayerParked`.
  - `move_login_in_pin_window_waits` (Review Focus 4): a login for a player on S at `at_tick − 10 s` → `Placement::Wait`, then placed on T; no `PLAYER_ELSEWHERE_REFUSAL`.
  - `move_walk_in_late_gets_notice` (Review Focus 1): a body enters S at `at_tick − 1 s` → `LandMoving` with a standby ticket, then moved at its pose.
  - `move_refused_with_pin_on_s`: a spawn pin on S at `BeginCut` → `CutFailed(MoveRefused)`; nothing masked after.

  Run: `just test-one game "--test it moves_tmp::"`. Expected: all pass.
- [ ] **Step 4:** `just smoke`, `just web-check`.
- [ ] **Step 5: Delete the tests; commit** `feat(host): the giver warns, parks keeping sessions, writes S in its part and hands its players over at the commit`.

### Task 5: The taker and the peers — expect, fence from the store, place at the pose, promote the standby session

**Files:**
- Modify: `crates/game/src/world_manager/moves.rs` (`expecting`, the taker's `move_held`, `MOVE_HOP_WAIT_MS`)
- Modify: `crates/game/src/world_manager/peers.rs` (`accept_move` with `Pose` → `restore_player`; `claimed` delivers holds and sends `RegionElsewhere` naming itself for S's subscribed regions; `reanswer_redirected_to` filtered to S on other homes; `ask_for_land` skips expected land; `peer_moving` lifted by a newer epoch)
- Modify: `crates/game/src/world_manager/mod.rs` (`close_idle_observers` spares a standby session)
- Modify: `crates/server/src/claims.rs` (`DirectoryKeeper`: fence an expected-and-committed move's row even while a succession is open; a read still showing G retries), `crates/server/src/lib.rs` (route `ExpectLand`)

**Interfaces:**
- Consumes:
  - Task 3's `ExpectLand`, `MoveOff`, `BeginCut.moves`;
  - Task 4's `PlayerMove { place: Pose }` and `move_held` forwarding;
  - `WorldManager::expected_committed() -> Vec<SuperRegion>` (produced here, read by the keeper).
- Produces: `WorldManager::expect_land(id: MoveId, sr: SuperRegion, giver: HostId, at_tick: Tick)`.

- [ ] **Step 1: Verify against the code:**
  - decision 4's taker list and departure 4 (the keeper's heal path, `known_epoch`, the withheld rows, `observer_connected` promoting an `awaiting_session` client);
  - departure 10 (`CoordMsg::Assigned` carries the read epoch).

  Stop and report on a mismatch.
- [ ] **Step 2: Implement** decision 4 for T and the peers.
- [ ] **Step 3: Temporary rig tests** (deleted before the commit):
  - `taker_never_loads_before_commit_prepared`: `CutDone{committed}` delivered while the part is still prepared → T does not fence, and no region load is attempted; after `COMMIT PREPARED` → fenced, then loaded.
  - `hop_without_standby_parks_once` (Review Focus 2): no observer session on T → one park, G refuses `PLAYER_PARKED_ELSEWHERE`, the rejoin places the player once on T.
  - `hop_with_standby_promotes`: an observer session on T before the commit → `HomeHop` + `PlayerRegion` + `RegionElsewhere` naming T; no refusal on G.
  - `taker_dies_after_commit_prepared`: T killed after the fence → crash succession; the heir holds S, and G's report re-sends the `PlayerMove`; the player exists once.
  - `peer_mask_lifts_on_taker_owns` and `peer_mask_lifts_on_abort`.
  - `expectation_cleared_on_reattach_without_resend`.

  Run: `just test-one game "--test it moves_tmp::"` and, for the keeper, `just test-one server "--lib claims::"`. Expected: all pass.
- [ ] **Step 4:** `just smoke`, `just web-check`.
- [ ] **Step 5: Delete the tests; commit** `feat(host): the taker fences moved land from the store, places moved players at their pose and promotes their standby sessions`.

### Task 6: Crash and conservation pass across Tasks 1–5 (Postgres rig)

**Files:**
- Test only (temporary): `crates/server/tests/takeover.rs` beside its existing rig (`hosts_rig`, real peer links, Postgres, `--features test-hooks`)
- Modify: only what the runs show broken, each fix in the task's own area

**Interfaces:** none new.

- [ ] **Step 1: Run decision 6's table as temporary scenarios** on the takeover rig, one per row that the rig can kill:
  - G killed during `Warning`, during `Cutting`, after the commit before sending;
  - T killed during `Warning`, between the decision and `COMMIT PREPARED` (the `hold_decision_retries` hook), after the fence;
  - a coordinator replica killed mid-run (`coordinators=2`);
  - G re-attached mid-`Warning`.

  Assert after each:
  - generation unchanged unless a rule names a rollback;
  - every player and entity exactly once (the rig's conservation assert);
  - the run ended with the reason the table names.

  Run: `just test-r4` scoped to the new scenarios (`just test-one server "--features test-hooks --test takeover move_"`). Expected: all pass.
- [ ] **Step 2: Fix what fails**, re-run, and record each fix as a ruling in the ledger.
- [ ] **Step 3: Delete the scenarios; commit the fixes** (if any) as `fix(move): …` with the scenario named in the message.

### Task 7: The client — notice, standby connection, still input during the pause, re-map at the hop

**Files:**
- Modify: `crates/client/src/instance.rs` (`ServerPacket::LandMoving` from the home only; a `MoveDeadline` beside `drain`; input hold; `home_moved` keeps working)
- Modify: `crates/client/src/connections.rs` (a standby connection: opened with the ticket `MOVE_STANDBY_LEAD_SECS` before `at_tick`, maps no region, exempt from `close_idle` until the hop, `at_tick: None`, or `at_tick + MOVE_DEADLINE_MS`)
- Modify: `crates/client/src/renderer/drain_notice.rs` (the move line: "This area moves to another server in M:SS." / "Moving this area to another server…"; no "You will reconnect"; shown only while the predicted body's super-region is S)
- Modify: `crates/client/src/renderer/mod.rs` / `bridge.rs` (`ClientUpdateEvent::LandMoving { remaining_secs: Option<u32> }`; the resource is registered with `add_session_reset`)

**Interfaces:**
- Consumes: `ServerPacket::LandMoving` (Task 4); `HomeHop` / `PlayerRegion` / `RegionElsewhere` naming the home (Task 5).
- Produces: none.

- [ ] **Step 1: Implement.**
  - Input hold: from `at_tick − WORLD_CUT_LEAD_TICKS` until the new home's `PlayerRegion`, or `at_tick: None`, local `PlayerInput` is held as `!is_caught_up` holds it.
  - A `RegionElsewhere` naming the new home re-maps S's regions (`remap_to_home`, existing).
  - Review Focus 5: `at_tick: None` clears the line, closes the standby and lifts the hold.
- [ ] **Step 2: Type-check:** `just smoke` and `just web-check`. Expected: PASS. **Client tests are opt-in: ask Dawid** before `just test-one client "--lib connections"`. If he agrees, run a temporary test `standby_survives_idle_close_and_closes_on_cancel`, then delete it.
- [ ] **Step 3: Commit** `feat(client): a moving area's notice, a standby connection to the taker, still input during the pause`.

### Task 8: Scale-down empties a host by moving its occupied land first

**Files:**
- Modify: `crates/game/src/coordinator/fleet.rs` (rule 6's players branch → `evacuating`; rule 6b; the scale gates)
- Modify: `crates/game/src/coordinator/moves.rs` (`evacuating` life: an ended / succeeded host leaves; `EVACUATE_MAX_MS`; `evac_abandoned`)
- Modify: `crates/server/src/coordinator_rt.rs` (the window line's `evacuating=`)

**Interfaces:**
- Consumes: Task 3's `open_move`, `pick_taker`, `evacuating`; Task 2's `HostSeen.land_load`.
- Produces: none.

- [ ] **Step 1: Verify `fleet.rs` rule 6 and the victim key against decision 7;** stop on a mismatch.
- [ ] **Step 2: Implement decision 7's rules 6 and 6b and the gates.**
- [ ] **Step 3: Temporary core tests** (deleted before the commit):
  - `scale_down_moves_then_drains_at_zero`: a victim with players on two super-regions → two moves in turn → `DrainHost { in_ms: 0 }`.
  - `scale_down_falls_back_after_two_abandons` → `DrainHost { in_ms: 300_000 }`.
  - `scale_down_no_taker_falls_back`.
  - `evacuation_ends_by_max`.
  - `evacuating_victim_ended_leaves_set`.
  - `no_scale_decision_while_move_open`.

  Run: `just test-one game "--lib coordinator::fleet"`. Expected: all pass.
- [ ] **Step 4:** `just smoke`.
- [ ] **Step 5: Delete the tests; commit** `feat(fleet): scale-down moves an occupied host's land before it drains it`.

### Task 9: Hotspot relief (droppable — Dawid decides)

**Files:**
- Modify: `crates/game/src/coordinator/fleet.rs` (rule 9 after rule 7), `crates/game/src/coordinator/moves.rs` (`hotspot_since` as a process field, `reset` in `projection!`; its timer moves only on a new `hosts` read)

**Interfaces:**
- Consumes: `pick_taker`, `HostSeen.land_load`, `open_move`.
- Produces: none.

- [ ] **Step 1: Implement rule 9** as decision 7 (one move at a time; never a host's only running super-region; the scale gates apply).
- [ ] **Step 2: Temporary tests** `hotspot_moves_busiest_after_gap_holds`, `hotspot_never_moves_only_land`, `hotspot_timer_resets_on_failed_read`. Run: `just test-one game "--lib coordinator::fleet"`. Expected: PASS.
- [ ] **Step 3:** `just smoke`. Delete the tests; commit `feat(fleet): relieve a hotspot by moving its busiest super-region`.

### Task 10: Metrics, alerts, the twin phase, the probe, docs, closing

**Files:**
- Modify: the coordinator metrics (`mmo_move_open`, `mmo_move_{opened,committed,abandoned}_total{why}`, `mmo_fleet_evacuating`) and the host metrics (`mmo_move_hop_parked_total`, `mmo_move_held_total`)
- Modify: `deploy/modules/monitoring/` (alert `WorldMoveAbandoning`: `increase(mmo_move_abandoned_total[30m]) > 3`, which tells an operator that scale-down falls back to warned drains)
- Modify: `scripts/twin_check.sh` (phase 14), `justfile` (`move-probe`; the twin root sets `VOXEL_MOVE_PROBE_MS` / `VOXEL_MOVE_WARNING_SECS` only for phase 14)
- Modify: `crates/client/src/renderer/autopilot.rs` (`VOXEL_AUTOPILOT=stand`: holds still, logs the predicted pose each second and at `HomeHop`)
- Modify: `CLAUDE.md` ("Many hosts": a new "Moving occupied land" bullet; "Owner epochs & fencing"; "Peer link rules"; the fleet's rule 6 / 6b / 9; the dials), `TODO.md` (an Elastic 5 block with the hand checks owed and the known limits)

**Interfaces:** none new.

- [ ] **Step 1: Metrics and the alert.** Check the alert with `promtool check rules` in a container; applying it to orion is Dawid's.
- [ ] **Step 2: Twin phase 14** (with `VOXEL_MOVE_PROBE_MS=15000`, `VOXEL_MOVE_WARNING_SECS=10`, `land_idle_ms` 20 s): a join places land on one host, its region runs, the probe moves that super-region.
  - **PASS:** the `super_regions` row's owner changes to the other host at `epoch + 1`; the generation is unchanged; no `successions` row; a second join lands on the taker; neither pod restarted.
  - **SKIP 14:** no move opened within 60 s (no running region was seen).

  Run: `just twin-check`. Expected: last line `PASS` (or `PASS (… skipped)` with the reason in the log).
- [ ] **Step 3: Write `just move-probe [delay] [seconds]`** on `scripts/dev_r3.sh` (two `claim` hosts, `VOXEL_MOVE_PROBE_MS`, `VOXEL_MOVE_WARNING_SECS=20`), plus one release client under `VOXEL_AUTOPILOT=stand`. Its summary reports the pause (last snapshot from G → first from T), the pose jump at the replace, the hop's packets and any refusal.
  - **Do NOT run it**: it is a live run, owed to Dawid after the coordinator HTTP plan's hand checks (a), (c), (d). Write the exact commands into `TODO.md`.
- [ ] **Step 4: Rewrite the `CLAUDE.md` sections** from the code, one load-bearing sentence at a time. The reviewer checks each claim against the code.
- [ ] **Step 5: Closing.**
  - Run `just test-full` (say so first; record any failing stage, and check `git status` for other sessions' edits before blaming this plan).
  - Run the whole-branch review in three areas on the most capable model, each given the seams:
    - **A** store + coordinator: decision 3's walks, decision 6's table, abandon paths vs successions / rollback / leaves / the fleet.
    - **B** hosts: decision 5's proof, `drop_stored_parked` ordering, holds, `host_fields!`, the keeper's open-run exception.
    - **C** client + docs + alerts + twin + probe.
  - Then ONE fix wave, one scoped re-review, and an "As built" section here.

## Not in this plan

- Re-keying `in_flight` by region and removing the `Leave` succession (departure 1, Dawid).
- A live move without a cut (design §5, "not planned"). Several nodes. Rebalancing as a goal of its own. An operator `move-land` command.
- Splitting a crowded super-region (impossible: one thread per region).

## Known limits (to carry into `TODO.md`)

- **A player with no standby connection to T reconnects once** (Review Focus 2): a late join to S, a failed QUIC connect, a browser that refused the second WebTransport session.
- **Momentum and look direction are not carried.** The input hold makes the body still. A falling or knocked-back body lands where the cut saved it.
- **The pause** is the cut's lead (≥ `WORLD_CUT_LEAD_TICKS`), plus `COMMIT PREPARED`, T's directory read (≤ `DIRECTORY_READ_RETRY_MS` per try) and S's region load. It is unmeasured until the probe runs.
- **One move per world at a time.** Evacuating a host with many occupied super-regions takes one cut and its pause per super-region.
- **A spawn pin on S refuses the move's part** (`MoveRefused`): a login in the last 120 s before `at_tick` can cost one retry of the cut.

## Self-review notes

- Spec coverage:
  - Design step B → Tasks 1, 3, 4, 5 and 7.
  - "Taker dies" → decision 6, Tasks 1 (the store hole) and 6.
  - "Host dies while draining, during the warning" → decision 2's abandon on G's lapse.
  - "A crowded super-region" → Task 9 moves it and never splits it.
  - The warning period → decision 8.
  - The handoff's §2 bullets: the move (Tasks 1–7), scale-down (Task 8), hotspot (Task 9). The `in_flight` re-key is departure 1.
- Type names used across tasks: `MoveId`, `LandMove`, `SrLoad`, `WorldCut::moves`, `CutDecide::moves`, `BeginCut.moves`, `MovePlace`, `ServerPacket::LandMoving`, `MoveRuns`, `pick_taker`, `open_move`, `moving_out` / `expecting` / `peer_moving` / `move_held`, `StoreError::Busy`. Each is defined once, in decision 1 or the producing task's Interfaces.
- Proportion: decisions 1–7 carry the design. Tasks give signatures, the checks and their assertions, and no bodies.
