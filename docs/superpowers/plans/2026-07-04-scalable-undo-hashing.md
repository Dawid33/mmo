# Scalable Undo Hashing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make rollback hashing cost O(mutated data) instead of O(total world data) — a cached content hash in the vendored parry `Voxels` shape — plus server-loop hygiene (no boot tick backlog, bounded tick queue, bounded undo log), unblocking the parked `microvoxel-scale` branch.

**Architecture:** `Voxels` gains a `cached_hash: u32` maintained by its one real constructor (`new`) and its content mutators (`set_voxel`, `crop`); a manual `impl Hash` feeds only `voxel_size` + the cache, so the rollback machinery's per-tick `pre_hash` and per-rollback verification stop walking voxel contents. The cache is content-derived and deterministic (sorted chunk-key iteration, FNV-1a over `Hash`), so the bit-exact rollback bar and cross-machine comparisons keep their meaning. Serde stays derived — the cache serializes (deviation from spec's skip-and-recompute: every mutation path maintains the invariant, and snapshots only travel between identical binaries; noted in the spec).

**Tech Stack:** Vendored parry fork (`enhanced-determinism`: `chunk_headers` is an insertion-ordered `ordermap::OrderMap`, hence sorted-key iteration for canonicality). Build: `~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins`. Tests: `cargo test -p game`, `cargo test -p client`.

**Spec:** `docs/superpowers/specs/2026-07-04-scalable-undo-hashing-design.md`

## Global Constraints

- Work on a feature branch off `develop` (e.g. `scalable-undo-hashing`).
- Rollback bar unchanged: `hash(before) == hash(after undo)`, bit-exact; the hash function may change but must stay deterministic, content-based, and identical cross-machine.
- The content hash must not depend on map iteration order or free-list state (`free_chunks`, chunk allocation order) — only on (chunk key → voxel states) content and `voxel_size`.
- Fork changes limited to `crates/parry/src/shape/voxels/` (consistent with existing rollback-support patches like `revert_insert`).
- Existing suites stay green: `cargo test -p game`, `cargo test -p client`.
- The multi-client suite's runtime should drop dramatically (from ~6 s); note observed timing in the report.

---

### Task 1: Cached content hash in `Voxels`

**Files:**
- Modify: `crates/parry/src/shape/voxels/voxels.rs` (struct field, derive change, manual `Hash`, FNV hasher, recompute fn, `new`)
- Modify: `crates/parry/src/shape/voxels/voxels_edition.rs` (`set_voxel`, `crop`)
- Test: `crates/game/tests/voxels_hash_cache.rs` (new)

**Interfaces:**
- Produces: `Voxels` hashes in O(1) w.r.t. voxel count; `Voxels::recompute_cached_hash(&mut self)` (pub(super)); no public API change.

- [ ] **Step 1: Write the tests**

Create `crates/game/tests/voxels_hash_cache.rs`:

```rust
//! Invariants for the cached content hash on the vendored parry Voxels
//! shape. See docs/superpowers/specs/2026-07-04-scalable-undo-hashing-design.md
use std::hash::{Hash, Hasher};
use std::time::Instant;

use game::parry::math::Real;
use game::parry::shape::Voxels;
use game::na::{Point3, Vector3};

fn vhash(v: &Voxels) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn cube(n: i32) -> Voxels {
    let mut coords = Vec::new();
    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                coords.push(Point3::new(x, y, z));
            }
        }
    }
    Voxels::new(Vector3::repeat(Real::from(1.0)), &coords)
}

#[test]
fn hash_stable_across_clone_and_serde() {
    let v = cube(8);
    assert_eq!(vhash(&v), vhash(&v.clone()));
    let bytes = bincode::serialize(&v).unwrap();
    let de: Voxels = bincode::deserialize(&bytes).unwrap();
    assert_eq!(vhash(&v), vhash(&de));
}

#[test]
fn hash_tracks_content_edits_exactly() {
    let mut v = cube(8);
    let h0 = vhash(&v);
    // Removing a voxel changes the hash...
    v.set_voxel(Point3::new(3, 3, 3), false);
    let h1 = vhash(&v);
    assert_ne!(h0, h1);
    // ...and restoring the same content restores the same hash
    // (content-based, not history-based — required by the rollback bar).
    v.set_voxel(Point3::new(3, 3, 3), true);
    assert_eq!(h0, vhash(&v));
    // Different content, different hash (probabilistically).
    let mut w = cube(8);
    w.set_voxel(Point3::new(0, 0, 0), false);
    assert_ne!(vhash(&v), vhash(&w));
}

#[test]
fn hashing_is_cheap_regardless_of_voxel_count() {
    // 64^3 = 262k voxels. Pre-fix, one Hash walk costs ~1 ms in debug
    // (measured via perf on the live server); 2000 walks would exceed 1 s
    // by a wide margin. Post-fix a walk is a few field hashes.
    let v = cube(64);
    let t = Instant::now();
    let mut acc = 0u64;
    for _ in 0..2000 {
        acc ^= vhash(&v);
    }
    assert!(acc != 0, "keep the loop from being optimized out");
    assert!(
        t.elapsed().as_millis() < 500,
        "2000 hashes took {:?} — Voxels::hash still walks contents",
        t.elapsed()
    );
}
```

