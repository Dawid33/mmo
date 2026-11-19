//! Successions (R4): when a host of a multi-host world dies (its lease lapsed, or its row
//! was vouched dead by its next boot) or left clean, the coordinator declares it in ONE
//! transaction — its own transition's, Elastic 2b Task 3 ([`declare_in`]) — that marks a crashed host's row dead, inserts the
//! `successions` row and moves every super-region of the predecessor to one heir with
//! `epoch + 1` — that update is the claim; the heir only reads its new fence. Nothing else
//! is written until the next committed world cut, whose decision settles the open
//! successions in its own transaction (`two_phase::decide_in`'s `settles`). A world
//! rollback ([`rollback_world_in`]) abandons the open ones; the land stays with the heirs.
//! Postgres only: several hosts need it (SQLite answers `Unsupported`).
//!
//! A `Leave` declaration needs the predecessor's row clean and does not mark it dead; a claim
//! by that boot is refused anyway — a claim's last statement (`rows.rs` `check_live`) needs
//! the claimer's row live AND dirty, so a cleanly-left boot claims nothing from its leave
//! cut's commit on, and its next boot registers dirty and claims normally.
//!
//! Locks (the declaration): `coordinator` (held `FOR NO KEY UPDATE` by its transition's
//! compare-and-set, below), then `world` `FOR NO KEY UPDATE`
//! (it serialises with `decide_in`, `register_host` and `rollback_world_in`, which take the
//! same lock in the same order, and never waits for a claim's `FOR KEY SHARE`), then the
//! predecessors' `hosts` rows `FOR NO KEY UPDATE` in id order (the heartbeat's load columns ride its same one-row
//! statement, no new lock; a heartbeat waits for
//! the declaration, so a beat between the lapse check and the update cannot be lost), then
//! each predecessor's `super_regions` rows `FOR NO KEY UPDATE` in key order. A staged cut
//! part of the predecessor holds those rows `FOR SHARE` (the rows it releases `FOR NO KEY
//! UPDATE`, Elastic 3: "A part's release" below) until its cut is decided: the
//! declaration waits for it, at most `lock_timeout` (the caller declares only between
//! cuts; a timeout is a retry). It never locks a row a staged part could be waiting for.
//!
//! One deadlock cycle with a CLAIM is possible, and it is safe (Postgres aborts one
//! transaction with `40P01`, which writes nothing; each side treats that as a retry or a
//! fence, never as a start-up error):
//! 1. **A boot's re-claim of its own rows vs the declaration of that boot**
//!    (`claim_super_regions_live`, `rows.rs`): the re-claim upserts the boot's
//!    `super_regions` rows in key order, then takes its `hosts` row `FOR SHARE` (its last
//!    statement); the declaration holds that `hosts` row `FOR NO KEY UPDATE` and waits for
//!    the same `super_regions` rows. If the re-claim is aborted, the server re-reads its row
//!    (`HostClaims::reclaim_own`): no longer its live boot (the declaration won) → the
//!    fenced exit (3); still live → it re-claims once more. If the declaration is aborted,
//!    the coordinator declares again after a fresh lease read (the re-claimed rows then move
//!    to the heir, or the row is no longer lapsed).
//!
//! A second safe cycle (Elastic 3) is a declaration against a cut part of the same host that
//! releases land: under "A part's release" below.
//!
//! (Corrected, Elastic 1: the R4 notes listed a second cycle, "a zombie's claim of free land vs
//! a declaration of several predecessors". Elastic 3 removed the host's claim of free land
//! (`claim_free`) altogether: free land is only ASSIGNED, by the coordinator inside its
//! transition (below). The host-side statements on `super_regions` left are a boot's re-claim of
//! its own rows (`claim_super_regions_live`, cycle 1 above) and the unguarded boot claim of
//! `AllInOne` / `Static` and tools (`claim_super_regions` with no live check, no `hosts` row).)
//!
//! **The drain mark** (Elastic 1 Task 7; `rows.rs` `drain_host`, `worldctl drain`): ONE
//! `UPDATE` of one dirty live `hosts` row's `drain_at_ms` (beaten within the lease: a lapsed
//! row is refused, the guard in the same statement), UNFENCED like the heartbeat (it
//! changes no ownership) — it holds that row and wants nothing after it, so it closes no cycle:
//! like a heartbeat it only waits (at most its `lock_timeout`) for whoever holds the row — a
//! declaration, an end of a landless host, a registration's vouch or total crash, a re-claim's
//! `check_live` `FOR SHARE`, an assignment's `FOR SHARE` of its assignee's row — and nothing waits
//! for it while it waits. An assignment reads the mark under its share lock (its first
//! statement, below): a mark that commits first is seen (`HostDraining`, nothing written), one
//! that comes after waits for the transition's commit (the land assigned just before is kept and
//! leaves with the host). A re-claim of a host's OWN rows (`claim_super_regions_live`) ignores
//! the mark.
//!
//! **The survivors** (Elastic 2a, the crash-loop rule): the declaration's survivors are the
//! dirty live hosts that hold land — a `super_regions` row they own or an `in_flight` row of
//! their id — plus the call's heirs, minus the predecessors: ONE plain `SELECT` over `hosts`
//! with `EXISTS` subqueries on `super_regions` and `in_flight`, under the `world` lock and after
//! the predecessors' rows are locked. It takes no lock (a plain read), so it changes no lock
//! order and joins no cycle. A claim committing land just before it is seen (that host is a
//! survivor). A host landless at the read exchanged no state with the dead boot BEFORE the
//! read (it ran no region). AFTER it, it may win land — and, its directory refreshed by no
//! order, route state to the dead host (land adjacent to the dead host's, a crossing before
//! the heir adopts): state no survivor reports to the heir — also when a zombie of the dead
//! host acknowledged it (its journal entry and unsaved-traffic mark stay). That host never saves it: its
//! next world part is refused (unsaved traffic with a host outside the cut), it reads the
//! directory, and at the next `BeginCut` — none begins until every recipient applied its
//! order, which reaches a recipient before that `BeginCut` — a dead row it holds no
//! succession of means it is no recipient: it stops without a save (`game` `world_cut.rs`,
//! `WorldManager::take_unaccounted_dead`). Its lapse is a LANDED host's while the run is open:
//! a second failure, the world rolls back. The same holds for any landed host dying before the
//! settle (the residual Elastic 2a accepts): a rollback, never a loss, never a world that stops
//! saving.
//!
//! **Ending a landless host** ([`end_landless_in`], Elastic 2a): in the coordinator's
//! transition, under `lock_timeout` = the cut lock timeout. The lock walk, in this order:
//! `coordinator` (the transition's compare-and-set), `world` `FOR NO KEY UPDATE` (no generation compared — it does
//! not touch the generation), then the host's one `hosts` row `FOR NO KEY UPDATE`. That is a
//! prefix of the declaration's walk (`coordinator` → `world` → the predecessors' `hosts` rows),
//! so it adds no cycle: it serialises with every declaration, registration, rollback and
//! decision on `world`, and takes no `super_regions` row at all — the cycle-1 shape (a
//! transaction holding `super_regions` rows and waiting for a `hosts` row, against one holding
//! that `hosts` row and waiting for those `super_regions` rows) cannot form with it. Then plain
//! reads (no locks): the row's lapse by the database clock, `EXISTS` over its `super_regions` /
//! `in_flight` rows, `EXISTS` over the open `successions` naming it heir or survivor (read under
//! `world`, which every writer of `successions` takes first). Then `UPDATE hosts SET state =
//! DEAD` of the row already locked. A heartbeat of that row waits for it (its 1 s lock timeout:
//! "no beat this time"; after the commit it answers `live: false` — the host is replaced). A
//! ZOMBIE (the ended process still running) wins no land: free land comes only by an assignment,
//! decided under the `coordinator` row like the end itself — committed before the end's
//! transition, the end's `EXISTS` sees the land and answers `OwnsLand` (nothing written); after
//! it (or later in the same transition), the assignment's `FOR SHARE` read finds the row dead
//! (`HostNotLive`). A re-claim of the host's own rows (`claim_super_regions_live`)
//! re-claims nothing for a landless host. A drain mark and a vouch wait for the row as for a
//! declaration. Nothing waits for a lock the end holds while that holder waits for it.
//!
//! **Assigning a free super-region** (`assign.rs` `assign_land_in`, Elastic 3): in the
//! coordinator's transition, in its savepoint, under `lock_timeout` = the cut lock timeout. The
//! `coordinator` row is already held (the compare-and-set). Then, in this order:
//! 1. `SELECT drain_at_ms FROM hosts WHERE id = $h AND boot = $b AND state = LIVE AND clean = 0
//!    FOR SHARE`. No row → `HostNotLive`; a drain mark → `HostDraining`.
//! 2. A PLAIN `SELECT owner FROM super_regions WHERE sx = $1 AND sz = $2` (no lock): owned →
//!    `Owned`, nothing more (Elastic 3 Task 5; why: the last bullet below).
//! 3. Only if that read found no row: `INSERT INTO super_regions (sx, sz, owner, epoch) VALUES
//!    ($1, $2, $h, 1) ON CONFLICT (sx, sz) DO NOTHING RETURNING epoch`.
//! 4. If no row was inserted: `UPDATE super_regions SET owner = $h, epoch = epoch + 1 WHERE sx
//!    = $1 AND sz = $2 AND owner = -1 RETURNING epoch`. No row → `Owned`.
//!
//! Why no cycle:
//! - Every coordinator decision runs under the `coordinator` row, so an assignment never runs
//!   CONCURRENTLY with a declaration, an end of a landless host or another assignment: those of
//!   other transitions wait for the row. Inside ONE transition's transaction an assignment may
//!   follow (or precede) a `Declare` or an `EndLandless` of host X, each decision in its own
//!   savepoint, in order. X's `hosts` row is then held `FOR NO KEY UPDATE` by that same
//!   transaction, and the assignment's `FOR SHARE` of it (or the declaration's lock after the
//!   assignment's share) is a lock of the transaction on its own row — no wait, no conflict with
//!   itself. It reads the row as the earlier decision left it: an assignment to X after X was
//!   declared or ended finds it dead (`HostNotLive`); a declaration after an assignment to X moves
//!   the assigned row with X's other land (it sees its own transaction's write). The declaration's
//!   moved rows are owned, so an assignment never writes one (`Owned`).
//! - Its walk is `coordinator` → `hosts` (one row, share) → `super_regions` (one row). The
//!   declaration's is `coordinator` → `world` → `hosts` → `super_regions`; a registration's is
//!   `world` → `hosts` → `successions` → `hosts` → `super_regions`. `hosts` before
//!   `super_regions` in all three.
//! - The host-side statements on `super_regions` (the host's claim of free land is gone,
//!   Elastic 3) are a boot's re-claim of its own rows (`claim_super_regions_live`:
//!   `super_regions`, then its `hosts` row `FOR SHARE`) and the unguarded boot claim of
//!   `AllInOne` / `Static` (no `hosts` row, never beside a coordinator that assigns). The
//!   re-claim locks only rows it owns (what its registration gave it); an assignment writes only
//!   a free or absent row. They share no `super_regions` row, and their locks of the host's
//!   `hosts` row are both `FOR SHARE`, which do not conflict.
//! - Statements 2 to 4 never wait for a staged part. A row a part holds is owned in every
//!   committed snapshot: the plain read answers `Owned` and neither write runs. A part that
//!   RELEASES the row is the same case until its `COMMIT PREPARED` — and the read is what keeps
//!   it so (found by Task 5's test): that part has UPDATED the row (owner -1, epoch + 1), and
//!   the unique check of an `INSERT … ON CONFLICT` meeting a row version an uncommitted
//!   transaction updated WAITS for that transaction; a part that only share-locks a row is a
//!   locker, not an updater, and is not waited for. The insert runs only for a row the read
//!   found absent (no part holds an absent row); the `UPDATE`'s `WHERE owner = -1` is evaluated
//!   on the committed version first, so an owned row is not locked.
//! - The share lock on the assignee's `hosts` row is held until the transition commits. The
//!   assignee's beat (`UPDATE hosts SET seen_ms …`) waits behind it, under its own 1 s
//!   `lock_timeout`, as it does behind a declaration. A total-crash registration that would
//!   mark the row dead waits too, then reads `super_regions` in a later statement and sees the
//!   assignment. A drain mark (one `UPDATE` of that row) waits likewise: one that committed
//!   first is seen (`HostDraining`), one after it finds land the host then leaves with.
//!
//! Cost: at most `ASSIGN_PER_PASS_MAX` (64) assignments per transition; each is at most four
//! single-row statements.
//!
//! **A part's release** (`rows.rs` `cut_rows`, Elastic 3 Task 5): a host's world-cut part
//! gives super-regions back (`WorldCut::releases`) inside its own transaction, before `PREPARE
//! TRANSACTION`. Refused with `Unsupported` before any statement: a release its fence does not
//! hold, and any release in a one-phase cut (`CutEnd::Commit`: static, all-in-one, SQLite —
//! nothing pairs the free land with a decision). The statements of a part, in this order (2 and 5
//! are the release's):
//! 1. `SET LOCAL lock_timeout`; the plain generation read (no lock).
//! 2. Only with `releases`: the fence check's statement over `releases` with `FOR NO KEY UPDATE`
//!    in place of `FOR SHARE`, in key order, compared as the fence check compares (owner and
//!    epoch must be the fence's; else `Fenced`).
//! 3. The fence check over the part's check set (`commit_check_set`: the touched super-regions
//!    and every one of the fence), `FOR SHARE`, in key order — a no-op on the rows step 2 holds.
//! 4. The region, chunk, entity, player and in-flight writes, unchanged.
//! 5. Per release: `UPDATE super_regions SET owner = -1, epoch = epoch + 1 WHERE sx = $1 AND sz =
//!    $2 AND owner = $me AND epoch = $e`; anything but one row → `Fenced`.
//! 6. `PREPARE TRANSACTION`.
//!
//! Why the released rows are locked `FOR NO KEY UPDATE` at step 2 and not at step 5: a part that
//! took them `FOR SHARE` (step 3) and updated them later would UPGRADE its lock, and a
//! declaration of this host waiting for its own `FOR NO KEY UPDATE` of those rows would queue
//! between the share lock and the upgrade — a deadlock on every such meeting. Locked for update
//! up front, the part holds what it will write from its first lock on.
//!
//! Why this is safe:
//! - "A cut part writes only rows of its own regions" holds: the released rows are the part's
//!   own until its commit, and no other host's part checks or writes them (a part's check set is
//!   its own land).
//! - The only other lockers of this host's `super_regions` rows are its own region loads (`FOR
//!   SHARE` of one row: none runs in land the host masks for release, and one still open from
//!   before the mask delays step 2 by its length), a declaration of this very host, a total-crash
//!   registration (nobody live) and a boot's re-claim of its own rows (a boot that is not this
//!   process). An assignment (above) never waits: the row is owned in every committed snapshot
//!   until `COMMIT PREPARED`, so its plain read answers `Owned` before any write (its insert
//!   would wait for the part, which has updated the row).
//! - **One new deadlock cycle, safe.** Steps 2 and 3 are two key-ordered passes, so a declaration
//!   of this host walking the same rows in ONE key-ordered pass can hold a row of step 3's set
//!   while this part holds one of step 2's and waits for it. Postgres aborts one side (`40P01`):
//!   the part's maps to `Fenced` (`fence_race`), which is true — the host is being declared dead;
//!   the declaration's is its savepoint's refusal, decided again later. Nothing is written by
//!   either aborted side.
//! - A STAGED releasing part holds the released rows `FOR NO KEY UPDATE` until `COMMIT PREPARED`
//!   / `ROLLBACK PREPARED`. A declaration of the releaser waits for it up to its `lock_timeout`, as
//!   it waits for a staged part's share lock, and then (READ COMMITTED re-checks the locked row)
//!   moves only the rows the releaser still owns: a committed release stays free. `decide_in`
//!   touches no `super_regions` row, and finishing the parts (`finish_staged`) runs after the
//!   decision's commit, outside the `coordinator` row: the part's lock never waits for anything.
//!
//! **A cut's begin** (`two_phase::begin_in`, Elastic 1 Task 4): in the transition that starts
//! the cut, after its compare-and-set holds `coordinator`, a PLAIN read of `world.next_host_id`
//! (no lock: a registration holding the world row commits its id before the read — below the
//! bound — or after it — at or above), the insert of the cut's own `cut_begins` row. No lock on
//! `world`, `hosts` or `super_regions`: it joins no cycle. `decide_in` reads that row plainly
//! after its `world` lock (it was committed before the decision began, or is absent: the
//! stricter rule then).
//!
//! **The coordinator's `hosts` read** (`rows::hosts`, the ticker's, Elastic 2b Task 6): a PLAIN
//! read of `world.next_host_id`, then a PLAIN `SELECT` over `hosts` (its `EXISTS` subqueries on
//! `super_regions`, `in_flight`, `successions`), then the database clock — no transaction, no
//! lock: it joins no cycle. An id below the first read's value was issued before the rows were
//! read; one at or above it may register after.
//!
//! **The prune of older begins** (Elastic 2a, `two_phase::decide_in`): right after a decision
//! row is inserted, in the decision's transaction, `DELETE FROM cut_begins WHERE cut_id < $cut`.
//! It locks only `cut_begins` rows of OLDER cuts — rows nothing reads or writes again (a cut
//! runs one at a time; only the cut being decided reads its own row, `begin_in` inserts
//! only the newer cut's row, `resolve_prepared` reads none) — after every lock the decision
//! already holds (`coordinator`, `world`, …) and wants nothing after them: it joins no cycle.
//! `cut_decisions` is never pruned.
//!
//! **The coordinator's transition** (Elastic 2b Task 3, `coordinator_tx.rs` `write_pg`): ONE
//! transaction, in this order:
//! 1. `UPDATE coordinator SET core, history = COALESCE($h, history), version = version + 1 …
//!    WHERE id = 1 AND version = $v RETURNING version, <now>` — the one `coordinator` row locked
//!    `FOR NO KEY UPDATE` to the commit. Zero rows (the version moved): rolled back, `Conflict`,
//!    nothing below ran. No epoch is compared: the version is the coordinator's only fence.
//! 2. With decisions, `SET LOCAL lock_timeout` (the cut lock timeout), then each decision of the
//!    transition, in order, in its own `SAVEPOINT`, its body WITHOUT a `coordinator` lock of its
//!    own (the row is held already, in a stronger mode than the `FOR SHARE` each took before):
//!    `two_phase::decide_in` (minus `finish_staged`, a post-commit effect), `two_phase::begin_in`,
//!    [`declare_in`], [`end_landless_in`], [`rollback_world_in`] (conditional on its generation),
//!    `assign::assign_land_in` (Elastic 3).
//!    A domain refusal or an error inside — a SQL error, a deadlock (`40P01`), a `lock_timeout`
//!    (`55P03`, a staged part holding a predecessor's rows), a stale generation — is
//!    `ROLLBACK TO SAVEPOINT` and that decision's answer: nothing of it is written, it is decided
//!    again later, and the core still commits.
//! 3. The outbox (Elastic 2b Task 5): `INSERT INTO outbox` of each message for a host linked to
//!    another replica (a new row each: no existing row locked).
//! 4. `DELETE FROM outbox WHERE host = ANY($attached_now)` — the rows of the hosts whose `Attach`
//!    this transition applied, discarded unsent (BEFORE the take: they are not delivered).
//! 5. `DELETE FROM outbox WHERE host = ANY($my_hosts) AND version <= $new_version RETURNING …` —
//!    this replica's own hosts' rows taken, in `seq` order (a row of another boot than its host's
//!    link is deleted unsent).
//!    Steps 4 and 5 lock `outbox` rows only, inserted by earlier transitions that committed
//!    before this one could hold `coordinator`: no transaction holds them now (only a take between
//!    transitions may — see below — and it holds nothing else).
//! 6. `SELECT pg_notify('coordinator_row', '<version>')` (no row lock; delivered at the commit).
//! 7. The commit.
//!
//! **A take between transitions** (`coordinator_tx::take_outbox_pg`, Elastic 2b Task 5): ONE
//! statement, `DELETE FROM outbox WHERE host = ANY($my_hosts) AND version <= $cache_version
//! RETURNING …` — the `outbox` rows only, no other lock: it holds them and wants nothing after
//! them. Bounded by the version of the cache that named `$my_hosts` (final review S-I2): a newer
//! row may have been decided after its host attached to another replica, whose relay it is. A transition's step 4 or 5
//! meeting a row it holds waits for its commit (one statement) and then skips it (the row is gone:
//! READ COMMITTED re-checks the deleted row) — a wait, never a cycle (only these deletes lock
//! `outbox` rows, each in one scan in the table's one order; should two ever meet in opposite
//! orders, the `40P01` aborts the take — retried — or the transition — a failed write, retried —
//! and nothing is lost: the rows stay until a take commits). Rows of one host are taken
//! by the one replica linking it (its runtime issues a take only while no transition of its own
//! is in flight), so a row is delivered at most once.
//!
//! Why no cycle: every statement that locks `coordinator` takes it FIRST (only the coordinator's
//! transitions, its bootstrap's one-statement insert, the replace path's one-statement epoch raise
//! — `rows::raise_coordinator_epoch`, reading `cut_decisions` without a lock — and a replica's
//! registration touch it); hosts, claims, registrations,
//! heartbeats, the drain mark and the operator never lock it. So nothing holds another lock and
//! then waits for `coordinator`. After it each decision keeps its own walk (`world` → `hosts` →
//! `super_regions`; `world` → `cut_decisions` → `hosts` → `successions`), which was already a
//! suffix of `coordinator` first; the `outbox` rows come last and are locked only by
//! coordinator statements (a transition, after `coordinator`; a take, alone). Another transition of the coordinator (a second replica, Elastic
//! 2b) waits on the row and then finds `version` moved (READ COMMITTED re-evaluates the `WHERE`):
//! a `Conflict`, never a deadlock. Cost: a decision holds the row for its `lock_timeout` at most
//! (a declaration waiting for a staged part); another transition waits that long, then conflicts.
//! On Postgres the transition takes no writer mutex of the store thread (a host's prepare queued
//! behind a staged part must not wait for it in-process); on SQLite every transition is one
//! statement, has no decision, and holds the writer lock.
//!
//! **The coordinator's replicas** (Elastic 2b Task 4, `replicas.rs`; table `coordinator_replicas`):
//! - A registration (`register_replica`), ONE transaction: `coordinator` FIRST — the upsert
//!   `INSERT … ON CONFLICT (id) DO UPDATE SET next_replica = next_replica + 1` issues the id
//!   (the row locked as a transition's compare-and-set locks it; it waits for a transition in
//!   flight, at most `CLAIM_LOCK_TIMEOUT_MS`) — then `coordinator_replicas`: the stale live rows
//!   marked dead (one `UPDATE`), the old dead rows pruned (one `DELETE`), the new row inserted.
//! - A beat (`beat_replica`): one `UPDATE` of the caller's own live `coordinator_replicas` row, on
//!   the beat connection under a 1 s `lock_timeout`; it holds that row and wants nothing after it.
//! - The end ([`game::coordinator::StoreDecision::EndReplica`], `replicas::end_replica_in`): in a
//!   transition, after its compare-and-set holds `coordinator`, in its savepoint — one `UPDATE` of
//!   that replica's row.
//!
//! No cycle: `coordinator_replicas` rows are locked only by these statements; the registration
//! and the end take `coordinator` first (as every statement locking it does), the beat takes no
//! other lock. So nothing holding a `coordinator_replicas` row waits for `coordinator`. A beat
//! behind a registration's lapse or an end waits at most its lock timeout ("no beat this time")
//! and, after their commit, answers "not live". Two transitions or registrations meet on the
//! `coordinator` row first: a wait, then a conflict, never a deadlock.
//!
//! **The coordinator's bootstrap** (`rows.rs` `bootstrap_coordinator`, Elastic 2b Task 3): ONE
//! statement, `INSERT INTO coordinator … ON CONFLICT (id) DO UPDATE … WHERE coordinator.core IS
//! NULL` — first writer wins; the one row and nothing else. It replaces the claim
//! (`claim_coordinator`, `epoch + 1` at every start), which no longer exists: there is nothing to
//! claim, a replica that finds the row loads it.
//!
//! **A registration** (`rows.rs` `register_host`, Elastic 1) — ONE transaction, UNFENCED: it
//! creates the identity a fence is built from; it writes no row a cut part writes except the
//! `in_flight` and `super_regions` rows of a total crash, which it takes at `epoch + 1` (so every
//! part or load fenced on the old epochs is refused). The lock walk, in this order:
//! `world` → `hosts` (the name vouch) → `successions` → `hosts` (the dirty rows) → `super_regions`
//! → `in_flight`:
//! - `world` `FOR NO KEY UPDATE` first (SQLite: the writer lock): serialises the counter
//!   `next_host_id`, the generation and every statement below with every other registration,
//!   declaration, end of a landless host, rollback and decision, which all take it first too. It never
//!   waits for a claim's `FOR KEY SHARE` (`check_side`), and the claim never waits for it.
//!   `UPDATE world SET next_host_id` (and, a total crash, `generation`) is the same row. A
//!   refused registration (`NotAlone`, the id bound, any error) rolls back: no id is issued.
//! - The name vouch, BEFORE anything is judged live: one `UPDATE hosts SET seen_ms = 0` over
//!   the other dirty live rows of the new host's (non-empty) name — several `hosts` rows, under
//!   `world`; against the one-row lockers as for the prune below (a heartbeat of such a row waits
//!   for this transaction and then lands after the vouch: the declaration's lapse re-check sees
//!   it). Those rows stay locked to the commit, so a restarted process's own old row is never
//!   counted live, and in a total crash it is among the dirty rows locked next (already held).
//! - The live read (`live_ids`) and the dirty count take no lock. The dirty count (Elastic 2a
//!   final review S-M1) counts a dirty row only if it is live, holds land (a `super_regions` or
//!   `in_flight` row) or is the predecessor of an open succession — plain subqueries over
//!   `super_regions`, `in_flight` and `successions`, still no lock, so it changes no lock order:
//!   an ENDED row (dead, landless, named by no open run) is no total crash; step 5 prunes it.
//! - A total crash only: the open `successions` rows `FOR UPDATE` (every other writer of them
//!   holds `world` first; a staged heir part reads them without a lock), then EVERY dirty
//!   `hosts` row `FOR NO KEY UPDATE` in id order — the live read is repeated under these locks,
//!   so a beat that landed in between makes its row live after all (no total crash; `static`
//!   is then `NotAlone`), and no beat lands from here on (a heartbeat holds only its own row
//!   and fails after its 1 s lock timeout: "no beat this time" — the row is lapsed already).
//!   Then every dirty row's `super_regions` rows `FOR NO KEY UPDATE` in key order (a
//!   declaration's order), the abandon (`UPDATE successions`, rows already locked; each
//!   predecessor's `in_flight` rows to its heir, [`rollback_in`]'s statements), the
//!   `super_regions` update, the dirty rows' `in_flight` rows, the dirty rows marked dead (rows
//!   already locked), the generation. A staged part of a dead host holds its
//!   `super_regions` rows `FOR SHARE` (and the `in_flight` rows it wrote or deleted) until its
//!   cut is decided: the registration waits for it at most its `lock_timeout`
//!   (`CLAIM_LOCK_TIMEOUT_MS`) and fails naming the staged gids, nothing written — every start
//!   resolves the staged parts of hosts that are not alive before it registers (Elastic 1
//!   Task 4, `server::roles::resolve_before_claim`: a part of a live host belongs to a cut under
//!   way and is its coordinator's; with nobody alive — the total crash — every part). A part being written locks `super_regions`
//!   first and `in_flight` after, as the registration does: no cycle. Nothing else here waits
//!   for a row a part could be waiting for.
//! - The prune: one `DELETE` of clean or dead `hosts` rows that own no `super_regions` /
//!   `in_flight` row and that no open succession names (the subqueries read without locks).
//!   The rows it can lock are clean or dead: a heartbeat matches only a live row (a clean live
//!   leaver's beat may wait for the delete and then answers `false` — it has left), a claim's
//!   `check_live` locks only a live dirty row, `vouch_host` and `drain_host` only a live dirty
//!   row; a cut part
//!   locks no `hosts` row; every statement that locks SEVERAL `hosts` rows (a declaration, a
//!   settle, an end of a landless host, another registration) holds `world` first. In a total crash the
//!   dirty rows it deletes were locked above (`FOR NO KEY UPDATE`, upgraded by the delete —
//!   no other transaction can hold a lock on them that conflicts).
//! - The `INSERT` of the new row: a key nobody holds (never issued before).
//!
//! Against the cycle above: a registration takes `hosts` rows before `super_regions` rows, as a
//! declaration does — so the one cycle it can join is cycle 1's shape: a re-claim of a row's
//! own land (`claim_super_regions_live`: `super_regions`, then its `hosts` row `FOR SHARE`)
//! while a total-crash registration holds that dirty `hosts` row and waits for those
//! `super_regions` rows. The re-claimer is a ZOMBIE (its lease lapsed, its process still
//! runs), or the process behind a row of the registration's own NAME within its lease: the
//! name vouch does not count that row live, so a registration with nobody else alive is a
//! total crash while the old process still runs and may re-claim. `40P01` aborts one: an
//! aborted re-claim is a fenced re-read (the row is
//! dead once the registration commits); an aborted registration wrote nothing and its process
//! exits and starts again. It extends no other cycle: an assignment takes its `hosts` row before
//! its one free `super_regions` row (the registration's order) and a free row is no dirty row's
//! land, which is all a registration locks there; a boot claim (`claim_super_regions` with no live check) locks no
//! `hosts` row, and the declaration, the end of a landless host, rollback and decision all wait for `world`
//! before anything a registration holds.

