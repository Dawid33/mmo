# Multi-Region World Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Regions become self-contained sim instances on their own server threads behind channels, managed (spawned, subscribed, parked) by a `WorldManager`, with the client holding a 3×3 window of regions that cycles as the viewer roams.

**Architecture:** Hub-and-spoke: netcode thread ↔ manager thread ↔ N region threads, all crossbeam channels. The manager core (`WorldManager`) is threadless and lives in `game`, shared by the real server (thread spawner) and the wasm `LocalServer` (inline spawner). Sims stay region-local; the world offset exists only in the client render bridge (region root `Transform`).

**Tech Stack:** Rust workspace; crossbeam channels; bincode; crc32fast; Bevy 0.18 (client only); quinn/tokio (server networking edge only).

**Spec:** `docs/superpowers/specs/2026-07-05-multi-region-world-design.md` — read it before starting any task.

## Global Constraints

- `cargo build --workspace --bins` (stable) must pass at the end of every task.
- `game`, `server`, `worldgen` stay Bevy-free and windowing-free. tokio stays confined to the server/client networking edge.
- Vendored forks (`nalgebra`, `simba`, `parry`, `rapier`, `approx`, `ordered-float`, `slotmapd`, `block-mesh`) must not be touched; no simulation-math changes (determinism).
- Wire format is bincode; state hashing is crc32fast over `std::hash::Hash`; cross-thread comms are crossbeam channels — never shared memory between manager and regions.
- Region size is fixed: 8×8 chunks × 32 voxels = 256×256 world units in x,z. `REGION_SIZE = 256.0` exactly representable in f32.
- Tick rate pinned to `game::TICK_RATE = 50` (ms per tick) per region; no per-region adaptive rates.
- Unload grace period `UNLOAD_GRACE_MS = 5000`; spawn region is `RegionCoords::new(0, 0)`.
- Rollback bar: `hash(before) == hash(after undo)` and `hash(before park) == hash(after restore)`, bit-exact.
- Both transports keep working: quinn on `127.0.0.1:6466` AND WebTransport on `127.0.0.1:6467` (merged in d936f0f). `cargo test -p server --test webtransport_handshake` must keep passing.
- Editions stay as-is (game/server/client 2021, worldgen/macros 2024).
- NOTE (post-plan merge): the WebTransport netcode landed after this plan's line references were taken (d936f0f..190fdc0). Server logic now lives in `crates/server/src/lib.rs` (not main.rs); client has `netcode_web.rs` and a changed input gate. Locate code by SYMBOL, not by the quoted line numbers.
- Commit after every task with the trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: `RegionCoords` type and the `RegionId` switch

Regions get signed, unbounded 2D coordinates. `RegionId` stops being an alias of `ChunkCoords` (unsigned, chunk-within-region) and becomes an alias of the new `RegionCoords`. Mechanical fallout across all three crates; the compiler finds every site.

**Files:**
- Modify: `crates/game/src/protocol.rs` (add `RegionCoords`, change `RegionId` alias)
- Modify: `crates/game/src/lib.rs` (World::basic region id; `World::remove_region`)
- Modify: `crates/game/src/region.rs:37-42` (`Region::new` id param type)
- Modify: `crates/server/src/lib.rs` (region id literals in `run()`: `ChunkCoords::new(0, 0, 0)` at the CreateClient site and the `current_tick` SyncClock site)
- Modify: `crates/server/tests/webtransport_handshake.rs` (region id literal in its scripted router)
- Modify: `crates/client/src/main.rs` (`player_chunk` type, `PlayerRegion` default, test literals)
- Modify: `crates/client/src/local_server.rs:32` (region id literal)
- Modify: `crates/client/src/renderer/bridge.rs` tests (region id literals)
- Test: `crates/game/tests/region_coords.rs` (new)

**Interfaces:**
- Produces: `game::RegionCoords { pub x: i32, pub z: i32 }` with `new(x, z)`, `world_offset() -> [f32; 3]`, `from_world(x: f32, z: f32) -> RegionCoords`, `window_3x3() -> Vec<RegionCoords>`; `game::REGION_CHUNKS: usize = 8`; `game::REGION_SIZE: f32 = 256.0`; `game::RegionId = RegionCoords`; `World::remove_region(&mut self, id: &RegionId) -> Option<Region>`; `World::reconcile_event` now tolerates unknown regions (returns `Ok(())`).
- Consumes: nothing new.

- [ ] **Step 1: Write the failing test**

Create `crates/game/tests/region_coords.rs`:

```rust
use game::{RegionCoords, REGION_SIZE};

#[test]
fn world_offset_is_exact_multiples_of_region_size() {
    assert_eq!(REGION_SIZE, 256.0);
    assert_eq!(RegionCoords::new(0, 0).world_offset(), [0.0, 0.0, 0.0]);
    assert_eq!(RegionCoords::new(1, -2).world_offset(), [256.0, 0.0, -512.0]);
}

#[test]
fn from_world_floor_divides_including_negatives() {
    assert_eq!(RegionCoords::from_world(0.0, 0.0), RegionCoords::new(0, 0));
    assert_eq!(RegionCoords::from_world(255.9, 255.9), RegionCoords::new(0, 0));
    assert_eq!(RegionCoords::from_world(256.0, 0.0), RegionCoords::new(1, 0));
    // Negative side must floor, not truncate toward zero.
    assert_eq!(RegionCoords::from_world(-0.1, -256.1), RegionCoords::new(-1, -2));
}

#[test]
fn window_3x3_is_the_nine_neighbours() {
    let w = RegionCoords::new(2, -1).window_3x3();
    assert_eq!(w.len(), 9);
    for dx in -1..=1 {
        for dz in -1..=1 {
            assert!(w.contains(&RegionCoords::new(2 + dx, -1 + dz)));
        }
    }
}

#[test]
fn reconcile_event_tolerates_unknown_region() {
    let mut world = game::World::basic();
    let ev = game::GameEvent::new(game::GameEventKind::Tick, 0, RegionCoords::new(99, 99));
    // Must not panic, must not error: unsubscribe races make this steady-state noise.
    assert!(world.reconcile_event(ev).is_ok());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test region_coords`
Expected: FAIL to compile — `RegionCoords` not found.

- [ ] **Step 3: Add `RegionCoords` to `crates/game/src/protocol.rs`**

Replace `pub type RegionId = ChunkCoords;` (protocol.rs:23) with:

```rust
/// Regions tile the horizontal plane in fixed 256-unit squares (8×8 chunks
/// of 32 voxels). Signed and unbounded: the world grows in every direction.
/// Sims stay region-local; the world offset exists only at the render
/// boundary (region root Transform) and, later, in handoff rebasing.
pub const REGION_CHUNKS: usize = 8;
pub const REGION_SIZE: f32 = (REGION_CHUNKS * 32) as f32; // 256.0, exact in f32

#[derive(
    Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct RegionCoords {
    pub x: i32,
    pub z: i32,
}

impl RegionCoords {
    pub fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// World-space origin of this region: `(x*256, 0, z*256)`. Exactly
    /// representable in f32, so render offsets are lossless.
    pub fn world_offset(&self) -> [f32; 3] {
        [self.x as f32 * REGION_SIZE, 0.0, self.z as f32 * REGION_SIZE]
    }

    /// Which region owns a world-space point (floor division, so the
    /// negative side maps correctly: -0.1 is region -1, not 0).
    pub fn from_world(x: f32, z: f32) -> Self {
        Self {
            x: (x / REGION_SIZE).floor() as i32,
            z: (z / REGION_SIZE).floor() as i32,
        }
    }

    /// The 3×3 window of regions centered on `self` — the client's desired
    /// loaded set.
    pub fn window_3x3(&self) -> Vec<RegionCoords> {
        let mut out = Vec::with_capacity(9);
        for dx in -1..=1 {
            for dz in -1..=1 {
                out.push(RegionCoords::new(self.x + dx, self.z + dz));
            }
        }
        out
    }
}

pub type RegionId = RegionCoords;
```

Also update the import at protocol.rs:4 — `ChunkCoords` is no longer needed there once `RegionId` stops aliasing it; keep the import only if still referenced.

- [ ] **Step 4: Chase the compiler through the mechanical rename**

Run `cargo build --workspace --bins` repeatedly; every error is one of these patterns:

- `crates/game/src/region.rs:40` — `Region::new(..., id: ChunkCoords, ...)` → `id: RegionId`.
- `crates/game/src/lib.rs:55` — `World.regions: BTreeMap<ChunkCoords, Region>` → `BTreeMap<RegionId, Region>`.
- `crates/game/src/lib.rs:66` — `World::basic`: `let one = ChunkCoords::new(0, 0, 0);` → `let one = RegionCoords::new(0, 0);` (the inner `create_basic(ChunkCoords::new(x, 0, z))` loop is chunk-local and stays `ChunkCoords`).
- `crates/server/src/lib.rs` (`run()`) — `let region_id = ChunkCoords::new(0, 0, 0);` → `RegionCoords::new(0, 0)`; and `world.current_tick(&ChunkCoords::new(0, 0, 0))` → `world.current_tick(id)` (use the iterated region id; this also fixes the hardcoded-origin TODO noted in local_server.rs:86-88). Update the `use game::{...}` import list. Same treatment for the region-id literal in `crates/server/tests/webtransport_handshake.rs`.
- `crates/client/src/main.rs:59` — `player_chunk: Option<ChunkCoords>` → `Option<RegionCoords>`; `main.rs:250` — `id.unwrap_or(ChunkCoords::new(0, 0, 0))` → `id.unwrap_or(RegionCoords::new(0, 0))`; tests in `manager_tests` (`pump_loads_region_and_ticks` and `player_input_flows_while_not_ready_once_caught_up`) — `ChunkCoords::new(0, 0, 0)` → `RegionCoords::new(0, 0)`. Update imports (`use game::{RegionCoords, ...}`).
- `crates/client/src/local_server.rs:32` — `ChunkCoords::new(0, 0, 0)` → `RegionCoords::new(0, 0)`; update imports.
- `crates/client/src/renderer/bridge.rs` tests (lines 303, 451, 477) — `ChunkCoords::new(0, 0, 0)` as a region id → `RegionCoords::new(0, 0)`; update the test imports.

- [ ] **Step 5: Add `World::remove_region` and tolerant `reconcile_event` in `crates/game/src/lib.rs`**

In `impl World`, replace the body of `reconcile_event` (lib.rs:119-126) and add `remove_region`:

```rust
    pub fn reconcile_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        // Tolerate events for regions we don't hold: with a moving 3×3
        // window, an event racing a just-released region is steady-state
        // noise, not an error.
        let Some(region) = self.regions.get_mut(&event.region_id) else {
            log::debug!("dropping event for unloaded region {:?}", event.region_id);
            return Ok(());
        };
        region.reconcile(event)
    }

    pub fn remove_region(&mut self, id: &RegionId) -> Option<Region> {
        self.regions.remove(id)
    }
```

- [ ] **Step 6: Run the full test suite**

Run: `cargo test -p game && cargo test -p client && cargo build --workspace --bins`
Expected: all PASS (26 client tests + game rollback suites + the new `region_coords` tests).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(game): signed RegionCoords replaces ChunkCoords as RegionId

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `worldgen::generate_region` and `Region::from_chunks`

Worldgen becomes a real (minimal) crate: a pure, deterministic function from region coordinates to chunk contents. Floor height varies by region parity so boundaries are visible as steps. `game` must NOT depend on `worldgen` (worldgen depends on game for the `Chunk` types) — generation is injected into the manager later as a closure.

**Files:**
- Modify: `crates/worldgen/Cargo.toml` (depend on `game`)
- Modify: `crates/worldgen/src/lib.rs` (replace stub)
- Modify: `crates/game/src/region.rs` (add `Region::from_chunks`)
- Test: `crates/worldgen/src/lib.rs` (inline `#[cfg(test)]`), `crates/game/tests/region_from_chunks.rs` (new)

