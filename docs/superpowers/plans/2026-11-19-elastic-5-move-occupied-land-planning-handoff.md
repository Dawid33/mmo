# Handoff: Elastic 5 — move occupied land in place

**Date:** 2026-11-19, refreshed 2026-11-20 against `3d15eea4` (the coordinator HTTP plan as BUILT, the Planet, the block-level map)
**For:** a fresh session. Where the two prerequisites stand at the refresh:
1. Elastic 4 (`docs/superpowers/plans/2026-11-19-elastic-4-reconciler-scaling.md`): built except Task 9 (the world panel, gated on `crates/panel` being committed); its "As built" is written; not accepted (no live run, nothing on orion). **`TODO.md`'s Elastic 4 block contradicts itself**: its "Open" line says the fix wave's scoped re-review is pending, its "Twin" line says the review is closed and confirmed by its re-review. Check the ledger (`.superpowers/sdd/2026-11-19-elastic-4-reconciler-scaling/progress.md`) and say which is true in the plan.
2. The coordinator HTTP plan (`docs/superpowers/plans/2026-11-19-coordinator-http-api.md`): **BUILT** 2026-11-19 (21 commits, `857f3f83` … `ea26497c`), task-reviewed, whole-branch-reviewed, one fix wave confirmed — read its "As built" and "Final review". **NOT accepted**: nothing has run as processes. Its hand checks (a) two hosts + two replicas with kills, (c) joins / revoke / 503, (d) the twin are its ACCEPTANCE (`TODO.md`, "The coordinator as an HTTP service"). E5 can be PLANNED now; **ask Dawid to run (a), (c), (d) before E5's first live run** — E5's own evidence for the hop needs that stack to work, and a failure there would otherwise be blamed on E5.

**Job:** write the E5 implementation plan with `superpowers:writing-plans` as `docs/superpowers/plans/<date>-elastic-5-move-occupied-land.md`, have Dawid look at it, then execute it subagent-driven. One plan. E6 gets its own handoff after it.

The decisions in §6 are defaults this handoff takes so the plan can proceed. They come from the design, from Elastic 3 and 4 as built, and from the coordinator HTTP plan AS BUILT. §4's code map was re-checked at `3d15eea4` (every claim held; line numbers there are current but drift). **Verify each decision against the code.** Where the code contradicts one, re-rule it in the plan under "Where this plan departs from the handoff" and keep going: do not stall. The ones marked **(Dawid)** are listed again in §11.

## 1. Read first, in this order

