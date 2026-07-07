# Far-Region Visual Chunks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render read-only, visual-only terrain for regions outside the client's 3×3 simulated window, served by the server as a stripped chunk-only snapshot and meshed with the normal pipeline, refreshed only when terrain changes.

**Architecture:** A third region tier ("far") on top of today's near/nothing model. The server serves a terrain-only `FarSnapshot` from a running region (pulled) or from a per-region `far_cache` / worldgen for parked/never-run regions (without waking them). A running region pushes a throttled refresh when its terrain hash changes. The client renders far regions as plain chunk entities under a region root at the region's world offset — no local sim `Region`, no physics, no rollback.

**Tech Stack:** Rust; `game` crate (engine-agnostic sim + netcode core); `client` crate (Bevy app + bridge); crossbeam channels; bincode + crc32fast; the existing `WorldManager` / `RegionRunner` / render-bridge seams.

## Global Constraints

- `game` and `server` stay Bevy-free and windowing-library-free — no rendering/windowing deps.
- Cross-thread communication is crossbeam channels; the `RegionInput`/`RegionOutput` pair and the `ClientPacket`/`ServerPacket` pair are the future network seams — regions never see subscriber lists or network types.
- Wire serialization is `bincode`; state/terrain hashing is `crc32fast`.
- Far-viewing must never wake a parked region and must never keep a region alive (mirrors the "ghost updates never wake a region" rule).
- Region sims stay region-local; the world offset (`RegionCoords::world_offset() = (x*256, 0, z*256)`) exists only at the render boundary.
- `FAR_VIEW_RADIUS` is Chebyshev, default `2` (5×5 footprint). `FAR_SNAPSHOT_MIN_INTERVAL_TICKS` default `25`. Both are named constants in `game` — no UI.
- Determinism/rollback bar is unaffected: the far path never enters undo/sim (`hash(before) == hash(after undo)` suites must still pass).

---

### Task 1: `FarSnapshot` type, constants, and extraction

**Files:**
- Modify: `crates/game/src/protocol.rs` (add `FarSnapshot`, constants; extend `use crate::{...}`)
- Modify: `crates/game/src/state.rs` (add `Rollback::far_snapshot`, in the existing `impl Rollback`)
- Modify: `crates/game/src/region.rs` (add `Region::far_snapshot`)
- Test: `crates/game/tests/far_snapshot.rs` (new)

**Interfaces:**
- Produces:
  - `pub struct FarSnapshot { pub chunks: Vec<(ChunkCoords, Chunk)> }` (Debug, Clone, Serialize, Deserialize)
  - `impl FarSnapshot { pub fn from_columns(columns: Vec<(ColumnCoords, Column)>) -> FarSnapshot; pub fn terrain_hash(&self) -> u32 }`
  - `pub const FAR_VIEW_RADIUS: i32 = 2;`
  - `pub const FAR_SNAPSHOT_MIN_INTERVAL_TICKS: usize = 25;`
  - `impl Rollback { pub fn far_snapshot(&self) -> FarSnapshot }`
  - `impl Region { pub fn far_snapshot(&self) -> FarSnapshot }`
- Consumes: `Chunk`, `ChunkCoords`, `Column`, `ColumnCoords`, `Column::materialized_sub_chunks`, `Region::from_chunks` (all existing).

- [ ] **Step 1: Write the failing test**

Create `crates/game/tests/far_snapshot.rs`:

```rust
use game::{
    Chunk, ChunkCoords, FarSnapshot, Region, RegionCoords, FAR_SNAPSHOT_MIN_INTERVAL_TICKS,
    FAR_VIEW_RADIUS,
};

fn flat_chunks() -> Vec<(ChunkCoords, Chunk)> {
    (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect()
}

#[test]
fn constants_have_expected_defaults() {
    assert_eq!(FAR_VIEW_RADIUS, 2);
    assert_eq!(FAR_SNAPSHOT_MIN_INTERVAL_TICKS, 25);
}

#[test]
fn region_far_snapshot_returns_all_terrain_chunks() {
    let region = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks());
    let snap = region.far_snapshot();
    // Four terrain chunks in, four out — no entities/players contributed.
    assert_eq!(snap.chunks.len(), 4);
    let mut coords: Vec<(usize, i32, usize)> =
        snap.chunks.iter().map(|(c, _)| (c.x, c.y, c.z)).collect();
    coords.sort();
    assert_eq!(coords, vec![(0, 0, 0), (0, 0, 1), (1, 0, 0), (1, 0, 1)]);
}

#[test]
fn terrain_hash_is_stable_and_content_sensitive() {
    let a = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).far_snapshot();
    let b = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).far_snapshot();
    assert_eq!(a.terrain_hash(), b.terrain_hash());

    let taller: Vec<(ChunkCoords, Chunk)> = (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(12))))
        .collect();
    let c = Region::from_chunks(RegionCoords::new(0, 0), taller).far_snapshot();
    assert_ne!(a.terrain_hash(), c.terrain_hash());
}

#[test]
fn from_columns_materializes_same_chunks_as_region() {
    use game::{Block, BlockId, Column, ColumnCoords, SubChunk};
    let below = Block::new(BlockId::AIR);
    let above = Block::new(BlockId::AIR);
    let columns: Vec<(ColumnCoords, Column)> = (0..2)
        .flat_map(move |x| {
            (0..2).map(move |z| {
                let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
                (ColumnCoords::new(x, z), Column::new(0, below, above, vec![sub]))
            })
        })
        .collect();
    let from_cols = FarSnapshot::from_columns(columns);
    let from_region = Region::from_chunks(RegionCoords::new(0, 0), flat_chunks()).far_snapshot();
    assert_eq!(from_cols.terrain_hash(), from_region.terrain_hash());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test far_snapshot`
