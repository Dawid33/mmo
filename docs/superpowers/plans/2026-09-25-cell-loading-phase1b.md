# Cell Loading — Phase 1b (wire cell mode into the running game) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `just region_chunks=128 cells=16 world=tiny run` starts a real server + client where the home region runs in cell mode: cells load and unload around the player through `CellChanges`, the client renders them, and client/server stay bit-exact.

**Architecture:** A build-time knob `game::CELL_CHUNKS` (`VOXEL_CELL_CHUNKS`, 0 = off, like `REGION_CHUNKS`) reaches both spawners (`ThreadRegionSpawner`, `InlineSpawner`), which build cell-mode runners through one shared `RegionSeed::into_runner`. The `WorldManager` never parks a cell-mode region (its dormant store is runner-private; Postgres is phase 5). Client-side, applying `CellChanges` emits the same render updates a runtime-created chunk already sends (`CreateEntity` / `SetEntityPosition` / `SetVoxelComponent`, `RemoveEntity`), so the bridge needs no new API. Two phase-1 review debts are paid first: server regions stop retaining every event, and a client never builds a region in the server role.

**Tech Stack:** Rust; `game`, `macros`, `server`, `client` crates; crossbeam; Bevy client (type-checked only — client test binaries are an explicit slow opt-in).

**Spec:** `docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md` (§1 tickets on the server runner, §4 join snapshot = active cells, §6 incremental render updates, §7 cells replace the near window for the home region). Phase 1 plan: `docs/superpowers/plans/2026-09-25-cell-loading-phase1.md`.

## Scope

In: bounded server event log; `CELL_CHUNKS` knob + justfile/config; spawners build cell runners; no parking in cell mode; render updates for cell terrain; client region role + replay error surfacing; manager-level convergence test; server/local-server/client wiring; docs + a run recipe.

Out: island worldgen/region = island (phase 3), zstd/framing (phase 4), Postgres + wind-down (phase 5), off-thread voxel derivation for activated cells (phase 2 measures the sim-thread cost first), rendering dormant terrain inside the home region via LOD (spec §6 — needs LOD keys per cell; later), `InlineSchedule` jitter for cell events (the manager-level convergence test here is lockstep; jitter variant deferred to phase 2).

## Global Constraints

- Work on `develop` in the main checkout. **Never create a git worktree.** Many files carry the user's uncommitted work: stage ONLY your own hunks (apply them onto the HEAD version and `git update-index`), never the user's changes. Untracked user files (`crates/game/src/slime.rs`, `crates/game/src/edit.rs`, possibly others — check `git status --short <file>`) are never committed; edits to them stay in the working tree and are listed in the final report.
- `game` and `server` stay Bevy-free.
- Build/test only through `just`. Verify with `just test-one <crate> "<target> <filter>"`, `just smoke`, and `just test-threads` (this plan touches `WorldManager` and `region_threads`). Never run whole suites. **Do not build or run client test binaries** (explicit slow opt-in per CLAUDE.md); client changes are verified by `just smoke`'s type-check.
- Tests slower than 100 ms get `#[ignore = "slow: >100ms — run via just test-full"]`.
- `CELL_CHUNKS == 0` must leave every existing behaviour unchanged.
- Never write the region edge as a literal — derive from `REGION_CHUNKS`.
- Keep the justfile in sync (CLAUDE.md rule): a new build knob gets a justfile variable and doc comment.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.
- Do not launch the GUI client or benchmarks; the live run is the user's.

## Review Focus

1. **A cell-mode region with zero subscribers and no home player** → must keep running (not park, not lose its store). Test in Task 3.
2. **The join snapshot of a cell-mode region** → contains exactly the active cells; the client renders them through `NewRegion` and later cells through updates. Test (sim side) in Task 5.
3. **A key freed by a dematerialized chunk and reused by the next activation in the same event** → `RemoveEntity` must be emitted before `CreateEntity` for that key. Test in Task 4.
4. **A server-role region (no render sink)** → must emit nothing and must not derive voxels. Test in Task 4.
5. **Legacy build (`CELL_CHUNKS == 0`)** → spawners build legacy runners; parking unchanged. Test in Task 3.

