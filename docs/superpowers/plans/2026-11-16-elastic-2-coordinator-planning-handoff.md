# Handoff: Elastic 2 — the coordinator without a leader

**Date:** 2026-11-16
**For:** a fresh session. E1 is built on `develop` (last commit of E1: `300419f0`).
**Job:** write the E2 implementation plan (`superpowers:writing-plans`) as
`docs/superpowers/plans/<date>-elastic-2-coordinator-without-a-leader.md`, have Dawid look at it, then execute
it subagent-driven. One plan, shipping working software; E3–E6 get their own handoffs after it.

**The decisions in §4 are taken** (Dawid, 2026-11-16: "you can make the decisions on open questions"). They
were made from the E1 execution record, not from a fresh reading of every file: verify each against the code
while planning, and where the code contradicts one, re-rule it in the plan under "What the handoff got wrong"
— do not stall on it.

## 1. Read first, in this order

1. `CLAUDE.md` — the rules. Its "Server / store" section was rewritten for E1 and is current.
2. `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md` — decisions 7 and 9, §3, §6, §10.
3. `docs/superpowers/plans/2026-11-14-elastic-region-hosts-planning-handoff.md` — §3 "E2", §4 (rules that
   carry over), §5 (decisions not to reopen).
4. `docs/superpowers/plans/2026-11-15-elastic-1-hosts-as-deployments.md` — only "As built (2026-11-16)" and
   "Whole-branch review and fix wave (2026-11-16)" at its end: what E1 really built, every ruling, every
   deferred finding.
5. `TODO.md` — the "Elastic 1" block: known limits, hand checks not yet run, the open decision this handoff
   settles (§4.1).
6. The E1 workspace, while it exists (git-ignored): `.superpowers/sdd/2026-11-15-elastic-1-hosts-as-deployments/`
   — `progress.md` (the ledger), `final-findings-{A,B,C}.md` (the three whole-branch reviews),
   `task-11-report.md` / `task-12-report.md` ("What the world panel must change").

## 2. Where things stand

- **Hosts:** an id issued at registration (`game::HostId`, `world.next_host_id`, never reused); a restart is a
  new host that owns nothing; peers discovered from `hosts`; a new host may register during a cut; warned
  drain (`worldctl drain`, `drained` state); one Deployment + NodePort Service + PodDisruptionBudget per key of
  `var.hosts`. No ordinal, no host count anywhere.
- **The coordinator today:** ONE process (`deploy/modules/world/coordinator.tf`, `replicas = 1`, `Recreate`).
  Durable: the `coordinator` row (`epoch`, `state` = `CoordinatorState { history, claims, memo }`), every
  save fenced on the epoch; cut decisions (`cut_decisions`, `cut_begins`), `successions`, `hosts` — all
  already transactions under the world-row lock. In memory only, rebuilt from each host's `Attach`:
  `online_on`, `region_host`, `hop_of`, the attached set and cut state (`coordinator/world_cut.rs`), the
  lease watch (`LeaseWatch`), `land_wanted`, the login tally and the held `SessionEnded` queue.
- **Links:** protocol 7; every hello carries the world id (E1 did this — it is NOT E2 work any more). The
  login service dials ONE address per world (`worlds.host_link`), now the coordinator's
  (`VOXEL_LOGIN_LINK_BIND`, Secret `login-link-identity`). The coordinator relays each login order to the
  *gate host* (lowest attached, live, non-draining, not-leaving id) and keeps no per-order state.
- **Verified at the end of E1** (on a clean export of the committed code): `test-r2` 16/16, `test-r3` 44/44,
  `test-r4` 24/24, `test-threads` 284 + 3 + 10, game `it` 926, store and server suites green,
  `twin-check` PASS (448 s). Known reds, not ours: three `lod::tests` in the game lib,
  `codec::golden_blobs_decode`, flaky SQLite `stud_refusal::…`.
- **Never run:** the client tests E1 added, any live dev stack, a player on the twin, anything on orion.
  `go-nogo-r3` is not valid as built (§4.4).

## 3. What E2 delivers

Several coordinator replicas, any of which serves any request; losing one loses nothing and stops nothing
for longer than a reconnect. Concretely, the E1 limits it removes: "no cuts, no takeover and no logins while
the coordinator is down", "one gate host at a time", and (§4.1) "a crash-looping host pod can roll the world
back".

