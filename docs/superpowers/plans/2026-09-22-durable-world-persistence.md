# Durable World Persistence (multi-instance stage 0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One server instance whose world — frozen terrain, player bodies, inventories — lives in Postgres and survives restarts and crashes; players have a stable account identity; logout takes the body out of the world and login puts it back.

**Architecture:** A sync `RegionStore` trait in `game` (engine- and IO-free, with an in-memory `MemoryStore` for tests, wasm and DB-less dev runs) is injected into the `WorldManager` and the region spawners, the same way `LodBuilder` is. Regions emit periodic `Checkpoint`s (entity bundles + terrain only when it changed); the manager stamps a monotonic `seq` and enqueues them; restore rebuilds a region from stored terrain + stored non-player entities through the existing `Region::from_chunks` / `apply_arrival` paths. `crates/server` provides `PgStore` (sqlx on a dedicated persistence thread, zstd-compressed bincode rows) and a login handshake at the QUIC/WebTransport edge.

**Tech Stack:** Rust, sqlx 0.9 (Postgres, tokio runtime, migrations), zstd, sha2, bincode, crossbeam, Postgres 17 (docker, dev only).

**Spec:** `docs/superpowers/specs/2026-09-20-durable-world-persistence-design.md` (parent: `docs/superpowers/specs/2026-09-20-multi-instance-k8s-postgres-research.md` §1a, §6, §12).

## Global Constraints

- Work on `develop` in the main checkout. **Never create a git worktree** (CLAUDE.md). The tree has many uncommitted user changes: stage only the files each task names (`git add <paths>`, never `git add -A`/`.`).
- `game` stays Bevy-free, IO-free, sync and wasm-safe: no sqlx, tokio, zstd, sha2 or filesystem in `game`. Postgres, compression and hashing live only in `crates/server`.
- Serialization on the wire only through `game::encode_packet` / `decode_packet`.
- Build/test only through `just` (pinned to 80 % of cores). Verify with `just test-one <crate> "<--test|--lib target> <filter>"` and `just smoke`; after touching handoff / `WorldManager` / `region_threads` also run `just test-threads`. Never run whole suites unless the user asks.
- Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
- Tunables are named constants: `CHECKPOINT_INTERVAL_TICKS`, `PARKED_CACHE_MAX`, `STORE_RETRY_MAX`.
- `ClientId` stays `usize` in the sim; account ids are `1..=i32::MAX` (wasm32 decodes `usize` as u32 — see memory note on the wasm32 usize hazard).
- **Never regenerate a region over a stored row**: a store error at region start stops that region, it never falls back to worldgen.
- A checkpoint never deletes an entity by absence.
- Keep the justfile in sync with any new command (CLAUDE.md rule).
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.

## Decisions and deviations from the spec (read before starting)

User-confirmed: sqlx; freeze-on-first-run; logout removes the body; entity-generic rows.
The spec's other review points were not answered explicitly; this plan takes the spec's recommendations and records these deviations so a reviewer can object:

1. **Dev login kept as specced** (name + client-kept 32-byte secret, trust on first use) — **but a second login for an account that is already online is rejected** (`LoginRejected::AlreadyOnline`) instead of "newest wins". Newest-wins needs per-connection generations in `ServerEvent::ClientDisconnected` to stop the old connection's late disconnect from tearing down the new session; rejection is safe and small. Cost: after a client crash the account can log in again only once the old QUIC/WT connection idles out (10 s on WebTransport, quinn's idle timeout natively).
2. **Warm parked-blob cache kept** in front of the store, LRU-capped at `PARKED_CACHE_MAX`, an entry evictable only after the store acknowledged the region's last save.
3. `CHECKPOINT_INTERVAL_TICKS` = 30 s of ticks.
4. Resuming bodies arrive through a **new** `ServerEvent::ClientResumed { id, saved }` rather than changing `ClientConnected(ClientId)` — `ClientConnected` has ~40 call sites in tests, and "connected with no stored body" is exactly its current meaning.
5. **Stored types are the sim types behind a format byte + checked-in golden blobs**, not hand-written `StoredTerrainV1` mirror structs. A layout change in `Chunk`/`EntityBundle`/`Client`/`Inventory`/`Motion` then fails the golden test instead of failing to compile — same forcing function, a fraction of the code.
6. `world.next_seq` column dropped: the store reports `max(entities.seq)` at startup.
7. `world_seed` becomes `world_identity text` (`"seed:<hex>"` or `"forged:<crc>"`), because `VOXEL_WORLD_FILE` forged worlds (landed after the spec) ignore the seed.

Known pre-existing state: `crates/client/tests/crossing.rs` has been red since `8e90481` (memory: region-threading-tests). Do not use it as a gate; do not try to fix it here.

## File Structure

| File | Responsibility |
|---|---|
| `crates/game/src/state.rs` (modify) | `EntityId`; `ecs.id` component; arrivals/ghosts keyed by `EntityId`; `bundle_of`; `owned_bundles` |
| `crates/game/src/protocol.rs` (modify) | `EntityBundle.id` / `GhostData.id` (drop `source_key`); `GameEventKind::ClientLeft`; `ClientPacket::Login`; `ServerPacket::LoginResult`; `LoginRejected` |
| `crates/game/src/region.rs` (modify) | `ClientLeft` handler, `pending_logouts`, `take_logouts` |
| `crates/game/src/persist.rs` (create) | `RegionStore` trait, `StoredRegion`, `RegionCheckpoint`, `SavedBody`, `LoginOutcome`, `StoreError`, `StoreEvent`, `MemoryStore` |
| `crates/game/src/region_runner.rs` (modify) | `Extracted`/`LogoutMissed`/`Checkpoint` outputs, `Stopped` carries a checkpoint, `RegionSeed::Stored`, store-aware `into_region_with`, `CHECKPOINT_INTERVAL_TICKS` |
| `crates/game/src/world_manager.rs` (modify) | `with_store`, seq stamping, logout/resume flow, departure divert, parked LRU + ack pinning, restore from store on the inline path |
| `crates/game/tests/persistence.rs` (create) | all store-level behaviour tests (MemoryStore + InlineSpawner) |
| `crates/server/src/region_threads.rs` (modify) | load from store on the region thread, retry on store error |
| `crates/server/src/persist_pg.rs` (create) | `PgStore`: persistence thread, sqlx pool, coalescing, retries |
| `crates/server/src/persist_codec.rs` (create) | format byte + zstd(bincode) row codecs |
| `crates/server/migrations/0001_init.sql` (create) | schema |
| `crates/server/src/login.rs` (create) | transport-independent login resolution |
| `crates/server/src/lib.rs`, `webtransport.rs` (modify) | store selection, login handshake, drop `AtomicUsize` ids |
| `crates/server/tests/persist_codec.rs` + `tests/fixtures/*.bin` (create) | golden blobs |
| `crates/server/tests/persist_pg.rs` (create) | Postgres integration tests (ignored unless `DATABASE_URL`) |
| `crates/worldgen/src/cache.rs` (modify) | `world_identity`, `worldgen_version` |
| `crates/client/src/account.rs` (create) | load-or-create credentials (native file / wasm localStorage) |
| `crates/client/src/netcode.rs`, `netcode_web.rs` (modify) | login before any other traffic |
| `justfile`, `CLAUDE.md`, `TODO.md` (modify) | recipes and docs |

---

### Task 1: Persistent `EntityId` as the handoff identity

Fixes spec finding 3 (a region rebuilt from storage reuses slotmap keys, so `(source_region, EntityKey)` can merge two players). Pure `game` change; no persistence yet.

**Files:**
- Modify: `crates/game/src/state.rs` (types near `ClientId` at :26; `Ecs` at ~:183; `GameData.ghosts`/`arrivals` at ~:176-180; `create_player_safe` ~:829; `remove_entity_safe` ~:836-880; `apply_arrival` ~:971; `apply_ghost` ~:1058; `extract_entity_safe` ~:1160; `scan_boundaries` ~:1215)
- Modify: `crates/game/src/protocol.rs:197-227` (bundle/ghost), `:385-395` (`matches_prediction`)
- Modify tests: `crates/game/tests/handoff_protocol.rs:70,85`, `handoff_scan.rs:181,215,269`, `handoff_state.rs:106,113`; `crates/client/src/instance.rs:1944,2178` (test code)
- Create: `crates/game/tests/persistent_identity.rs`

**Interfaces:**
- Produces: `game::EntityId` (`Copy + Ord + Hash + Default + Serialize`), `EntityId::player(ClientId) -> EntityId`, `EntityId::client(self) -> Option<ClientId>`; `EntityBundle.id: EntityId`, `GhostData.id: EntityId` (`source_key` removed, `source_region` kept); `Rollback::bundle_of(&self, key: EntityKey, region: RegionCoords) -> EntityBundle`; `Rollback::owned_bundles(&self, region: RegionCoords) -> Vec<EntityBundle>` (every entity with an `ecs.id`, in `EntityId` order).

- [ ] **Step 1: Write the failing regression test**

`crates/game/tests/persistent_identity.rs`:

```rust
//! Handoff identity must survive a region being rebuilt (restore-by-rebuild
//! resets slotmap keys). Before EntityId, player Y arriving from a rebuilt
//! region was mistaken for player X's replayed arrival and vanished.
use game::{GameEventKind, Region, RegionCoords};

fn cross(src: &mut Region, client: usize, target: RegionCoords) -> game::EntityBundle {
    let key = *src.data().player_entites.get(&client).unwrap();
    let rc = src.id();
    let mut iso = src.data().physics.bodies[src.data().ecs.rigidbody.try_get(key).unwrap()]
        .position()
        .clone();
    iso.translation.vector.x = game::REGION_SIZE + 10.0; // past +x edge + hysteresis
    src.with_data(|d| d.set_body_pose_safe(key, iso));
    src.handle_event(GameEventKind::Tick).unwrap();
    src.forget_last_event();
    let (mut deps, _) = src.take_transfers();
    let (mut bundle, tgt) = deps.remove(0);
    assert_eq!(tgt, target);
    bundle.isometry = game::rebase_isometry(&bundle.isometry, rc, target);
    bundle
}

#[test]
fn arrival_from_a_rebuilt_region_is_a_new_entity() {
    let r = RegionCoords::new(0, 0);
    let t = RegionCoords::new(1, 0);
    let mut target = Region::from_chunks(t, Vec::new());

    let mut src = Region::from_chunks(r, Vec::new());
    src.handle_event(GameEventKind::CreateClient(1)).unwrap();
    src.forget_last_event();
    let x = cross(&mut src, 1, t);
    target.handle_event(GameEventKind::EntityArrived(x)).unwrap();
    target.forget_last_event();

    // R is rebuilt (same deterministic genesis): Y gets X's old slot key.
    let mut rebuilt = Region::from_chunks(r, Vec::new());
    rebuilt.handle_event(GameEventKind::CreateClient(2)).unwrap();
    rebuilt.forget_last_event();
    let y = cross(&mut rebuilt, 2, t);
    target.handle_event(GameEventKind::EntityArrived(y)).unwrap();
    target.forget_last_event();

    assert!(target.data().player_entites.contains_key(&1));
    assert!(target.data().player_entites.contains_key(&2), "Y merged into X");
    assert_ne!(target.data().player_entites[&1], target.data().player_entites[&2]);
}

#[test]
fn replayed_arrival_is_still_idempotent() {
    let r = RegionCoords::new(0, 0);
    let t = RegionCoords::new(1, 0);
    let mut src = Region::from_chunks(r, Vec::new());
    src.handle_event(GameEventKind::CreateClient(1)).unwrap();
    src.forget_last_event();
    let x = cross(&mut src, 1, t);
    let mut target = Region::from_chunks(t, Vec::new());
    for _ in 0..2 {
        target.handle_event(GameEventKind::EntityArrived(x.clone())).unwrap();
        target.forget_last_event();
    }
    let players = target.data().ecs.kind.iter().filter(|(_, k)| k.is_some()).count();
    assert_eq!(players, 1);
    assert_eq!(x.id, game::EntityId::player(1));
}
```

If `Region::id()` or `set_body_pose_safe` visibility differ, use the accessors the existing `crates/game/tests/handoff_state.rs` uses for the same purpose (it builds bundles by hand and teleports via `with_data`); keep the scenario identical.

- [ ] **Step 2: Run it — expect a compile failure (`EntityId`, `bundle.id` do not exist)**

Run: `just test-one game "--test persistent_identity"`
Expected: FAIL, `cannot find type EntityId` / `no field id`.

- [ ] **Step 3: Add `EntityId` and the component**

In `crates/game/src/state.rs` next to `pub type ClientId = usize;`:

```rust
/// Persistent entity identity: stable across region rebuilds, handoffs,
/// logout/login and (later) pods. The top byte is a namespace; players are
/// namespace 0 with the account's client id in the low bits, so the sim can
/// derive a player's id from `CreateClient` with no allocation. Handoff
/// dedup (`arrivals`), ghost mirrors and persisted rows all key on it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
    serde::Serialize, serde::Deserialize,
)]
pub struct EntityId(pub u64);

impl EntityId {
    const NS_SHIFT: u32 = 56;
    pub const PLAYER_NS: u64 = 0;

    pub fn player(client: ClientId) -> Self {
        EntityId((Self::PLAYER_NS << Self::NS_SHIFT) | client as u64)
    }

    /// The owning client if this is a player id.
    pub fn client(self) -> Option<ClientId> {
        (self.0 >> Self::NS_SHIFT == Self::PLAYER_NS).then(|| (self.0 & ((1 << Self::NS_SHIFT) - 1)) as ClientId)
    }
}
```

In `Ecs` add a column after `motion`: `id: Component<crate::EntityId>,` (the `#[rollback]` macro handles `Component<T>` columns uniformly; check `use super::{…}` inside `mod game_data` and add `EntityId` if the macro module needs it in scope). Re-key the two maps:

```rust
ghosts: BTreeMap<crate::EntityId, GhostEntry>,
arrivals: BTreeMap<crate::EntityId, EntityKey>,
```

- [ ] **Step 4: Thread the id through bundles, ghosts and the create/remove paths**

`crates/game/src/protocol.rs`: in `EntityBundle` replace `pub source_key: EntityKey,` with `pub id: crate::EntityId,`; same in `GhostData`. Update the doc comment above (`source_region`/`source_key` → "`id` is the identity token; `source_region` is where it came from"). In `matches_prediction` replace both `a.source_key == b.source_key` comparisons with `a.id == b.id` (keep the `source_region` equality).

`state.rs`:
- `create_player_safe`: after the `motion` line add `self.data.ecs.id.set_safe(e, Some(crate::EntityId::player(client_id)));`
- `remove_entity_safe`: where `kind`/`motion` are cleared add `self.data.ecs.id.set_safe(key, None);` (`set_safe` is undo-logged like the others).
- `apply_arrival`: `let identity = bundle.id;` and after the kind is set: `self.data.ecs.id.set_safe(e, Some(bundle.id));`. Everything else unchanged.
- `apply_ghost`: `let identity = data.id;` (ghosts do **not** get an `ecs.id` — they are mirrors; the upgrade path in `apply_arrival` sets it).
- `expire_ghosts`: key type follows the map.
- Split `extract_entity_safe`: move everything up to building `bundle` into

```rust
/// Read-only half of extraction: the entity as a transferable/persistable
/// bundle. `region` is recorded as `source_region`.
pub fn bundle_of(&self, key: EntityKey, region: crate::RegionCoords) -> crate::EntityBundle { … }
```

and have `extract_entity_safe` call `let bundle = self.bundle_of(key, region);` then do the removals. In `bundle_of` set `id: (*self.data.ecs.id.try_get(key)).expect("owned entities carry an EntityId")`. In the stale-arrival cleanup inside `extract_entity_safe`, filter on `**e == key` exactly as today (the map value is still the `EntityKey`).
- `scan_boundaries`: GhostData gets `id: (*self.data.ecs.id.try_get(key)).expect(…)` instead of `source_key: key`.
- Add:

```rust
/// Every owned (id-carrying) entity as a bundle, in EntityId order — the
/// entity half of a region checkpoint. Ghosts carry no id and are skipped.
pub fn owned_bundles(&self, region: crate::RegionCoords) -> Vec<crate::EntityBundle> {
    let mut keyed: Vec<(crate::EntityId, EntityKey)> = self
        .data
        .ecs
        .id
        .iter()
        .filter_map(|(key, id)| id.map(|id| (id, key)))
        .collect();
    keyed.sort();
    keyed.into_iter().map(|(_, key)| self.bundle_of(key, region)).collect()
}
```

(match the element shape `Component::iter` yields — `scan_boundaries` uses `for (key, kind) in self.data.ecs.kind.iter()`.)

- [ ] **Step 5: Fix the test fixtures that set `source_key`**

In `handoff_protocol.rs`, `handoff_scan.rs`, `handoff_state.rs`: replace `source_key: <expr>,` with `id: game::EntityId::player(<the client id used by that test>),` for player bundles, or `id: game::EntityId(<distinct literal>),` where the test builds a non-player/ghost fixture. Where a test compares identities via `source_key`, compare `id`. In `crates/client/src/instance.rs` (test code at ~1944 and ~2178) delete the `bundle.source_key = player_key;` lines — both sides now derive `EntityId::player(0)` from `CreateClient(0)`, so the identities match without forcing.

- [ ] **Step 6: Run the new tests and the handoff suites**

Run: `just test-one game "--test persistent_identity"` → PASS (2 tests).
Run: `just test-one game "--test handoff_state"`, `just test-one game "--test handoff_scan"`, `just test-one game "--test handoff_protocol"` → PASS.
Run: `just smoke` → PASS (type-checks client lib + tests).
Run: `just test-threads` → PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/game/src/state.rs crates/game/src/protocol.rs crates/game/tests/persistent_identity.rs \
  crates/game/tests/handoff_protocol.rs crates/game/tests/handoff_scan.rs crates/game/tests/handoff_state.rs \
  crates/client/src/instance.rs
git commit -m "feat(game): persistent EntityId as handoff and ghost identity

Rebuilding a region resets its slotmap, so (source_region, EntityKey)
could merge a new arrival into an earlier one. Players derive their id
from the client id.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

Note: `crates/client/src/instance.rs` has unrelated uncommitted user edits. Before `git add`, run `git diff crates/client/src/instance.rs`; if hunks other than yours exist, stage only yours with `git add -p crates/client/src/instance.rs` answering from the diff (the `-p` prompt is interactive — if unavailable, leave the file unstaged and say so in the task report).

---

### Task 2: `RegionStore` trait and `MemoryStore`

**Files:**
- Create: `crates/game/src/persist.rs`
- Modify: `crates/game/src/lib.rs` (add `mod persist; pub use persist::*;` next to the other glob re-exports)
- Create: `crates/game/tests/persistence.rs`

**Interfaces:**
- Consumes: `EntityBundle`, `EntityId`, `TerrainSnapshot`, `ChunkCoords`, `Chunk`, `RegionCoords`, `ClientId`.
- Produces (exact):

```rust
pub struct StoredRegion { pub tick: u64, pub chunks: Vec<(ChunkCoords, Chunk)>, pub entities: Vec<EntityBundle> }
pub struct RegionCheckpoint { pub region: RegionCoords, pub tick: u64, pub entities: Vec<EntityBundle>, pub terrain: Option<TerrainWrite> }
pub struct TerrainWrite { pub snapshot: TerrainSnapshot, pub is_edit: bool }
pub struct SavedBody { pub region: RegionCoords, pub bundle: EntityBundle }
pub enum LoginOutcome { Accepted { client_id: ClientId, saved: Option<SavedBody> }, WrongSecret }
pub struct StoreError(pub String);
pub enum StoreEvent { Saved { region: RegionCoords, seq: u64 } }
pub trait RegionStore: Send + Sync { fn load_region(..); fn login(..); fn max_seq(..); fn save_checkpoint(..); fn save_logout(..); fn flush(..) }
pub struct MemoryStore; MemoryStore::new() -> (Arc<MemoryStore>, Receiver<StoreEvent>)
```

- [ ] **Step 1: Write the failing store-semantics tests**

`crates/game/tests/persistence.rs`:

```rust
//! Store-level persistence behaviour against MemoryStore (+ InlineSpawner
//! in later tasks). PgStore implements the same contract (server tests).
use game::{
    EntityBundle, EntityId, LoginOutcome, MemoryStore, RegionCheckpoint, RegionCoords, RegionStore,
    TerrainSnapshot, TerrainWrite,
};

fn body(client: usize, x: f32) -> EntityBundle {
    let mut region = game::Region::from_chunks(RegionCoords::new(0, 0), Vec::new());
    region.handle_event(game::GameEventKind::CreateClient(client)).unwrap();
    region.forget_last_event();
    let key = region.data().player_entites[&client];
    let mut b = region.data().bundle_of(key, RegionCoords::new(0, 0));
    b.isometry.translation.vector.x = x;
    b
}

fn cp(rc: RegionCoords, entities: Vec<EntityBundle>, terrain: bool) -> RegionCheckpoint {
    RegionCheckpoint {
        region: rc,
        tick: 7,
        entities,
        terrain: terrain.then(|| TerrainWrite { snapshot: TerrainSnapshot { chunks: Vec::new() }, is_edit: false }),
    }
}

#[test]
fn unknown_region_loads_as_none_and_first_checkpoint_freezes_it() {
    let (store, _ev) = MemoryStore::new();
    let rc = RegionCoords::new(3, -2);
    assert!(store.load_region(rc).unwrap().is_none());
    store.save_checkpoint(1, cp(rc, Vec::new(), true));
    let stored = store.load_region(rc).unwrap().expect("frozen");
    assert_eq!(stored.tick, 7);
}

#[test]
fn older_seq_never_overwrites_an_entity() {
    let (store, _ev) = MemoryStore::new();
    let a = RegionCoords::new(0, 0);
    let b = RegionCoords::new(1, 0);
    // A non-player entity (players are never loaded with a region).
    let mut npc = body(5, 1.0);
    npc.client = None;
    npc.id = EntityId(1 << 56 | 9);
    store.save_checkpoint(2, cp(b, vec![npc.clone()], true));
    store.save_checkpoint(1, cp(a, vec![npc.clone()], true)); // stale: must lose
    assert_eq!(store.load_region(b).unwrap().unwrap().entities.len(), 1);
    assert!(store.load_region(a).unwrap().unwrap().entities.is_empty());
}

#[test]
fn checkpoint_never_deletes_by_absence_and_players_stay_out_of_region_loads() {
    let (store, _ev) = MemoryStore::new();
    let rc = RegionCoords::new(0, 0);
    store.save_checkpoint(1, cp(rc, vec![body(1, 2.0)], true));
    store.save_checkpoint(2, cp(rc, Vec::new(), false));
    assert!(store.load_region(rc).unwrap().unwrap().entities.is_empty(), "players load via login only");
    let LoginOutcome::Accepted { saved, .. } = store.login("p1", &[0; 32]).unwrap() else { panic!() };
    // login created a NEW account (id 1 is the first) whose body is the stored player 1
    assert_eq!(saved.expect("body kept").bundle.isometry.translation.vector.x, 2.0);
}

#[test]
fn login_is_trust_on_first_use() {
    let (store, _ev) = MemoryStore::new();
    let LoginOutcome::Accepted { client_id, saved } = store.login("ana", &[1; 32]).unwrap() else { panic!() };
    assert!(client_id >= 1 && saved.is_none());
    assert!(matches!(store.login("ana", &[2; 32]).unwrap(), LoginOutcome::WrongSecret));
    let LoginOutcome::Accepted { client_id: again, .. } = store.login("ana", &[1; 32]).unwrap() else { panic!() };
    assert_eq!(again, client_id);
}

#[test]
fn logout_saves_body_and_acks() {
    let (store, events) = MemoryStore::new();
    let LoginOutcome::Accepted { client_id, .. } = store.login("bo", &[3; 32]).unwrap() else { panic!() };
    let rc = RegionCoords::new(2, 2);
    store.save_logout(4, rc, body(client_id, 9.0));
    assert!(matches!(events.try_recv(), Ok(game::StoreEvent::Saved { region, seq: 4 }) if region == rc));
    let LoginOutcome::Accepted { saved, .. } = store.login("bo", &[3; 32]).unwrap() else { panic!() };
    assert_eq!(saved.unwrap().region, rc);
    assert_eq!(store.max_seq().unwrap(), 4);
}
```

- [ ] **Step 2: Run — expect compile failure**

Run: `just test-one game "--test persistence"` → FAIL (`MemoryStore` unresolved).

- [ ] **Step 3: Implement `persist.rs`**