**Interfaces:**
- Consumes: `game::{Chunk, ChunkCoords, RegionCoords, REGION_CHUNKS}`, `game::Chunk::flat_floor(depth: u32)` (voxel.rs:31).
- Produces: `worldgen::generate_region(coords: RegionCoords) -> Vec<(ChunkCoords, Chunk)>` (pure, deterministic, 64 chunks, floor height 8 on even `x+z`, 12 on odd); `game::Region::from_chunks(id: RegionId, chunks: Vec<(ChunkCoords, Chunk)>) -> Region`.

- [ ] **Step 1: Write the failing worldgen tests**

Replace `crates/worldgen/src/lib.rs` entirely:

```rust
//! Deterministic world generation. `generate_region` is a pure function of
//! region coordinates: same coords → identical output on every machine,
//! which is what makes "cycle out = park, cycle in = restore-or-regenerate"
//! safe for the multi-region world.

use game::{Chunk, ChunkCoords, RegionCoords, REGION_CHUNKS};

/// Floor height for a region: 8 on even `x+z`, 12 on odd — a checkerboard,
/// so region boundaries are visible as steps while roaming.
pub fn floor_height(coords: RegionCoords) -> u32 {
    if (coords.x + coords.z).rem_euclid(2) == 0 {
        8
    } else {
        12
    }
}

/// The full 8×8 chunk grid for one region, region-local coordinates.
/// Pure and deterministic; no clocks, no RNG.
pub fn generate_region(coords: RegionCoords) -> Vec<(ChunkCoords, Chunk)> {
    let depth = floor_height(coords);
    let mut chunks = Vec::with_capacity(REGION_CHUNKS * REGION_CHUNKS);
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            chunks.push((ChunkCoords::new(x, 0, z), Chunk::flat_floor(depth)));
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};

    fn crc(chunks: &[(ChunkCoords, Chunk)]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        for (_, c) in chunks {
            c.hash(&mut h);
        }
        h.finalize()
    }

    #[test]
    fn generation_is_pure() {
        let a = generate_region(RegionCoords::new(-3, 7));
        let b = generate_region(RegionCoords::new(-3, 7));
        assert_eq!(a.len(), 64);
        assert_eq!(crc(&a), crc(&b));
    }

    #[test]
    fn parity_heights_checkerboard() {
        assert_eq!(floor_height(RegionCoords::new(0, 0)), 8);
        assert_eq!(floor_height(RegionCoords::new(1, 0)), 12);
        assert_eq!(floor_height(RegionCoords::new(-1, 0)), 12);
        assert_eq!(floor_height(RegionCoords::new(-1, -1)), 8);
    }

    #[test]
    fn neighbouring_regions_differ() {
        let even = generate_region(RegionCoords::new(0, 0));
        let odd = generate_region(RegionCoords::new(1, 0));
        assert_ne!(crc(&even), crc(&odd));
    }
}
```

Update `crates/worldgen/Cargo.toml` dependencies:

```toml
[dependencies]
game = { path = "../game" }

[dev-dependencies]
crc32fast = { workspace = true }
```

(If `crc32fast` is not in `[workspace.dependencies]` of the root `Cargo.toml`, check — game/Cargo.toml uses `crc32fast = { workspace = true }`, so it is.)

- [ ] **Step 2: Run to verify fail, then pass**

Run: `cargo test -p worldgen`
Expected: first FAIL (Chunk isn't `Hash`? — it is, voxel.rs:10 derives `Hash`; the failure should only be missing deps/old stub), then PASS after the lib.rs replacement. If `worldgen/src/main.rs` (stub bin) breaks, make it `fn main() {}`.

- [ ] **Step 3: Write the failing `Region::from_chunks` test**

Create `crates/game/tests/region_from_chunks.rs`:

```rust
use game::{Chunk, ChunkCoords, Region, RegionCoords};

#[test]
fn from_chunks_builds_a_region_with_all_chunk_entities() {
    let chunks: Vec<(ChunkCoords, Chunk)> = (0..8)
        .flat_map(|x| (0..8).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect();
    let region = Region::from_chunks(RegionCoords::new(0, 0), chunks);
    // One sim entity per chunk (World::basic parity: 8×8 grid).
    assert_eq!(region.data().ecs.entities.len(), 64);
}
```

Run: `cargo test -p game --test region_from_chunks` — Expected: FAIL, method not found.

- [ ] **Step 4: Implement `Region::from_chunks` in `crates/game/src/region.rs`**

Add next to `create_basic` (region.rs:224):

```rust
    /// Build a server-side region from generated chunk contents. Mirrors
    /// what `World::basic` does for the origin region, for any region.
    pub fn from_chunks(id: RegionId, chunks: Vec<(ChunkCoords, Chunk)>) -> Self {
        let mut region = Region::new(Rollback::new(None), None, id, None);
        for (coords, chunk) in chunks {
            region.data.create_mesh(coords, chunk);
        }
        region
    }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p game --test region_from_chunks && cargo test -p worldgen`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(worldgen): deterministic generate_region with parity floor heights

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Region actor protocol — `RegionInput`/`RegionOutput`, `SerializedRegion`, `RegionRunner`

The message pair that is the future network seam, plus the threadless per-region actor core. `RegionRunner` is pumped by a thread loop on the server (Task 5) and inline by `LocalServer` (Task 8). Also adds `ClientPacket::ReleaseRegionConnection` to the wire protocol.

**Files:**
- Create: `crates/game/src/region_runner.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod region_runner; pub use region_runner::*;`)
- Modify: `crates/game/src/protocol.rs` (add `ClientPacket::ReleaseRegionConnection(RegionId)`)
- Test: `crates/game/tests/region_runner.rs` (new)

**Interfaces:**
- Consumes: `game::{Region, Rollback, RegionCoords, RegionId, GameEvent, GameEventKind, ClientId, Tick, TICK_RATE, Chunk, ChunkCoords}`; `Region::from_chunks` (Task 2); `Region::new`, `Region::handle_event`, `Region::forget_last_event`, `Region::data()`, `Region::current_tick()` (existing, region.rs).
- Produces (all in `game`, re-exported at crate root):

```rust
pub struct SerializedRegion(pub Vec<u8>);
impl SerializedRegion {
    pub fn from_rollback(r: &Rollback) -> Self;
    pub fn to_rollback(&self) -> Result<Rollback, Box<bincode::ErrorKind>>;
}

pub enum RegionInput {
    Event(GameEventKind),      // routed client events + manager-authoritative CreateClient
    RequestSnapshot(ClientId), // reply: RegionOutput::Snapshot
    Shutdown,                  // reply: RegionOutput::Stopped, then the runner stops
}

pub enum RegionOutput {
    EventProcessed(GameEvent),
    Snapshot(ClientId, Rollback),
    SyncClock { tick_rate: u64, tick: Tick },
    Stopped(SerializedRegion),
}

pub enum RegionSeed {
    Fresh(Vec<(ChunkCoords, Chunk)>),
    Parked(SerializedRegion, Vec<(ChunkCoords, Chunk)>), // fallback chunks if blob is corrupt
}
impl RegionSeed {
    pub fn into_region(self, id: RegionId) -> Region;
}

pub struct RegionRunner { /* id, region, out */ }
impl RegionRunner {
    pub fn new(id: RegionCoords, region: Region,
               out: crossbeam::channel::Sender<(RegionCoords, RegionOutput)>) -> Self;
    pub fn handle_input(&mut self, input: RegionInput) -> bool; // false = stop (after Shutdown)
    pub fn tick(&mut self);
    pub fn current_tick(&self) -> usize;
}
```

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/region_runner.rs`:

```rust
use crossbeam::channel::unbounded;
use game::{
    Chunk, ChunkCoords, GameEventKind, Region, RegionCoords, RegionInput, RegionOutput,
    RegionRunner, RegionSeed, SerializedRegion,
};
use std::hash::{Hash, Hasher};

fn flat_chunks() -> Vec<(ChunkCoords, Chunk)> {
    (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect()
}

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.hash(&mut h);
    h.finalize()
}

fn runner(id: RegionCoords) -> (RegionRunner, crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>) {
    let (out_send, out_recv) = unbounded();
    let region = Region::from_chunks(id, flat_chunks());
    (RegionRunner::new(id, region, out_send), out_recv)
}

#[test]
fn tick_emits_event_processed() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    r.tick();
    let (rc, output) = out.try_recv().expect("tick output");
    assert_eq!(rc, id);
    let RegionOutput::EventProcessed(ev) = output else { panic!("expected EventProcessed") };
    assert_eq!(ev.kind, GameEventKind::Tick);
    assert_eq!(ev.region_id, id);
}

#[test]
fn sync_clock_every_ten_ticks() {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    for _ in 0..10 {
        r.tick();
    }
    let clocks = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::SyncClock { .. }))
        .count();
    assert_eq!(clocks, 1, "exactly one SyncClock in the first 10 ticks");
}

#[test]
fn create_client_event_is_processed_and_snapshot_includes_player() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    assert!(r.handle_input(RegionInput::Event(GameEventKind::CreateClient(7))));
    assert!(r.handle_input(RegionInput::RequestSnapshot(7)));
    let outputs: Vec<_> = out.try_iter().map(|(_, o)| o).collect();
    assert!(matches!(&outputs[0], RegionOutput::EventProcessed(ev) if ev.kind == GameEventKind::CreateClient(7)));
    let RegionOutput::Snapshot(client, rollback) = &outputs[1] else { panic!("expected Snapshot") };
    assert_eq!(*client, 7);
    assert!(rollback.player_entites.contains_key(&7), "FIFO: snapshot after CreateClient includes the player");
}

#[test]
fn shutdown_stops_and_park_restore_is_hash_exact() {
    let id = RegionCoords::new(1, 0);
    let (mut r, out) = runner(id);
    // Mutate state so the roundtrip is non-trivial.
    r.handle_input(RegionInput::Event(GameEventKind::CreateClient(3)));
    for _ in 0..5 {
        r.tick();
    }
    let before = {
        // Snapshot for hashing: same clone path the wire uses.
        r.handle_input(RegionInput::RequestSnapshot(0));
        let RegionOutput::Snapshot(_, rb) = out.try_iter().map(|(_, o)| o).last().unwrap() else {
            panic!()
        };
        let mut h = crc32fast::Hasher::new();
        rb.data.hash(&mut h);
        h.finalize()
    };

    assert!(!r.handle_input(RegionInput::Shutdown), "Shutdown must stop the runner");
    let RegionOutput::Stopped(serialized) = out.try_iter().map(|(_, o)| o).last().unwrap() else {
        panic!("expected Stopped")
    };

    // Restore: the exact cycle-in path the manager uses.
    let restored = RegionSeed::Parked(serialized, flat_chunks()).into_region(id);
    assert_eq!(before, crc(&restored), "hash(before park) == hash(after restore), bit-exact");
}

#[test]
fn corrupt_parked_blob_falls_back_to_generation() {
    let id = RegionCoords::new(2, 2);
    let garbage = SerializedRegion(vec![0xde, 0xad, 0xbe, 0xef]);
    let region = RegionSeed::Parked(garbage, flat_chunks()).into_region(id);
    assert_eq!(region.data().ecs.entities.len(), 4, "fell back to the 2x2 fallback chunks");
}
```

Add to `crates/game/Cargo.toml` a `[dev-dependencies]` section with `crc32fast = { workspace = true }`. (It is already a *normal* dependency of `game`, but integration tests under `tests/` are separate crates — they cannot use `game`'s dependencies, only their own dev-dependencies.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p game --test region_runner`
Expected: FAIL to compile — `RegionRunner` etc. not found.

- [ ] **Step 3: Implement `crates/game/src/region_runner.rs`**

