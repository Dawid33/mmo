# Durable world persistence (multi-instance stage 0) — design

Date: 2026-09-20. Status: **draft for review — nothing implemented.**
Parent: `2026-09-20-multi-instance-k8s-postgres-research.md` (§1a decisions, §6, §12 stage 0).

## Goal

One server instance whose world survives restarts and crashes: terrain, player bodies and
inventories live in Postgres; players have a stable identity; logout takes the body out of the
world and login puts it back. Everything here is also the foundation the later stages
(super-region leases, cross-pod crossing, k8s) stand on, so formats and seams are chosen for
them — but no directory, lease, second instance or container appears in this stage.

Decisions already taken (parent §1a): freeze-on-first-run terrain policy; logout removes the
body; entity-generic rows; portable format (terrain + bundles), not the `Rollback` memory image.

### Non-goals

Leases/fencing/`instances` table (stage 1) · real authentication · per-column terrain rows ·
LOD payload persistence · a `resume_blob` fast path · non-player persistent entities (the
schema and ids allow them; only players exist today — `EntityKind` has one variant) ·
in-game terrain editing (does not exist yet: `Chunk::set_block` is called only by tests and
worldgen; this design only guarantees edits *will* persist once an edit event exists).

## Findings that shape the design (code, 2026-09-20)

1. **No terrain-edit event exists.** `GameEventKind` = `Tick, PlayerInput, CreateClient, Quit,
   EntityArrived, GhostUpdate, UiAction`. Terrain persistence is therefore driven by (a) the
   freeze-on-first-run write and (b) the existing `terrain_fingerprint()` change check, which
   will pick up edits from whatever event later introduces them. Tests edit via `with_data`.
2. **No way to remove a client.** Disconnect today only unsubscribes; the body stays. Logout
   needs a new server-authoritative sim event so observing clients replay the removal
   deterministically.
3. **Rebuilding a region resets its slotmap, and handoff identity is `(source_region,
   EntityKey)`.** `GameData::arrivals` is keyed by that pair and pruned only on extraction.
   Terrain entities are inserted first in deterministic order, so after a rebuild the first
   player in a region gets *the same key as the first player before the rebuild*. Sequence:
   X walks R→T (T holds `arrivals[(R,K1)] = X`); R parks and is later rebuilt from storage; Y
   logs in in R, receives K1, walks to T; `apply_arrival` finds `(R,K1)` with a live entity →
   "already arrived, pose-correct only" → **Y is merged into X and vanishes.** Today's bit-exact
   blob restore hides this (keys continue); restore-by-rebuild exposes it. Stage 0 must move
   handoff identity to a persistent id.
4. `Region::from_chunks(id, Vec<(ChunkCoords, Chunk)>)` exists and `TerrainSnapshot.chunks` is
   exactly that type → terrain restore needs no new construction path. `apply_arrival(bundle)`
   is the existing, idempotent entity insert.
5. `extract_entity_safe` both *reads* the bundle and *removes* the entity; checkpoints need the
   read half alone.
6. `ClientId = usize` lives inside `GameData` and crosses the 64-bit↔wasm32 wire; bincode
   writes it as u64 and a wasm client fails to decode values above `u32::MAX`.

## Design

### 1. Identity

- `accounts` table; each account gets a small integer `client_id` from a sequence, constrained
  to `1..=i32::MAX` (finding 6). **The in-sim `ClientId` becomes the account's `client_id`** —
  no sim type changes, the edge simply stops inventing ids from `AtomicUsize`.
- **`EntityId(u64)`**: persistent entity identity. Namespaced by the top byte: `0x00` = player,
  low bits = `client_id`; other namespaces reserved (e.g. region-scoped spawn counters) for
  future NPC/item entities. A player's `EntityId` is thus derivable inside the sim from
  `CreateClient(ClientId)` with no allocation and no extra event data.
