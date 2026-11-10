# Stud Chunks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A second chunk content type — the **stud chunk**, toy bricks on a stud/plate grid — runs through sim, server, network and client, with a 3×3-region all-stud debug world (`just run-studs`) you can look at and build in.

**Architecture:** `game::stud` holds the data (`StudChunk` = canonical `BTreeMap<min, StudBrick>`) and derives its colliders (parry `Voxels` with `(8, 1.6, 8)` fine cells + `(32, 48, 32)` coarse cells + one convex hull per non-box part). A stud chunk is its own ECS entity (`Ecs.stud`, `GameData.stud_index`) with one fixed body, built raw at genesis and undo-safely at runtime; edits are two new `Order`s. The server's `studs` world source seeds stud chunks into fresh regions through a spawner hook; the client mirrors them, meshes them from baked LDraw triangles, and feeds the height grid. LDraw is read only by an offline bake tool.

**Tech Stack:** Rust; rapier/parry forks (`Voxels`, `ConvexPolyhedron`); `weldr` 0.3.1 (tool only); Bevy 0.19.1 (`Mesh`, `StandardMaterial`); bincode via `game::encode_packet`; RON.

**Spec:** `docs/superpowers/specs/2026-11-08-stud-chunks-design.md` (every design decision; read it). Context handoff: `docs/superpowers/plans/2026-11-09-stud-chunks-plan-handoff.md` (code map, verified 2026-11-08; this plan re-verified the line numbers on 2026-11-09).

## Global Constraints

- **Do not create git worktrees**; work on `develop` in place. The tree has many unrelated uncommitted user edits (client, server roles, deploy, docs, `crates/server/tests/roles.rs`): never touch, revert, reformat or stage them. Stage only the files your task lists (`git add <paths>`, never `-A`/`.`). Commit only if the user asked for commits in this session; otherwise leave changes unstaged and say so.
- Name everything toy-brick `stud` (`StudChunk`, `StudBrick`, …). "Brick" already means the 32³ render unit (`BRICK_VOXELS`).
- Anti-goal: **no change** to voxel `Chunk`, its serialization, the store schema, worldgen columns or voxel rendering. A voxel world plays exactly as before except for state-hash values.
- Grid (exact values): **1 stud = 8 units = 0.5 m**; **1 plate = 16/5 = 3.2 units = 0.2 m**; **1 brick = 3 plates = 9.6 units**; stud chunk **32 × 75 × 32** (studs × plates × studs) = **256 × 240 × 256 units**; body origin `(x·256, y·240, z·256)`; 1 LDU = 0.4 units; LDraw −Y is up.
- Plate heights are always computed by one formula from integer plates: `(plates * 16) as f32 / 5.0` units. Never accumulate 3.2.
- Rollback/determinism (CLAUDE.md): `BTreeMap` only in sim state; no inherent `f32` transcendentals in sim paths (use `libm`); hashing via `StateHash`; every runtime mutation is undo-safe and passes `hash(before) == hash(after undo)`.
- `create_entity_safe` / `remove_entity_safe` slot lists are maintained by hand; `carry_components` (`migrate.rs`) and `portable::classify` destructure exhaustively on purpose — classify, never add `..`.
- Tests: `just test-one <crate> "<args>"` for exactly the touched tests; game integration tests go in the `it` binary (`--test it <file>::`) unless they touch process-global state. Tag tests >100 ms `#[ignore = "slow: >100ms — run via just test-full"]`. **Never run whole suites.** `just smoke` after client work. **Running client tests is opt-in: ask the user first.** Ask before launching anything GPU-heavy.
- `weldr` (and its cgmath 0.17 / nom 5) must never enter the `game`, `server` or `client` dependency trees.
- Play recipes keep `region_blocks` 256 (`play_blocks`); do not change it.
- New justfile recipes (`ldraw-bake`, `run-studs`) are added in the same task that needs them.
- Every new client resource / system `Local` / entity root is registered with `add_session_reset` / `SessionLocal` or tagged `SessionScoped` (`session.rs`).

## Review Focus

1. **A brick placed across a chunk edge or above plate 75** — the order is a silent no-op (never a panic, never a half-placed brick), on both the predicting client and the server. Pinned in Task 4 (`place_refuses_*`) and Task 8 (`place_order_across_chunk_edge_is_noop`).
2. **Removing the last brick of a chunk, then placing again** — the entity stays, colliders go disabled then come back, and undo of either restores the hash exactly. Pinned in Task 6 (`remove_to_empty_then_place_rolls_back`).
3. **The player spawns over stud terrain** — the voxel-only `player_spawn_y` would put a 2.4 m capsule at 1.6 m, inside 2–13 m of bricks. Pinned in Task 6 (`player_spawns_above_stud_surface`).
4. **A region rejoined from parking** — stud chunks and the body scale travel in the serialized `Rollback`; a parked-and-restored stud region must keep its bricks and not regenerate. Pinned in Task 10 (`parked_stud_region_keeps_its_edit`).
5. **Load time with 2 304 stud chunks** — main-thread meshing must be budgeted per frame or the first frame freezes for seconds. Pinned in Task 12 (`mesher_respects_frame_budget`) and measured in Task 16.

---

## File structure

| File | Responsibility |
|---|---|
| `tools/ldraw-bake/` (new workspace member) | Offline LDraw → committed assets. `src/main.rs` (CLI), `src/flatten.rs` (sub-file flatten, BFC, colour 16/24), `src/classify.rs` (Face/Stud/Inner tags, catalog cells, hulls), `src/emit.rs` (writes the four outputs), `parts.txt` (the part list). |
| `crates/game/src/stud_assets.rs` (new) | Serde types shared by the bake tool and the client: `StudPartDef`, `BakedParts`, `BakedPart`, `BakedLod`, `BakedTri`, `BakedEdge`, `TriTag`, `EdgeTag`, `FaceDir`, `TriColor`, `StudColor`, `Finish`. No logic. |
| `crates/game/src/stud_catalog.rs` (generated, committed) | `STUD_PARTS: &[StudPartDef]`, `STUD_COLORS: &[u16]`. Never hand-edited. |
| `crates/game/src/stud.rs` (new) | Units, `StudChunkCoords`, `StudShape`, `StudBrick`, `StudChunk`, `StudError`, `StudOccupancy`, rotation, tops, ray pick. Pure. |
| `crates/game/src/stud_collider.rs` (new) | `StudColliderCells`, `stud_collider_cells`, `stud_hulls`, cell-size constants. Pure. |
| `crates/game/src/stud_ops.rs` (new) | `impl Rollback`: `seed_stud_genesis`, `create_stud_chunk_safe`, `set_stud_chunk_safe`, `place_stud_safe`, `remove_stud_safe`, collider sync/patch, `stud_surface_y`. |
| `crates/game/src/movement.rs` (modify) | `BodyScale` and the `_scaled` movement entry points. |
| `crates/worldgen/src/stud_world.rs` (new) | `region_studs(RegionCoords) -> Vec<(StudChunkCoords, StudChunk)>`. |
| `crates/server/src/world_file.rs`, `lib.rs`, `region_threads.rs` (modify) | `WorldSource::Studs`, genesis hook, LOD opt-out. |
| `crates/client/src/renderer/stud/` (new dir) | `mod.rs` (`StudPlugin`, `SimStudChunk`, assets load), `mesh.rs` (pure mesh building), `systems.rs` (remesh queue, stud LOD), `build.rs` (build mode). |

---

### Task 1: LDraw bake tool and baked assets

Runs in parallel with Task 2. Everything else depends on this task's `stud_assets.rs` and `stud_catalog.rs`.

**Files:**
- Create: `tools/ldraw-bake/Cargo.toml`, `tools/ldraw-bake/parts.txt`, `tools/ldraw-bake/src/{main,flatten,classify,emit}.rs`
- Create: `crates/game/src/stud_assets.rs`, `crates/game/src/stud_catalog.rs` (generated)
- Create (generated): `assets/studs/parts.bin`, `assets/studs/colors.ron`, `assets/studs/ATTRIBUTION.md`
- Modify: `Cargo.toml` (workspace `members` += `"tools/ldraw-bake"`), `crates/game/src/lib.rs` (`pub mod stud_assets; pub mod stud_catalog;` — **no glob re-export** of either), `justfile` (recipe `ldraw-bake`)