```rust
//! The per-region actor core: one `RegionRunner` per running region, fed
//! `RegionInput`s and emitting `RegionOutput`s over channels. This message
//! pair is the future network seam — the runner never sees subscriber
//! lists, client sessions, or network types. On the server each runner
//! lives on its own thread with its own tick timer (crates/server); in the
//! wasm build LocalServer pumps runners inline (crates/client).

use crossbeam::channel::Sender;
use crate::{
    Chunk, ChunkCoords, ClientId, GameEvent, GameEventKind, Region, RegionCoords, RegionId,
    Rollback, Tick, TICK_RATE,
};

/// Bincode-serialized region state — the parking-lot format. Same payload
/// the wire's `ServerPacket::Region` carries, kept opaque so parking never
/// aliases live state.
#[derive(Debug, Clone)]
pub struct SerializedRegion(pub Vec<u8>);

impl SerializedRegion {
    pub fn from_rollback(r: &Rollback) -> Self {
        Self(bincode::serialize(r).expect("region state must serialize"))
    }

    pub fn to_rollback(&self) -> Result<Rollback, Box<bincode::ErrorKind>> {
        bincode::deserialize(&self.0)
    }
}

#[derive(Debug)]
pub enum RegionInput {
    /// A routed client event or a manager-authoritative event
    /// (CreateClient). The region assigns the authoritative event id.
    Event(GameEventKind),
    /// A new subscriber needs the full state; replied with
    /// `RegionOutput::Snapshot(client, ...)`.
    RequestSnapshot(ClientId),
    /// Graceful stop; replied with `RegionOutput::Stopped(state)`.
    Shutdown,
}

#[derive(Debug)]
pub enum RegionOutput {
    EventProcessed(GameEvent),
    Snapshot(ClientId, Rollback),
    SyncClock { tick_rate: u64, tick: Tick },
    Stopped(SerializedRegion),
}

/// How to build a region when it spawns: restored from the parking lot if
/// possible, else generated. The fallback chunks make a corrupt parked blob
/// recoverable (log + regenerate deterministically).
pub enum RegionSeed {
    Fresh(Vec<(ChunkCoords, Chunk)>),
    Parked(SerializedRegion, Vec<(ChunkCoords, Chunk)>),
}

impl RegionSeed {
    pub fn into_region(self, id: RegionId) -> Region {
        match self {
            RegionSeed::Fresh(chunks) => Region::from_chunks(id, chunks),
            RegionSeed::Parked(serialized, fallback) => match serialized.to_rollback() {
                Ok(rollback) => Region::new(rollback, None, id, None),
                Err(e) => {
                    log::error!("parked region {:?} corrupt ({e}); regenerating", id);
                    Region::from_chunks(id, fallback)
                }
            },
        }
    }
}

pub struct RegionRunner {
    id: RegionCoords,
    region: Region,
    out: Sender<(RegionCoords, RegionOutput)>,
}

impl RegionRunner {
    pub fn new(
        id: RegionCoords,
        region: Region,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Self {
        Self { id, region, out }
    }

    /// Returns false when the runner should stop (after Shutdown).
    pub fn handle_input(&mut self, input: RegionInput) -> bool {
        match input {
            RegionInput::Event(kind) => match kind {
                // The manager filters these; drop defensively rather than
                // double-ticking or stopping on a stray packet.
                GameEventKind::Tick | GameEventKind::Quit => {}
                kind => {
                    // Server-side regions never roll back: forget each
                    // event's transaction immediately (undo log stays
                    // bounded), same policy as the old main loop.
                    let event = self
                        .region
                        .handle_event(kind)
                        .expect("region event processing failed");
                    self.region.forget_last_event();
                    let _ = self.out.send((self.id, RegionOutput::EventProcessed(event)));
                }
            },
            RegionInput::RequestSnapshot(client_id) => {
                let _ = self.out.send((
                    self.id,
                    RegionOutput::Snapshot(client_id, self.region.data().clone()),
                ));
            }
            RegionInput::Shutdown => {
                let serialized = SerializedRegion::from_rollback(self.region.data());
                let _ = self.out.send((self.id, RegionOutput::Stopped(serialized)));
                return false;
            }
        }
        true
    }

    /// One sim tick + the every-10-ticks SyncClock self-report. The caller
    /// owns pacing (thread timer on the server, frame accumulator on wasm).
    pub fn tick(&mut self) {
        let event = self
            .region
            .handle_event(GameEventKind::Tick)
            .expect("region tick failed");
        self.region.forget_last_event();
        let _ = self.out.send((self.id, RegionOutput::EventProcessed(event)));
        if self.region.current_tick() % 10 == 0 {
            let _ = self.out.send((
                self.id,
                RegionOutput::SyncClock {
                    tick_rate: TICK_RATE,
                    tick: self.region.current_tick(),
                },
            ));
        }
    }

    pub fn current_tick(&self) -> usize {
        self.region.current_tick()
    }
}
```

Wire into `crates/game/src/lib.rs` (next to the other module decls, lib.rs:17-23 / re-exports lib.rs:28-32):

```rust
pub mod region_runner;
pub use region_runner::*;
```

Add `ClientPacket::ReleaseRegionConnection(RegionId)` to protocol.rs:37-42:

```rust
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum ClientPacket {
    /// Game event generated by client that needs to be processed by the server.
    GameEvent(GameEvent),
    RequestPlayerRegion,
    RequestRegionConnection(RegionId),
    /// Client no longer wants this region's events (window moved on).
    ReleaseRegionConnection(RegionId),
}
```

The server's match on `ClientPacket` (main.rs:200) needs a temporary arm to keep compiling until Task 5 replaces the loop:

```rust
                ClientPacket::ReleaseRegionConnection(_) => {
                    // Subscription management lands with the WorldManager (Task 5).
                }
```

Same for `crates/client/src/local_server.rs` `pump()` match (until Task 8):

```rust
                ClientPacket::ReleaseRegionConnection(_) => {}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p game --test region_runner && cargo test -p game && cargo build --workspace --bins`
Expected: PASS. The park/restore hash test is the milestone's rollback-bar guarantee — if it fails, do NOT weaken the assertion; the serialization path is losing state (check that `Region::new` on restore doesn't mutate hashed fields — `reinitialize(None)` only rewires the update sender).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(game): region actor protocol (RegionInput/Output) + RegionRunner + SerializedRegion

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: `WorldManager` core + `InlineSpawner` (threadless, in `game`)

The world-level brain: sessions, homes, region registry, parking lot, routing, lifecycle. Threadless — it never spawns threads itself; a `RegionSpawner` implementation decides how regions run (threads on the server, inline for wasm/tests). `ServerEvent` moves into `game` so both the real server and `LocalServer` speak it.

**Files:**
- Create: `crates/game/src/world_manager.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod world_manager; pub use world_manager::*;`)
- Test: `crates/game/tests/world_manager.rs` (new)

**Interfaces:**
- Consumes: Task 3's `RegionInput`, `RegionOutput`, `RegionSeed`, `RegionRunner`, `SerializedRegion`; `game::{ClientPacket, ServerPacket, GameEvent, GameEventKind, ClientId, RegionCoords, Chunk, ChunkCoords, Rollback}`.
- Produces (all in `game`, re-exported at crate root):

```rust
pub const SPAWN_REGION: RegionCoords = RegionCoords { x: 0, z: 0 };
pub const UNLOAD_GRACE_MS: u64 = 5000;

/// Internal event handled by the world manager (moved here from
/// crates/server/src/main.rs; ServerTickTimer is gone — regions self-tick).
pub enum ServerEvent {
    ClientPacket(ClientPacket, ClientId),
    ClientConnected(ClientId),
    ClientDisconnected(ClientId),
}

pub type RegionGenerator = Box<dyn FnMut(RegionCoords) -> Vec<(ChunkCoords, Chunk)> + Send>;

pub trait RegionSpawner {
    fn spawn(&mut self, id: RegionCoords, seed: RegionSeed,
             out: Sender<(RegionCoords, RegionOutput)>) -> Sender<RegionInput>;
    /// Reclaim a stopped region's resources (join its thread). Default no-op.
    fn reap(&mut self, _id: RegionCoords) {}
}

/// Runs regions inline in the caller's thread — wasm LocalServer and tests.
#[derive(Default)]
pub struct InlineSpawner { /* runners: BTreeMap<RegionCoords, (Receiver<RegionInput>, RegionRunner)> */ }
impl InlineSpawner {
    pub fn pump(&mut self);      // drain every runner's inputs
    pub fn tick_all(&mut self);  // one tick on every runner
    pub fn running(&self) -> Vec<RegionCoords>;
}

pub struct WorldManager<S: RegionSpawner> { /* ... */ }
impl<S: RegionSpawner> WorldManager<S> {
    pub fn new(spawner: S, generator: RegionGenerator,
               out: Sender<(Option<ClientId>, ServerPacket)>,
               region_out_send: Sender<(RegionCoords, RegionOutput)>) -> Self;
    /// Returns false when the server should quit (a Quit game event).
    pub fn handle_server_event(&mut self, ev: ServerEvent, now_ms: u64) -> bool;
    pub fn handle_region_output(&mut self, rc: RegionCoords, output: RegionOutput, now_ms: u64);
    /// Grace-period unloads. Call periodically (~every 200ms is fine).
    pub fn maintain(&mut self, now_ms: u64);
    /// Send Shutdown to every running region (server exit path).
    pub fn shutdown_all(&mut self);
    pub fn running_regions(&self) -> Vec<RegionCoords>;
    pub fn parked_regions(&self) -> Vec<RegionCoords>;
    pub fn spawner_mut(&mut self) -> &mut S;
}
```

Time is always an explicit `now_ms: u64` parameter (monotonic, caller-defined origin) — keeps the core deterministic in tests and wasm-safe (no `Instant` in `game`).

- [ ] **Step 1: Write the failing tests**

Create `crates/game/tests/world_manager.rs`. The harness drives the manager exactly like `LocalServer` will:

```rust
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Chunk, ChunkCoords, ClientId, ClientPacket, GameEvent, GameEventKind, InlineSpawner,
    InputEvent, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager,
    SPAWN_REGION, UNLOAD_GRACE_MS,
};

struct Harness {
    manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

fn harness() -> Harness {
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let generator = Box::new(|_rc: RegionCoords| -> Vec<(ChunkCoords, Chunk)> {
        vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))]
    });
    Harness {
        manager: WorldManager::new(InlineSpawner::default(), generator, out_send, region_out_send),
        region_out,
        packets,
    }
}

impl Harness {
    /// Route → pump runners → route outputs, twice (outputs can trigger
    /// respawns/resubscribes that need one more pump). Mirrors LocalServer.
    fn settle(&mut self, now_ms: u64) {
        for _ in 0..2 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now_ms);
            }
        }
    }
    fn event(&mut self, ev: ServerEvent, now_ms: u64) -> bool {
        let alive = self.manager.handle_server_event(ev, now_ms);
        self.settle(now_ms);
        alive
    }
    fn drain_packets(&mut self) -> Vec<(Option<ClientId>, ServerPacket)> {
        self.packets.try_iter().collect()
    }
}

#[test]
fn connect_spawns_home_and_creates_player() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    assert_eq!(h.manager.running_regions(), vec![SPAWN_REGION]);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestPlayerRegion, 0), 0);
    let packets = h.drain_packets();
    assert!(packets.iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc == SPAWN_REGION)
    }));
}

#[test]
fn subscribe_spawns_region_and_delivers_snapshot() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    h.drain_packets();
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    assert!(h.manager.running_regions().contains(&rc));
    let packets = h.drain_packets();
    assert!(packets.iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::Region(id, _) if *id == rc)
    }));
}

#[test]
fn events_route_only_to_subscribed_regions() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.drain_packets();

    // Subscribed: input event comes back as an authoritative GameEvent.
    let ev = GameEvent::new(GameEventKind::PlayerInput(0, InputEvent::default()), 0, rc);
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0);
    assert!(h.drain_packets().iter().any(|(_, p)| matches!(p, ServerPacket::GameEvent(_))));

    // Not subscribed: dropped silently.
    let far = RegionCoords::new(9, 9);
    let ev = GameEvent::new(GameEventKind::PlayerInput(0, InputEvent::default()), 0, far);
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0);
    assert!(!h.manager.running_regions().contains(&far));
}

#[test]
fn release_then_grace_parks_region_and_resubscribe_restores_it() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0), 1000);

    // Before the grace period: still running.
    h.manager.maintain(1000 + UNLOAD_GRACE_MS - 1);
    h.settle(1000 + UNLOAD_GRACE_MS - 1);
    assert!(h.manager.running_regions().contains(&rc));

    // After: shut down and parked.
    h.manager.maintain(1000 + UNLOAD_GRACE_MS);
    h.settle(1000 + UNLOAD_GRACE_MS);
    assert!(!h.manager.running_regions().contains(&rc));
    assert!(h.manager.parked_regions().contains(&rc));

    // Resubscribe restores from the parking lot.
    h.drain_packets();
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 20_000);
    assert!(h.manager.running_regions().contains(&rc));
    assert!(!h.manager.parked_regions().contains(&rc));
    assert!(h.drain_packets().iter().any(|(_, p)| matches!(p, ServerPacket::Region(id, _) if *id == rc)));
}

#[test]
fn home_region_survives_zero_subscribers() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    // Client subscribes to home then wanders off and releases it.
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(SPAWN_REGION), 0), 100);
    h.manager.maintain(100 + UNLOAD_GRACE_MS * 2);
    h.settle(100 + UNLOAD_GRACE_MS * 2);
    // Still running: it hosts a connected client's player entity.
    assert!(h.manager.running_regions().contains(&SPAWN_REGION));

    // Once the client disconnects, the home region may park.
    h.event(ServerEvent::ClientDisconnected(0), 200 + UNLOAD_GRACE_MS * 2);
    h.manager.maintain(200 + UNLOAD_GRACE_MS * 4);
    h.settle(200 + UNLOAD_GRACE_MS * 4);
    assert!(!h.manager.running_regions().contains(&SPAWN_REGION));
    assert!(h.manager.parked_regions().contains(&SPAWN_REGION));
}

#[test]
fn reconnect_does_not_create_second_player() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    h.drain_packets();
    h.event(ServerEvent::ClientDisconnected(0), 100);
    h.event(ServerEvent::ClientConnected(0), 200);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 200);
    let packets = h.drain_packets();
    let snapshot = packets.iter().find_map(|(_, p)| match p {
        ServerPacket::Region(_, rollback) => Some(rollback),
        _ => None,
    }).expect("snapshot after resubscribe");
    assert_eq!(snapshot.player_entites.len(), 1, "reconnect must not duplicate the player");
}

#[test]
fn dead_region_respawns_and_resnapshots_subscribers() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    let rc = RegionCoords::new(2, 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.drain_packets();

    // Kill the runner behind the manager's back (thread-death stand-in:
    // the input channel's receiver is dropped, so the next send fails).
    h.manager.spawner_mut().kill(rc);

    // Next routed event detects the death, respawns, resnapshots.
    let ev = GameEvent::new(GameEventKind::PlayerInput(0, InputEvent::default()), 0, rc);
    h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 500);
    assert!(h.manager.running_regions().contains(&rc));
    assert!(h.drain_packets().iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::Region(id, _) if *id == rc)
    }));
}

#[test]
fn quit_event_stops_the_manager() {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    let ev = GameEvent::new(GameEventKind::Quit, 0, SPAWN_REGION);
    assert!(!h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), 0), 0));
}
```