```rust
//! The persistence seam: what a region/world store must do, independent of
//! where the bytes go. `game` stays IO-free — PgStore lives in
//! crates/server; MemoryStore serves tests, wasm and DB-less dev runs.
//!
//! Blocking vs non-blocking is part of the contract: `load_region`,
//! `login`, `max_seq` and `flush` may block (region threads, the network
//! edge via spawn_blocking, startup/shutdown) and must NEVER be called from
//! the manager thread's routing path. `save_*` only enqueue and are safe
//! anywhere.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::{Chunk, ChunkCoords, ClientId, EntityBundle, EntityId, RegionCoords, TerrainSnapshot};

/// A region as persisted: frozen/edited terrain plus the non-player
/// entities that were in it. Players are loaded by login only.
#[derive(Debug, Clone)]
pub struct StoredRegion {
    pub tick: u64,
    pub chunks: Vec<(ChunkCoords, Chunk)>,
    pub entities: Vec<EntityBundle>,
}

#[derive(Debug, Clone)]
pub struct TerrainWrite {
    pub snapshot: TerrainSnapshot,
    /// false = first freeze of worldgen output; true = terrain changed since.
    pub is_edit: bool,
}

/// One region's periodic save. `terrain` is `Some` only on the first
/// checkpoint of a never-stored region or when the terrain changed.
#[derive(Debug, Clone)]
pub struct RegionCheckpoint {
    pub region: RegionCoords,
    pub tick: u64,
    pub entities: Vec<EntityBundle>,
    pub terrain: Option<TerrainWrite>,
}

#[derive(Debug, Clone)]
pub struct SavedBody {
    pub region: RegionCoords,
    pub bundle: EntityBundle,
}

#[derive(Debug, Clone)]
pub enum LoginOutcome {
    Accepted { client_id: ClientId, saved: Option<SavedBody> },
    WrongSecret,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "store error: {}", self.0)
    }
}

/// Store → manager notifications, drained like LOD results.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreEvent {
    /// Everything for `region` up to `seq` is durable.
    Saved { region: RegionCoords, seq: u64 },
}

pub trait RegionStore: Send + Sync {
    /// Blocking. `Ok(None)` = never stored (generate and freeze).
    fn load_region(&self, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError>;
    /// Blocking. Creates the account on first use of `name`.
    fn login(&self, name: &str, secret: &[u8; 32]) -> Result<LoginOutcome, StoreError>;
    /// Blocking. Highest `seq` ever saved (0 for an empty store).
    fn max_seq(&self) -> Result<u64, StoreError>;
    /// Non-blocking enqueue. Upserts every entity with `seq` greater than
    /// its stored one (in-world, in `cp.region`); never deletes by absence.
    fn save_checkpoint(&self, seq: u64, cp: RegionCheckpoint);
    /// Non-blocking enqueue. Stores a logged-out body (`in_world = false`).
    fn save_logout(&self, seq: u64, region: RegionCoords, bundle: EntityBundle);
    /// Blocking. Wait until everything enqueued is durable; false on timeout.
    fn flush(&self, timeout: Duration) -> bool;
}

#[derive(Debug, Clone)]
struct EntityRow {
    region: RegionCoords,
    in_world: bool,
    seq: u64,
    bundle: EntityBundle,
}

#[derive(Default)]
struct MemoryState {
    regions: BTreeMap<RegionCoords, (u64, Vec<(ChunkCoords, Chunk)>)>,
    entities: BTreeMap<EntityId, EntityRow>,
    accounts: BTreeMap<String, (ClientId, [u8; 32])>,
    next_client: ClientId,
    max_seq: u64,
}

/// In-process store with the same semantics as PgStore. Saves apply
/// immediately and are acknowledged on the event channel.
pub struct MemoryStore {
    state: Mutex<MemoryState>,
    events: Sender<StoreEvent>,
}

impl MemoryStore {
    pub fn new() -> (Arc<Self>, Receiver<StoreEvent>) {
        let (events, recv) = unbounded();
        let state = MemoryState { next_client: 1, ..Default::default() };
        (Arc::new(Self { state: Mutex::new(state), events }), recv)
    }

    fn upsert(st: &mut MemoryState, seq: u64, region: RegionCoords, in_world: bool, bundle: EntityBundle) {
        let fresher = st.entities.get(&bundle.id).map_or(true, |row| row.seq < seq);
        if fresher {
            st.entities.insert(bundle.id, EntityRow { region, in_world, seq, bundle });
        }
    }
}

impl RegionStore for MemoryStore {
    fn load_region(&self, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError> {
        let st = self.state.lock().unwrap();
        Ok(st.regions.get(&rc).map(|(tick, chunks)| StoredRegion {
            tick: *tick,
            chunks: chunks.clone(),
            entities: st
                .entities
                .values()
                .filter(|row| row.in_world && row.region == rc && row.bundle.client.is_none())
                .map(|row| row.bundle.clone())
                .collect(),
        }))
    }

    fn login(&self, name: &str, secret: &[u8; 32]) -> Result<LoginOutcome, StoreError> {
        let mut st = self.state.lock().unwrap();
        let client_id = match st.accounts.get(name) {
            Some((_, s)) if s != secret => return Ok(LoginOutcome::WrongSecret),
            Some((id, _)) => *id,
            None => {
                let id = st.next_client;
                st.next_client += 1;
                st.accounts.insert(name.to_string(), (id, *secret));
                id
            }
        };
        let saved = st
            .entities
            .get(&EntityId::player(client_id))
            .map(|row| SavedBody { region: row.region, bundle: row.bundle.clone() });
        Ok(LoginOutcome::Accepted { client_id, saved })
    }

    fn max_seq(&self) -> Result<u64, StoreError> {
        Ok(self.state.lock().unwrap().max_seq)
    }

    fn save_checkpoint(&self, seq: u64, cp: RegionCheckpoint) {
        let mut st = self.state.lock().unwrap();
        st.max_seq = st.max_seq.max(seq);
        match cp.terrain {
            Some(write) => {
                st.regions.insert(cp.region, (cp.tick, write.snapshot.chunks));
            }
            None => {
                if let Some(entry) = st.regions.get_mut(&cp.region) {
                    entry.0 = cp.tick;
                }
            }
        }
        for bundle in cp.entities {
            Self::upsert(&mut st, seq, cp.region, true, bundle);
        }
        let _ = self.events.send(StoreEvent::Saved { region: cp.region, seq });
    }

    fn save_logout(&self, seq: u64, region: RegionCoords, bundle: EntityBundle) {
        let mut st = self.state.lock().unwrap();
        st.max_seq = st.max_seq.max(seq);
        Self::upsert(&mut st, seq, region, false, bundle);
        let _ = self.events.send(StoreEvent::Saved { region, seq });
    }

    fn flush(&self, _timeout: Duration) -> bool {
        true
    }
}
```

Note on the `checkpoint_never_deletes…` test: account "p1" is created by login with client id 1 (first account), and the checkpoint stored `EntityId::player(1)` — that is how the test proves the body is looked up by id.

- [ ] **Step 4: Run** `just test-one game "--test persistence"` → PASS (5 tests). `just smoke` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/persist.rs crates/game/src/lib.rs crates/game/tests/persistence.rs
git commit -m "feat(game): RegionStore persistence seam with an in-memory store

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Logout extracts the body; login resumes it

**Files:**
- Modify: `crates/game/src/protocol.rs` (`GameEventKind::ClientLeft(ClientId)`; `origin_client` → `None`)
- Modify: `crates/game/src/region.rs` (handler ~:520, `pending_logouts`, `take_logouts`, the event-label match ~:142)
- Modify: `crates/game/src/region_runner.rs` (`RegionOutput::Extracted`, `LogoutMissed`; emit after `apply`)
- Modify: `crates/game/src/world_manager.rs` (`ServerEvent::ClientResumed`, `with_store`, `ClientDisconnected`, `Departures`, new outputs)
- Modify tests that relied on bodies surviving a disconnect (see Step 6)
- Test: `crates/game/tests/persistence.rs`

**Interfaces:**
- Consumes: Task 2's `RegionStore`, `MemoryStore`, `StoreEvent`, `SavedBody`; Task 1's `EntityId`, `bundle_of`.
- Produces: `GameEventKind::ClientLeft(ClientId)`; `Region::take_logouts(&mut self) -> Vec<(ClientId, Option<EntityBundle>)>`; `RegionOutput::Extracted(ClientId, EntityBundle)`, `RegionOutput::LogoutMissed(ClientId)`; `ServerEvent::ClientResumed { id: ClientId, saved: SavedBody }`; `WorldManager::with_store(self, store: Arc<dyn RegionStore>, events: Receiver<StoreEvent>) -> Self`; `WorldManager::pump_store_events(&mut self)`; `WorldManager::homes(&self) -> &BTreeMap<ClientId, RegionCoords>` (test accessor).

- [ ] **Step 1: Write the failing manager-level tests** — append to `crates/game/tests/persistence.rs` a harness copied from `crates/game/tests/world_manager.rs:1-70` (the `Harness`/`harness()`/`settle`/`event` block, verbatim) with one change: build the manager as `WorldManager::new(…).with_store(store.clone(), store_events)` and keep `store: Arc<MemoryStore>` on the harness. Then:

```rust
fn tick(h: &mut Harness, now_ms: u64) {
    h.manager.spawner_mut().tick_all();
    h.settle(now_ms);
}

#[test]
fn logout_extracts_the_body_and_login_puts_it_back() {
    let mut h = harness();
    let LoginOutcome::Accepted { client_id: c, saved: None } = h.store.login("ana", &[7; 32]).unwrap() else { panic!() };
    h.event(ServerEvent::ClientConnected(c), 0);
    tick(&mut h, 50);
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| assert!(r.data().player_entites.contains_key(&c)));

    h.event(ServerEvent::ClientDisconnected(c), 100);
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| assert!(!r.data().player_entites.contains_key(&c)));
    assert!(!h.manager.homes().contains_key(&c));

    let LoginOutcome::Accepted { saved: Some(saved), .. } = h.store.login("ana", &[7; 32]).unwrap() else { panic!("body not stored") };
    assert_eq!(saved.region, SPAWN_REGION);
    assert_eq!(saved.bundle.id, EntityId::player(c));
    h.event(ServerEvent::ClientResumed { id: c, saved }, 200);
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| assert!(r.data().player_entites.contains_key(&c)));
}

#[test]
fn quick_reconnect_during_logout_reinserts_the_fresh_body() {
    let mut h = harness();
    let LoginOutcome::Accepted { client_id: c, .. } = h.store.login("bo", &[1; 32]).unwrap() else { panic!() };
    h.event(ServerEvent::ClientConnected(c), 0);
    // Disconnect and resume BEFORE the region processes ClientLeft: the
    // stored row is stale (there is none yet) — the manager must wait for
    // the extraction and re-insert that bundle.
    h.manager.handle_server_event(ServerEvent::ClientDisconnected(c), 10);
    let stale = SavedBody { region: SPAWN_REGION, bundle: {
        let mut b = h.first_player_bundle(c); b.isometry.translation.vector.y = -999.0; b } };
    h.manager.handle_server_event(ServerEvent::ClientResumed { id: c, saved: stale }, 11);
    h.settle(12);
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let key = r.data().player_entites[&c];
        let y = r.data().bundle_of(key, SPAWN_REGION).isometry.translation.vector.y;
        assert!(y > -999.0, "stale stored body was used");
    });
}

#[test]
fn logout_racing_a_crossing_is_diverted_to_the_store_without_waking_the_target() {
    let mut h = harness();
    let LoginOutcome::Accepted { client_id: c, .. } = h.store.login("cy", &[2; 32]).unwrap() else { panic!() };
    h.event(ServerEvent::ClientConnected(c), 0);
    // Put the body past the +x edge; the next tick departs it.
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let key = r.data().player_entites[&c];
        let mut iso = r.data().bundle_of(key, SPAWN_REGION).isometry;
        iso.translation.vector.x = game::REGION_SIZE + 10.0;
        r.with_data(|d| d.set_body_pose_safe(key, iso));
    });
    h.manager.spawner_mut().tick_all();          // departure is now queued in region_out
    h.manager.handle_server_event(ServerEvent::ClientDisconnected(c), 50); // ClientLeft queued to (0,0)
    h.settle(51);
    let east = RegionCoords::new(1, 0);
    assert!(!h.manager.running_regions().contains(&east), "logout woke the target");
    let LoginOutcome::Accepted { saved: Some(saved), .. } = h.store.login("cy", &[2; 32]).unwrap() else { panic!() };
    assert_eq!(saved.region, east);
}
```

Add to the harness `fn first_player_bundle(&mut self, c: ClientId) -> EntityBundle` that reads it through `with_region(SPAWN_REGION, …)` into an `Option` and unwraps.

- [ ] **Step 2: Run** `just test-one game "--test persistence"` → FAIL (compile: `ClientResumed`, `with_store`, `homes`).

- [ ] **Step 3: Sim side — `ClientLeft`**

`protocol.rs`: add the variant with a doc comment ("Server-authoritative: remove the client's player body (logout). Manager-internal — a client sending it is dropped."), and list it in `origin_client`'s `None` arm.

`region.rs`: field `pending_logouts: Vec<(ClientId, Option<EntityBundle>)>` initialised in the constructor, and in `handle_event`:

```rust
GameEventKind::ClientLeft(client_id) => {
    // Read-after-event, REPLACED like pending_departures: the runner takes
    // it right after this event; a client replaying the echo discards it.
    let key = self.data.player_entites.get(&client_id).copied();
    let bundle = key.map(|key| self.data.extract_entity_safe(key, self.id));
    self.pending_logouts = vec![(client_id, bundle)];
}
```