//! **After a world rollback** (the coordinator's [`rollback_world_in`] or the operator's
//! [`rollback_world_operator`], both through [`rollback_in`]): every host restarts from the
//! last committed cut K — no cut commits between a declaration and its settle, so that is the
//! abandoned succession's base. The land stays with the heir, and so do the predecessor's
//! in-flight rows of K: they are re-keyed to the heir (`in_flight.host`) in the same
//! transaction. Left under the predecessor's id they would have no reader: a host reads
//! the rows of its own id at boot (`load_in_flight(Some(own))`) and an heir reads a
//! predecessor's only while an open succession names it (`load_in_flight_of`), and an id is
//! never issued again. Re-keyed, the heir restores each exactly once at its boot
//! (forwarding a row whose target another host owns, as any boot does), and its first part
//! replaces them (it deletes every row of its id). A total crash at registration abandons the
//! same way and then re-keys every dirty row's rows (the heirs' among them) to the new host.
//!
//! The coordinator's rollback is conditional (Elastic 2a): under the same locks (`coordinator`
//! held by its transition, then `world` `FOR NO KEY UPDATE`) it compares the world's generation with the one
//! it decided the rollback at, and with another one writes nothing (a rollback that committed
//! before its coordinator recorded it is never done twice). No new lock, no new edge.
//!
//! The operator's commands (`worldctl retire-host`'s vouch, `worldctl rollback`) are in `operator.rs`.