## File Structure

| File | Responsibility |
|---|---|
| `crates/game/src/region.rs` (modify) | `Region::commit_server_event` |
| `crates/game/src/region_runner.rs` (modify) | use `commit_server_event`; `RegionSeed::into_runner` |
| `crates/game/src/protocol.rs` (modify) | `CELL_CHUNKS` + `parse_cell_chunks` |
| `crates/game/src/world_manager.rs` (modify) | `RegionSpawner::cell_mode`, `InlineSpawner::with_cells`, no parking in cell mode |
| `crates/macros/src/lib.rs` (modify) | `has_render_sink()` next to `send()` |
| `crates/game/src/cells.rs` (modify) | render updates in materialize/dematerialize |
| `crates/game/tests/cell_manager.rs` (create) | manager-level tests + convergence |
| `crates/server/src/region_threads.rs`, `crates/server/src/lib.rs` (modify) | server spawner cell mode |
| `crates/client/src/local_server.rs`, `crates/client/src/instance.rs`, `crates/client/src/world_scale.rs` (modify) | local server cell mode, client role, replay errors, near radius 0 in cell mode |
| `.cargo/config.toml`, `justfile`, `CLAUDE.md`, `TODO.md` (modify) | knob + docs |

---

### Task 1: Server regions stop retaining events

**Files:** Modify `crates/game/src/region.rs` (next to `forget_last_event`), `crates/game/src/region_runner.rs` (`apply`, `tick`). Test: `crates/game/tests/region_runner.rs`.

**Interfaces:** Produces `Region::commit_server_event(&mut self)` = `forget` + drop the event just pushed to `event_log`.