Note the harness requires `InlineSpawner::kill(rc)` (test-support: drop a runner without going through Shutdown) and `InputEvent::default()` — check `crates/game/src/input.rs`; if `InputEvent` doesn't derive `Default`, use its simplest constructible variant instead (open input.rs and pick, e.g. a key-release event).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p game --test world_manager`
Expected: FAIL to compile — `WorldManager` not found.

- [ ] **Step 3: Implement `crates/game/src/world_manager.rs`**

```rust
//! World-level management: which regions run, who is subscribed to what,
//! where players' home regions are, and the parking lot for cycled-out
//! regions. Threadless: a `RegionSpawner` decides how regions actually run
//! (OS threads on the server, inline for wasm/tests), and time arrives as
//! an explicit `now_ms` so the core stays deterministic and wasm-safe.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};

use crate::{
    Chunk, ChunkCoords, ClientId, ClientPacket, GameEventKind, RegionCoords, RegionInput,
    RegionOutput, RegionRunner, RegionSeed, SerializedRegion, ServerPacket,
};

pub const SPAWN_REGION: RegionCoords = RegionCoords { x: 0, z: 0 };
pub const UNLOAD_GRACE_MS: u64 = 5000;

/// Internal event handled by the world manager. (Moved from
/// crates/server/src/main.rs; ServerTickTimer is gone — regions self-tick.)
#[derive(Debug)]
pub enum ServerEvent {
    ClientPacket(ClientPacket, ClientId),
    ClientConnected(ClientId),
    ClientDisconnected(ClientId),
}

pub type RegionGenerator = Box<dyn FnMut(RegionCoords) -> Vec<(ChunkCoords, Chunk)> + Send>;

pub trait RegionSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput>;

    /// Reclaim a stopped region's resources (join its thread). Called after
    /// `RegionOutput::Stopped` or when a dead region is detected.
    fn reap(&mut self, _id: RegionCoords) {}
}

/// Runs regions inline in the caller's thread: the wasm LocalServer and the
/// headless tests. `pump()`/`tick_all()` stand in for thread scheduling.
#[derive(Default)]
pub struct InlineSpawner {
    runners: BTreeMap<RegionCoords, (Receiver<RegionInput>, RegionRunner)>,
}

impl InlineSpawner {
    pub fn pump(&mut self) {
        let ids: Vec<RegionCoords> = self.runners.keys().copied().collect();
        for id in ids {
            let mut stopped = false;
            if let Some((recv, runner)) = self.runners.get_mut(&id) {
                while let Ok(input) = recv.try_recv() {
                    if !runner.handle_input(input) {
                        stopped = true;
                        break;
                    }
                }
            }
            if stopped {
                self.runners.remove(&id);
            }
        }
    }

    pub fn tick_all(&mut self) {
        for (_, (_, runner)) in self.runners.iter_mut() {
            runner.tick();
        }
    }

    pub fn running(&self) -> Vec<RegionCoords> {
        self.runners.keys().copied().collect()
    }

    /// Test support: make a region's channel dead without a Shutdown
    /// handshake, simulating a crashed region thread.
    pub fn kill(&mut self, id: RegionCoords) {
        self.runners.remove(&id);
    }
}

impl RegionSpawner for InlineSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput> {
        let (send, recv) = unbounded();
        let runner = RegionRunner::new(id, seed.into_region(id), out);
        self.runners.insert(id, (recv, runner));
        send
    }
}

#[derive(Default)]
struct Session {
    subscribed: BTreeSet<RegionCoords>,
}

struct RegionLink {
    input: Sender<RegionInput>,
    subscribers: BTreeSet<ClientId>,
    /// Set when the region lost its last keep-alive reason; cleared on
    /// resubscribe. Grace-period timestamp base.
    empty_since_ms: Option<u64>,
    /// Shutdown sent, Stopped not yet received. Subscribes arriving in this
    /// window queue in `resubscribe_pending` and re-run after Stopped.
    stopping: bool,
    resubscribe_pending: Vec<ClientId>,
}

pub struct WorldManager<S: RegionSpawner> {
    spawner: S,
    generator: RegionGenerator,
    regions: BTreeMap<RegionCoords, RegionLink>,
    parked: BTreeMap<RegionCoords, SerializedRegion>,
    /// Connected clients and their subscriptions.
    sessions: BTreeMap<ClientId, Session>,
    /// Survives disconnects: which region holds this client's player entity.
    /// (The player parks with its region; reconnects create nothing.)
    homes: BTreeMap<ClientId, RegionCoords>,
    out: Sender<(Option<ClientId>, ServerPacket)>,
    region_out_send: Sender<(RegionCoords, RegionOutput)>,
}

impl<S: RegionSpawner> WorldManager<S> {
    pub fn new(
        spawner: S,
        generator: RegionGenerator,
        out: Sender<(Option<ClientId>, ServerPacket)>,
        region_out_send: Sender<(RegionCoords, RegionOutput)>,
    ) -> Self {
        Self {
            spawner,
            generator,
            regions: BTreeMap::new(),
            parked: BTreeMap::new(),
            sessions: BTreeMap::new(),
            homes: BTreeMap::new(),
            out,
            region_out_send,
        }
    }

    /// Returns false when the server should quit (a Quit game event).
    pub fn handle_server_event(&mut self, ev: ServerEvent, now_ms: u64) -> bool {
        match ev {
            ServerEvent::ClientConnected(id) => {
                self.sessions.insert(id, Session::default());
                if !self.homes.contains_key(&id) {
                    // Server-authoritative player creation, once per client.
                    self.ensure_running(SPAWN_REGION);
                    self.homes.insert(id, SPAWN_REGION);
                    self.send_to_region(
                        SPAWN_REGION,
                        RegionInput::Event(GameEventKind::CreateClient(id)),
                    );
                } else {
                    // Reconnect: the player already exists in its home
                    // region (running or parked); nothing to create.
                    log::info!("client {id} reconnected");
                }
            }
            ServerEvent::ClientDisconnected(id) => {
                if let Some(session) = self.sessions.remove(&id) {
                    for rc in session.subscribed {
                        self.unsubscribe_link(id, rc, now_ms);
                    }
                }
                // The home region may have just lost its keep-alive reason.
                if let Some(home) = self.homes.get(&id).copied() {
                    self.refresh_keepalive(home, now_ms);
                }
            }
            ServerEvent::ClientPacket(packet, id) => match packet {
                ClientPacket::RequestPlayerRegion => {
                    let home = self.homes.get(&id).copied();
                    let _ = self.out.send((Some(id), ServerPacket::PlayerRegion(home, id)));
                }
                ClientPacket::RequestRegionConnection(rc) => self.subscribe(id, rc),
                ClientPacket::ReleaseRegionConnection(rc) => self.unsubscribe(id, rc, now_ms),
                ClientPacket::GameEvent(event) => match event.kind {
                    GameEventKind::Tick => {}
                    GameEventKind::Quit => return false,
                    kind => {
                        let subscribed = self
                            .sessions
                            .get(&id)
                            .map_or(false, |s| s.subscribed.contains(&event.region_id));
                        if subscribed && self.regions.contains_key(&event.region_id) {
                            self.send_to_region(event.region_id, RegionInput::Event(kind));
                        } else {
                            log::debug!(
                                "dropping event from client {id} for unsubscribed region {:?}",
                                event.region_id
                            );
                        }
                    }
                },
            },
        }
        true
    }

    pub fn handle_region_output(&mut self, rc: RegionCoords, output: RegionOutput, now_ms: u64) {
        match output {
            RegionOutput::EventProcessed(event) => {
                if let Some(link) = self.regions.get(&rc) {
                    for client in &link.subscribers {
                        let _ = self
                            .out
                            .send((Some(*client), ServerPacket::GameEvent(event.clone())));
                    }
                }
            }
            RegionOutput::Snapshot(client, rollback) => {
                let _ = self.out.send((Some(client), ServerPacket::Region(rc, rollback)));
            }
            RegionOutput::SyncClock { tick_rate, tick } => {
                if let Some(link) = self.regions.get(&rc) {
                    for client in &link.subscribers {
                        let _ = self.out.send((
                            Some(*client),
                            ServerPacket::SyncClock(rc, tick_rate, tick, Duration::ZERO),
                        ));
                    }
                }
            }
            RegionOutput::Stopped(serialized) => {
                self.parked.insert(rc, serialized);
                let pending = self
                    .regions
                    .remove(&rc)
                    .map(|l| l.resubscribe_pending)
                    .unwrap_or_default();
                self.spawner.reap(rc);
                for client in pending {
                    // A client asked for this region while it was stopping:
                    // now that the state is parked, cycle it right back in.
                    self.subscribe(client, rc);
                }
                let _ = now_ms;
            }
        }
    }

    /// Grace-period unloads: a region with no subscribers and no connected
    /// client's player entity parks after UNLOAD_GRACE_MS.
    pub fn maintain(&mut self, now_ms: u64) {
        let connected_homes: BTreeSet<RegionCoords> = self
            .sessions
            .keys()
            .filter_map(|c| self.homes.get(c).copied())
            .collect();
        let expired: Vec<RegionCoords> = self
            .regions
            .iter()
            .filter(|(rc, link)| {
                !link.stopping
                    && link.subscribers.is_empty()
                    && !connected_homes.contains(*rc)
                    && link
                        .empty_since_ms
                        .map_or(false, |t| now_ms.saturating_sub(t) >= UNLOAD_GRACE_MS)
            })
            .map(|(rc, _)| *rc)
            .collect();
        for rc in expired {
            let link = self.regions.get_mut(&rc).unwrap();
            link.stopping = true;
            if link.input.send(RegionInput::Shutdown).is_err() {
                // Already dead: nothing to park (state lost), just clean up.
                log::error!("region {:?} died before shutdown", rc);
                self.regions.remove(&rc);
                self.spawner.reap(rc);
            }
        }
    }

    pub fn shutdown_all(&mut self) {
        for (rc, link) in self.regions.iter_mut() {
            if !link.stopping {
                link.stopping = true;
                if link.input.send(RegionInput::Shutdown).is_err() {
                    log::error!("region {:?} died before shutdown", rc);
                }
            }
        }
    }

    pub fn running_regions(&self) -> Vec<RegionCoords> {
        self.regions.keys().copied().collect()
    }

    pub fn parked_regions(&self) -> Vec<RegionCoords> {
        self.parked.keys().copied().collect()
    }

    pub fn spawner_mut(&mut self) -> &mut S {
        &mut self.spawner
    }

    fn ensure_running(&mut self, rc: RegionCoords) {
        if self.regions.contains_key(&rc) {
            return;
        }
        let chunks = (self.generator)(rc);
        let seed = match self.parked.remove(&rc) {
            Some(serialized) => RegionSeed::Parked(serialized, chunks),
            None => RegionSeed::Fresh(chunks),
        };
        let input = self.spawner.spawn(rc, seed, self.region_out_send.clone());
        self.regions.insert(
            rc,
            RegionLink {
                input,
                subscribers: BTreeSet::new(),
                empty_since_ms: None,
                stopping: false,
                resubscribe_pending: Vec::new(),
            },
        );
    }

    fn subscribe(&mut self, client: ClientId, rc: RegionCoords) {
        self.ensure_running(rc);
        let link = self.regions.get_mut(&rc).unwrap();
        if link.stopping {
            link.resubscribe_pending.push(client);
            return;
        }
        link.subscribers.insert(client);
        link.empty_since_ms = None;
        if let Some(session) = self.sessions.get_mut(&client) {
            session.subscribed.insert(rc);
        }
        self.send_to_region(rc, RegionInput::RequestSnapshot(client));
    }

    fn unsubscribe(&mut self, client: ClientId, rc: RegionCoords, now_ms: u64) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.subscribed.remove(&rc);
        }
        self.unsubscribe_link(client, rc, now_ms);
    }

    fn unsubscribe_link(&mut self, client: ClientId, rc: RegionCoords, now_ms: u64) {
        let Some(link) = self.regions.get_mut(&rc) else { return };
        link.subscribers.remove(&client);
        drop(link);
        self.refresh_keepalive(rc, now_ms);
    }

    /// Re-evaluate whether `rc` still has a keep-alive reason; start the
    /// grace timer if not.
    fn refresh_keepalive(&mut self, rc: RegionCoords, now_ms: u64) {
        let is_home = self
            .sessions
            .keys()
            .any(|c| self.homes.get(c) == Some(&rc));
        let Some(link) = self.regions.get_mut(&rc) else { return };
        if link.subscribers.is_empty() && !is_home {
            link.empty_since_ms.get_or_insert(now_ms);
        } else {
            link.empty_since_ms = None;
        }
    }

    /// Route an input to a running region; a failed send means the region
    /// thread died — respawn it (parked state if any, else regenerated) and
    /// resnapshot every subscriber. The failed input itself is dropped; the
    /// snapshot resync covers the gap.
    fn send_to_region(&mut self, rc: RegionCoords, input: RegionInput) {
        let Some(link) = self.regions.get(&rc) else {
            log::debug!("send_to_region: {:?} not running", rc);
            return;
        };
        if link.input.send(input).is_ok() {
            return;
        }
        log::error!("region {:?} thread died; respawning", rc);
        let subscribers = self.regions.remove(&rc).unwrap().subscribers;
        self.spawner.reap(rc);
        self.ensure_running(rc);
        let link = self.regions.get_mut(&rc).unwrap();
        link.subscribers = subscribers.clone();
        for client in subscribers {
            let _ = link.input.send(RegionInput::RequestSnapshot(client));
        }
    }
}
```