use std::collections::BTreeSet;

use game::HostId;
use game::{CutId, HOST_DEAD, HOST_LIVE};
use sqlx::Row;

use crate::rows::i;
use crate::{StoreError, SuperRegion};

pub use game::{Declared, LandlessEnd, Succession, SuccessionId, SuccessionKind};

/// `successions.survivors`: ordinals, comma-separated.
pub(crate) fn encode_survivors(survivors: &[HostId]) -> String {
    survivors.iter().map(|h| h.0.to_string()).collect::<Vec<_>>().join(",")
}

pub(crate) fn decode_survivors(id: u64, s: &str) -> Result<Vec<HostId>, StoreError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(|o| o.parse().map(HostId).map_err(|_| StoreError::Corrupt(format!("successions {id}: survivors {s:?}"))))
        .collect()
}

/// `successions.moved`: `sx:sz:epoch`, comma-separated.
pub(crate) fn encode_moved(moved: &[(SuperRegion, u64)]) -> String {
    moved.iter().map(|((sx, sz), e)| format!("{sx}:{sz}:{e}")).collect::<Vec<_>>().join(",")
}

pub(crate) fn decode_moved(id: u64, s: &str) -> Result<Vec<(SuperRegion, u64)>, StoreError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let bad = || StoreError::Corrupt(format!("successions {id}: moved {s:?}"));
    s.split(',')
        .map(|m| {
            let mut p = m.split(':');
            match (p.next(), p.next(), p.next(), p.next()) {
                (Some(sx), Some(sz), Some(e), None) => Ok((
                    (sx.parse().map_err(|_| bad())?, sz.parse().map_err(|_| bad())?),
                    e.parse().map_err(|_| bad())?,
                )),
                _ => Err(bad()),
            }
        })
        .collect()
}