and `pub fn take_logouts(&mut self) -> Vec<(ClientId, Option<EntityBundle>)> { std::mem::take(&mut self.pending_logouts) }`. Add the arm to the label/describe match near :142. Check `extract_entity_safe`'s `region` parameter name matches `self.id`.

`region_runner.rs`: add outputs

```rust
/// Reply to a routed `ClientLeft`: the extracted body, for storage.
Extracted(ClientId, EntityBundle),
/// `ClientLeft` found no such player here (it departed first).
LogoutMissed(ClientId),
```

and at the end of `RegionRunner::apply` (after sending `EventProcessed`):

```rust
for (client, bundle) in self.region.take_logouts() {
    let out = match bundle {
        Some(bundle) => RegionOutput::Extracted(client, bundle),
        None => RegionOutput::LogoutMissed(client),
    };
    let _ = self.out.send((self.id, out));
}
```

- [ ] **Step 4: Manager side**

`world_manager.rs`:

```rust
pub enum ServerEvent {
    ClientPacket(ClientPacket, ClientId),
    ClientConnected(ClientId),
    /// A logged-in account with a stored body (logout or crash): insert it
    /// instead of creating a fresh player.
    ClientResumed { id: ClientId, saved: crate::SavedBody },
    ClientDisconnected(ClientId),
}
```

New fields: `store: Arc<dyn RegionStore>`, `store_events: Receiver<StoreEvent>`, `seq: u64`, `logging_out: BTreeSet<ClientId>`, `resume_after_logout: BTreeSet<ClientId>`. In `new`, default to `let (store, store_events) = crate::MemoryStore::new();` (MemoryStore coerces to `Arc<dyn RegionStore>`), `seq: 0`. Add:

```rust
/// Persist through `store` instead of the default in-memory one. Blocks
/// once on `max_seq` (startup only).
pub fn with_store(mut self, store: Arc<dyn RegionStore>, events: Receiver<StoreEvent>) -> Self {
    self.seq = store.max_seq().expect("store unreachable at startup");
    self.store = store;
    self.store_events = events;
    self
}

fn next_seq(&mut self) -> u64 {
    self.seq += 1;
    self.seq
}

pub fn homes(&self) -> &BTreeMap<ClientId, RegionCoords> {
    &self.homes
}

pub fn pump_store_events(&mut self) {
    while let Ok(ev) = self.store_events.try_recv() {
        self.handle_store_event(ev);
    }
}

fn handle_store_event(&mut self, _ev: StoreEvent) {} // Task 4 adds parked-cache acks

fn begin_logout(&mut self, id: ClientId) {
    if let Some(home) = self.homes.get(&id).copied() {
        self.logging_out.insert(id);
        self.send_to_region(home, RegionInput::Event(GameEventKind::ClientLeft(id)));
    }
}

fn finish_logout(&mut self, id: ClientId, region: RegionCoords, bundle: EntityBundle, now_ms: u64) {
    self.logging_out.remove(&id);
    self.homes.remove(&id);
    let seq = self.next_seq();
    self.store.save_logout(seq, region, bundle.clone());
    if self.resume_after_logout.remove(&id) && self.sessions.contains_key(&id) {
        // Reconnected while the extraction was in flight: the store row the
        // edge read was stale; this bundle is the truth.
        self.insert_body(id, region, bundle);
    }
    self.refresh_keepalive(region, now_ms);
}

fn insert_body(&mut self, id: ClientId, region: RegionCoords, bundle: EntityBundle) {
    self.ensure_running(region);
    self.homes.insert(id, region);
    self.send_to_region(region, RegionInput::Event(GameEventKind::EntityArrived(bundle)));
}
```

Wire it:
- `ClientDisconnected(id)`: keep the existing unsubscribe code, replace the trailing `refresh_keepalive(home)` block with `self.begin_logout(id);`.
- `ClientResumed { id, saved }`: `self.sessions.insert(id, Session::default());` then `if self.logging_out.contains(&id) { self.resume_after_logout.insert(id); } else if !self.homes.contains_key(&id) { self.insert_body(id, saved.region, saved.bundle); } else { log::warn!("resume for client {id} whose body is still in the world; ignoring stored copy"); }`.
- Client-packet filter: add `GameEventKind::ClientLeft(_)` to the arm that drops `EntityArrived | GhostUpdate`.
- `handle_region_output`: `RegionOutput::Extracted(c, bundle) => self.finish_logout(c, rc, bundle, now_ms),` and `RegionOutput::LogoutMissed(c) => { if self.logging_out.contains(&c) { if let Some(home) = self.homes.get(&c).copied().filter(|h| *h != rc) { self.send_to_region(home, RegionInput::Event(GameEventKind::ClientLeft(c))); } } }` (the departure divert below usually finishes the logout first; this covers a departure relayed before the disconnect).
- `Departures`: at the top of the loop body, after computing `client` and rebasing:

```rust
if let Some(c) = client.filter(|c| self.logging_out.contains(c)) {
    if !self.resume_after_logout.contains(&c) {
        // Logged out mid-crossing: store the body where it was heading;
        // never wake the target for a player who is gone.
        self.finish_logout(c, target, bundle, now_ms);
        continue;
    }
    self.logging_out.remove(&c);
    self.resume_after_logout.remove(&c);
}
```

- `LocalServer` and every other `new` caller keep compiling (default store).

- [ ] **Step 5: Run** `just test-one game "--test persistence"` → PASS (8 tests).

- [ ] **Step 6: Update tests that assumed the body survives a disconnect**

Run: `just test-one game "--test world_manager"`, `just test-one game "--test lod_streaming"`, `just test-one game "--test handoff_manager"`. Any failure that asserts a region stays home / keeps the player / takes the reconnect branch after `ClientDisconnected` is testing the old semantics: change it to assert the new ones (the body is gone from the region; `homes` has no entry; a following `ClientConnected(id)` creates a fresh player). `lod_streaming.rs:280-294` explicitly documents "ClientConnected takes the reconnect branch" — rewrite that comment and assertion accordingly. Do not change assertions unrelated to disconnect. Then `just smoke` and `just test-threads` → PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/game/src/protocol.rs crates/game/src/region.rs crates/game/src/region_runner.rs \
  crates/game/src/world_manager.rs crates/game/tests/persistence.rs \
  crates/game/tests/world_manager.rs crates/game/tests/lod_streaming.rs crates/game/tests/handoff_manager.rs
git commit -m "feat(game): logout extracts the player body into the store, login resumes it

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

(Stage only the test files you actually changed.)

---

### Task 4: Periodic checkpoints, seq stamping, parked-cache pinning

**Files:**
- Modify: `crates/game/src/region_runner.rs` (`CHECKPOINT_INTERVAL_TICKS`, `persisted_terrain_hash`, `Checkpoint` output, `Stopped` third field)
- Modify: `crates/game/src/world_manager.rs` (`Checkpoint`/`Stopped` handling, `PARKED_CACHE_MAX`, acks)
- Modify: `crates/server/src/region_threads.rs` (perf event only if needed — see step 4)
- Test: `crates/game/tests/persistence.rs`

**Interfaces:**
- Consumes: `RegionCheckpoint`, `TerrainWrite`, `Rollback::owned_bundles`, `StoreEvent::Saved`.
- Produces: `pub const CHECKPOINT_INTERVAL_TICKS: usize`; `RegionOutput::Checkpoint(RegionCheckpoint)`; `RegionOutput::Stopped(SerializedRegion, TerrainSnapshot, RegionCheckpoint)`; `RegionRunner::set_terrain_persisted(&mut self, persisted: bool)`; `pub const PARKED_CACHE_MAX: usize`; `WorldManager::set_parked_cache_max(&mut self, n: usize)` (test hook).

- [ ] **Step 1: Failing tests** (append to `persistence.rs`)

```rust
#[test]
fn a_fresh_region_freezes_its_terrain_on_the_first_tick() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(1), 0);
    assert!(h.store.load_region(SPAWN_REGION).unwrap().is_none());
    tick(&mut h, 50);
    assert!(h.store.load_region(SPAWN_REGION).unwrap().is_some(), "not frozen on first run");
}

#[test]
fn periodic_checkpoints_carry_players_and_skip_unchanged_terrain() {
    let mut h = harness();
    let LoginOutcome::Accepted { client_id: c, .. } = h.store.login("di", &[4; 32]).unwrap() else { panic!() };
    h.event(ServerEvent::ClientConnected(c), 0);
    for i in 0..game::CHECKPOINT_INTERVAL_TICKS as u64 {
        tick(&mut h, 50 * (i + 1));
    }
    let LoginOutcome::Accepted { saved: Some(saved), .. } = h.store.login("di", &[4; 32]).unwrap() else { panic!("player not checkpointed") };
    assert_eq!(saved.region, SPAWN_REGION);
}

#[test]
fn parked_blobs_are_evicted_only_after_their_save_is_acknowledged() {
    let mut h = harness();
    h.manager.set_parked_cache_max(0);
    h.event(ServerEvent::ClientConnected(1), 0);
    h.event(ServerEvent::ClientDisconnected(1), 10);
    h.manager.maintain(10 + game::UNLOAD_GRACE_MS);
    h.settle(10 + game::UNLOAD_GRACE_MS);
    h.manager.pump_store_events();
    h.manager.maintain(20 + game::UNLOAD_GRACE_MS);
    assert!(h.manager.parked_regions().is_empty(), "acked blob not evicted at cap 0");
    assert!(h.store.load_region(SPAWN_REGION).unwrap().is_some(), "evicted without being stored");
}
```

Tag `periodic_checkpoints…` with `#[ignore = "slow: >100ms — run via just test-full"]` if it measures over 100 ms (600 ticks on an empty region usually do not; measure with `--nocapture` timing).

- [ ] **Step 2: Run** `just test-one game "--test persistence"` → FAIL (compile).

- [ ] **Step 3: Runner**

```rust
/// Ticks between region checkpoints: the most progress a crash can lose.
pub const CHECKPOINT_INTERVAL_TICKS: usize = 30_000 / TICK_RATE as usize;
```

Field `persisted_terrain_hash: Option<u32>` (constructor: `None`), plus

```rust
/// Spawners call this for regions restored from the store or a parked blob
/// (their terrain is already durable); fresh worldgen stays unpersisted
/// and freezes at the first tick.
pub fn set_terrain_persisted(&mut self, persisted: bool) {
    self.persisted_terrain_hash = persisted.then_some(self.last_terrain_hash);
}

fn checkpoint(&mut self) -> crate::RegionCheckpoint {
    let hash = self.region.terrain_fingerprint();
    let terrain = match self.persisted_terrain_hash {
        Some(h) if h == hash => None,
        previous => {
            self.persisted_terrain_hash = Some(hash);
            Some(crate::TerrainWrite {
                snapshot: self.region.terrain_snapshot(),
                is_edit: previous.is_some(),
            })
        }
    };
    crate::RegionCheckpoint {
        region: self.id,
        tick: self.region.current_tick() as u64,
        entities: self.region.data().owned_bundles(self.id),
        terrain,
    }
}
```

In `tick()`, after the existing work:

```rust
if self.persisted_terrain_hash.is_none()
    || self.region.current_tick() % CHECKPOINT_INTERVAL_TICKS == 0
{
    let watch = crate::perf::Stopwatch::start();
    let cp = self.checkpoint();
    crate::perf_event!(
        "region_checkpoint",
        "region={},{} ms={} entities={} terrain={}",
        self.id.x, self.id.z, crate::perf::ms(watch.elapsed()), cp.entities.len(), cp.terrain.is_some()
    );
    let _ = self.out.send((self.id, RegionOutput::Checkpoint(cp)));
}
```

In `Shutdown`, after applying held input: `let checkpoint = self.checkpoint();` and send `RegionOutput::Stopped(serialized, far, checkpoint)`. Update the `RegionOutput::Stopped` doc comment. `InlineSpawner::spawn` and `ThreadRegionSpawner::spawn`: call `runner.set_terrain_persisted(seed_was_persisted)` where `seed_was_persisted` is `matches!(seed, RegionSeed::Parked(..) | RegionSeed::Deferred(Some(_)))` computed before the seed is consumed (Task 5 extends it with `Stored`).

- [ ] **Step 4: Manager**

```rust
/// Parked blobs kept in memory in front of the store (each ~40 MiB).
pub const PARKED_CACHE_MAX: usize = 16;
```

Fields: `parked_cache_max: usize` (= `PARKED_CACHE_MAX`), `parked_seq: BTreeMap<RegionCoords, u64>` (seq of the Stopped checkpoint), `acked_seq: BTreeMap<RegionCoords, u64>`, `parked_order: BTreeMap<RegionCoords, u64>` (stamp from a counter; reuse `lod_access_counter`-style monotonic `u64`).

