# The Coordinator as an HTTP Service (no svc-link to the coordinator) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hosts and the login service reach the world coordinator as a stateless HTTP service behind one Kubernetes Service — any request to any replica — and the coordinator's and login's `svc-link` connections are gone.

**Architecture:**
- **Coordinator → host is a durable mailbox.** Every message for a host is a row of the existing `outbox` table, inserted by the transition that decided it. A host long-polls its mailbox from any replica and acknowledges by cursor. No replica holds a host: the core's attachment no longer names a replica.
- **Host → coordinator is a POST of a batch.** Any replica applies it as inputs and answers once the transition they caused committed. Order and duplicates are handled as today: every batch carries the `(boot, epoch)` of its `Attach`, a failed POST is a link drop, and the host re-attaches.
- **Login ↔ coordinator the same way:** login POSTs orders to the Service and long-polls the answers from a second small table, `login_outbox`.
- `svc-link` stays as the host ↔ host peer transport only (`PeerRole::Host`).

**Tech Stack:** Rust — axum 0.8 (already in `server` and `login`), reqwest 0.12 over rustls with **ring** (already in both), `axum-server` for the TLS listener (new, ring only), sqlx (PostgreSQL), crossbeam; OpenTofu (`deploy/modules/world`); bash (`scripts/dev_r3.sh`, `scripts/twin_check.sh`); the justfile.

**Spec:** there is no separate spec; the decisions are below. Read with it:
- `docs/superpowers/plans/2026-11-17-elastic-2b-coordinator-replicas.md` — "Decisions settled in planning" 1–4 (the input log, the outbox, the ticker), "As built".
- `docs/superpowers/plans/2026-11-16-elastic-2a-coordinator-state-in-the-store.md` — write-before-send.
- `CLAUDE.md` — "Several coordinator replicas over one row", "Link rules", "The world on k8s".

## Before Task 1

- [x] Elastic 4's whole-branch review and hand checks are closed, or Dawid says to go ahead beside them (`TODO.md`, Elastic 4). This plan rewrites `crates/server/src/coordinator_rt.rs`, which those reviews read. If neither holds, stop and ask. — Dawid ordered implementation beside them (the ledger, "Rulings before Task 1").

## Where this plan departs from the investigation (2026-11-19, this conversation)