Scope (the parent handoff's list, corrected for what E1 already did):

- Every coordinator decision is a compare-and-set transaction in PostgreSQL that bumps an epoch the store
  checks. Cut decisions, successions and host registration already are; history, claims, the request memo
  and everything rebuilt from `Attach` are not.
- Timed work (start a cut, watch leases, the calendar) is "claim it if it is due": any replica tries, one wins.
- Hosts and the login service reach "a coordinator" through a Service; a request may land on any replica.
- ~~The world id on every link~~ — done in E1.

## 4. Decisions taken

### 4.1 The crash-loop rule (E1's open decision) — first task of E2

Today a restarted host is at once a recipient (survivor or heir) of its predecessor's succession, and a
recipient that dies or does not answer within `SUCCESSION_DEADLINE_MS` is "a second failure before the
settle": the world generation is raised. One flapping host pod therefore rolls the whole world back on
every second restart.

**Decision:** a succession's recipients are the hosts that were **attached to a coordinator when it was
declared**. A host that registers later, or that never attached, is not a recipient: it owes no report (it
cannot have exchanged anything with a host that was already dead), its death is not a second failure, and
its own death is its own ordinary succession (of a host that owns nothing, unless it had claimed since).
The heir is chosen among recipients (E1's rule: prefer hosts that take logins). If there is no recipient at
all, nothing is declared and the next registration's total-crash rule applies, as today.

The store's `declare` lists "every dirty live row" as survivors today (`world-store/src/succession.rs`); the
attached set must become something the store can check (this falls out of §4.2: attachments are rows).
Conservation tests: a pod that crash-loops beside a healthy host never raises the generation; a host that
registered after the death and DID take a hand-off before the settle is handled exactly once (decide whether
such a host must then report — it has a journal with survivors, not with the dead host).

### 4.2 Which state becomes rows

- **History, claims:** already in the `coordinator` row under its epoch. Keep one row; every mutation is
  read–modify–write under the epoch (a replica that loses re-reads and retries). Split the row only if
  measurement shows contention — not on speculation.
- **The request memo:** moves out of the blob into its own table keyed by `RequestId` (host id, seq), written
  in the same transaction as the state change it answers; `Settled` deletes below a sequence. A resend that
  lands on another replica finds the row.
- **Attachments:** a table `coordinator_attachments (host, boot, replica, attached_ms)`, upserted when a
  host's `Attach` is accepted and deleted on a known detach; a row whose replica is gone is ignored by its
  age against the replica's own liveness (below). This is what makes "attached" checkable by the store
  (§4.1) and lets any replica compute the gate host and a cut's participants.
- **What a host's `Attach` rebuilds** (`online_on`, `region_host`, `hop_of`, people lists): stays in memory,
  **per replica, for the hosts attached to that replica**. A decision that needs the whole world's view
  (embark placement, "where is this player") reads it from rows or asks the owning replica through the
  database — the plan must list each such decision and say which. "Wait, never guess" still holds: a replica
  that cannot see what it needs answers "not yet", never a guess.
- **Replicas' own liveness:** a small `coordinator_replicas` lease (as `hosts.seen_ms`: `clock_timestamp()`,
  beaten from the main loop, never a timer thread). Nothing is fenced on it; it only lets others ignore a
  dead replica's attachment rows and duty claims.

### 4.3 Timed work, cuts, and two replicas answering

- **Duties:** one row per duty (`start_cut`, `watch_leases`, `calendar`, `settle`) with `due_ms`, `holder`,
  `epoch`. Any replica whose loop sees a due row tries a compare-and-set; the winner does the work and writes
  the next `due_ms` in the same transaction as the work's own decision. No Kubernetes Lease (design
  decision 7).
- **A world cut across replicas:** the cut becomes a row-driven state machine (`cut_begins` already records
  the begin; add the phase). The replica that wins `start_cut` records the begin and the participant list
  from the attachment rows; **every replica relays `BeginCut` / `CutDone` to the hosts attached to itself**,
  driven by the row (LISTEN/NOTIFY with the polling backstop the terrain listener already uses); a host's
  `CutPrepared` / `CutFailed` goes to its own replica, which records it; whichever replica sees every part
  prepared calls `decide_cut` (already one transaction; a second caller finds it decided).
- **Lease watch across replicas:** "ten seconds of this coordinator's own unchanged reads" becomes
  "unchanged since a read recorded in the duty row": the watcher's observations must survive a hand-over of
  the duty, or the lease is re-counted from zero by the new holder (conservative: a hand-over delays a
  declaration by one lease, never hastens it). Pick the conservative form unless it costs more than a lease.
- **Two replicas answering the same request:** every reply carries the `RequestId`; a host keeps the first
  and drops later ones for the same id. Both replies are the same by construction (the memo row is written
  before either is sent). A host's re-`Attach` after a reconnect may land on another replica: it must be
  exactly what a re-`Attach` to a restarted coordinator is today.