1. `CLAUDE.md`: "Testing"; under "Server / store": "Many hosts" (Ownership — assignment and the release in a cut part; Succession; Leave and drain; the fleet), "Owner epochs & fencing", "Peer link rules", "Peer journal", "Regions & seams"; under "Client": "Connections".
2. The design spec, `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md`: decision 8; **§5 step 2 "Step B — move in place"** and the "What is not planned" paragraph; §6, the rows "Taker dies during a move in place", "Host dies while draining, during the warning", "A crowded super-region"; §9, "The warning period".
3. The parent handoff, `docs/superpowers/plans/2026-11-14-elastic-region-hosts-planning-handoff.md`: §3 "E5", §4, §5.
4. The Elastic 3 plan, `docs/superpowers/plans/2026-11-18-elastic-3-land-assignment.md`: "Decisions settled in planning" 2 (release), 3 (the lock walks — the part's release is E5's template), 4 (what arrives for released land); "Known limits"; "Final review fix wave".
5. The Elastic 4 plan: "Decisions settled in planning" 4 (the ticker's rules — E5 changes what a scale-down does), "Known limits", "As built".
6. The coordinator HTTP plan: "Decisions settled in planning" 2 (the mailbox), 3 (attachment without a link), 4 (the host's end), then its "As built" and "Final review" — where the build departs from those decisions, the build wins. And `CLAUDE.md` "Several coordinator replicas over one row" (the mailbox, attachments, orphan rows, the version check) and "Link rules" (the host's link: down / up / generation, the mailbox gate), which were rewritten from the code.
7. `TODO.md`: the Elastic 3, Elastic 4 and coordinator-HTTP blocks (known limits, hand checks, the deferred minors).
8. `CLAUDE.md` "Worldgen" (the Planet: x wraps; **under any role but `all` the wrap seam is CLOSED**, `roles::seam_for_role`, `crates/server/src/roles.rs:48`, until the host-to-host seam is built) and "Server / store" (`chunks.top`, migration `0021`, written in the chunk's own INSERT of the cut part).

## 2. What E5 delivers

- **An occupied host can be emptied without a reconnect.** One super-region at a time: its regions are frozen at a tick and committed in a world cut; ownership passes to another host inside that cut under a new epoch; the taker loads the regions; the players standing there keep their exact pose and their session hops to the taker. They see a warning, then a pause of seconds.
- **A scale-down no longer makes players rejoin.** Elastic 4's rule 6 drains a host with the warned drain (its players reconnect). With E5 the coordinator first moves its occupied land, then drains the empty host.
- **A hotspot can be relieved** (design §5): one overloaded host hands a busy super-region to a less loaded one. A crowded super-region still cannot be split.
- **`in_flight` is keyed by region, not by host** (parent handoff, "What E4 leaves"), so a leave or a move never strands rows keyed to a host id.

What it does not deliver: a live move without a cut (design §5, "not planned"); several nodes; rebalancing as a goal of its own.

## 3. What the coordinator HTTP plan (as built) changes underneath E5

- **Coordinator → host is a durable mailbox** (`outbox` rows `(host, boot, at_ms)` inserted ONLY by the transition that decided them, after its compare-and-set; the host long-polls and acknowledges by cursor: exactly once, in order). A move's orders to the giver and the taker are `CoordMsg`s, each a row of the transition that decided it. E5 needs no delivery machinery and "a lost order" is not a case. But:
  - **An applied `Attach` deletes the host's older rows** (`outbox_drop`) before its `Attached { epoch }` row, and the host's link delivers nothing before that row. A move order decided before a re-attach is GONE for that host: the move must be re-derived from the durable run when the `Attach` is applied (as a succession's order is, through the `Attach`'s `successions` list) or abandoned. Write which, per run state.
  - The mailbox gate lets one message through without an `Attached` row: `CoordMsg::Generation`. Nothing else.
  - **Orphan rows**: a head row unread past `MAILBOX_STALL_MS` whose boot is not the one attached is dropped by the ticker (`drop_orphan_mailbox`). A move order to a host that re-registered (a new boot) is dropped this way: never rely on it arriving.
- **Host → coordinator is a POSTed `HostBatch`** stamped with the `(boot, epoch)` of its `Attach`; answered once its transition committed. A failed POST or `attached: false` is a link drop: the host drops what was queued and re-attaches. Answers a move waits for (the giver froze, the taker loaded) are ordinary logged `HostMsg` inputs and can be LOST this way: the run must re-ask or re-derive them, never wait for one forever.
- **Attachment names no replica** (`WorldCuts::attached`: host → boot; `attached_ms`); the gate is world-wide (`Coordinator::gate_host()`, `coordinator/mod.rs:1283`); `attached_live` (`world_cut.rs:954`) skips succeeded and left hosts.
- **A dead or cut-off host stays attached for up to ≈ 27 s** (the whole-branch review's main finding): only `detach_host` removes an attachment — a stalled mailbox (`Input::HostStalled { host, boot, epoch }`, `input.rs:81`; `MAILBOX_STALL_MS` 15 s once a row waits; a `Calendar` row every `DAY_MS` 10 s guarantees one waits), a new generation, a rollback, a refused `Attach`. A host's END drops its rows but keeps the attachment (`attached_live` then skips it). **A taker chosen during that window may be dead.** The move must not count a taker as reachable from `attached` alone: rule on what qualifies a taker (`pick_assignee`'s rules: attached, live lease row, takes logins, known ≥ `ASSIGN_MIN_ATTACHED_MS`), and what a taker that dies after being named does to each run state.
- **Core-less transitions**: a transition that changed nothing durable is written WITHOUT a core, and a failed one is never remembered as ours (a conflict re-issues it). Only `core_less_kind` messages and `core_less_decision` store decisions (`AssignLand`, `AddHost`, `DrainHost`, `RemoveHost` — `crates/server/src/coordinator_rt.rs:1079`; each idempotent) may ride one; `check_core_less` debug-asserts the rest. A move is durable core state (§6.2), so its transitions carry a core — but if E5 adds a store decision or a `CoordMsg` that can be issued without a durable change, decide whether it joins those lists and prove it is safe twice.
- **Login orders** are held by the runtime (never logged) until their row commits and re-issued after a rebase; the gate host places them. A player whose land is being moved can be the subject of a held order: classify it in §6.3.
- **Versions at the refresh:** `LINK_PROTOCOL_VERSION` 12 (`crates/svc-link/src/lib.rs:71`), `COORDINATOR_STATE_VERSION` **8** (`crates/game/src/coordinator/state.rs:40`; 7 lived only inside that plan). E5 bumps each once: 12 → 13, 8 → 9.
- **Migrations:** `0020_mailbox` and `0021_chunk_tops` are taken; E5's first is **`0022`** (recheck: other plans are landing).
- **`crates/server/src/coordinator_rt.rs` was rewritten** (≈ 3 100 lines). Every line number for it in the Elastic 3 and 4 documents is void. `svc-link` is only the host ↔ host peer transport, which E5 uses heavily and which is unchanged.
- **Write volume is unmeasured** (that plan's hand check (e)): every coordinator → host message is a row insert + delete, each version bump makes every other replica reload the whole core. A move is a handful of messages; say so, do not optimise — but do not add a per-tick message.

## 4. Where things stand (code map re-checked at `3d15eea4`; every claim held; line numbers drift)

Current anchors: `WorldCut::releases` `crates/game/src/save_cut.rs:104`; the release lock `RELEASE_LOCK` `crates/world-store/src/rows.rs:471`, its fence check ~`:1778`, the free loop ~`:1945`; `land.rs` `busy_land` :237, `leave_release` :341, `land_released` :402, `Reliable::Released` sent :447, `drop_stored_parked` :464, `masked_or_released` :532; `assign_land_in` `crates/world-store/src/assign.rs:27` (the assignee's `hosts` row `FOR SHARE` first); `HostClaims::add` `crates/server/src/claims.rs:296`; `hand_off_to_peer` `peers.rs:405`, `accept_handoff` :453, `move_to_peer` :526, `park_handed` :716; `landing_in` `world_manager/mod.rs:1961`; `placement.rs` `SPAWN_RELAY_MAX_HOPS` :62, `SPAWN_PIN_MS` (120 s) :74, `departed_denied` :455; `ServerPacket::RegionElsewhere` / `HomeHop` / `HostDraining` `crates/game/src/protocol.rs:875/883/890`; `in_flight.host` added by migration `0014_host_ids.sql:20`; `LeaveGate` `coordinator/succession.rs:290`; the fleet's victim key `coordinator/fleet.rs:340`; `HostLoad` and `DRAIN_WARNING_SECS` (300) `crates/game/src/ownership.rs:39/51`; `SUCCESSION_DEADLINE_MS` (30 s) `ownership.rs:112`.

**What moves land today**
- **Release** (Elastic 3): a host frees idle super-regions in its OWN prepared cut part. `WorldCut::releases`; `world-store/src/rows.rs` `cut_rows` locks the released rows `FOR NO KEY UPDATE`, checks them against the fence, frees them (`owner = -1`, `epoch + 1`) after every other write, then `PREPARE`. Host side `crates/game/src/world_manager/land.rs`: the `releasing` mask (`owner_of` answers `Unknown`), `land_released`, `drop_stored_parked`, `Reliable::Released` to peers; server side `HostClaims::shrink` before `cut_committed`. **Only idle land** (`busy_land`).
- **Assignment** (Elastic 3): the store decision `AssignLand` gives a FREE row to a host (`world-store/src/assign.rs`); `CoordMsg::Assigned`; the keeper grows the fence (`HostClaims::add`) before the manager learns it — **fence before use**.
- **Succession** (R4): all of a dead or departed host's land to ONE heir, with journal reports, the heir's `inheriting` mask and adoption (`coordinator/succession.rs`, `world_manager/succession.rs`).
- Nothing moves OCCUPIED land of a live host. Nothing names a taker for a release.

**What moves a player between hosts today**
- `hand_off_to_peer` (`world_manager/peers.rs`): a body crossing a host seam travels as `Reliable::Transfer { player: Some(PlayerHandoff) }` with its exact state; the session on the old host becomes an observer; the client follows `ServerPacket::HomeHop`.
- `move_to_peer`: a move (portal, adventure start) into another host's land travels as `Reliable::PlayerMove` with a **parked record** standing at the move's **landing** (`landing_in`), not at an arbitrary pose.
- A player handed into land that is masked for a release or an inheritance is **parked and rejoins** (`accept_handoff` → `park_handed`, `masked_or_released`): Elastic 3's known limit.
- `departed` pointers and spawn-order relays (`placement.rs`; `departed_denied`, `SPAWN_RELAY_MAX_HOPS` 32).

**What the client does today** (`crates/client/src/connections.rs`, `renderer/drain_notice.rs`)
- One connection per host; `RegionElsewhere { endpoint, ticket }` opens an observer connection; the home follows `PlayerRegion` from another connection only after a higher `HomeHop`.
- `ServerPacket::HostDraining { at_tick }` from the home drives a countdown line. There is no per-region warning.
- The client re-maps regions whose owner changed after a succession (R4 Task 16).

**`in_flight`**
- Rows are keyed to a HOST id. Only a succession's adopting part moves them. A leave part that carries one releases nothing and waits for a `Leave` succession (`leave_release`, `LeaveGate`). Since Elastic 4's fix wave a drained leaver's desired row is removed even when it kept land, so its pod goes while its land waits for an heir.

**Scaling** (Elastic 4, `crates/game/src/coordinator/fleet.rs`)
- Rule 6 (scale down) picks a victim (fewest players, then fewest running regions, then the highest id) and issues `DrainHost`: 0 s notice with 0 reported players, else `DRAIN_WARNING_SECS`. Rule 7 takes the last host of a `min 0` world when it reports 0 players and 0 regions.
- No rule looks at one super-region's load; `HostLoad` is per host (players, running regions, worst tick permille).

**Nothing exists yet for:** a move decision, a taker named in a cut part, a per-region warning, a session hop at an exact pose outside a seam crossing, per-super-region load.

## 5. The shape of a move (the design's step B, against today's code)

1. The coordinator decides: super-region S goes from giver G to taker T at a stated tick, after a warning.
2. During the warning the players on S are told, and their clients open a connection to T.
3. At the deadline a world cut begins. G masks S (as `releasing`), its regions park at the cut's tick, G's part writes them and **names T as the next owner** of S's row at `epoch + 1`.
4. The cut commits. G shrinks its fence and tells its peers; T grows its fence, then loads S's regions from the store.
5. The players' sessions hop to T at their saved pose; G keeps `departed` pointers.
6. What arrived for S meanwhile was held and goes on to T.

Each step has a failure row in design §6. The plan's failure table is the heart of E5.

## 6. Decisions taken (defaults; verify against the code)

### 6.1 Ownership passes in the giver's own cut part

- A move is a release that names its successor: `WorldCut::moves: Vec<(SuperRegion, HostId /*taker*/, u64 /*taker's boot*/)>`. The part locks the row as a release does and, after every other write, sets `owner = taker, epoch = epoch + 1` instead of freeing it.
- This keeps "a cut part writes only rows of its own regions" (the row is the giver's until its commit) and "commits and aborts with the part".
- **Not** "free it, then `AssignLand` in the decision's transition": the staged part holds the row until `COMMIT PREPARED`, which runs after that transition commits, so the assignment would be refused.
- **Must settle:** the taker's liveness. `AssignLand` checks the assignee's `hosts` row FIRST (live, dirty, undrained) under a share lock. A part cannot hold the taker's `hosts` row for a staged transaction's life. Options: the decision (`DecideCut`) checks every moved row's taker in its transaction and aborts the cut otherwise; or a taker dead at commit is simply a landed host that died (a `Crash` succession). Write the lock walk into the plan before the tasks.
- **Must settle:** the taker must take part in that cut (it is frozen out otherwise — "Peer link rules": a participant freezes its session with every host outside the cut), and must be a host the giver may send state to.
- One-phase cuts (static, all-in-one, SQLite) refuse a move, as they refuse a release.

### 6.2 The coordinator runs a move as a durable run, like a succession

- A move spans a warning, a cut and a settle, across ticker changes and re-attaches: it is durable core state (`projection!`, `COORDINATOR_STATE_VERSION` 8 → 9) with a deadline, not a process timer. Model it on `SuccessionRuns` (`successions` in `projection!`, `coordinator/mod.rs:208`): how a run's orders are re-sent on an `Attach`, how its deadline survives a reload (`CALENDAR_GAP_MS` restarts an old deadline's count), how answers count per BOOT.
- Store decision `BeginMove` (records the run; marks nothing in the store beyond it) or none at all — **must settle** whether the store needs to know of a move before the part names the taker. The default: no table; the part's row change is the only store fact, and the run lives in the core.
- Messages (appended; `LINK_PROTOCOL_VERSION` 12 → 13 once): to the giver "move S to T at tick X, warn now"; to the taker "expect S from G"; from each, its answer. Through the mailbox both are exactly once per attach; a re-attach mid-run deletes the undelivered order (§3), so the `Attach`'s application re-derives it from the run or abandons the run.
- **One move at a time per world, none while a succession run is open, none of land that is `inheriting` or `releasing`, never a host that is leaving.** A move's cut is an ordinary world cut that carries a move; no second kind of cut.
- A move is abandoned, never forced: taker gone, giver gone, cut aborted, deadline passed → the land stays with the giver, which lifts its mask and resumes (design §6). The run records why.

### 6.3 The player's hop keeps the exact pose

- The moved regions' entities are in the regions' saved state; players are records. A player standing on S at the cut is written by G's part **at its exact pose** (`cut_transit` / `parked_written` are the existing hooks for "a record wrapped in the cut") and is NOT offline: its session, its login, its claims and its hop go to T.
- **Must settle:** the carrier. Either a new `Reliable` (a `PlayerHandoff` with no body, "this player is yours when S loads"), `InCut`-wrapped and journalled, or the existing `PlayerMove` with a pose instead of a landing. The player's body must exist exactly once: in G's region until the cut's tick, in the store between, in T's region after. **This is E5's conservation proof; write it in the plan.**
- T places the player from the record when S's region starts (never from the store before every peer synced — "wait, never guess" holds), promotes its session (`promote_session`, `HomeHop`), and reports it online at the next hop.
- A player who disconnects during the pause is parked on T at the saved pose, as today.
- `SPAWN_PIN_MS` pins, held spawn orders and `departed` pointers for players on S must be classified: each travels, is re-pointed, or is refused.

### 6.4 What arrives for S during the gap

- From G's mask until T's adoption S has no usable owner. Elastic 3 already holds what arrives for released land on the unowned path and delivers it once to the next owner (`flush_unowned`, the mailbox forward). A move uses that path; the next owner is known in advance.
- **A player hand-off into S during the gap must not be parked** (today's known limit, and the parent handoff's requirement). The default: the sender keeps the body at the seam (as Elastic 3's plan named but did not build: "waits at the seam and crosses once"), or forwards the hand-off to T once it owns S. Must settle which; a parked player is a reconnect, which E5 exists to remove.
- Peers learn the new owner as they learn a release today, plus the owner: `Reliable::Released` grows a successor, or a new message. Not state-bearing; the store's row is the truth.

### 6.5 `in_flight` re-keyed from host to region — first

- Migration `0022` (after `0020_mailbox` and `0021_chunk_tops`; recheck the next free number at that task), both engines, identical in shape. Rows keyed by the region (or super-region) they belong to; a cut part writes only its own regions' rows, so a moved or released super-region's rows move with the row, with no adoption step.
- Then every clean leave can release all its land (`leave_release` loses its exception), the `Leave` succession and the `LeaveGate` may become dead code, and Elastic 4's "a leaver that kept land" cases disappear. **Must settle** how much of that removal belongs in E5: the default is to re-key, make leaves release, and delete the `Leave` path only if nothing else declares one (a drained host that died mid-leave is a `Crash`).
- Do it as Task 1–2: a move's part would otherwise carry in-flight rows keyed to the giver.
- Worlds are re-forged, never migrated.

### 6.6 The warning, and the client

- The warning period reuses `DRAIN_WARNING_SECS` (300 s) as the default **(Dawid)**; a hotspot move may want a shorter one.
- A new `ServerPacket` tells a player "this area moves to another server at tick X" (per region, from the home only, as `HostDraining`); the countdown line formats it. A player may walk off the land; then nothing happens to them.
- During the warning the client opens its connection to T (`RegionElsewhere`-style ticket for T, before T owns S). **Must settle** what T serves on that connection before the hop (nothing; it only has to be open and authenticated).
- At the hop the client's home changes with a higher `HomeHop`; its predicted regions for S keep running across the pause or are restarted from the first snapshot T sends — **must settle**, with `crates/client/src/regions/` and the rollback bar in mind (the pose is exact, the tick numbering is the world's, so a restart from T's snapshot should be a rollback like any other).
- The pause is the cut plus T's region load. Measure it on `dev-r3` before choosing any bound (ask Dawid first: a live run).

### 6.7 Who asks for a move

- **Scale-down (replaces part of Elastic 4's rule 6):** the victim's occupied super-regions are moved, one at a time, to the serving host with the most headroom; when the victim reports no players and holds no land, it is drained with a 0 s notice. If no taker has headroom, or a move is abandoned twice, fall back to the warned drain (today's behaviour).
- **Hotspot relief (new rule):** a host above `SCALE_UP_LOAD_PERMILLE` beside one well below it, sustained → move one super-region. It needs **load per super-region**, which the beat does not report (`HostLoad` is per host). Must settle: a per-super-region load in the beat's row, a message to the coordinator, or "the giver picks which of its super-regions goes". The default: the coordinator names the giver and the taker, **the giver names the super-region** (it knows its own tick time per region).
- **Hotspot relief may be cut from E5 (Dawid):** the drain path is the delivery; relief is the same mechanism with a second trigger. Plan it as the last tasks so it can be dropped.
- An operator's move (`worldctl move-land`?) is out, unless a hand check needs one.

### 6.7b The Planet's wrap and the map

- Under split roles the x-wrap seam is CLOSED (`roles::seam_for_role`): no hand-off, halo or intent crosses it. A move does not change which seams are open, but a super-region at the east or west edge has a neighbour across the wrap that is "closed", not "another host's": check that the giver's mask, the taker's load and the peers' re-routing treat a closed seam as such and never as "unowned land to wait for". Read `docs/superpowers/plans/2026-11-14-planet-frame-1a.md` for what the closed seam does today.
- `chunks.top` (migration `0021`) is written in the chunk's own INSERT of the cut part, and the map API (`crates/server/src/map_api.rs`, on the coordinator's listener) reads it: a moved region's chunks keep their tops through the giver's part; nothing else to do — confirm, do not build.

### 6.8 What does not change

- Fencing in the store is the only safety; fence before use; shrink before the next part.
- Peer links (`svc-link`), the peer journal's rules, successions for crashes.
- The reconciler, the module and the recipes (a move changes no Kubernetes object).
- `static` and `allinone` never move land.
- One thread per region; a move never splits a super-region.

### 6.9 Tunables (named consts; defaults to argue in the plan)

| Constant | Default | Note |
|---|---|---|
| `MOVE_WARNING_SECS` | = `DRAIN_WARNING_SECS` | per move; **(Dawid)** |
| `MOVE_DEADLINE_MS` | to argue | from the cut's begin to the taker's adoption, else abandoned |
| `MOVE_RETRY_AFTER_MS` | to argue | before the same land is tried again |
| `MOVE_ABANDON_MAX` | 2 | then the warned drain |
| `HOTSPOT_GAP_PERMILLE` | to argue | giver's load minus taker's |
| `HOTSPOT_AFTER_MS` | 300 000 | how long the gap must hold |

## 7. Order of work (the plan decides; this is a suggestion)

1. `in_flight` by region: the migration, the part's writes, the adoption that no longer moves rows.
2. Leaves release everything; what is left of the `Leave` succession.
3. The store: a part names a taker (`moves`), its lock walk, the decision's check.
4. The host: the giver's mask and part; the taker's fence, load and adoption; peers learn the owner.
5. The player's hop at an exact pose; hand-offs into S during the gap.
6. The coordinator: the durable run, its messages, its failure table.
7. The client: the warning, the early connection, the hop.
8. Scale-down by moving first.
9. Hotspot relief.
10. Metrics, alerts, probes, docs and closing.

**Sizing:** the parent handoff says 10–14 tasks. With the re-key first, expect the upper end.

## 8. Rules that carry over

- **Conservation beats latency; wait, never guess.** Every entity, item, player and person exists exactly once through any crash, conflict, abort, re-attach or abandoned move.
- **Write-before-send.** Since the HTTP plan this is structural: a host sees a coordinator message only as a row of a committed transition.
- **Lock walks** for every new or changed statement on `super_regions`, `hosts`, `coordinator`, `in_flight`, written into `world-store/src/succession.rs`'s notes and INTO THE PLAN before the tasks. A prepared transaction is not a waiting backend.
- **Migrations** immutable, identical in shape on both engines. Wire enums append-only. Every new core field in `projection!`, every new `WorldManager` field in `host_fields!`, every new input classified, every new state-bearing `Reliable` journalled or argued out, and `carry_components` / `Client::arrive` re-classified if a field travels.
- **Everything on the world side movable to another cluster** (design §10).
- **Ask Dawid first:** client tests, live runs (`dev-r3`, `go-nogo-r3`, `just run`), measurements, anything on orion. `just twin-check` is allowed.
- Never `rustfmt` / `cargo fmt` from the root. Commit only your own hunks.

## 9. Testing, and what the twin cannot do

- Only regression tests are committed (`CLAUDE.md`, Testing). No "write the failing test" steps. Each task: scenarios traced in the report and checked by the reviewer; temporary tests (the seeded rig `crates/game/tests/common/multi_rig.rs`; temporary in-crate tests against the dev Postgres — there is no `just test-pg`); `just smoke`; `just web-check` when `game` or the client's login/connection code changes; `just determinism-wasm` if anything hashed or serialized in the sim changes (a player's record at an exact pose may).
- **The twin has no connected player**, and E5 is about a player standing on land. `twin-check` can prove the store and process side of a move (the row changes owner at `epoch + 1`, generation unchanged, no succession, the taker loads, a join lands on the taker), not the hop. **Must settle** the evidence for the hop: the seeded rig (bodies, sessions, the conservation assert) plus a live probe — `go-nogo-r3` / `fly-probe` style, a headless client standing on S during a move on `dev-r3`. That probe is a live run: plan it, and ask Dawid before each run.
- The failure table (taker dies before the cut, at the cut, after it; giver dies in each; the cut aborts; a coordinator replica dies mid-run; a re-attach mid-run) is exercised on the rig, and the kills that the twin can do are twin phases.

## 10. How Elastic 4 was executed — keep what worked, fix what did not

- Subagent-driven, one implementer and one reviewer per task, a ledger with rulings (`Ruling: what — why — cost if wrong`), a whole-branch review in three areas on the most capable model, one fix wave, one scoped re-review.
- **Five of the plan's rules were wrong and were found by reviews, not by the plan:** the stop window the reconciler's guard did not cover; a drain that finished before the ticker looked; `min 0 / max 1`; an alert on a counter that could never move; a leaver that kept land holding its slot for ever. Four of the five are **states a world can reach and never leave**. For E5 enumerate those in the plan, per run state: who is waiting on whom, and what ends the wait.
- The whole-branch review found six Important issues after every task review had passed, all at seams (rules against successions and leaves; recipes against the reconciler; alerts against a world at zero by design). Give it the seams explicitly, as the E4 dispatches did.
- **Reports and reviews go to files.** Messages between agents are truncated; ask for a file and a short verdict.
- **Pathspec commits sweep other sessions' hunks** (two tasks did it). Every dispatch says: a file that already shows ` M` is staged by a filtered patch (`git diff -U3`, own hunks only, `git apply --cached --recount`), verified against a saved baseline; no pathspec, no amend.
- Other sessions' uncommitted edits broke the wasm build and the image build for hours, and the sim goldens in `just test-full`; check `git status` before blaming a task, and re-run when the tree is clean.
- The twin needs disk; `twin-up` now sets kubelet's image GC to 98 / 95 %.
- Implementers verified the plan's arguments against the code before building on them and stopped when one failed (Task 1 of E4). Keep that step wherever the plan argues conservation.

**And from the coordinator HTTP plan's execution (2026-11-19):**
- **Its most important finding came only from the whole-branch review**: a dead host now stays attached for ≈ 27 s, and what that does to the gate, assignees, heirs and the settle cut. Per-task reviews cannot see a TIMING window that spans tasks. For E5, write a table into the plan before the tasks: per run state, every window in which a party may be dead, detached, stalled or re-attached while the run still counts it, its worst-case length from the named consts, and what ends it. Give that table to the whole-branch review as a seam.
- A design detail the implementer ran into, not the plan, was the right call more than once (the `Generation` row passing the mailbox gate; an `Attach` and a detach both dropping rows). Tell implementers to report such rulings in their report, and ledger them.
- **Commit from the index with NO pathspec** when a file carries someone else's hunks — `git commit -- <file>` takes the working tree and sweeps them (it happened again in this plan's Task 3). Stage your own hunks positively (`git diff -U3`, keep yours, `git apply --cached --recount`), never by subtracting an older snapshot: other sessions edit the same files meanwhile.
- **Other sessions commit on `develop` during the run** (worldgen committed ~10 times mid-plan): build review packages from the plan's commit list, not a range, once foreign commits interleave.
- **Docs written by an implementer were wrong in one load-bearing sentence** ("detached by the host's lapse or end") that every task review had missed; the Task 8 review caught it by checking each claim against the code. Keep that review shape for E5's `CLAUDE.md` rewrite.
- Hand checks that need `just pg-reset` (it wipes the shared dev world), a GUI client, the twin or orion were never run by subagents; they were written as owed with exact commands. Keep that, and ask Dawid for each live run.

## 11. Open, for Dawid (the plan proceeds on the defaults)

- **The coordinator HTTP plan's acceptance first**: its hand checks (a), (c), (d) (`TODO.md`) before E5's first live run (§ header). Not needed to WRITE the E5 plan.
- **Elastic 4's re-review state**: `TODO.md` says both "pending" and "closed" (§ header).

- **The warning period for a move** (§6.6): `DRAIN_WARNING_SECS`, or shorter; and what a player may do during it.
- **Hotspot relief in or out of E5** (§6.7).
- **How much of the `Leave` succession is deleted** once `in_flight` is keyed by region (§6.5).
- **The live probe for the hop** (§9): a headless client on `dev-r3`, runs asked for one by one.
- **The pause's bound**: measured on `dev-r3` before a number is chosen (§6.6).
- **Elastic 4's open items that E5 touches:** the world panel (`crates/panel`, still untracked; E5 changes no recipe, so it stays independent); the hand checks on orion; the known limits a move makes rarer (the warned drain's rejoin) or more frequent (a hand-off into land in its release window).
