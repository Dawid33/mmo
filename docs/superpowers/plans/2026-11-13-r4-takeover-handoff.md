# Handoff: R4 — takeover (a host fails over alone, on-demand claiming, drain)

**Date:** 2026-11-13
**For:** a fresh session.
**Job:** write the R4 implementation plan (`superpowers:writing-plans`), have Dawid review it, then
execute it subagent-driven (`superpowers:subagent-driven-development`). R4 has **no plan yet** — only the
scope lines below.

**Outputs:**
1. `docs/superpowers/plans/2026-11-xx-r4-takeover.md` (use the date you write it on).
2. After Dawid's review: the implementation, committed task by task on `develop`.

## 1. Where things stand (verified 2026-11-13 at `b328e1d5`)

- **R3 is built, reviewed and accepted.** Plan: `docs/superpowers/plans/2026-11-12-r3-multi-host.md` — read its
  header, "Global Constraints", "Amendments made during execution" (what was really built, where it differs
  from the plan's first text) and "Known limits". 24 tasks plus extra store work; ~70 commits since `3a647fe9`;
  nothing is pushed.
- **Go / no-go: accepted as GO by Dawid on 2026-11-13.** Three rounds at 0 / 1 / 2 / 5 ms injected peer delay;
  numbers and reading in `TODO.md` ("Go / no-go, measured 2026-11-13"); logs in `target/go-nogo-r3/rounds/`.
- **Not yet run (Dawid's, not blocking R4):** the native live check (`just dev-r3 2` + `just run-online`), the
  browser live check, the store-thread reading. Checklist: `TODO.md` "To run by hand". The probe did run the
  real stack many times: logins, both hosts, ~170 host-seam hand-offs per delay, both hosts leaving in one
  committed cut at every stop, zero state-hash mismatches.
- **In progress in another session:** a plan for a deterministic order of same-tick neighbour messages at
  seams ("option 1" of the correction-burst finding in `TODO.md`). If
  `docs/superpowers/plans/2026-11-13-seam-slot-order.md` exists, read its status first: it touches
  `crates/game/src/region_runner.rs` and the client's region routing, **not** ownership or claims, so it does
  not block R4 — but do not run both plans' implementers in the same files at the same time.
- **Working tree:** Dawid has uncommitted work of his own (`deploy/**`, `justfile` orion recipes, a few lines of
  `CLAUDE.md`, `assets/studs/parts.bin`, `stud/mesh.rs`, `stud_assets.rs`, `tools/ldraw-bake`, two docs). Never
  `git add -A`; stage only your own hunks in `justfile` / `CLAUDE.md` (`git apply --cached` of a filtered patch).

## 2. Read first

1. `CLAUDE.md` — the rules. The role-split, store and client paragraphs describe R3 as built.
2. `TODO.md` — the R3 block: "Known limits", "Residuals from the final whole-branch review", the R4 and R5 lines.
3. `docs/superpowers/plans/2026-11-12-r3-multi-host.md` — decisions 1–13 (decision 7 is the world rollback
   R4 replaces), the amendments section, "Extension points left for later plans".
4. `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §0 — the settled design: §0.4
   (roles, ownership policy behind a trait, fencing always in Postgres), §0.5 (R4's line), §0.10.
5. The execution ledger of R3, if still present: `.superpowers/sdd/2026-11-12-r3-multi-host/progress.md`
   (git-ignored). Every ruling, as-built note and deferred minor is there; `task-N-report.md` beside it.

## 3. R4 scope (from `TODO.md` and the R3 plan — verify each in code; line numbers drift)

1. **`Ownership` becomes a trait with `PgClaim`.** Today an enum (`crates/game/src/ownership.rs`:
   `AllInOne | Static { ordinal, hosts }`); super-regions (`super_region_of`, S = 16 by default,
   `VOXEL_SUPER_REGION_REGIONS`) are claimed in Postgres with an owner epoch and every write is fenced on it.
   On-demand claiming replaces the static rendezvous split.
2. **Per-host failover instead of the world rollback.** Today a host that boots with a dirty `hosts` row bumps
   `world.generation` and every host exits `EXIT_WORLD_ROLLBACK` (5). R4: a dead host's super-regions are
   claimed by another host and the world keeps running. The `departed` pointers and the peer queues already
   say what is owed to whom; cut rows toward an absent peer exist (`cut_peer_rows`).
3. **Drain** = the leave cut plus re-placement of the leaver's regions and players. Today a leaver parks
   everything and its claims are **not released** (static ownership reclaims them on return).
4. **Sender-side in-flight cut rows**, so a peer-link drop no longer aborts a cut (today: no cuts while any peer
   link or the coordinator is down).
5. **Carried from R3** (each is in `TODO.md`): `HostMsg::Request` carries no hop (a stale grant from an older
   placement can still be given); region incarnations are per region, not per host; a fenced `worldctl` command
   to retire a host that will never return (today a dirty row with no attached host stops all cuts — warned,
   not fixable); a hung (not dead) host is neither detected nor replaced; role `all` on a world with other
   hosts' dirty rows is not refused; the smaller residuals in the "Residuals" line.
6. **Not R4** (mention only as extension points): R5 — k8s StatefulSet, `max_prepared_transactions` on the
   managed Postgres, clock sync across machines, an advertised peer address, certificate pinning for native
   QUIC; R6 — autoscaling. Deferred as decided: rate limiting, backups.

**Design questions the plan must settle** (trade-offs in prose first; ask Dawid only what is his):
- What triggers a takeover: a lease/heartbeat in Postgres, the coordinator's view, peers' reports — and what
  stops two hosts taking the same super-region (the owner epoch already fences writes; the claim must be the
  only path).
- What state a taker starts from: the last committed cut of those regions plus what peers still hold for them
  (in-flight rows, peer queues, `departed`), and what is lost for players who were live on the dead host
  (bounded by one save interval, for that host's regions only — say so explicitly).
- How peers and clients learn the new owner (the `hosts` table, `Running` / `RegionUp`, `RegionElsewhere`,
  the client's `ConnectionSet` retry path) and what a client homed on the dead host sees.
- Whether the world generation survives as a last resort, and when.
- Drain: who decides where regions go; how it differs from a crash (nothing lost, a committed cut first).
- The four defaults Dawid never confirmed in R3 — world rollback on a host crash (R4 replaces it), no cuts
  while the coordinator is down, browser support, go/no-go ratio — say which R4 changes.

## 4. Rules that are easy to break (all learned the hard way in R3)

- **Conservation beats latency; wait, never guess.** Every entity, item and player exists exactly once across
  live state and the store, through any crash point. A plan task without a conservation test is not done.
- **The store's guards** (do not weaken; extend them to any new table or path):
  - `entities` is keyed per region (migrations 0009 / 0010); a cut part writes only rows of its own regions;
    never add a table two hosts' parts can both write.
  - A commit decision is refused unless every host with a non-clean `hosts` row is listed (`decide_cut`), under
    the same `world`-row lock (`FOR NO KEY UPDATE`) that `register_host` takes. A claim takes
    `world … FOR KEY SHARE`. A prepared transaction is not a waiting backend: a wait cycle through one is
    invisible to Postgres — walk the lock matrix for every new statement.
  - `resolve_prepared` runs at every start before the claim; migrations are immutable, column-for-column in
    both engines, and run under a lock timeout.
- **Cut ownership** is on the wire (`Reliable::InCut`, mark at T) — `.superpowers/…/task-13-ruling.md` or the
  plan's amended decision 6. A takeover changes who "the sender" of an in-flight message is: re-derive the
  three sender cases for a taken-over region before writing any task.
- **Wire enums are append-only**; service-link enums change only with a `LINK_PROTOCOL_VERSION` bump (now 4);
  client and server have no version handshake and are built together.
- **Filtered test runs hide breakage.** Two broken world-store tests sat unnoticed through several R3 tasks.
  After a wide change run each touched crate's whole non-client suite once (it found a real deadlock).
- **Reviews find what implementers miss; a second, whole-branch review finds what task reviews miss.** R3's
  per-task reviews caught 20+ Important defects; the final review caught four more that only exist across
  tasks; a test added afterwards (all hosts leaving at once) caught a Critical one. Budget for all three.
- **A plan's own text can be wrong.** R3 needed ~40 rulings; record each (`Ruling: what — why — cost if wrong`)
  and amend the plan file at the end so it says what was built.
- **Test commands:** `just test-one <crate> "--test <target> -- <filters>"` (cargo needs `--` before several
  filters or flags). `hosts`, `roles`, `coordinator_rt` need `--features test-hooks`; `observer` does not.
  Postgres: `MMO_TEST_DATABASE_URL=postgres://mmo:mmo@127.0.0.1:55432/postgres`, `just test-pg`, `just test-r2`,
  `just test-r3`. After Postgres tests `select count(*) from pg_prepared_xacts` must be 0.
- **Ask Dawid first** before client tests, live runs and measurements (he approved filtered client tests and
  the `go-nogo-r3` probe during R3 — a new session asks again). Never create git worktrees. Never commit or
  print secrets (`.env`, `deploy/**/*.tfvars`; a Grafana token lives in `.env`, see `CLAUDE.md`).
- **Disk:** `target/` grows ~50 GB per few tasks; clean only `target/debug` and
  `target/wasm32-unknown-unknown` (`target/garage-import` and `target/tools` are not build output).

## 5. Tools R3 left you

- `just dev-r3 <hosts> [delay]` (2..6 hosts; `scripts/dev_r3.sh` supervises: restart on exit 5 / 137, bounded;
  stop order hosts → coordinator → login). `just test-r3`. `worldctl super-region <url> <n>`.
- `just go-nogo-r3 <delay> [seconds]` — the seam probe (own database `mmo_go_nogo_r3`, exit 0 GO / 1 NO-GO /
  2 INVALID). A takeover changes seams under a moving player: rerun it after R4 and compare **pooled** runs —
  single runs flip (see `TODO.md`).
- The seeded multi-host rig: `crates/game/tests/common/multi_rig.rs`, `crates/game/tests/peer_schedule.rs`
  (scheduling-sensitive correctness goes here; it checks world-wide uniqueness after every committed cut).
  Real processes: `crates/server/tests/hosts.rs`. The crash table of R3's Task 14 is the model for R4's.
- The `[server]` line: `store_req= store_wait_max_ms= store_lat_max_ms= store_cpu_max_ms= store_cpu_sum_ms=`.

## 6. How to work

1. Survey first (Explore agents; keep conclusions, verify every claim before it goes into the plan):
   ownership and claims (`game::ownership`, `world-store` claims / fence / `hosts`), the coordinator
   (`game::coordinator`, `server/src/coordinator_rt.rs`), the manager's peer half (`world_manager/peers.rs`,
   `world_cut.rs`, `placement.rs`), start-up order in `server/src/lib.rs` and `roles.rs`, the client's
   `ConnectionSet` retry and home-switch paths.
2. Write the plan with the writing-plans skill: header, Global Constraints (copy R3's, then update), Review
   Focus, tasks with Files / Interfaces / test-first steps / exact `just` commands, a crash table, a self-review.
3. Give Dawid the plan path, the design questions you settled and how, and those left for him. Wait for his review.
4. Execute subagent-driven: a fresh implementer per task, a task review, fix rounds with scoped re-reviews, a
   ledger under `.superpowers/sdd/<plan>/progress.md`, a final whole-branch review on the most capable model,
   one fix wave. Stop for Dawid only at: client tests, live runs, measurements, anything destructive.
5. End with: what was built, every ruling, what is his to run, what is left for R5.
