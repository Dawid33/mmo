# Elastic 1 — Hosts as Individual Deployments (a fixed set) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A region host is an ordinary pod with an id issued when it registers; N hosts are N Deployments declared from a list in OpenTofu; the login service links to the coordinator; an operator can drain a host with an in-game warning and countdown.

**Architecture:** The host ordinal (a raw `u32` from `VOXEL_HOST_ORDINAL`, checked against `VOXEL_HOSTS`) becomes `HostId`, issued by `register_host` from a counter on the world row and never reused; a restarted process is a new host that owns nothing, and the old id is ended by R4's succession. Hosts find each other only through the `hosts` table. The coordinator gets a second listener for the login service and hands each login order to one live host, which places the player exactly as the entry host does today. A drain is a mark on the host's row, read at its heartbeat: players are told the tick at which the host leaves, then R4's leave cut and the client's own rejoin do the rest. The world module's StatefulSet becomes one Deployment, Service and PodDisruptionBudget per entry of a map of hosts.

**Tech Stack:** Rust (sqlx on SQLite and PostgreSQL, quinn through `svc-link`, bincode), Slint, OpenTofu, k3s / k3d.

**Spec:** `docs/superpowers/specs/2026-11-14-elastic-region-hosts-design.md` (decisions 1–3, 9; §5 step A; §6; §7 item 1) and the handoff `docs/superpowers/plans/2026-11-14-elastic-region-hosts-planning-handoff.md` (§3 E1, §4, §5). This is the first of six plans (E1–E6); E2–E6 are written after this one is built, because each names code this one rewrites.

## What the handoff and the design got wrong or left out (verified 2026-11-15, `5e2f3c41`)

- There is no ordinal type. The ordinal is a bare `u32` under many names (`host`, `peer`, `owner`, `heir`, `from`, `to`, `me`, `mine`, `origin`): about 720 source and 315 test occurrences of the word alone, and `crates/game/tests/peer_schedule.rs` has over 1 200 host / peer references. A search for "ordinal" undercounts; only the compiler finds them all (Task 1).
- The host claims its land (`crates/server/src/lib.rs` ~1013) and starts its peer links (~1043) **before** it registers (~1069). An id issued at registration needs that order turned round (Task 3).
- `hosts` already has a per-registration number (`reg`, `MAX + 1`) and a per-process `boot`. The row is upserted `ON CONFLICT (ordinal)`.
- Observer tickets carry no ordinal (`ObserverTicket { client, nonce }`); the ordinal travels in `HostEndpoint.ordinal`, which reaches clients.
- The seeded test rig is built on `game::Static { ordinal, hosts }` (`crates/game/tests/common/multi_rig.rs:394`). `Static` cannot simply go.
- The coordinator's listener is typed `<HostMsg, CoordMsg>` and closes a `Login` peer. It knows which host an *online* player is on (`online_on`), not where a parked one belongs; and it never sees a `PlayerId` for a login order (the token is resolved on the host).
- SIGTERM parks every player at once (`cuts.rs` `leave_world`); there is no warning, no countdown packet and no countdown UI. The only server text is `ServerPacket::Notice(String)`, a history line.
- R5's world module has no PodDisruptionBudget. Nothing of R5 has been applied to orion, so E1 replaces manifests that exist only on the k3d twin.

## Decisions settled in planning