Wire into `crates/game/src/lib.rs`:

```rust
pub mod world_manager;
pub use world_manager::*;
```

Note the borrow gymnastics: `unsubscribe_link` uses a `drop(link)` before `refresh_keepalive`; if the borrow checker still objects, inline the subscriber-removal and keepalive logic into one method. `InlineSpawner::kill` exists solely for the dead-region test.

- [ ] **Step 4: Run tests**

Run: `cargo test -p game --test world_manager && cargo test -p game`
Expected: PASS (all 8 new tests + existing suites).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(game): threadless WorldManager core with sessions, lifecycle, parking

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Server on region threads

The server keeps its dual-transport ingress (quinn `:6466` + WebTransport `:6467`, `ClientSink` fan-out, shared writer task — all merged in d936f0f and NOT to be restructured) and swaps only the game side: `run()`'s single-`World` loop, the tick-generator thread, and the adaptive tick rate are replaced by a manager loop + one thread per running region. The local `ServerEvent` enum is deleted in favor of `game::ServerEvent` (Task 4), and both transports gain `ClientDisconnected` reporting.

**Files:**
- Modify: `crates/server/src/lib.rs` (delete local `ServerEvent`; rewrite `run()`; add disconnect reporting in the quinn cleanup path)
- Modify: `crates/server/src/webtransport.rs` (disconnect reporting after the read loop)
- Create: `crates/server/src/region_threads.rs`
- Modify: `crates/server/Cargo.toml` (add `worldgen = { path = "../worldgen" }`)
- Modify: `crates/server/tests/webtransport_handshake.rs` (import `ServerEvent` from `game`; its scripted router is ITS OWN loop and keeps working — do not rewrite it)
- Test: `crates/server/tests/threaded_world.rs` (new)

**Interfaces:**
- Consumes: `game::{WorldManager, RegionSpawner, RegionSeed, RegionInput, RegionOutput, RegionRunner, ServerEvent, RegionCoords, TICK_RATE}`; `worldgen::generate_region`.
- Produces:

```rust
// crates/server/src/region_threads.rs
#[derive(Default)]
pub struct ThreadRegionSpawner { /* handles: BTreeMap<RegionCoords, JoinHandle<()>> */ }
impl game::RegionSpawner for ThreadRegionSpawner { /* spawn: OS thread; reap: join */ }
pub fn region_thread_loop(runner: game::RegionRunner,
                          recv: crossbeam::channel::Receiver<game::RegionInput>);

// crates/server/src/lib.rs
pub fn run(); // full server: netcode thread + manager loop; returns on Quit
```

- [ ] **Step 1: Swap `ServerEvent` to the shared type and report disconnects**

In `crates/server/src/lib.rs`:

1. DELETE the local `pub enum ServerEvent` (the block with `ClientPacket` / `ClientConnected` / `ServerTickTimer` doc comments) and add `ServerEvent` to the `use game::{...}` import. `game::ServerEvent` (Task 4) has `ClientDisconnected` instead of `ServerTickTimer`.
2. In `WorldIngress::listen`'s quinn accept loop, the per-connection cleanup task currently ends with `sinks.remove(&id);` — extend it:

```rust
                let fut = handle_connection(connection, send.clone(), id);
                tokio::spawn(async move {
                    if let Err(e) = fut.await {
                        error!("connection failed: {reason}", reason = e.to_string())
                    }
                    sinks.remove(&id);
                    // The world must know: unsubscribe everywhere, let the
                    // home region's grace timer start.
                    let _ = send.send(ServerEvent::ClientDisconnected(id));
                });
```

(The `send` passed to `handle_connection` must be a clone so the cleanup task still owns one — adjust the existing `let send = send.clone()` dance minimally.)

3. In `crates/server/src/webtransport.rs`, the read loop ends with `sinks.remove(&id);` — add the same report after it:

```rust
            sinks.remove(&id);
            let _ = send.send(ServerEvent::ClientDisconnected(id));
```

4. In `crates/server/tests/webtransport_handshake.rs`, fix the `ServerEvent` import to come from `game` (its scripted router matches on `ClientPacket`/`ClientConnected` only, which both still exist; add a catch-all `_ => {}` arm if the match becomes non-exhaustive over `ClientDisconnected`).

- [ ] **Step 2: Rewrite `run()` as the manager loop**

Replace everything in `run()` AFTER the `WorldIngress` thread spawn (i.e. delete: `World::basic()`, `results_buffer`, the tick-rate atomics, the tick-generator `std::thread::spawn`, and the whole `while let Ok(event)` match) with:

```rust
    let (region_out_send, region_out_recv) = crossbeam::channel::unbounded();
    let mut manager = game::WorldManager::new(
        region_threads::ThreadRegionSpawner::default(),
        Box::new(worldgen::generate_region),
        server_send,
        region_out_send,
    );

    let start = std::time::Instant::now();
    'main: loop {
        let now_ms = start.elapsed().as_millis() as u64;
        crossbeam::channel::select! {
            recv(client_packet_recv) -> ev => match ev {
                Ok(ev) => {
                    if !manager.handle_server_event(ev, now_ms) {
                        break 'main;
                    }
                }
                Err(_) => break 'main,
            },
            recv(region_out_recv) -> out => {
                if let Ok((rc, output)) = out {
                    manager.handle_region_output(rc, output, now_ms);
                }
            },
            default(std::time::Duration::from_millis(200)) => {}
        }
        manager.maintain(now_ms);
    }

    // Orderly exit: park everything, join region threads.
    manager.shutdown_all();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !manager.running_regions().is_empty() && std::time::Instant::now() < deadline {
        let now_ms = start.elapsed().as_millis() as u64;
        if let Ok((rc, output)) = region_out_recv.recv_timeout(std::time::Duration::from_millis(100)) {
            manager.handle_region_output(rc, output, now_ms);
        }
    }
```

Add `pub mod region_threads;` next to `pub mod webtransport;`. Keep the `simplelog` init at the top of `run()` and the channel names (`client_packet_send/recv`, `server_send/recv`) exactly as they are — the ingress side of the function does not change. Clean up now-unused imports (`World`, `ChunkCoords`, `BTreeMap`, `AtomicU64`, ...); the `use crossbeam::channel::select` style may differ from the fully-qualified form above — either is fine, match the file.

Add to `crates/server/Cargo.toml` `[dependencies]`: `worldgen = { path = "../worldgen" }`.

- [ ] **Step 3: Implement `crates/server/src/region_threads.rs`**

```rust
//! One OS thread per running region, each with its own tick timer.
//! Fully independent pacing: a slow region ticks late and catches up by
//! skipping missed deadlines; it never blocks the manager or its neighbours.
use std::collections::BTreeMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use game::{RegionCoords, RegionInput, RegionOutput, RegionRunner, RegionSeed, RegionSpawner};

#[derive(Default)]
pub struct ThreadRegionSpawner {
    handles: BTreeMap<RegionCoords, JoinHandle<()>>,
}

impl RegionSpawner for ThreadRegionSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput> {
        let (send, recv) = unbounded();
        let handle = std::thread::Builder::new()
            .name(format!("region {},{}", id.x, id.z))
            .spawn(move || {
                // Deserialize/generate on the region's own thread so a big
                // region never stalls the manager.
                let runner = RegionRunner::new(id, seed.into_region(id), out);
                region_thread_loop(runner, recv);
            })
            .expect("failed to spawn region thread");
        self.handles.insert(id, handle);
        send
    }

    fn reap(&mut self, id: RegionCoords) {
        if let Some(handle) = self.handles.remove(&id) {
            // The thread exits right after emitting Stopped (or its channel
            // died); this join is quick.
            if handle.join().is_err() {
                log::error!("region {:?} thread panicked", id);
            }
        }
    }
}

/// recv_deadline is the whole scheduler: handle inputs as they arrive, tick
/// when the deadline fires. Backpressure is inherent — a slow region ticks
/// late; missed deadlines are skipped rather than burst-replayed.
pub fn region_thread_loop(mut runner: RegionRunner, recv: Receiver<RegionInput>) {
    let tick = Duration::from_millis(game::TICK_RATE);
    let mut next = Instant::now() + tick;
    loop {
        match recv.recv_deadline(next) {
            Ok(input) => {
                if !runner.handle_input(input) {
                    return; // Shutdown acknowledged with Stopped
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                runner.tick();
                next += tick;
                let now = Instant::now();
                if next < now {
                    // Fell behind (heavy tick / scheduler stall): skip the
                    // missed deadlines instead of spiralling.
                    next = now + tick;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
```

