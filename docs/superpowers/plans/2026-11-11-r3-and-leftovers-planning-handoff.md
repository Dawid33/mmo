# Handoff: plan R3 (many hosts on one machine) and the R2b cleanups

**Date:** 2026-11-11
**For:** a fresh session.
**Job:** **write implementation plans**, not code. Use the `superpowers:writing-plans` skill for each plan.

**Outputs:**
1. `docs/superpowers/plans/2026-11-xx-r3-multi-host.md` (the main deliverable).
2. `docs/superpowers/plans/2026-11-xx-r2b-cleanups.md` (small; it may run before R3 to make R3 easier, so say which order you recommend).

Use the date you write them on. Commit nothing unless the user asks.

## 1. Where things stand (verified 2026-11-11 at `0d963a20`)

Done and committed:
- **World store** on SQLite/Postgres: `crates/world-store`, portable regions, world-tick save cuts, LOD and halo from stored regions (plan `2026-11-07-world-store-r0-r1.md`).
- **Forged world as store tiles:** an LRU tile cache, the stored `WorldMap`, and `worldctl forge|export|retile` (plan `2026-11-07-forged-world-in-db.md`).
- **R2a:** the login service (`crates/login`, `auth`, `login-api`), JWT join tickets, client login screens, local-name identity (plan `2026-11-07-r2a-login-accounts.md`).
- **R2b:**
  - `VOXEL_ROLE=all|host|coordinator`, with `game::coordinator` and the `svc-link` QUIC service link;
  - owner epochs and fenced writes in Postgres;
  - login pushes `SpawnPlayer` / `Kick` to the host;
  - `just dev-r2`, `just test-r2`.

  Plan: `2026-11-07-r2b-role-split.md`.
- **Stud world** replaced the voxel world entirely (P1–P10, plan `2026-11-09-stud-world-replacement.md`). Terrain is stud chunks; old voxel worlds are refused at store open.

Not done: R3 onwards (below). None of the R2a, R2b or stud-world live checks has been run by the user yet.

## 2. Read first

1. `CLAUDE.md`. It was pruned on 2026-11-08; the old detail is in `docs/claude-md-full-reference-2026-11-08.md`, which you can grep.
2. `TODO.md`, especially:
   - the **R2b paragraph and its "R3 must pick up" line** (the R3 work list);
   - "R2b minors" (the cleanup list);
   - the stud-world "Open" list (for context only).