- `RegionOutput::Checkpoint(cp) => { let seq = self.next_seq(); self.store.save_checkpoint(seq, cp); }`
- `Stopped(serialized, far, cp)`: before the existing body, `let seq = self.next_seq(); self.store.save_checkpoint(seq, cp);`; after `self.parked.insert(rc, serialized)`: `self.parked_seq.insert(rc, seq); self.parked_order.insert(rc, <next stamp>); self.evict_parked();`
- Wherever `self.parked.remove(&rc)` happens (in `ensure_running`), also remove `parked_seq`/`parked_order` for `rc`.
- `handle_store_event(StoreEvent::Saved { region, seq })`: `let e = self.acked_seq.entry(region).or_default(); *e = (*e).max(seq); self.evict_parked();`
- `evict_parked`: while `parked.len() > parked_cache_max`, pick the smallest `parked_order` among regions with `acked_seq[rc] >= parked_seq[rc]`; none → break (unacked blobs are the only copy of recent state); remove from all three maps.
- `pub fn set_parked_cache_max(&mut self, n: usize) { self.parked_cache_max = n; self.evict_parked(); }`
- Call `self.pump_store_events()` at the end of `maintain` too, so both server and inline loops drain acks.

Update any test that pattern-matches `RegionOutput::Stopped(a, b)` to `Stopped(a, b, _)` (grep `Stopped(` under `crates/*/tests` and `crates/client/src`).

- [ ] **Step 5: Run** `just test-one game "--test persistence"` → PASS; `just test-one game "--test world_manager"` → PASS; `just smoke`; `just test-threads` → PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/game/src/region_runner.rs crates/game/src/world_manager.rs crates/server/src/region_threads.rs \
  crates/game/tests/persistence.rs
git commit -m "feat(game): periodic region checkpoints, freeze-on-first-run, ack-pinned parked cache

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Restore regions from the store (inline and threaded), never regenerate over a row

**Files:**
- Modify: `crates/game/src/region_runner.rs` (`RegionSeed::Stored`, `into_region_with` gets a loader and returns `Result`)
- Modify: `crates/game/src/world_manager.rs` (`ensure_running` inline branch loads first; `InlineSpawner` passes the flag)
- Modify: `crates/server/src/region_threads.rs` (thread loads, retries with backoff)
- Test: `crates/game/tests/persistence.rs`, `crates/server/tests/threaded_world.rs` (one new test)

**Interfaces:**
- Consumes: `RegionStore::load_region`, `StoredRegion`, `apply_arrival`.
- Produces: `RegionSeed::Stored(StoredRegion)`; `RegionSeed::into_region_with(self, id, generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>, load: &dyn Fn(RegionCoords) -> Result<Option<StoredRegion>, StoreError>) -> Result<(Region, bool), StoreError>` (bool = terrain already persisted); `RegionSeed::from_stored(id, stored) -> Region` helper; `ThreadRegionSpawner::with_store(self, store: Arc<dyn RegionStore>) -> Self`; `pub const STORE_RETRY_MAX: Duration`.

- [ ] **Step 1: Failing tests** (append to `persistence.rs`; the harness store is shared by two managers to simulate a restart/crash)

```rust
#[test]
fn a_crash_restores_terrain_edits_and_bodies_from_the_last_checkpoint() {
    let (store, events) = game::MemoryStore::new();
    let edited_at = game::ChunkCoords::new(0, 0, 0);
    {
        let mut h = harness_with(store.clone(), events);
        let LoginOutcome::Accepted { client_id: c, .. } = store.login("ed", &[5; 32]).unwrap() else { panic!() };
        h.event(ServerEvent::ClientConnected(c), 0);
        tick(&mut h, 50); // freeze
        h.manager.spawner_mut().with_region(SPAWN_REGION, |r| r.with_data(|d| d.edit_block_for_test(edited_at, 1, 1, 1, game::BlockId(3))));
        for i in 0..game::CHECKPOINT_INTERVAL_TICKS as u64 { tick(&mut h, 100 + 50 * i); }
        // manager dropped here without shutdown = crash
    }
    // Restarted process: same store, fresh manager (acks go nowhere — fine).
    let mut h = harness_with(store.clone(), crossbeam::channel::never());
    let LoginOutcome::Accepted { client_id: c, saved: Some(saved) } = store.login("ed", &[5; 32]).unwrap() else { panic!() };
    h.event(ServerEvent::ClientResumed { id: c, saved }, 0);
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        assert!(r.data().player_entites.contains_key(&c));
        let snap = r.terrain_snapshot();
        let (_, chunk) = snap.chunks.iter().find(|(cc, _)| *cc == edited_at).expect("chunk");
        assert_eq!(chunk.block(1, 1, 1).id, game::BlockId(3), "terrain edit lost");
        assert_eq!(r.data().player_entites.len(), 1, "orphan or duplicate body");
    });
}

#[test]
fn a_stored_region_is_never_regenerated() {
    let (store, events) = game::MemoryStore::new();
    {
        let mut h = harness_with(store.clone(), events);
        h.event(ServerEvent::ClientConnected(1), 0);
        tick(&mut h, 50);
    }
    let mut h = harness_with_generator(
        store.clone(),
        crossbeam::channel::never(),
        Box::new(|_| panic!("regenerated a stored region")),
    );
    h.event(ServerEvent::ClientConnected(2), 0);
    tick(&mut h, 50);
}

#[test]
fn a_failing_store_never_falls_back_to_worldgen() {
    let seed = game::RegionSeed::Deferred(None);
    let result = seed.into_region_with(
        SPAWN_REGION,
        &|_| panic!("generated after a store error"),
        &|_| Err(game::StoreError("down".into())),
    );
    assert!(result.is_err());
}
```

Factor the Task 3 harness into `harness_with_generator(store: Arc<MemoryStore>, events: Receiver<StoreEvent>, generator: RegionGenerator) -> Harness`, `harness_with(store, events)` (default flat-floor generator) and `harness()` (fresh `MemoryStore::new()`). `MemoryStore` hands out one event receiver, so a second manager over the same store (simulated restart) gets `crossbeam::channel::never()`. `edit_block_for_test` does not exist: use the chunk-edit pattern already in `crates/game/tests/walking_smoothness.rs:~200-215` (it edits `chunk.set_block` through `with_data`); if that pattern needs a helper, add `#[doc(hidden)] pub fn edit_block_for_test(&mut self, chunk: ChunkCoords, bx, by, bz, id: BlockId)` on `Rollback` in `state.rs` built the same way, and the matching `Chunk::block(x, y, z)` reader if none exists (check `voxel.rs` first — use the existing getter).

- [ ] **Step 2: Run** → FAIL (compile).

- [ ] **Step 3: `RegionSeed`**

```rust
pub enum RegionSeed {
    Fresh(Vec<(ColumnCoords, Column)>),
    Parked(SerializedRegion, Vec<(ColumnCoords, Column)>),
    /// Rebuilt from the store: frozen terrain + non-player entities.
    Stored(StoredRegion),
    Deferred(Option<SerializedRegion>),
}

impl RegionSeed {
    /// Terrain of this seed is already durable (no freeze checkpoint needed).
    /// `Deferred(None)` resolves on the region thread (`into_region_with`).
    pub fn terrain_persisted(&self) -> bool {
        matches!(self, RegionSeed::Parked(..) | RegionSeed::Stored(_) | RegionSeed::Deferred(Some(_)))
    }

    pub fn from_stored(id: RegionId, stored: StoredRegion) -> Region {
        let mut region = Region::from_chunks(id, stored.chunks);
        region.with_data(|d| {
            d.tick.update(|t| *t = stored.tick as usize);
            for bundle in stored.entities {
                d.apply_arrival(bundle);
            }
        });
        region
    }
}
```

`into_region(self, id) -> Region` handles `Stored` via `from_stored`. `into_region_with` becomes:

```rust
pub fn into_region_with(
    self,
    id: RegionId,
    generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>,
    load: &dyn Fn(RegionCoords) -> Result<Option<StoredRegion>, StoreError>,
) -> Result<(Region, bool), StoreError> {
    match self {
        // The parked blob is a warm cache of state that is (being) stored;
        // a corrupt blob falls back to the STORE, never to worldgen.
        RegionSeed::Deferred(Some(blob)) => match blob.to_rollback() {
            Ok(rollback) => Ok((Region::new(rollback, None, id, None), true)),
            Err(e) => {
                log::error!("parked region {id:?} blob corrupt ({e}); restoring from store");
                RegionSeed::Deferred(None).into_region_with(id, generate, load)
            }
        },
        RegionSeed::Deferred(None) => match load(id)? {
            Some(stored) => Ok((Self::from_stored(id, stored), true)),
            None => Ok((Region::from_columns(id, generate(id)), false)),
        },
        other => {
            let persisted = other.terrain_persisted();
            Ok((other.into_region(id), persisted))
        }
    }
}
```