Expected: FAIL to compile — `FarSnapshot`, `FAR_VIEW_RADIUS`, `FAR_SNAPSHOT_MIN_INTERVAL_TICKS`, `Region::far_snapshot` do not exist.

- [ ] **Step 3a: Add the type and constants**

In `crates/game/src/protocol.rs`, extend the existing crate import (currently `use crate::{Client, ClientId, EntityKey, EntityKind, IsometryReal, Rollback};`) to also bring in the voxel/column types:

```rust
use crate::{
    Chunk, ChunkCoords, Client, ClientId, Column, ColumnCoords, EntityKey, EntityKind,
    IsometryReal, Rollback,
};
```

Then, immediately after the `pub const REGION_SIZE` line, add the constants:

```rust
/// Chebyshev radius of the visual-only far ring around the viewer region.
/// Default 2 → a 5×5 footprint (one ring outside the near 3×3 window). No UI;
/// raise this constant (with LOD, later) to extend the horizon.
pub const FAR_VIEW_RADIUS: i32 = 2;

/// A running region checks its terrain hash this often and pushes a
/// `FarSnapshotChanged` only when it changed — the check cadence IS the
/// refresh throttle.
pub const FAR_SNAPSHOT_MIN_INTERVAL_TICKS: usize = 25;
```

And add the type near the other wire structs (e.g. just before `pub enum ClientPacket`):

```rust
/// The visual-only subset of a region's data: terrain chunks, nothing else.
/// Much smaller than a `Rollback` (a `Chunk` is 8 blocks + a sparse chisel
/// map). Served to far-viewers; never carries entities/physics/undo state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FarSnapshot {
    pub chunks: Vec<(ChunkCoords, Chunk)>,
}

impl FarSnapshot {
    /// Materialize worldgen columns into the same `(ChunkCoords, Chunk)` form
    /// `Region::from_columns` uses — how never-run regions are served.
    pub fn from_columns(columns: Vec<(ColumnCoords, Column)>) -> Self {
        let chunks = columns
            .into_iter()
            .flat_map(|(col, column)| {
                column
                    .materialized_sub_chunks()
                    .into_iter()
                    .map(move |(sub_y, chunk)| (ChunkCoords::new(col.x, sub_y, col.z), chunk))
            })
            .collect();
        Self { chunks }
    }

    /// crc32 of the terrain, for cheap change detection on the region thread.
    pub fn terrain_hash(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&bincode::serialize(self).expect("far snapshot serializes"));
        h.finalize()
    }
}
```

- [ ] **Step 3b: Add `Rollback::far_snapshot`**

In `crates/game/src/state.rs`, inside the existing `impl Rollback` block (the one containing `create_mesh` / `scan_boundaries`), add:

```rust
    /// Terrain-only extraction for far-region views. Iterates chunk
    /// components; recovers each chunk's `ChunkCoords` from its fixed body
    /// translation (`create_mesh` places bodies at `(cx*32, cy*32, cz*32)`,
    /// exact). No entities/physics/undo state leaks out.
    pub fn far_snapshot(&self) -> crate::FarSnapshot {
        let mut chunks = Vec::new();
        for (key, maybe_chunk) in self.data.ecs.chunk.iter() {
            let Some(chunk) = maybe_chunk else { continue };
            let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let t = body.translation();
            // Real = OrderedFloat<f32>; `.0` unwraps to f32 (as scan_boundaries).
            let coords = ChunkCoords::new(
                (t.x.0 / 32.0) as usize,
                (t.y.0 / 32.0) as i32,
                (t.z.0 / 32.0) as usize,
            );
            chunks.push((coords, chunk.clone()));
        }
        crate::FarSnapshot { chunks }
    }
```

(`ChunkCoords` is already imported at the top of `state.rs`.)

- [ ] **Step 3c: Add `Region::far_snapshot`**

In `crates/game/src/region.rs`, inside `impl Region` (e.g. next to `data()`), add:

```rust
    pub fn far_snapshot(&self) -> crate::FarSnapshot {
        self.data.far_snapshot()
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game --test far_snapshot`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/protocol.rs crates/game/src/state.rs crates/game/src/region.rs crates/game/tests/far_snapshot.rs
git commit -m "feat(game): FarSnapshot terrain-only extraction + constants"
```

---

### Task 2: Far-region wire protocol variants

**Files:**
- Modify: `crates/game/src/protocol.rs` (`ClientPacket`, `ServerPacket`)
- Test: `crates/game/tests/far_snapshot.rs` (append)

**Interfaces:**
- Produces:
  - `ClientPacket::RequestFarRegion(RegionId)`, `ClientPacket::ReleaseFarRegion(RegionId)`
  - `ServerPacket::FarRegion(RegionId, FarSnapshot)`
- Consumes: `FarSnapshot` (Task 1), `RegionId` (existing).

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/far_snapshot.rs`:

```rust
#[test]
fn far_wire_packets_roundtrip_bincode() {
    use game::{ClientPacket, RegionCoords, ServerPacket};

    let rc = RegionCoords::new(3, -1);
    let req = ClientPacket::RequestFarRegion(rc);
    let bytes = bincode::serialize(&req).unwrap();
    assert!(matches!(
        bincode::deserialize::<ClientPacket>(&bytes).unwrap(),
        ClientPacket::RequestFarRegion(r) if r == rc
    ));

    let rel = ClientPacket::ReleaseFarRegion(rc);
    let bytes = bincode::serialize(&rel).unwrap();
    assert!(matches!(
        bincode::deserialize::<ClientPacket>(&bytes).unwrap(),
        ClientPacket::ReleaseFarRegion(r) if r == rc
    ));

    let snap = Region::from_chunks(rc, flat_chunks()).far_snapshot();
    let pkt = ServerPacket::FarRegion(rc, snap);
    let bytes = bincode::serialize(&pkt).unwrap();
    assert!(matches!(
        bincode::deserialize::<ServerPacket>(&bytes).unwrap(),
        ServerPacket::FarRegion(r, s) if r == rc && s.chunks.len() == 4
    ));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test far_snapshot far_wire_packets_roundtrip_bincode`
Expected: FAIL to compile — the new variants don't exist.

- [ ] **Step 3: Add the variants**

In `crates/game/src/protocol.rs`, add to `enum ClientPacket`:

```rust
    /// Client wants read-only visual terrain for a far (unsimulated) region.
    RequestFarRegion(RegionId),
    /// Client no longer wants this region's far terrain (ring moved on).
    ReleaseFarRegion(RegionId),
```

And to `enum ServerPacket`:

```rust
    /// Read-only visual terrain for a far region (initial send AND refresh —
    /// replace-on-re-receipt). Removal is client-driven; no "gone" packet.
    FarRegion(RegionId, FarSnapshot),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game --test far_snapshot`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/protocol.rs crates/game/tests/far_snapshot.rs
git commit -m "feat(game): far-region wire packets"
```

---

### Task 3: Region actor far messages + runner behavior

**Files:**
- Modify: `crates/game/src/region_runner.rs` (`RegionInput`, `RegionOutput`, `RegionRunner`)
- Modify: `crates/game/tests/region_runner.rs` (fix the `Stopped` match; add far tests)

**Interfaces:**
- Produces:
  - `RegionInput::RequestFarSnapshot(ClientId)`
  - `RegionOutput::FarSnapshot(ClientId, FarSnapshot)`
  - `RegionOutput::FarSnapshotChanged(FarSnapshot)`
  - `RegionOutput::Stopped(SerializedRegion, FarSnapshot)` (was `Stopped(SerializedRegion)`)
- Consumes: `Region::far_snapshot`, `FarSnapshot::terrain_hash`, `FAR_SNAPSHOT_MIN_INTERVAL_TICKS` (Task 1).

- [ ] **Step 1: Write the failing test**

In `crates/game/tests/region_runner.rs`, first fix the existing `Stopped` match at line ~98 so it compiles with the new field:

```rust
    let RegionOutput::Stopped(serialized, _far) = out.try_iter().map(|(_, o)| o).last().unwrap()
    else {
        panic!("expected Stopped");
    };
```

Then append new tests:

```rust
#[test]
fn request_far_snapshot_replies_with_terrain() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    r.handle_input(RegionInput::RequestFarSnapshot(7));
    let (rc, output) = out.try_recv().expect("far snapshot output");
    assert_eq!(rc, id);
    let RegionOutput::FarSnapshot(client, snap) = output else {
        panic!("expected FarSnapshot")
    };
    assert_eq!(client, 7);
    assert_eq!(snap.chunks.len(), 4);
}

#[test]
fn stable_terrain_never_pushes_far_change() {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    for _ in 0..(game::FAR_SNAPSHOT_MIN_INTERVAL_TICKS * 2 + 1) {
        r.tick();
    }
    let pushes = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::FarSnapshotChanged(_)))
        .count();
    assert_eq!(pushes, 0, "unchanged terrain must not push a far refresh");
}

#[test]
fn terrain_edit_pushes_one_throttled_far_change() {
    use game::{Chunk, ChunkCoords};
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    // Mutate terrain: replace a chunk with a taller floor via the test hook.
    r.region_mut().with_data(|d| {
        d.create_mesh(ChunkCoords::new(0, 1, 0), Chunk::flat_floor(8));
    });
    for _ in 0..(game::FAR_SNAPSHOT_MIN_INTERVAL_TICKS + 1) {
        r.tick();
    }
    let pushes = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::FarSnapshotChanged(_)))
        .count();
    assert_eq!(pushes, 1, "one change → exactly one throttled push");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test region_runner`
Expected: FAIL to compile — `RequestFarSnapshot`/`FarSnapshot`/`FarSnapshotChanged` and the two-field `Stopped` don't exist yet.

- [ ] **Step 3a: Extend the message enums**

In `crates/game/src/region_runner.rs`, update the import line to add the far items:

```rust
use crate::{
    ClientId, Column, ColumnCoords, EntityBundle, FarSnapshot, GameEvent, GameEventKind, GhostData,
    Region, RegionCoords, RegionId, Rollback, Tick, FAR_SNAPSHOT_MIN_INTERVAL_TICKS, TICK_RATE,
};
```

Add to `enum RegionInput`:

```rust
    /// A far-viewer needs read-only terrain; replied with
    /// `RegionOutput::FarSnapshot(client, ...)`. Does not affect sim state.
    RequestFarSnapshot(ClientId),
```

Change `Stopped` and add the far outputs in `enum RegionOutput`:

```rust
    /// Reply to `RequestFarSnapshot`: read-only terrain for one far-viewer.
    FarSnapshot(ClientId, FarSnapshot),
    /// Throttled push when this region's terrain changed; manager fans out.
    FarSnapshotChanged(FarSnapshot),
    Stopped(SerializedRegion, FarSnapshot),