/// What every store that cannot declare a succession answers.
pub(crate) const SUCCESSION_UNSUPPORTED: &str = "successions need several hosts, which need Postgres";

/// The `lock_timeout` of a declaration ran out: a staged cut part (or another transaction)
/// holds a predecessor's row. Nothing was written; the caller declares again later.
pub(crate) fn declare_error(what: &str, e: sqlx::Error, lock_timeout_ms: u64) -> StoreError {
    match StoreError::lock_conflict_of(&e, |text| {
        format!(
            "{what} waited longer than its lock_timeout ({lock_timeout_ms} ms) for a row a staged cut part or \
             another transaction holds, or was chosen in a deadlock; nothing was written — retry once the cut is \
             decided ({text})"
        )
    }) {
        Some(lost) => lost,
        None => e.into(),
    }
}

/// The world row `FOR NO KEY UPDATE` (`StaleGeneration` unless at `generation`, when given),
/// in a coordinator transition whose compare-and-set already holds the `coordinator` row
/// (Elastic 2b Task 3: the row's version is the fence, no epoch is compared). Returns the
/// stored generation.
async fn lock_world_at(tx: &mut sqlx::PgConnection, generation: Option<u64>) -> Result<u64, DeclareError> {
    let stored: i64 = sqlx::query_scalar("SELECT generation FROM world WHERE id = 1 FOR NO KEY UPDATE")
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
    if let Some(mine) = generation {
        if stored as u64 != mine {
            return Err(StoreError::StaleGeneration { stored: stored as u64, mine }.into());
        }
    }
    Ok(stored as u64)
}