1. **No per-host sequence number in the core.** The investigation proposed one. It would make every batch a durable change, so every batch a whole-core write. The existing rule already orders a host's messages across replicas: the projection keeps each host's newest applied `Attach` as `(boot, epoch)` (`WorldCuts::attach_epochs`), and a message of an older attach is dropped at apply and at replay. A batch carries that pair; an ambiguous POST is a link drop and a new `Attach`. Nothing new is stored.
2. **Not two steps.** "Mailbox-always over svc-link first" needs a cursor acknowledgement on the old wire that would be thrown away. One cutover, built in an order that keeps `just smoke` green after every task (the core's mailbox mode is off until Task 5 turns it on).
3. **Nothing is answered in the POST body.** `Reply`, `Assigned`, `LandIs`, `Attached` go through the mailbox like everything else: one path, one order.
4. **`Calendar`, `Sites`, `View` stay messages** (mailbox rows), not polled state.
5. **The version check stays** (`take_out` waits for a `SELECT version` with more than one live replica): a replica may still answer from a stale cache.

## Decisions settled in planning

### 1. The wire

One listener per coordinator replica, HTTPS (HTTP/2), on `VOXEL_COORDINATOR_BIND` (TCP now; default `127.0.0.1:6480`). Bodies are `game::encode_packet` / `decode_packet` of the types below, `Content-Type: application/octet-stream`. Every request carries:

| Header | Value | Refusal |
|---|---|---|
| `Authorization` | `Bearer <VOXEL_SERVICE_TOKEN>` | 401 |
| `X-World` | `VOXEL_WORLD_ID` (`""` matches only `""`) | 412 `world` |
| `X-Link-Version` | `svc_link::LINK_PROTOCOL_VERSION` (12 after this plan) | 412 `version` |

A replica that is not Ready (its `ReplicaJoined` not committed) or is fenced answers 503 to everything.

| Route | Body in → out | Meaning |
|---|---|---|
| `POST /v1/hosts/{host}/messages` | `HostBatch` → 200 `BatchAck` | Apply the batch; answer when its inputs' transition committed (or at once when nothing durable changed and the version check passed). |
| `GET /v1/hosts/{host}/mailbox?boot=&after=&wait_ms=` | → 200 `MailboxPage` | Delete this host's rows with `seq <= after`, return rows with `seq > after` for `boot` in order (at most `MAILBOX_PAGE_MAX`); none → wait for a `coordinator_row` notice or `wait_ms`, then answer (possibly empty). |
| `POST /v1/login/orders` | `login_api::LoginToHost` → 202, or 503 body `no_gate` / `world_starting` | The order's mailbox row committed for the gate host. |
| `GET /v1/login/answers?after=&wait_ms=` | → 200 `LoginPage` | Rows of `login_outbox` with `seq > after`, long-polled as the mailbox. Nothing is deleted by a read. |

```rust
// crates/game/src/coordinator/msg.rs
pub struct HostBatch { pub boot: u64, pub epoch: u64, pub msgs: Vec<HostMsg> }
pub struct BatchAck { pub attached: bool }
pub struct MailboxPage { pub rows: Vec<(u64, CoordMsg)> }
// crates/login-api/src/lib.rs
pub struct LoginPage { pub rows: Vec<(u64, HostToLogin)> }
```

`BatchAck.attached` is false when, after the batch was applied, the core's `attach_epochs[host]` is not the batch's `(boot, epoch)`: the host then re-attaches at `epoch + 1`. A batch whose first message is `HostMsg::Attach` sets that pair.

### 2. The mailbox

- The table is `outbox` (migration 0017) plus a column `at_ms` (the store's clock at insert). Rows are inserted ONLY inside a transition, after its compare-and-set `UPDATE` locked the `coordinator` row — so `seq` order is commit order and a reader at `seq > after` can never miss a lower `seq` that commits later. **Never insert an `outbox` or `login_outbox` row outside a transition.**
- `Target::Host(h)` → one row `(h, attached boot)`; `Target::Every` → one row per attached host. A message for a host that is not attached is dropped (as a detached host's today).
- A transition that applies a host's `Attach` deletes that host's older rows, then inserts `CoordMsg::Attached` (as today's "rows older than the newest applied `Attach` are discarded"). A detach or an end of a host deletes its rows.
- Outputs with nothing durable behind them (a `Reply` from the memo, `LandIs`, a relayed login order) still need a row: a transition may carry outbox rows with an UNCHANGED core — the compare-and-set bumps `version` and writes no `core` bytes (`CoordinatorWrite.core: Option<Vec<u8>>`).
- Delivery is at least once (a lost response is read again); the host's link drops `seq <= ` its cursor: **exactly once, in order**, where today it is at most once.
- The mailbox read needs no core: it is a store read in the HTTP handler. The wake is the replica's existing `coordinator_row` listener, fanned out to waiting polls (`tokio::sync::watch<u64>` of the notified version), with `MAILBOX_RECHECK_MS` as the backstop.

### 3. Attachment without a link

- `WorldCuts::attached: BTreeMap<HostId, u64>` (boot only; the `ReplicaId` goes). "Attached" = this boot's `Attach` was applied and nothing detached it since. Every rule that says "attached to a live replica" says "attached".
- Detach causes: a lapse / end of the host (unchanged), `first_of_generation` (unchanged), and **a stalled mailbox**: the ticker's `hosts` read also returns, per host, the `at_ms` of its oldest unread row; older than `MAILBOX_STALL_MS` → logged `Input::HostStalled { host, boot }` (replaces `Input::LinkDown`): the host is detached and its rows deleted. Its next POST is answered `attached: false` and it re-attaches. `MAILBOX_STALL_MS` is 15 s — today's `LINK_IDLE_TIMEOUT`, the time a dead link takes to be noticed now.
- `ReplicaLapsed` no longer detaches anyone. `forget_absent_hosts` forgets ids below the read's `next_host_id` that are not attached.
- The gate is world-wide: `Coordinator::gate_host()` = the lowest attached host that is live, not leaving, not draining. `gate_host_here`, `Gate::Elsewhere` and `HostToLogin::NoGate`'s rotation go (the variant stays in its append-only enum, unsent).

### 4. The host's end (`HttpWorldLink`, behind the unchanged `WorldLink` trait)

- One thread with a current-thread tokio runtime (the networking edge), two loops, crossbeam to the main loop.
- **Send loop:** drains the out channel into one `HostBatch` (at most `LINK_BATCH_MAX` messages), ONE POST in flight, timeout `LINK_POST_TIMEOUT_MS`. It reads `(boot, epoch)` from each `HostMsg::Attach` passing through and stamps the batches after it. Idle for `LINK_IDLE_POST_MS` → an empty batch (how a detached host learns it). A failed POST, a non-200, or `attached: false` → the link is **down**: queued messages are dropped (the link is not a queue — as today), `generation()` grows, and it is up again at the next successful empty batch (retry backoff `LINK_RETRY_MIN_MS … LINK_RETRY_MAX_MS`, the existing consts).
- **Mailbox loop:** long-polls with its cursor (kept for the process's life). Rows are handed to `recv()` only from the row `CoordMsg::Attached { epoch }` matching the link's current epoch onwards; everything before it is dropped (acknowledged, never delivered) — a row decided for an older attach never reaches the manager.
- `VOXEL_COORDINATOR_ADDR` is a URL or a comma list of URLs (`https://coordinator.<ns>.svc.cluster.local:6480`; `dev-r3` with several replicas lists each). A failed request moves to the next URL. Trust: the existing pin (`VOXEL_COORDINATOR_CERT_HASH_FILE` / `_CERT_SHA256`, `svc_link::read_cert_hash`, `pin_beside`) through `svc-link`'s pin verifier, exported as `svc_link::pinned_client_config(pin: &str) -> io::Result<rustls::ClientConfig>` and given to reqwest (`use_preconfigured_tls`).

### 5. Login

- Per world ONE client and ONE standing answers poll (cursor in memory, from 0 at start: every answer kind is idempotent on login's side). `spawn` = POST the order, then wait for the `SpawnAck` matched by `(player_token, session_epoch)` up to `SPAWN_ACK_TIMEOUT_MS` (3 s, unchanged). 503 `world_starting` → `SpawnError::Starting`; 503 `no_gate` or no answer → 503 as today.
- The kick sweeper POSTs every pending kick each `KICK_RESEND_MS` until its `KickAck` row arrives. Kicks stay durable in login's store.
- `HostMsg::Login` becomes a LOGGED input (replayed): its only effect is a `login_outbox` row, written by the transition. `SessionEnded` is therefore durable until pruned; `LOGIN_ENDS_HELD_MAX` and the replicas' in-memory relay queues go.
- A login order is held in the runtime (not the log) until the transition carrying its row commits, and issued again after a rebase; hosts treat a repeated epoch as the same order.
- The ticker prunes `login_outbox` rows older than `LOGIN_ANSWER_TTL_MS` in its transitions.
- The world registry row keeps its columns: `host_link` holds the URL(s), `host_link_cert` the coordinator's pin. No login migration; worlds are registered again (`world add …`).
- **Role `all`** (one process, `jwt`): serves the same two login routes on `VOXEL_HOST_LINK_BIND` from an in-memory answer queue (bounded by `LOGIN_ANSWERS_HELD_MAX`; no store table — SQLite has no outbox). Login has one transport for both.

### 6. Trust and listeners

One identity, the existing `coordinator` one, serves hosts and login; the `login-link` identity, its Secret `login-link-identity`, `VOXEL_LOGIN_LINK_BIND` and `VOXEL_LOGIN_LINK_CERT_HASH_FILE` go. The Service `coordinator` becomes a ClusterIP Service, TCP 6480 (Kubernetes sends a connection only to a Ready replica).

### 7. Tunables (named consts)

| Const | Value | Where |
|---|---|---|
| `MAILBOX_WAIT_MS` | 20 000 | the client's `wait_ms`; the server caps a wait at it |
| `MAILBOX_RECHECK_MS` | 1 000 | a waiting poll re-reads at least this often (`COORDINATOR_POLL_MS` today) |
| `MAILBOX_PAGE_MAX` | 256 | rows per page |
| `MAILBOX_STALL_MS` | 15 000 | oldest unread row → detach |
| `LINK_POST_TIMEOUT_MS` | 5 000 | one POST |
| `LINK_IDLE_POST_MS` | 2 000 | empty batch when idle |
| `LINK_BATCH_MAX` | 256 | messages per batch |
| `LOGIN_ANSWER_TTL_MS` | 600 000 | `login_outbox` prune |
| `LOGIN_ANSWERS_HELD_MAX` | 4 096 | role `all`'s queue |
| `COORDINATOR_STATE_VERSION` | 7 | `attached` changed shape — an older row beside saved regions is refused: re-forge |
| `LINK_PROTOCOL_VERSION` | 12 | the HTTP header and the peer hello |

## To confirm with Dawid (the plan proceeds on these defaults)

1. **`svc-link` stays as the peer transport** (host ↔ host), not renamed. Folding it into `server/src/peer_link.rs` is a later tidy-up.
2. **TLS by the existing pin**, not a cluster CA or plain HTTP. It keeps `identities.tf`, the Secret and the dev pin files as they are.
3. **Detection of a host that cannot reach the coordinator needs a pending row** (decision 3): a quiet world notices at the next message for that host + 15 s. Today the QUIC keep-alive notices without traffic.
4. **Role `all` gets the login routes** (decision 5) so login keeps one transport.

## Global Constraints

- The row's `version` compare-and-set stays the ONLY fence. Nothing in this plan adds a lease, a lock or a sticky route.
- Write-before-send: a host or login service never sees a decision the row does not hold. A mailbox row exists only by a committed transition.
- The only rustls provider is **ring**; after Task 3 `cargo tree -i aws-lc-rs` prints nothing.
- Tokio stays at the networking edge: handlers talk to the coordinator thread over crossbeam + a oneshot; the core stays threadless.
- Wire bodies are `game::encode_packet` / `decode_packet`, never raw bincode.
- Every new `Input` is classified (replayed or not, why) in `coordinator/input.rs`; every new `Coordinator` field in `projection!`.
- Every new statement that locks `coordinator` takes it first; lock walks go into `world-store/src/succession.rs`'s notes.
- **Tests:** only regression tests and gates are committed (`CLAUDE.md`, Testing). Tests written to drive a task are temporary and deleted before its commit; existing tests that name removed things are updated or, when they pinned only the removed mechanism, deleted with it. `just test-audit` must pass.
- Verify each task with its touched tests (`just test-one …`) and `just smoke`; whole suites once, at closing. No worktrees. Commit only this plan's paths, with a pathspec.
- The justfile, `CLAUDE.md` and `TODO.md` change in the same commit as the behaviour they describe.

## Review Focus

Each is proved by the named check (a twin phase or a temporary test), since no new test may be committed:

1. **A POST that committed but whose response was lost** — the host re-attaches; nothing is applied twice, nothing decided for the old epoch reaches the manager. Task 4 (temporary test), Task 8 (`twin_check`: a replica killed under load).
2. **Two replicas, one host's consecutive batches on different replicas** — the second replica's cache is stale; the rebase replays; order holds. Task 2 (temporary seeded-rig test over two rig replicas).
3. **A host that beats its lease but cannot reach the coordinator** — detached after `MAILBOX_STALL_MS` once a row waits; it is not gate, assignee or heir; it re-attaches when reachable. Task 2 (temporary test), Task 8 (a NetworkPolicy in a twin phase).
4. **A login answer arriving on another replica than the one that took the order, with two login pods** — the join still completes inside 3 s. Task 6, Task 8 (`twin_check` join phases with `coordinator_replicas = 2`).
5. **Write volume** — every coordinator → host message is now a row and possibly a version bump. Assumed acceptable (Dawid, 2026-11-19: no measurement before building); Task 8 records what it is.

## File structure

| File | Change |
|---|---|
| `crates/world-store/migrations/{pg,sqlite}/0020_mailbox.sql` | new: `outbox.at_ms`, `login_outbox` |
| `crates/world-store/src/mailbox.rs` | new: mailbox and login-answer reads, the ack delete, the stall read |
| `crates/world-store/src/coordinator_tx.rs` | core-less transition; `login_outbox` inserts and prune; deletes by host |
| `crates/game/src/coordinator/msg.rs` | `HostBatch`, `BatchAck`, `MailboxPage` |
| `crates/game/src/coordinator/{mod,world_cut,input,replicas,state}.rs` | attachment by boot; `Delivery`; `gate_host`; `HostStalled`; logged `HostMsg::Login` |
| `crates/server/src/coordinator_api.rs` | new: the axum router, auth, the four handlers, the TLS listener |
| `crates/server/src/coordinator_rt.rs` | the listener's `Routes` / `ListenerHosts` / `LoginSide` / `LinkSet` replaced by the API's channel; outbox taking removed |
| `crates/server/src/world_http.rs` | new: `HttpWorldLink` |
| `crates/server/src/roles.rs` | `QuicWorldLink` removed; env parsing for URLs |
| `crates/server/src/login_link.rs` | role `all`'s login routes; the svc-link listener removed |
| `crates/login/src/host_link.rs` | rewritten: `WorldClients` over reqwest |
| `crates/login-api/src/lib.rs` | `LoginPage`, the route consts |
| `crates/svc-link/src/lib.rs` | `PeerRole` loses `Login` and the coordinator role; `pinned_client_config` exported; version 12 |
| `deploy/modules/world/{coordinator,identities,fleet,hosts}.tf` | Service, ports, env, the `login-link` identity removed |
| `scripts/dev_r3.sh`, `scripts/twin_check.sh`, `justfile` | URLs, one pin, TCP |
| `crates/server/tests/common/hosts_rig.rs`, `crates/server/tests/takeover.rs` | the rig's link constructor |
| `CLAUDE.md`, `TODO.md` | as built |

---

### Task 1: The store — the mailbox, login answers, a core-less transition

**Files:**
- Create: `crates/world-store/migrations/pg/0020_mailbox.sql`, `…/sqlite/0020_mailbox.sql`, `crates/world-store/src/mailbox.rs`
- Modify: `crates/world-store/src/coordinator_tx.rs`, `crates/world-store/src/lib.rs`, `crates/world-store/src/succession.rs` (lock walk notes)

**Interfaces — produces:**
```rust
// mailbox.rs (Postgres; SQLite answers StoreError::Unsupported)
pub async fn mailbox_read(&self, host: HostId, boot: u64, after: u64, limit: u32) -> Result<Vec<(u64, Vec<u8>)>, StoreError>; // deletes this host's rows with seq <= after first, same statement batch
pub async fn mailbox_oldest(&self) -> Result<Vec<(HostId, u64 /*boot*/, u64 /*at_ms*/)>, StoreError>; // one row per host with unread rows
pub async fn login_answers_read(&self, after: u64, limit: u32) -> Result<Vec<(u64, Vec<u8>)>, StoreError>;
// coordinator_tx.rs
pub struct CoordinatorWrite { pub core: Option<Vec<u8>>, /* existing fields */
    pub outbox: Vec<OutboxRow>, pub outbox_drop: Vec<HostId>, pub login_outbox: Vec<Vec<u8>>, pub login_prune_before_ms: Option<u64> }
```
The migration: `ALTER TABLE outbox ADD COLUMN at_ms BIGINT NOT NULL DEFAULT 0;` and `CREATE TABLE login_outbox (seq BIGSERIAL PRIMARY KEY, at_ms BIGINT NOT NULL, msg BYTEA NOT NULL);`. Inserts stamp `at_ms` with `PG_NOW_MS`'s expression.

- [x] **Step 1:** Write the two migrations. `0017`'s file is not edited.
- [x] **Step 2:** `write_transition`: when `core` is `None` the compare-and-set is `UPDATE coordinator SET version = version + 1 WHERE version = $v` (no `core`, no `history`); everything after it is unchanged. Then, in order: `outbox_drop` deletes, `outbox` inserts, `login_outbox` inserts, the prune, the existing `pg_notify`.
- [x] **Step 3:** Remove the taker's side: `take_outbox_later` and the `version <= $v` take (their callers go in Task 2 and 5; until then keep them compiling behind the old path if `just smoke` needs it, and delete in Task 5).
- [x] **Step 4:** Implement `mailbox.rs`. `mailbox_read` never takes the `coordinator` row; its delete touches only `(host, seq <= after)`.
- [x] **Step 5:** Add the lock walk of the three new statements to `succession.rs`'s notes (none locks `coordinator`; the transition's inserts run under its lock).
- [x] **Step 6:** Temporary test against the dev Postgres (`crates/pg-test-support`): two transitions insert rows for one host; `mailbox_read(after = 0)` returns both in order; `mailbox_read(after = first)` returns the second and the first is gone; a core-less transition bumps the version and a stale one conflicts. Run `just test-r4`-style: `just test-one world-store "--test it mailbox -- --ignored"`. Delete the test.
- [x] **Step 7:** `just smoke`. Commit (`feat(world-store): the host mailbox and login answers; a transition without a core write`).

### Task 2: The core — attachment by boot, mailbox delivery, a world-wide gate

**Files:**
- Modify: `crates/game/src/coordinator/mod.rs` (`route_out`, `gate_host_here`, `login_gate`, `projection!`, `COORDINATOR_STATE_VERSION`), `world_cut.rs` (`attached`, `attachments`, `detach_host`), `input.rs`, `replicas.rs`, `state.rs`, `link.rs` (the rig), `msg.rs`

**Interfaces — produces:**
```rust
pub enum Delivery { Direct, Mailbox }           // Coordinator::set_delivery(Delivery); default Direct
pub fn gate_host(&self) -> Option<HostId>       // replaces gate_host_here
pub enum Gate { Host(HostId), Nobody }          // Elsewhere removed
Input::HostStalled { host: HostId, boot: u64 }  // replaces Input::LinkDown; logged, replayed; ticker only
Input::HostsRead { /* existing */ mailbox_oldest: Vec<(HostId, u64, u64)> }
// Transition gains: outbox_drop: Vec<HostId>, login_outbox: Vec<LoginOut>, core_changed: bool
```

- [x] **Step 1:** `attached: BTreeMap<HostId, u64>`; update every reader (`attachments()`, `attached_live`, `attached_boot`, `forget_absent_hosts`, heir candidates, `pick_assignee`, the fleet's serving test). `ReplicaLapsed` detaches nobody. Bump `COORDINATOR_STATE_VERSION` to 7.
- [x] **Step 2:** `Delivery`. `Direct` (role `all`, the inline and channel links, an unstored core): `route_out` leaves everything in `out`, as it does today when every attachment is this replica's. `Mailbox`: every `Target::Host` becomes an `OutboxMsg` at the host's attached boot; `Target::Every` one per attached host; a message for a host not attached is dropped with the existing debug line; `out` ends empty. `take_transition` reports `core_changed` so the runtime can write a core-less transition.
- [x] **Step 3:** An applied `Attach` puts the host in `outbox_drop` before its `Attached` row; a detach and a host's end do the same.
- [x] **Step 4:** `Input::HostStalled`: applied only if `attached[host] == boot`; detaches as `LinkDown` did. The ticker derives it in `HostsRead` from `mailbox_oldest` and `MAILBOX_STALL_MS`, on the store's clock. Classify both in `input.rs`'s table (`HostStalled`: logged, replayed, guarded by the boot; `mailbox_oldest`: part of a read, never logged).
- [x] **Step 5:** `gate_host()` / `Gate`; `login_gate` answers `Host` or `Nobody` (with fleet dials → `Starting` + `StartWanted`, unchanged). Remove `link_attach`'s per-replica use only where it named a replica; the `(boot, epoch)` rule of `attach_epochs` is untouched.
- [x] **Step 6:** `HostMsg::Login` is logged and replayed; applying it pushes onto `login_outbox` (a `reset` process field). Remove `login_out`.
- [x] **Step 7:** The rig (`link.rs`, `RigStore`): give it a mailbox so seeded tests can run a core in `Mailbox` mode with two rig replicas.
- [x] **Step 8 (Review Focus 2, 3):** Temporary seeded-rig tests: (a) two replicas, a host's batches alternating between them with rig delays — every `CoordMsg` reaches the mailbox once and in an order consistent with the inputs; (b) rows left unread past `MAILBOX_STALL_MS` detach the host, `gate_host()` skips it, a new `Attach` restores it. Run with `just test-one game "--lib coordinator -- --features test-hooks"` (see memory: filter syntax). Delete them.
- [x] **Step 9:** Update existing coordinator regression tests that construct attachments with a replica; delete none unless it pinned only replica-keyed routing (list any deleted in the commit message).
- [x] **Step 10:** `just smoke`; `just determinism-wasm` (the projection's bytes changed shape; the coordinator is hashed and serialized). Commit (`feat(coordinator): attachment by boot, mailbox delivery, a world-wide gate`).

### Task 3: The coordinator's HTTP API

**Files:**
- Create: `crates/server/src/coordinator_api.rs`
- Modify: `crates/server/Cargo.toml`, root `Cargo.toml` (`axum-server`, feature `tls-rustls-no-provider`), `crates/svc-link/src/lib.rs` (`pub fn pinned_client_config`), `crates/server/src/lib.rs`

**Interfaces — produces:**
```rust
pub enum ApiCall {
    Batch { host: HostId, batch: HostBatch, done: tokio::sync::oneshot::Sender<Result<BatchAck, ApiRefusal>> },
    LoginOrder { order: LoginOrder, done: tokio::sync::oneshot::Sender<Result<(), ApiRefusal>> },
}
pub enum ApiRefusal { NotReady, Fenced, NoGate, Starting }
pub struct ApiConfig { pub bind: SocketAddr, pub token: String, pub world: String, pub cert_file: PathBuf /* <file>.cert / .key beside it */ }
pub fn serve(cfg: ApiConfig, store: StoreHandle, calls: crossbeam::channel::Sender<ApiCall>, row_version: tokio::sync::watch::Receiver<u64>) -> io::Result<ApiListener>;
```

- [x] **Step 1:** The router of decision 1: one auth layer (token, world, version → 401 / 412), the four routes. Mailbox and login-answer handlers read the store directly and wait on `row_version` or `MAILBOX_RECHECK_MS`, capped at `MAILBOX_WAIT_MS`. Batch and order handlers send an `ApiCall` and await `done`.
- [x] **Step 2:** The listener: its own thread and runtime (as `health.rs`), TLS from the identity files, ALPN `h2`. Build the rustls `ServerConfig` with the ring provider named explicitly (as `svc-link` does).
- [x] **Step 3:** Export `svc_link::pinned_client_config` (the existing verifier, with ALPN `h2` set by the caller).
- [x] **Step 4:** Temporary test: start `serve` with a stub thread answering `ApiCall`s and a pg-test-support store; a reqwest client with the pin posts a batch (200), is refused without the token (401) and with another world (412); a mailbox poll returns a row inserted by a transition while it waits, in under 200 ms. Delete it.
- [x] **Step 5:** `cargo tree -i aws-lc-rs` prints nothing. `just smoke`. Commit (`feat(server): the coordinator's HTTP API (unused yet)`).

### Task 4: The host's `HttpWorldLink`

**Files:**
- Create: `crates/server/src/world_http.rs`
- Modify: `crates/server/src/lib.rs`

**Interfaces — produces:**
```rust
pub struct HttpWorldLink { /* … */ }
impl HttpWorldLink {
    pub fn connect(urls: Vec<String>, pin: impl Fn() -> io::Result<String> + Send + 'static, host: HostId, token: String, world: String) -> HttpWorldLink;
}
impl game::WorldLink for HttpWorldLink { /* send, recv, is_up, generation */ }
```

- [x] **Step 1:** Implement decision 4 exactly: the send loop, the idle batch, down/up and `generation()`, the mailbox loop with its cursor and the `Attached { epoch }` gate. Log `[world-link] up to <url> (<remote ip>)` and `[world-link] down: <why>` (`twin_check.sh` reads the first).
- [x] **Step 2 (Review Focus 1):** Temporary test against Task 3's listener with a stub coordinator: the stub commits a batch but drops the response → the link goes down, `generation()` grows, messages queued meanwhile never arrive, and rows the stub put in the mailbox before the new `Attached` row are never returned by `recv()`; a page delivered twice yields each row once. Delete it.
- [x] **Step 3:** `just smoke`. Commit (`feat(server): the host's HTTP link to the coordinator (unused yet)`).

### Task 5: The cutover — the runtime serves the API, hosts use it, the coordinator's svc-link goes

**Files:**
- Modify: `crates/server/src/coordinator_rt.rs` (remove `LinkSet`, `Links`, `Route`, `ListenerHosts`, `Routes`, `Admit`, the outbox take and `lost_answer_relinks`; `start_listening*` start `coordinator_api::serve`), `crates/server/src/roles.rs` (remove `QuicWorldLink`; `VOXEL_COORDINATOR_ADDR` as URLs), `crates/server/src/main.rs`, `crates/server/tests/common/hosts_rig.rs`, `crates/server/tests/takeover.rs`, `crates/svc-link/src/lib.rs`

- [x] **Step 1:** The runtime's loop takes `ApiCall::Batch`: applies each message as `Input::Host` with the batch's attach pair (the pair that `link_attach` gave a link's messages), remembers `done` against the core's log position, and answers it when the transition covering that position committed — or at once when nothing durable changed and the version check passed. A rebase keeps the waiters (their inputs are replayed); a fenced end answers `ApiRefusal::Fenced`.
- [x] **Step 2:** Role `coordinator` sets `Delivery::Mailbox`. A transition with `core_changed == false` and rows to write is written core-less. The admission checks that ran at the link's hello (`policy_agrees`, `generation_current`, `stored_registration`) run at a batch's `Attach`; a refusal is `attached: false` plus the existing log line.
- [x] **Step 3:** The row listener feeds the API's `row_version` watch. `/readyz`, the ticker, the replica beat are unchanged.
- [x] **Step 4:** Hosts build an `HttpWorldLink`. `svc-link`: remove the coordinator's `PeerRole` variant use on the dial and listen sides (the enum's wire order stays: leave the variant, unsent), set `LINK_PROTOCOL_VERSION = 12` with its doc line.
- [x] **Step 5:** Port the rigs (`hosts_rig.rs::quic_world_link` → an HTTP link to the rig's coordinator). Run the takeover rig: `just test-r4`.
- [x] **Step 6:** `justfile` (`coordinator`, `host`, `dev-r2`, `dev-r3` recipes and their comments) and `scripts/dev_r3.sh`: `VOXEL_COORDINATOR_ADDR=https://127.0.0.1:6480[,https://127.0.0.1:6481…]`. Login is still on the old link until Task 6 — keep `VOXEL_LOGIN_LINK_*` here.
- [ ] **Step 7:** Hand check: `just coordinators=2 dev-r3 2`, one client; walk across a seam; kill one coordinator replica (`kill -9`), see the host re-attach to the other and a cut commit; kill a host, see the succession settle. Note the log lines in the commit message. — **owed: hand check (a), TODO.md.**
- [x] **Step 8:** `just smoke`. Commit (`feat(world): hosts reach the coordinator over HTTP; its svc-link listener is gone`).

### Task 6: Login over HTTP

**Files:**
- Modify: `crates/login/src/host_link.rs` (rewrite), `crates/login/src/{lib,api,ctl,metrics}.rs`, `crates/login/Cargo.toml` (drop `svc-link` if nothing else uses it), `crates/login-api/src/lib.rs`, `crates/server/src/coordinator_rt.rs` (remove `LoginListen`, `LoginSide`, `LoginTally`'s link parts, `LOGIN_ENDS_HELD_MAX`), `crates/server/src/login_link.rs`, `justfile`, `scripts/dev_r3.sh`

**Interfaces — produces:**
```rust
// login: same public surface as today's HostLinks, so api.rs changes little
pub struct WorldClients { /* … */ }
impl WorldClients {
    pub async fn spawn(&self, world: &WorldRow, player_token: &str, session_epoch: u64) -> Result<SpawnPlace, SpawnError>;
    pub fn wake_kicks(&self);
    pub async fn link_states(&self, world: &WorldRow) -> Result<Vec<(String, bool)>, SpawnError>;
}
// login-api
pub const PATH_COORD_LOGIN_ORDERS: &str = "/v1/login/orders";
pub const PATH_COORD_LOGIN_ANSWERS: &str = "/v1/login/answers";
```

- [x] **Step 1:** The runtime takes `ApiCall::LoginOrder`: `login_gate()` → `Nobody` answers `NoGate` / `Starting` at once (and `StartWanted` as today); `Host(h)` queues `CoordMsg::Login` for `h`, held until its transition commits (decision 5), then answers `Ok`.
- [x] **Step 2:** `WorldClients` per decision 5: one pinned client and one answers poll per world, started on first use and by the kick sweeper for worlds with pending kicks; `SpawnError::NoServiceToken`, `BadLink`, `Starting` keep their meanings. `SpawnError`'s variants for `NoGate` rotation go.
- [x] **Step 3:** Role `all`: `login_link.rs` serves the two login routes on `VOXEL_HOST_LINK_BIND` with `coordinator_api`'s auth layer and an in-memory `LoginPage` queue (`LOGIN_ANSWERS_HELD_MAX`); its svc-link listener is removed. `svc-link` loses `PeerRole::Login`'s use.
- [x] **Step 4:** `login world add --link <url[,url]> --link-cert <coordinator pin>`; `just dev-login`, `dev-r2`, `dev-r3`, `world-register` print and use the URL and the coordinator's pin. Remove `VOXEL_LOGIN_LINK_BIND` / `_CERT_HASH_FILE` and `login-link.cert-sha256` everywhere.
- [ ] **Step 5 (Review Focus 4):** Hand check on `just coordinators=2 dev-r3 2`: ten joins in a row complete; `login account revoke <email>` ends a live session (the kick acked: `pending_kicks` empty); stop every host, a join answers 503. Time one join from the login log. — **owed: hand check (c), TODO.md.**
- [x] **Step 6:** Existing login tests that used a svc-link listener stub: port to an axum stub. `just test-one login "--test it"`; `just smoke`. Commit (`feat(login): orders and answers over the coordinator's HTTP API`).

### Task 7: The module and the twin

**Files:**
- Modify: `deploy/modules/world/coordinator.tf`, `identities.tf`, `fleet.tf` / `hosts.tf` (the host template's env), `outputs.tf`, `deploy/envs/{local,orion}/world/*.tf` as needed, `scripts/twin_check.sh`, `justfile` (`world-register`, `world-status`), `crates/panel/src/` only if it names port 6490 (grep first)

- [x] **Step 1:** Service `coordinator`: ClusterIP, one port, TCP 6480. The container's ports and env: `VOXEL_COORDINATOR_BIND=0.0.0.0:6480`; `VOXEL_LOGIN_LINK_*` removed. Remove `tls_private_key.login_link`, `tls_self_signed_cert.login_link` and Secret `login-link-identity`. Output `login_link` = `https://coordinator.<ns>.svc.cluster.local:6480`.
- [x] **Step 2:** The host template: `VOXEL_COORDINATOR_ADDR=https://coordinator.<ns>.svc.cluster.local:6480`. This is a value the coordinator and the hosts share: the runbook is stop → apply → start.
- [x] **Step 3:** `twin_check.sh`: line 600's `world add` pattern (URL, the coordinator's pin); `link_ip` reads `[world-link] up to`; the phases that kill a coordinator replica expect a re-attach through the Service, not a dial of the other address. Add one phase (Review Focus 3): a NetworkPolicy that cuts one host pod from the coordinator's port for 30 s while a join is asked of it — the host is detached (`world-status` shows it not serving), the join lands on the other host, and after the policy is removed the host serves again. Update the header's list of phases.
- [ ] **Step 4:** `just twin-down && just twin-up && just twin-images && just twin-apply && just twin-check` (say so first: it takes the machine for a while). Record `cold-start.txt`'s seconds beside the last run's. — **owed: hand check (d), TODO.md.**
- [ ] **Step 5:** `orion-plan world` only (no apply): read the plan — the Service is replaced, one Secret destroyed, the coordinator and host template change. Write the orion runbook into `TODO.md` (NOT RUN): `orion-world-push` → `world-stop` → `kubectl set image` on `coordinator`, `toolbox`, `fleet`, `web` → `orion-apply world` → a new empty database and `world-ctl forge …` (the core format is 7: worlds are re-forged, never migrated) → `world-start` → `world-register` and `just orion-login world add …` with the URL. — **`orion-plan` owed (hand check f); the runbook is in TODO.md.**
- [x] **Step 6:** Commit (`feat(deploy): the coordinator behind one ClusterIP Service; the login-link identity is gone`).

### Task 8: Alerts, the write volume, documentation, closing

**Files:**
- Modify: `crates/server/src/health.rs`, `deploy/modules/monitoring/` (alert rules), `CLAUDE.md`, `TODO.md`, this plan ("As built")

- [x] **Step 1:** Metrics (coordinator role): `mmo_coordinator_mailbox_rows` and `mmo_coordinator_mailbox_oldest_ms` (the ticker's read), `mmo_coordinator_api_requests_total{route,code}`, `mmo_coordinator_stalled_total`; `stalled=` on the `[coordinator] window=` line. Host: `mmo_world_link_up`, `mmo_world_link_reattach_total`.
- [x] **Step 2:** Alerts: `WorldMailboxStalled` (`mailbox_oldest_ms > 30 s` for 2 min), `WorldCoordinatorApiErrors` (5xx share). `just orion-plan monitoring` only. — `promtool check rules` on the rendered template instead; `orion-plan monitoring` + apply owed (TODO.md).
- [ ] **Step 3 (Review Focus 5):** Run `just coordinators=2 dev-r3 2` with two clients walking across a seam for 10 minutes (say so first; Dawid may want to run it). Write into "As built": transitions per second, written bytes, mailbox rows per second and `mmo_coordinator_conflicts_total` from the `[coordinator] window=` lines. There is no earlier baseline; report the numbers to Dawid, and if `WorldCoordinatorConflicts` would fire on them, say so before closing. — **owed: hand check (e), TODO.md.**
- [x] **Step 4:** `CLAUDE.md`: rewrite "Several coordinator replicas" (the outbox bullet → the mailbox; attachments; the gate), "Link rules", the login bullet of "Many hosts", "World on k8s variables" (`VOXEL_COORDINATOR_ADDR` as URLs; removed variables), the deployment chain (`login-link-identity`, ports), the `svc-link` mentions (peer links only), `LINK_PROTOCOL_VERSION` 12, `COORDINATOR_STATE_VERSION` 8 (7 lived only between two commits of this plan). `TODO.md`: the hand checks still owed (orion runbook; a browser join through the twin).
- [x] **Step 5:** `grep -rn "LOGIN_LINK\|login-link\|QuicWorldLink\|gate_host_here\|take_outbox\|NoGate" crates scripts deploy justfile CLAUDE.md` — every hit is either the unsent enum variant or a doc line that says "removed".
- [ ] **Step 6:** Closing runs (say so first): `just test-full`, `just clippy`, `just determinism-wasm`. Then request the whole-branch review (superpowers:requesting-code-review). — **owed: re-run on a clean tree** (run once, results in "As built": the scene/tree goldens fail against uncommitted physics/tree work; `clippy -D` is red in `macros`, `block-mesh`, `bbmodel`); the whole-branch review is the controller's.
- [x] **Step 7:** Commit (`docs: the coordinator as an HTTP service — as built`).

## Not in this plan

- Peer links (host ↔ host) — unchanged, still `svc-link`.
- Removing the ticker or the replica rows: timed work still needs one replica to run it.
- Splitting the `coordinator` row (see `WorldCoordinatorConflicts`).
- A cluster CA, mTLS, or a mesh.
- The fleet reconciler (it never talked to the coordinator) and the panel's operations.
- The old compose game (`just deploy`).

## Known limits (to carry into `TODO.md`)

- A host cut off from the coordinator but beating its lease is noticed only when a row waits for it, 15 s after (confirm 3). Corrected by the whole-branch review: a row always comes — the ticker broadcasts `Calendar` to every attachment at each day turn (`DAY_MS`, 10 s) — so the bound is about 10 + 15 + 1–2 ≈ 27 s, not "a quiet world: at its next row".
- Every coordinator → host message costs a row insert and delete; one with nothing durable behind it costs a version bump too.
- A login pod that restarts re-reads up to `LOGIN_ANSWER_TTL_MS` of answers (idempotent, but a burst).
- With every replica down hosts run on, as today; their links are down and they re-attach when one is back.
- Hosts, login and coordinator of this build do not mix with older ones (version 12): stop, upgrade, start.

## Self-review notes

- Every row of decision 1's route table has a handler (Task 3), a client (Tasks 4, 6) and a runtime end (Tasks 5, 6).
- Names used across tasks: `HostBatch`, `BatchAck`, `MailboxPage`, `LoginPage`, `Delivery`, `gate_host`, `Gate::{Host,Nobody}`, `Input::HostStalled`, `ApiCall`, `ApiRefusal`, `HttpWorldLink`, `WorldClients`, `CoordinatorWrite.core: Option<…>` — each defined once (decisions 1–3; Tasks 1–4, 6).
- Not verified while planning (the implementer checks first): `axum-server`'s exact ring-only feature name; the names of the core's log-position accessors used in Task 5 Step 1 (`log`, `log_boundary` in `projection!`).

## As built

Executed 2026-11-19, subagent-driven, on `develop` (no worktree); ledger and every task report and review:
`.superpowers/sdd/2026-11-19-coordinator-http-api/` (`progress.md`, `task-N-report.md`, `task-N-review.md`). No foreign
commit is interleaved: the range is `eecc6ad7..` this docs commit. Every task was reviewed; the whole-branch review is
pending (the controller's). Nothing ran live, on the twin or on orion: the hand checks are owed (`TODO.md`, "The
coordinator as an HTTP service", a–f, and its orion runbook).

| Task | Commits | What differs from the plan text |
|---|---|---|
| 1 store | `857f3f83` | The write struct is the existing `TransitionWrite` (`core: Option<Vec<u8>>`, `outbox_drop`, `login_outbox`, `login_prune_before_ms`); `outbox` stays `Vec<(HostId, boot, bytes)>`; `mailbox_read` filters by boot; SQLite answers `login_outbox` `Unsupported`. Step 3 (removing the taker) was moved to Tasks 2/5 with its callers (pre-flight ruling). |
| 2 core | `400aa462`, fix `010a20ad` | `Input::HostStalled { host, boot, epoch }` (not `{ host, boot }`): a re-`Attach` keeps the boot and raises the epoch, so a stale or replayed stall must name the attachment it observed (review I2). Durable `WorldCuts::attached_ms` (the store time an `Attach` was applied) with `ATTACH_CLOCK_SLACK_MS` (1 s): a read issued before a re-`Attach` committed still shows the previous attachment's rows. A recognised lost commit applies its drops once (`outbox_committed`, review I1). `HostStalled` is applied on any replica (only its derivation is the ticker's). `COORDINATOR_STATE_VERSION` 7 here, 8 in Task 5 (`attached_ms`) — 7 was never released. Between Tasks 2 and 5 a split world ran with ONE replica only (the runtime's relay was removed with the core's replica attachments). |
| 3 API | `9a659507`, fix `229421a3` | No `axum-server`: TLS is tokio-rustls (ring) + hyper / hyper-util under the axum router (`serve_tls`); `ApiListener::set_refusal(Option<ApiRefusal>)` (starts `NotReady`; `None` once Ready, `Fenced` once fenced) instead of a readiness input; the calls channel is unbounded; a long poll with no notice sender degrades to the recheck interval (review I1: it spun). `tokio-rustls` / `hyper` are pinned in `crates/server/Cargo.toml`, not the workspace (the root `Cargo.toml` carries the owner's hunks). |
| 4 host link | `5d8e19e0`, fix `edcdf947` | `generation()` grows at UP (a probe answered 200), as the QUIC link did; messages carry the generation they were sent in; the backoff resets only when an `Attach` is answered attached (review I1: a refused `Attach` looped at round-trip speed); a failed mailbox GET moves the URL and backs off but never takes the link down; up/down lines are rate-limited while flapping. |
| 5 cutover | `e2118846` | Empty batches apply and write nothing; batch answers wait for the covering transition or, nothing durable changed, the runtime's version check (`pump_clean_batches`); `admit_attach` runs the old hello's checks at a batch's `Attach`; core-less writes carry only `core_less_kind` messages (`check_core_less`); the host's mailbox gate also passes `CoordMsg::Generation` (an `Attach` refused for its world generation has no `Attached` row); the ticker's `hosts` + mailbox read runs under every ownership with `Mailbox` (stall detection for `static`); `COORDINATOR_STATE_VERSION` 8, `LINK_PROTOCOL_VERSION` 12; hosts refuse a bare `host:port` `VOXEL_COORDINATOR_ADDR`. |
| 6 login | `475962a3` | Task 5's review minors folded in (a detached newest link's messages dropped by the core at apply and replay; a refused `Attach` detaches only at least the newest epoch; a refresh notes the row version). `SpawnError::NoGate` (503 `no_gate` logged truthfully); a login POST also tries the next URL on a 503 `not_ready` / `fenced`; a cursor past the newest kept answer reads from the start (a re-forge under a running login pod); role `all`'s login routes go through `coordinator_api::{serve_tls, check_auth, long_poll}` on `VOXEL_HOST_LINK_BIND`. Login had no tests left to port. |
| 7 module, twin | `a6459cb3`, fixes `cb6d4f33`, `cdcf7ce6`, `c5b10a0e` | `cluster_ip` is Optional+Computed in the provider: an EXISTING world keeps its headless Service on an apply and is replaced only with `-replace=module.world.kubernetes_service_v1.coordinator` (review I1; the runbook does it). Twin phase 7 waits for both hosts Ready, cuts and a join (the `[world-link]` line's IP is the Service's); phase 7d = a NetworkPolicy + a conntrack flush (kube-router keeps established flows, review I2) on the host that does NOT own 7c's land, three outcomes: PASS, `SKIP 7d` (the cut took no visible effect), INCONCLUSIVE (not Ready without a stall line — no row may have waited) — never a failure. The runbook's delete selects `errantry/fleet=<world id>` (not `component=host`, which matches the Terraform Service `hosts`). |
| 8 closing | `aa1f19ae`, `c16f7e3d`, this docs commit | Metrics and alerts below; `mmo_coordinator_mailbox_rows` needed the row count in the ticker's read (`mailbox_oldest` returns `MailboxHead = (host, boot, at_ms, rows)`: one statement with `COUNT(*) OVER (PARTITION BY host)`, no lock); the window line also gained `mailbox_rows=` / `login_rows=` / `core_less=` for Step 3's measurement. The measuring run (Step 3) and every hand check are owed. |

**Metrics and alerts.** Coordinator: `mmo_coordinator_mailbox_rows`, `mmo_coordinator_mailbox_oldest_ms` (the ticker's
read; 0 on the other replicas), `mmo_coordinator_api_requests_total{route,code}` (routes `host_messages`,
`host_mailbox`, `login_orders`, `login_answers`, `other`; codes 200 202 400 401 404 412 413 500 503 and `other`;
counted in the API's guard layer, one atomic add), `mmo_coordinator_stalled_total` (the hosts detached as stalled by
the ticker's read — counted around the `hosts` read's apply, never at a replay); `stalled=`, `mailbox_rows=`,
`login_rows=`, `core_less=` on the `[coordinator] window=` line. Host: `mmo_world_link_up`,
`mmo_world_link_reattach_total` (`Attach` batches after the first). Alerts `WorldMailboxStalled` (`max by (namespace)`
of the oldest unread row > 30 s for 2 min) and `WorldCoordinatorApiErrors` (the 5xx share of every route but
`login_orders` > 5 % for 10 min — the orders' 503 `world_starting` / `no_gate` are answers). `promtool check rules` on
the rendered template: 21 rules, SUCCESS; `orion-plan monitoring` + apply owed.

**The write volume (Review Focus 5): NOT measured.** The run needs `just pg-reset` on the shared dev database and two
GUI clients; it is hand check (e) in `TODO.md`. No number is recorded here yet.

**Closing runs** (Task 8 Step 6, at `c16f7e3d` + the docs): `just smoke` PASS (test-audit green). `just test-full`: PASS test-audit, physics-next-upstream, slotmapd, takeover (the
Postgres rig over the HTTP link), quest, quest-wasm, site-test; FAIL workspace — only `wasm_determinism::scene_hash_native`
(digest `0xE6CD22F6` vs `0x38D86635`), `worldgen` `scene_hash_golden::canonical_scene_state_hash` and
`tree_fall::{mature_trees_topple_about_their_base, young_trees_land_on_the_ground}`; FAIL determinism-wasm — the same
`scene_hash_native`. Not this plan's: the golden `0x38D86635` is the owner's UNCOMMITTED re-pin in
`crates/game/tests/wasm_determinism.rs` beside uncommitted physics and tree work (`movement.rs`, `trees.rs`, the rapier
character controller, the physics-golden CSVs); the scenes hold no coordinator, and the native digest moved since Task 2
(`0x5F76C6CF`) with only that work changing — re-run on a clean tree. `just clippy` (workspace, `-D warnings`) stops at
pre-existing hits in `macros`, `block-mesh`, `bbmodel` (untouched since `62d8ccc9`); `cargo clippy --no-deps` over
`server`, `world-store`, `svc-link`, `login`, `login-api`, `game` shows 159 warnings, 8 on lines this plan wrote: three
`type_complexity` of Task 8's own fixed in `c16f7e3d`; left (reported): `result_large_err` at `coordinator_api.rs`
`check_auth` / the answers decode (an axum `Response` as the error, 3 hits), a no-op `..Default::default()` in the
runtime's `TransitionWrite` (every field set), `large_enum_variant` on `world_http.rs`'s `Event`.

**Known limits** — as the plan's "Known limits" above, carried into `TODO.md` with the deferred minors (grouped).

### Final review (2026-11-19)

The whole-branch review of `eecc6ad7..f86005f6` (ledger: `final-review.md`): ready to rely on WITH FIXES — 0 Critical, 3 Important, 6 Minor; no objection to any ruling.

- **A dead or cut-off host stays attached until its mailbox stalls** (the risk named before the review): bounded, not broken. A row always waits within `DAY_MS` (the ticker's `Calendar` broadcast), so the bound is about 10 + 15 + 1–2 ≈ 27 s; cuts and the settle cut are not blocked (`left` / `succeeded` are filtered); the gate, an assignee or an heir may name such a host for that long (a dead one: for the lease, as before).
- **Important 3 is not code:** nothing has run as processes with two replicas, a login service and a client. Hand checks (a), (c), (d) in `TODO.md` are this plan's acceptance, before the orion runbook.

The ONE fix wave (ledger: `fix-wave-report.md`), confirmed by its scoped re-review (`fix-wave-rereview.md`: all addressed, nothing Critical or Important new):

| Commit | What |
|---|---|
| `3f3ca2e6` | Mailbox rows of a boot no attachment holds are dropped once their head is older than `MAILBOX_STALL_MS` (by `(host, boot)`, inside the transition; before, a rollback's `Generation` row for a dead host stayed for ever and `WorldMailboxStalled` never cleared). An empty batch is answered `attached: false` only from a cache known current. `pump_clean_batches` skips an evicted core. `check_core_less` names the decisions that may ride without a core (`AssignLand`, `AddHost`, `DrainHost`, `RemoveHost`: idempotent). The dead hook `crash_before_claim_reply` is gone. |
| `f49fd893` | The shared pinned HTTP/2 client has a connect timeout and keep-alive (5 s each, also idle); login rebuilds a world's client after a transport failure; a link going down (or a new `Attach`) cancels the host's in-flight mailbox GET; a dead link thread turns `is_up()` false. |
| `16bf4008` | Docs: the ≈ 27 s bound, sticky HTTP/2 connections, hand check (e) records refreshes and conflicts (`refreshes=` on the window line), the acceptance checks named. |

Left open after the re-review (optional): a `hosts` read issued just before a refused `Attach` can drop that host's fresh `Generation` row (≤ ~1 s window; the next refused `Attach` tells it again); the mailbox loop's backoff stays at 250 ms while the coordinator is unreachable (a few cheap failed GETs per probe). Parked: `admit_attach`'s synchronous store reads on the coordinator's thread. The other deferred minors: `final-review.md`, "Triage".