- **Lock matrix:** every new statement gets its walk in the module notes of
  `world-store/src/succession.rs`; a prepared transaction is not a waiting backend; the heartbeat and the
  drain mark stay one-row statements that want nothing else.

### 4.4 The login link with several replicas

The login service keeps **one link per coordinator replica**: the world's registered link is a headless
Service name; `svc_link::LinkTarget` already resolves at every dial — resolve to all addresses and hold a
link to each. An order goes out on one link; that replica relays it to a gate host **among the hosts
attached to it**; if it has none it says so at once and login tries the next link. Answers
(`SpawnAck`, `KickAck`, `SessionEnded`) come up the answering host's own replica, which forwards them on its
own login link; login already matches acks by `(token, epoch)`, so which link they arrive on does not matter.
Two gates on two replicas placing the same order is the existing `Repeat` case. No table, no replica-to-
replica channel. This changes `crates/login/src/host_link.rs` (one link → several per world) and nothing in
the login database.

### 4.5 `go-nogo-r3` — repaired as a task of E2, before anything changes who decides ownership

The probe (`crates/client/src/renderer/autopilot.rs`) lays out seams as if host ids were 0 and 1; a fresh
world issues 1 and 2, so thread and host seams are mislabelled. Fix: the autopilot picks its seam from the
client's own region → host map (`pick_loaded_seam`, the connections it holds), never from a recomputed
rendezvous; the recipe drops and re-forges its database every run; `scripts/crossing_summary.py` scans for
successions only inside the probe window. It cannot be validated without a live run: the task ends with the
exact command for Dawid and what a valid first run prints.

### 4.6 A host added after a release