**Interfaces:**
- Produces (`game::stud_assets`, all `#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]`):
  - `pub struct StudPartDef { pub number: &'static str, pub name: &'static str, pub size: [u8; 3], pub studs: &'static [[u8; 3]], pub hull: &'static [[i16; 3]] }` (not serde; a const table row). `size` = unrotated bounding cells (studs x, plates y, studs z). `studs` = cells `[x, size[1], z]` (the cell *above* the top) that carry a top stud. `hull` = convex-hull points in LDU relative to the bounding-box min corner, Y up; **empty = box part** (collides as its cells).
  - `pub struct BakedParts { pub version: u32, pub parts: Vec<BakedPart> }` — `parts[i]` ↔ `STUD_PARTS[i]`.
  - `pub struct BakedPart { pub number: String, pub hi: BakedLod, pub lo: BakedLod }`
  - `pub struct BakedLod { pub tris: Vec<BakedTri>, pub edges: Vec<BakedEdge> }`
  - `pub struct BakedTri { pub pos: [[f32; 3]; 3], pub normal: [f32; 3], pub color: TriColor, pub tag: TriTag }` — positions in **sim units**, part-local, origin at the bbox min corner, Y up, CCW front faces.
  - `pub enum TriColor { Own, Edge, Fixed(u16) }` (`Own` = LDraw 16, `Edge` = 24).
  - `pub enum FaceDir { PosX, NegX, PosY, NegY, PosZ, NegZ }` (+ `Copy, Eq, Ord`).
  - `pub enum TriTag { Face(FaceDir), Stud([u8; 3]), Inner }`
  - `pub struct BakedEdge { pub a: [f32; 3], pub b: [f32; 3], pub tag: EdgeTag }`; `pub enum EdgeTag { Faces(u8 /* bit per FaceDir as u8 */), Stud([u8; 3]), Inner }`.
  - `pub struct StudColor { pub code: u16, pub name: String, pub rgb: [u8; 3], pub edge: [u8; 3], pub alpha: u8, pub finish: Finish }`; `pub enum Finish { Solid, Transparent, Chrome, Pearl, Metal, Rubber, Other }`.
  - `pub const BAKED_PARTS_VERSION: u32 = 1;`
- Produces (`game::stud_catalog`, generated): `pub const STUD_PARTS: &[StudPartDef]` (in `parts.txt` order — **append-only**: part indices are stored in sim state and orders), `pub const STUD_COLORS: &[u16]` (sorted ascending, every `!COLOUR` code in `LDConfig.ldr`).

- [ ] **Step 1: Bootstrap the game-side files**

Write `stud_assets.rs` with the types above and a hand stub `stud_catalog.rs` (`pub const STUD_PARTS: &[StudPartDef] = &[]; pub const STUD_COLORS: &[u16] = &[];`) with the header comment `// @generated by tools/ldraw-bake (just ldraw-bake). Do not edit.` The tool depends on `game` (for these types and `encode_packet`), so the stub must exist first.

- [ ] **Step 2: Create the tool crate**

`tools/ldraw-bake/Cargo.toml`: edition 2024, `publish = false`, deps `weldr = "0.3.1"`, `game = { workspace = true }`, `ron`, `serde`, `anyhow`. `parts.txt`, one part number per line, in this order:

```
3005 3004 3622 3010 3009 3008 3003 3002 3001 2456 3007
3024 3023 3623 3710 3666 3460 3022 3021 3020 3795 3034
3040 3039
```

(one per line in the file). `3023` and `3040` are `~Moved to` aliases: the bake follows the alias (→ `3023b`, `3040b`) but **keeps the listed number** in `StudPartDef::number`.

- [ ] **Step 3: Write the failing bake tests** in `tools/ldraw-bake/src/classify.rs` (`#[cfg(test)]`, reading `LDRAW_DIR` or `assets/ldraw`; skip with an `eprintln!` when the library is absent):

```rust
#[test]
fn brick_2x4_matches_ldraw() {
    let p = bake_part(&lib(), "3001").unwrap();
    assert_eq!(p.def.size, [4, 3, 2]);              // 80 x 24 x 40 LDU
    assert_eq!(p.def.studs.len(), 8);
    assert!(p.def.studs.iter().all(|s| s[1] == 3));
    assert!(p.def.hull.is_empty());                   // box part
    let bb = bbox(&p.hi.tris, |t| !matches!(t.tag, TriTag::Stud(_)));
    assert_eq!(bb, ([0.0, 0.0, 0.0], [32.0, 9.6, 16.0])); // sim units, within 1e-4
    assert!(p.hi.tris.iter().filter(|t| matches!(t.tag, TriTag::Stud(_))).count()
        > p.lo.tris.iter().filter(|t| matches!(t.tag, TriTag::Stud(_))).count());
}
#[test]
fn plate_1x2_is_one_plate_tall() { assert_eq!(bake_part(&lib(), "3023").unwrap().def.size, [2, 1, 1]); }
#[test]
fn slope_has_hull_and_one_stud_row() {
    let p = bake_part(&lib(), "3040").unwrap();
    assert!(!p.def.hull.is_empty());
    assert_eq!(p.def.studs.len(), 1);
}
#[test]
fn every_face_tri_lies_on_its_bbox_plane() { /* for 3001: each Face(d) tri's 3 vertices on plane d (1e-4) */ }
```

- [ ] **Step 4: Run them to verify they fail**

Run: `just test-one ldraw-bake ""`
Expected: FAIL (`bake_part` not defined).

- [ ] **Step 5: Implement the bake**

- `flatten.rs`: `pub fn flatten(lib: &Library, part: &str, detail: Detail) -> Flat` — resolve sub-files with `weldr` (search `parts/`, `p/`, and for `Detail::Lo` prefer `p/8/<name>` when it exists, for `Detail::Hi` plain `p/`); compose transforms; BFC: honour `0 BFC CERTIFY CCW/CW`, `INVERTNEXT`, and flip winding when `det(matrix) < 0`; colour 16 → `TriColor::Own`, 24 → `Edge`, others `Fixed(code)`; collect type-2 lines as edges (skip type 5). Record for each triangle whether it came through a `stud.dat`-family top-stud reference (`stud.dat`, `stud2.dat`, `studa.dat`, and their `p/8/` variants) and that reference's translation.
- `classify.rs`: convert LDU → units (`× 0.4`), flip Y (`y_up = -y`), translate so the bbox min of the **non-stud** geometry is the origin. `size = round(extent / [20, 8, 20] LDU)`. Tag: stud-reference tris → `Stud([cx, size[1], cz])` from the reference's translation; tris with all three vertices on one bbox plane (1e-3 units) and normal along that plane's outward axis → `Face(dir)`; everything else `Inner`. Edges: `Stud` like tris; on one or two bbox planes → `Faces(mask)`; else `Inner`. `hull`: empty when the non-stud, non-`Inner` geometry fills the bbox (every bbox face is covered by `Face` tris); else the convex hull (gift-wrap or quickhull over the non-stud vertices, in LDU rounded to `i16`).
- `emit.rs`: write `crates/game/src/stud_catalog.rs` (rustfmt-friendly literal), `assets/studs/parts.bin` = `game::encode_packet(&BakedParts { version: BAKED_PARTS_VERSION, .. })`, `assets/studs/colors.ron` (every `!COLOUR` line: CODE, VALUE, EDGE, ALPHA default 255, finish from `CHROME`/`PEARLESCENT`/`METAL`/`RUBBER`/`MATERIAL` keywords, `Transparent` when ALPHA < 255), `assets/studs/ATTRIBUTION.md` (one line per baked part: number, name, `Author:` and `!LICENSE` header lines, plus a CC BY 2.0/4.0 credit paragraph naming the LDraw library).
- `main.rs`: `ldraw-bake [--ldraw DIR] [--check]`; `--check` bakes in memory and exits non-zero if any output differs from the committed file (for CI later).

- [ ] **Step 6: Add the justfile recipe** next to `creatures`:

```just
# Bake the LDraw parts in tools/ldraw-bake/parts.txt into crates/game/src/stud_catalog.rs and
# assets/studs/ (LDRAW_DIR overrides assets/ldraw, which is git-ignored).
ldraw-bake *args:
    {{cap}} {{cargo}} run -p ldraw-bake --release -- --ldraw "${LDRAW_DIR:-assets/ldraw}" {{args}}
```

- [ ] **Step 7: Run tests, bake, and check the dependency firewall**

Run: `just test-one ldraw-bake ""` → PASS.
Run: `just ldraw-bake` → writes the four outputs; `crates/game/src/stud_catalog.rs` has 24 parts.
Run: `cargo tree -p client -i weldr; cargo tree -p server -i weldr; cargo tree -p game -i weldr` → each prints `error: package ID specification \`weldr\` did not match any packages` (i.e. not in the tree).
Run: `nice -n 10 cargo check -p game` → compiles with the generated catalog.

- [ ] **Step 8: Commit** (only if commits were requested)

```bash
git add tools/ldraw-bake Cargo.toml Cargo.lock justfile crates/game/src/lib.rs crates/game/src/stud_assets.rs crates/game/src/stud_catalog.rs assets/studs
git commit -m "feat(ldraw-bake): bake LDraw bricks, plates and slopes into the stud catalog and assets/studs"
```

---

### Task 2: Pin anisotropic `Voxels` in physics-next

Runs in parallel with Task 1. Needs no stud code: it builds raw `Voxels` with `(8, 1.6, 8)` cells.