- [ ] **Step 4: Build**

Run: `cargo build --workspace --bins`
Expected: compiles. The tick thread, `World::basic`, adaptive tick rate, and the old `ServerEvent` match are gone from `run()`; the ingress half of lib.rs (ClientSink, WorldIngress::listen, handle_connection, webtransport) is untouched apart from the two disconnect sends.

- [ ] **Step 5: Write the threaded integration test**

Create `crates/server/tests/threaded_world.rs` — real region threads, inline-driven manager, scripted client over channels (no QUIC):

```rust
//! Threaded smoke test: real region threads + the manager core, driven by a
//! scripted client over channels (bypassing quinn). Asserts the running-
//! region set tracks the client's window as it roams, and that event
//! streams keep flowing across the whole window.
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver};
use game::{
    ClientId, ClientPacket, RegionCoords, RegionOutput, ServerEvent, ServerPacket, WorldManager,
    SPAWN_REGION, UNLOAD_GRACE_MS,
};
use server::region_threads::ThreadRegionSpawner;

struct Rig {
    manager: WorldManager<ThreadRegionSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
    start: Instant,
}

impl Rig {
    fn new() -> Self {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let manager = WorldManager::new(
            ThreadRegionSpawner::default(),
            Box::new(worldgen::generate_region),
            out_send,
            region_out_send,
        );
        Rig { manager, region_out, packets, start: Instant::now() }
    }
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn event(&mut self, ev: ServerEvent) {
        assert!(self.manager.handle_server_event(ev, self.now_ms()));
        self.settle();
    }
    /// Drain region outputs until quiet for 100ms (region threads are async
    /// to the test; give them time to answer).
    fn settle(&mut self) {
        loop {
            match self.region_out.recv_timeout(Duration::from_millis(100)) {
                Ok((rc, output)) => {
                    let now = self.now_ms();
                    self.manager.handle_region_output(rc, output, now);
                }
                Err(_) => return,
            }
        }
    }
    /// Drain ALL pending packets once and return the set of region ids that
    /// got a snapshot. (try_iter consumes; never call this expecting packets
    /// to survive for a second look.)
    fn snapshot_regions(&mut self) -> std::collections::BTreeSet<RegionCoords> {
        self.packets
            .try_iter()
            .filter_map(|(_, p)| match p {
                ServerPacket::Region(id, _) => Some(id),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn roaming_client_cycles_regions_across_threads() {
    let mut rig = Rig::new();
    rig.event(ServerEvent::ClientConnected(0));

    // Subscribe the 3x3 window around home.
    for rc in SPAWN_REGION.window_3x3() {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0));
    }
    let mut running = rig.manager.running_regions();
    running.sort();
    let mut expected = SPAWN_REGION.window_3x3();
    expected.sort();
    assert_eq!(running, expected, "3x3 window running on real threads");
    let snapshots = rig.snapshot_regions();
    for rc in SPAWN_REGION.window_3x3() {
        assert!(snapshots.contains(&rc), "snapshot for {:?}", rc);
    }

    // Region threads tick on their own: SyncClock/GameEvent packets arrive
    // without the test doing anything.
    std::thread::sleep(Duration::from_millis(600));
    rig.settle();
    let ticked = rig
        .packets
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::GameEvent(_)))
        .count();
    assert!(ticked >= 9, "all 9 regions tick independently (saw {ticked} events)");

    // Roam east: window shifts from center (0,0) to center (1,0).
    let old_column: Vec<RegionCoords> = (-1..=1).map(|z| RegionCoords::new(-1, z)).collect();
    let new_column: Vec<RegionCoords> = (-1..=1).map(|z| RegionCoords::new(2, z)).collect();
    for rc in &old_column {
        rig.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(*rc), 0));
    }
    for rc in &new_column {
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(*rc), 0));
    }
    for rc in &new_column {
        assert!(rig.manager.running_regions().contains(rc));
    }

    // The released column parks after the grace period. Instead of sleeping
    // 5s, drive maintain with a future timestamp — time is an explicit input.
    let future = rig.now_ms() + UNLOAD_GRACE_MS + 1;
    rig.manager.maintain(future);
    // Stopped outputs come back over the channel from real threads.
    let deadline = Instant::now() + Duration::from_secs(2);
    while old_column.iter().any(|rc| rig.manager.running_regions().contains(rc)) {
        assert!(Instant::now() < deadline, "released regions failed to stop");
        if let Ok((rc, output)) = rig.region_out.recv_timeout(Duration::from_millis(100)) {
            rig.manager.handle_region_output(rc, output, future);
        }
    }
    for rc in &old_column {
        assert!(rig.manager.parked_regions().contains(rc), "{:?} parked", rc);
    }
}
```