No module change in E2. The documented steps stand (apply, then set the new host's image to the
coordinator's). E4's reconciler creates host Deployments itself and copies the coordinator's image, which
closes the window for good.

### 4.7 E1's four planning defaults

Confirmed as built: after a total crash the first host back takes all claimed land (E3 releases idle land);
`static` ownership is for one host; `DRAIN_WARNING_SECS` = 300 with nothing blocked; a node port pair per
host Service.

### 4.8 The world panel — last task of E2

Dawid: the panel is adapted last, since it must follow the coordinator change too. E2's final task (before
docs) updates `crates/panel` and `deploy/modules/panel` for **both** shapes at once: host Deployments by
label (E1's list in "As built" → "What the world panel must change") and several coordinator replicas
(start / stop order, readiness, what "the coordinator" means in its views). `panel/src/ops.rs` mirrors
`world-start` / `world-stop` step for step — change the recipes and the panel in the same task. **Before
that task, check `git status`: the panel was still uncommitted, in-progress work on 2026-11-16** (untracked
`crates/panel/`, `deploy/modules/panel/`, `deploy/envs/*/panel/`, `deploy/modules/world/panel.tf`; hunks in
`justfile`, `deploy/modules/world/variables.tf`, both world roots). If it is still uncommitted then, ask
Dawid to commit it first — do not edit someone's untracked work.

### 4.9 Deployment shape

`coordinator` becomes N replicas (start with 2 on the twin and on orion) behind a **headless** Service (hosts
and login need every address, and a dial must find a new pod within the DNS TTL); strategy stays `Recreate`
only if a mixed-build pair of replicas cannot be allowed — it cannot (no version handshake): a release is
still stop → replace images → start, and the recipes scale every coordinator replica together. One identity
Secret for all replicas (the pin is per Service name, not per pod). A PodDisruptionBudget for the coordinator
(`min_available = 1`) so a node drain cannot take the last replica while hosts run — this also closes E1's
one-node drain hazard for multi-node clusters; on one node the documented order (`world-stop`, then drain)
stays.

## 5. E1 follow-ups: what E2 takes, what it leaves

**E2 takes** (they sit in code E2 rewrites):
- the crash-loop rule (§4.1);
- the coordinator never forgets a host id in memory (attachments become rows; in-memory maps are per replica
  and rebuilt);
- `cut_begins` rows are never pruned → prune with `cut_decisions` when the cut state machine gets its phase;
- offline `worldctl retire-host` writes a succession the next total crash always abandons → with §4.1 decide:
  refuse offline retirement (the total-crash rule already does its job), keep the online vouch;
- the gate may be a `static` host without a service token → a replica never picks a host that did not
  present the token (it cannot attach without it under `claim`; state the `static` case);
- `/readyz` is 200 at registration, before attach → with replicas, "ready" for a host means attached to some
  replica; decide and test.

**E2 leaves** (documented in `TODO.md`; each is small and independent — do one only if its file is already
open for another reason): a committed drain exiting 3 in a sub-second window (A-M2); land whose directory
owner is a forgotten host (B-M1) and the landless-prune residual of B-I1; a refused forget not retried
(B-M2); the gate's kick reaching only peers it has a session with (B-M4); the dead "another boot of the same
id" branches (the post-E1 clean-up); `claims::live_hosts` without a generation filter; the admission
cache-miss read on the router thread; the login 503 for unlinked rows; a process-level test of
`server::run`'s drained branch; lockstep-vs-shuffled comparisons for drain / gate / forget.

## 6. Rules that carry over (unchanged from the parent handoff §4, plus what E1 taught)

- **Conservation beats latency; wait, never guess.** A task without a conservation test is not done.
- **Fencing in PostgreSQL is the only safety.** Nothing a Deployment, a Service, a replica count or a duty
  row does may be what prevents two writers. Never a store write that skips the fence.
- Migrations are immutable, identical in shape in `migrations/pg` and `migrations/sqlite`; the next is
  `0016`. **Existing worlds and world databases are wiped, never migrated** (Dawid, 2026-11-15): write the
  schema migration, no data path; say "re-forge / `just pg-reset`" in the docs.
- Wire enums are append-only; link enums change only with ONE `LINK_PROTOCOL_VERSION` bump per plan (it is 7).
- A new per-host `WorldManager` field is classified in `host_fields!` or it does not compile. A host id is
  never reused and nothing is derived from its number.
- Scheduling-sensitive correctness goes in the seeded layer (`crates/game/tests/peer_schedule.rs`,
  `common/multi_rig.rs`); the rig will need several coordinators — plan that as its own early task, the way
  E1's Task 3 rebuilt the rig for ids. Real hosts over real links: `crates/server/tests/hosts.rs`,
  `takeover.rs` (in-process; no test spawns the binary).
- **Ask Dawid first** before client tests, live runs, measurements and every apply on orion. `just twin-check`
  is allowed without asking (it starts no client); it is the only place the real binary runs.
- After a wide change run each touched crate's whole non-client suite once; `just smoke` after every Rust task.

## 7. How E1 was executed — keep what worked

- Subagent-driven: one implementer and one reviewer per task, a ledger in `.superpowers/sdd/<plan>/`,
  rulings recorded as `Ruling: what — why — cost if wrong`, one whole-branch review at the end split into
  area reviewers (the range was too large for one), one fix wave, one scoped re-review.
- **Plan defects found in execution were the expensive part** (five in E1: an ordering inside one
  transaction, a rule specified only as a guard, an alert's operator precedence, a type living in another
  crate, a test level that does not exist in this repo). For E2 the risk is §4.2–4.3: write the lock walk and
  the "which replica sees what" table INTO the plan, per decision, before the tasks.
- **Dawid works in this checkout at the same time**, with uncommitted hunks in files a task edits
  (`CLAUDE.md`, `TODO.md`, `justfile`, `deploy/**`): every dispatch must carry the filtered-staging rule
  (`git diff -U3`, keep own hunks, `git apply --cached --recount`; never `git add -A`, `git stash`,
  `--unidiff-zero`), and the controller verifies each commit's stat and that his hunks are still uncommitted.
- Other sessions run here too. One ran `rustfmt` over the whole workspace mid-wave (600 files, the vendored
  forks included; reverted 2026-11-16, backup `.superpowers/rustfmt-revert-backup-2026-11-16.patch`). When a
  subagent reports the tree does not build, check `git status` for foreign changes before blaming the task;
  suites can be run on a `git archive` export of the commit.
- Subagents edit with the Edit / Write tools, one file per call — a shell-scripted multi-file rewrite was
  refused by the permission system once; never work around a refusal, report it.
- Commit trailers: each subagent's own `Co-Authored-By` line for its model, plus the session line the
  harness gives.
- The dev PostgreSQL container (`just pg-up`, 127.0.0.1:55432) must be up for `test-r2|r3|r4`, `test-pg`;
  PostgreSQL-backed suites one at a time.

## 8. Sizing

Medium to large. Roughly: the rig with several coordinators; the memo table; attachments and replica
liveness; duties; the cut state machine across replicas; the lease watch hand-over; the crash-loop rule; the
login service's several links; the module, recipes and twin for N replicas; `go-nogo-r3`; the panel; docs.
Expect 12–15 tasks. Order them so the world stays playable with ONE replica after every task and the second
replica is switched on only when the last correctness task has landed.

## 9. Open, for Dawid (the plan proceeds on these defaults)

- Two coordinator replicas on orion's single node buy tolerance of a coordinator crash or restart, not of the
  node failing; the default is 2 anyway. Say if one replica should stay the orion default until a second node
  exists.
- Refusing offline `worldctl retire-host` (§5): it was added in R4 as the way out of a host that never
  returns; with ids and the total-crash rule it has no remaining use.