/// The database clock in ms (as `rows`' `NOW_MS` for Postgres: `clock_timestamp()`, not the
/// transaction's start — this one may have waited for locks).
pub(crate) const PG_NOW_MS: &str = "(extract(epoch from clock_timestamp()) * 1000)::bigint";

/// What a declaration's lock timeout names.
pub(crate) const DECLARING: &str = "a succession's declaration";
/// What a rollback's lock timeout names.
pub(crate) const ROLLING_BACK: &str = "a world rollback";

/// A declaration's arguments, checked before any statement: one heir per predecessor, each
/// predecessor once, no heir among them.
pub(crate) fn check_declare(dead: &[(HostId, u64)], heirs: &[HostId]) -> Result<BTreeSet<HostId>, StoreError> {
    let predecessors: BTreeSet<HostId> = dead.iter().map(|(o, _)| *o).collect();
    if dead.is_empty() || heirs.len() != dead.len() || predecessors.len() != dead.len() {
        return Err(StoreError::Unsupported(format!(
            "a succession needs one heir per predecessor and each predecessor once (dead {dead:?}, heirs {heirs:?})"
        )));
    }
    if let Some(h) = heirs.iter().find(|h| predecessors.contains(h)) {
        return Err(StoreError::Unsupported(format!("host {h} cannot inherit while it is declared dead itself")));
    }
    Ok(predecessors)
}