- [ ] **Step 2: Run tests to verify the canary fails**

Run: `cargo test -p game --test voxels_hash_cache -- --nocapture`
Expected: `hash_stable_across_clone_and_serde` and `hash_tracks_content_edits_exactly` PASS already (derive-Hash is content-based too); `hashing_is_cheap_regardless_of_voxel_count` FAILS with a duration ≥ 1 s. The canary is the requirement under test. (If `set_voxel` is not visible: it is `pub` in `voxels_edition.rs`; check the `game::parry` re-export path compiles — adjust imports, not the assertion.)

- [ ] **Step 3: Implement in the fork**

In `crates/parry/src/shape/voxels/voxels.rs`:

Change the derive (remove `Hash`) and add the field:

```rust
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde-serialize", derive(Serialize, Deserialize))]
pub struct Voxels {
    /// A BVH of chunk keys.
    ///
    /// The bounding boxes are the ones of the chunk’s voxels **keys**. This is equivalent to a bvh
    /// of the chunks with a uniform voxel size of 1.
    pub(super) chunk_bvh: Bvh,
    pub(super) chunk_headers: HashMap<Point<i32>, VoxelsChunkHeader>,
    pub(super) chunk_keys: Vec<Point<i32>>,
    pub(super) chunks: Vec<VoxelsChunk>,
    pub(super) free_chunks: Vec<usize>,
    pub(super) voxel_size: Vector<Real>,
    /// Content hash over (sorted chunk key → voxel states), maintained by
    /// every constructor/mutator. Lets `Hash` be O(1) in voxel count so the
    /// rollback machinery's per-tick hashing doesn't walk voxel contents.
    pub(super) cached_hash: u32,
}
```

Below the struct, the manual `Hash` and the canonical recompute:

```rust
impl core::hash::Hash for Voxels {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        // Content is summarized by cached_hash; free lists, chunk allocation
        // order, and BVH layout are deliberately excluded (they are not
        // semantic content — but note they ARE part of `Clone`/serde state,
        // so exact-restore invariants are unaffected).
        self.voxel_size.hash(state);
        self.cached_hash.hash(state);
    }
}

/// Minimal FNV-1a, used only for the canonical content hash below.
/// Deterministic across platforms (no_std, no external deps).
struct ContentHasher(u64);

impl core::hash::Hasher for ContentHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

impl Voxels {
    /// Recompute the cached content hash. Iterates chunk keys in sorted
    /// order so the result is independent of map iteration order and of
    /// the chunk allocation history.
    pub(super) fn recompute_cached_hash(&mut self) {
        let mut keys: alloc::vec::Vec<Point<i32>> =
            self.chunk_headers.keys().copied().collect();
        keys.sort_unstable_by_key(|p| (p.x, p.y, p.z));
        let mut h = ContentHasher(0xcbf29ce484222325);
        for key in keys {
            core::hash::Hash::hash(&key, &mut h);
            let id = self.chunk_headers[&key].id;
            core::hash::Hash::hash(&self.chunks[id].states[..], &mut h);
        }
        self.cached_hash = core::hash::Hasher::finish(&h) as u32;
    }
}
```