3. `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0: the settled design.
   - §0.4: roles, ownership policy behind a trait, fencing always in Postgres, client multi-connection.
   - §0.5 R3: the go/no-go (conservation holds, and client corrections at a cross-host flip match a cross-thread flip at p95 under 1–5 ms of injected latency).
   - §0.9f: LISTEN/NOTIFY (direct unpooled listener, re-sync by `terrain_version > last_seen`, notify only from debounced terrain writes).
   - §0.10: final decisions.
   - §8: LOD across instances.
4. `docs/superpowers/plans/2026-11-07-r2b-role-split.md`: its decisions, Review Focus and the shape of `WorldLink` / `HostMsg` / `CoordMsg`. R3 extends exactly these.
5. `docs/superpowers/plans/2026-11-06-seam-transparent-regions.md`: R3's host↔host link carries *this* seam protocol (stamped transfers applied at T+1, halo deltas, intents, `HaloSourceGone`, the `in_flight` table and its conservation invariant). No region ever waits for another.

## 3. Decisions already made (do not re-open)

- **Seamless crossing only.** If R3's go/no-go misses, the work is to fix the seam; there is no static-border fallback.
- **Fixed super-regions are the claim unit:** S = 16, `VOXEL_SUPER_REGION_REGIONS`, `game::ownership::super_region_of`.
- **Ownership is a policy behind a trait** (`VOXEL_OWNERSHIP=allinone|static` exists). Fencing is always the Postgres owner epoch, and no store write may skip the fence.
- **One world per deployment, one logical database per world.** Split roles need Postgres.
- **Development:** compose / `just` recipes launch N hosts. **Production:** a k8s StatefulSet (R5, not this plan).
- **Deferred, do not plan:** rate limiting (§0.11), Postgres backups, node autoscaling. R4 (on-demand claiming `PgClaim`, failover, drain), R5 (k8s, the panel, GDPR) and R6 (pod autoscaling) are later plans: mention them only as extension points.
- Link rules from R2b (CLAUDE.md "Link rules"):
  - a re-`Attach` after any reconnect rebuilds the coordinator's view;
  - requests are memoised by `RequestId`;
  - "wait, never guess" (conservation beats latency);
  - kicks are durable until acked.

## 4. R3 scope: what the plan must cover

The deferred list in `TODO.md` ("R3 must pick up"), turned into tasks. Verify each item in code first; line numbers drift.

1. **The host↔host link over `svc-link`:**
   - departures, halo deltas, `HaloSourceGone`, intents and replies, and forwarded inputs between hosts;
   - crossings into another host's region (today they wait as `in_flight` rows forever);
   - re-delivery and conservation across a host link drop and reconnect.
2. **Multi-host save cuts coordinated by the coordinator:**
   - one world tick T across hosts;
   - each host's cut commit stays fenced on its own epoch;
   - the in-flight rule across hosts (each host's per-host in-flight rows already exist).
   - Decide whether one cut is one transaction per host plus a coordinator-recorded cut id, or something else, and prove no duplicate or loss on a crash between host commits.
3. **Per-ordinal placement and the guard:**
   - lift "the coordinator serves only host ordinal 0" (not sticky across a coordinator restart);
   - `Routes::admit` admitting two live peers with one ordinal;
   - login and the coordinator place a player on the owning host.
4. **Client multi-connection** (§0.4f):
   - one connection per host owning a region of the client's window;
   - a `region → connection` routing table;
   - a `RegionElsewhere` redirect carrying a ticket;
   - only the home connection carries input.

   Native and browser. This is the largest client change; size it honestly.
5. **LISTEN/NOTIFY** for terrain changes between hosts (and a polling fallback), driving LOD and halo invalidation of foreign regions (§8 case 2).
6. **The R2b correctness gaps listed under R3 in `TODO.md`:**
   - `restore_player` checks only local players (a dropped `PlayerOnline` claim goes unanswered; a host may keep a `fortress_of` the coordinator lacks);
   - the coordinator memo is capped at 4096 (a resend may re-execute; a claim leaks);
   - moves and parks into foreign or unavailable regions park without `owns()` (repeated "stuck" cuts instead of fencing; `world_manager.rs` ~1073, ~2755, ~2948–2964);
   - the store thread decodes inline on one current-thread runtime: **measure before changing; ask first**.
7. **Dev launch and tests:**
   - `just dev-r3` (login + coordinator + N hosts under `Static` ownership; the count is a variable);
   - a seeded-jitter layer for cross-host scheduling, like `handoff_schedule.rs`;
   - real-process tests like `test-r2`;
   - an injected per-link latency knob for the go/no-go measurement.

   **The measurement itself is a benchmark: ask the user before running it.**

The plan must state the go/no-go test as a concrete task: conservation, plus correction p95 at a cross-host flip vs a cross-thread flip at 1, 2 and 5 ms of injected delay.

**Design questions to settle in the plan.** Give the trade-offs in prose before the options, and ask the user only what is genuinely theirs:
- the host↔host topology (full mesh among neighbours vs via the coordinator);
- the multi-host cut protocol;
- how the client learns about foreign hosts (redirect vs the home host proxying a ticket);
- NOTIFY vs polling as the primary mechanism;
- whether R3 needs the browser multi-connection at once, or native first.

## 5. The cleanups plan (small)

From "R2b minors" in `TODO.md`. Each is one task:
- split `crates/game/src/world_manager.rs` (4,313 lines) — its world-link half goes into `world_manager/world_link.rs` (pure move, no behaviour change);
- cfg-gate the `crash_before_claim_reply` / `with_core` test hooks that sit in the production loop;
- one shared Postgres test-support crate or module replacing the 4 copies of `PgDb`;
- make `pending_kicks` rows expire, and stop the every-2-s warning for an unusable `host_link` pin with pending kicks;
- decide (and plan) the jwt-host-without-service-token fallback (today: newest-epoch admission with a WARN only) and the coordinator envelope-version mismatch (fatal today).

Recommend running this plan before R3, since R3 edits the same `world_manager.rs`.

## 6. Not for the plans: the user's own items (list them in your final report, don't plan them)

- Live checks:
  - R2a (`TODO.md` "R2a live check");
  - R2b (`just dev-r2`, kill the coordinator, restart the host);
  - the stud world's P10a play checklist (`plans/2026-11-09-stud-world-replacement.md` Status).
- Measurements: memory per host on a 100 km world, tile sizes, and the slowdown of tiled reads (Plan B's "left measurable only by a run").
- Old voxel-era worlds still under `worlds/` (`island-3273`, `sweep64`, `world-196a`, `world-72e2`, `world-b42f`, probably `default.sqlite`) are refused at open. Offer to delete them; **never delete without the user's yes.**
- Stud-world open items (`TODO.md` "Stud world — Open"). They are not part of R3.
- Native QUIC skips certificate verification while sending a join JWT. R5 fixes it (cert pin from the ticket or a real cert); mention it in R3 only if multi-connection makes it worse.

## 7. How to work

- **Workspace:**
  - `develop`, the main checkout, **no git worktrees**, including for subagents.
  - The user has concurrent uncommitted work: never `git add -A`.
- **Code surveys:**
  - Use Explore agents for the surveys: the world link, the coordinator, svc-link, ownership, the client netcode and session.
  - Keep only the conclusions, and verify any claim before it goes into a plan.
- **Plans follow the writing-plans skill:**
  - a header, Global Constraints (copy the R2b plan's, then update them), and a Review Focus;
  - tasks with Files / Interfaces / test-first steps / exact `just` commands, followed by a self-review.
- **Verification commands in the plans:**
  - `just test-one`, `just smoke`, `just test-threads`, `just test-r2` (and the new R3 recipe), `just determinism-wasm` where serialized sim types change;
  - never whole suites;
  - client tests are a slow opt-in.
- **Final report:** the two plan paths, the design questions you settled and how, those left for the user, the user-only items of §6, and the execution choice per the skill.