/// A declaration's statement error, kept raw so a lock timeout is named as one.
pub(crate) enum DeclareError {
    Store(StoreError),
    Sql(sqlx::Error),
}

impl DeclareError {
    pub(crate) fn into_store(self, what: &str, lock_timeout_ms: u64) -> StoreError {
        match self {
            DeclareError::Store(e) => e,
            DeclareError::Sql(e) => declare_error(what, e, lock_timeout_ms),
        }
    }
}

impl From<sqlx::Error> for DeclareError {
    fn from(e: sqlx::Error) -> Self {
        DeclareError::Sql(e)
    }
}

impl From<StoreError> for DeclareError {
    fn from(e: StoreError) -> Self {
        DeclareError::Store(e)
    }
}

/// The declaration's statements inside `tx` — a coordinator transition's, its `coordinator`
/// row held by the compare-and-set (Elastic 2b Task 3; see the module docs for the locks);
/// anything but `Done` is rolled back by the caller (to its savepoint). `lease_ms`: a row
/// lapsed past it ([`game::HOST_LEASE_MS`] unless a test shortened it).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn declare_in(
    tx: &mut sqlx::PgConnection,
    dead: &[(HostId, u64)],
    heirs: &[HostId],
    predecessors: &BTreeSet<HostId>,
    kind: SuccessionKind,
    generation: u64,
    lease_ms: u64,
) -> Result<Declared, DeclareError> {
    lock_world_at(&mut *tx, Some(generation)).await?;
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM successions WHERE settled_cut IS NULL AND abandoned = 0")
        .fetch_one(&mut *tx)
        .await?;
    if open > 0 {
        return Ok(Declared::SuccessionOpen);
    }
    // The predecessors' rows, locked in id order: a heartbeat waits for this transaction.
    let mut sorted: Vec<(HostId, u64)> = dead.to_vec();
    sorted.sort();
    let (mut gone, mut not_lapsed) = (Vec::new(), Vec::new());
    for (ordinal, boot) in &sorted {
        let row = sqlx::query(&format!(
            "SELECT boot, clean, state, seen_ms = 0 OR {PG_NOW_MS} - seen_ms > $2, \
             EXISTS (SELECT 1 FROM successions WHERE predecessor = $1 AND predecessor_boot = hosts.boot AND abandoned = 0) \
             FROM hosts WHERE id = $1 FOR NO KEY UPDATE"
        ))
        .bind(i((*ordinal).0))
        .bind(i(lease_ms))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            gone.push(*ordinal);
            continue;
        };
        let (stored_boot, clean, state, lapsed, succeeded): (i64, i64, i64, bool, bool) =
            (row.try_get(0)?, row.try_get(1)?, row.try_get(2)?, row.try_get(3)?, row.try_get(4)?);
        if stored_boot as u64 != *boot || state != HOST_LIVE || succeeded {
            gone.push(*ordinal);
            continue;
        }
        let ready = match kind {
            SuccessionKind::Crash => clean == 0 && lapsed,
            SuccessionKind::Leave => clean != 0,
        };
        if !ready {
            not_lapsed.push(*ordinal);
        }
    }
    // Each heir: live, dirty and beating (a vouched row, `seen_ms = 0`, is not fresh).
    for heir in heirs {
        let fresh: Option<bool> = sqlx::query_scalar(&format!(
            "SELECT seen_ms <> 0 AND {PG_NOW_MS} - seen_ms <= $2 FROM hosts \
             WHERE id = $1 AND clean = 0 AND state = {HOST_LIVE}"
        ))
        .bind(i(heir.0))
        .bind(i(lease_ms))
        .fetch_optional(&mut *tx)
        .await?;
        if fresh != Some(true) && !gone.contains(heir) {
            gone.push(*heir);
        }
    }
    if !gone.is_empty() {
        gone.sort();
        return Ok(Declared::Gone(gone));
    }
    if !not_lapsed.is_empty() {
        return Ok(Declared::NotLapsed(not_lapsed));
    }
    // Read under the world row's lock, which `register_host` takes: a host registering
    // later never met the dead boot; a live one the coordinator is not attached to is listed.
    // Only a host holding land (a `super_regions` or `in_flight` row) — or an heir of this
    // call — is a survivor (Elastic 2a, the crash-loop rule): a landless host exchanged no
    // state with the dead one, and its own death is ended, never a second failure.
    let heir_ids: Vec<i64> = heirs.iter().map(|h| i(h.0)).collect();
    let live: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT id FROM hosts h WHERE clean = 0 AND state = {HOST_LIVE} \
         AND (EXISTS (SELECT 1 FROM super_regions WHERE owner = h.id) \
              OR EXISTS (SELECT 1 FROM in_flight WHERE host = h.id) \
              OR h.id = ANY($1)) \
         ORDER BY id"
    ))
    .bind(&heir_ids)
    .fetch_all(&mut *tx)
    .await?;
    let mut survivors = Vec::with_capacity(live.len());
    for o in live {
        let o = u64::try_from(o).map(HostId).map_err(|_| StoreError::Corrupt(format!("hosts.id {o}")))?;
        if !predecessors.contains(&o) {
            survivors.push(o);
        }
    }
    let base_cut: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cut_id), 0) FROM cut_decisions WHERE committed = 1")
        .fetch_one(&mut *tx)
        .await?;
    let mut out = Vec::with_capacity(dead.len());
    for ((predecessor, boot), heir) in dead.iter().zip(heirs) {
        if kind == SuccessionKind::Crash {
            sqlx::query(&format!("UPDATE hosts SET state = {HOST_DEAD} WHERE id = $1 AND boot = $2"))
                .bind(i(predecessor.0))
                .bind(i(*boot))
                .execute(&mut *tx)
                .await?;
        }
        // Key order first (a claim upserts in key order too), then the claim itself.
        sqlx::query("SELECT 1 FROM super_regions WHERE owner = $1 ORDER BY sx, sz FOR NO KEY UPDATE")
            .bind(i(predecessor.0))
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(
            "UPDATE super_regions SET owner = $1, epoch = epoch + 1 WHERE owner = $2 RETURNING sx, sz, epoch",
        )
        .bind(i(heir.0))
        .bind(i(predecessor.0))
        .fetch_all(&mut *tx)
        .await?;
        let mut moved = Vec::with_capacity(rows.len());
        for r in rows {
            let (sx, sz, epoch): (i64, i64, i64) = (r.try_get(0)?, r.try_get(1)?, r.try_get(2)?);
            let sr = (
                i32::try_from(sx).map_err(|_| StoreError::Corrupt(format!("super_regions.sx {sx}")))?,
                i32::try_from(sz).map_err(|_| StoreError::Corrupt(format!("super_regions.sz {sz}")))?,
            );
            moved.push((sr, epoch as u64));
        }
        moved.sort();
        let id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) + 1 FROM successions")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO successions (id, predecessor, predecessor_boot, heir, kind, base_cut, settled_cut, abandoned, \
             declared_utc, survivors, moved) VALUES ($1, $2, $3, $4, $5, $6, NULL, 0, $7, $8, $9)",
        )
        .bind(id)
        .bind(i(predecessor.0))
        .bind(i(*boot))
        .bind(i(heir.0))
        .bind(kind.code())
        .bind(base_cut)
        .bind(crate::open::utc_now())
        .bind(encode_survivors(&survivors))
        .bind(encode_moved(&moved))
        .execute(&mut *tx)
        .await?;
        log::info!(
            "[world-store] succession {id}: host {predecessor} (boot {boot}, {kind:?}) → heir {heir}, {} super-regions, \
             base cut {base_cut}, survivors {survivors:?}",
            moved.len()
        );
        out.push(Succession {
            id: SuccessionId(id as u64),
            predecessor: *predecessor,
            predecessor_boot: *boot,
            heir: *heir,
            kind,
            base_cut: CutId(base_cut as u64),
            moved,
            survivors: survivors.clone(),
        });
    }
    Ok(Declared::Done(out))
}