```

(Remove the old `Stopped(SerializedRegion),` line — leave the doc comment.)

- [ ] **Step 3b: Track the terrain hash on the runner**

Add a field to `struct RegionRunner`:

```rust
pub struct RegionRunner {
    id: RegionCoords,
    region: Region,
    out: Sender<(RegionCoords, RegionOutput)>,
    /// Last-pushed terrain hash; a change past the throttle window pushes a
    /// `FarSnapshotChanged`. Seeded at construction so the initial state is a
    /// baseline, not a spurious push.
    last_terrain_hash: u32,
}
```

Update `RegionRunner::new` to seed it (signature unchanged):

```rust
    pub fn new(
        id: RegionCoords,
        region: Region,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Self {
        let last_terrain_hash = region.far_snapshot().terrain_hash();
        Self { id, region, out, last_terrain_hash }
    }
```

- [ ] **Step 3c: Handle the far request and the Shutdown reply**

In `handle_input`, add a match arm for the new input (alongside `RequestSnapshot`):

```rust
            RegionInput::RequestFarSnapshot(client_id) => {
                let _ = self.out.send((
                    self.id,
                    RegionOutput::FarSnapshot(client_id, self.region.far_snapshot()),
                ));
            }
```

Update the `Shutdown` arm to carry the far snapshot:

```rust
            RegionInput::Shutdown => {
                let serialized = SerializedRegion::from_rollback(self.region.data());
                let far = self.region.far_snapshot();
                let _ = self.out.send((self.id, RegionOutput::Stopped(serialized, far)));
                return false;
            }
```

- [ ] **Step 3d: Throttled change-push in `tick`**

At the end of `RegionRunner::tick`, after the `SyncClock` block, add:

```rust
        // Push-on-change, throttled: the check cadence is the throttle. The
        // manager drops the push if the region has no far-viewers.
        if self.region.current_tick() % FAR_SNAPSHOT_MIN_INTERVAL_TICKS == 0 {
            let snap = self.region.far_snapshot();
            let hash = snap.terrain_hash();
            if hash != self.last_terrain_hash {
                self.last_terrain_hash = hash;
                let _ = self.out.send((self.id, RegionOutput::FarSnapshotChanged(snap)));
            }
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game --test region_runner`
Expected: PASS (existing tests + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/region_runner.rs crates/game/tests/region_runner.rs
git commit -m "feat(game): region actor far-snapshot request + throttled change push"
```

---

### Task 4: Manager far-viewer tier

**Files:**
- Modify: `crates/game/src/world_manager.rs` (`Session`, `WorldManager`, `handle_server_event`, `handle_region_output`, new `far_subscribe`/`far_unsubscribe`)
- Test: `crates/game/tests/world_manager.rs` (append)

**Interfaces:**
- Consumes: `ClientPacket::RequestFarRegion`/`ReleaseFarRegion`, `ServerPacket::FarRegion` (Task 2); `RegionInput::RequestFarSnapshot`, `RegionOutput::FarSnapshot`/`FarSnapshotChanged`/`Stopped(_, _)` (Task 3); `FarSnapshot::from_columns` (Task 1).
- Produces: far-viewing routed end-to-end; `far_subscribers`/`far_cache` state; keep-alive still ignores far-viewers.

- [ ] **Step 1: Write the failing test**

Append to `crates/game/tests/world_manager.rs`. Add the helper import at the top file `use` (add `FarSnapshot`, `RegionInput` are not needed by tests; only `ServerPacket::FarRegion` is matched — already have `ServerPacket`). Append:

```rust
/// Connect client 0 and drain the join packets, returning the harness ready
/// for far-region assertions.
fn connected_harness() -> Harness {
    let mut h = harness();
    h.event(ServerEvent::ClientConnected(0), 0);
    h.drain_packets();
    h
}

#[test]
fn far_request_for_never_run_region_serves_without_waking() {
    let mut h = connected_harness();
    let rc = RegionCoords::new(5, 5);
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestFarRegion(rc), 0),
        0,
    );
    // Never spun up a sim thread for the far region.
    assert!(!h.manager.running_regions().contains(&rc));
    let packets = h.drain_packets();
    assert!(
        packets.iter().any(|(target, p)| {
            *target == Some(0) && matches!(p, ServerPacket::FarRegion(id, _) if *id == rc)
        }),
        "far request must deliver a FarRegion packet"
    );
}

#[test]
fn far_request_for_running_region_pulls_live_snapshot() {
    let mut h = connected_harness();
    let rc = RegionCoords::new(1, 0);
    // Make it running via a normal (near) subscribe first.
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0),
        0,
    );
    h.drain_packets();
    assert!(h.manager.running_regions().contains(&rc));
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestFarRegion(rc), 0),
        0,
    );
    let packets = h.drain_packets();
    assert!(packets.iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::FarRegion(id, _) if *id == rc)
    }));
}

#[test]
fn far_viewers_do_not_keep_a_region_alive() {
    let mut h = connected_harness();
    let rc = RegionCoords::new(9, 9);
    // Wake it (arrival-style) by near-subscribing, then release near but keep far.
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0),
        0,
    );
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestFarRegion(rc), 0),
        0,
    );
    h.drain_packets();
    h.event(
        ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0),
        0,
    );
    // Grace timer starts; after the window, maintain must park it despite the far-viewer.
    h.manager.maintain(UNLOAD_GRACE_MS + 1);
    h.settle(UNLOAD_GRACE_MS + 1);
    assert!(
        !h.manager.running_regions().contains(&rc),
        "a region with only far-viewers must still park"
    );
    assert!(h.manager.parked_regions().contains(&rc));
}

#[test]
fn parked_far_viewer_still_gets_served_from_cache() {
    // After the region parks (previous scenario), a fresh far request is
    // served from far_cache without waking it.
    let mut h = connected_harness();
    let rc = RegionCoords::new(2, 3);
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0),
        0,
    );
    h.event(
        ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0),
        0,
    );
    h.manager.maintain(UNLOAD_GRACE_MS + 1);
    h.settle(UNLOAD_GRACE_MS + 1);
    assert!(h.manager.parked_regions().contains(&rc));
    h.drain_packets();
    h.event(
        ServerEvent::ClientPacket(ClientPacket::RequestFarRegion(rc), UNLOAD_GRACE_MS + 2),
        UNLOAD_GRACE_MS + 2,
    );
    assert!(!h.manager.running_regions().contains(&rc), "far serve must not wake a parked region");
    let packets = h.drain_packets();
    assert!(packets.iter().any(|(target, p)| {
        *target == Some(0) && matches!(p, ServerPacket::FarRegion(id, _) if *id == rc)
    }));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p game --test world_manager`
Expected: FAIL to compile — the manager doesn't handle the far packets/outputs yet.

- [ ] **Step 3a: Add manager state**

In `crates/game/src/world_manager.rs`, add `FarSnapshot` to the crate import:

```rust
use crate::{
    ClientId, ClientPacket, Column, ColumnCoords, FarSnapshot, GameEventKind, Region, RegionCoords,
    RegionInput, RegionOutput, RegionRunner, RegionSeed, SerializedRegion, ServerPacket,
};
```

Add a field to `struct Session`:

```rust
#[derive(Default)]
struct Session {
    subscribed: BTreeSet<RegionCoords>,
    far_subscribed: BTreeSet<RegionCoords>,
}
```

Add two fields to `struct WorldManager<S: RegionSpawner>` (after `parked`):

```rust
    /// Far-viewers per region, INDEPENDENT of running/parked state.
    far_subscribers: BTreeMap<RegionCoords, BTreeSet<ClientId>>,
    /// Frozen terrain for parked / never-run regions, so far serving never
    /// wakes a region. Refreshed on FarSnapshotChanged and on park (Stopped).
    far_cache: BTreeMap<RegionCoords, FarSnapshot>,
```

Initialize them in `WorldManager::new` (after `parked: BTreeMap::new(),`):

```rust
            far_subscribers: BTreeMap::new(),
            far_cache: BTreeMap::new(),
```

- [ ] **Step 3b: Route the client packets**

In `handle_server_event`, in the `ClientPacket` match, add two arms (next to `RequestRegionConnection`):

```rust
                ClientPacket::RequestFarRegion(rc) => self.far_subscribe(id, rc),
                ClientPacket::ReleaseFarRegion(rc) => self.far_unsubscribe(id, rc),
```

In the `ServerEvent::ClientDisconnected(id)` arm, after the existing `for rc in session.subscribed { ... }` loop and still inside `if let Some(session) = self.sessions.remove(&id)`, add:

```rust
                    for rc in session.far_subscribed {
                        if let Some(set) = self.far_subscribers.get_mut(&rc) {
                            set.remove(&id);
                            if set.is_empty() {
                                self.far_subscribers.remove(&rc);
                            }
                        }
                    }
```

- [ ] **Step 3c: Handle the region outputs**

In `handle_region_output`, add two arms and update `Stopped`:

```rust
            RegionOutput::FarSnapshot(client, snap) => {
                self.far_cache.insert(rc, snap.clone());
                let _ = self.out.send((Some(client), ServerPacket::FarRegion(rc, snap)));
            }
            RegionOutput::FarSnapshotChanged(snap) => {
                self.far_cache.insert(rc, snap.clone());
                if let Some(subs) = self.far_subscribers.get(&rc) {
                    for client in subs {
                        let _ = self
                            .out
                            .send((Some(*client), ServerPacket::FarRegion(rc, snap.clone())));
                    }
                }
            }
```

Change the `Stopped` arm's first two lines to destructure the new field and cache it:

```rust
            RegionOutput::Stopped(serialized, far) => {
                self.far_cache.insert(rc, far);
                self.parked.insert(rc, serialized);
                // … unchanged remainder (resubscribe_pending drain, reap, replay) …
```

- [ ] **Step 3d: Add the far subscribe/unsubscribe methods**

Add to `impl<S: RegionSpawner> WorldManager<S>` (near `subscribe`):

```rust
    fn far_subscribe(&mut self, client: ClientId, rc: RegionCoords) {
        let Some(session) = self.sessions.get_mut(&client) else {
            log::debug!("dropping far subscribe from sessionless client {client} for {:?}", rc);
            return;
        };
        session.far_subscribed.insert(rc);
        self.far_subscribers.entry(rc).or_default().insert(client);
        // Running (and not stopping) → pull live from the region.
        let running = self.regions.get(&rc).map_or(false, |l| !l.stopping);
        if running {
            self.send_to_region(rc, RegionInput::RequestFarSnapshot(client));
            return;
        }
        // Parked / never-run → serve from cache without waking; generate on miss.
        let snap = match self.far_cache.get(&rc) {
            Some(snap) => snap.clone(),
            None => {
                let columns = (self.generator)(rc);
                let snap = FarSnapshot::from_columns(columns);
                self.far_cache.insert(rc, snap.clone());
                snap
            }
        };
        let _ = self.out.send((Some(client), ServerPacket::FarRegion(rc, snap)));
    }

    fn far_unsubscribe(&mut self, client: ClientId, rc: RegionCoords) {
        if let Some(session) = self.sessions.get_mut(&client) {
            session.far_subscribed.remove(&rc);
        }
        if let Some(set) = self.far_subscribers.get_mut(&rc) {
            set.remove(&client);
            if set.is_empty() {
                self.far_subscribers.remove(&rc);
            }
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p game --test world_manager`
Expected: PASS (existing + 4 new). Also run the full crate to catch match-exhaustiveness fallout:
Run: `cargo test -p game`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/world_manager.rs crates/game/tests/world_manager.rs
git commit -m "feat(game): manager far-viewer tier (serve from cache/worldgen, never wake or keep alive)"
```

---

### Task 5: Client far-window policy + receive path

**Files:**
- Modify: `crates/game/src/lib.rs` (`ClientUpdateEvent`)
- Modify: `crates/client/src/instance.rs` (`far_subscribed` field, `desired_far`, `update_far_window`, `drop_far_region`, handle `ServerPacket::FarRegion`, call `update_far_window` in the tick)
- Test: `crates/client/src/instance.rs` inline `#[cfg(test)]` module (append)

**Interfaces:**
- Produces:
  - `ClientUpdateEvent::NewFarRegion(RegionId, FarSnapshot)`, `ClientUpdateEvent::RemoveFarRegion(RegionId)`
  - client emits `ClientPacket::RequestFarRegion`/`ReleaseFarRegion` from the far ring diff; forwards far terrain to the bridge.
- Consumes: `ServerPacket::FarRegion`, `FAR_VIEW_RADIUS` (Tasks 1–2).

- [ ] **Step 1: Write the failing test**

First inspect the existing inline test module at the bottom of `crates/client/src/instance.rs` (search for `mod tests` / `emit_hud_status_noop_without_home`) to match its `GameInstanceManager` construction style. Append a test that exercises the pure ring math:

```rust
    #[test]
    fn desired_far_is_ring_minus_near() {
        use game::{RegionCoords, FAR_VIEW_RADIUS};
        let center = RegionCoords::new(0, 0);
        let near: std::collections::BTreeSet<RegionCoords> =
            center.window_3x3().into_iter().collect();
        let far = super::desired_far(center, &near, FAR_VIEW_RADIUS);
        // Disjoint from the near window.
        assert!(far.is_disjoint(&near));
        // 5×5 footprint (25) minus the 3×3 near window (9) = 16 far regions.
        assert_eq!(far.len(), 16);
        // A ring member is present; a near member is absent.
        assert!(far.contains(&RegionCoords::new(2, 0)));
        assert!(!far.contains(&RegionCoords::new(1, 0)));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client desired_far_is_ring_minus_near`
Expected: FAIL to compile — `desired_far` doesn't exist.

- [ ] **Step 3a: Add the `ClientUpdateEvent` variants**

In `crates/game/src/lib.rs`, add to `enum ClientUpdateEvent` (after `RemoveRegion`):

```rust
    /// Read-only visual terrain for a far region (initial send or refresh —
    /// replace-on-re-receipt). Never becomes a local sim `Region`.
    NewFarRegion(RegionId, FarSnapshot),
    /// The far ring moved on: tear down this region's far render state.
    RemoveFarRegion(RegionId),
```

(`FarSnapshot` is in scope via `pub use protocol::*;` above the enum.)

- [ ] **Step 3b: Add the `desired_far` helper**

In `crates/client/src/instance.rs`, next to the existing `desired_window` free function, add:

```rust
/// The far ring: every region within `radius` (Chebyshev) of `center`, minus
/// the near window. Far and near are disjoint by construction, so a region is
/// never both simulated and merely visualized.
fn desired_far(
    center: RegionCoords,
    near: &std::collections::BTreeSet<RegionCoords>,
    radius: i32,
) -> std::collections::BTreeSet<RegionCoords> {
    let mut out = std::collections::BTreeSet::new();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let rc = RegionCoords::new(center.x + dx, center.z + dz);
            if !near.contains(&rc) {
                out.insert(rc);
            }
        }
    }
    out
}
```

- [ ] **Step 3c: Add the `far_subscribed` field**

Add to `struct GameInstanceManager` (next to `subscribed`):

```rust
    /// Far (visual-only) regions we have requested and not released.
    far_subscribed: std::collections::BTreeSet<RegionCoords>,
```

Initialize it in `GameInstanceManager::new` (next to `subscribed: std::collections::BTreeSet::new(),`):

```rust
            far_subscribed: std::collections::BTreeSet::new(),
```

- [ ] **Step 3d: Add `update_far_window` and `drop_far_region`**

Add methods to `impl GameInstanceManager` (near `update_window`):

```rust
    /// Diff the desired far ring against current far subscriptions; request
    /// the new, release the stale. Runs right after `update_window` so the
    /// near set it subtracts is already current.
    fn update_far_window(&mut self) {
        let Some(center) = self.viewer_region() else { return };
        let near = desired_window(center, self.home_region);
        let desired = desired_far(center, &near, game::FAR_VIEW_RADIUS);
        if desired == self.far_subscribed {
            return;
        }
        for rc in desired.difference(&self.far_subscribed) {
            self.server_game_send
                .send(ClientPacket::RequestFarRegion(*rc))
                .unwrap();
        }
        let stale: Vec<RegionCoords> =
            self.far_subscribed.difference(&desired).copied().collect();
        for rc in stale {
            self.server_game_send
                .send(ClientPacket::ReleaseFarRegion(rc))
                .unwrap();
            self.drop_far_region(rc);
        }
        self.far_subscribed = desired;
    }

    /// Tell the render bridge to drop a far region.
    fn drop_far_region(&mut self, rc: RegionCoords) {
        let _ = self
            .client_event_send
            .send(ClientUpdateEvent::RemoveFarRegion(rc));
    }
```

- [ ] **Step 3e: Call it from the tick**

In `handle_game_event`, in the `GameEventKind::Tick` arm, add the far-window update right after `self.update_window();`:

```rust
                self.update_window();
                self.update_far_window();
```

- [ ] **Step 3f: Handle `ServerPacket::FarRegion`**

In `handle_server`, add a match arm (after the `ServerPacket::Region` arm):

```rust
            game::ServerPacket::FarRegion(id, snap) => {
                if !self.far_subscribed.contains(&id) {
                    // Ring moved on while the snapshot was in flight.
                    self.server_game_send
                        .send(ClientPacket::ReleaseFarRegion(id))
                        .unwrap();
                    return Ok(());
                }
                self.client_event_send
                    .send(ClientUpdateEvent::NewFarRegion(id, snap))
                    .unwrap();
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p client desired_far_is_ring_minus_near`
Expected: PASS.
Run: `cargo test -p game` (exhaustiveness of the new `ClientUpdateEvent` variants across `game`)
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/game/src/lib.rs crates/client/src/instance.rs
git commit -m "feat(client): far-ring subscription policy + FarRegion receive path"
```

---

### Task 6: Render bridge — far regions

**Files:**
- Modify: `crates/client/src/renderer/bridge.rs` (`FarRegionRoots` resource, `NewFarRegion`/`RemoveFarRegion` arms, `spawn_far_region`, mutual exclusion with near roots)
- Modify: `crates/client/src/renderer/mod.rs` (register the `FarRegionRoots` resource — match how `RegionRoots` is registered)
- Test: `crates/client/src/renderer/bridge.rs` inline `#[cfg(test)]` module (append)

**Interfaces:**
- Consumes: `ClientUpdateEvent::NewFarRegion(RegionId, FarSnapshot)`, `ClientUpdateEvent::RemoveFarRegion(RegionId)` (Task 5); the existing `VoxelData` component + `queue_meshing`/`apply_meshed_chunks` pipeline.
- Produces: far regions rendered as chunk entities under a root at `world_offset()`; near/far roots mutually exclusive.

- [ ] **Step 1: Write the failing test**

Inspect the existing `#[cfg(test)]` module in `bridge.rs` (search for `region_root_sits_at_world_offset`) to reuse its App/harness setup, then append:

```rust
    #[test]
    fn far_region_roots_at_offset_with_meshable_chunks() {
        use game::{Chunk, ChunkCoords, FarSnapshot, RegionCoords};
        let mut app = bridge_test_app(); // same helper the near-region tests use
        let id = RegionCoords::new(1, -2);
        let snap = FarSnapshot {
            chunks: vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))],
        };
        send_client_update(&mut app, ClientUpdateEvent::NewFarRegion(id, snap));
        app.update();

        // Root exists at the region's world offset (256, 0, -512).
        let roots = app.world().resource::<FarRegionRoots>();
        let root = *roots.0.get(&id).expect("far root");
        let tf = app.world().get::<Transform>(root).unwrap();
        assert_eq!(tf.translation, Vec3::new(256.0, 0.0, -512.0));

        // The chunk child carries VoxelData (so queue_meshing will mesh it).
        let child = app
            .world()
            .iter_entities()
            .find(|e| e.get::<VoxelData>().is_some())
            .expect("a far chunk with VoxelData");
        assert!(child.get::<VoxelData>().is_some());
    }

    #[test]
    fn remove_far_region_despawns_root() {
        use game::{Chunk, ChunkCoords, FarSnapshot, RegionCoords};
        let mut app = bridge_test_app();
        let id = RegionCoords::new(0, 0);
        let snap = FarSnapshot {
            chunks: vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))],
        };
        send_client_update(&mut app, ClientUpdateEvent::NewFarRegion(id, snap));
        app.update();
        send_client_update(&mut app, ClientUpdateEvent::RemoveFarRegion(id));
        app.update();
        assert!(app.world().resource::<FarRegionRoots>().0.get(&id).is_none());
    }
```

> Note for the implementer: use the exact helper names the existing bridge tests already define (`bridge_test_app`, `send_client_update`, or their local equivalents). If the near tests spawn the App differently, mirror that — do not invent a new harness.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p client --lib far_region_roots_at_offset_with_meshable_chunks`
Expected: FAIL to compile — `FarRegionRoots` and the new arms don't exist.

- [ ] **Step 3a: Add the resource**

In `crates/client/src/renderer/bridge.rs`, add next to `RegionRoots`:

```rust
#[derive(Resource, Default)]
pub struct FarRegionRoots(pub BTreeMap<RegionId, Entity>);
```

Register it wherever `RegionRoots` is registered (in `crates/client/src/renderer/mod.rs`, add `.init_resource::<FarRegionRoots>()` alongside `.init_resource::<RegionRoots>()`; import `FarRegionRoots` from `bridge`).

- [ ] **Step 3b: Add `spawn_far_region`**

Add to `bridge.rs`:

```rust
/// Build the render tree for a read-only far region: a root Transform at the
/// region's world offset, one child per chunk at its region-local grid
/// position carrying `VoxelData` (meshed by the normal pipeline). No
/// rigidbody, no collider — visual only.
fn spawn_far_region(
    commands: &mut Commands,
    roots: &mut FarRegionRoots,
    id: RegionId,
    snap: &game::FarSnapshot,
) {
    let offset = id.world_offset();
    let root = commands
        .spawn((
            Transform::from_translation(Vec3::new(offset[0], offset[1], offset[2])),
            Visibility::default(),
            Name::new(format!("far region {:?}", id)),
        ))
        .id();
    for (coords, chunk) in &snap.chunks {
        commands.spawn((
            Transform::from_translation(Vec3::new(
                (coords.x * 32) as f32,
                (coords.y * 32) as f32,
                (coords.z * 32) as f32,
            )),
            Visibility::default(),
            VoxelData(game::derive_voxels(&chunk.blocks, &chunk.chisel)),
            ChildOf(root),
        ));
    }
    roots.0.insert(id, root);
}

/// Despawn a far region's render tree.
fn remove_far_region(commands: &mut Commands, roots: &mut FarRegionRoots, id: RegionId) {
    if let Some(root) = roots.0.remove(&id) {
        commands.entity(root).despawn();
    }
}
```

- [ ] **Step 3c: Handle the events + mutual exclusion**

Add `mut far_roots: ResMut<FarRegionRoots>` to `drain_client_updates`'s parameters. Then:

In the `ClientUpdateEvent::NewRegion` arm, before spawning the near root, drop any far root for the same id (near wins for a simulated region):

```rust
                remove_far_region(&mut commands, &mut far_roots, id);
```

In the `ClientUpdateEvent::RemoveRegion` arm, no change.

Add two new arms:

```rust
            ClientUpdateEvent::NewFarRegion(id, snap) => {
                // Replace-on-re-receipt (refresh) and mutual exclusion: a far
                // region never coexists with its near (simulated) root.
                remove_far_region(&mut commands, &mut far_roots, id);
                if roots.0.contains_key(&id) {
                    remove_region(&mut commands, &mut regions, &mut roots, &mut map, id);
                }
                spawn_far_region(&mut commands, &mut far_roots, id, &snap);
                info!("bridge: far region {:?} loaded", id);
            }
            ClientUpdateEvent::RemoveFarRegion(id) => {
                remove_far_region(&mut commands, &mut far_roots, id);
                info!("bridge: far region {:?} removed", id);
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p client --lib far_region`
Expected: PASS (both new bridge tests).

- [ ] **Step 5: Commit**

```bash
git add crates/client/src/renderer/bridge.rs crates/client/src/renderer/mod.rs
git commit -m "feat(client): render far regions as visual-only chunk trees"
```

---

### Task 7: Full-crate verification + manual acceptance

**Files:** none (verification only).

- [ ] **Step 1: Run the full game + client suites**

Run: `cargo test -p game`
Expected: PASS (rollback/handoff/far suites all green).
Run: `cargo test -p client`
Expected: PASS.

- [ ] **Step 2: Clippy**

Run: `just clippy`
Expected: no new warnings in the touched crates.

- [ ] **Step 3: Manual acceptance (native)**

Run the server + one client (`just run`, or the play-test recipe). Roam with the free-cam until a region leaves the 3×3 window: it must remain visible as far terrain out to the 5×5 ring edge (floor-height checkerboard steps make the boundary obvious), then disappear past the ring. Cross back: the far region promotes to a near (simulated) region without leaving two overlapping copies rendered.

- [ ] **Step 4: Manual acceptance (wasm)**

Build/run the wasm client per `CLAUDE.md` and confirm far regions render in the browser (the `LocalServer` path exercises the same manager core). `?offline` is sufficient.

- [ ] **Step 5: Commit (if any lint fixups were needed)**

```bash
git add -A
git commit -m "chore: far-region visual chunks verification fixups"
```

---

## Self-Review

**Spec coverage:**
- Data types (`FarSnapshot`, constants, extraction) → Task 1. ✓
- Wire protocol (`ClientPacket`/`ServerPacket` far variants) → Task 2. ✓
- Region actor (`RequestFarSnapshot`, `FarSnapshot`, `FarSnapshotChanged`, `Stopped` carries far, throttled push) → Task 3. ✓
- Manager (far-subscriber registry + far_cache, serve running/parked/worldgen without waking, keep-alive ignores far, park caches, disconnect cleanup) → Task 4. ✓
- Client (far-ring policy disjoint from near, bypass sim, receive path, `ClientUpdateEvent` variants) → Task 5. ✓
- Render bridge (`FarRegionRoots`, chunk trees at world offset, no collider, mutual exclusion, refresh) → Task 6. ✓
- Near↔far transitions → mutual-exclusion in Task 6 + disjoint sets in Task 5; exercised in Task 7 manual. ✓
- wasm/local server → no special-casing; verified in Task 7 Step 4. ✓
- Testing plan items (manager-core, worldgen materialization, client window diff, bridge roots, rollback bar) → Tasks 1/3/4/5/6/7. ✓
- Out-of-scope items (LOD, entities-at-distance, per-region radius, durable far_cache, worldgen/hybrid optimizations) → not implemented, by design. ✓

**Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N" — every code step shows the code. The one soft reference (Task 6 test helper names) is explicitly flagged for the implementer to match the existing bridge-test harness rather than invent one.

**Type consistency:** `FarSnapshot { chunks: Vec<(ChunkCoords, Chunk)> }` used identically in Tasks 1–6. `FarSnapshot::from_columns`/`terrain_hash`, `Region::far_snapshot`/`Rollback::far_snapshot`, `RegionInput::RequestFarSnapshot`, `RegionOutput::FarSnapshot`/`FarSnapshotChanged`/`Stopped(SerializedRegion, FarSnapshot)`, `ServerPacket::FarRegion(RegionId, FarSnapshot)`, `ClientPacket::RequestFarRegion`/`ReleaseFarRegion`, `ClientUpdateEvent::NewFarRegion`/`RemoveFarRegion`, `FarRegionRoots`, `far_subscribers`/`far_cache`/`far_subscribed`, `desired_far`/`update_far_window`/`drop_far_region` — names are consistent across every referencing task.