**Files:**
- Create: `crates/physics-next/tests/stud_voxels.rs`
- Modify: `crates/physics-next/tests/it.rs` (`mod stud_voxels;`), `crates/physics-next/tests/common/mod.rs` (add `Walker::over`)

**Interfaces:**
- Produces (test rig only): `pub fn Walker::over(w: PhysicsWorld, top_units: f32) -> Walker` — the body of `Walker::new` after the chunks are added (spawn above `top_units` at `SPAWN`, settle, assert grounded). `Walker::new` calls it.

- [ ] **Step 1: Write the failing tests** (`stud_voxels.rs`). Terrain: one fixed body at the origin with a `ColliderBuilder::voxels(Vector::new(8.0, 1.6, 8.0), &keys)`. A "brick floor" = fine y cells 0..6 (3 plates) over an area; "caps" = one extra fine cell at y = 6 on every column.

```rust
const FINE: Vector = Vector::new(8.0, 1.6, 8.0);
#[test] fn capsule_rests_flat_on_capped_floor()   // settles; |y - expected| < 0.01 where expected = 7*1.6 + half_height + radius + skin; grounded for 60 ticks
#[test] fn capsule_walks_level_across_cap_cells()  // 120 ticks walking +z over 40 studs of capped floor: per-tick |dy| < LEVEL (0.005), speed constant within 1%, grounded throughout (no seam snag at 8-unit cell edges)
#[test] fn capsule_walks_level_across_two_bodies() // same, floor split into two bodies at a 256-unit chunk seam
#[test] fn ball_and_cuboid_rest_on_capped_floor()  // dynamic ball (r=4) and cuboid (half 4,4,4) dropped from 20 units: come to rest, bottom at 7*1.6 within 0.05, no tunnelling
#[test] fn capsule_steps_up_one_brick_course()      // floor + a 3-plate (+cap) step: with autostep max_height 0.65 m * 16 the body climbs it walking; with 0.55 m it does not
```

- [ ] **Step 2: Run to verify they fail** — `just test-one physics-next "--test it stud_voxels::"` → FAIL (`Walker::over` missing).

- [ ] **Step 3: Add `Walker::over` and the terrain helpers** inside the test file. The step test drives `movement::character_controller` through a local copy of the controller with `max_height` overridden (the scaled API arrives in Task 3; this test pins the fork, not the game).

- [ ] **Step 4: Run** — `just test-one physics-next "--test it stud_voxels::"` → PASS. If a contact or seam case fails, **stop and report**: the spec's collider choice depends on it (fallback to discuss with the user: per-brick cuboids in one compound).

- [ ] **Step 5: Commit** — `git add crates/physics-next/tests/{stud_voxels.rs,it.rs,common/mod.rs}`; `test(physics-next): pin anisotropic (8, 1.6, 8) voxels for stud chunks`.

---

### Task 3: Per-world body scale (`BodyScale`)

The least-explored part. Movement constants are global `const`s used across sim, tests and client; this task threads one per-region value to the **player** path only and leaves every existing call site and constant intact.