/// What ending a landless host's lock timeout names.
pub(crate) const ENDING: &str = "ending a landless host";

/// Ending a landless host (Elastic 2a; `crate::StoreHandle::end_landless_host`), its
/// statements inside `tx` — a coordinator transition's, its `coordinator` row held by the
/// compare-and-set (see the module docs for the locks); anything but `Ended` is rolled back by
/// the caller (to its savepoint).
pub(crate) async fn end_landless_in(
    tx: &mut sqlx::PgConnection,
    host: HostId,
    boot: u64,
    lease_ms: u64,
) -> Result<LandlessEnd, DeclareError> {
    // The declaration's prefix: the world row (no generation compared: the generation is not
    // touched — a host of an older one is refused anyway).
    lock_world_at(&mut *tx, None).await?;
    // The host's row: a heartbeat waits for this transaction, so a beat between the lapse check
    // and the update cannot be lost; a claim's `check_live` (`FOR SHARE`, its last statement)
    // waits too and then finds the row dead — the zombie's claim is refused.
    let row = sqlx::query(&format!(
        "SELECT boot, clean, state, seen_ms = 0 OR {PG_NOW_MS} - seen_ms > $2 FROM hosts WHERE id = $1 FOR NO KEY UPDATE"
    ))
    .bind(i(host.0))
    .bind(i(lease_ms))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else { return Ok(LandlessEnd::Gone) };
    let (stored_boot, clean, state, lapsed): (i64, i64, i64, bool) =
        (row.try_get(0)?, row.try_get(1)?, row.try_get(2)?, row.try_get(3)?);
    if stored_boot as u64 != boot || state != HOST_LIVE || clean != 0 {
        return Ok(LandlessEnd::Gone);
    }
    if !lapsed {
        return Ok(LandlessEnd::NotLapsed);
    }
    // Plain reads (no lock): a claim that commits its land before this read is seen here; one
    // that would commit after waits at its `check_live` for this transaction and is refused.
    let owns: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM super_regions WHERE owner = $1) OR EXISTS (SELECT 1 FROM in_flight WHERE host = $1)",
    )
    .bind(i(host.0))
    .fetch_one(&mut *tx)
    .await?;
    if owns {
        return Ok(LandlessEnd::OwnsLand);
    }
    // Read under the world row's lock, which every writer of `successions` takes first.
    let named: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM successions WHERE settled_cut IS NULL AND abandoned = 0 \
         AND (heir = $1 OR (',' || survivors || ',') LIKE ('%,' || CAST($1 AS TEXT) || ',%')))",
    )
    .bind(i(host.0))
    .fetch_one(&mut *tx)
    .await?;
    if named {
        return Ok(LandlessEnd::Recipient);
    }
    sqlx::query(&format!("UPDATE hosts SET state = {HOST_DEAD} WHERE id = $1 AND boot = $2"))
        .bind(i(host.0))
        .bind(i(boot))
        .execute(&mut *tx)
        .await?;
    Ok(LandlessEnd::Ended)
}