(If `alloc::vec::Vec` isn't already in scope in this file, use whatever `Vec` path the file already imports — it constructs `Vec`s in `new` today.)

In `Voxels::new`, add the field to the literal and recompute at the end:

```rust
        let mut result = Self {
            chunk_bvh: Bvh::new(),
            chunk_headers: HashMap::default(),
            chunk_keys: vec![],
            chunks: vec![],
            free_chunks: vec![],
            voxel_size,
            cached_hash: 0,
        };
        // ... existing body unchanged ...
        result.recompute_all_voxels_states();
        result.recompute_cached_hash();
        result
```

In `crates/parry/src/shape/voxels/voxels_edition.rs`:
- `set_voxel`: the early `return VoxelState::EMPTY;` for the already-empty
  case stays untouched (no content change). Every path that actually flips
  a voxel gets `self.recompute_cached_hash();` immediately before its
  return. Read the whole function and cover each mutating exit.
- `crop`: append `self.recompute_cached_hash();` at the end of the function
  (it removes chunks wholesale).
- Audit for other content mutations: `grep -n "chunks\[\|chunk_headers\." crates/parry/src/shape/voxels/voxels_edition.rs` — any site that writes voxel states must be followed by a recompute before the shape can be observed. (`set_voxel_size` and `scaled` change only `voxel_size`, which the manual `Hash` covers directly — no recompute.)

- [ ] **Step 4: Run the fork + game suites**

Run: `cargo test -p game && cargo check --workspace`
Expected: all `voxels_hash_cache` tests PASS including the canary (< 50 ms typical); the whole game suite PASSES — `hash_restore`, `random_ops`, and the multi-client suite exercise snapshot/rollback paths over the new hash. **Report the multi-client suite wall time** — it should drop well below the ~6 s it cost after the terrain-collider work.

- [ ] **Step 5: Amend the spec's serde note and commit**

In `docs/superpowers/specs/2026-07-04-scalable-undo-hashing-design.md`, replace the serde bullet ("`#[serde(skip)]` the cache and recompute on deserialize...") with:

```markdown
- Serde: the cache field serializes with the derive (deviation from the
  earlier skip-and-recompute idea: every mutation path maintains the
  invariant, snapshots only travel between identical binaries, and keeping
  the derive minimizes fork surface).
```

```bash
git add crates/parry/src/shape/voxels/ crates/game/tests/voxels_hash_cache.rs docs/superpowers/specs/2026-07-04-scalable-undo-hashing-design.md
git commit -m "perf(parry): cached content hash on Voxels - O(1) Hash for rollback machinery"
```

---

### Task 2: Server loop hygiene

**Files:**
- Modify: `crates/server/src/main.rs` (tick thread placement + coalescing, per-tick forget)

**Interfaces:**
- Consumes: nothing from Task 1 (independent, but sequenced after so the suite timing in Task 1 Step 4 is attributable).
- Produces: behavior only — no API change.

- [ ] **Step 1: Move and gate the tick thread**

In `main()`, delete the current tick-thread block (it sits above `let mut world = World::basic();`):

```rust
    let tick_rate = Arc::new(AtomicU64::new(game::TICK_RATE));
    let tick_thread_tick_rate = tick_rate.clone();
    let tick_thread_send = client_packet_send.clone();
    // Handle game loop
    std::thread::spawn(move || loop {
        // TODO: Sync ticks with server.
        tick_thread_send.send(ServerEvent::ServerTickTimer).unwrap();
        let rate = tick_thread_tick_rate.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(rate));
    });
```

and re-insert it AFTER `let mut world = World::basic();` in this form:

```rust
    // Tick generation starts only after the world exists (no boot backlog)
    // and never runs more than one tick ahead of processing, so client
    // packets are never starved behind a tick pileup.
    let tick_rate = Arc::new(AtomicU64::new(game::TICK_RATE));
    let pending_ticks = Arc::new(AtomicU64::new(0));
    let tick_thread_tick_rate = tick_rate.clone();
    let tick_thread_pending = pending_ticks.clone();
    let tick_thread_send = client_packet_send.clone();
    std::thread::spawn(move || loop {
        if tick_thread_pending.load(Ordering::SeqCst) < 1 {
            tick_thread_pending.fetch_add(1, Ordering::SeqCst);
            tick_thread_send.send(ServerEvent::ServerTickTimer).unwrap();
        }
        let rate = tick_thread_tick_rate.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(rate));
    });
```

(`let mut world = World::basic();` currently sits below the channel setup; keep `results_buffer` where it is. `tick_rate` stays in scope for the `SyncClock` arm.)

- [ ] **Step 2: Consume the pending marker and forget tick transactions**

In the `ServerEvent::ServerTickTimer` arm:

```rust
            ServerEvent::ServerTickTimer => {
                pending_ticks.fetch_sub(1, Ordering::SeqCst);
                world.progress_world_one_tick(&mut results_buffer);
                for (id, result) in &results_buffer {
                    server_send
                        .send((None, ServerPacket::GameEvent(result.as_ref().unwrap().clone())))
                        .unwrap();
                    if world.current_tick(&ChunkCoords::new(0, 0, 0)) % 10 == 0 {
                        server_send
                            .send((
                                None,
                                ServerPacket::SyncClock(
                                    *id,
                                    tick_rate.load(Ordering::SeqCst),
                                    world.current_tick(&id),
                                    Duration::new(0, 0),
                                ),
                            ))
                            .unwrap();
                    }
                }
                // The server never rolls back: drop each tick's transaction
                // immediately so the undo log and memory stay bounded.
                for id in results_buffer.keys() {
                    world.forget_last_event(id);
                }
            }
```

- [ ] **Step 3: Verify build + suites**

Run: `cargo check --workspace && cargo test -p game 2>&1 | grep -cE "^test result: ok"`
Expected: clean check; 10 green game test binaries (9 existing + voxels_hash_cache).

- [ ] **Step 4: Commit**

```bash
git add crates/server/src/main.rs
git commit -m "fix(server): tick thread starts after boot, coalesces, and forgets tick transactions"
```

---

### Task 3: Verification on develop (old scale) + finish branch

**Files:** none (verification only).

- [ ] **Step 1: Full build and suites**

```bash
~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins
cargo test -p game
cargo test -p client
```
Expected: all green.

- [ ] **Step 2: Live join-latency check**

```bash
./target/debug/server > /tmp/mmo-server.log 2>&1 &
sleep 1   # deliberately join almost immediately after server start
WGPU_ADAPTER_NAME="6700" ./target/debug/client > /tmp/mmo-client1.log 2>&1 &
```

Verify: client 1's log shows "Region recieved and loaded!" within a couple of seconds of "connected"; movement works (E + WASD; floor still solid). Stop both processes.

- [ ] **Step 3: Finish the branch**

superpowers:verification-before-completion, then superpowers:finishing-a-development-branch (merge to `develop` on user choice).

---

### Task 4: Un-park `microvoxel-scale` — the real acceptance gate

**Files:** branch operation + live verification (no new code expected).

- [ ] **Step 1: Rebase the parked branch onto the merged develop**

```bash
git checkout microvoxel-scale
git rebase develop
cargo test -p game && cargo test -p client
```
Expected: clean rebase (the branch touches `voxel.rs`/`state.rs`/`camera.rs` constants and world gen; this work touched parry + server main — no overlap except possibly `state.rs` context lines); all suites green. Report the multi-client suite time at 64-chunk scale — expect ~1 s-ish, not 6 s.

- [ ] **Step 2: The previously-failing live gate**

```bash
~/Software/rustc_codegen_cranelift/dist/cargo-clif build --workspace --bins
./target/debug/server > /tmp/mmo-server.log 2>&1 &
sleep 3
WGPU_ADAPTER_NAME="6700" ./target/debug/client > /tmp/mmo-client1.log 2>&1 &
sleep 6
WGPU_ADAPTER_NAME="6700" ./target/debug/client > /tmp/mmo-client2.log 2>&1 &
```

Verify:
- Server boots fast (probe earlier: World::basic was ~5 s pre-fix from O(N²) hashing; expect well under 1 s now).
- **Both clients log "Region recieved and loaded!" within a few seconds** — this exact step failed before (never loaded).
- Two players on the 16 m microvoxel floor, each ~29 voxels tall, movement and collision working, no panics / hash failures / reconcile spam in any log.
- Note join-to-loaded latency for the 9 MB snapshot in the report.

- [ ] **Step 3: Finish the microvoxel branch**

superpowers:finishing-a-development-branch for `microvoxel-scale` (merge to `develop` on user choice).