Note: `snapshot_regions()` drains the packet channel once and asserts set-wise — packet *ordering* across regions is not guaranteed (nor meaningful; QUIC uni-streams don't preserve cross-stream order in production either).

- [ ] **Step 6: Run it**

Run: `cargo test -p server --test threaded_world -- --nocapture && cargo test -p server --test webtransport_handshake`
Expected: both PASS in a few seconds. Also rerun `cargo test -p game && cargo test -p client`.

- [ ] **Step 7: Manual smoke — server + native client still work single-region**

Run the server and client together briefly (the client still only requests one region until Task 6):

```bash
cargo run --bin server &
sleep 2
cargo run --bin client   # expect: world loads, player spawns, movement works
kill %1
```

Expected: identical behavior to before this task (one region requested, rendered at origin).

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(server): region threads with own timers behind a WorldManager thread

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Client window tracking and region cycling

The client derives its 3×3 window from the viewer's world position, subscribes/releases as the window moves, buffers events for regions whose snapshot hasn't arrived yet, replaces regions on re-receipt, and tells the render bridge when a region goes away.

**Files:**
- Modify: `crates/game/src/lib.rs:37-46` (add `ClientUpdateEvent::RemoveRegion(RegionId)`)
- Modify: `crates/client/src/main.rs` (`GameInstanceManager` fields + routing + window tracking)
- Test: `crates/client/src/main.rs` `manager_tests` module (extend)

**Interfaces:**
- Consumes: `RegionCoords::{from_world, window_3x3, world_offset}` (Task 1); `ClientPacket::ReleaseRegionConnection` (Task 3); `World::{remove_region, region_exists, data}`; `GameData` field access as the bridge already does (`data.player_entites`, `data.ecs.rigidbody`, `data.physics.bodies` — see bridge.rs:98-110).
- Produces: `ClientUpdateEvent::RemoveRegion(RegionId)` (consumed by Task 7); `GameInstanceManager` fields `home_region: Option<RegionCoords>`, `subscribed: BTreeSet<RegionCoords>`, `pending_events: BTreeMap<RegionCoords, Vec<GameEvent>>`; private methods `viewer_region()`, `update_window()`, `route_server_game_event(ev)`, `drop_region(rc)`.

- [ ] **Step 1: Add the update event**

In `crates/game/src/lib.rs`, extend `ClientUpdateEvent` (lib.rs:38-46):

```rust
#[derive(Debug)]
pub enum ClientUpdateEvent {
    NewRegion(
        RegionId,
        GameData,
        crossbeam::channel::Receiver<GameDataUpdate>,
    ),
    /// The window moved on (or a region is being replaced): tear down this
    /// region's render state.
    RemoveRegion(RegionId),
    GameCrash(GameError),
    SetPlayer(ClientId),
}
```

`crates/client/src/renderer/bridge.rs` `drain_client_updates` needs a temporary arm to compile until Task 7:

```rust
            ClientUpdateEvent::RemoveRegion(_) => { /* Task 7 */ }
```

- [ ] **Step 2: Write the failing manager tests**

Extend `manager_tests` in `crates/client/src/main.rs` (alongside `pump_loads_region_and_ticks`):

```rust
    fn manager_with_player() -> (
        GameInstanceManager,
        crossbeam::channel::Sender<ServerPacket>,
        Receiver<ServerPacket>,
        Receiver<ClientUpdateEvent>,
        crossbeam::channel::Sender<GameEventKind>,
    ) {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);
        (manager, server_send, server_recv, client_recv, game_send)
    }

    /// PlayerRegion must trigger subscription requests for the full 3x3
    /// window around home, not just home.
    #[test]
    fn player_region_requests_full_window() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        assert!(manager.pump(&server_recv).unwrap());

        let mut requested = std::collections::BTreeSet::new();
        while let Ok(p) = manager.client_packet_recv().try_recv() {
            if let ClientPacket::RequestRegionConnection(rc) = p {
                requested.insert(rc);
            }
        }
        let expected: std::collections::BTreeSet<_> = home.window_3x3().into_iter().collect();
        assert_eq!(requested, expected);
    }

    /// Events for a subscribed-but-not-yet-loaded region are buffered and
    /// applied when the snapshot arrives, not dropped.
    #[test]
    fn events_for_pending_region_are_buffered_until_snapshot() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        manager.pump(&server_recv).unwrap();

        // Load home so `world` exists.
        let world = game::World::basic();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump(&server_recv).unwrap();

        // A neighbour's tick arrives before its snapshot.
        let neighbour = RegionCoords::new(1, 0);
        let tick_from_neighbour =
            GameEvent::new(GameEventKind::Tick, 0, neighbour);
        server_send.send(ServerPacket::GameEvent(tick_from_neighbour)).unwrap();
        manager.pump(&server_recv).unwrap();
        assert!(!manager.world.as_ref().unwrap().region_exists(&neighbour));

        // Snapshot arrives: region loads, then the buffered event is
        // replayed through reconcile (which no-ops it here — id 0 is at the
        // snapshot's base_event_id — the point is it must not panic or drop
        // the region).
        let mut neighbour_world = game::World::new();
        neighbour_world.load(&neighbour, Region::from_chunks(neighbour, Vec::new()));
        server_send.send(neighbour_world.build_region_server_packet(&neighbour)).unwrap();
        manager.pump(&server_recv).unwrap();
        assert!(manager.world.as_ref().unwrap().region_exists(&neighbour));
    }

    /// A snapshot for an already-loaded region replaces it (crash-respawn /
    /// resubscribe path), emitting RemoveRegion then NewRegion to the bridge.
    #[test]
    fn region_re_receipt_replaces_and_signals_bridge() {
        let (mut manager, server_send, server_recv, client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let world = game::World::basic();
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump(&server_recv).unwrap();
        while client_recv.try_recv().is_ok() {} // clear initial events

        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump(&server_recv).unwrap();

        let events: Vec<ClientUpdateEvent> = client_recv.try_iter().collect();
        assert!(
            matches!(events[0], ClientUpdateEvent::RemoveRegion(rc) if rc == home),
            "teardown precedes rebuild, got {:?}", events
        );
        assert!(matches!(events[1], ClientUpdateEvent::NewRegion(rc, _, _) if rc == home));
    }

    /// A snapshot for a region outside the desired window is released
    /// immediately, never loaded (window moved on before it arrived).
    #[test]
    fn stale_snapshot_is_released_not_loaded() {
        let (mut manager, server_send, server_recv, _client_recv, _game) = manager_with_player();
        let home = RegionCoords::new(0, 0);
        let world = game::World::basic();
        server_send.send(ServerPacket::PlayerRegion(Some(home), 0)).unwrap();
        server_send.send(world.build_region_server_packet(&home)).unwrap();
        manager.pump(&server_recv).unwrap();
        while manager.client_packet_recv().try_recv().is_ok() {}

        let far = RegionCoords::new(50, 50);
        let mut far_world = game::World::new();
        far_world.load(&far, Region::from_chunks(far, Vec::new()));
        server_send.send(far_world.build_region_server_packet(&far)).unwrap();
        manager.pump(&server_recv).unwrap();

        assert!(!manager.world.as_ref().unwrap().region_exists(&far));
        let released: Vec<ClientPacket> = manager.client_packet_recv().try_iter().collect();
        assert!(released.iter().any(|p| matches!(p, ClientPacket::ReleaseRegionConnection(rc) if *rc == far)));
    }
```

Adjust imports at the top of `manager_tests` as needed (`Region`, `RegionCoords`, `ClientPacket`).

Run: `cargo test -p client manager_tests` — Expected: FAIL (fields/behavior missing).

- [ ] **Step 3: Implement the manager changes in `crates/client/src/main.rs`**

Field changes on `GameInstanceManager` (locate by symbol; line refs predate the WebTransport merge): replace `player_chunk: Option<RegionCoords>` with:

```rust
    home_region: Option<RegionCoords>,
    /// Regions we have requested (and not released) — the desired window.
    subscribed: std::collections::BTreeSet<RegionCoords>,
    /// Events for subscribed regions whose snapshot hasn't arrived yet.
    pending_events: BTreeMap<RegionCoords, Vec<GameEvent>>,
```

Initialize all three in `new()` (`None` / `BTreeSet::new()` / `BTreeMap::new()`). Delete every `player_chunk` mention; `handle_game_event`'s PlayerInput arm routes to `self.home_region` instead (same shape as before). Do NOT touch the input-admission guard arm — it is now `PlayerInput(_, _) if !self.is_caught_up` (commit 190fdc0 removed the flapping `ready` gate; there is a regression test).

Viewer position + window logic (new `impl GameInstanceManager` methods, target-independent):

```rust
    /// The viewer's current region, from the local player's sim body in the
    /// home region (region-local pose + home offset). Free-cam local coords
    /// run past the region bounds on purpose; from_world floor-divides them
    /// into the right neighbour. Falls back to home when the player entity
    /// isn't readable yet.
    fn viewer_region(&self) -> Option<RegionCoords> {
        let home = self.home_region?;
        let world = self.world.as_ref()?;
        if !world.region_exists(&home) {
            return Some(home);
        }
        let data = world.data(&home);
        let Some(client_id) = self.client_id else { return Some(home) };
        let Some(key) = data.player_entites.get(&client_id).copied() else {
            return Some(home);
        };
        let Some(handle) = *data.ecs.rigidbody.try_get(key) else {
            return Some(home);
        };
        let Some(body) = data.physics.bodies.get(handle) else {
            return Some(home);
        };
        let t = body.translation();
        let off = home.world_offset();
        // Real = OrderedFloat<f32>; .0 unwraps to f32 (same as convert.rs).
        Some(RegionCoords::from_world(t.x.0 + off[0], t.z.0 + off[2]))
    }

    /// Diff the desired 3x3 window against current subscriptions; request
    /// the new, release the stale, and tear stale regions out of the local
    /// world + render bridge. Cheap when nothing changed (set compare).
    fn update_window(&mut self) {
        let Some(center) = self.viewer_region() else { return };
        let desired: std::collections::BTreeSet<RegionCoords> =
            center.window_3x3().into_iter().collect();
        if desired == self.subscribed {
            return;
        }
        for rc in desired.difference(&self.subscribed) {
            self.server_game_send
                .send(ClientPacket::RequestRegionConnection(*rc))
                .unwrap();
        }
        let stale: Vec<RegionCoords> = self.subscribed.difference(&desired).copied().collect();
        for rc in stale {
            self.server_game_send
                .send(ClientPacket::ReleaseRegionConnection(rc))
                .unwrap();
            self.drop_region(rc);
        }
        self.subscribed = desired;
    }

    /// Remove a region from the local world and the render bridge.
    fn drop_region(&mut self, rc: RegionCoords) {
        self.pending_events.remove(&rc);
        if let Some(ref mut world) = self.world {
            if world.remove_region(&rc).is_some() {
                let _ = self.client_event_send.send(ClientUpdateEvent::RemoveRegion(rc));
            }
        }
    }
```

Call `update_window()` at the end of `handle_game_event`'s `Tick` arm (after `progress_world_one_tick`).

`handle_server` rework (main.rs:170-262):

- `PlayerRegion(id, client_id)` arm: keep the `SetPlayer` send and `self.client_id = Some(client_id)`; then:

```rust
                let home = id.unwrap_or(RegionCoords::new(0, 0));
                self.home_region = Some(home);
                // Ask for the whole 3x3 window up front; update_window keeps
                // it in sync from then on.
                for rc in home.window_3x3() {
                    self.server_game_send
                        .send(ClientPacket::RequestRegionConnection(rc))
                        .unwrap();
                    self.subscribed.insert(rc);
                }
```

- `Region(id, raw_game_data)` arm becomes:

```rust
            game::ServerPacket::Region(id, raw_game_data) => {
                if !self.subscribed.contains(&id) {
                    // Window moved on while the snapshot was in flight.
                    self.server_game_send
                        .send(ClientPacket::ReleaseRegionConnection(id))
                        .unwrap();
                    return Ok(());
                }
                // Replace-on-re-receipt: crash-respawn/resubscribe resyncs.
                self.drop_region(id);
                new_region(id, raw_game_data, &mut self.world);
                // Replay events that raced ahead of the snapshot; reconcile
                // skips anything already baked in (base_event_id).
                if let Some(pending) = self.pending_events.remove(&id) {
                    if let Some(ref mut world) = self.world {
                        for ev in pending {
                            let _ = world.reconcile_event(ev);
                        }
                    }
                }
            }
```

(`drop_region` before `new_region` also emits `RemoveRegion` ahead of `NewRegion` for the bridge — assert order in the test above. Note `new_region` is the existing closure at main.rs:174-193; hoist it into a method if the borrow checker fights the new call order.)

- `GameEvent(game_event)` arm: replace the body with routing that distinguishes loaded / pending / foreign regions (the pre-world `buffer` stays as-is):

```rust
            game::ServerPacket::GameEvent(game_event) => {
                if self.world.is_none() {
                    self.buffer.push(game_event);
                    self.is_caught_up = false;
                    return Ok(());
                }
                for event in self.buffer.drain(..).collect::<Vec<_>>() {
                    self.route_server_game_event(event)?;
                }
                self.route_server_game_event(game_event)?;
                self.is_caught_up = true;
            }
```

with:

```rust
    fn route_server_game_event(&mut self, event: GameEvent) -> Result<(), GameError> {
        let world = self.world.as_mut().expect("routed only when world exists");
        if world.region_exists(&event.region_id) {
            match world.reconcile_event(event) {
                Ok(()) => Ok(()),
                Err(e) => {
                    warn!("reconcile failed: {:?}", e);
                    Err(e)
                }
            }
        } else if self.subscribed.contains(&event.region_id) {
            // Snapshot still in flight: hold the event for replay.
            self.pending_events.entry(event.region_id).or_default().push(event);
            Ok(())
        } else {
            // Released/never-wanted region: steady-state noise.
            log::debug!("dropping event for region {:?}", event.region_id);
            Ok(())
        }
    }
```

- [ ] **Step 4: Run the client tests**

Run: `cargo test -p client`
Expected: PASS — the four new tests plus all pre-existing ones (`pump_loads_region_and_ticks` needs its `player_chunk` references gone; `offline_handshake_loads_region` in local_server.rs still passes because LocalServer answers `RequestRegionConnection` for any region via `build_region_server_packet` — if it panics on unknown regions (World::build_region_server_packet unwraps), give local_server.rs a temporary guard that ignores requests for regions the world doesn't have; Task 8 replaces this file wholesale).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(client): 3x3 window tracking with subscribe/release region cycling

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Render bridge — region offsets and teardown

Two changes from the spec: region roots sit at their world offset (regions stop overlapping), and `RemoveRegion` tears down everything the bridge built for a region. Meshing already tolerates despawned entities (`apply_meshed_chunks` guards with `commands.get_entity`, meshing.rs:128).

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (`drain_client_updates` + helper)
- Test: `crates/client/src/renderer/bridge.rs` tests module

**Interfaces:**
- Consumes: `ClientUpdateEvent::RemoveRegion` (Task 6), `RegionCoords::world_offset` (Task 1).
- Produces: nothing new outside the bridge; `NewRegion` roots now have `Transform::from_translation(offset)`.

- [ ] **Step 1: Write the failing tests**

Add to the bridge tests module:

```rust
    #[test]
    fn region_root_sits_at_world_offset() {
        let (mut app, client) = app_shell();
        let (_send, update_recv) = crossbeam::channel::unbounded();
        let region_id = game::RegionCoords::new(1, -2);
        let rb = Rollback::new(None);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv))
            .unwrap();
        app.update();
        let root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();
        let tf = app.world().entity(root).get::<Transform>().unwrap();
        assert_eq!(tf.translation, Vec3::new(256.0, 0.0, -512.0));
    }

    #[test]
    fn remove_region_tears_down_root_maps_and_receiver() {
        let (mut app, client, _updates, region_id) = test_app();
        app.update();
        let root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();

        client.send(ClientUpdateEvent::RemoveRegion(region_id)).unwrap();
        app.update();

        assert!(app.world().get_entity(root).is_err(), "root despawned (with children)");
        assert!(!app.world().resource::<RegionRoots>().0.contains_key(&region_id));
        assert!(!app.world().resource::<Regions>().0.contains_key(&region_id));
        assert!(app.world().resource::<SimEntityMap>().0.keys().all(|(r, _)| *r != region_id));
    }

    #[test]
    fn new_region_for_loaded_region_replaces_it() {
        let (mut app, client, _updates, region_id) = test_app();
        app.update();
        let first_root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();

        let (_send2, update_recv2) = crossbeam::channel::unbounded();
        let rb = Rollback::new(None);
        client
            .send(ClientUpdateEvent::NewRegion(region_id, (*rb.data).clone(), update_recv2))
            .unwrap();
        app.update();

        let second_root = *app.world().resource::<RegionRoots>().0.get(&region_id).unwrap();
        assert_ne!(first_root, second_root, "old root replaced");
        assert!(app.world().get_entity(first_root).is_err(), "old root despawned");
    }
```

Run: `cargo test -p client bridge` — Expected: FAIL (offset is IDENTITY; RemoveRegion arm is a no-op).

- [ ] **Step 2: Implement in `crates/client/src/renderer/bridge.rs`**

Add the teardown helper and rewrite the two arms of `drain_client_updates` (bridge.rs:62-85):

```rust
/// Tear down everything the bridge built for a region: root entity tree,
/// update receiver, entity map entries. Also the first half of
/// replace-on-re-receipt (crash-respawn resync).
fn remove_region(
    commands: &mut Commands,
    regions: &mut Regions,
    roots: &mut RegionRoots,
    map: &mut SimEntityMap,
    id: RegionId,
) {
    if let Some(root) = roots.0.remove(&id) {
        // despawn() removes descendants via ChildOf relationships (Bevy 0.16+).
        commands.entity(root).despawn();
    }
    regions.0.remove(&id);
    map.0.retain(|(region, _), _| *region != id);
}
```

In the `NewRegion` arm, before spawning the root:

```rust
            ClientUpdateEvent::NewRegion(id, data, receiver) => {
                if roots.0.contains_key(&id) {
                    // Replace-on-re-receipt: resubscribe/crash-respawn resync.
                    remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                }
                let offset = id.world_offset();
                let root = commands
                    .spawn((
                        Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2])),
                        Visibility::default(),
                        Name::new(format!("region {:?}", id)),
                    ))
                    .id();
                roots.0.insert(id, root);
                regions.0.insert(id, receiver);
                spawn_region_snapshot(&mut commands, root, id, &data, &mut map, player.0);
                info!("bridge: region {:?} loaded", id);
            }
            ClientUpdateEvent::RemoveRegion(id) => {
                remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                info!("bridge: region {:?} removed", id);
            }
```

One structural check: `drain_region_updates` iterates `regions.0` and `expect`s a matching root (bridge.rs:149). Removal keeps both maps in sync in the same helper, so the invariant holds; leave the `expect`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p client`
Expected: PASS, including the three new bridge tests and the meshing suite (in-flight tasks against despawned entities are already guarded).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(client): region roots at world offsets + RemoveRegion teardown

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: `LocalServer` on the shared `WorldManager` core (wasm parity)

The wasm/offline server stops hand-mirroring server logic and becomes a thin single-threaded shell around `WorldManager<InlineSpawner>` — the browser build gets region cycling for free. The public interface (`new(recv, send)`, `pump()`, `tick()`) is unchanged, so `sim_driver.rs` needs no changes.

**Files:**
- Modify: `crates/client/src/local_server.rs` (rewrite the struct internals; keep tests, add cycling test)
- Modify: `crates/client/Cargo.toml` (add `worldgen = { path = "../worldgen" }`)

**Interfaces:**
- Consumes: `game::{WorldManager, InlineSpawner, ServerEvent, RegionOutput, RegionCoords, ClientId, ClientPacket, ServerPacket, TICK_RATE}`; `worldgen::generate_region`.
- Produces: `LocalServer::{new(recv, send), pump(), tick()}` — same signatures as today, so `sim_driver.rs` keeps compiling untouched. (Since the WebTransport merge, sim_driver has an online mode that skips LocalServer entirely; only the offline path constructs it. Don't touch sim_driver.)

- [ ] **Step 1: Rewrite `crates/client/src/local_server.rs`**

Keep the file header intent, replace the body:

```rust
//! Embedded single-player "server" for the wasm build: the same
//! WorldManager core the real server runs, with regions pumped inline
//! (no threads in the browser), behind the same channel interface
//! netcode::ServerConnection provides on native. A future
//! WebTransport/WebSocket transport replaces this without touching
//! GameInstanceManager.
use crossbeam::channel::{unbounded, Receiver, Sender};
use game::{
    ClientId, ClientPacket, GameError, InlineSpawner, RegionCoords, RegionOutput, ServerEvent,
    ServerPacket, WorldManager, TICK_RATE,
};

/// The only client in an offline world.
pub const LOCAL_CLIENT_ID: ClientId = 0;

pub struct LocalServer {
    manager: WorldManager<InlineSpawner>,
    recv: Receiver<ClientPacket>,
    send: Sender<ServerPacket>,
    out_recv: Receiver<(Option<ClientId>, ServerPacket)>,
    region_out_recv: Receiver<(RegionCoords, RegionOutput)>,
    /// Monotonic sim-time for grace-period lifecycle; advances TICK_RATE ms
    /// per authoritative tick (no wall clock on wasm).
    now_ms: u64,
}

impl LocalServer {
    pub fn new(
        recv: Receiver<ClientPacket>,
        send: Sender<ServerPacket>,
    ) -> Result<Self, GameError> {
        let (out_send, out_recv) = unbounded();
        let (region_out_send, region_out_recv) = unbounded();
        let mut manager = WorldManager::new(
            InlineSpawner::default(),
            Box::new(worldgen::generate_region),
            out_send,
            region_out_send,
        );
        // Server-authoritative player creation, as on ClientConnected.
        manager.handle_server_event(ServerEvent::ClientConnected(LOCAL_CLIENT_ID), 0);
        let mut server = Self {
            manager,
            recv,
            send,
            out_recv,
            region_out_recv,
            now_ms: 0,
        };
        server.drain();
        Ok(server)
    }

    /// Drain pending client packets without blocking.
    pub fn pump(&mut self) -> Result<(), GameError> {
        while let Ok(packet) = self.recv.try_recv() {
            // Quit is a no-op offline (no process to stop); the manager
            // returning false is deliberately ignored here.
            let _ = self
                .manager
                .handle_server_event(ServerEvent::ClientPacket(packet, LOCAL_CLIENT_ID), self.now_ms);
        }
        self.drain();
        Ok(())
    }

    /// Advance every running region one tick and run lifecycle upkeep.
    pub fn tick(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.manager.maintain(self.now_ms);
        self.drain();
    }

    /// Pump inline regions and route their outputs, twice: outputs can
    /// trigger follow-up work (resubscribe-after-park, respawn snapshots)
    /// that needs one more pump to answer within the same frame. Then
    /// forward everything to the single client (all packets are ours).
    fn drain(&mut self) {
        for _ in 0..2 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, output)) = self.region_out_recv.try_recv() {
                self.manager.handle_region_output(rc, output, self.now_ms);
            }
        }
        while let Ok((_target, packet)) = self.out_recv.try_recv() {
            let _ = self.send.send(packet);
        }
    }
}
```

Add to `crates/client/Cargo.toml` `[dependencies]`: `worldgen = { path = "../worldgen" }`.

- [ ] **Step 2: Update + extend the local_server tests**

The existing `offline_handshake_loads_region` test should pass with ONE change: the client now requests a 3×3 window, so more `NewRegion` events arrive — the test's `saw_region` logic already tolerates that. Bump the pump loop from `0..4` to `0..6` iterations if the handshake needs an extra round trip.

Add two cycling tests:

```rust
    /// The offline client loads the full 3x3 window through the shared
    /// WorldManager core.
    #[test]
    fn offline_window_loads_nine_regions() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded::<ServerPacket>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);
        let mut server = LocalServer::new(manager.client_packet_recv(), server_send).unwrap();
        manager.start();

        // Handshake + window subscription needs a few pump rounds:
        // RequestPlayerRegion -> PlayerRegion -> 9x RequestRegionConnection -> 9x Region.
        for _ in 0..6 {
            server.pump().unwrap();
            assert!(manager.pump(&server_recv).unwrap());
        }

        let mut loaded = std::collections::BTreeSet::new();
        let mut receivers = Vec::new(); // keep bridge receivers alive
        while let Ok(ev) = client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(rc, _, recv) = ev {
                loaded.insert(rc);
                receivers.push(recv);
            }
        }
        let expected: std::collections::BTreeSet<_> =
            game::RegionCoords::new(0, 0).window_3x3().into_iter().collect();
        assert_eq!(loaded, expected, "3x3 offline window loaded");
        drop(receivers);
    }

    /// Release + grace parks a region offline; resubscribing restores it
    /// from the parking lot (a fresh Region packet arrives again).
    #[test]
    fn offline_release_parks_and_resubscribe_restores() {
        use game::{RegionCoords, UNLOAD_GRACE_MS, TICK_RATE};

        let (packet_send, packet_recv) = crossbeam::channel::unbounded::<ClientPacket>();
        let (out_send, out_recv) = crossbeam::channel::unbounded::<ServerPacket>();
        let mut server = LocalServer::new(packet_recv, out_send).unwrap();

        let corner = RegionCoords::new(-1, -1);
        packet_send.send(ClientPacket::RequestRegionConnection(corner)).unwrap();
        server.pump().unwrap();
        assert!(
            out_recv.try_iter().any(|p| matches!(p, ServerPacket::Region(rc, _) if rc == corner)),
            "snapshot on first subscribe"
        );

        packet_send.send(ClientPacket::ReleaseRegionConnection(corner)).unwrap();
        server.pump().unwrap();
        // tick() advances the internal clock TICK_RATE ms per call; run past
        // the grace period so maintain() parks the corner.
        for _ in 0..(UNLOAD_GRACE_MS / TICK_RATE + 2) {
            server.tick();
        }
        while out_recv.try_recv().is_ok() {} // discard tick traffic

        packet_send.send(ClientPacket::RequestRegionConnection(corner)).unwrap();
        server.pump().unwrap();
        assert!(
            out_recv.try_iter().any(|p| matches!(p, ServerPacket::Region(rc, _) if rc == corner)),
            "parked region restored on resubscribe"
        );
    }