/// The coordinator's world rollback (`crate::StoreHandle::rollback_world`) inside `tx` — a
/// coordinator transition's, its `coordinator` row held by the compare-and-set — under the
/// world row's lock (the declaration's order), [`rollback_in`] — only from generation `from`
/// (the one the coordinator decided the rollback at, Elastic 2a): a world past it was rolled
/// back already (this rollback committed before its coordinator could record it, or the
/// operator's), nothing is written (`None`) and its generation is the answer. Idempotent.
/// Returns the world's generation after it, and the successions it abandoned when it wrote.
pub(crate) async fn rollback_world_in(tx: &mut sqlx::PgConnection, from: u64) -> Result<(u64, Option<u64>), DeclareError> {
    let stored = lock_world_at(&mut *tx, None).await?;
    if stored != from {
        log::warn!("[world-store] the world is at generation {stored}, not {from}: rolled back already, nothing written");
        return Ok((stored, None));
    }
    let (generation, abandoned) = rollback_in(&mut *tx, stored).await?;
    log::warn!("[world-store] world rolled back to generation {generation} (with its coordinator's transition); {abandoned} open successions abandoned");
    Ok((generation, Some(abandoned)))
}

/// The world row `FOR NO KEY UPDATE` (the lock every declaration, rollback, registration and
/// decision takes); its generation.
pub(crate) async fn lock_world(tx: &mut sqlx::PgConnection) -> Result<u64, DeclareError> {
    let stored: i64 = sqlx::query_scalar("SELECT generation FROM world WHERE id = 1 FOR NO KEY UPDATE")
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
    Ok(stored as u64)
}

/// A world rollback inside `tx` (the world row locked, at `stored`): `world.generation + 1`,
/// every open succession abandoned, and each abandoned one's predecessor in-flight rows
/// re-keyed to its heir — the land stays with the heir, which restarts from the last
/// committed cut owning it, so the rows of that cut go with it (the module docs). Returns
/// `(generation, successions abandoned)`.
pub(crate) async fn rollback_in(tx: &mut sqlx::PgConnection, stored: u64) -> Result<(u64, u64), DeclareError> {
    let generation = stored + 1;
    sqlx::query("UPDATE world SET generation = $1 WHERE id = 1").bind(i(generation)).execute(&mut *tx).await?;
    // Ordered by id: a predecessor's rows end with the heir of its LAST open succession (one
    // predecessor has at most one open succession: its row is dead, or clean, after it).
    let abandoned: Vec<(i64, i64, i64)> = sqlx::query_as(
        "UPDATE successions SET abandoned = 1 WHERE settled_cut IS NULL AND abandoned = 0 \
         RETURNING id, predecessor, heir",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut abandoned = abandoned;
    abandoned.sort();
    for (id, predecessor, heir) in &abandoned {
        // `seq` keeps its range (the predecessor's): the key is `seq` alone, never shared.
        let moved = sqlx::query("UPDATE in_flight SET host = $1 WHERE host = $2")
            .bind(heir)
            .bind(predecessor)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if moved > 0 {
            log::warn!(
                "[world-store] succession {id} abandoned: host {predecessor}'s {moved} in-flight row(s) go to its heir {heir} \
                 with its land"
            );
        }
    }
    Ok((generation, abandoned.len() as u64))
}

/// The ids of the open successions, in `tx` (no lock: `decide_in` reads them under the
/// world row's lock, which every declaration and rollback takes first).
pub(crate) async fn open_ids(tx: &mut sqlx::PgConnection) -> Result<BTreeSet<u64>, StoreError> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM successions WHERE settled_cut IS NULL AND abandoned = 0")
        .fetch_all(&mut *tx)
        .await?;
    Ok(ids.into_iter().map(|i| i as u64).collect())
}

/// Settles `settles` with `cut` (inside the committing decision's transaction): each
/// gets `settled_cut`, and each predecessor's row — of the boot that died — turns clean.
pub(crate) async fn settle(tx: &mut sqlx::PgConnection, cut: CutId, settles: &[SuccessionId]) -> Result<(), StoreError> {
    if settles.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = settles.iter().map(|s| s.0 as i64).collect();
    sqlx::query("UPDATE successions SET settled_cut = $1 WHERE id = ANY($2) AND settled_cut IS NULL AND abandoned = 0")
        .bind(cut.0 as i64)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE hosts SET clean = 1 FROM successions s \
         WHERE s.id = ANY($1) AND hosts.id = s.predecessor AND hosts.boot = s.predecessor_boot",
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