- New component column `ecs.id: EntityId` on every non-terrain entity. `EntityBundle` and
  `GhostData` carry `id: EntityId`; **`arrivals` and `ghosts` re-key from
  `(RegionCoords, EntityKey)` to `EntityId`**; `matches_prediction` matches transfers on `id`.
  `source_region` stays in the bundle (the client's predicted synthesis and rebasing use it);
  `source_key` is dropped. This is the fix for finding 3 and is what stage 1's entity rows and
  cross-pod handoff key on.
- Dev-grade login (placeholder for real auth, same packet shape later carries a signed
  ticket): first packet on a connection is `ClientPacket::Login { name, secret }`. The secret is
  32 random bytes the client generates once and keeps (native: a file under the platform config
  dir; wasm: `localStorage`). Unknown name → account created, secret hash stored
  (trust-on-first-use). Known name + wrong secret → connection refused. Same account already
  connected → the older session is dropped (newest wins). `ServerPacket::LoginResult`.
- The edge defers `ServerEvent::ClientConnected` until login resolves, and it now carries what
  the store returned: `ClientConnected { id, saved: Option<(RegionCoords, EntityBundle)> }`.

### 2. Login and logout are arrival and extraction

- **Login**, `saved = Some((region, bundle))`: `ensure_running(region)`, `homes.insert`,
  `RegionInput::Event(EntityArrived(bundle))`. `saved = None` (new account): today's path,
  `CreateClient` in `SPAWN_REGION`. Either way the client's flow from `RequestPlayerRegion` on
  is unchanged.
- **Logout** (disconnect, or replaced session): manager sends `RegionInput::Event(
  GameEventKind::ClientLeft(id))` to the home region. The handler runs `extract_entity_safe`
  and parks the bundle in a `pending_logouts` field (same read-after-tick discipline as
  `pending_departures`); the runner emits `RegionOutput::Extracted(ClientId, EntityBundle)`;
  the manager saves it with `in_world = false`, drops `homes[id]`, refreshes keep-alive.
  `ClientLeft` is echoed to subscribers like any event, so observers remove the body
  deterministically. It is manager-internal: a client sending it is dropped, like
  `EntityArrived`.
- A logout racing a boundary crossing: the manager routes `ClientLeft` by `homes`, which it
  updates when it relays the departure, and region inputs are FIFO — so the event reaches
  whichever region holds the body. If the region reports the client unknown (already
  extracted by `scan_boundaries` this tick), the manager re-sends to the new home once the
  `Departures` output arrives. Covered by a test.

### 3. What is stored

```sql
CREATE TABLE world (                       -- exactly one row
  world_seed bigint NOT NULL, worldgen_version text NOT NULL, format smallint NOT NULL,
  next_seq bigint NOT NULL);               -- high-water mark for checkpoint seq (§4)

CREATE TABLE accounts (
  client_id   integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,   -- = in-sim ClientId
  name        text NOT NULL UNIQUE,
  secret_hash bytea NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now());

CREATE TABLE regions (
  rx int, rz int, PRIMARY KEY (rx, rz),
  tick bigint NOT NULL,
  worldgen_version text NOT NULL,          -- version that generated the frozen terrain
  edited boolean NOT NULL DEFAULT false,   -- terrain differs from that version's worldgen
  terrain_version bigint NOT NULL,         -- bumped per terrain write (LOD key, stage 1)
  terrain bytea NOT NULL,                  -- format byte + zstd(bincode(StoredTerrainV1))
  saved_at timestamptz NOT NULL DEFAULT now());
ALTER TABLE regions ALTER COLUMN terrain SET STORAGE EXTERNAL;   -- already compressed

CREATE TABLE entities (
  entity_id bigint PRIMARY KEY,            -- EntityId
  rx int NOT NULL, rz int NOT NULL,
  in_world boolean NOT NULL,               -- false = logged out, body held here
  seq bigint NOT NULL,                     -- stale-writer guard (§4)
  client_id integer UNIQUE REFERENCES accounts,   -- NULL for non-players
  bundle bytea NOT NULL);                  -- format byte + zstd(bincode(StoredEntityV1))
CREATE INDEX entities_by_region ON entities (rx, rz) WHERE in_world;
```

- **Freeze on first run:** a region with no row is generated, and its terrain is written at its
  first checkpoint with `edited = false`. A region *with* a row is never generated again.
  `edited` flips when a later fingerprint differs from the frozen one.
- **Stored types are explicit mirrors**, not the sim structs: `persist::StoredTerrainV1`,
  `persist::StoredEntityV1`, with `From`/`TryFrom` conversions. A change to `Chunk`, `Client`,
  `Inventory` or `Motion` then breaks the conversion at compile time and forces a conscious
  `V2` + migration, instead of silently changing the on-disk format. A checked-in golden blob
  of each version must keep decoding (test).
- `worldgen_version` is the existing `WorldgenCache` key (crc32 of seed ‖ block manifest ‖
  worldgen sources). On startup a mismatch between `world.worldgen_version` and the running
  build is *allowed* (that is the point of freezing) and updates the `world` row; a
  `world_seed` mismatch refuses to start.

### 4. Checkpoints

- A region checkpoints every `CHECKPOINT_INTERVAL_TICKS` (named constant; 600 = 30 s at the
  current 50 ms tick) and on `Shutdown`. It emits
  `RegionOutput::Checkpoint(RegionCheckpoint { tick, entities: Vec<(EntityId, EntityBundle)>,
  terrain: Option<TerrainSnapshot> })`.
  - `entities`: every entity with an `ecs.id`, via a new read-only `bundle_of(key)` split out
    of `extract_entity_safe` (finding 5). A handful of small structs.
  - `terrain`: `Some` only on the first checkpoint of a never-stored region, or when
    `terrain_fingerprint()` differs from the last *persisted* fingerprint. The runner already
    computes the fingerprint every 25 ticks; the checkpoint reuses it. The in-tick cost when
    terrain is unchanged is therefore the entity bundles only. When it changed, the in-tick
    cost is the `TerrainSnapshot` clone; **encode + zstd happen on the persistence thread.**
  - `Stopped(serialized, far)` gains the final checkpoint: `Stopped(serialized, far, checkpoint)`.
- The manager stamps each received checkpoint with `seq` from a monotonic counter seeded from
  `world.next_seq`, and hands it to the store. One transaction per region checkpoint:
  upsert `regions` (terrain columns only when `terrain` is `Some`), and for each entity
  `INSERT … ON CONFLICT (entity_id) DO UPDATE … WHERE entities.seq < EXCLUDED.seq`.
  **A checkpoint never deletes an entity by absence** — a body that left the region is
  overwritten by whoever holds it next.
- Why a manager-stamped `seq` is sufficient *in one instance*: every handoff passes through
  the manager, and crossbeam preserves per-sender order. A checkpoint R1 took before E
  departed is received before R1's `Departures`, hence before the manager relays the arrival,
  hence before any R2 checkpoint containing E. Receipt order respects handoff causality, so
  "highest seq wins" never resurrects a stale position. (Stage 1 replaces this with a
  per-entity handoff counter carried in the bundle, because two managers have no common order.)
- Resulting guarantee: after any crash, each entity resumes from its most recent checkpoint,
  in exactly one place; the loss window is ≤ one checkpoint interval; duplication and loss
  across in-process handoffs are impossible by construction (one row per entity).
- `[perf] event=region_checkpoint region=… ms=… entities=… terrain_bytes=…` and
  `event=region_restore …`, per checkpoint/restore (within the `perf.rs` volume contract);
  store queue depth and save latency join the periodic `[server]` line.

### 5. Restore

- `RegionSeed::Deferred` grows a store handle. On the region's own thread:
  `store.load_region(rc)` →
  - `Ok(None)`: never stored → generate (as today) → first checkpoint freezes it.
  - `Ok(Some(stored))`: `Region::from_chunks(id, stored.terrain)`, restore `tick`, then
    `apply_arrival` for every stored **non-player** entity with `in_world` in that region, in
    `entity_id` order, inside one forgotten transaction. **Player entities are never loaded
    with the region — only by login.** A crash therefore cannot leave orphan bodies: players
    who were online resume from their last checkpointed row when they log in again.
  - `Err(_)` (store unreachable) or an undecodable row: **the region does not start.** Retry
    with backoff, log at `error!`, surface as a stalled region to subscribers. **Never fall
    back to worldgen** — that would overwrite a real row at the next checkpoint. This
    deliberately inverts today's "corrupt parked blob → regenerate" rule, which stays valid
    only for the in-memory cache below.
- The in-memory `parked` map stays as a **warm cache in front of the store**: park writes the
  checkpoint to the store *and* keeps the blob; a resubscribe that hits the cache restores
  bit-exact as today (49 ms), a miss rebuilds from the store (≈ 0.5 s, off the manager
  thread). The difference is invisible to clients (every subscriber starts from a fresh
  snapshot) and keeps the existing park/restore tests valid. New: the cache is LRU-capped
  (`PARKED_CACHE_MAX`, blobs are ~40 MiB each in memory), and a blob whose store write has not
  been acknowledged is pinned.

### 6. Code shape

- `crates/game/src/persist.rs` (engine- and IO-free, sync, wasm-safe):
  `trait RegionStore { load_region, save_checkpoint, save_entity, load_player, login }`,
  the `Stored*V1` types + codecs, `MemoryStore`. The `WorldManager` takes a
  `Box<dyn RegionStore>` exactly like it takes a `LodBuilder`. wasm `LocalServer` and every
  existing test get `MemoryStore` and behave as before.
- `crates/server/src/persist_pg.rs`: `PgStore`. A dedicated persistence thread owns the
  Postgres pool on a small tokio runtime, fed by a crossbeam channel — the same
  async-at-the-edge pattern as the network ingress. Saves are **coalesced per region** (a newer
  checkpoint replaces a queued older one) and retried forever with backoff; loads and logins
  are request/reply. Migrations run at startup.
- Crate: `sqlx` (Postgres, runtime-checked `query()` API so builds never need a database;
  brings pool + migrations now and `PgListener` for stage 1). Server-only dependency.
- `DATABASE_URL` unset → `MemoryStore` plus one loud `warn!` that the world is not durable, so
  `just run` keeps working with zero setup. Set → `PgStore`; failing to connect at startup is
  fatal.
- Justfile: `pg-up` / `pg-down` (one `postgres` container, named volume), `run-durable`
  (`pg-up` + `run` with `DATABASE_URL`), `test-pg`.

### 7. Failure handling (summary)

| Failure | Behaviour |
|---|---|
| Store down during play | sim continues; checkpoints coalesce in memory; `[server]` line shows backlog + age of oldest unsaved; parked blobs stay pinned |
| Store down at region start / login | region does not start / login refused with a retryable error; never generate over unknown state |
| Undecodable stored row | that region refuses to start, `error!` with format byte; other regions unaffected |
| Server crash | restart; each region restores from its last checkpoint on demand; players log in to their last checkpointed body |
| Crash between logout extraction and its save | row still holds the previous checkpoint (`in_world = true`, last position) → login resumes from there |
| Duplicate login | newest session wins. The `saved` body the edge read from the store is **ignored** when the manager still holds a live session for that id (it would be stale): the manager sends `ClientLeft`, and re-inserts the freshly `Extracted` bundle for the new session itself, saving it on the way |

### 8. Testing

All in `game` with `MemoryStore` + `InlineSpawner` unless noted; fast ones in the `smoke`
binaries, anything > 100 ms tagged per the repo rule.

1. **Restore equivalence**: run, edit terrain via `with_data`, checkpoint, rebuild from store →
   `terrain_fingerprint` equal, entity bundles equal (modulo `EntityKey`), tick restored.
2. **Logout → login round trip**: pose, motion, camera, inventory preserved; observers see the
   body leave and return.
3. **Crash**: drop the manager without shutdown; new manager over the same store; login
   resumes from the last checkpoint; no body exists before login; no duplicate after a
   handoff-then-crash at each of the three interleavings (before R2's checkpoint, between,
   after R1's next).
4. **Arrival-identity regression** (finding 3): the X/Y scenario above must leave two players.
5. **Freeze**: a stored region is never regenerated even when the generator closure changes
   output; `edited` flips only on a fingerprint change.
6. **Store failure**: `load_region → Err` never calls the generator; saves coalesce and drain
   once the store recovers; an unacknowledged parked blob is not evicted.
7. **Golden blobs**: `StoredTerrainV1` / `StoredEntityV1` fixtures decode.
8. **Logout/crossing race** (§2).
9. `server`: `PgStore` integration tests behind `#[ignore]` + `DATABASE_URL` (`just test-pg`):
   migrations, the `seq` guard, coalescing, reconnect.
10. Client: login packet + persisted secret, native and wasm (type-check in `smoke`; one
    headless harness test).

### 9. Open points for review

1. `sqlx` (recommended) vs `tokio-postgres` + `deadpool` + `refinery`.
2. Dev login as described (name + locally stored secret, trust-on-first-use) until real auth?
3. Keep the in-memory parked-blob cache in front of the store (recommended; two restore paths,
   existing tests stay valid) vs. a single rebuild-only restore path (simpler, ~0.5 s per
   re-entry after the 5 s grace).
4. `CHECKPOINT_INTERVAL_TICKS` default of 30 s — the amount of progress a crash may cost.