1. **`HostId(u64)` is the registration number.** Issued from `world.next_host_id` under the world-row lock, starting at 1, never reused. `HostId::SOLO` (0) is role `all` and SQLite, which keep today's behaviour. `boot` stays where it is (a host id has exactly one boot, so every "same ordinal, other boot" branch is dead); removing it is a clean-up after E1, not part of it.
2. **A new boot never waits.** `BootVerdict` / `WaitFor` go: no succession can name an id that did not exist. This removes R4's limit "a boot whose ordinal an open succession names waits; if the heir never returns only `worldctl rollback` gets it out".
3. **One heir per predecessor stays** (`heir_of`, a rendezvous pick among live ids). E3 replaces the preference with the coordinator's answer; E1 does not touch who decides.
4. **A total crash:** the first registration that finds no live row and at least one dirty row abandons the open successions, takes every dirty row's `super_regions` and `in_flight` rows itself at `epoch + 1`, marks those rows dead and raises the generation — in one transaction. The hosts that register after it own nothing. (Today each ordinal keeps its land. Until E3 releases idle land, the first host back holds everything that was claimed.)
5. **A cleanly departed host never returns**, so `HEIR_WAIT_MS` goes: `LandWanted` for a clean row's land declares the `Leave` succession at once (today's rule for "an ordinal beyond the host count").
6. **`VOXEL_HOSTS`, `VOXEL_HOST_ORDINAL` and `VOXEL_ENTRY_HOST` are removed.** `claim` means "any number of hosts" and always needs role `host`, PostgreSQL and `jwt` (what `check_several_hosts` asked of `hosts > 1`).
7. **`Static` survives as the escape hatch for ONE host** (`dev-r2`, a split world with a single host): a `static` registration beside a live row is refused. In `game` it becomes a fixed table (`Static { me, layout }`), which is what the seeded tests use for several hosts.
8. **`VOXEL_HOST_NAME`** (free text, default `host`) labels a process for people: log lines, default file names, `hosts.name`, the Deployment's name. It is never a key. One use beyond display: a registration marks other dirty live rows of the same non-empty name lapsed (`seen_ms = 0`), so a restarted pod's old id is declared at the coordinator's next read instead of a lease later — R4's vouch, by name. A wrongly vouched live host is fenced, not duplicated.
9. **Placement stays on the hosts.** The coordinator relays each login order to the *gate host* (the lowest attached, live, non-draining id) and relays the answers back; it keeps no state for it (E2 wants none). The gate host does what the entry host does today (`placement.rs`).
10. **The login service does not change.** It dials an address with a pin; that address is now the coordinator's. The `worlds.host_link` column keeps its name.
11. **A drain is a row mark**, set by `worldctl drain`, read by the host at its heartbeat. SIGTERM keeps meaning "leave now" (a node eviction must finish inside the grace period). A drained host stays up, idle and unready, until it is removed: an exit would make its Deployment start a new host.
12. **Client UDP: a node port pair per host Service** (as R5 built; R5's Phase D fallback to `hostPort` is unchanged). Several nodes are out of scope.
13. **The world id goes into `LinkHello`** with this plan's protocol bump (design §7 "what R5 can do"; E2 needs it).

## To confirm with Dawid (the plan proceeds on the defaults above)

- Decision 4's cost: after a whole-cluster restart one host carries all claimed land until E3. The alternative (free the land in the registration) needs `in_flight` rows re-keyed from host to region, which is E5's work.
- Decision 7: several hosts under `static` (R3's fixed layout) stop existing outside tests. `go-nogo-r3` already runs `claim`.
- `DRAIN_WARNING_SECS` = 300, and what a player may do during it: nothing is blocked; walking onto another host's land avoids the reconnect.
- Decision 12 (handoff §6).

## Global Constraints

- No git worktrees; work on `develop`. Dawid commits concurrently and has uncommitted hunks in `CLAUDE.md`, `justfile`, `deploy/**`, `crates/client/**`: never `git add -A`, never `git stash`. A file that also holds his hunks is staged with a filtered patch (`-U3`, `git apply --cached --recount`), never `--unidiff-zero`; check `git diff --cached --stat` before every commit.
- **Conservation beats latency; wait, never guess.** Every entity, item and player exists exactly once across live state and the store through any crash point. A task that changes who holds what is not done without a conservation test.
- **Fencing in PostgreSQL is the only safety.** Nothing a Deployment, a PodDisruptionBudget or a name does may be what prevents two writers. Never add a store write that skips the fence.
- A cut part writes only rows of its own regions. Migrations are immutable, and identical in shape in `migrations/pg` and `migrations/sqlite`; `0001`–`0013` are never edited. Every new or changed `hosts` / `super_regions` / `successions` / `world` statement gets its lock walk in the module notes of `world-store/src/succession.rs` (the operator's in `operator.rs`). A prepared transaction is not a waiting backend.
- The lease clock is `clock_timestamp()`. The heartbeat stays in the host's main loop on its own connection.
- Wire enums are append-only (`ServerPacket`, `ClientPacket`, `Credential`: new variants last). Link enums change only with the one `LINK_PROTOCOL_VERSION` bump of this plan (6 → 7, Task 6). Client and server have no handshake: this plan's builds are not compatible with older clients or hosts (an upgrade is a stop of the world and a rejoin).
- `game`, `server`, `worldgen` stay free of Bevy. rustls: ring only (`cargo tree -i aws-lc-rs -p server` stays empty).
- Role `all`, SQLite and single-player keep their behaviour, with `HostId::SOLO`.
- **An implementer never runs** `just orion-apply`, `just orion-*-push`, `kubectl --context orion` with a write verb. Ask Dawid before client tests (`just test-one client …`), any live run (`dev-r3`, `go-nogo-r3`, the twin with a client) and any measurement. `just twin-check` is allowed.
- Tests: `just test-one <crate> "<cargo test args>"`, name filters after `--`; server targets `roles`, `coordinator_rt`, `hosts`, `takeover` need `--features test-hooks`; PostgreSQL tests take `MMO_TEST_DATABASE_URL` (`just test-r3`, `test-r4`, `test-pg` set it). A test slower than 100 ms is tagged `#[ignore = "slow: >100ms — run via just test-full"]`. `just smoke` after every Rust task. Scheduling-sensitive correctness goes in the seeded layer (`crates/game/tests/peer_schedule.rs`, `common/multi_rig.rs`); real processes in `crates/server/tests/hosts.rs` and `takeover.rs`.
- Tunables are named consts. Ordered state is `BTreeMap` / `BTreeSet`. A new per-host `WorldManager` field is handled in `world_manager/succession.rs`; a new state-bearing `Reliable` is journalled or argued out.
- The justfile is canonical: a changed command changes its recipe in the same commit. Builds and tests go through `just`.
- Secrets: never commit, print or log `.env`, `*.tfvars`, a token, a database URL. OpenTofu, not Terraform; CRD objects through the module's local chart, never `kubernetes_manifest`.
- Player-facing text says **Errantry**; dated documents are not edited.
- A decision this plan did not make is recorded in the ledger `.superpowers/sdd/2026-11-15-elastic-1/progress.md` as `Ruling: what — why — cost if wrong`.

## Review Focus

1. **A pod restarts while its old id's succession is still open.** Expected: it registers at once under a new id, owns nothing, and may itself be the heir. Pinned by Task 2 (`a_restarted_host_registers_at_once_beside_its_open_succession`) and Task 12 (the twin's kill step).
2. **A host row is pruned and a new host registers.** Expected: the new id is higher than every id ever issued — a fence, a journal or a prepared-transaction name of the pruned host can never match it. Pinned by Task 2 (`an_id_is_never_issued_twice_after_a_prune`).
3. **The gate host dies holding a login order.** Expected: the login service gets no answer, the join is a 503 and the client's retry is placed by another host; the player has one body. Pinned by Task 6 (`a_gate_host_dying_with_an_order_spawns_the_player_once`).
4. **A host registers while a world cut is under way.** Expected: it is not in that cut, it is in the next, and nothing sent to it meanwhile is lost or counted twice. Pinned by Task 4 (`a_host_joining_during_a_cut_is_in_the_next_one`).
5. **A drain's deadline passes while the coordinator is down.** Expected: the host keeps its players and keeps simulating; it leaves when a leave cut can commit. It never parks players for a cut that cannot happen. Pinned by Task 8 (`a_drain_past_its_deadline_waits_for_the_coordinator`).

## File structure

| Path | Responsibility after this plan |
|---|---|
| `crates/game/src/ownership.rs` | `HostId`; `Static { me, layout }`; `PgClaim::new(me)`; `heir_of` over ids; no host count |
| `crates/world-store/migrations/{pg,sqlite}/0014_host_ids.sql` (new) | `hosts.id`, `hosts.name`, `hosts.drain_at_ms`, `world.next_host_id` |
| `crates/world-store/src/rows.rs`, `handle.rs` | `register_host` issues the id; pruning; the total-crash rule; `heartbeat` returns the drain |
| `crates/world-store/src/operator.rs`, `ctl.rs`, `bin/worldctl.rs` | `retire-host` by id; `drain`; `hosts` |
| `crates/world-store/src/two_phase.rs` | staged parts found by scanning prepared-transaction names |
| `crates/server/src/lib.rs`, `lease.rs`, `claims.rs` | register first, then fence, claims, peers |
| `crates/server/src/peer_link.rs` | peers discovered from `hosts` rows; no pre-made links, no port from a number |
| `crates/game/src/world_manager/{peers,succession}.rs` | peers come and go; `forget_host` |
| `crates/game/src/coordinator/{mod,succession,world_cut,msg}.rs` | no host count; the gate host; `CoordMsg::Login` / `HostMsg::Login` |
| `crates/server/src/coordinator_rt.rs`, `login_link.rs` | the coordinator's login listener; the host's side of the relay |
| `crates/game/src/world_manager/drain.rs` (new) | the drain's state and deadline |
| `crates/game/src/protocol.rs` | `ServerPacket::HostDraining` (last) |
| `crates/client/src/renderer/drain_notice.rs` (new), `crates/slint-ui/ui/game.slint` | the countdown line |
| `justfile`, `scripts/dev_r3.sh` | dev stacks without ordinals |
| `deploy/modules/world/hosts.tf`, `variables.tf`, `outputs.tf`, `coordinator.tf`, `identities.tf` | a Deployment, Service and PodDisruptionBudget per host |
| `scripts/world_update.sh`, `scripts/twin_check.sh`, `deploy/modules/monitoring/` | recipes, release and checks over Deployments |

---

# Phase A — identity

### Task 1: `HostId`, a type for what is a bare `u32` (no behaviour change)

**Files:**
- Modify: `crates/game/src/ownership.rs`, every `u32` host value in `crates/game/src/{peer,coordinator,world_manager}/`, `crates/game/src/protocol.rs` (`HostEndpoint`), `crates/svc-link/src/lib.rs` (`PeerRole::Host`), `crates/world-store/src/**`, `crates/server/src/**`, `crates/client/src/{connections,instance,harness}.rs` and the tests of all four crates.

**Interfaces:**
- Produces, in `game::ownership` (re-exported at the crate root):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HostId(pub u64);
impl HostId { pub const SOLO: HostId = HostId(0); }
impl std::fmt::Display for HostId { /* "h7" */ }
```

- `HostEndpoint { id: HostId, .. }`, `PeerHello { id: HostId, .. }`, `PeerRole::Host { id: HostId, boot: u64, reg: u64 }`, `Owner::Host(HostId)`, `Succession { predecessor: HostId, heir: HostId, survivors: Vec<HostId>, .. }`, `RequestId { host: HostId, .. }`, `Target::Host(HostId)`, `Fence { owner: HostId, .. }`, `HostRow { id: HostId, .. }`, `cut_gid(world_tag: u32, cut: CutId, host: HostId)`.
- In this task the VALUE still comes from `VOXEL_HOST_ORDINAL` (`HostId(ordinal as u64)`) and `VOXEL_HOSTS` still exists. SQL is unchanged (`BIGINT`).

- [ ] **Step 1:** Add `HostId` and change `Ownership::ordinal(&self) -> u32` to `fn me(&self) -> HostId`. Let the compiler list the rest; change every host-valued `u32` (parameters, fields, map keys, `Vec`s, tuple members) to `HostId`. A `u32` that is a count, a super-region size or a port stays. Do not rename SQL columns, env variables or log keys here.
- [ ] **Step 2:** In tests, replace literals with a helper `fn h(n: u64) -> HostId` in each crate's `tests/common` (and `multi_rig.rs`); do not weaken or delete an assertion.
- [ ] **Step 3:** Add `ownership::tests::a_host_id_orders_and_prints_by_its_number`: `HostId(2) < HostId(10)`; `format!("{}", HostId(7)) == "h7"`; a bincode round trip of `HostEndpoint` keeps the id.
- [ ] **Step 4:** Run each touched crate's whole non-client suite once: `just test-one game ""`, `just test-one world-store ""`, `just test-one server "--features test-hooks"`, then `just test-threads`, `just test-r3`, `just test-r4`, `just smoke`, `just client-wasm`. Expected: the same passes and the same known reds as before the task (record the before / after counts in the ledger; the three `lod::tests` and the `stud_refusal` flakes are listed in `TODO.md`).
- [ ] **Step 5:** Commit: `refactor: HostId — the host's number is a type (no behaviour change)`.

### Task 2: The store issues the id

**Files:**
- Create: `crates/world-store/migrations/pg/0014_host_ids.sql`, `crates/world-store/migrations/sqlite/0014_host_ids.sql`
- Modify: `crates/world-store/src/rows.rs` (`register_host`, `hosts`), `handle.rs`, `thread.rs`, `succession.rs` (module notes), `open.rs` (the newest-schema check)
- Test: `crates/world-store/tests/hosts.rs`, `lease.rs`, `succession.rs`

**Interfaces:**
- Produces:

```rust
pub struct NewHost { pub name: String, pub boot: u64, pub peer_addr: Option<String>, pub peer_cert: Option<String>,
                     pub quic: Option<String>, pub webtransport: Option<String>, pub wt_cert_hash: Option<String> }
pub struct Registered { pub id: HostId, pub generation: u64, pub bumped: bool }
// WorldStore:
pub fn register_host(&self, host: NewHost, policy: game::CrashPolicy) -> Result<Registered, StoreError>;
// StoreError gains: NotAlone { live: Vec<HostId> }   (a `static` registration beside a live row)
// HostRow gains: name: String, drain_in_ms: Option<i64>   (None = not draining; may be negative)
```

- Removed: `BootVerdict`, `WaitFor`, `HostBoot`, the `fence` parameter of `register_host`.

**The migration (both engines, same statements):**

```sql
ALTER TABLE hosts RENAME COLUMN ordinal TO id;
ALTER TABLE hosts ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE hosts ADD COLUMN drain_at_ms BIGINT;
ALTER TABLE world ADD COLUMN next_host_id BIGINT NOT NULL DEFAULT 1;
UPDATE world SET next_host_id = (SELECT COALESCE(MAX(id), 0) + 1 FROM hosts);
```

An existing world's rows keep their numbers as ids (0, 1, …): they are old hosts that never register again. `reg` stays in the table and is no longer read. `super_regions.owner`, `in_flight.host` and `successions` need no change (they hold the same numbers).

**`register_host`, one transaction, in this order** (the world row `FOR UPDATE` first, as today):
1. `id = world.next_host_id`; `UPDATE world SET next_host_id = id + 1`.
2. `policy == Static` and another row is live (not clean, `state = live`, `seen_ms` within `HOST_LEASE_MS` of `clock_timestamp()`): return `NotAlone`.
3. No other row is live and at least one row is dirty (decision 4): abandon every open succession as `rollback_world` does; `UPDATE super_regions SET owner = id, epoch = epoch + 1` and `UPDATE in_flight SET host = id` for every dirty row's rows; mark those rows dead; `generation + 1`, `bumped = true`.
4. Otherwise, when `name` is not empty: `UPDATE hosts SET seen_ms = 0` for other dirty live rows of that name (decision 8).
5. Prune: delete every row that is clean or dead, owns no `super_regions` row and no `in_flight` row, and is not the predecessor, the heir or a survivor of an unsettled, unabandoned succession.
6. `INSERT` the new row (no `ON CONFLICT`), `clean = 0`, live, `seen_ms` now.

- [ ] **Step 1: Write the failing tests** (both engines where the file's harness runs both):
  - `hosts::ids_are_issued_in_order_from_the_world_row`: two registrations → ids `h(1)`, `h(2)`; both rows exist.
  - `hosts::an_id_is_never_issued_twice_after_a_prune`: register A; mark A clean with no land; register B (A's row is gone, `hosts()` has one row); register C → `C.id > B.id > A.id`.
  - `hosts::a_migrated_world_keeps_its_rows_and_issues_above_them`: a store at migration 0013 with rows 0 and 1 (one owning a super-region) → after open, `hosts()` ids are 0 and 1, the owner is unchanged, the next registration is `h(2)`.
  - `hosts::a_restarted_host_registers_at_once_beside_its_open_succession`: A (dirty, declared dead, succession to B open) → a registration named like A returns `Registered` with a new id, owns nothing (`owned_by(new)` empty), the succession row is unchanged.
  - `hosts::the_first_host_after_a_total_crash_takes_every_dirty_row`: A and B dirty with lapsed leases, each owning land and one `in_flight` row; C registers → `bumped`, generation + 1, `owned_by(C)` is the union at `epoch + 1`, both `in_flight` rows are C's, A and B are dead; D registers next → not bumped, owns nothing.
  - `hosts::a_clean_stop_keeps_the_leavers_land`: A clean with land, nobody alive; B registers → not bumped, `owned_by(A)` unchanged.
  - `hosts::a_static_host_beside_a_live_row_is_refused`: `NotAlone` naming the live id; nothing written (`next_host_id` unchanged — the transaction rolled back).
  - `hosts::a_same_named_registration_lapses_the_old_row`: A live and dirty named `a`; a registration named `a` → A's `seen_ms == 0`; a row named `b` is untouched; an empty name vouches nothing.
  - `hosts::a_row_named_by_an_open_succession_is_not_pruned`.
- [ ] **Step 2:** Run them (`just test-one world-store "--test it -- hosts::"`, or the file's own `[[test]]` target — see `crates/world-store/Cargo.toml`; with PostgreSQL: `just test-pg`). Expected: FAIL to compile (`NewHost` not found).
- [ ] **Step 3:** Write the migration and `register_host`. Rename `ordinal` → `id` in every statement of `rows.rs`, `succession.rs`, `operator.rs` (`ORDER BY id`). `hosts()` reads `name` and `drain_at_ms - {NOW_MS}` as `drain_in_ms`.
- [ ] **Step 4:** Add the lock walk for steps 3–5 to the module notes of `succession.rs`: the order is `world` → `successions` → `hosts` (several rows) → `super_regions` → `in_flight`; state against each of the two documented deadlock cycles why it is not extended, and correct the stale second cycle noted in the R4 amendments while there.
- [ ] **Step 5:** Run the store's whole suite on both engines (`just test-one world-store ""`, `just test-pg`). Callers in `server` are reordered in Task 3; here keep `server` compiling with a thin adapter in `peer_link::register` (it builds a `NewHost` named from `VOXEL_HOST_ORDINAL`, and uses the returned id only for the heartbeat) so that `just smoke` passes at this commit. Task 3 deletes the adapter.
- [ ] **Step 6:** Commit: `feat(world-store): a host's id is issued at registration and never reused (migration 0014)`.

### Task 3: A host registers first and owns nothing; no ordinal, no host count on the host

**Files:**
- Modify: `crates/server/src/lib.rs` (`run`, ~720–1140), `lease.rs`, `claims.rs`, `peer_link.rs` (`register`, `peer_bind_from`, `peer_cert_hash_file_from`), `webtransport.rs` (`cert_hash_file_names`), `roles.rs`, `crates/game/src/ownership.rs`
- Test: `crates/server/tests/roles.rs`, `hosts.rs`, `crates/game/tests/ownership.rs`

**Interfaces:**
- Consumes: `register_host(NewHost, CrashPolicy) -> Registered` (Task 2).
- Produces:
  - `game::ownership`: `pub struct Static { pub me: HostId, pub layout: Vec<HostId> }` (owner = the rendezvous winner among `layout`); `PgClaim::new(me: HostId)`; `pub fn ownership_for(policy: &str, me: HostId) -> Result<Box<dyn Ownership>, String>` replacing `ownership_from_values` / `ownership_from_env` (`static` → `Static { me, layout: vec![me] }`). `Ownership::hosts()` is removed; `rendezvous_owner(sr, hosts: u32)` becomes `rendezvous_among` only.
  - `server::roles`: `pub fn check_claim_needs(role: Role, policy: CrashPolicy, postgres: bool, login_link: bool) -> Result<(), String>` replacing `check_several_hosts` (a `claim` host needs role `host`, PostgreSQL and `jwt`).
  - `server`: `pub const HOST_NAME_ENV: &str = "VOXEL_HOST_NAME";` (default `host`). `peer_bind_from(value: Option<&str>) -> Result<SocketAddr, String>` (default `127.0.0.1:6500`); `peer_cert_hash_file_from(name: &str, value: Option<&str>)` (default `target/dev-r3/<name>.peer.cert-sha256`); `cert_hash_file_names(name: &str)` (`webtransport-cert-hash-<name>.json`, plus the unsuffixed file when the name is the default).
- Removed: `HOST_ORDINAL_ENV`, `HOSTS_ENV` on the host side (the coordinator's use goes in Task 5), `PEER_PORT_BASE` arithmetic, the registration wait loop.

**The new order in `run` for role `host`:** open the store → resolve staged cuts (Task 5 changes how they are found; until then pass what it needs) → build the client endpoint and the peer listener's address and pin → `register_host` → `Fence { owner: id, .. }`, the ownership (`ownership_for`), the heartbeat (`Heartbeat::new(store, id, boot)`) → claims (`claim` claims nothing at start; `static` claims the extent as today) → peer links. A `NotAlone` is fatal with a message naming `VOXEL_OWNERSHIP=claim`.

- [ ] **Step 1: Write the failing tests:**
  - `ownership::a_static_layout_is_a_fixed_table`: `Static { me: h(1), layout: vec![h(1), h(2)] }` — every super-region's owner is `h(1)` or `h(2)`, the same for both hosts' instances, and both appear over an 8 × 8 range.
  - `roles::a_claim_host_without_postgres_or_jwt_is_refused` (was `several_hosts_refuse_local_auth`).
  - `hosts::two_hosts_started_with_the_same_environment_get_two_ids` (real processes, PostgreSQL): no `VOXEL_HOST_ORDINAL` anywhere; both register, ids differ, both `/readyz` 200.
  - `hosts::a_restarted_host_comes_back_as_a_new_host_owning_nothing`: kill -9 host A after it claimed land; start a process with A's name → new id; after the succession settles the land is the heir's and the entities on it exist once (the existing conservation helper of `takeover.rs`).
  - `roles::a_second_static_host_is_refused_at_registration`: exit code 1, the message names `claim`.
- [ ] **Step 2:** Run: `just test-one game "--test it -- ownership::"`, `just test-one server "--features test-hooks --test roles"`. Expected: FAIL.
- [ ] **Step 3:** Implement. In `multi_rig.rs`, `MultiRig::new` builds `Static { me, layout }` over its hosts' ids; `MultiRig::start(h)` starts a host under a NEW id and returns it (`pub fn start(&mut self, name: &str) -> HostId`); update the tests that restarted "the same" host to use the returned id. A test whose point was "the returning ordinal waits for its succession" is rewritten to assert Review Focus 1 instead — list each in the ledger.
- [ ] **Step 4:** `just test-threads`, `just test-r3`, `just test-r4`, the server's whole suite, `just smoke`.
- [ ] **Step 5:** Commit: `feat(server): a host registers before it claims and is a new host at every start`.

### Task 4: Peers are discovered, come and go

**Files:**
- Modify: `crates/server/src/peer_link.rs` (`Admission`, `PeerLinks`), `crates/game/src/world_manager/peers.rs`, `succession.rs`, `world_cut.rs`, `crates/game/src/peer/journal.rs`
- Test: `crates/game/tests/peer_schedule.rs`, `peer_journal.rs`, `crates/server/tests/hosts.rs`

**Interfaces:**
- Produces:
  - `PeerLinks::start(me: HostId, boot, endpoint, token, store, fence, config, policy, stop)` (no `hosts`); `pub fn refresh(&mut self, rows: &[HostRow]) -> Vec<(HostId, PeerSession, Box<dyn PeerLink>)>` — one new dialing link per live row with an id LOWER than `me` that is not known yet (the newer host dials the older; the rule "the higher dials the lower" is kept). Called from the main loop wherever the directory is read (`claims::read_directory`), never from a timer thread.
  - `Admission::admit`: an inbound `PeerRole::Host { id, boot, .. }` is accepted when the store has a live row with that id and boot; anything else is refused and logged. The `ordinal >= hosts` refusal goes.
  - `WorldManager::forget_host(&mut self, host: HostId)` in `world_manager/succession.rs`: removes the host from EVERY per-host field (the list is the "Per-ordinal fields" of `mod.rs` ~1030–1433: `peers`, `peer_waiting`, `peer_queued`, `peer_endpoints`, `peer_boots`, `peer_synced`, `pair_owns_seen`, `left_peers`, `returning_peers`, `left_land`, `left_land_unread`, `leave_pairs`, `count_pairs`, `early_marks`, `succeeded_peers`, `dead_boots`, and the values of `foreign_running`, `redirected`, `departed`, `handed`, `claim_wanted_sent`, `observer_tickets`, `awaiting_session`, `kicks_pending`). Called at `SuccessionDone` for the predecessor, and for a peer that left clean once it owns nothing. Destructure or exhaustively list so that a new field does not compile unhandled, as `carry_components` does.
  - `PeerJournal::forget(&mut self, peer: HostId)`, called by `forget_host` — only after the cut that settles the succession committed (the reports are read from it until then).

**What is dropped when a dead host is forgotten — argue each in the module notes:** its queued `Reliable`s. State-bearing ones are in the journal and were resolved by the succession; `SpawnOrder` / `SpawnAck` are retried by the login service's client; a `Kick` stays durable in the login service until acked and is sent again. Count them (`peer_dropped_for_dead=` on the `[server]` line).

- [ ] **Step 1: Write the failing tests** (seeded layer unless said):
  - `peer_schedule::a_third_host_joins_two_running_hosts`: `rig.start("c")` → after `refresh` every pair has a session; a body walks from a's land to land c then claims; lockstep and shuffled schedules agree on the final hashes.
  - `peer_schedule::a_host_joining_during_a_cut_is_in_the_next_one`: begin a cut on {a, b}; register and attach c before `CutDone`; the cut commits with parts of a and b only; the next cut has three parts; an entity that crossed a → c between the two exists once after a restore from each cut.
  - `peer_schedule::a_forgotten_host_leaves_no_trace`: after a settled succession of b, no field of a's manager mentions `b` (a debug accessor `WorldManager::mentions(host) -> Vec<&'static str>` listing the fields that still do, `#[cfg(any(test, feature = "test-hooks"))]`); expected empty.
  - `peer_journal::forgetting_a_peer_before_its_settle_cut_is_refused` (a `debug_assert` path is not enough: return `Err`).
  - `hosts::an_unregistered_peer_is_refused` (real processes): a dial with a valid token and an id that has no live row is closed.
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `just test-threads`, `just test-r3`, `just test-r4`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(peer): hosts find each other in the hosts table; a dead host is forgotten after its settle`.

### Task 5: The coordinator without a host count

**Files:**
- Modify: `crates/game/src/coordinator/{mod,succession,world_cut}.rs`, `crates/game/src/ownership.rs` (`heir_of`, `HEIR_WAIT_MS`), `crates/server/src/roles.rs`, `coordinator_rt.rs`, `crates/world-store/src/two_phase.rs`, `operator.rs`, `ctl.rs`, `bin/worldctl.rs`
- Test: `crates/game/tests/coordinator.rs`, `crates/world-store/tests/two_phase.rs`, `worldctl.rs`, `crates/server/tests/coordinator_rt.rs`, `takeover.rs`

**Interfaces:**
- Produces:
  - `two_phase`: `pub fn staged_cut_parts(&self) -> Result<Vec<(CutId, HostId)>, StoreError>` (every prepared transaction whose name `parse_gid` accepts for this world's tag); `recover_staged_cuts(&store)` and `resolve_before_claim(postgres, store)` lose their `hosts` parameter and use it.
  - `roles::coordinator_crash_policy(ownership: &str) -> CrashPolicy` — from `VOXEL_OWNERSHIP` alone (`claim` declares successions, whatever the number of hosts).
  - `heir_of(predecessor: HostId, live: &BTreeSet<HostId>) -> Option<HostId>` (unchanged rule, ids).
  - `worldctl retire-host <url> <host id>`; `worldctl hosts <url>` printing one line per row: `id name state clean land=<n> seen=<age ms> drain=<ms|->`.
- Removed: `Coordinator::set_host_count`, `host_count`, `roles::hosts_beyond_count`, `HEIR_WAIT_MS` and its wait (`land_wanted` declares the `Leave` succession at once), the `VOXEL_HOSTS` read in `roles.rs`, the error texts naming `VOXEL_HOST_ORDINAL`.

- [ ] **Step 1: Write the failing tests:**
  - `coordinator::land_of_a_host_that_left_is_given_at_once`: a clean row with land, `LandWanted` from a live host at `now` → a `Leave` succession is declared in the same `handle` call (was: after 30 000 ms).
  - `coordinator::a_claim_coordinator_declares_successions_with_one_host`: one attached host under `claim`, a second lapses → a succession is declared (was: only with several hosts).
  - `two_phase::staged_parts_of_unknown_hosts_are_found`: stage parts for `h(3)` and `h(41)`; `staged_cut_parts()` returns both; `recover_staged_cuts` resolves both.
  - `worldctl::hosts_lists_every_row` and `worldctl::retire_host_takes_an_id`.
  - `takeover::the_heir_of_a_dead_host_may_be_its_own_restart`: two hosts, kill b, restart b's process (new id c) before the lease lapses → the succession names c or a as heir; either way every identity exists once and the cut after it settles.
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement. `operator.rs`'s offline rule reads "the lowest other live-row id".
- [ ] **Step 4:** The coordinator's and the store's whole suites, `just test-r4`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(coordinator): no host count — cut parts, heirs and leavers are found by id`.

---

# Phase B — the login link and the drain

### Task 6: The login service links to the coordinator

**Files:**
- Modify: `crates/svc-link/src/lib.rs` (`LINK_PROTOCOL_VERSION`, `LinkHello`), `crates/game/src/coordinator/msg.rs`, `mod.rs`, `crates/game/src/world_manager/placement.rs`, `world_link.rs`, `crates/server/src/coordinator_rt.rs`, `login_link.rs`, `lib.rs` (~799–862), `roles.rs`
- Test: `crates/game/tests/coordinator.rs`, `peer_schedule.rs`, `crates/server/tests/login_link.rs`, `coordinator_rt.rs`, `crates/svc-link/tests/link.rs`

**Interfaces:**
- Produces:
  - `svc-link`: `LINK_PROTOCOL_VERSION = 7`; `LinkHello { world: String, .. }` (`LinkHello::new(token, role, world)`); a listener refuses a hello whose `world` is not its own (`""` matches only `""`). The login service passes the world row's `world_id`; hosts and the coordinator pass `VOXEL_WORLD_ID`.
  - `game::world_manager::placement`: `pub enum LoginOrder { Spawn { player_token: String, session_epoch: u64 }, Kick { player_token: String, session_epoch: u64, reason: String } }` (its `Debug` redacts the token, as `LoginToHost`'s does).
  - `CoordMsg::Login(LoginOrder)` and `HostMsg::Login(LoginOut)`, both appended last.
  - `Coordinator::gate_host(&self) -> Option<HostId>`: the lowest attached host whose row is live and not draining (Task 7 supplies "draining"; until then: live). `pub fn login_order(&mut self, order: LoginOrder) -> Option<(HostId, CoordMsg)>`; `HostMsg::Login(out)` is returned to the runtime untouched (`pub fn take_login_out(&mut self) -> Vec<LoginOut>`). The coordinator stores nothing about an order.
  - `server`: env `VOXEL_LOGIN_LINK_BIND` (default `127.0.0.1:6490`) and `VOXEL_LOGIN_LINK_CERT_HASH_FILE` (default `target/dev-r2/login-link.cert-sha256`) on the COORDINATOR: a second listener `LinkListener::bind::<LoginToHost, HostToLogin>` admitting only `PeerRole::Login`. `login_link::order_from(msg: LoginToHost) -> LoginOrder` and `login_link::answer_from(out: LoginOut) -> HostToLogin`.
  - Host side: `CoordMsg::Login` is resolved by the code `serve` uses today (factor it into `pub fn resolve_order(order: LoginOrder, orders: &SpawnOrders, players: &PlayerStore, me: HostId) -> Option<ServerEvent>`) and enters the manager as today's `ServerEvent::SpawnOrder { origin: me, hops: 0, .. }` / `ServerEvent::Kick`. `LoginRelay::pump` sends its `LoginOut`s as `HostMsg::Login` when the role is `host`. `login_session_ended` goes up the host's own coordinator link, never to a peer.
- Removed: `login_link::start_relaying`, `VOXEL_ENTRY_HOST`, `entry_host_from`, `WorldManager::set_entry_host` / `entry_host`, `Reliable::SessionEnded`'s sender (the variant stays in the enum; it is no longer sent). Role `all` keeps `login_link::start` on `VOXEL_HOST_LINK_BIND`.

- [ ] **Step 1: Write the failing tests:**
  - `link::a_hello_for_another_world_is_refused` (`svc-link`).
  - `coordinator::a_login_order_goes_to_the_lowest_live_host` and `coordinator::a_login_order_with_no_host_is_dropped` (`login_order` returns `None`; nothing is queued).
  - `peer_schedule::a_player_parked_on_another_hosts_land_is_placed_through_the_gate`: order at gate a for a player whose record region is b's → one `SpawnAck` reaches the coordinator from a, naming b's endpoint; the player has one body, on b.
  - `peer_schedule::a_gate_host_dying_with_an_order_spawns_the_player_once`: a holds the order (`Placement::Wait`), a is killed; no `SpawnAck` ever arrives; the retry goes to b; after the succession settles the player has exactly one body and one parked-or-live record.
  - `login_link::the_coordinator_relays_a_spawn_and_its_ack` (real coordinator and host, PostgreSQL): a `LoginToHost::SpawnPlayer` on the coordinator's login listener comes back as `HostToLogin::SpawnAck { ok: true, quic: Some(_), .. }`; a `Kick` comes back as `KickAck`.
  - `coordinator_rt::a_host_peer_on_the_login_listener_is_closed` and the reverse.
- [ ] **Step 2:** Run them. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `just test-r2`, `just test-r3`, `just test-r4`, `just test-one login ""` (the login service is unchanged; its suite proves it), `just smoke`.
- [ ] **Step 5:** Commit: `feat(link): the login service links to the coordinator, which relays to a live host (link protocol 7)`.

### Task 7: The drain mark in the store and `worldctl drain`

**Files:**
- Modify: `crates/world-store/src/rows.rs` (`heartbeat`, `claim_free`'s guard), `handle.rs`, `operator.rs`, `ctl.rs`, `bin/worldctl.rs`, `crates/server/src/lease.rs`, `crates/game/src/coordinator/succession.rs` (`LeaseWatch` rows carry `draining`)
- Test: `crates/world-store/tests/lease.rs`, `worldctl.rs`, `directory.rs`, `crates/game/tests/coordinator.rs`

**Interfaces:**
- Produces:
  - `pub struct Beat { pub live: bool, pub drain_in_ms: Option<i64> }`; `heartbeat(&self, id: HostId, boot: u64) -> Result<Beat, StoreError>` (was `bool`); the remaining time is computed in SQL (`drain_at_ms - {NOW_MS}`) — a host never compares its own clock with the database's.
  - `operator::drain_host(pool, id: HostId, in_ms: u64) -> Result<(), StoreError>`: `UPDATE hosts SET drain_at_ms = {NOW_MS} + $2 WHERE id = $1 AND clean = 0 AND state = live` — one row, one statement, unfenced like the heartbeat; refuses (`NotDrainable`) a row that is not live. `drain_host(.., 0)` on a draining row brings the deadline forward; there is no un-drain in E1.
  - `worldctl drain <url> <host id> [--in <secs>]` (default `DRAIN_WARNING_SECS`); `pub const DRAIN_WARNING_SECS: u64 = 300;` in `game::ownership`.
  - `claim_free`'s `FOR SHARE` guard on the claimer's row also requires `drain_at_ms IS NULL`: a draining host wins no new land. `Coordinator::gate_host` skips a draining row.
- [ ] **Step 1: Write the failing tests:** `lease::a_beat_reports_the_drain_and_its_remaining_time` (drain in 60 000 ms → `Some(d)` with `59_000 <= d <= 60_000`; no mark → `None`); `directory::a_draining_host_claims_nothing` (its `claim_free` returns no rows; the land stays free); `worldctl::drain_marks_a_live_host_and_refuses_a_dead_one`; `coordinator::a_draining_host_is_not_the_gate`.
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement; add the statement to the lock notes (one `hosts` row, wants nothing else).
- [ ] **Step 4:** The store's whole suite on both engines, `just smoke`.
- [ ] **Step 5:** Commit: `feat(world-store): a host can be marked draining; a draining host wins no land`.

### Task 8: The host drains with a warning

**Files:**
- Create: `crates/game/src/world_manager/drain.rs`
- Modify: `crates/game/src/world_manager/mod.rs`, `placement.rs`, `crates/game/src/protocol.rs`, `crates/server/src/lib.rs` (main loop), `cuts.rs` (`leave_world`), `health.rs`
- Test: `crates/game/tests/peer_schedule.rs`, `world_manager.rs`, `crates/server/tests/hosts.rs`

**Interfaces:**
- Consumes: `Beat.drain_in_ms` (Task 7).
- Produces:
  - `ServerPacket::HostDraining { at_tick: u64 }`, appended after `HomeHop`.
  - `WorldManager::drain_at(&mut self, at_tick: u64)` (idempotent; a later call with an earlier tick wins), `pub fn draining(&self) -> Option<u64>`, `pub fn drain_due(&self) -> bool` (`current tick >= at_tick` AND the coordinator link is up), `pub fn drain_remaining_secs(at_tick: u64, now_tick: u64) -> u64` (free function in `game`, rounds up, 0 when past; `TICK_RATE` is a period in ms).
  - The server's main loop: on a beat with `drain_in_ms = Some(d)`, `drain_at(now_tick + max(d, 0) / TICK_RATE)`. When `drain_due()`, it runs the existing `cuts::leave_world` once; on `ServerExit::Clean` with a committed leave it does NOT exit: it stops the heartbeat, sets health to `drained` (`/readyz` 503 body `drained`, `/livez` 200, `mmo_waiting{why="drained"}`) and idles until a stop signal, then exits 0. A leave that did not commit ends the process as today.
  - The packet is sent to every session whose home is this host when the drain is first known, and to each session that becomes home here afterwards (a join, a hand-off in). Never to an observer session.
  - While draining: a spawn order for a player this host does not hold is placed as today (the store refuses the claim, so free land is another host's to claim — `Placement::Host` or `Wait`). Hand-offs in and out work as before.
- [ ] **Step 1: Write the failing tests:**
  - `world_manager::drain_remaining_rounds_up`: with `TICK_RATE` 50 ms, `drain_remaining_secs(1_000, 0) == 50`, `(1_001, 0) == 51`, `(10, 20) == 0`.
  - `world_manager::a_home_session_is_told_the_drain_once_and_a_late_joiner_too`.
  - `peer_schedule::a_warned_drain_moves_its_players_and_loses_nothing`: two hosts, a player and a pile of items on a's land; `drain_at(T)`; before T the player moves and mines; at T a leaves with a committed cut; the player's next spawn order is placed on b after the `Leave` succession; the player stands where it stood at T, the items exist once, `items_destroyed == 0`.
  - `peer_schedule::a_drain_past_its_deadline_waits_for_the_coordinator`: the coordinator link is down at T → no player is parked, the regions keep ticking; link up → the leave happens.
  - `hosts::a_drained_host_stays_up_unready_and_exits_zero_on_sigterm` (real processes): after `worldctl drain <id> --in 1`, the host's log has the leave line, `/readyz` answers 503 `drained`, the process is alive 3 s later, SIGTERM → exit 0, and no new `hosts` row appeared.
  - `hosts::sigterm_during_the_warning_leaves_at_once`.
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement. `drain.rs` holds only the deadline and "told" set; the leave itself stays in `cuts.rs`.
- [ ] **Step 4:** `just test-threads`, `just test-r3`, `just test-r4`, `just smoke`.
- [ ] **Step 5:** Commit: `feat(server): a marked host warns its players, leaves at the deadline and waits to be removed`.

### Task 9: The client shows the countdown

**Files:**
- Create: `crates/client/src/renderer/drain_notice.rs`
- Modify: `crates/client/src/instance.rs` (~2606, beside `ServerPacket::Notice`), `renderer/bridge.rs`, `renderer/slint_overlay.rs` (~2127), `renderer/mod.rs`, `connections.rs` / `harness.rs` (`HostEndpoint.id`, done in Task 1 — only what the packet needs here), `crates/slint-ui/ui/game.slint`

**Interfaces:**
- Consumes: `ServerPacket::HostDraining { at_tick }`, `game::drain_remaining_secs`.
- Produces: `ClientUpdateEvent::HostDraining { at_tick: u64 }` (only from the HOME connection); resource `DrainNotice { at_tick: Option<u64> }`, registered with `add_session_reset` and cleared when the home connection changes (`HomeHop`, a rejoin); `pub fn drain_line(remaining_secs: u64) -> String`; a Slint property `drain-line: string` on the game screen shown above `game-status` when not empty (bind `visible` on the text element, never on the component root).
- Exact copy: `"This area moves to another server in {m}:{ss}. You will reconnect for a moment."`; at 0: `"This area is moving to another server…"`.
- [ ] **Step 1:** Write `drain_notice::tests::the_line_counts_down_in_minutes_and_seconds` (`drain_line(299)` contains `4:59`; `drain_line(5)` contains `0:05`; `drain_line(0)` is the moving text) and `instance::manager_tests::a_drain_from_an_observer_connection_is_ignored`. Client tests are ask-first: build them with `just test-one client "--lib --no-run"` and tell Dawid the two names to run.
- [ ] **Step 2:** Implement; the line is recomputed from the predicted world tick (`PredictedClock`), not from wall time.
- [ ] **Step 3:** `just smoke`, `just slint-wasm` (a Slint source changed), `just client-wasm`.
- [ ] **Step 4:** Commit: `feat(client): a countdown while the home server drains`.

---

# Phase C — running it

### Task 10: Dev stacks without ordinals

**Files:**
- Modify: `justfile` (`host_env_sh` ~1225, `coordinator` ~1201, `host` ~1283, `dev_r3_launch_sh` ~1349, `dev-r3` ~1383, `go-nogo-r3` ~1430, `dev-r2`, `test-r2|r3|r4`), `scripts/dev_r3.sh`

**Interfaces:**
- Produces: `just host <index> [delay] [ownership]` (the index only picks ports and the name `h<index>`: `VOXEL_HOST_NAME=h$i VOXEL_SERVER_PORT=$((6466+10*i)) VOXEL_WT_PORT=$((6467+10*i)) VOXEL_PEER_BIND=127.0.0.1:$((6500+i)) VOXEL_PEER_CERT_HASH_FILE=$dir/h$i.peer.cert-sha256`); `just coordinator [ownership]` (adds `VOXEL_LOGIN_LINK_BIND=127.0.0.1:6490 VOXEL_LOGIN_LINK_CERT_HASH_FILE=$dir/login-link.cert-sha256`); `just dev-r3 <hosts> [delay]` unchanged in use; `just world-hosts-dev` → `worldctl hosts` on the dev database; `just drain-dev <host id> [secs]`.
- `scripts/dev_r3.sh`: `host_pin` becomes `$dir/login-link.cert-sha256` (written by the coordinator); `world add dev … --link 127.0.0.1:6490 --link-cert …` without `--quic` / `--wt` (the ack names the endpoint); a host that exits 0 after a drain is NOT restarted; exit 3 / 5 / 137 restart rules stay, under any ownership word that is `claim`.
- [ ] **Step 1:** Check what the login service does with a world row that has no `--quic` / `--wt` and an ack without an endpoint (`crates/login/src/api.rs:415–430`): add `api::tests::a_spawn_ack_without_an_endpoint_is_world_unavailable` (503) if it is not already pinned.
- [ ] **Step 2:** Change the recipes and the script; remove every `VOXEL_HOST_ORDINAL`, `VOXEL_HOSTS`, `VOXEL_ENTRY_HOST` (`grep -rn "VOXEL_HOSTS\|VOXEL_HOST_ORDINAL\|VOXEL_ENTRY_HOST" justfile scripts crates deploy` → only dated documents and `crates/client/src/renderer/autopilot.rs`'s comment, which is corrected).
- [ ] **Step 3:** `just test-r2`, `just test-r3`, `just test-r4`; `bash -n scripts/dev_r3.sh`. A live `just dev-r3 2` is Dawid's (Task 13).
- [ ] **Step 4:** Commit: `chore(just): dev stacks start hosts by name; the login link is the coordinator's`.

### Task 11: The world module — a Deployment, Service and PodDisruptionBudget per host

**Files:**
- Modify: `deploy/modules/world/hosts.tf`, `variables.tf`, `outputs.tf`, `main.tf`, `coordinator.tf`, `identities.tf`, `charts/world-monitor/templates/servicemonitor.yaml`, `deploy/envs/local/world/main.tf`, `deploy/envs/orion/world/main.tf`

**Interfaces:**
- Produces:
  - `variable "hosts"`: `map(object({ quic_node_port = number, wt_node_port = number }))`; keys match `^[a-z][a-z0-9]{0,11}$`; at least one; ports distinct and in 30000–32767. Both roots use `{ a = { 30466, 30467 }, b = { 30468, 30469 } }`.
  - Per key `k`: `kubernetes_deployment_v1.host[k]` named `host-<k>` (1 replica, strategy `Recreate`, `termination_grace_period_seconds = 100`, labels `app.kubernetes.io/component = host`, `errantry/host = <k>`, `wait_for_rollout = false`, `ignore_changes` on the image), env `VOXEL_HOST_NAME=<k>`, `VOXEL_ADVERTISE_QUIC=<public_host>:<quic_node_port>`, `VOXEL_ADVERTISE_WT=https://<public_host>:<wt_node_port>/`, `VOXEL_ADVERTISE_PEER=$(POD_IP):6500` and the rest as today minus `VOXEL_ENTRY_HOST`, `VOXEL_HOST_LINK_*`, the `sh -c` wrapper and the `host-endpoints` ConfigMap; `kubernetes_service_v1.host_endpoint[k]` named `host-<k>` (NodePort, `external_traffic_policy = "Local"`, selector the two labels); `kubernetes_pod_disruption_budget_v1.host[k]` (`max_unavailable = 0`, selector `errantry/host = <k>`).
  - The headless Service `hosts` stays for the ServiceMonitor (health port only, selector `component = host`, `publish_not_ready_addresses`).
  - `local.server_env` loses `VOXEL_HOSTS`.
  - Coordinator: container port `login` 6490/UDP, `VOXEL_LOGIN_LINK_BIND=0.0.0.0:6490`, `VOXEL_LOGIN_LINK_CERT_HASH_FILE` on the Secret renamed `login-link-identity` (was `host-link-identity`, now mounted in the coordinator only); the `coordinator` Service gains the port.
  - Outputs: `login_link = "coordinator.<ns>.svc.cluster.local:6490"` (was `host_link`), `login_link_cert_pem`, `endpoints` (a map by host name), `host_names`.
- [ ] **Step 1:** Rewrite `hosts.tf`; update the root headers: an apply still owns the replica counts (one per Deployment), and **an apply that changes the host pod template restarts every host at once** (each leaves through its cut) — apply with the world running and no update under way, or stopped on purpose.
- [ ] **Step 2:** `tofu -chdir=deploy/envs/local/world validate` and `tofu -chdir=deploy/envs/orion/world validate` (after `init -backend=false`); `tofu fmt -check -recursive deploy/modules/world`.
- [ ] **Step 3:** `just orion-plan world` is allowed (read-only) only once the orion root has state; today it has none — skip and say so in the ledger.
- [ ] **Step 4:** Commit: `feat(deploy): one Deployment, Service and PodDisruptionBudget per host; the login link is the coordinator's`.

### Task 12: Recipes, the release script, the twin's checks, the alert

**Files:**
- Modify: `justfile` (`world-status`, `world-stop`, `world-start`, `world-register`, `world-logs`, `world-metrics`, `twin-up` only if ports change — they do not), `scripts/world_update.sh`, `scripts/twin_check.sh`, `deploy/modules/monitoring/values/kube-prometheus-stack.yaml.tftpl` (~166)

**Interfaces:**
- Produces:
  - Selector everywhere: `-l app.kubernetes.io/component=host`. Stop: `kubectl scale deployment -l … --replicas=0`, wait until no host pod is left; start: `--replicas=1`, wait until every host Deployment has `status.readyReplicas == 1`. The host count for a restore is the number of host Deployments (`HOSTS_REPLICAS` goes; the script's failure hint is "re-run").
  - `world_update.sh`: `set image` on every host Deployment while scaled to 0 (a `Recreate` Deployment at 0 replicas starts nothing); the "server images differ" check covers every host Deployment; `release-history` fields unchanged.
  - New recipes: `world-hosts` (`worldctl hosts` through the toolbox), `world-drain <name> [secs]` — finds the live row named `<name>` in `worldctl hosts`, runs `worldctl drain <id> --in <secs>`, follows the pod's log until the leave line, prints it, and prints the next step (`remove "<name>" from var.hosts and apply`, or `kubectl rollout restart deployment/host-<name>` to bring it back as a new host). It deletes nothing.
  - `world-register` prints `world add <id> --name … --link coordinator.<ns>.svc.cluster.local:6490 --link-cert <sha256 of login-link-identity's cert>` with no `--quic` / `--wt`.
  - `world-logs` / `world-metrics` take `host-<name>` or `coordinator` and resolve the pod by label.
  - Alert `WorldHostsBelowReplicas`: `max by (namespace) (mmo_hosts_live{role="coordinator"}) < on (namespace) count by (namespace) (kube_deployment_spec_replicas{namespace=~"world-.*", deployment=~"host-.*"} > 0) > 1`; its summary says "than its host Deployments want".
  - `twin_check.sh` steps, replacing every `hosts-0` / `hosts-1`: forge-wait on both Deployments; kill `host-b`'s pod (`--force --grace-period=0`) → a new pod is Ready WITHOUT waiting for the succession, `worldctl hosts` shows a third id, the old one dead, then gone after the settle; `host-a`'s pod uid and restart count unchanged; a clean leave of `host-a` (scale to 0, then 1); a coordinator restart; stop / start; the `/login` proxy's scope; the register line (`--link coordinator\.$ns\.svc\.cluster\.local:6490`, no `--quic`); **new:** `worldctl drain` of `host-b`'s id with `--in 5` → its `/readyz` body is `drained`, the pod is not restarted, no new `hosts` row; **new:** every host has a PodDisruptionBudget whose `status.disruptionsAllowed` is 0 (and, if this k3s evaluates budgets on a server-side dry run, `kubectl drain <node> --dry-run=server` names the host pods as not evictable).
- [ ] **Step 1:** Change the scripts and recipes; `bash -n` both scripts; `promtool check rules` on the rendered rule file as R5 did (in a container).
- [ ] **Step 2:** `just twin-up && just twin-images && just twin-apply && just twin-check`. Expected: `PASS`. Record the duration and each step's line in the ledger. `just twin-down`.
- [ ] **Step 3:** Commit: `feat(ops): world recipes, the release script and the twin's checks over host Deployments; a warned drain recipe`.

### Task 13: Documentation and closing

**Files:**
- Modify: `CLAUDE.md` (Deployment chain, the world's operator recipes, "Many hosts", "Link rules", "World on k8s variables", the client's Connections paragraph where it says ordinal), `TODO.md` (an E1 block: built, hand steps, known limits; strike the limits this plan removes), this plan ("As built").

- [ ] **Step 1:** Run each touched crate's whole non-client suite once more (`game`, `world-store` on both engines, `server` with `test-hooks`, `login`, `svc-link`), `just test-threads`, `just test-r2`, `just test-r3`, `just test-r4`, `just determinism-wasm`, `just smoke`, `just client-wasm`. Record counts against Task 1's baseline.
- [ ] **Step 2:** Write the known limits into `TODO.md`: after a total crash the first host back holds all claimed land (until E3); a login order needs the coordinator (logins stop while it is down — E2); one gate host at a time (its death costs the orders it held: a 503 and a retry); a `Leave` succession gives ALL of a leaver's land to the first host that asks; no un-drain; a drained pod stays until removed; a PodDisruptionBudget of 0 blocks `kubectl drain` of the node until hosts are drained and removed by hand; `boot` is still carried beside the id; `Reliable::SessionEnded` is dead wire; old clients and old worlds' running hosts are incompatible (stop, upgrade, rejoin).
- [ ] **Step 3:** Update `CLAUDE.md` (stage with a filtered patch — it holds Dawid's hunks).
- [ ] **Step 4:** Report to Dawid what is his to run: the client tests of Task 9; `just dev-r3 2` + `just run-online` — kill -9 a host (it returns as a new host; the client rejoins), `just drain-dev <id> 60` (the countdown, the reconnect, standing where it stood); `just go-nogo-r3 0|1|2|5`, pooled, against 2026-11-13 (the plan changes who owns land under a moving player); the twin with a player; nothing on orion.
- [ ] **Step 5:** Commit: `docs: elastic 1 — hosts by id, the coordinator's login link, the warned drain, as built`.

---

## Not in this plan

- Several coordinator replicas; the request memo and history in the database (E2).
- The coordinator assigning land, releasing idle land, load in the heartbeat row (E3).
- `desired_hosts`, the reconciler, scale to zero (E4). Scaling here is editing `var.hosts` and applying.
- Moving occupied land without a reconnect (E5). PostgreSQL consolidation (E6).
- Several nodes, host networking, a name per node for the WebTransport certificate.
- Removing `boot` and `hosts.reg`.

## Self-review notes

- **Handoff E1 scope → tasks:** identity by id and every re-key (T1–T5: `hosts`, peer sessions and resume, `departed` and the other per-host fields, the host count, cut parts, the journal, succession rows; observer tickets carry no host and need nothing beyond `HostEndpoint.id`); a dead host never returns as itself (T2, T3); discovery through `hosts`, addresses from the environment (T3, T4); the login link to the coordinator and where placement lives (T6, decision 9); the warning (T7–T9); OpenTofu, budgets, UDP ports (T11, decision 12).
- **"Must settle":** the migration (T2; rows keep their numbers, the counter starts above them, both engines); `Static` (decision 7); one heir per predecessor (decision 3).
- **Design §6 rows in E1's reach:** host dies / hung host (T3, T5 tests, `takeover.rs`); dies mid-save (unchanged, T5 finds staged parts without a count); dies while draining (`hosts::sigterm_during_the_warning…` covers the stop; a crash during the warning is an ordinary succession); eviction → SIGTERM → leave inside the grace period, a budget per host (T8, T11, T12); two hosts told the same land (unchanged fencing; T7 adds the draining guard).
- **Names across tasks:** `HostId` / `HostId::SOLO` (T1) → all; `NewHost` / `Registered` / `NotAlone` (T2) → T3; `Static { me, layout }` / `ownership_for` / `check_claim_needs` (T3) → T5, T10; `PeerLinks::refresh` / `forget_host` (T4) → T5, T8; `staged_cut_parts` / `coordinator_crash_policy` (T5); `LoginOrder` / `CoordMsg::Login` / `HostMsg::Login` / `gate_host` (T6) → T7, T10–T12; `Beat` / `drain_host` / `DRAIN_WARNING_SECS` (T7) → T8, T12; `ServerPacket::HostDraining` / `drain_remaining_secs` (T8) → T9.
- **Proportion:** the plan is longer than the E1 section of the handoff because the ordinal is in four crates; steps name tests, signatures, SQL shapes and the one registration algorithm a signature does not determine, and leave bodies to the implementer.

## As built (2026-11-16)

Where this section and the plan above differ, this section is what was built; the body above is left as the plan that was executed (its checkboxes were never ticked).

**Status.** Tasks 1–13 are built and committed on `develop` (`dbf1be5b..HEAD`, start `eb90005c`), each task reviewed (spec and quality) with its fix rounds recorded in the ledger `.superpowers/sdd/2026-11-15-elastic-1-hosts-as-deployments/progress.md`; the per-task reports sit beside it. The whole-branch review is pending. Execution was subagent-driven on `develop`, no worktree; Dawid committed beside it (`6faaea0c`, `88aa5fca` are his, inside the range). Order actually run: T1 → T2 → T3 → T4 → T5 → T6 → **T10** → T7 → T8 → T9 → T11 → T12 → T13 (ruling 14). Run: every suite of the final table below, `tofu fmt` / `validate` of both world roots, `promtool` on the alert, and on a fresh k3d twin `just twin-check` (PASS, 464 s). NOT run: see "Not run / not proven". Nothing has been applied to orion.

### Tasks, commits, what differs from the plan text

| Task | Commits | As built |
|---|---|---|
| 1 `HostId` | `dbf1be5b` | `HostId(u64)` everywhere a host was a `u32`. Two non-mechanical changes: the coordinator state blob is format 2 (`RequestId.host` widened; v1 decodes through a frozen copy) and the in-flight seq key packs the host into the high 32 bits (bounded in T2). Logs print `h7`, operator text the bare number. |
| 2 the store issues the id | `2d18afb1`, `1f29df77` | `register_host(NewHost, policy) -> Registered` in one transaction, migration `0014_host_ids` (rows keep their numbers, `next_host_id` starts above them). Order inside it (ruling 6): issue the id → the same-name lapse → the `static` refusal (`NotAlone`) → the total-crash rule → prune → insert. `HOST_ID_MAX = 2^31 − 1` (ruling 5). `HostRow.reg` removed from the struct (the column stays). |
| 3 register first, own nothing | `9b2e490a`, `e8a2b61f`, `3bf5901c`, `a2e6c290` | `server::run` registers before it claims or links; `Static { me, layout }`; `check_claim_needs`; `allinone` on a host refused (ruling 7); interim entry host = lowest live id until T6 (ruling 8). A conservation hole found and fixed: a lone `static` host restarted after a clean leave never loaded its old id's in-flight rows — a lone static host's fence is `all_hosts` (`Static::runs_every_host`). Brought forward from T5: `staged_cut_parts` and the count-free `resolve_before_claim` (ruling 1). |
| 4 discovery, come and go | `d10ca2fd`, `37359ca4` | `PeerLinks::refresh` from the main loop's directory reads; `forget_host` / `mentions` generated from one field list `host_fields!` (compile-enforced, no `..`). The mid-cut join: migration `0015_cut_begins` (the coordinator records `next_host_id` at each cut's begin; `decide_cut` may omit only ids at or above it), participants freeze sessions with hosts outside the cut and refuse their part after unsaved state traffic with one (`unsaved_traffic`); a start resolves only staged cuts no live host has a part in. Fix round: seeded proofs of the mid-cut join; a state-bearing `Reliable` to a forgotten host is never dropped silently (`to_forgotten`). |
| 5 coordinator without a count | `3cfd1693` | `HEIR_WAIT_MS`, `HOSTS_ENV`, `set_host_count`, `heir_wait_for` gone; `leave_wanted` declares a `Leave` at once. `coordinator_crash_policy(Option<&str>) -> Result<CrashPolicy, String>` (a bad word is still refused; the plan's `(&str) -> CrashPolicy` could not refuse); `policy_agrees` always compares (a `static` coordinator now refuses a `claim` host). `worldctl hosts <url>` (adds `seen=vouched`), `retire-host <url> <host id>`. |
| 6 login link to the coordinator | `6e52bd40`, `0c9b6ed6` | `LINK_PROTOCOL_VERSION` 7 (also covers T1's widened ids), `LinkHello.world` (`""` matches only `""`). `LoginOrder`, `CoordMsg::Login`, `HostMsg::Login`, `Coordinator::gate_host` / `takes_logins`; the coordinator stores nothing per order; `SessionEnded` queued (bounded) on host and coordinator; `KickAck` only via the gate. `resolve_order` takes the event sender too (a kick sends its supersede first). Role `all` keeps `VOXEL_HOST_LINK_BIND`. Fix round: the only end-to-end login→relay test was vacuous (world `""` vs `"hosts"`), made real; drops counted, not logged per message. |
| 10 dev stacks (run 7th) | `43338f3f` | `just coordinator [ownership]`, `just host <index> [delay] [ownership]` (names `h<i>`, ports `6466+10i` / `6500+i`), `dev-r2` / `dev-r3` / `go-nogo-r3` register the world with `--link 127.0.0.1:6490 --link-cert <login-link pin>`, `world-hosts-dev`. Login gap fixed in `crates/login`: a linked world whose ack names no endpoint is a 503 before the refresh rotation (was a 200 with an empty ticket). |
| 7 the drain mark | `8d5ede74`, `0487b47b` | `Beat { live, drain_in_ms }` from the heartbeat statement itself; `drain_host` in `rows.rs` for both engines (not a Postgres-only `operator::drain_host`); `LEAST` — a mark only moves earlier; `DRAIN_MAX_SECS` 7 days; `worldctl drain`. The claim guard reads `drain_at_ms` under the same lock and answers an empty fence (a `WHERE` refusal would have been `Fenced`, and a fenced host exits). Fix round (ruling 15): a draining host is in no claim preference and asks for nothing; an empty claim is paced `CLAIM_NOTHING_WON_RETRY_MS`. `drain-dev` recipe here (moved from T10). |
| 8 the warned drain | `523a95f1` | `world_manager/drain.rs`, `server/src/drain.rs`, `ServerPacket::HostDraining { at_tick }` (appended; the only wire change), `/readyz` `drained`. Only `claim` hosts act on a mark. After a committed drain leave the process drops everything, makes no store call and idles until a stop signal; an uncommitted one ends as today (exit 0, dirty row). An unanswered `ClaimWanted` re-reads the directory after `CLAIM_UNANSWERED_REREAD_MS`. |
| 9 the countdown | `52d8429b`, `198c2e34` | The event is `game::ClientUpdateEvent::HostDraining { remaining_secs }` (ruling 16: that enum lives in `game`); the game thread owns deadline and clock and sends once per second, the render side formats (`renderer/drain_notice.rs`). Fix round: the observer test starts the clock (it was vacuous); the line wraps and the status line follows its height. Client tests compiled, NOT run. |
| 11 world module | `28ab7b49` | `var.hosts` is a MAP (key = `VOXEL_HOST_NAME`, node ports per host); per key a Deployment (1 replica, `Recreate`, grace 100 s), NodePort Service and PodDisruptionBudget (`max_unavailable` 0) `host-<k>`; the coordinator gets `VOXEL_WORLD_ID`, the login port 6490 and Secret `login-link-identity`; ConfigMap `host-endpoints`, StatefulSet `hosts`, Secret `host-link-identity` gone. Validated only (no cluster in this task). |
| 12 recipes, release, twin, alert | `0185fb2f` | `world-hosts`, `world-drain <name> [secs]`, `world-logs|world-metrics <host-<name>|coordinator|pod>`; `world-start` waits per Deployment and never on a drained host; `world_update.sh` without `HOSTS_REPLICAS`, refusing a coordinator at 0 (found on the twin: a re-run after `world-stop` started hosts with no coordinator); `twin_check.sh` adds the budgets and a drain; the alert `WorldHostsBelowReplicas` counts host Deployments (parenthesised, see defects). `twin-check` PASS 464 s; an old `world-status` bug fixed (every Ready pod printed "(no answer)"). |
| 13 docs and closing | `8d920710`, this commit | `CLAUDE.md`, `TODO.md`, this section; the test warnings this plan introduced. |

### Rulings (ledger; what — why)
1. T3 takes `staged_cut_parts` and the count-free `resolve_before_claim` from T5 — the host could not drop its count otherwise.
2. In T3 `PeerLinks::start` dials the live lower rows read once at start; T4 replaced it with `refresh` — kept test-r3 green between the two.
3. T10's grep for removed env names excludes `deploy/`; T11 runs it there — task order.
4. The twin asserts a dead row is dead, and gone only after a later registration — pruning happens at registration by T2's design.
5. No id is issued at or above `HOST_ID_MAX` (`StoreError::Corrupt`) — the in-flight key packs the host into 32 bits; re-keying is E5's.
6. The same-name lapse runs BEFORE the live count, and `VOXEL_HOST_NAME` defaults to empty — a restarted process's own old row must not make it `NotAlone` or landless; two unnamed hosts must never vouch each other.
7. A host under `allinone` is refused (was a warning) — a host's claims and parts must be its issued id's.
8. Interim entry host = lowest live id at start until T6 — kept test-r3's logins working; never released between tasks.
9. "Real processes" in `hosts.rs` / `takeover.rs` means the in-process real hosts over real links; exit codes are asserted as the `fatal` message; the twin covers real pods.
10. T3 only stops the mid-cut test asserting ordinal semantics; T4 rewrote it as `a_host_joining_during_a_cut_is_in_the_next_one`.
11. No work on carrying old worlds across this plan (Dawid, 2026-11-15: wipe and re-create); what T1/T2 already built for it stays.
12. A world rollback is followed by a second generation raise when the first process back finds nobody live — the state is the last cut either way; avoiding it needs the rollback to mark rows.
13. A subagent's commit carries its own harness attribution — the line is truthful.
14. T10 ran right after T6's review — after `6e52bd40` the dev stacks did not start at all.
15. A draining host is in no claim preference (its own included), asks for no claim, and an empty answer is paced — fixed in T7, not T8: the preference is where "wins no land" lives.
16. T9 may append one variant to `game::ClientUpdateEvent` — the dispatch had wrongly assumed that type was the client's; edits one file per call after a refused multi-file shell edit.
17. T11/T12 touch no file of Dawid's uncommitted world panel and write down what it must change — his panel work overlapped the world module.

### Plan defects found in execution
- **The vouch order** (decision 8 / T2's step order): the live count before the name vouch made a restarted lone host `NotAlone` or landless — ruling 6.
- **A draining host stayed the preferred claimer** (T7/T8): its share of free land would have been claimed by nobody for the whole notice, and it hot-looped claim cycles — ruling 15.
- **The alert's operator precedence** (T12): `live < count … > 1` parses as `(live < count) > 1` and never fires for 1 of 2 hosts — parenthesised, proven with `promtool test rules`.
- **`ClientUpdateEvent` lives in `game`**, not the client (T9) — ruling 16.
- **"Real processes"** in the tests meant the in-process rig (no test spawns the binary) — ruling 9.
- Smaller: the T7 guard as a `WHERE` would have fenced (and so stopped) a draining host; T5's `coordinator_crash_policy` signature could not refuse a bad word; T6's `resolve_order` needed the event sender; T11's coordinator needed `VOXEL_WORLD_ID` (only hosts had it).

### Deferred minor findings (for the whole-branch review)
- **T2**: four `succession::` store tests failed once in one combined run (not reproduced in 9+ runs); slow-test tags on the new `hosts.rs` store tests unmeasured.
- **T3**: `/readyz` is 200 before a static host's claim (`set_registered` before `HostClaims::claim`); `a_host_restarted_cleanly_is_a_new_host_owning_nothing` dropped its fence assertion, `counts_of_a_new_host_start_at_zero` the late slime; `Takeover::restart` never shuts the dead process's store handle; aliases in `run` (`several`, `host_ordinal`); rigs issue ids from 0 (`HostId::SOLO`, which production never has).
- **T4**: `replaced_host_exits_fenced_at_its_next_heartbeat` should assert the abort's cause; `linked_or_outside` never checks the newcomer gets linked after the cut; `resolve_before_claim` lease race after a total crash restarted within the lease (waits up to `CLAIM_LOCK_TIMEOUT_MS`, exit 1, converges); `abort_cut(cut, 0)` → `SWEEP_ABORT_TICK`, stale `cuts.rs` docs; the admission cache can admit a boot declared dead since the last directory read; a refused forget is not retried and `forget_landless_leavers` warns every period while the journal refuses; `cut_begins` rows never pruned; `PeerLinks.dialing` keeps every id ever dialed.
- **T5**: `the_heir_of_a_dead_host_may_be_its_own_restart` duplicates a scenario and never makes the restart heir deterministically; no integration test runs a real `run_coordinator` with one claim host; the "back" guard in `leave_wanted` is untested; offline `retire-host` writes a succession the next total crash always abandons; stale wording (`cuts.rs` "(several hosts)", `SEVERAL_HOSTS_NEED_TWO_PHASE`); `login_on_departed_land`'s adoption wait accepts `successions_open_count() == 0` while its message says "the heir adopted".
- **T6**: the gate host resolves each spawn with a synchronous player-store read on its main loop; the gate may be a host without a service token (orders dropped, joins 503); M5's seeded test infers the owner's epoch rather than asserting it; the 503-vs-timeout discriminator is a 3 s wall-clock bound.
- **T7**: the `rows.rs` engine is chosen by `$share_lock.is_empty()`; `CLAIM_NOTHING_WON_RETRY_MS` also follows a lost claim race.
- **T8** (the first two weigh for k8s): `/livez` is not beaten during the drain's leave (a leave past `LIVEZ_STALL_MS` turns a committing drain into a crash — call `loop_beat()` in the exit's idle closure); no test runs `server::run`'s drained branch end to end (the twin's drain step is its only exercise); a `static` host ignores a mark silently; while drained the edge's `quic_hello` may answer a store error instead of `LEAVING_REFUSAL`; `tell_drain` is O(sessions) per pass while draining.
- **T9**: the drain line is a plain wrapping `Text` without `LabelFace`'s outline stroke — a human look.
- **T10**: `justfile` `retire-host` doc wording "lowest other ordinal"; `dev_r3.sh`'s header should say a host exiting 0 ends the whole stack; `crates/panel/tests/it.rs:124` still sets `VOXEL_HOST_ORDINAL=0` (Dawid's crate).
- **T11**: `main.tf:13` comment vs `world_update.sh` object names.
- **T12**: `twin_check.sh` "no new hosts row" / "old id gone" pass vacuously on an empty listing; `world-stop` / `world-start` warn about a drained host after acting; `WorldHostNotReady` also fires on a drained host; `world_update.sh` checks the coordinator's replicas, not Ready.

### Final suite run (Task 13, after its warnings clean-up)
Each run once, sequentially, through `just` (baseline: Task 1 at `eb90005c`). The only reds are the known pre-existing ones.

| Command | Result | Baseline |
|---|---|---|
| `just test-one game "--no-fail-fast"` | lib 157 / 3 failed / 3 ignored (the three known `lod::tests`); `it` 922 / 0 / 308 ignored; hash_mode 7/0/1, state_hash_check 1, voxel_body 20/0/3, wasm_determinism 7 | lib 155/3/3; `it` 910/0/289 |
| `just test-one world-store "--no-fail-fast"` (SQLite) | lib 6; `it` 129 / 1 failed / 42 ignored (`codec::golden_blobs_decode`, known) | `it` 113/1/44 |
| `just test-pg "--no-fail-fast"` | `it` 129 / 1 / 42 (same known red) | — |
| `just test-pg "--test it -- --include-ignored"` (both engines) | 171 / 1 (same known red) | — |
| `just test-one server "--features test-hooks --no-fail-fast"` | all green: lib 102 / 0 / 3 ignored, it 29, login_link 7, observer 3, store_world 4, … | lib 98/0/2 |
| `coordinator_rt` + `login_link` with `--include-ignored` on Postgres | 8/8, 12/12 | — |
| `just test-one login` | lib 3, `it` 85 / 0 / 3 ignored | — |
| `just test-one svc-link` | lib 12, `it` 1 (11 ignored, incl. the flaky `dialer_reconnects_after_listener_restart`) | — |
| `just test-threads` | 283 + 3 + 10, 0 failed | 262 + 3 + 10 |
| `just test-r2` / `test-r3` / `test-r4` | 15/15, 44/44, 24/24 | — / 40/40 / 21/21 |
| `just determinism-wasm`, `just smoke`, `just client-wasm` | pass (wasm 7 + 1 both sides; client-wasm 2) | — |
| `just test-one client "--lib --no-run"` | compiles (no client test run) | — |

### Not run / not proven
- Client tests: never run (`drain_notice::`, `a_drain_from_an_observer_connection_is_ignored`; owner's).
- No live run of `dev-r2` / `dev-r3` / `go-nogo-r3` / `just host` / `just coordinator` after T10 (dry runs and the in-process suites only).
- No `go-nogo-r3` measurement (this plan changes who owns land under a moving player).
- The twin: no player (no join through the coordinator's login relay), no host owning land (both stayed `land=0`), no real node drain against the budgets (only `disruptionsAllowed` 0 and a refused eviction dry run), no browser.
- Nothing applied to orion; no `orion-plan`. `shellcheck` is not installed (T12 ran it once in a container).

### What the world panel must change (Dawid's `crates/panel`, not touched; from T11/T12)
`crates/panel/src/ops.rs` must mirror the recipes again:
- **Host set**: `GET deployments?labelSelector=app.kubernetes.io/component%3Dhost` in the world namespace (count = items; 0 → refuse to start); host pods by the same label; a host's own pod `app.kubernetes.io/component=host,errantry/host=<name>`, skipping pods with a `deletionTimestamp`. ConfigMap `host-endpoints` and StatefulSet `hosts` are gone.
- **Stop** (`world-stop`): read the coordinator's replicas, the host Deployments and pods (coordinator at 0 with a host pod → refuse); follow each host pod's log; scale every host Deployment to 0; wait until no host pod is left (`WORLD_STOP_TIMEOUT` 150 s; a failed listing is retried, never read as empty); report each leave (`left without a cut` WARN → `stop signal while drained` → `[server] left the world in cut` OK → unknown WARN); scale the coordinator to 0 and wait for its pod to go.
- **Start** (`world-start`): refuse with no host Deployment; coordinator to 1 and wait Ready; every host Deployment to 1; poll each Deployment's `status.readyReplicas == 1` (300 s); a not-Ready one whose `/readyz` body is `drained` counts as done, reported as drained with "remove `<name>` from var.hosts and apply, or `rollout restart deployment/host-<name>`". Never rollout status for hosts.
- **Status**: `/readyz` may say `drained` — its own state, not an error, with the removal hint.
- **RBAC** (ClusterRole `world-panel`): `deployments` get/list, `deployments/scale` get/patch/update, `pods` get/list, `pods/log`, `pods/proxy`; the StatefulSet rules can go; `configmaps` only if something else (`release-history`) still reads them.