- [ ] **Step 1: Failing test** — append to `crates/game/tests/region_runner.rs` (adapt imports to that file's existing ones):

```rust
#[test]
fn a_server_runner_keeps_no_event_history() {
    let (out, _rx) = crossbeam::channel::unbounded();
    let mut runner = game::RegionRunner::new(
        game::RegionCoords::new(0, 0),
        game::Region::from_chunks(game::RegionCoords::new(0, 0), Vec::new()),
        out,
    );
    runner.handle_input(game::RegionInput::Event(game::GameEventKind::CreateClient(0)));
    for _ in 0..50 {
        runner.tick();
    }
    assert!(
        runner.region_mut().pending_event_ids().is_empty(),
        "server regions must not retain applied events"
    );
}
```

- [ ] **Step 2: Run** `just test-one game "--test region_runner a_server_runner_keeps"` → FAIL (51 ids retained).

- [ ] **Step 3: Implement** — `region.rs`:

```rust
    /// Server-side commit of the event just handled: forget its undo
    /// transaction AND drop it from `event_log`. Server regions never
    /// reconcile, so the log would otherwise grow by every event forever
    /// (and `CellChanges` carry whole cells).
    pub fn commit_server_event(&mut self) {
        self.data.forget();
        self.event_log.pop_back();
    }
```

In `region_runner.rs`, replace `self.region.forget_last_event();` with `self.region.commit_server_event();` in both `apply` and `tick`.

- [ ] **Step 4: Run** the test → PASS; `just test-one game "--test region_runner"` and `just test-one game "--test cell_activation -- --include-ignored"` → pass.

- [ ] **Step 5: Commit** (`fix(game): server regions drop applied events from the log`).

---

### Task 2: The `CELL_CHUNKS` build knob

**Files:** Modify `crates/game/src/protocol.rs` (after `parse_region_chunks`), `.cargo/config.toml` (`[env]`), `justfile` (after `region_chunks`).

**Interfaces:** Produces `pub const CELL_CHUNKS: usize` (0 = cell mode off) and `pub(crate) const fn parse_cell_chunks(var: Option<&str>) -> usize`.

- [ ] **Step 1: Failing test** — in `protocol.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn cell_chunks_parse() {
        assert_eq!(parse_cell_chunks(None), 0);
        assert_eq!(parse_cell_chunks(Some("")), 0);
        assert_eq!(parse_cell_chunks(Some("0")), 0);
        assert_eq!(parse_cell_chunks(Some("8")), 8);
        assert_eq!(parse_cell_chunks(Some("32")), 32);
        assert_eq!(CELL_CHUNKS, parse_cell_chunks(option_env!("VOXEL_CELL_CHUNKS")));
    }
```

- [ ] **Step 2: Run** `just test-one game "--lib protocol::tests::cell_chunks_parse"` → compile error.

- [ ] **Step 3: Implement** — `protocol.rs`:

```rust
/// Cell loading (spec 2026-09-25-island-regions-cell-loading-design): every
/// region runs in cell mode with `CELL_CHUNKS`-chunk cells when non-zero. A
/// BUILD-time dial like `REGION_CHUNKS`, for the same reason: server, client
/// and the wasm LocalServer must agree. `just cells=16 …`; 0/unset = off.
pub const CELL_CHUNKS: usize = parse_cell_chunks(option_env!("VOXEL_CELL_CHUNKS"));

/// Unset/empty/0 = off; otherwise a power of two dividing `REGION_CHUNKS`
/// (checked at BUILD time).
pub(crate) const fn parse_cell_chunks(var: Option<&str>) -> usize {
    let Some(text) = var else { return 0 };
    let bytes = text.as_bytes();
    let mut value = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii_digit(), "VOXEL_CELL_CHUNKS must be a number");
        value = value * 10 + (bytes[i] - b'0') as usize;
        i += 1;
    }
    assert!(
        value == 0 || (value.is_power_of_two() && REGION_CHUNKS % value == 0),
        "VOXEL_CELL_CHUNKS must be 0 or a power of two dividing REGION_CHUNKS"
    );
    value
}
```

`.cargo/config.toml` `[env]`: add `VOXEL_CELL_CHUNKS = "0"` under `VOXEL_REGION_CHUNKS`. `justfile`, after the `region_chunks` block:

```
# Cell loading (phase 1b of docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md):
# a BUILD-time constant (`game::CELL_CHUNKS`), 0 = off. Every region then loads
# terrain in `cells`×`cells`-chunk cells around players; must be a power of two
# dividing `region_chunks`. The client forces near radius 0 in cell mode.
#   just region_chunks=128 cells=16 world=tiny run
cells := env_var_or_default("VOXEL_CELL_CHUNKS", "0")
export VOXEL_CELL_CHUNKS := cells
```

- [ ] **Step 4: Run** the test → PASS; `just smoke` → passes.

- [ ] **Step 5: Commit** (`feat(game): VOXEL_CELL_CHUNKS build knob`).

---

### Task 3: Spawners build cell runners; cell-mode regions never park

**Files:** Modify `crates/game/src/region_runner.rs` (`RegionSeed::into_runner`), `crates/game/src/world_manager.rs` (`RegionSpawner::cell_mode`, `InlineSpawner.cell_chunks` + `with_cells`, `maintain`). Create `crates/game/tests/cell_manager.rs`.

**Interfaces:**
- `RegionSeed::into_runner(self, id, out, generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>, cell_chunks: usize) -> RegionRunner` — `cell_chunks == 0`: `RegionRunner::new(id, self.into_region_with(id, generate), out)` (today's path). Otherwise: `Fresh(columns)` / `Deferred(None)` → columns (generated if deferred) → `RegionRunner::new_with_cells(id, Region::new_cells(id, cell_chunks, None, None), out, CellStore::from_columns(columns, cell_chunks), CellConfig::new(cell_chunks))`; `Parked`/`Deferred(Some)` → `log::error!` and the legacy restore (cell regions never park, so this is unreachable in practice).
- `RegionSpawner::cell_mode(&self) -> bool { false }` (default method).
- `InlineSpawner::with_cells(cell_chunks: usize) -> Self`; its `spawn` uses `seed.into_runner(id, out, &|rc| unreachable!(...), self.cell_chunks)`; `cell_mode()` = `self.cell_chunks != 0`.
- `WorldManager::maintain` skips regions when `self.spawner.cell_mode()`.

- [ ] **Step 1: Failing tests** — create `crates/game/tests/cell_manager.rs`:

```rust
//! Cell mode through the WorldManager (phase 1b).
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, InlineLodBuilder,
    InlineSpawner, LodBuilder, RegionCoords, RegionOutput, ServerEvent, ServerPacket, SubChunk,
    WorldManager, REGION_CHUNKS, SPAWN_REGION, UNLOAD_GRACE_MS,
};

pub const CC: usize = 8;

pub struct Harness {
    pub manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

/// A flat region: one dense floor sub-chunk in every column.
pub fn harness(cell_chunks: usize) -> Harness {
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let generator = Box::new(|_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
        let mut cols = Vec::new();
        for x in 0..REGION_CHUNKS {
            for z in 0..REGION_CHUNKS {
                let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
                let air = Block::new(BlockId::AIR);
                cols.push((ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub])));
            }
        }
        cols
    });
    let (lod_result_send, lod_result_recv) = unbounded();
    let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        out: lod_result_send,
    });
    Harness {
        manager: WorldManager::new(
            InlineSpawner::with_cells(cell_chunks),
            generator,
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        ),
        region_out,
        packets,
    }
}

impl Harness {
    pub fn settle(&mut self, now_ms: u64) {
        for _ in 0..2 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now_ms);
            }
            self.manager.pump_lod_results();
        }
    }
    pub fn event(&mut self, ev: ServerEvent, now_ms: u64) {
        self.manager.handle_server_event(ev, now_ms);
        self.settle(now_ms);
    }
    pub fn tick(&mut self, now_ms: u64) {
        self.manager.spawner_mut().tick_all();
        self.settle(now_ms);
    }
    pub fn drain(&mut self) -> Vec<(Option<ClientId>, ServerPacket)> {
        self.packets.try_iter().collect()
    }
    pub fn active_cells(&mut self, rc: RegionCoords) -> usize {
        let mut n = 0;
        self.manager.spawner_mut().with_region(rc, |r| n = r.data().cells.active.len());
        n
    }
}

#[test]
fn a_joining_player_gets_a_cell_mode_home_with_its_cells_active() {
    let mut h = harness(CC);
    h.event(ServerEvent::ClientConnected(0), 0);
    let mut cell_chunks = 0;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| cell_chunks = r.data().cells.cell_chunks);
    assert_eq!(cell_chunks, CC);
    assert_eq!(h.active_cells(SPAWN_REGION), 4, "spawn cell (0,0) + clipped 3×3");
}

#[test]
fn the_join_snapshot_holds_the_active_cells() {
    let mut h = harness(CC);
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    let snap = h.drain().into_iter().find_map(|(_, p)| match p {
        ServerPacket::Region(rc, rb) if rc == SPAWN_REGION => Some(rb),
        _ => None,
    });
    let rb = snap.expect("snapshot delivered");
    assert_eq!(rb.data.cells.active.len(), 4);
    assert!(rb.data.ecs.chunk.iter().filter(|(_, c)| c.is_some()).count() > 0);
}

#[test]
fn a_cell_mode_region_never_parks() {
    let mut h = harness(CC);
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0), 0);
    h.manager.maintain(UNLOAD_GRACE_MS + 1);
    h.settle(UNLOAD_GRACE_MS + 1);
    assert!(h.manager.running_regions().contains(&rc), "cell regions keep their store: no parking");
}

#[test]
fn a_legacy_spawner_still_parks() {
    let mut h = harness(0);
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0), 0);
    h.manager.maintain(UNLOAD_GRACE_MS + 1);
    h.settle(UNLOAD_GRACE_MS + 1);
    assert!(!h.manager.running_regions().contains(&rc));
}
```

Check the exact names used by `crates/game/tests/world_manager.rs` (`ReleaseRegionConnection`, `maintain`, `running_regions`, whether `maintain` needs the `empty_since` stamp set by a prior `settle`/event at time 0) and mirror them; the legacy-parks test pins that behaviour is unchanged.

- [ ] **Step 2: Run** `just test-one game "--test cell_manager"` → compile error (`with_cells` missing).

- [ ] **Step 3: Implement**

`region_runner.rs` (imports: `CellConfig, CellStore` already partly imported; add `Column, ColumnCoords` if missing):

```rust
impl RegionSeed {
    /// Build this seed's runner: legacy (fully materialized) when
    /// `cell_chunks == 0`, else a cell-mode runner whose store holds the
    /// generated columns. Cell regions never park, so a parked seed in cell
    /// mode is unexpected and restores legacy.
    pub fn into_runner(
        self,
        id: RegionId,
        out: Sender<(RegionCoords, RegionOutput)>,
        generate: &dyn Fn(RegionCoords) -> Vec<(ColumnCoords, Column)>,
        cell_chunks: usize,
    ) -> RegionRunner {
        if cell_chunks == 0 {
            return RegionRunner::new(id, self.into_region_with(id, generate), out);
        }
        let columns = match self {
            RegionSeed::Fresh(columns) => columns,
            RegionSeed::Deferred(None) => generate(id),
            parked => {
                log::error!("cell-mode region {id:?} got a parked seed; restoring legacy");
                return RegionRunner::new(id, parked.into_region_with(id, generate), out);
            }
        };
        RegionRunner::new_with_cells(
            id,
            Region::new_cells(id, cell_chunks, None, None),
            out,
            CellStore::from_columns(columns, cell_chunks),
            CellConfig::new(cell_chunks),
        )
    }
}
```

`world_manager.rs`: add to `trait RegionSpawner`:

```rust
    /// Whether spawned regions run in cell mode (never parked: their dormant
    /// store is runner-private until persistence lands).
    fn cell_mode(&self) -> bool {
        false
    }
```

`InlineSpawner`: add field `cell_chunks: usize` (Default 0), `pub fn with_cells(cell_chunks: usize) -> Self { Self { cell_chunks, ..Self::default() } }`, in `spawn`: `let runner = seed.into_runner(id, out, &|rc| unreachable!("InlineSpawner never generates columns ({rc:?})"), self.cell_chunks);`, and `fn cell_mode(&self) -> bool { self.cell_chunks != 0 }`. `maintain`, first line: `if self.spawner.cell_mode() { return; }`.

- [ ] **Step 4: Run** `just test-one game "--test cell_manager"` → 4 passed; `just test-one game "--test world_manager"` → pass; `just test-threads` → pass.

- [ ] **Step 5: Commit** (`feat(game): spawners build cell-mode runners; cell regions never park`).

---

### Task 4: Render updates for cell terrain

**Files:** Modify `crates/macros/src/lib.rs` (next to `pub fn send` ~line 876: `has_render_sink`), `crates/game/src/cells.rs` (`materialize_cell`, `dematerialize_cell`). Test: `crates/game/tests/cell_activation.rs`.

**Interfaces:** `has_render_sink(&self) -> bool` on the same type as `send` (true iff a render channel is attached). Materialize emits, per chunk in canonical order: `CreateEntity(e)`, `SetEntityPosition(e, pose)`, `SetVoxelComponent(e, Some(derive_voxels(..)))`; dematerialize emits `RemoveEntity(e)` per chunk — all `GameDataTransactionKind::Do`, and only when `has_render_sink()`.

- [ ] **Step 1: Failing tests** — append to `cell_activation.rs`:

```rust
fn render_kinds(recv: &Receiver<game::GameDataUpdate>) -> Vec<&'static str> {
    recv.try_iter()
        .map(|u| match u.kind {
            game::GameDataUpdateKind::CreateEntity(_) => "create",
            game::GameDataUpdateKind::SetEntityPosition(..) => "pos",
            game::GameDataUpdateKind::SetVoxelComponent(..) => "voxels",
            game::GameDataUpdateKind::RemoveEntity(_) => "remove",
            _ => "other",
        })
        .collect()
}

#[test]
fn a_client_tells_the_renderer_about_activated_and_released_terrain() {
    let (send, recv) = unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    let _ = recv.try_iter().count();
    let ev = server_apply(&mut s, activate(&[CellCoords::new(1, 1)]));
    c.handle_event(ev.kind).unwrap();
    c.forget_last_event();
    // cell_content has two chunks.
    assert_eq!(render_kinds(&recv), vec!["create", "pos", "voxels", "create", "pos", "voxels"]);
    let ev = server_apply(&mut s, deactivate(&[CellCoords::new(1, 1)]));
    c.handle_event(ev.kind).unwrap();
    c.forget_last_event();
    assert_eq!(render_kinds(&recv), vec!["remove", "remove"]);
}

#[test]
fn a_reused_chunk_key_is_removed_before_it_is_recreated() {
    let (send, recv) = unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    let ev = server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    c.handle_event(ev.kind).unwrap();
    c.forget_last_event();
    let _ = recv.try_iter().count();
    // One event: release (0,0), load (2,2) — the freed keys are reused.
    let ev = server_apply(
        &mut s,
        GameEventKind::CellChanges(CellChanges {
            deactivate: vec![CellCoords::new(0, 0)],
            activate: vec![CellPayload { cell: CellCoords::new(2, 2), version: 1, materialized_hash: 0, content: cell_content(CellCoords::new(2, 2)) }],
        }),
    );
    c.handle_event(ev.kind).unwrap();
    c.forget_last_event();
    let kinds = render_kinds(&recv);
    assert_eq!(&kinds[..2], &["remove", "remove"], "removals come first: {kinds:?}");
    assert_eq!(state_hash(s.data()), state_hash(c.data()));
}

#[test]
fn a_server_region_emits_nothing() {
    // No render sink: nothing to send, and no voxels are derived.
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(1, 1)]));
    assert!(!s.data().has_render_sink());
}
```

(`GameDataUpdate`'s field may be named differently than `.kind`; check `crates/game/src/state.rs` `GameDataUpdate::new(transaction, kind)` and use its accessor.)

- [ ] **Step 2: Run** `just test-one game "--test cell_activation"` → first test FAILS (no updates), third fails to compile.

- [ ] **Step 3: Implement** — macros, next to `send`:

```rust
                /// Whether a render bridge is attached (client regions).
                /// Lets raw paths skip building updates nobody receives.
                pub fn has_render_sink(&self) -> bool {
                    self.global_log.lock().unwrap().client.is_some()
                }
```

`cells.rs` `materialize_cell`, replacing the `seed_terrain_genesis` line (it must still insert chunks in the given order):

```rust
        let chunks = payload.content.chunks.clone();
        let render = self.has_render_sink();
        let voxels: Vec<_> = if render {
            chunks.iter().map(|(_, c)| crate::derive_voxels(&c.blocks, &c.chisel)).collect()
        } else {
            Vec::new()
        };
        let coords: Vec<ChunkCoords> = chunks.iter().map(|(c, _)| *c).collect();
        self.seed_terrain_genesis(chunks);
        if render {
            let index: BTreeMap<(usize, i32, usize), EntityKey> = self
                .cell_chunk_keys(payload.cell)
                .into_iter()
                .map(|(c, k)| ((c.x, c.y, c.z), k))
                .collect();
            for (c, v) in coords.iter().zip(voxels) {
                let e = index[&(c.x, c.y, c.z)];
                let pose = IsometryReal::translation(
                    Real::from((c.x * 32) as f32),
                    Real::from((c.y * 32) as f32),
                    Real::from((c.z * 32) as f32),
                );
                for kind in [
                    GameDataUpdateKind::CreateEntity(e),
                    GameDataUpdateKind::SetEntityPosition(e, pose),
                    GameDataUpdateKind::SetVoxelComponent(e, Some(v)),
                ] {
                    self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, kind));
                }
            }
        }
```

In `dematerialize_cell`, inside the chunk loop before `remove_chunk_raw(key)`:

```rust
            if self.has_render_sink() {
                self.data.send(GameDataUpdate::new(
                    GameDataTransactionKind::Do,
                    GameDataUpdateKind::RemoveEntity(key),
                ));
            }
```

Match the exact pose/voxel types `edit.rs` `set_block_in` uses for a new chunk (it sends the same `SetEntityPosition` + `SetVoxelComponent`) — copy its expressions rather than this snippet if they differ. `has_render_sink` lives on whichever type `send` is defined for in the macro; call it the way `self.data.send(...)` is reached.

- [ ] **Step 4: Run** `just test-one game "--test cell_activation -- --include-ignored"` → all pass; `just smoke` → passes.

- [ ] **Step 5: Commit** (`feat(game): clients tell the renderer about loaded and released cells`).

---

### Task 5: A client joined through the manager converges while cells load

**Files:** Test: `crates/game/tests/cell_manager.rs`.

**Interfaces:** Consumes Tasks 1–4; `Region::new(rollback, Some(send), rc, Some(client))`, `Region::reconcile`.

- [ ] **Step 1: Write the test** — append:

```rust
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_client_joined_through_the_manager_converges_while_cells_load() {
    use game::{GameEventKind, InputEvent, Key, Region};
    game::set_hash_mode(game::HashMode::Full);
    let mut h = harness(CC);
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    let (send, _recv) = unbounded();
    let mut client: Option<Region> = None;
    let mut now = 0u64;
    let mut feed = |h: &mut Harness, client: &mut Option<Region>| {
        for (_, p) in h.drain() {
            match p {
                ServerPacket::Region(rc, rb) if rc == SPAWN_REGION => {
                    *client = Some(Region::new(rb, Some(send.clone()), rc, Some(0)));
                }
                ServerPacket::GameEvent(ev) if ev.region_id == SPAWN_REGION => {
                    if let Some(c) = client.as_mut() {
                        c.reconcile(ev).unwrap();
                    }
                }
                _ => {}
            }
        }
    };
    feed(&mut h, &mut client);
    let predict = |client: &mut Option<Region>, lead: usize| {
        let c = client.as_mut().expect("joined");
        while c.pending_event_ids().len() < lead {
            c.handle_event(GameEventKind::Tick).unwrap();
        }
    };
    let input = |h: &mut Harness, client: &mut Option<Region>, ev: InputEvent| {
        client.as_mut().unwrap().handle_event(GameEventKind::PlayerInput(0, ev.clone())).unwrap();
        let gev = game::GameEvent::new(GameEventKind::PlayerInput(0, ev), 0, SPAWN_REGION);
        h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(gev), 0), 0);
    };
    input(&mut h, &mut client, InputEvent::FpsCam(true));
    input(&mut h, &mut client, InputEvent::MouseMotion { dx: 1208.0, dy: 0.0 });
    input(&mut h, &mut client, InputEvent::Key { key: Key::KeyW, pressed: true });
    for i in 0..160 {
        predict(&mut client, 1 + i % 3);
        now += 50;
        h.tick(now);
        feed(&mut h, &mut client);
    }
    for _ in 0..8 {
        now += 50;
        h.tick(now);
        feed(&mut h, &mut client);
    }
    let c = client.as_ref().unwrap();
    assert!(c.pending_event_ids().is_empty(), "client still ahead");
    let mut server_hash = 0;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| server_hash = game::state_hash(r.data()));
    assert_eq!(server_hash, game::state_hash(c.data()), "client diverged from the server");
    assert!(c.data().cells.active.len() > 4, "cells loaded around the walking player");
}
```

Adapt `PlayerInput` routing to what the manager expects (`world_manager.rs` routes `PlayerInput` by `homes`; if it wants `ClientPacket::GameEventAfterTick`, use that with the client's current tick). If borrowck rejects the closures, inline them as functions taking `&mut`.

- [ ] **Step 2: Run** `just test-one game "--test cell_manager -- --include-ignored"` → pass. Mutation check (then revert): make `Region`'s `CellChanges` arm skip materialization when `local_client_id.is_some()`; the test must fail with "client diverged".

- [ ] **Step 3: Commit** (`test(game): manager-joined client converges while cells load`).

---

### Task 6: Wire the server, the local server and the client

**Files:** Modify `crates/server/src/region_threads.rs` (`ThreadRegionSpawner` field `cell_chunks`, `.with_cells(cc)` builder, `spawn` via `seed.into_runner`, `cell_mode`), `crates/server/src/lib.rs:338` (`ThreadRegionSpawner::with_generator(region_generator).with_cells(game::CELL_CHUNKS)` + an `info!` of the mode), `crates/client/src/local_server.rs:69` (`InlineSpawner::with_cells(game::CELL_CHUNKS)`), `crates/client/src/instance.rs:762` (client role) and `:1326` (replay errors), `crates/client/src/world_scale.rs` (near radius 0 in cell mode).

**Interfaces:** `ThreadRegionSpawner::with_cells(self, cell_chunks: usize) -> Self`.

- [ ] **Step 1: Implement**

`region_threads.rs`: add `cell_chunks: usize` to the struct (0 in `with_generator` and `Default`), `pub fn with_cells(mut self, cell_chunks: usize) -> Self { self.cell_chunks = cell_chunks; self }`, capture `let cell_chunks = self.cell_chunks;` before `thread::spawn`, and replace the two lines building `region` + `RegionRunner::new(id, region, out)` with

```rust
                let runner = seed.into_runner(id, out, &|rc| {
                    let started = Instant::now();
                    let columns = match &generator {
                        Some(generate) => generate(rc),
                        None => unreachable!("Deferred seed without a region generator"),
                    };
                    generate_time.set(started.elapsed());
                    columns
                }, cell_chunks);
```

keeping the timing/perf lines (compute `build` after `into_runner`); add `fn cell_mode(&self) -> bool { self.cell_chunks != 0 }`.

`instance.rs:762` — never build a client region in the server role:

```rust
        let Some(client_id) = self.client_id else {
            // PlayerRegion always precedes region snapshots; a region built
            // without our id would act as the server (cell-hash checks off).
            log::error!("region {id:?} snapshot before PlayerRegion: dropped");
            return;
        };
        let data = Region::new(raw_game_data, Some(send), id, Some(client_id));
```

(move the `NewRegion` send below this guard so the bridge never gets a region the sim dropped).

`instance.rs:1326` replay loop: replace `if let Ok(stats) = world.reconcile_event(ev) { … }` with a `match` that keeps the `Ok` body and on `Err(e)` does `warn!("replayed event failed to reconcile: {e:?}");` and, for `GameError::CellHashMismatch(_)`, `return Err(e)` (hard fail, as the live path does). Check the enclosing function's return type and propagate accordingly.

`world_scale.rs`: where the near radius is resolved, when `game::CELL_CHUNKS != 0` force radius 0 and `log::info!("cell mode: near radius 0 (cells replace the near window)")`.

- [ ] **Step 2: Verify**

Run: `just smoke` → passes (type-checks server + client).
Run: `just test-threads` → passes.
Run: `just test-one server "--lib"` → passes.
Build the cell-mode binaries once to prove the knob compiles end-to-end: `just region_chunks=128 cells=16 build` → succeeds. (Do NOT launch them.)

- [ ] **Step 3: Commit** (`feat: run the server, local server and client in cell mode behind VOXEL_CELL_CHUNKS`).

---

### Task 7: Docs

**Files:** `CLAUDE.md` (the "Cell loading" sentence in the `crates/game` bullet + the build-knob paragraph near "Region size"), `TODO.md`.

- [ ] **Step 1:** CLAUDE.md — extend the cell-loading sentence: "Phase 1b wired it end to end behind the build knob `VOXEL_CELL_CHUNKS` (`just region_chunks=128 cells=16 world=tiny run`): spawners build cell runners via `RegionSeed::into_runner`, cell regions never park (no persistence yet), the client forces near radius 0 and renders cells through the ordinary `CreateEntity`/`SetEntityPosition`/`SetVoxelComponent`/`RemoveEntity` updates; dormant terrain inside the home region is not rendered yet." Add `cell_manager` to the tests line.
- [ ] **Step 2:** TODO.md — replace the phase line with: "phase 1 + 1b DONE (cell mode runs live behind `cells=`); next: user live run, then phase 2 (measure activation cost — sim-thread voxel derivation on the client, genesis per cell — then cheaper activation)."
- [ ] **Step 3:** `just smoke` → passes. Commit (`docs: cell mode knob and status`).