**Design (decided here):**
- `pub struct BodyScale { pub factor: f32, pub step_height_m: f32 }` in `movement.rs`, `#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]`, `Default` = `VOXEL`.
- `pub const VOXEL: BodyScale = BodyScale { factor: 1.0, step_height_m: STEP_HEIGHT }`.
- `pub const STUD: BodyScale = BodyScale { factor: STUD_BODY_FACTOR, step_height_m: STUD_STEP_HEIGHT_M }` with `pub const STUD_BODY_FACTOR: f32 = 4.0 / 3.0;` (1.8 m → 2.4 m) and `pub const STUD_STEP_HEIGHT_M: f32 = 0.65;` (one 0.6 m brick course plus margin; **decision point:** at first playtest pick between one plate (0.25) + jump-a-brick, and this).
- What scales by `factor`: capsule half height and radius, eye offset, spawn clearance, `JUMP_SPEED`, `GRAVITY`, `FALL_GRAVITY`, `TERMINAL_SPEED`, `GROUND_SNAP` (= `step_height_m`), `AUTOSTEP_MIN_WIDTH_M`. Same jump/fall times, lengths ×4/3. `WALK_SPEED` and swimming do **not** scale (decision point at playtest).
- Methods: `capsule_half_height(&self) -> f32`, `capsule_radius(&self) -> f32`, `eye_offset(&self) -> f32` (units), `move_params(&self, base: MoveParams) -> MoveParams` (scales `jump_speed`, sets `gravity_scale`).
- `MoveParams` gains `pub gravity_scale: f32` (`PLAYER`: 1.0; slime: 1.0); `plan_with` multiplies `GRAVITY`, `FALL_GRAVITY`, `TERMINAL_SPEED` by it.
- New `character_controller_scaled(motion, scale)`, `sweep_character_scaled(.., scale)`, `depenetration_lift_scaled(.., scale)`; the old functions call them with `BodyScale::VOXEL` (no other call site changes).
- Storage: `GameData.body_scale: #[undo(cell)] crate::movement::BodyScale` — written only raw at genesis (Task 6's `seed_stud_genesis`), read by the player path; travels with the serialized `Rollback` (parking, snapshots).

**Files:**
- Modify: `crates/game/src/movement.rs`, `crates/game/src/state.rs` (GameData field; `player_spawn_y` ~:1193; player creation ~:1258-1306 capsule + rig offset; accessor `pub fn body_scale(&self) -> BodyScale`), `crates/game/src/camera.rs` (~:290-320 walk path), `crates/game/src/edit.rs` (~:612 eye), `crates/game/src/portable.rs` / `migrate.rs` only if they destructure `GameData` (classify `body_scale`: not persisted — stud worlds have no store; restored regions get `VOXEL`)
- Test: `crates/game/tests/body_scale.rs` (+ `mod body_scale;` in `tests/it.rs`)

**Interfaces:**
- Produces: `game::movement::BodyScale` (`movement` is not glob re-exported), `Rollback::body_scale(&self) -> BodyScale`, `Rollback::set_body_scale_raw(&mut self, s: BodyScale)` (genesis-only, unlogged), `movement::{sweep_character_scaled, character_controller_scaled, depenetration_lift_scaled}`.

- [ ] **Step 1: Write the failing tests** (`body_scale.rs`)

```rust
#[test] fn voxel_scale_is_todays_constants() {
    let s = BodyScale::VOXEL;
    assert_eq!(s.capsule_half_height(), PLAYER_CAPSULE_HALF_HEIGHT);
    assert_eq!(s.capsule_radius(), PLAYER_CAPSULE_RADIUS);
    assert_eq!(s.eye_offset(), PLAYER_EYE_OFFSET);
}
#[test] fn stud_scale_is_a_2_4_m_minifigure() {
    let s = BodyScale::STUD;
    let h = 2.0 * (s.capsule_half_height() + s.capsule_radius()) / UNITS_PER_METER;
    assert!((h - 2.4).abs() < 1e-4);
}
#[test] fn stud_jump_peak_scales_with_body() // peak = jump² / (2 g): STUD peak / VOXEL peak == 4/3 within 1e-4, airtime equal within 1e-4
#[test] fn stud_player_capsule_is_scaled() // Rollback::new, set_body_scale_raw(STUD), add a player the way tests/player.rs does: its collider is a capsule with STUD dims
#[test] fn default_rollback_keeps_voxel_scale() // Rollback::new(None).body_scale() == BodyScale::VOXEL
```

- [ ] **Step 2: Run to verify failure** — `just test-one game "--test it body_scale::"` → FAIL (no `BodyScale`).

- [ ] **Step 3: Implement** per the design. The player creation path reads `self.data.body_scale` for the capsule, rig offset and `player_spawn_y`'s clearance; `camera.rs` passes `scale.move_params(MoveParams { allow_flight, ..MoveParams::PLAYER })` and calls the `_scaled` sweep; `edit.rs:612` uses `scale.eye_offset()`. The crossing fallback `ColliderSpec::CapsuleY` (~:1783) stays VOXEL (the real spec travels with the shape).

- [ ] **Step 4: Run** — `just test-one game "--test it body_scale::"` → PASS.

- [ ] **Step 5: Hash shift.** Run `just test-one game "--test it physics_goldens::"`. If and only if the diffs are state-hash values (positions unchanged), re-record: `PHYSICS_GOLDENS_RECORD=1 just test-one game "--test it physics_goldens::"`, re-run → PASS. Any position/velocity diff is a bug in this task: fix it, do not re-record. Also run `just test-one game "--test it jump_arc:: slope_walk:: swimming:: player::"` → PASS (VOXEL behaviour unchanged).

- [ ] **Step 6: Commit** — `feat(game): per-region BodyScale for the player body (voxel 1.8 m, stud 2.4 m)`; message says the goldens were re-recorded for the GameData hash shift.

---

### Task 4: Stud data (`game::stud`)

**Files:**
- Create: `crates/game/src/stud.rs`
- Modify: `crates/game/src/lib.rs` (`pub mod stud;` + `pub use stud::*;`)
- Test: `crates/game/tests/stud.rs` (+ `mod stud;` in `tests/it.rs`)

**Interfaces:**
- Consumes: `game::stud_catalog::{STUD_PARTS, STUD_COLORS}`, `game::stud_assets::StudPartDef` (Task 1).
- Produces:
  - Consts: `STUD_UNITS: usize = 8`, `STUD_CHUNK_STUDS: usize = 32`, `STUD_CHUNK_PLATES: usize = 75`, `STUD_CHUNK_HEIGHT_UNITS: usize = 240`, `PLATES_PER_BRICK: u8 = 3`, `LDU_UNITS: f32 = 0.4`; compile-time asserts `STUD_CHUNK_PLATES % 15 == 0`, `STUD_UNITS * STUD_CHUNK_STUDS == CHUNK_UNITS`, `STUD_CHUNK_PLATES * 16 / 5 == STUD_CHUNK_HEIGHT_UNITS`.
  - `pub fn plate_units(plates: i32) -> f32` = `(plates * 16) as f32 / 5.0`.
  - `pub struct StudChunkCoords { pub x: usize, pub y: i32, pub z: usize }` — derives `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde, StateHash`; `fn origin(&self) -> Vector` (`(x·256, y·240, z·256)`), `fn from_origin(t: Vector) -> Self` (exact division, `round`).
  - `pub enum StudShape { Part { part: u16, rot: u8 }, Filler { size: [u8; 3] } }`; `pub struct StudBrick { pub min: [u8; 3], pub shape: StudShape, pub color: u16 }` — both `Debug, Clone, Copy, PartialEq, Eq, serde, StateHash`.
  - `impl StudBrick { fn size(&self) -> [u8; 3] /* rotated */; fn cells(&self) -> impl Iterator<Item = [u8; 3]>; fn stud_cells(&self) -> Vec<[u8; 3]> /* chunk cells, may have y == 75 */; fn part(&self) -> Option<&'static StudPartDef>; fn is_box(&self) -> bool /* Filler or empty hull */ }`.
  - `pub fn rotate_cell(rot: u8, c: [u8; 3], size: [u8; 3]) -> [u8; 3]`: rot 0 identity; 1 → `[sz-1-z, y, x]`; 2 → `[sx-1-x, y, sz-1-z]`; 3 → `[z, y, sx-1-x]` (`size` unrotated). Rotated size swaps x/z for odd `rot`. Hull points rotate the same way in continuous coordinates (`x → Sz - z` etc.).
  - `pub enum StudError { OutOfChunk, Overlap, UnknownPart, UnknownColor, BadRotation, EmptyFiller }` (`Debug, Clone, Copy, PartialEq, Eq`).
  - `pub struct StudChunk { bricks: BTreeMap<[u8; 3], StudBrick> }` — `Default, Debug, Clone, PartialEq, Eq, serde, StateHash`. Methods: `new(impl IntoIterator<Item = StudBrick>) -> Result<Self, StudError>`, `bricks(&self) -> impl Iterator<Item = &StudBrick>`, `len`, `is_empty`, `brick_at(&self, cell: [u8; 3]) -> Option<&StudBrick>`, `occupancy(&self) -> StudOccupancy`, `place(&mut self, b: StudBrick) -> Result<(), StudError>`, `remove_at(&mut self, cell: [u8; 3]) -> Option<StudBrick>`, `top_plate(&self, sx: u8, sz: u8) -> Option<u8>` (one past the highest occupied plate in that stud column).
  - `pub struct StudOccupancy` (bitset of 32·75·32 bits) with `get(&self, c: [i32; 3]) -> bool` (out of range = false), `set`.
  - Validation order in `place`: rotation `< 4` → part index `< STUD_PARTS.len()` → filler non-empty → colour `∈ STUD_COLORS` (binary search) → bounds (`min + size ≤ [32, 75, 32]`) → overlap.

- [ ] **Step 1: Write the failing tests** (`tests/stud.rs`)

```rust
fn part(n: &str) -> u16 { STUD_PARTS.iter().position(|p| p.number == n).unwrap() as u16 }
fn brick(n: &str, min: [u8; 3], rot: u8) -> StudBrick { StudBrick { min, shape: StudShape::Part { part: part(n), rot }, color: 4 } }

#[test] fn units_are_exact() { assert_eq!(plate_units(75), 240.0); assert_eq!(plate_units(15), 48.0); assert_eq!(plate_units(5), 16.0); }
#[test] fn coords_round_trip_origin() { let c = StudChunkCoords { x: 3, y: -2, z: 7 }; assert_eq!(StudChunkCoords::from_origin(c.origin()), c); assert_eq!(c.origin(), Vector::new(768.0, -480.0, 1792.0)); }
#[test] fn rotation_four_times_is_identity() // all cells of a 2x4 (3001), every rot
#[test] fn rotated_2x4_footprint_swaps() { assert_eq!(brick("3001", [0,0,0], 1).size(), [2, 3, 4]); }
#[test] fn brick_2x4_occupies_24_cells_and_8_studs() // cells().count()==24, stud_cells() all y==3
#[test] fn place_refuses_overlap() // place 3001 at 0,0,0 then 3003 at [1,1,0] -> Err(Overlap), chunk unchanged
#[test] fn place_refuses_out_of_chunk() // 3001 at [30,0,0] rot 0 -> OutOfChunk; 3005 at [0,73,0] -> OutOfChunk (3 plates from 73 ends at 76)
#[test] fn place_refuses_unknown_part_colour_rotation() // part 9999 / colour 9999 / rot 4
#[test] fn canonical_form_is_order_independent() // StudChunk::new of the same bricks in two orders: == and equal bincode bytes and equal StateHash
#[test] fn remove_at_any_cell_removes_whole_brick() // remove_at([3,2,1]) of a 3001 at 0,0,0 returns it; occupancy empty
#[test] fn top_plate_reads_highest_brick() // filler 32x10x32 + 3001 on top at [0,10,0] -> top_plate(0,0)==Some(13), top_plate(31,31)==Some(10)
```

- [ ] **Step 2: Run** — `just test-one game "--test it stud::"` → FAIL (module missing).
- [ ] **Step 3: Implement `stud.rs`** per Interfaces.
- [ ] **Step 4: Run** — same command → PASS.
- [ ] **Step 5: Commit** — `feat(game): stud chunk data — StudBrick, StudChunk, canonical BTreeMap, validation`.

---

### Task 5: Stud colliders (pure derivation)

**Files:**
- Create: `crates/game/src/stud_collider.rs`
- Modify: `crates/game/src/lib.rs` (`pub mod stud_collider; pub use stud_collider::*;`)
- Test: `crates/game/tests/stud_collider.rs` (+ `mod` line in `it.rs`)

**Interfaces:**
- Consumes: `StudChunk`, `StudBrick::{cells, stud_cells, is_box, part}`, `rotate_cell` (Task 4).
- Produces:
  - `pub const STUD_FINE_CELL: Vector = Vector::new(8.0, 1.6, 8.0);` `pub const STUD_COARSE_CELL: Vector = Vector::new(32.0, 48.0, 32.0);` (4 studs × 15 plates × 4 studs).
  - `pub struct StudColliderCells { pub coarse: Vec<IVector>, pub fine: Vec<IVector> }` — each sorted by `(y, z, x)`, no duplicates.
  - `pub fn stud_collider_cells(chunk: &StudChunk) -> StudColliderCells`. Rules: *solid* cells = cells of box bricks (`is_box`). Coarse key `(cx, cy, cz)` ∈ 8×5×8 is listed iff all its 4·15·4 cells are solid. Fine keys: every solid cell `[x, y, z]` not inside a listed coarse cell → `(x, 2y, z)` and `(x, 2y+1, z)`; plus a **cap** `(x, 2y, z)` for every `stud_cells()` cell of every brick (box or not) that is not occupied (any brick) — `y == 75` included (overlap with the chunk above is tolerated).
  - `pub fn stud_hulls(chunk: &StudChunk) -> Vec<Vec<Vector>>` — one point list per non-box brick, in `BTreeMap` order: `hull` LDU points rotated by `rot`, scaled by `LDU_UNITS`, offset by `(min.x·8, plate_units(min.y), min.z·8)`.
  - `pub fn stud_collider_shapes(chunk: &StudChunk) -> Vec<SharedShape>` — the wanted collider list in fixed order: coarse `Voxels` (if non-empty), fine `Voxels` (if non-empty), then one `ConvexPolyhedron::from_convex_hull` per hull. Used by every builder so genesis and runtime agree.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn single_brick_fine_cells_and_caps() // 3001 at [0,0,0]: coarse empty; fine = 24 cells * 2 + 8 caps at y=6
#[test] fn stacked_brick_covers_caps() // 3001 on 3001 (min y 3): lower brick's studs are covered -> no cap at y=6 under it; caps at y=12
#[test] fn full_filler_goes_coarse() // Filler [32, 15, 32] at 0: coarse = the 64 keys at cy 0; fine empty (a Filler has no studs, so no caps)
#[test] fn partial_coarse_cell_stays_fine() // Filler [32, 14, 32]: coarse empty
#[test] fn slope_is_a_hull_not_cells() // 3040 alone: fine = its 1 cap only; stud_hulls len 1, all points inside the brick's bbox (units)
#[test] fn collider_matches_occupancy() // random (seeded oorandom) valid placements: a downward ray (parry Voxels cast_local_ray) at every stud column centre hits at plate_units(top_plate) + 1.6 (cap) or plate_units(top_plate) (no stud there) within 1e-3
#[test] fn shapes_order_is_coarse_fine_hulls()
```

- [ ] **Step 2: Run** — `just test-one game "--test it stud_collider::"` → FAIL.
- [ ] **Step 3: Implement** per Interfaces. Use `IVector` / `Vector` from `crate::math` like `block.rs`'s `ColliderCells`.
- [ ] **Step 4: Run** → PASS. Tag `collider_matches_occupancy` slow if >100 ms.
- [ ] **Step 5: Commit** — `feat(game): stud colliders — fine/coarse anisotropic voxels, stud caps, part hulls`.

---

### Task 6: Stud chunks in the ECS — genesis, runtime create/set, hash shift

**Files:**
- Create: `crates/game/src/stud_ops.rs` (`impl Rollback`), test `crates/game/tests/stud_ops.rs` (+ `mod`)
- Modify: `crates/game/src/lib.rs` (`mod stud_ops;`), `crates/game/src/state.rs` — `Ecs.stud: Component<crate::StudChunk>` (~:367), `GameData.stud_index: #[undo(map)] BTreeMap<crate::StudChunkCoords, EntityKey>` (~:290), `create_entity_safe` (~:520) `self.stud.insert_safe(key)`, `remove_entity_safe` (~:1318) remove the `stud` slot and the `stud_index` entry, `GameDataUpdateKind` (~:55) `AddStudChunk(EntityKey, crate::StudChunkCoords, Box<crate::StudChunk>)` and `SetStudChunk(EntityKey, Box<crate::StudChunk>)`, `player_spawn_y` (~:1193) also reads `stud_surface_y`; `crates/game/src/migrate.rs:~76` classify `stud` (terrain, never travels); `crates/game/src/portable.rs:~255` classify `stud` / `stud_index` (not persisted); `crates/game/src/region.rs` `pub fn seed_studs(&mut self, chunks: Vec<(StudChunkCoords, StudChunk)>)` next to `from_chunks` (~:1144) — calls `seed_stud_genesis` **and** `set_body_scale_raw(BodyScale::STUD)`.
- Every exhaustive `match` on `GameDataUpdateKind` (the compiler lists them; e.g. `client/src/renderer/bridge.rs::drain_region_updates`) gets the two new arms; outside the game crate add them as `{}` with `// Task 11` only where the client needs to compile now.

**Interfaces:**
- Consumes: Task 3 (`BodyScale`, `set_body_scale_raw`), Task 4, Task 5 (`stud_collider_shapes`).
- Produces (`impl Rollback`):
  - `pub fn seed_stud_genesis(&mut self, chunks: Vec<(StudChunkCoords, StudChunk)>)` — raw, unlogged, like `seed_terrain_genesis`: entity via a new `Undo<Ecs>::create_stud_entity_raw()` that inserts slots for `rigidbody` and `stud` only; raw fixed body at `coords.origin()` with `user_data` = entity key; colliders = `stud_collider_shapes`. An empty chunk still gets its entity and body.
  - `pub fn stud_key_at(&self, c: StudChunkCoords) -> Option<EntityKey>`; `pub fn stud_chunk_at(&self, c: StudChunkCoords) -> Option<&StudChunk>`.
  - `pub fn create_stud_chunk_safe(&mut self, c: StudChunkCoords, chunk: StudChunk) -> EntityKey` — mirrors `create_mesh`: `create_entity_safe`, `stud.set_safe`, `stud_index.insert`, fixed body via `undo_scope_digest` + `revert_insert`, colliders via `attach_collider_safe`; emits `AddStudChunk` (undo emits nothing extra: the entity insert already emits `RemoveEntity` on undo).
  - `pub fn set_stud_chunk_safe(&mut self, key: EntityKey, chunk: StudChunk)` — `emit_on_undo(SetStudChunk(old))` then `stud.set_safe`, then `sync_stud_colliders_safe`; emits `SetStudChunk(new)`.
  - `fn sync_stud_colliders_safe(&mut self, key: EntityKey)` — the `rebuild_body_colliders_safe` pattern (`edit.rs:~401`): wanted = `stud_collider_shapes(chunk)`; existing colliders reused in order with `set_shape` via `mutate_collider_safe` (enable if disabled), spares **disabled**, missing ones attached with `attach_collider_safe`. An empty chunk therefore keeps its entity with every collider disabled.
  - `pub fn stud_surface_y(&self, x: f32, z: f32) -> Option<f32>` — highest `origin.y + plate_units(top_plate)` over the stud chunks of that stud column (region-local units); `player_spawn_y` takes the max of it and the voxel surface.

- [ ] **Step 1: Write the failing tests** (`tests/stud_ops.rs`; hash helper as in `tests/random_ops.rs`)

```rust
#[test] fn genesis_builds_body_and_colliders() // seed one chunk with a 3001: stud_key_at Some, body fixed at origin, 1 enabled fine Voxels collider
#[test] fn genesis_empty_chunk_has_entity_and_no_colliders()
#[test] fn create_rolls_back() // new_transaction; create_stud_chunk_safe; rollback -> hash == before; AddStudChunk then RemoveEntity seen on the channel
#[test] fn set_place_rolls_back() // genesis chunk; tx: set with one more brick; rollback -> hash == before
#[test] fn set_remove_rolls_back()
#[test] fn remove_to_empty_then_place_rolls_back() // tx1 remove last brick (all colliders disabled, entity kept); tx2 place again (enabled); rollback twice -> each boundary hash matches
#[test] fn runtime_and_genesis_colliders_agree() // same content via seed_stud_genesis vs create_stud_chunk_safe: the shapes' StateHash equal
#[test] fn player_spawns_above_stud_surface() // Region seeded with a Filler [32, 50, 32] under spawn: player_spawn_y > plate_units(50)
#[test] fn stud_chunks_never_enter_chunk_index() // chunk_key_at(any) None; voxel scans unaffected
```

- [ ] **Step 2: Run** — `just test-one game "--test it stud_ops::"` → FAIL.
- [ ] **Step 3: Implement** per Interfaces and the Files list.
- [ ] **Step 4: Run** — same → PASS. Then `just test-one game "--test it random_ops:: hash_restore:: region_from_chunks:: portable:: handoff_state::"` → PASS.
- [ ] **Step 5: Hash shift** — as Task 3 Step 5 (`physics_goldens::`, re-record only for hash-only diffs).
- [ ] **Step 6: Commit** — `feat(game): stud chunk entities — genesis, undo-safe create/set, renderer updates` (mention the golden re-record).

---

### Task 7: In-place stud collider patches

Spec: edits are in-place `set_voxel` through `mutate_collider_safe`. Task 6's sync swaps whole shapes (always correct); this task patches the fine/coarse grids instead when it can, falling back to Task 6's sync exactly where `edit_block_colliders_safe` (`edit.rs:~313`) falls back.

**Files:**
- Modify: `crates/game/src/stud_ops.rs`
- Test: `crates/game/tests/stud_ops.rs` (add)

**Interfaces:**
- Produces: `fn patch_stud_colliders_safe(&mut self, key: EntityKey, old: &StudColliderCells, new: &StudColliderCells, old_hulls: usize, new_hulls: usize)`; `set_stud_chunk_safe` calls it instead of the full sync. Patchable iff the enabled colliders are exactly `[coarse?, fine?, hulls…]` as `stud_collider_shapes` would list them, no grid gains its first or loses its last cell, and the hull list is unchanged in length and content (hull edits → full sync). Removals in `old` order, then additions in `new` order, one `mutate_collider_safe` per grid with `shape_mut().as_voxels_mut().set_voxels(edits)`.
- Produces: `pub fn stud_collider_rebuilds() -> u64` (thread-local counter, like `COLLIDER_REBUILDS`) for tests.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn patched_shapes_hash_like_a_fresh_build() // 200 seeded random place/remove edits on a genesis chunk with a Filler: after each, each collider shape's StateHash == stud_collider_shapes(chunk)[i]'s
#[test] fn box_edits_patch_without_rebuild() // placing/removing plain bricks on a non-empty chunk leaves stud_collider_rebuilds() unchanged
#[test] fn patch_rolls_back() // tx place, tx remove; rollback each -> boundary hashes
```

- [ ] **Step 2: Run** — `just test-one game "--test it stud_ops::"` → the new tests FAIL (`stud_collider_rebuilds` missing).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `perf(game): patch stud collider grids in place on edits`.

---

### Task 8: Orders — `PlaceStud` / `RemoveStud`

**Files:**
- Modify: `crates/game/src/orders.rs` (`Order` ~:131 two variants; `apply_order_safe` ~:365 arms), `crates/game/src/stud_ops.rs`
- Test: `crates/game/tests/stud_orders.rs` (+ `mod`), uses a real `Region` (`Region::from_chunks(id, vec![])` + `seed_studs`)
- Determinism: `just determinism-wasm` at the end of this task (covers Tasks 3–8)

**Interfaces:**
- Produces: `Order::PlaceStud { chunk: StudChunkCoords, brick: StudBrick }`, `Order::RemoveStud { chunk: StudChunkCoords, cell: [u8; 3] }` (`Order` stays `Copy + StateHash`).
- Produces: `pub fn place_stud_safe(&mut self, c: StudChunkCoords, b: StudBrick) -> bool` (missing chunk → `create_stud_chunk_safe` with the one brick; `place` error → `false`, no state touched), `pub fn remove_stud_safe(&mut self, c: StudChunkCoords, cell: [u8; 3]) -> bool` (no brick → `false`). `apply_order_safe` returns their result. Region-local: a chunk outside the region's x/z range (`c.x, c.z >= REGION_CHUNKS`) → `false`.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn place_order_round_trip() // region.handle_event(Order(0, PlaceStud{..})) -> stud_chunk_at has it; event_log has it; rollback the event -> hash restored
#[test] fn remove_order_round_trip()
#[test] fn place_order_across_chunk_edge_is_noop() // 3001 at [30, 0, 0]: hash unchanged, no update emitted
#[test] fn place_order_into_missing_chunk_creates_it() // AddStudChunk emitted
#[test] fn place_order_outside_region_is_noop()
#[test] fn replayed_orders_match_server() // two regions from the same seed: one handles orders as server (commit_server_event), one predicts + rolls back + replays: equal state_hash
```

- [ ] **Step 2: Run** — `just test-one game "--test it stud_orders::"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS. Then `just test-one game "--test it orders::"` → PASS.
- [ ] **Step 5: Determinism** — `just determinism-wasm` → PASS (the stud catalog, `StudChunk` hashing and the new `Ecs`/`GameData` fields hash identically on wasm32).
- [ ] **Step 6: Commit** — `feat(game): PlaceStud / RemoveStud orders`.

---

### Task 9: The stud test world generator (worldgen)

**Files:**
- Create: `crates/worldgen/src/stud_world.rs`; modify `crates/worldgen/src/lib.rs` (`pub mod stud_world;`)
- Test: `crates/worldgen/tests/stud_world.rs` (+ `mod` in `crates/worldgen/tests/it.rs`)

**Interfaces:**
- Consumes: `game::{StudChunk, StudBrick, StudShape, StudChunkCoords, STUD_CHUNK_STUDS, REGION_CHUNKS, RegionCoords}`, `game::stud_catalog::STUD_PARTS`.
- Produces: `pub fn region_studs(rc: RegionCoords) -> Vec<(StudChunkCoords, StudChunk)>` — pure; `|rc.x| ≤ 1 && |rc.z| ≤ 1` → every chunk column of the region at stud layer `y = 0`; any other region → empty `Vec`.
- Constants (named, in the file): `STUD_WORLD_RADIUS: i32 = 1`, `MIN_SURFACE_PLATES: u8 = 10` (2 m), `MAX_SURFACE_PLATES: u8 = 65` (13 m), `FILLER_GAP_PLATES: u8 = 6`, colours `GRASS = 2`, `DIRT = 19` (tan, the next 2 courses), `STONE = 72` (dark bluish grey, deeper and the filler).
- Algorithm:
  - Height field: per world stud column, value noise from an integer hash of `(wx, wz, octave)` (splitmix64), two octaves, smoothstep interpolation in `f64` (server-only, not sim), mapped to `MIN..=MAX` plates and quantized to whole plates.
  - Per chunk: `Filler { size: [32, f, 32] }` at `[0,0,0]` with `f` = `(min height in chunk − FILLER_GAP_PLATES)` rounded **down** to a multiple of 3 (omit if ≤ 0).
  - Courses of 3 plates from `f` up: for course base `b`, the mask is columns with `h ≥ b + 3`. Lay rows along x on even courses and along z on odd courses; walk each row's runs and place the longest 1×N brick (N ∈ 8, 6, 4, 3, 2, 1 from the catalog's 1-wide bricks) that fits; odd rows/courses start their first run with a shorter brick (offset by 1 stud on alternating courses) so seams stagger (running bond).
  - The remaining 1–2 plates per column (`b < h < b + 3` at the last course) are filled with 1×N plates the same way, one plate layer at a time.
  - Colour by depth below the column's surface: top course/plate layer `GRASS`, next two courses `DIRT`, below `STONE`.
  - Feature in region (0, 0), chunk (0, 0): a 1-stud-thick staggered wall of 2×4/1×4 bricks 6 courses tall along x at studs `z = 24`, `x = 4..28`, and a 4×4-stud tower of 8 courses at `x = 26..30, z = 26..30`, both standing on the local surface, built with `place` (they must not overlap the terrain bricks — lay them first above the surface).
  - Every brick goes through `StudChunk::place` (so the result is valid by construction); `place` errors are bugs (`expect`).

- [ ] **Step 1: Failing tests**

```rust
#[test] fn outside_radius_is_empty() { assert!(region_studs(RegionCoords::new(2, 0)).is_empty()); }
#[test] fn inside_radius_fills_every_chunk_column() // len == REGION_CHUNKS^2, all y == 0
#[test] fn deterministic() // two calls equal
#[test] fn surface_matches_height_field_and_bounds() // every column's top_plate in MIN..=MAX
#[test] fn top_is_grass_and_seams_stagger() // the brick at each column's top cell has colour 2; in some course, two vertically adjacent bricks have different min x (running bond)
#[test] fn neighbouring_regions_meet_without_cliffs() // across the (0,0)/(1,0) region edge, adjacent column heights differ by <= 6 plates
#[test] fn spawn_feature_exists() // region (0,0) chunk (0,0) has bricks above the surface at the wall cells
```

Tag the region-wide tests slow if >100 ms.

- [ ] **Step 2: Run** — `just test-one worldgen "--test it stud_world::"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS (with `-- --include-ignored` for the slow ones once).
- [ ] **Step 5: Commit** — `feat(worldgen): stud test world — height field laid as running-bond courses`.

---

### Task 10: Server — `studs` world source, genesis hook, LOD opt-out

**Files:**
- Modify: `crates/server/src/world_file.rs` (`STUDS_KEYWORD: &str = "studs"`, `WorldSource::Studs`, `resolve` early return like `procedural` with the same `VOXEL_WORLD_DB` warning; `open_world` → `OpenedWorld { store: None, world: None, map: None, world_tick: 0 }`), `crates/server/src/lib.rs` (~:639 log arm; generators; LOD builder; spawner; ~:747 arm), `crates/server/src/region_threads.rs` (`genesis` field + `with_genesis`), `crates/game/src/lod_builder.rs` (`NullLodBuilder`)
- Test: `crates/server/tests/stud_world.rs` (+ `mod` in `crates/server/tests/it.rs`); unit test in `world_file.rs` tests

**Interfaces:**
- Consumes: `worldgen::stud_world::region_studs` (Task 9), `Region::seed_studs` (Task 6).
- Produces: `pub type StudGenesis = Arc<dyn Fn(RegionCoords) -> Vec<(game::StudChunkCoords, game::StudChunk)> + Send + Sync>` and `ThreadRegionSpawner::with_genesis(self, g: StudGenesis) -> Self` in `region_threads.rs`. In `spawn`, replace `seed.into_runner(id, out, gen)` on the not-stored path with: `let fresh = matches!(seed, RegionSeed::Deferred(None));` → `let mut region = seed.into_region_with(id, gen);` → `if fresh { if let Some(g) = &genesis { region.seed_studs(g(id)); } }` → `RegionRunner::new(id, region, out)` (genesis lands **before** the runner's baseline hash). Parked/restored regions never get genesis.
- Produces: `game::NullLodBuilder` — `impl LodBuilder` whose `submit` drops the job (logs once at `info`: `"[server] lod: none for this world"`).
- `lib.rs` for `Studs`: region generator and halo generator return `Vec::new()`; **no** `with_prefetch`; `with_genesis(Arc::new(worldgen::stud_world::region_studs))`; `lod_builder = Box::new(game::NullLodBuilder)`; the manager's own generator closure returns `Vec::new()`; log `"[server] world: studs (toy bricks, nothing is saved)"`.

- [ ] **Step 1: Failing tests**

```rust
// world_file.rs unit tests
assert_eq!(resolve(Some("studs"), None).unwrap(), WorldSource::Studs);
assert_eq!(resolve(Some("studs"), Some("x.sqlite")).unwrap(), WorldSource::Studs);
// tests/stud_world.rs
#[test] fn fresh_stud_region_starts_with_its_chunks() // ThreadRegionSpawner::with_region_generator(empty).with_genesis(region_studs) spawns (0,0) Deferred(None); request its snapshot over the channel; the Rollback has REGION_CHUNKS^2 stud chunks and body_scale == STUD
#[test] fn far_region_is_empty_air()                  // (5,5): zero stud chunks, body_scale == STUD
#[test] fn parked_stud_region_keeps_its_edit()        // place a brick via an Order, park (Parked/Deferred(Some)), respawn: brick present, genesis not re-run (chunk count unchanged, edit kept)
```

Use the snapshot/park helpers `crates/server/tests/threaded_world.rs` uses; mark tests slow if >100 ms.

- [ ] **Step 2: Run** — `just test-one server "--lib world_file"` and `just test-one server "--test it stud_world::"` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** both → PASS (`-- --include-ignored` for slow ones).
- [ ] **Step 5: Seam/threads check** — `just test-threads` → PASS.
- [ ] **Step 6: Commit** — `feat(server): VOXEL_WORLD_FILE=studs — stud genesis hook, no store, no LOD`.

---

### Task 11: Client bridge — mirror stud chunks, body scale

**Files:**
- Create: `crates/client/src/renderer/stud/mod.rs` (`StudPlugin`, `SimStudChunk`)
- Modify: `crates/client/src/renderer/bridge.rs` (`spawn_region_snapshot` ~:920-962; `drain_region_updates` ~:1024 arms; replace Task 6's placeholder arms), `crates/client/src/renderer/interpolate.rs:~26` (`Without<SimStudChunk>`), `crates/client/src/renderer/mod.rs:~108` (`app.add_plugins(stud::StudPlugin)` next to `WaterPlugin`), `crates/client/src/renderer/avatar.rs:~87` (capsule from `ClientBodyScale`), `bridge.rs:~182` `EYE_OFFSET` users read `ClientBodyScale`

**Interfaces:**
- Produces: `#[derive(Component, Clone)] pub struct SimStudChunk { pub coords: game::StudChunkCoords, pub chunk: game::StudChunk }`; `#[derive(Resource, Default, Clone, Copy)] pub struct ClientBodyScale(pub game::movement::BodyScale)` (session-reset).
- Snapshot: an entity with `data.ecs.stud.try_get(key).is_some()` spawns as `ChildOf(terrain root)` with `Transform` from its body pose, `SimStudChunk { coords: StudChunkCoords::from_origin(pose.translation), chunk }`, `SessionScoped` as chunk entities are. `ClientBodyScale` is set from the snapshot's `body_scale()`.
- `AddStudChunk(key, coords, chunk)` → spawn/replace like the snapshot path; `SetStudChunk(key, chunk)` → replace the `SimStudChunk.chunk` (triggers `Changed`). `RemoveEntity` already despawns.

- [ ] **Step 1:** Implement (bridge code is integration glue; it is exercised by Task 12's tests and Task 14's look).
- [ ] **Step 2: Verify** — `just smoke` → passes within 60 s.
- [ ] **Step 3: Commit** — `feat(client): mirror stud chunks from region snapshots and updates`.

---

### Task 12: Client stud mesher and stud LOD

**Files:**
- Create: `crates/client/src/renderer/stud/mesh.rs` (pure: no ECS), `crates/client/src/renderer/stud/systems.rs`
- Modify: `crates/client/src/renderer/stud/mod.rs`

**Interfaces:**
- Consumes: `game::stud_assets::{BakedParts, StudColor, …}` loaded once at plugin build from `assets/studs/parts.bin` (`game::decode_packet`) and `assets/studs/colors.ron` into `#[derive(Resource)] StudAssets { parts: BakedParts, colors: BTreeMap<u16, StudColor> }` (panic with the path and `run just ldraw-bake` if missing or version ≠ `BAKED_PARTS_VERSION`).
- Produces (`mesh.rs`):
  - `pub struct StudNeighbours<'a> { pub get: &'a dyn Fn([i32; 3]) -> bool }` — occupancy in **chunk-local cells extended past the edges** (x/z −1 and 32, y −1 and 75); the system answers from loaded neighbour mirrors in the same region (same layer for x/z, layers ±1 for y); anything unloaded or across a region seam = free.
  - `pub struct StudMeshData { pub positions: Vec<[f32; 3]>, pub normals: Vec<[f32; 3]>, pub colors: Vec<[f32; 4]>, pub indices: Vec<u32> }`.
  - `pub fn body_mesh(chunk: &StudChunk, n: &StudNeighbours, assets: &StudAssets) -> StudMeshData` — per brick, transform its **hi** baked tris (rotate by `rot` about the bbox, translate by `(min.x·8, plate_units(min.y), min.z·8)`), keep `Face(d)` iff any cell across face `d` over the brick's footprint is free, `Inner` iff any cell under the footprint is free, never `Stud`; `Filler` = its 6 box faces with the same rule. Colours: `Own` → brick colour RGB, `Edge` → the colour's edge RGB, `Fixed(c)` → `c`'s RGB. Kept edges (`Faces(mask)` with any kept face) become quads `STUD_EDGE_WIDTH = 0.12` units wide in the face plane, offset `STUD_EDGE_LIFT = 0.01` units outward, in the colour's edge RGB.
  - `pub fn stud_mesh(chunk: &StudChunk, n: &StudNeighbours, assets: &StudAssets) -> StudMeshData` — the **lo** `Stud(cell)` tris of each brick whose stud cell (chunk-local, may be y = 75) is free.
- Produces (`systems.rs`): `remesh_studs` — on `Changed<SimStudChunk>` queue the chunk **and its 6 loaded neighbours**; process at most `STUD_MESH_BUDGET_PER_FRAME = 8` chunks per frame (`SessionLocal` queue, FIFO, deduplicated); each chunk owns two children (`StudBodyMesh`, `StudTopMesh`) with `Mesh3d` + `MeshMaterial3d` of one shared `StandardMaterial { base_color: WHITE, perceptual_roughness: 0.35, ..}` (vertex colours), created once per session. `stud_lod` — `StudTopMesh` `Visibility::Hidden` unless the chunk centre is within `STUD_TOPS_RADIUS_M = 40.0` of the commander camera's focus (the point `renderer/commander/top_down.rs` orbits).

- [ ] **Step 1: Failing tests** (`mesh.rs` `#[cfg(test)]`, using a tiny `StudAssets` built from the committed `parts.bin`)

```rust
#[test] fn lone_brick_keeps_all_faces_and_studs()     // 3001 alone: body has Face tris for all 6 dirs; stud_mesh tri count == 8 * lo tris per stud
#[test] fn stacked_bricks_cull_shared_faces()          // 3001 on 3001: lower's PosY and upper's NegY dropped; lower's studs dropped
#[test] fn neighbour_chunk_culls_edge_face()           // brick at x 31 with neighbour occupied at x 32: PosX dropped
#[test] fn buried_filler_draws_nothing()               // Filler with every neighbour cell occupied: empty mesh
#[test] fn mesher_respects_frame_budget()              // queue of 20 changed chunks drains in 3 calls of the pure queue step (8, 8, 4)
```

- [ ] **Step 2: Ask the user** before running client tests (CLAUDE.md). If yes: `just test-one client "--lib stud::"` → FAIL, then after Step 3 → PASS. If no: rely on `just smoke` and Task 14's look, and say so in the report.
- [ ] **Step 3: Implement** `mesh.rs` then `systems.rs`.
- [ ] **Step 4: Verify** — `just smoke` → passes.
- [ ] **Step 5: Commit** — `feat(client): stud chunk mesher from baked LDraw parts, culled faces, edge lines, stud LOD`.

---

### Task 13: Height grid from stud chunks

Without it the commander camera never places, the loading cover (`frontend/loading.rs:~99-145`) loops and `CursorPolicy.loading` sticks.

**Files:**
- Modify: `crates/client/src/renderer/commander/height.rs`

**Interfaces:**
- Produces: `pub struct StudTops { pub surface: [f32; COLUMNS] }` (per 1 m block column of the 16 m chunk: highest `plate_units(top_plate)` over its 2×2 stud columns, units above the stud chunk floor; `0.0` = none); `pub fn stud_chunk_tops(chunk: &StudChunk) -> StudTops`; `HeightGrid::insert_stud_chunk(&mut self, entity: Entity, key: [i32; 3] /* world cx, stud layer, cz */, tops: StudTops)`; `remove_entity` also clears stud entries. New fields `stud_stacks: HashMap<(i32, i32), BTreeMap<i32, StudTops>>`, `stud_by_entity: HashMap<Entity, [i32; 3]>`. `recompute` takes, per block column, the max of the voxel top and `layer · 240 + tops.surface` (stud surface = ground = surface; no water).
- `update_height_grid` gains `Query<(Entity, &SimStudChunk, &SimEntity), Changed<SimStudChunk>>` and `RemovedComponents<SimStudChunk>`; world chunk x/z from `SimEntity.region.world_offset()` + `coords`.

- [ ] **Step 1: Failing tests** (`height.rs` tests)

```rust
#[test] fn stud_chunk_sets_surface() // Filler [32, 50, 32] at layer 0: surface_at(centre) == Some(plate_units(50))
#[test] fn stud_and_voxel_take_the_max()
#[test] fn removing_stud_entity_clears_columns()
```

- [ ] **Step 2: Ask** (client tests, as Task 12) → run `just test-one client "--lib commander::height"` before/after.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Verify** — `just smoke`.
- [ ] **Step 5: Commit** — `feat(client): feed the height grid from stud chunks`.

---

### Task 14: Launch — `client debug studs` and `just run-studs`

**Files:**
- Modify: `crates/client/src/debug_launch.rs` (`DebugScenario::Studs` ~:97; `USAGE` ~:74 adds `client debug studs`; `parse_args` ~:142; `boot` ~:383; `auto_embark` ~:581), `justfile` (recipe `run-studs` after `run` ~:197)

**Interfaces:**
- `boot` for `Studs`: create `worlds/debug-studs/` (only `server.log` goes there), set `PendingWorld { entry_dir: "worlds/debug-studs", terrain: "studs".into(), name: "Studs" }` (`LocalHost::start_on` passes `terrain` verbatim as `VOXEL_WORLD_FILE`), and no LOD rings for this session (the `VOXEL_LOD_RINGS` override in `world_scale.rs`, set to none before the scale is read — confirm in `world_scale.rs:~158` that it is read once per process, after `boot`).
- `auto_embark` for `Studs`: no world map arrives; once connected and the region snapshot is in, insert `Embarked` itself, as the `EmbarkMove::PlayHere` arm (~:670) does.
- Recipe:

```just
# Debug launch into the toy-brick test world: 3x3 regions of stud chunks, no store, no menu.
run-studs *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export VOXEL_REGION_BLOCKS={{play_blocks}}
    if [ -n "{{adapter}}" ]; then export WGPU_ADAPTER_NAME="{{adapter}}"; fi
    {{cap}} {{cargo}} build --bin client --bin server
    RUST_BACKTRACE=1 ./target/debug/client debug studs {{args}}
```

- [ ] **Step 1: Failing test** — in `debug_launch.rs` tests (next to the existing `parse_args` cases ~:707): `assert_eq!(parse_args(&["debug", "studs"]), Ok(Some(DebugScenario::Studs)));` and that `"debug studs --reset"` is an error (no flags).
- [ ] **Step 2: Ask** (client tests) → `just test-one client "--lib debug_launch"`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Verify** — `just smoke`; then **ask the user** before launching: `just run-studs`. Expected: the loading cover clears; green/tan brick hills with seam lines in a 3×3-region area, studs near the camera focus only; `server.log` has `[server] world: studs` and nine `region_genesis` perf events; no LOD jobs. Report region genesis ms and first-frame-to-cover-clear time.
- [ ] **Step 5: Commit** — `feat(client): client debug studs and just run-studs`.

---

### Task 15: Build mode — pick, ghost, place, remove

**Files:**
- Modify: `crates/game/src/stud.rs` (ray pick), `crates/game/tests/stud.rs` (tests)
- Create: `crates/client/src/renderer/stud/build.rs`; modify `stud/mod.rs`

**Interfaces:**
- Produces (game, pure): `pub struct StudHit { pub chunk: StudChunkCoords, pub cell: [u8; 3], pub normal: [i8; 3], pub t: f32 }`; `pub fn raycast_studs<'a>(chunks: impl Fn(StudChunkCoords) -> Option<&'a StudChunk>, origin: Vector, dir: Vector, reach: f32) -> Option<StudHit>` — Amanatides–Woo over the anisotropic cell grid (8, `plate_units`, 8) in region-local units, crossing chunks; occupancy from each chunk's `occupancy()` (cache per call); stops at `reach` and at the region edge.
- Produces: `pub fn placement_for(hit: &StudHit, part: u16, rot: u8, color: u16) -> Option<(StudChunkCoords, StudBrick)>` — target cell = `cell + normal` (wrapping into the neighbouring chunk/layer by euclidean division; `None` outside the region); `min` = target cell, then shifted so the brick's footprint is centred under the cursor on x/z (`min.x -= (size.x - 1) / 2`, same for z, clamped ≥ 0).
- Client (`build.rs`): `#[derive(Resource)] StudBuild { on: bool, part: u16, rot: u8, color_ix: usize }` (session-reset); keys (dev-style, no Slint): `B` toggles, `R` rotates, `[`/`]` cycle `BUILD_PARTS` (all `STUD_PARTS` in catalog order), `C` cycles `BUILD_COLORS: [u16; 8] = [15, 0, 4, 1, 14, 2, 71, 19]`. Ray from `designate::cursor_ray`. Ghost: one entity with the part's hi baked mesh (whole part, untagged, unculled) at the placement, alpha-blended material, red (`StudError` from `clone().place`) or the brick colour. Left click sends `Order::PlaceStud`, right click `Order::RemoveStud { chunk: hit.chunk, cell: hit.cell }`, both via `GameEvents` (`events.0.send(GameEventKind::Order(client, order))`, pattern at `designate.rs:~616`). While on, the designation tools ignore clicks (gate them on `!StudBuild.on`). `CursorPolicy` stays the only cursor writer.

- [ ] **Step 1: Failing game tests** (`tests/stud.rs`)

```rust
#[test] fn ray_hits_top_of_brick() // 3001 at [0,0,0]; ray down at (4, 100, 4): cell [0,2,0], normal [0,1,0]
#[test] fn ray_hits_side_across_chunks() // brick at x 0 of chunk (1,0,0); ray +x from chunk (0,0,0): chunk (1,0,0), normal [-1,0,0]
#[test] fn ray_misses_beyond_reach()
#[test] fn placement_on_top_lands_on_next_plate() // placement_for(top hit, 3001, 0, 4): min y == 3
#[test] fn placement_off_chunk_edge_wraps_to_neighbour()
```

- [ ] **Step 2: Run** — `just test-one game "--test it stud::"` → FAIL; implement; → PASS.
- [ ] **Step 3: Implement `build.rs`.** Verify `just smoke`.
- [ ] **Step 4: Ask, then look** — `just run-studs`: `B`, place/remove bricks, rotate, change colour; edits survive a server round trip (no snap-back after ~1 s). A second client seeing the edits is covered by Task 8's `replayed_orders_match_server`; a two-client `run-studs` recipe is a follow-up for `TODO.md`.
- [ ] **Step 5: Commit** — `feat(client): stud build mode — pick, ghost, place, remove`.

---

### Task 16: End check

- [ ] `just test-one game "--test it stud:: stud_collider:: stud_ops:: stud_orders:: body_scale::"`, `just test-one worldgen "--test it stud_world::"`, `just test-one server "--test it stud_world::" -- --include-ignored`, `just test-one physics-next "--test it stud_voxels::"` → all PASS.
- [ ] `just determinism-wasm`, `just test-threads`, `just smoke` → PASS.
- [ ] A voxel world still plays: `just run` (ask first) — loads and walks as before.
- [ ] `just ldraw-bake --check` → no diff.
- [ ] With the user: `just run-studs` look; record genesis ms per region, snapshot size, time to first frame, frame time at the stud LOD radius, and the step-height feel (decision point from Task 3).
- [ ] Update `TODO.md` with the open decisions: step height (one brick vs plate + jump), split-on-overlap placement (LEGO Worlds' evict-and-decompose upgrade to `place`), glTF vs custom `parts.bin`, walk-speed scaling, transparent colours drawn opaque, client avatar/selection/cut-entity sizes still VOXEL outside `avatar.rs`.

---

## Open decision points carried (do not resolve during implementation)

- **Step height:** `STUD_STEP_HEIGHT_M = 0.65` (auto-step one brick). LEGO-style controllers auto-step a plate and jump a brick. Decide at first playtest (Task 16).
- **Split-on-overlap placement:** `place` refuses overlaps; LEGO Worlds splits the old brick. Later upgrade; the data model allows it.
- **glTF vs `parts.bin`:** the bake's `emit.rs` is the only place that changes if the user picks glTF (same named primitives); `StudAssets` loading is the only client change.