```

(Second test caveat: the corner region must not be the spawn region — `SPAWN_REGION` hosts the local player's entity and never parks while the client session exists. `(-1,-1)` is safe.)

- [ ] **Step 3: Run everything**

Run: `cargo test -p client && cargo test -p game`
Expected: PASS.

- [ ] **Step 4: wasm build check**

```bash
rustup target list --installed | grep -q wasm32-unknown-unknown || rustup target add wasm32-unknown-unknown
cargo build -p client --target wasm32-unknown-unknown
```

Expected: compiles. (`InlineSpawner`/`WorldManager` are thread-free; `worldgen` is pure — nothing here can pull in native-only deps.)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(client): LocalServer wraps the shared WorldManager core (offline region cycling)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: End-to-end acceptance, docs, and cleanup

**Files:**
- Modify: `CLAUDE.md` (server/`game` crate descriptions)
- Modify: `TODO.md` (mark the multi-region items done / roll forward)
- No new code; fixes only if acceptance surfaces bugs.

**Interfaces:** none.

- [ ] **Step 1: Full test sweep**

```bash
cargo test -p game && cargo test -p client && cargo test -p worldgen && cargo test -p server
cargo build --workspace --bins
cargo build -p client --target wasm32-unknown-unknown
```

Expected: all PASS. Fix regressions before proceeding.

- [ ] **Step 2: Manual acceptance — roam across regions (server + one client)**

```bash
cargo run --bin server &
sleep 2
WGPU_ADAPTER_NAME=6700 cargo run --bin client
```

Checklist (free-cam with `E`, WASD + mouse to roam):

1. World loads; the floor around spawn is at height 8; adjacent regions are visible with floors at height 12 (checkerboard steps at region boundaries every 256 units).
2. Fly across a boundary: the next ring of regions streams in ahead of you (watch the client log for `bridge: region ... loaded`).
3. Regions two boundaries behind you disappear (`bridge: region ... removed`); the server log shows their threads parking after ~5s.
4. Fly back: the same regions return (from the parking lot — identical terrain).
5. Player movement/physics feel identical to before (home region sim untouched).
6. Kill the client and reconnect: player still exists, world resyncs (no duplicate player).

Then the offline browser build:

```bash
WASM_SERVER_RUNNER_CUSTOM_INDEX_HTML=crates/client/index.html \
  cargo run -p client --target wasm32-unknown-unknown
```

Expected: same roaming behavior fully offline (single-threaded cycling).

If any step fails: use superpowers:systematic-debugging, fix, and re-run the sweep in Step 1 before continuing.

- [ ] **Step 3: Update docs**

`CLAUDE.md` — rewrite the `crates/server` bullet to describe the netcode-thread + manager-thread + region-threads topology and `game`'s new role (WorldManager core, RegionRunner, region actor protocol; `worldgen` no longer a stub — deterministic `generate_region`). Mention `REGION_SIZE`/`RegionCoords` and that `RegionId` is no longer `ChunkCoords`.

`TODO.md` — remove the four completed items (3×3 grid, world-level events, per-instance generation/rendering, cycling); add the known follow-ups uncovered by this milestone:

```markdown
- Entity/player handoff between regions (deferred by design; spec 2026-07-05-multi-region-world).
- Client per-region clock tracking (single shared tick_rate today; 9 SyncClock streams fight over it).
- Durable region persistence (parking lot is in-memory only).
- Cross-region event relay groundwork (deferred with handoff).
```

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "docs: multi-region world — CLAUDE.md topology + TODO rollforward

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Known risks (read before Task 1)

- **`InputEvent::default()`** in the Task 4/5 tests: verify `InputEvent` derives `Default` (crates/game/src/input.rs); substitute the simplest constructible variant if not.
- **`Rollback` field access from integration tests** (`rollback.data`, `data.player_entites`, `data.ecs.entities`): the client bridge already does exactly this cross-crate (bridge.rs:98-135), so visibility is proven; if an integration test hits a private field the macro didn't make `pub`, prefer adding a small accessor on `Region`/`Rollback` over changing the macro.
- **Client shared `tick_rate` across 9 regions**: 9 SyncClock streams adjusting one atomic. All regions run pinned at TICK_RATE so the adjustments agree in steady state; if `ready` flaps in practice, gate SyncClock handling to the home region as a stopgap and note it in TODO.md (proper per-region clocks are follow-up work).
- **Event-order race on subscribe** (EventProcessed reaching the client before its Region snapshot): handled by `pending_events` buffering (Task 6) + `base_event_id` skipping (region.rs:62). Do not "fix" by reordering server sends — QUIC uni-streams don't guarantee cross-stream order anyway.
- **Bevy 0.18 API drift**: `despawn()` is relationship-recursive (0.16+); `commands.get_entity` returns `Result` (already used, meshing.rs:128). Any other drift: check the pinned 0.18 docs, not memory.