Keep the existing `Parked(blob, fallback)` branch in `into_region` as is (inline path — the fallback there is the manager's generation, only reached when the store also had no row; see step 4).

- [ ] **Step 4: Inline path (`ensure_running`, non-`generates_columns` branch)**

Order: parked blob → store → generate.

```rust
let seed = match self.parked.remove(&rc) {
    Some(serialized) => {
        let columns = self.generate_timed(rc); // corrupt-blob fallback, as today
        RegionSeed::Parked(serialized, columns)
    }
    None => match self.store.load_region(rc) {
        Ok(Some(stored)) => RegionSeed::Stored(stored),
        Ok(None) => RegionSeed::Fresh(self.generate_timed(rc)),
        // Inline stores are in-memory: an error here is a bug, not an outage.
        Err(e) => panic!("store failed loading {rc:?}: {e}"),
    },
};
```

Keep the `region_generate` perf event around the generating arms only. `InlineSpawner::spawn`: `let persisted = seed.terrain_persisted(); let mut runner = RegionRunner::new(id, seed.into_region(id), out); runner.set_terrain_persisted(persisted);`.

Document on `load_region` in the trait that the inline branch calls it on the manager thread and is therefore only valid for non-blocking stores (MemoryStore); the server uses the threaded spawner (`generates_columns() == true`).

- [ ] **Step 5: Threaded path (`crates/server/src/region_threads.rs`)**

Add `store: Option<Arc<dyn game::RegionStore>>` to `ThreadRegionSpawner` and `pub fn with_store(mut self, store: Arc<dyn game::RegionStore>) -> Self`. In the thread closure replace the `seed.into_region_with(id, &|rc| …)` call with a retry loop:

```rust
/// Longest pause between attempts to load a region from an unreachable store.
pub const STORE_RETRY_MAX: Duration = Duration::from_secs(30);

let load = |rc| match &store {
    Some(store) => store.load_region(rc),
    None => Ok(None),
};
let mut backoff = Duration::from_millis(500);
let mut seed = Some(seed);
let (region, persisted) = loop {
    match seed.take().unwrap().into_region_with(id, &generate, &load) {
        Ok(built) => break built,
        Err(e) => {
            // Never generate over unknown state: wait for the store. Inputs
            // queue in the channel meanwhile; subscribers see a stalled region.
            log::error!("region {id:?} cannot start: {e}; retrying in {backoff:?}");
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(STORE_RETRY_MAX);
            seed = Some(RegionSeed::Deferred(None));
        }
    }
};
let mut runner = RegionRunner::new(id, region, out);
runner.set_terrain_persisted(persisted);
```

(`generate` is the existing timing closure, bound to a variable first.) Extend the `region_genesis` perf line with `stored={persisted}`.

Add to `crates/server/tests/threaded_world.rs` one test `cold_region_restores_from_store_on_its_thread` (tagged slow like its neighbours): build the rig's `ThreadRegionSpawner::with_generator(…).with_store(store.clone())` over a `MemoryStore` pre-seeded by `save_checkpoint(1, RegionCheckpoint { region: SPAWN_REGION, tick: 42, entities: vec![], terrain: Some(TerrainWrite{ snapshot: <one-chunk snapshot>, is_edit: true }) })`, connect a client, wait for the first `SyncClock` of `SPAWN_REGION`, and assert its tick is ≥ 42 (restored tick) — follow the file's existing rig helpers for waiting on outputs.

- [ ] **Step 6: Run** `just test-one game "--test persistence"` → PASS; `just test-threads` → PASS; `just smoke` → PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/game/src/region_runner.rs crates/game/src/world_manager.rs crates/game/src/state.rs \
  crates/server/src/region_threads.rs crates/game/tests/persistence.rs crates/server/tests/threaded_world.rs
git commit -m "feat: restore regions from the store; a store error never regenerates a region

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Row codecs, schema and `PgStore`

**Files:**
- Modify: `crates/server/Cargo.toml` — add `sqlx = { version = "0.9", default-features = false, features = ["runtime-tokio", "postgres", "migrate", "macros"] }`, `zstd = "0.13"`, `sha2 = "0.10"`; `tokio` needs `rt` (check the workspace entry's features; add `"rt"` locally if missing)
- Create: `crates/server/migrations/0001_init.sql`, `crates/server/src/persist_codec.rs`, `crates/server/src/persist_pg.rs`
- Modify: `crates/server/src/lib.rs` (`pub mod persist_codec; pub mod persist_pg;`)
- Modify: `crates/worldgen/src/cache.rs` (+ re-export in `lib.rs`): `world_identity`, `worldgen_version`
- Create: `crates/server/tests/persist_codec.rs`, `crates/server/tests/fixtures/terrain_v1.bin`, `entity_v1.bin`, `crates/server/tests/persist_pg.rs`
- Modify: `justfile` (`pg-up`, `pg-down`, `test-pg`)

**Interfaces:**
- Consumes: the `RegionStore` contract (Task 2), `EntityBundle`, `TerrainSnapshot`.
- Produces: `server::persist_codec::{encode_terrain, decode_terrain, encode_entity, decode_entity, FORMAT_V1}`; `server::persist_pg::PgStore::connect(url: &str, world_identity: &str, worldgen_version: &str) -> Result<(Arc<PgStore>, Receiver<StoreEvent>), StoreError>`; `worldgen::world_identity(seed: u64) -> String`, `worldgen::worldgen_version(seed: u64, block_manifest: &str) -> String`.

- [ ] **Step 1: Codec + golden tests (failing)**

`crates/server/tests/persist_codec.rs`:

```rust
//! Stored rows are the sim types behind a format byte. A layout change in
//! Chunk / EntityBundle / Client / Inventory / Motion makes these goldens
//! fail to decode: bump the format, keep a V1 decoder, write a migration.
//! Regenerate ONLY for a deliberate format bump: UPDATE_GOLDEN=1.
use server::persist_codec::*;

fn sample_terrain() -> game::TerrainSnapshot {
    let mut chunk = game::Chunk::flat_floor(4);
    chunk.set_block(1, 2, 3, game::Block::new(game::BlockId(3)));
    game::TerrainSnapshot { chunks: vec![(game::ChunkCoords::new(1, 2, -3), chunk)] }
}

fn sample_entity() -> game::EntityBundle {
    let mut region = game::Region::from_chunks(game::RegionCoords::new(0, 0), Vec::new());
    region.handle_event(game::GameEventKind::CreateClient(7)).unwrap();
    region.forget_last_event();
    let key = region.data().player_entites[&7];
    region.data().bundle_of(key, game::RegionCoords::new(0, 0))
}

fn golden(name: &str, bytes: Vec<u8>) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
    }
    std::fs::read(&path).expect("golden missing: run once with UPDATE_GOLDEN=1")
}

#[test]
fn terrain_v1_golden_decodes() {
    let bytes = golden("terrain_v1.bin", encode_terrain(&sample_terrain()));
    assert_eq!(bytes[0], FORMAT_V1);
    assert_eq!(decode_terrain(&bytes).unwrap(), sample_terrain());
}

#[test]
fn entity_v1_golden_decodes() {
    let bytes = golden("entity_v1.bin", encode_entity(&sample_entity()));
    assert_eq!(decode_entity(&bytes).unwrap(), sample_entity());
}

#[test]
fn unknown_format_byte_is_an_error_not_a_panic() {
    assert!(decode_terrain(&[99, 0, 0]).is_err());
}
```

(`TerrainSnapshot` needs `PartialEq` for the assert — derive it if missing; `EntityBundle` already has it.)

- [ ] **Step 2: Run** `just test-one server "--test persist_codec"` → FAIL (module missing).

- [ ] **Step 3: `persist_codec.rs`**

```rust
//! Row encoding: [format byte] + zstd(bincode(value)). Rows are written
//! rarely and read on region start / login, so zstd's ratio (0.35 MiB vs
//! lz4's 1.15 MiB on a whole region blob) beats lz4's speed here; the wire
//! keeps lz4 (`game::encode_packet`).
pub const FORMAT_V1: u8 = 1;
const ZSTD_LEVEL: i32 = 3;

fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let raw = bincode::serialize(value).expect("stored types serialize");
    let mut out = vec![FORMAT_V1];
    out.extend(zstd::encode_all(&raw[..], ZSTD_LEVEL).expect("zstd encode"));
    out
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    match bytes.split_first() {
        Some((&FORMAT_V1, body)) => {
            let raw = zstd::decode_all(body).map_err(|e| format!("zstd: {e}"))?;
            bincode::deserialize(&raw).map_err(|e| format!("bincode: {e}"))
        }
        Some((other, _)) => Err(format!("unknown row format {other}")),
        None => Err("empty row".into()),
    }
}

pub fn encode_terrain(t: &game::TerrainSnapshot) -> Vec<u8> { encode(t) }
pub fn decode_terrain(b: &[u8]) -> Result<game::TerrainSnapshot, String> { decode(b) }
pub fn encode_entity(e: &game::EntityBundle) -> Vec<u8> { encode(e) }
pub fn decode_entity(b: &[u8]) -> Result<game::EntityBundle, String> { decode(b) }
```

Add `serde` / `bincode` to server deps if not already direct dependencies (check `crates/server/Cargo.toml`; use the workspace versions).

- [ ] **Step 4: Generate goldens, then run** `UPDATE_GOLDEN=1 just test-one server "--test persist_codec"` once, then `just test-one server "--test persist_codec"` → PASS. Inspect `ls -l crates/server/tests/fixtures` (both files a few hundred bytes).

- [ ] **Step 5: Schema** — `crates/server/migrations/0001_init.sql`:

```sql
CREATE TABLE world (
  id               boolean PRIMARY KEY DEFAULT true CHECK (id),   -- exactly one row
  world_identity   text NOT NULL,          -- "seed:<hex>" | "forged:<crc>"
  worldgen_version text NOT NULL,          -- version of the running build (informational)
  format           smallint NOT NULL);

CREATE TABLE accounts (
  client_id   integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,  -- = in-sim ClientId
  name        text NOT NULL UNIQUE,
  secret_hash bytea NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now());

CREATE TABLE regions (
  rx int NOT NULL, rz int NOT NULL, PRIMARY KEY (rx, rz),
  tick             bigint NOT NULL,
  worldgen_version text NOT NULL,          -- version that generated the frozen terrain
  edited           boolean NOT NULL DEFAULT false,
  terrain_version  bigint NOT NULL,
  terrain          bytea NOT NULL,
  saved_at         timestamptz NOT NULL DEFAULT now());
ALTER TABLE regions ALTER COLUMN terrain SET STORAGE EXTERNAL;   -- already zstd

CREATE TABLE entities (
  entity_id  bigint PRIMARY KEY,           -- EntityId (u64 bit-cast)
  rx int NOT NULL, rz int NOT NULL,
  in_world   boolean NOT NULL,
  seq        bigint NOT NULL,
  client_id  integer UNIQUE REFERENCES accounts,
  bundle     bytea NOT NULL);
CREATE INDEX entities_by_region ON entities (rx, rz) WHERE in_world;
```

- [ ] **Step 6: worldgen identity helpers** — in `crates/worldgen/src/cache.rs`:

```rust
/// Which world a store belongs to. A forged world file replaces the seed.
pub fn world_identity(seed: u64) -> String {
    match crate::forged::active() {
        Some(world) => format!("forged:{:08x}", world.content_crc()),
        None => format!("seed:{seed:016x}"),
    }
}

/// Version of the worldgen that produced fresh terrain in this build (the
/// disk-cache key). "unversioned" when the source tree is unavailable (a
/// deployed binary) — stage 3 moves this into a build-time constant.
pub fn worldgen_version(seed: u64, block_manifest: &str) -> String {
    cache_key(seed, block_manifest).map_or_else(|| "unversioned".into(), |k| format!("{k:08x}"))
}
```

Re-export both from `crates/worldgen/src/lib.rs`.

- [ ] **Step 7: `PgStore`** — `crates/server/src/persist_pg.rs`. One OS thread named `persist` owns a current-thread tokio runtime and a `PgPool` (`max_connections(4)`, `acquire_timeout(5 s)`); callers talk to it over a crossbeam channel.

```rust
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{bounded, unbounded, Receiver, RecvTimeoutError, Sender};
use game::{
    ClientId, EntityBundle, EntityId, LoginOutcome, RegionCheckpoint, RegionCoords, RegionStore,
    SavedBody, StoreError, StoreEvent, StoredRegion,
};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};

use crate::persist_codec::{decode_entity, decode_terrain, encode_entity, encode_terrain, FORMAT_V1};

/// Longest pause between retries of a failed save batch.
pub const SAVE_RETRY_MAX: Duration = Duration::from_secs(30);

enum Save {
    Checkpoint { seq: u64, cp: RegionCheckpoint },
    Logout { seq: u64, region: RegionCoords, bundle: EntityBundle },
}

enum Request {
    Load(RegionCoords, Sender<Result<Option<StoredRegion>, StoreError>>),
    Login(String, [u8; 32], Sender<Result<LoginOutcome, StoreError>>),
    MaxSeq(Sender<Result<u64, StoreError>>),
    Save(Save),
    Flush(Sender<()>),
}

pub struct PgStore {
    requests: Sender<Request>,
}

fn err(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

impl PgStore {
    /// Connect, migrate, and check/record the world row. Blocking; fatal
    /// errors are returned (the server refuses to start on a wrong world).
    pub fn connect(
        url: &str,
        world_identity: &str,
        worldgen_version: &str,
    ) -> Result<(Arc<Self>, Receiver<StoreEvent>), StoreError> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(err)?;
        let pool = rt.block_on(async {
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .acquire_timeout(Duration::from_secs(5))
                .connect(url)
                .await
                .map_err(err)?;
            sqlx::migrate!("./migrations").run(&pool).await.map_err(err)?;
            let existing: Option<String> = sqlx::query_scalar("SELECT world_identity FROM world")
                .fetch_optional(&pool)
                .await
                .map_err(err)?;
            match existing {
                Some(id) if id != world_identity => {
                    return Err(StoreError(format!(
                        "database holds world {id}, this server runs {world_identity}"
                    )))
                }
                Some(_) => {
                    sqlx::query("UPDATE world SET worldgen_version = $1").bind(worldgen_version)
                        .execute(&pool).await.map_err(err)?;
                }
                None => {
                    sqlx::query("INSERT INTO world (world_identity, worldgen_version, format) VALUES ($1, $2, $3)")
                        .bind(world_identity).bind(worldgen_version).bind(FORMAT_V1 as i16)
                        .execute(&pool).await.map_err(err)?;
                }
            }
            Ok::<_, StoreError>(pool)
        })?;
        let (requests, rx) = unbounded();
        let (events, events_rx) = unbounded();
        let worldgen_version = worldgen_version.to_string();
        std::thread::Builder::new()
            .name("persist".into())
            .spawn(move || persist_loop(rt, pool, rx, events, worldgen_version))
            .map_err(err)?;
        Ok((Arc::new(Self { requests }), events_rx))
    }

    fn ask<T>(&self, make: impl FnOnce(Sender<Result<T, StoreError>>) -> Request) -> Result<T, StoreError> {
        let (tx, rx) = bounded(1);
        self.requests.send(make(tx)).map_err(|_| StoreError("persistence thread gone".into()))?;
        rx.recv().map_err(|_| StoreError("persistence thread gone".into()))?
    }
}

impl RegionStore for PgStore {
    fn load_region(&self, rc: RegionCoords) -> Result<Option<StoredRegion>, StoreError> {
        self.ask(|tx| Request::Load(rc, tx))
    }
    fn login(&self, name: &str, secret: &[u8; 32]) -> Result<LoginOutcome, StoreError> {
        self.ask(|tx| Request::Login(name.to_string(), *secret, tx))
    }
    fn max_seq(&self) -> Result<u64, StoreError> {
        self.ask(Request::MaxSeq)
    }
    fn save_checkpoint(&self, seq: u64, cp: RegionCheckpoint) {
        let _ = self.requests.send(Request::Save(Save::Checkpoint { seq, cp }));
    }
    fn save_logout(&self, seq: u64, region: RegionCoords, bundle: EntityBundle) {
        let _ = self.requests.send(Request::Save(Save::Logout { seq, region, bundle }));
    }
    fn flush(&self, timeout: Duration) -> bool {
        let (tx, rx) = bounded(1);
        self.requests.send(Request::Flush(tx)).is_ok() && rx.recv_timeout(timeout).is_ok()
    }
}
```

`persist_loop` semantics (write it in full in the file):
- Keep `pending_cps: BTreeMap<RegionCoords, (u64, RegionCheckpoint)>` and `pending_logouts: Vec<(u64, RegionCoords, EntityBundle)>`, plus `flush_waiters: Vec<Sender<()>>`.
- **Coalescing:** a newer checkpoint for a region replaces a queued older one **but carries over its terrain** if the newer one has none (`if new.terrain.is_none() { new.terrain = old.terrain }` — otherwise a queued freeze/edit would be lost), and `is_edit` becomes `old || new`.
- Loop: wait on `rx.recv_timeout(if pending empty { 1 s } else { 0 })`; handle `Load`/`Login`/`MaxSeq` immediately (reply with `Err` on DB errors — do not retry reads); `Save` into the pending sets; `Flush` into waiters; `Disconnected` → write what is pending once, then exit. When pending is non-empty, write everything in one pass: each checkpoint in **its own transaction** (region upsert, then entity upserts), each logout as a single upsert; on success send `StoreEvent::Saved { region, seq }` per written item and remove it; on any error `log::error!`, keep the rest pending, sleep with doubling backoff capped at `SAVE_RETRY_MAX` (reset on success). When pending becomes empty, answer all flush waiters.
- SQL (all via `sqlx::query(..).bind(..)`, runtime-checked so builds never need a database):
  - terrain present: `INSERT INTO regions (rx, rz, tick, worldgen_version, edited, terrain_version, terrain) VALUES ($1,$2,$3,$4,$5,1,$6) ON CONFLICT (rx, rz) DO UPDATE SET tick = EXCLUDED.tick, terrain = EXCLUDED.terrain, edited = regions.edited OR EXCLUDED.edited, terrain_version = regions.terrain_version + 1, saved_at = now()` — `worldgen_version` is bound only on insert (a frozen region keeps the version that generated it); `$5` = `is_edit`.
  - terrain absent: `UPDATE regions SET tick = $3, saved_at = now() WHERE rx = $1 AND rz = $2`.
  - entity: `INSERT INTO entities (entity_id, rx, rz, in_world, seq, client_id, bundle) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (entity_id) DO UPDATE SET rx = EXCLUDED.rx, rz = EXCLUDED.rz, in_world = EXCLUDED.in_world, seq = EXCLUDED.seq, bundle = EXCLUDED.bundle WHERE entities.seq < EXCLUDED.seq`, with `entity_id = bundle.id.0 as i64`, `client_id = bundle.id.client().map(|c| c as i32)`, `seq as i64`.
  - load: `SELECT tick, terrain FROM regions WHERE rx=$1 AND rz=$2`, then `SELECT bundle FROM entities WHERE rx=$1 AND rz=$2 AND in_world AND client_id IS NULL ORDER BY entity_id`; decode failures → `Err(StoreError("region (x,z) row undecodable: …"))`.
  - login: `SELECT client_id, secret_hash FROM accounts WHERE name=$1`; missing → `INSERT INTO accounts (name, secret_hash) VALUES ($1,$2) RETURNING client_id`; hash = `Sha256::digest(secret)`; mismatch → `WrongSecret`; saved body = `SELECT rx, rz, bundle FROM entities WHERE client_id = $1`.
  - max_seq: `SELECT COALESCE(MAX(seq), 0) FROM entities`.

- [ ] **Step 8: Postgres integration tests** — `crates/server/tests/persist_pg.rs`, every test `#[ignore = "needs Postgres: just test-pg"]` and early-returning if `DATABASE_URL` is unset. Each test uses a unique world by first running `DROP SCHEMA public CASCADE; CREATE SCHEMA public;` through a separate `sqlx::PgPool` (tests run with `--test-threads=1`, see recipe). Cover, mirroring the MemoryStore tests in `crates/game/tests/persistence.rs` (copy their bodies, swapping the constructor): freeze on first checkpoint; seq guard (stale checkpoint loses); never delete by absence + players excluded from `load_region`; login TOFU + `WrongSecret`; logout then login returns the body; **coalescing keeps a queued terrain write** (enqueue a freeze checkpoint and an entity-only checkpoint for the same region back to back, `flush`, then `load_region` returns the terrain); **world identity mismatch refuses to connect**; flush returns true when drained.

- [ ] **Step 9: Justfile recipes** (after `test-threads`):

```make
# Dev Postgres for durable worlds (docker, named volume `mmo-pg`).
pg_url := "postgres://mmo:mmo@127.0.0.1:5432/mmo"
pg-up:
    docker start mmo-pg 2>/dev/null || docker run -d --name mmo-pg -e POSTGRES_USER=mmo -e POSTGRES_PASSWORD=mmo -e POSTGRES_DB=mmo -p 5432:5432 -v mmo-pg:/var/lib/postgresql/data postgres:17
    until docker exec mmo-pg pg_isready -U mmo >/dev/null 2>&1; do sleep 0.5; done

pg-down:
    docker stop mmo-pg

# PgStore integration tests against the dev database (WIPES its schema).
test-pg: pg-up
    DATABASE_URL={{pg_url}} {{cap}} {{cargo}} test -p server --test persist_pg -- --include-ignored --test-threads=1
```

- [ ] **Step 10: Run** `just test-one server "--test persist_codec"` → PASS; `just smoke` → PASS; `just test-pg` → PASS. **Disk check first:** on 2026-09-20 `/` had 1.7 GB free and docker refused to start containers. If `docker run` fails for space, stop, report it, and do not prune anything yourself.

- [ ] **Step 11: Commit**

```bash
git add crates/server/Cargo.toml Cargo.lock crates/server/migrations crates/server/src/persist_codec.rs \
  crates/server/src/persist_pg.rs crates/server/src/lib.rs crates/server/tests/persist_codec.rs \
  crates/server/tests/fixtures crates/server/tests/persist_pg.rs crates/worldgen/src/cache.rs \
  crates/worldgen/src/lib.rs justfile
git commit -m "feat(server): PgStore — sqlx persistence thread, zstd rows, golden formats

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

(`Cargo.lock` and `justfile` may carry unrelated user edits: check `git diff` for them and stage only your hunks.)

---

### Task 7: Server wiring — store selection, login handshake, account ids

**Files:**
- Modify: `crates/game/src/protocol.rs` (`ClientPacket::Login`, `ServerPacket::LoginResult`, `LoginRejected`)
- Create: `crates/server/src/login.rs`
- Modify: `crates/server/src/lib.rs` (`run()` store selection, `WorldIngress::listen` + `handle_connection`, remove `next_client_id`; shutdown flush)
- Modify: `crates/server/src/webtransport.rs` (same handshake; signature loses `next_client_id`, gains the store)
- Modify: `crates/server/tests/webtransport_handshake.rs` (login first)
- Test: unit tests in `login.rs`

**Interfaces:**
- Consumes: `RegionStore::login`, `LoginOutcome`, `ServerEvent::ClientResumed`, `PgStore::connect`, `worldgen::{world_identity, worldgen_version}`.
- Produces: `ClientPacket::Login { name: String, secret: [u8; 32] }`; `ServerPacket::LoginResult(Result<ClientId, LoginRejected>)`; `pub enum LoginRejected { WrongSecret, AlreadyOnline, StoreUnavailable, Malformed }`; `server::login::resolve(store: &dyn RegionStore, online: impl Fn(ClientId) -> bool, packet: ClientPacket) -> Result<(ClientId, Option<SavedBody>), LoginRejected>`; `pub const MAX_NAME_LEN: usize = 32`.

- [ ] **Step 1: Failing unit tests** in `crates/server/src/login.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use game::{ClientPacket, MemoryStore};

    fn login(name: &str, s: u8) -> ClientPacket {
        ClientPacket::Login { name: name.into(), secret: [s; 32] }
    }

    #[test]
    fn first_login_creates_the_account() {
        let (store, _) = MemoryStore::new();
        let (id, saved) = resolve(&*store, |_| false, login("ana", 1)).unwrap();
        assert!(id >= 1 && saved.is_none());
    }

    #[test]
    fn wrong_secret_and_double_login_are_rejected() {
        let (store, _) = MemoryStore::new();
        let (id, _) = resolve(&*store, |_| false, login("ana", 1)).unwrap();
        assert_eq!(resolve(&*store, |_| false, login("ana", 2)), Err(LoginRejected::WrongSecret));
        assert_eq!(resolve(&*store, |c| c == id, login("ana", 1)), Err(LoginRejected::AlreadyOnline));
    }

    #[test]
    fn anything_but_a_sane_login_is_malformed() {
        let (store, _) = MemoryStore::new();
        assert_eq!(resolve(&*store, |_| false, ClientPacket::RequestPlayerRegion), Err(LoginRejected::Malformed));
        assert_eq!(resolve(&*store, |_| false, login("", 1)), Err(LoginRejected::Malformed));
        assert_eq!(resolve(&*store, |_| false, login(&"x".repeat(33), 1)), Err(LoginRejected::Malformed));
    }
}
```

(`resolve` returns `Result<(ClientId, Option<SavedBody>), LoginRejected>`; `SavedBody` needs `PartialEq` for `assert_eq!` on the `Err` side only — compare with `matches!` instead if you prefer not to derive it.)

- [ ] **Step 2: Run** `just test-one server "--lib login::"` → FAIL.

- [ ] **Step 3: Protocol + resolver**

`protocol.rs`: add to `ClientPacket`

```rust
/// First packet on every connection. Dev-grade identity (trust on first
/// use); the same slot later carries a signed ticket.
Login { name: String, secret: [u8; 32] },
```

and to `ServerPacket` `LoginResult(Result<ClientId, LoginRejected>)`, plus

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoginRejected { WrongSecret, AlreadyOnline, StoreUnavailable, Malformed }
```

In `WorldManager::handle_server_event`'s `ClientPacket` match, add `ClientPacket::Login { .. } => log::debug!("late Login from client {id} ignored"),` (the edge consumes the real one). Fix any other non-exhaustive `ClientPacket`/`ServerPacket` matches the compiler reports (client `instance.rs` ignores `LoginResult`).

`login.rs`:

```rust
//! Transport-independent login: validates the first packet of a connection
//! and resolves it against the store. Blocking (store I/O) — call through
//! `tokio::task::spawn_blocking` on the async edge.
use game::{ClientId, ClientPacket, LoginOutcome, LoginRejected, RegionStore, SavedBody};

pub const MAX_NAME_LEN: usize = 32;

pub fn resolve(
    store: &dyn RegionStore,
    online: impl Fn(ClientId) -> bool,
    packet: ClientPacket,
) -> Result<(ClientId, Option<SavedBody>), LoginRejected> {
    let ClientPacket::Login { name, secret } = packet else { return Err(LoginRejected::Malformed) };
    if name.is_empty() || name.len() > MAX_NAME_LEN || name.chars().any(char::is_control) {
        return Err(LoginRejected::Malformed);
    }
    match store.login(&name, &secret) {
        Err(e) => {
            log::error!("[login] store unavailable: {e}");
            Err(LoginRejected::StoreUnavailable)
        }
        Ok(LoginOutcome::WrongSecret) => Err(LoginRejected::WrongSecret),
        Ok(LoginOutcome::Accepted { client_id, .. }) if online(client_id) => Err(LoginRejected::AlreadyOnline),
        Ok(LoginOutcome::Accepted { client_id, saved }) => Ok((client_id, saved)),
    }
}
```

- [ ] **Step 4: QUIC edge (`lib.rs`)**

`WorldIngress::listen` takes `store: Arc<dyn game::RegionStore>`. Remove `next_client_id`. Per accepted connection:

```rust
tokio::spawn(async move {
    let connection = match conn.await { Ok(c) => c, Err(e) => return log::warn!("handshake failed: {e:?}") };
    // Login is the first uni stream; nothing else is read before it resolves.
    let first = match connection.accept_uni().await {
        Ok(mut s) => s.read_to_end(4096).await.ok().and_then(|b| game::decode_packet::<ClientPacket>(&b).ok()),
        Err(_) => None,
    };
    let Some(first) = first else { return };
    let online = sinks.clone();
    let st = store.clone();
    let verdict = tokio::task::spawn_blocking(move || crate::login::resolve(&*st, |c| online.contains_key(&c), first))
        .await
        .unwrap_or(Err(game::LoginRejected::StoreUnavailable));
    let sink = ClientSink::Quinn(connection.clone());
    sink.send_packet(game::encode_packet(&ServerPacket::LoginResult(verdict.as_ref().map(|(id, _)| *id).map_err(|e| *e))).unwrap()).await;
    let (id, saved) = match verdict {
        Ok(v) => v,
        Err(reason) => {
            log::info!("[login] rejected: {reason:?}");
            connection.close(1u32.into(), b"login rejected");
            return;
        }
    };
    game::perf_event!("client_connected", "client={id} transport=quic resumed={}", saved.is_some());
    sinks.insert(id, sink);
    send.send(match saved {
        Some(saved) => ServerEvent::ClientResumed { id, saved },
        None => ServerEvent::ClientConnected(id),
    }).unwrap();
    // … existing handle_connection + disconnect tail, unchanged …
});
```

(There is a check-then-insert race between two simultaneous logins of one account; acceptable for dev login — note it in a comment.)

- [ ] **Step 5: WebTransport edge** — same sequence in `webtransport.rs` using its stream API (read the first `accept_uni` stream fully, `spawn_blocking(resolve)`, reply `LoginResult` via `ClientSink::Web(session.clone()).send_packet`, then register + announce). `serve` loses `next_client_id` and gains `store: Arc<dyn game::RegionStore>`. Update `crates/server/tests/webtransport_handshake.rs`: the test client sends `ClientPacket::Login { name: "wt".into(), secret: [9; 32] }` first, expects `LoginResult(Ok(_))`, and matches the announced `ServerEvent::ClientConnected(id)` with that id (it no longer starts at 0).

- [ ] **Step 6: `run()` wiring**

```rust
let world_identity = worldgen::world_identity(world_seed);
let worldgen_version = worldgen::worldgen_version(world_seed, &manifest);
let (store, store_events): (Arc<dyn game::RegionStore>, _) = match std::env::var("DATABASE_URL") {
    Ok(url) => {
        let (store, events) = persist_pg::PgStore::connect(&url, &world_identity, &worldgen_version)
            .unwrap_or_else(|e| panic!("DATABASE_URL set but the store is unusable: {e}"));
        info!("[server] durable world in Postgres ({world_identity}, worldgen {worldgen_version})");
        (store, events)
    }
    Err(_) => {
        log::warn!("[server] DATABASE_URL unset: the world is NOT durable (in-memory store)");
        let (store, events) = game::MemoryStore::new();
        (store, events)
    }
};
```

Build the store before spawning the network thread, pass `store.clone()` to `rgi.listen(…)`, `.with_store(store.clone())` on the `ThreadRegionSpawner`, and `.with_store(store.clone(), store_events)` on the manager. In the main loop call `manager.pump_store_events();` next to `pump_lod_results()`. After the orderly-exit park loop: `if !store.flush(Duration::from_secs(10)) { log::error!("[server] store did not drain before exit: last checkpoint may be lost"); }`.

- [ ] **Step 7: Run** `just test-one server "--lib login::"` → PASS; `just test-one server "--test webtransport_handshake -- --include-ignored"` → PASS; `just smoke` → PASS; `just test-threads` → PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/game/src/protocol.rs crates/game/src/world_manager.rs crates/server/src/login.rs \
  crates/server/src/lib.rs crates/server/src/webtransport.rs crates/server/tests/webtransport_handshake.rs
git commit -m "feat(server): login handshake, account-backed client ids, DATABASE_URL store selection

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: Client logs in (native + wasm)

**Files:**
- Create: `crates/client/src/account.rs`
- Modify: `crates/client/src/lib.rs` or `main.rs` (module declaration — follow where `netcode` is declared)
- Modify: `crates/client/src/netcode.rs` (send `Login`, await `LoginResult` before forwarding traffic)
- Modify: `crates/client/src/netcode_web.rs` (same)
- Modify: `crates/client/Cargo.toml` (`getrandom = "0.3"` under `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`; web-sys features `"Storage"`, `"Crypto"`, `"Url"`, `"UrlSearchParams"`)
- Test: unit tests in `account.rs` (native file path)

**Interfaces:**
- Consumes: `ClientPacket::Login`, `ServerPacket::LoginResult`, `LoginRejected`.
- Produces: `client::account::Credentials { pub name: String, pub secret: [u8; 32] }`, `account::load_or_create() -> Credentials`, native-only `account::load_or_create_at(dir: &Path, name: &str) -> std::io::Result<Credentials>`.

- [ ] **Step 1: Failing test** (native only)

```rust
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_created_once_and_reused_per_name() {
        let dir = std::env::temp_dir().join(format!("mmo-account-{}", std::process::id()));
        let a = load_or_create_at(&dir, "ana").unwrap();
        let again = load_or_create_at(&dir, "ana").unwrap();
        let b = load_or_create_at(&dir, "bo").unwrap();
        assert_eq!(a.secret, again.secret);
        assert_ne!(a.secret, b.secret);
        let _ = std::fs::remove_dir_all(dir);
    }
}
```

- [ ] **Step 2: Implement `account.rs`**

```rust
//! Dev-grade player identity: a name plus a 32-byte secret generated once
//! and kept locally (trust on first use server-side). Native: one file per
//! name under $XDG_CONFIG_HOME/voxel-mmo (or ~/.config/voxel-mmo); the name
//! comes from VOXEL_PLAYER, else $USER, else "player" — so two clients on
//! one machine are two accounts with VOXEL_PLAYER=a / b. Wasm: localStorage
//! key `voxel-account-<name>`, name from the page's `?player=` parameter.

pub struct Credentials {
    pub name: String,
    pub secret: [u8; 32],
}

fn to_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 { return None; }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_or_create_at(dir: &std::path::Path, name: &str) -> std::io::Result<Credentials> {
    let path = dir.join(format!("account-{name}"));
    if let Some(secret) = std::fs::read_to_string(&path).ok().and_then(|s| from_hex(&s)) {
        return Ok(Credentials { name: name.into(), secret });
    }
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, to_hex(&secret))?;
    Ok(Credentials { name: name.into(), secret })
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_or_create() -> Credentials {
    let name = std::env::var("VOXEL_PLAYER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "player".into());
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("voxel-mmo");
    load_or_create_at(&dir, &name).expect("cannot store player credentials")
}

#[cfg(target_arch = "wasm32")]
pub fn load_or_create() -> Credentials {
    let window = web_sys::window().expect("no window");
    let name = window
        .location()
        .search()
        .ok()
        .and_then(|q| web_sys::UrlSearchParams::new_with_str(&q).ok())
        .and_then(|p| p.get("player"))
        .unwrap_or_else(|| "player".into());
    let key = format!("voxel-account-{name}");
    let storage = window.local_storage().ok().flatten();
    if let Some(secret) = storage.as_ref().and_then(|s| s.get_item(&key).ok().flatten()).and_then(|v| from_hex(&v)) {
        return Credentials { name, secret };
    }
    let mut secret = [0u8; 32];
    window
        .crypto()
        .expect("no WebCrypto")
        .get_random_values_with_u8_array(&mut secret)
        .expect("random bytes");
    if let Some(s) = storage {
        let _ = s.set_item(&key, &to_hex(&secret));
    }
    Credentials { name, secret }
}
```

- [ ] **Step 3: Native netcode** — in `connect_and_handle`, right after `connect_with_retry` succeeds and before the existing read/write tasks start:

```rust
let creds = crate::account::load_or_create();
let mut login = connection.open_uni().await.map_err(std::io::Error::other)?;
login
    .write_all(&game::encode_packet(&ClientPacket::Login { name: creds.name.clone(), secret: creds.secret }).unwrap())
    .await
    .map_err(std::io::Error::other)?;
let _ = login.finish();
let mut reply = connection.accept_uni().await.map_err(std::io::Error::other)?;
let bytes = reply.read_to_end(1024).await.map_err(std::io::Error::other)?;
match game::decode_packet::<ServerPacket>(&bytes) {
    Ok(ServerPacket::LoginResult(Ok(id))) => {
        info!("[client] logged in as {} (client {id})", creds.name);
        game::perf_event!("logged_in", "client={id}");
    }
    Ok(ServerPacket::LoginResult(Err(reason))) => {
        error!("[client] login as {} rejected: {reason:?}", creds.name);
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("login rejected: {reason:?}")));
    }
    other => return Err(std::io::Error::other(format!("expected LoginResult, got {other:?}"))),
}
```

Instance traffic queued in `self.recv` meanwhile is sent afterwards by the existing write loop, so the server sees `Login` strictly first.

- [ ] **Step 4: Wasm netcode** — in `netcode_web.rs::run`, after the transport opens and before spawning the read/write loops: open one outgoing uni stream, write the encoded `Login` from `account::load_or_create()`, close it; read the first incoming stream fully (reuse the helper the read loop uses to drain one stream), decode, and on `LoginResult(Err(r))` return `Err(JsValue::from_str(&format!("login rejected: {r:?}")))` (surfaces through `connection_lost`). The `?offline` `LocalServer` path is untouched.

- [ ] **Step 5: Run** `just test-one client "--lib account::"` — this links a Bevy-sized test binary (slow): per CLAUDE.md, ask the user before running it; if not approved, rely on `just smoke` (type-checks client lib + bin + tests) and the live check in Task 9. Also type-check wasm: `{{cap}}`-pinned `cargo check -p client --target wasm32-unknown-unknown` via `just` if a recipe exists (`just --list | grep -i wasm`), else `nice -n 10 taskset -c 0-15 cargo check -p client --target wasm32-unknown-unknown`.

- [ ] **Step 6: Commit**

```bash
git add crates/client/src/account.rs crates/client/src/netcode.rs crates/client/src/netcode_web.rs \
  crates/client/Cargo.toml Cargo.lock crates/client/src/main.rs
git commit -m "feat(client): log in with a locally kept dev identity before any traffic

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

(Stage only the declaration file you actually touched; check `git diff` on shared files for user hunks.)

---

### Task 9: Durable run recipe, docs, live verification

**Files:**
- Modify: `justfile` (`run-durable`)
- Modify: `CLAUDE.md` (persistence paragraph under "Building and Running"; `crates/server` + `crates/game` bullets mention `persist.rs` / `PgStore`; the "server listens" paragraph mentions login)
- Modify: `TODO.md` (tick off "Durable region persistence" and "Stable client identity across connections"; add stage-0 follow-ups: newest-wins login, real auth, per-column terrain rows, build-time `worldgen_version`)
- Modify: `docs/superpowers/specs/2026-09-20-durable-world-persistence-design.md` status line → "implemented (plan 2026-09-22)", with the deviations list from this plan's header

- [ ] **Step 1: Recipe**

```make
# Server + client with the world persisted in the dev Postgres (`pg-up`).
# Plain `run` keeps the in-memory store (nothing survives a restart).
run-durable: pg-up
    DATABASE_URL={{pg_url}} just adapter={{adapter}} run
```

(Check how `run` consumes `adapter`/`world` variables and forward them the same way; if `run` reads env vars rather than just-variables, export instead.)

- [ ] **Step 2: Docs** — write the CLAUDE.md paragraph (≤ 8 lines): `DATABASE_URL` unset → in-memory store and a warning; `just pg-up` / `run-durable` / `test-pg`; freeze-on-first-run; logout removes the body; `VOXEL_PLAYER` names the dev account (native), `?player=` on wasm; a store error stops a region instead of regenerating it; tests use `MemoryStore`.

- [ ] **Step 3: Live verification (one client, per the project's testing preference)**
  1. `just pg-up`, then `just world=tiny run-durable` (pin `adapter=` per CLAUDE.md). Walk ~50 blocks, note the position, close the client. Server log shows `region_checkpoint` perf lines and no errors.
  2. Restart the client only: it spawns where it left (logout → login).
  3. Stop the server with Ctrl-C mid-walk *after at least one 30 s checkpoint*, restart `run-durable`: the player resumes at the last checkpointed position; `docker exec mmo-pg psql -U mmo -c "select rx, rz, edited, terrain_version, length(terrain) from regions"` shows the frozen region(s); `select entity_id, rx, rz, in_world, seq from entities` shows the player row.
  4. `just smoke` and `just test-threads` → PASS.
  Record the observed numbers (checkpoint ms, terrain row bytes, restore ms from `region_genesis stored=true`) in the spec's status section.

- [ ] **Step 4: Commit**

```bash
git add justfile CLAUDE.md TODO.md docs/superpowers/specs/2026-09-20-durable-world-persistence-design.md
git commit -m "docs: durable world persistence — recipes, CLAUDE.md, TODO

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>"
```

(These files carry unrelated user edits; stage your hunks only.)

---

## Spec coverage check

| Spec section | Task |
|---|---|
| Findings 1 (no edit event) / 4 (from_chunks + apply_arrival) | 4 (fingerprint-driven terrain), 5 (restore) |
| Finding 2 (no remove-client) | 3 |
| Finding 3 (key reuse merges players) | 1 |
| Finding 5 (read-only bundle) | 1 (`bundle_of`) |
| Finding 6 (wasm32 usize) | 6 (`integer` identity), 7 |
| §1 Identity, dev login | 1, 7, 8 (deviation: reject double login) |
| §2 Login/logout as arrival/extraction, crossing race | 3 |
| §3 Schema, freeze, versioned formats | 6 (deviation: goldens instead of mirror structs; `world_identity`) |
| §4 Checkpoints, seq, never delete by absence, perf events | 2, 4 |
| §5 Restore, never regenerate on error, warm cache + LRU + ack pin | 4, 5 |
| §6 Code shape, sqlx, persistence thread, coalescing, DATABASE_URL, justfile | 2, 6, 7, 9 |
| §7 Failure handling table | 5 (store down at start), 6 (retry/coalesce), 7 (login store down → `StoreUnavailable`), 3 (quick reconnect) |
| §8 Tests 1–10 | 1 (4), 3 (2, 8), 4, 5 (1, 3, 5, 6), 6 (7, 9), 8 (10) |
