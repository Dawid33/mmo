# Water Prototype Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an offline, headless, throwaway Rust prototype. It compares two models on the
same fixtures:
- the July plan's per-cell water in a dense grid ("cell-only");
- the synthesis's **water bodies + frontier CA** ("bodies").

It proves that the bodies model is exactly mass-conserving, exactly undoable, bit-identical on
native and wasm32, and cheaper. Since 2026-11-06 it must also show that:
- the player actions (dam breach, gates, pumps, scoops) give exact results;
- two worlds joined at a lagged seam conserve mass and stay damped;
- a brick journal is a cheaper undo shape than per-cell entries.

**Architecture:** A standalone crate at `prototypes/water/` with **no dependencies** and its own
`[workspace]`, so it never touches the engine build. The world is a 64³ grid of 1 m cells, and
all state is integer.

- Every wet cell is either a **CA cell** holding `water: u16` (0..=4096) or a **member** of a
  `Body`. A body pools its water in one `volume: u64`, and its level is derived from volume and
  per-layer member counts.
- One water step visits only *awake* CA cells, in ascending index order. Each cell settles its
  six edges (CA↔CA, CA↔body). The step then absorbs or seeds cells, expands bodies, and wakes
  shores.
- Every mutation goes through logging setters, so a step can be undone exactly (as the engine's
  `#[rollback]` layer does).
- Scenarios run both models and report cost, undo bytes, state size, mass and hashes.
- A wasm32 build run under node must reproduce the native hashes.

**Tech Stack:** the repo's pinned toolchain (`rust-toolchain.toml`), edition 2024, std only. The recipes pin cores with `just`'s `cap`
prefix. Node (v25, already installed) runs the wasm32-unknown-unknown build.

**Spec:** `docs/superpowers/specs/2026-09-22-voxel-water-efficiency/00-synthesis.md`. Read the
"User decisions, 2026-09-22" section first; it overrides the rest. **Amended 2026-11-06**
(Tasks 6–11, measurement moved to Task 12) from
`docs/superpowers/specs/2026-11-06-water-revisit/00-synthesis.md`, which adds four things:
- the flow models: a checkerboard frontier CA, and one start-of-step edge rule for gates,
  pumps and seams;
- what players can do with water, run as scenarios;
- brick-journal undo;
- film evaporation into the ledger (off by default). Background:
`02-algorithms-and-data-structures.md` §5 (body mechanics) and §6 (scenarios).

## Global Constraints

- **Deterministic in-sim water, not server-solved** (user decision). All authoritative state and
  rules are integer. No floats anywhere in state or rules (floats only in report printing).
- **Water is finite** (user decision). There are no infinite bodies and no sources. Water enters
  only through `Edit::Pour`, which the ledger `poured` counts, so `mass() == initial + poured`
  must hold exactly. From Task 7 on, the identity is
  `accounted() == initial + poured`, where `accounted = mass + evaporated + scooped`. A seam pair
  also counts its in-flight arrivals.
- **Canonical order everywhere:** ascending cell index, which is y-major:
  `idx = (y * Z + z) * X + x`, and `BTreeMap` for bodies.
  - From Task 6 on, the CA pass visits even parity, then odd, each ascending.
  - Links go in ascending `LinkId`.
  - **No `HashMap`/`HashSet`** anywhere.
  - Horizontal edge order rotates with the step (`horizontal_order(step)`).
- **Work budgets count cells, never time** (`EXPAND_BUDGET`, `EXPAND_BACKOFF` in steps).
- **Tunables are named constants in `src/lib.rs`** (user preference: expose tunables as named
  constants):

  | Constant | Value |
  |---|---|
  | `FULL` | 4096 |
  | `FILM` | 256 |
  | `MAX_FLOW` | 1024 |
  | `SLEEP_STEPS` | 4 |
  | `SEED_QUIET` | 2 |
  | `ABSORB_TOL` | 1 |
  | `EXPAND_AREA_FACTOR` | 4 |
  | `EXPAND_BUDGET` | 16384 |
  | `EXPAND_BACKOFF` | 16 |
- **Standalone crate.** `prototypes/water/Cargo.toml` has an empty `[workspace]` table and no
  dependencies. Do not add it to the root workspace, and do not touch any `crates/*` file.
- **Build and test only through the `just` recipes** added here: `water-proto-test`,
  `water-proto`, `water-proto-wasm`. They carry the `cap` core-pinning prefix. Never run the
  game/server/client suites for this work.
- **Ask the user before any timing run** (`just water-proto` without `--golden`). The user runs
  their own benchmarks on this box. Tests and `--golden` hash runs are fine.
- **Git:**
  - Work on `develop` in the main checkout. No worktrees.
  - The tree has unrelated uncommitted user changes, so **never** `git add -A` / `git add .`.
    Add only the paths each task names.
  - Before committing `justfile`, run `git diff justfile`. If the diff shows only this plan's
    recipes, commit it with the task. If it holds the user's own edits, leave it uncommitted
    and list it in the final report. Ignore the crate's `target/` with a crate-local
    `prototypes/water/.gitignore`, not the root one.
  - End every commit message with
    `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

These cases are not the main scenarios, but a user of the model would hit them first. Each has
a test in the owning task.

1. **Water at the box boundary.** Pouring into a corner cell (x=0, z=0) must treat outside as
   solid, conserve mass exactly, and settle. Test `pour_in_corner_conserves` (Task 3).
2. **Placing a block into a body.**
   - With a sealed ceiling, it is rejected with `NoRoom` and the state hash is unchanged.
   - Into a lake with headroom, the displaced cell's water raises the level by exactly
     `FULL / area`.

   Tests `place_into_sealed_body_rejected` and `place_into_full_lake_raises_level` (Task 3).
3. **A hole in a lake's floor** drains the lake downward into a cavity. The emptied lake body
   is removed: no zombie body with 0 cells. The cavity water ends as one body. Test
   `floor_hole_drains_lake_into_cavity` (Task 4).
4. **Overflow from a pit onto a flat plain.** It must not flood-fill the plain layer every step:
   refusals back off by `EXPAND_BACKOFF`. The spilled water stays a CA film. Test
   `plain_overflow_backs_off` (Task 4).
5. **Undo across structural changes.** Rolling back a step that splits, merges or deletes a
   body (id counter plus body before-images) must restore the hash exactly. Tests
   `undo_across_split` and `undo_is_exact_all_scenarios`, which covers the merges in
   `small_spill`/`u_tube`/`big_drain` (Task 5).

---

## The model, in one page (read before any task)

**Units.** 1 cell = 1 m³ = `FULL` = 4096 units. A body's `level` is `top * FULL + fill` of its
top layer.

**CA cells:** non-solid cells with `owner == NO_BODY`.
- `water: u16` holds 0..=FULL.
- A CA cell is processed only while its `active` bit is set.
- It goes to sleep after `SLEEP_STEPS` steps with no transfer touching it (`last_touch`).

**Bodies.** `Body { volume, top, area[y], below, cells, bbox, shore, expand_retry }`.
- Members are the cells whose `owner` is the body's id. Members hold no `water`; the body's
  `volume` holds it all.
- Every layer below `top` is virtually full, and the top layer holds `frac = volume - FULL*below`
  spread evenly. So `virtual_at(y)` is `FULL` below the top, `frac / area[top]` at the top, and 0
  above.
- **Invariant:** `FULL*below < volume <= FULL*(below + area[top])`.
  - If `frac` reaches 0, the top layer's cells leave the body (`drop_empty_layers`), which may
    split it (`split_if_disconnected`).
  - A body with 0 cells is removed.

**Edges, settled from an awake CA cell `c`:**

| Edge | Rule |
|---|---|
| **down** into CA | `q = min(w_c, FULL - w_below)` |
| **down** onto a body member at the body's top layer | `q = min(w_c, room)`; if `room == 0`, request an expansion |
| **up** from a CA cell above | the above cell falls into `c` |
| **up** from a member above (hole in a lake floor) | `q = min(virtual, FULL - w_c, MAX_FLOW, volume)` from the body into `c` |
| **horizontal** CA↔CA | Water moves from higher to lower (symmetric, either end can be the processed one). To an **unsupported** receiver (the cell below is a non-full CA cell): `q = ceil(diff/2)`. To a supported receiver: `q = diff/2`, but only if the donor has `> FILM` (film rule) and `diff >= 2`. Capped at `MAX_FLOW`. |
| **horizontal** CA↔member | Compare `w_c` with `virtual_at(y_c)`. If the body is higher by more than `ABSORB_TOL`, it flows into `c` (an outlet). If `c` is higher and `y_c == top`, it flows into the body, limited by `room`. |

**After the edges of `c`** (bodies mode only):

- **Absorb.** `c` joins a touching body if all of these hold:
  - it rests on solid or a member;
  - it holds water;
  - *every* body touching it at or below that body's top is level with `c`: `w_c == FULL` below
    a body's top, or within `ABSORB_TOL` of `virtual_at` at the top.

  If several bodies touch it, they merge. Otherwise `c` stays a *pipe* cell carrying flow
  between them.
- **Seed.** If no body touches `c`, and `c` is full, rests on solid, and has been quiet for
  `SEED_QUIET` steps, then the connected patch of such cells in its layer becomes a new body.

**After all cells** (bodies mode only):

- Requested expansions run. `expand_layer` floods layer `top+1` from above the top-layer members
  through supported CA cells, and absorbs them and their water. It is refused unless one of
  these holds:
  - the new layer is at most `EXPAND_AREA_FACTOR ×` the top area (a bowl or wall);
  - the layer's CA water averages at least `FILM` per cell.

  A plain fails both, and a refusal backs off for `EXPAND_BACKOFF` steps.
- Bodies whose volume rose wake their `shore`: the non-member, non-solid cells next to members
  at spill edges and edit holes.

**Merge** (instant equalisation, communicating vessels):
1. Union the members and volumes.
2. `relevel`: cut membership back to the lower of the two tops.
3. Regrow layer by layer (forced expansion) while `volume > capacity`.

**Edits:** `Remove(i)`, `Place(i)` and `Pour(i, q)` are applied at the start of a step, inside
the same undo log.

---

## File structure

```
prototypes/water/
  Cargo.toml            standalone package + empty [workspace], crate-type rlib+cdylib
  .gitignore            target/
  wasm_golden.mjs       node runner: prints "<scenario> <mode> <hash>" from the wasm build
  src/lib.rs            constants, Mode, module list
  src/grid.rs           Dims (indexing, neighbours), Dir, Bits (bitset)
  src/state.rs          World (rollback state + scratch), Undo log, Stats, Fnv hash, mass
  src/body.rs           Body data + math; seed/expand/drop/split/merge/relevel; invariants
  src/fixture.rs        terrain/water builders for fixtures (unlogged initial snapshot)
  src/ca.rs             the water step: edges, absorb/seed, after-step
  src/edit.rs           Edit::{Remove, Place, Pour}
  src/metrics.rs        column surface, surface spread, dense/sparse byte estimates
  src/link.rs           (Task 7) gates, pumps: the start-of-step edge rule; head/avail/room/take/give
  src/seam.rs           (Task 10) two worlds joined at a lagged, capped seam
  src/scenario.rs       the six scenarios, step_once, run -> Report
  src/wasm.rs           #[cfg(wasm32)] exported golden(scenario, mode) -> u64
  src/bin/water_proto.rs CLI table / --golden
  tests/t1_state.rs  tests/t2_bodies.rs  tests/t3_ca.rs  tests/t4_scenarios.rs  tests/t5_rollback.rs
  tests/t6_order.rs  tests/t7_links.rs  tests/t8_journal.rs  tests/t9_evaporation.rs
  tests/t10_seam.rs  tests/t11_players.rs
```

---

### Task 1: Crate scaffold, grid, rollback state, body data

**Files:**
- Create: `prototypes/water/Cargo.toml`, `prototypes/water/.gitignore`,
  `prototypes/water/src/lib.rs`, `src/grid.rs`, `src/state.rs`, `src/body.rs`
- Test: `prototypes/water/tests/t1_state.rs`
- Modify (do not commit): `justfile`, adding the recipe `water-proto-test`

**Interfaces:**
- Produces:
  - `grid`: `Dims { x, y, z }` with `cube`, `len`, `layer`, `idx`, `xyz`, `y_of`, `step`; `Dir`;
    `HORIZONTAL`; `ALL`; `horizontal_order(u32) -> [Dir; 4]`; `Bits` with `new`, `get`, `set`,
    `words`, `ones`, `count`.
  - `state`: `BodyId`, `NO_BODY`, `Undo` + `bytes()`, `Stats`, `Fnv`, `World`, with:
    - fields `dims mode solid water owner active last_touch bodies next_id step poured stats`;
    - `new`, `is_ca`, `supported`, `set_water`, `set_owner`, `set_solid`, `set_active`,
      `touch`, `set_step`, `add_poured`, `alloc_id`, `insert_body`, `remove_body`,
      `body_mut`, `begin_log`, `end_log`, `undo`, `hash`, `mass`.
  - `body::Body` with `empty`, `top_area`, `capacity`, `frac`, `room`, `virtual_at`, `level`,
    `byte_size`, `hash_into`, `grow_bbox`.
  - The crate constants and `Mode { CellOnly, Bodies }` with `name()`.

- [ ] **Step 1: Create the crate files**

`prototypes/water/Cargo.toml`:
```toml
[package]
name = "water_proto"
version = "0.0.0"
edition = "2024"
publish = false

# Throwaway prototype: standalone so it never joins the engine workspace build.
[workspace]

[lib]
crate-type = ["rlib", "cdylib"]

[profile.release]
debug = 1
```

`prototypes/water/.gitignore`:
```
target/
```

`prototypes/water/src/lib.rs`:
```rust
//! Offline water prototype: water bodies + frontier CA versus cell-only CA.
//! Throwaway. Plan: docs/superpowers/plans/2026-09-22-water-prototype.md.
//! Everything authoritative is integer and iterated in ascending cell index.

pub mod body;
pub mod grid;
pub mod state;

/// Units in one full 1 m³ cell (16³ voxels).
pub const FULL: u16 = 4096;
/// Film depth (1/16 m): a CA cell holding this much or less stops spreading on solid footing.
pub const FILM: u16 = 256;
/// Most units one edge moves in one step (1/4 m³).
pub const MAX_FLOW: u16 = 1024;
/// Steps without a transfer before a CA cell sleeps.
pub const SLEEP_STEPS: u32 = 4;
/// Steps a full CA cell on solid ground must be quiet before it seeds a body.
pub const SEED_QUIET: u32 = 2;
/// Units of level mismatch still counted as "level" for absorb and outlet flow.
pub const ABSORB_TOL: u16 = 1;
/// A new top layer up to this multiple of the current top area is a bowl/wall: always expand.
pub const EXPAND_AREA_FACTOR: u32 = 4;
/// Most cells one expansion flood may visit before it is refused.
pub const EXPAND_BUDGET: u32 = 16384;
/// Steps a refused expansion waits before retrying.
pub const EXPAND_BACKOFF: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The July plan: every wet cell is a CA cell.
    CellOnly,
    /// Settled water pools into bodies; only the frontier is CA.
    Bodies,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::CellOnly => "cell",
            Mode::Bodies => "bodies",
        }
    }
}
```

`prototypes/water/src/grid.rs`:
```rust
//! Grid geometry and a bitset. Index order is y-major, `(y * z_len + z) * x_len + x`,
//! so ascending index visits the bottom layer first. Every "canonical order" in the
//! prototype is ascending index.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dims {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

impl Dims {
    pub const fn cube(n: u32) -> Self {
        Dims { x: n, y: n, z: n }
    }
    pub const fn len(self) -> usize {
        (self.x * self.y * self.z) as usize
    }
    pub const fn layer(self) -> u32 {
        self.x * self.z
    }
    pub fn idx(self, x: u32, y: u32, z: u32) -> u32 {
        debug_assert!(x < self.x && y < self.y && z < self.z);
        (y * self.z + z) * self.x + x
    }
    pub fn xyz(self, i: u32) -> (u32, u32, u32) {
        let x = i % self.x;
        let r = i / self.x;
        (x, r / self.z, r % self.z)
    }
    pub fn y_of(self, i: u32) -> u32 {
        i / self.layer()
    }
    /// Face neighbour, or `None` outside the box. The box is closed: callers
    /// treat `None` as solid.
    pub fn step(self, i: u32, d: Dir) -> Option<u32> {
        let (x, y, z) = self.xyz(i);
        let (x, y, z) = match d {
            Dir::Down => (x, y.checked_sub(1)?, z),
            Dir::Up => (x, (y + 1 < self.y).then_some(y + 1)?, z),
            Dir::PosX => ((x + 1 < self.x).then_some(x + 1)?, y, z),
            Dir::NegX => (x.checked_sub(1)?, y, z),
            Dir::PosZ => (x, y, (z + 1 < self.z).then_some(z + 1)?),
            Dir::NegZ => (x, y, z.checked_sub(1)?),
        };
        Some(self.idx(x, y, z))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Down,
    Up,
    PosX,
    NegX,
    PosZ,
    NegZ,
}

pub const HORIZONTAL: [Dir; 4] = [Dir::PosX, Dir::PosZ, Dir::NegX, Dir::NegZ];
pub const ALL: [Dir; 6] = [Dir::Down, Dir::PosX, Dir::PosZ, Dir::NegX, Dir::NegZ, Dir::Up];

/// Horizontal visiting order, rotated by the step so no direction is favoured.
pub fn horizontal_order(step: u32) -> [Dir; 4] {
    let r = (step % 4) as usize;
    core::array::from_fn(|k| HORIZONTAL[(k + r) % 4])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bits {
    words: Vec<u64>,
}

impl Bits {
    pub fn new(len: usize) -> Self {
        Bits { words: vec![0; len.div_ceil(64)] }
    }
    pub fn get(&self, i: u32) -> bool {
        ((self.words[(i / 64) as usize] >> (i % 64)) & 1) == 1
    }
    pub fn set(&mut self, i: u32, v: bool) {
        let w = &mut self.words[(i / 64) as usize];
        let m = 1u64 << (i % 64);
        if v { *w |= m } else { *w &= !m }
    }
    pub fn words(&self) -> &[u64] {
        &self.words
    }
    /// Set bits, ascending.
    pub fn ones(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for (wi, &word) in self.words.iter().enumerate() {
            let mut word = word;
            while word != 0 {
                out.push(wi as u32 * 64 + word.trailing_zeros());
                word &= word - 1;
            }
        }
        out
    }
    pub fn count(&self) -> u64 {
        self.words.iter().map(|w| w.count_ones() as u64).sum()
    }
}
```

`prototypes/water/src/state.rs`:
```rust
//! The rollback state of one region's water, the per-step undo log, and hashing.
//! Scratch fields (log, imaged, touched, expand_requests, mark) are rebuilt every
//! step and are never hashed; `stats` is diagnostics only.

use std::collections::BTreeMap;

use crate::body::Body;
use crate::grid::{Bits, Dims, Dir};
use crate::{FULL, Mode};

pub type BodyId = u32;
pub const NO_BODY: BodyId = 0;

/// One before-image. `World::undo` replays a step's log in reverse.
#[derive(Clone, Debug)]
pub enum Undo {
    Water(u32, u16),
    Owner(u32, BodyId),
    Solid(u32, bool),
    Active(u32, bool),
    Touch(u32, u32),
    Body(BodyId, Option<Box<Body>>),
    NextId(BodyId),
    Step(u32),
    Poured(u64),
}

impl Undo {
    /// What this entry would cost in a packed log: 1 tag byte + payload.
    pub fn bytes(&self) -> usize {
        match self {
            Undo::Water(..) => 7,
            Undo::Owner(..) | Undo::Touch(..) | Undo::Poured(..) => 9,
            Undo::Solid(..) | Undo::Active(..) => 6,
            Undo::NextId(..) | Undo::Step(..) => 5,
            Undo::Body(_, b) => 5 + b.as_ref().map_or(0, |b| b.byte_size()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub cells_visited: u64,
    pub transfers: u64,
    pub flood_cells: u64,
    pub seeds: u64,
    pub absorbs: u64,
    pub expansions: u64,
    pub expansion_refusals: u64,
    pub layer_drops: u64,
    pub splits: u64,
    pub merges: u64,
}

/// FNV-1a, 64-bit: tiny, dependency-free, identical on every target.
pub struct Fnv(u64);

impl Fnv {
    pub fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    pub fn u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    pub fn u32(&mut self, v: u32) {
        self.u64(v as u64)
    }
    pub fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for Fnv {
    fn default() -> Self {
        Self::new()
    }
}

pub struct World {
    pub dims: Dims,
    pub mode: Mode,
    pub solid: Bits,
    pub water: Vec<u16>,
    pub owner: Vec<BodyId>,
    pub active: Bits,
    pub last_touch: Vec<u32>,
    pub bodies: BTreeMap<BodyId, Body>,
    pub next_id: BodyId,
    pub step: u32,
    /// Units added by `Edit::Pour` since the fixture was built (the mass ledger).
    pub poured: u64,
    pub stats: Stats,
    log: Option<Vec<Undo>>,
    imaged: Vec<BodyId>,
    /// Bodies changed this step, with their volume before the first change.
    pub(crate) touched: Vec<(BodyId, u64)>,
    pub(crate) expand_requests: Vec<BodyId>,
    /// Flood-fill visited marks, generation-stamped (scratch).
    pub(crate) mark: Vec<u32>,
    pub(crate) mark_gen: u32,
}

impl World {
    pub fn new(dims: Dims, mode: Mode) -> Self {
        let n = dims.len();
        World {
            dims,
            mode,
            solid: Bits::new(n),
            water: vec![0; n],
            owner: vec![NO_BODY; n],
            active: Bits::new(n),
            last_touch: vec![0; n],
            bodies: BTreeMap::new(),
            next_id: 1,
            step: 0,
            poured: 0,
            stats: Stats::default(),
            log: None,
            imaged: Vec::new(),
            touched: Vec::new(),
            expand_requests: Vec::new(),
            mark: vec![0; n],
            mark_gen: 0,
        }
    }

    /// Non-solid and not a body member.
    pub fn is_ca(&self, i: u32) -> bool {
        !self.solid.get(i) && self.owner[i as usize] == NO_BODY
    }

    /// Water in `i` can rest: below is the box floor, solid, a member, or a full CA cell.
    pub fn supported(&self, i: u32) -> bool {
        match self.dims.step(i, Dir::Down) {
            None => true,
            Some(b) => {
                self.solid.get(b)
                    || self.owner[b as usize] != NO_BODY
                    || self.water[b as usize] == FULL
            }
        }
    }

    fn push(&mut self, u: Undo) {
        if let Some(log) = &mut self.log {
            log.push(u);
        }
    }

    pub fn set_water(&mut self, i: u32, v: u16) {
        let old = self.water[i as usize];
        if old != v {
            self.push(Undo::Water(i, old));
            self.water[i as usize] = v;
        }
    }
    pub fn set_owner(&mut self, i: u32, v: BodyId) {
        let old = self.owner[i as usize];
        if old != v {
            self.push(Undo::Owner(i, old));
            self.owner[i as usize] = v;
        }
    }
    pub fn set_solid(&mut self, i: u32, v: bool) {
        let old = self.solid.get(i);
        if old != v {
            self.push(Undo::Solid(i, old));
            self.solid.set(i, v);
        }
    }
    pub fn set_active(&mut self, i: u32, v: bool) {
        let old = self.active.get(i);
        if old != v {
            self.push(Undo::Active(i, old));
            self.active.set(i, v);
        }
    }
    /// Record that a transfer touched `i` this step (keeps it awake).
    pub fn touch(&mut self, i: u32) {
        let old = self.last_touch[i as usize];
        if old != self.step {
            self.push(Undo::Touch(i, old));
            self.last_touch[i as usize] = self.step;
        }
    }
    pub fn set_step(&mut self, s: u32) {
        self.push(Undo::Step(self.step));
        self.step = s;
    }
    pub fn add_poured(&mut self, q: u64) {
        self.push(Undo::Poured(self.poured));
        self.poured += q;
    }
    pub fn alloc_id(&mut self) -> BodyId {
        let id = self.next_id;
        self.push(Undo::NextId(id));
        self.next_id += 1;
        id
    }

    /// Before-image body `id` (or its absence) once per logged step.
    fn image(&mut self, id: BodyId) {
        if self.log.is_some() && !self.imaged.contains(&id) {
            let img = self.bodies.get(&id).cloned().map(Box::new);
            self.push(Undo::Body(id, img));
            self.imaged.push(id);
        }
    }
    fn note_touched(&mut self, id: BodyId) {
        if !self.touched.iter().any(|t| t.0 == id) {
            let v = self.bodies.get(&id).map_or(0, |b| b.volume);
            self.touched.push((id, v));
        }
    }
    pub fn insert_body(&mut self, b: Body) {
        self.image(b.id);
        self.bodies.insert(b.id, b);
    }
    pub fn remove_body(&mut self, id: BodyId) -> Body {
        self.image(id);
        self.bodies.remove(&id).expect("remove_body: no such body")
    }
    pub fn body_mut(&mut self, id: BodyId) -> &mut Body {
        self.image(id);
        self.note_touched(id);
        self.bodies.get_mut(&id).expect("body_mut: no such body")
    }

    pub fn begin_log(&mut self) {
        self.log = Some(Vec::new());
        self.imaged.clear();
    }
    pub fn end_log(&mut self) -> Vec<Undo> {
        self.imaged.clear();
        self.log.take().unwrap_or_default()
    }
    /// Restore the state from before the logged step.
    pub fn undo(&mut self, log: Vec<Undo>) {
        assert!(self.log.is_none(), "undo while a step is being logged");
        for u in log.into_iter().rev() {
            match u {
                Undo::Water(i, v) => self.water[i as usize] = v,
                Undo::Owner(i, v) => self.owner[i as usize] = v,
                Undo::Solid(i, v) => self.solid.set(i, v),
                Undo::Active(i, v) => self.active.set(i, v),
                Undo::Touch(i, v) => self.last_touch[i as usize] = v,
                Undo::Body(id, Some(b)) => {
                    self.bodies.insert(id, *b);
                }
                Undo::Body(id, None) => {
                    self.bodies.remove(&id);
                }
                Undo::NextId(v) => self.next_id = v,
                Undo::Step(v) => self.step = v,
                Undo::Poured(v) => self.poured = v,
            }
        }
    }

    /// Hash of all rollback state (not scratch, not stats).
    pub fn hash(&self) -> u64 {
        let mut h = Fnv::new();
        h.u32(self.step);
        h.u32(self.next_id);
        h.u64(self.poured);
        for w in self.solid.words() {
            h.u64(*w);
        }
        for w in self.active.words() {
            h.u64(*w);
        }
        for (i, &v) in self.water.iter().enumerate() {
            if v != 0 {
                h.u32(i as u32);
                h.u32(v as u32);
            }
        }
        for (i, &o) in self.owner.iter().enumerate() {
            if o != NO_BODY {
                h.u32(i as u32);
                h.u32(o);
            }
        }
        for (i, &t) in self.last_touch.iter().enumerate() {
            if t != 0 {
                h.u32(i as u32);
                h.u32(t);
            }
        }
        for b in self.bodies.values() {
            b.hash_into(&mut h);
        }
        h.finish()
    }

    /// All water: CA cells plus body volumes.
    pub fn mass(&self) -> u64 {
        self.water.iter().map(|&w| w as u64).sum::<u64>()
            + self.bodies.values().map(|b| b.volume).sum::<u64>()
    }
}
```

`prototypes/water/src/body.rs` (data and math only; Task 2 appends the operations):
```rust
//! Water bodies: a connected pool whose water is one `volume`. Every member layer
//! below `top` is virtually full; the top layer shares `frac` evenly.

use crate::FULL;
use crate::grid::Dims;
use crate::state::{BodyId, Fnv};

pub(crate) const F: u64 = FULL as u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Body {
    pub id: BodyId,
    pub volume: u64,
    /// Highest member layer.
    pub top: u32,
    /// Member count per layer (len = dims.y).
    pub area: Vec<u32>,
    /// Members strictly below `top` (= cells - area[top]).
    pub below: u64,
    pub cells: u64,
    /// Inclusive bounding box of members, (x, y, z). Grows only.
    pub lo: [u32; 3],
    pub hi: [u32; 3],
    /// Sorted non-member, non-solid cells to wake when the level rises (spill edges, edit holes).
    pub shore: Vec<u32>,
    /// A refused expansion is not retried before this step.
    pub expand_retry: u32,
}

impl Body {
    pub fn empty(id: BodyId, dims: Dims) -> Self {
        Body {
            id,
            volume: 0,
            top: 0,
            area: vec![0; dims.y as usize],
            below: 0,
            cells: 0,
            lo: [u32::MAX; 3],
            hi: [0; 3],
            shore: Vec::new(),
            expand_retry: 0,
        }
    }
    pub fn top_area(&self) -> u64 {
        self.area[self.top as usize] as u64
    }
    pub fn capacity(&self) -> u64 {
        F * (self.below + self.top_area())
    }
    /// Water in the top layer.
    pub fn frac(&self) -> u64 {
        self.volume.saturating_sub(F * self.below)
    }
    pub fn room(&self) -> u64 {
        self.capacity().saturating_sub(self.volume)
    }
    /// The water a member at layer `y` holds, as a CA cell would see it.
    pub fn virtual_at(&self, y: u32) -> u16 {
        if y < self.top {
            FULL
        } else if y == self.top && self.top_area() > 0 {
            (self.frac() / self.top_area()) as u16
        } else {
            0
        }
    }
    /// Surface height in units.
    pub fn level(&self) -> u64 {
        self.top as u64 * F + self.virtual_at(self.top) as u64
    }
    pub fn byte_size(&self) -> usize {
        48 + 4 * self.area.len() + 4 * self.shore.len()
    }
    pub fn hash_into(&self, h: &mut Fnv) {
        h.u32(self.id);
        h.u64(self.volume);
        h.u32(self.top);
        for a in &self.area {
            h.u32(*a);
        }
        h.u64(self.below);
        h.u64(self.cells);
        for v in self.lo.iter().chain(&self.hi) {
            h.u32(*v);
        }
        for s in &self.shore {
            h.u32(*s);
        }
        h.u32(self.expand_retry);
    }
    pub(crate) fn grow_bbox(&mut self, (x, y, z): (u32, u32, u32)) {
        for (k, v) in [x, y, z].into_iter().enumerate() {
            self.lo[k] = self.lo[k].min(v);
            self.hi[k] = self.hi[k].max(v);
        }
    }
}
```

- [ ] **Step 2: Add the test recipe to `justfile`** (do not commit `justfile`)

Put it next to the `forge-proto` recipe:
```just
# Water prototype (prototypes/water/, standalone, no engine deps): bodies + frontier
# CA vs cell-only CA. Plan: docs/superpowers/plans/2026-09-22-water-prototype.md.
water-proto-test *args:
    cd prototypes/water && {{cap}} cargo test --release {{args}}
```

- [ ] **Step 3: Write the failing tests**

`prototypes/water/tests/t1_state.rs`:
```rust
use water_proto::body::Body;
use water_proto::grid::{Dims, Dir, horizontal_order};
use water_proto::state::World;
use water_proto::{FULL, Mode};

#[test]
fn index_roundtrip_and_closed_box() {
    let d = Dims::cube(8);
    for i in 0..d.len() as u32 {
        let (x, y, z) = d.xyz(i);
        assert_eq!(d.idx(x, y, z), i);
        assert_eq!(d.y_of(i), y);
    }
    let corner = d.idx(0, 0, 0);
    assert_eq!(d.step(corner, Dir::Down), None);
    assert_eq!(d.step(corner, Dir::NegX), None);
    assert_eq!(d.step(corner, Dir::NegZ), None);
    assert_eq!(d.step(corner, Dir::Up), Some(d.idx(0, 1, 0)));
    assert_eq!(d.step(d.idx(7, 7, 7), Dir::PosX), None);
}

#[test]
fn horizontal_order_rotates() {
    assert_ne!(horizontal_order(0), horizontal_order(1));
    assert_eq!(horizontal_order(0), horizontal_order(4));
}

#[test]
fn undo_restores_every_field() {
    let mut w = World::new(Dims::cube(8), Mode::Bodies);
    let h0 = w.hash();
    w.begin_log();
    w.set_step(3);
    w.set_water(5, 100);
    w.set_owner(6, 3);
    w.set_solid(7, true);
    w.set_active(8, true);
    w.touch(9);
    w.add_poured(7);
    let id = w.alloc_id();
    let mut b = Body::empty(id, w.dims);
    b.volume = 42;
    w.insert_body(b);
    w.body_mut(id).volume = 43;
    let log = w.end_log();
    assert!(!log.is_empty());
    assert_ne!(w.hash(), h0);
    w.undo(log);
    assert_eq!(w.hash(), h0);
    assert!(w.bodies.is_empty());
    assert_eq!(w.next_id, 1);
    assert_eq!(w.step, 0);
}

#[test]
fn body_level_math() {
    let mut b = Body::empty(1, Dims::cube(8));
    b.top = 2;
    b.area[1] = 4;
    b.area[2] = 4;
    b.below = 4;
    b.cells = 8;
    b.volume = 4 * FULL as u64 + 4 * 1000;
    assert_eq!(b.capacity(), 8 * FULL as u64);
    assert_eq!(b.frac(), 4000);
    assert_eq!(b.room(), 4 * FULL as u64 - 4000);
    assert_eq!(b.virtual_at(1), FULL);
    assert_eq!(b.virtual_at(2), 1000);
    assert_eq!(b.virtual_at(3), 0);
    assert_eq!(b.level(), 2 * FULL as u64 + 1000);
}

#[test]
fn hash_ignores_stats() {
    let mut w = World::new(Dims::cube(8), Mode::CellOnly);
    let h = w.hash();
    w.stats.transfers = 9;
    assert_eq!(w.hash(), h);
}
```

- [ ] **Step 4: Run the tests**

Run: `just water-proto-test --test t1_state`
Expected: all 5 pass. The tests land together with the code in this task, so the TDD
"see it fail" beat is compile-driven: running before Step 1 fails with unresolved imports.

- [ ] **Step 5: Commit**

```bash
git add prototypes/water/Cargo.toml prototypes/water/Cargo.lock prototypes/water/.gitignore \
  prototypes/water/src prototypes/water/tests/t1_state.rs
git commit -m "proto(water): standalone crate with grid, undo-logged state, body math

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Body operations and fixtures

> **Pre-flight ruling (2026-11-06), overrides the text below:** In `expand_layer`, right after the `room > 0 || …` early return, add `if !forced && wet == 0 { return false }` with no backoff. Here `wet` is the CA water in the candidate layer: compute it before the bowl test, or check it before accepting. Without it, `settle_init` grows a dry top layer on every full body: `lake_settles_into_one_body` would see 14,400 cells instead of 12,800, `merge_keeps_the_larger_body` 60 instead of 40, and `check_invariants` fails on a frac-0 top layer. `finish` must be idempotent: a second call on a settled world changes nothing.

**Files:**
- Modify: `prototypes/water/src/body.rs` (append the operations below)
- Create: `prototypes/water/src/fixture.rs`
- Modify: `prototypes/water/src/lib.rs` (add `pub mod fixture;`)
- Test: `prototypes/water/tests/t2_bodies.rs`

**Interfaces:**
- Consumes: Task 1's `World` setters, `Body`, and `F`.
- Produces:
  - `body::{on_solid, on_solid_or_member, layer_members, all_members, add_member, add_volume,
    remove_volume, drop_empty_layers, split_if_disconnected, seed, try_expand, merge,
    settle_init, check_invariants}`. All take `(&mut World, …)`, except `on_solid*`,
    `layer_members` and `all_members`, which take `&World`, and `check_invariants`, which
    takes `&World` and returns `Result<(), String>`.
  - `fixture::{fill_solid, carve, fill_water, finish}`.

- [ ] **Step 1: Write the failing tests**

`prototypes/water/tests/t2_bodies.rs`:
```rust
use water_proto::body::{self, check_invariants};
use water_proto::fixture::{carve, fill_solid, fill_water, finish};
use water_proto::grid::Dims;
use water_proto::state::World;
use water_proto::{EXPAND_BACKOFF, FULL, Mode};

const F: u64 = FULL as u64;

/// 40×40×8 m lake in a 64³ world, 4 m freeboard: water y 8..=15, air to y 19.
pub fn lake(mode: Mode) -> World {
    let mut w = World::new(Dims::cube(64), mode);
    fill_solid(&mut w, 0..=19);
    carve(&mut w, 12..=51, 8..=19, 12..=51);
    fill_water(&mut w, 12..=51, 8..=15, 12..=51);
    finish(&mut w);
    w
}

#[test]
fn lake_settles_into_one_body() {
    let w = lake(Mode::Bodies);
    assert_eq!(w.bodies.len(), 1);
    let b = w.bodies.values().next().unwrap();
    assert_eq!(b.cells, 12_800);
    assert_eq!(b.volume, 12_800 * F);
    assert_eq!(b.level(), 16 * F);
    assert!(w.water.iter().all(|&v| v == 0));
    assert_eq!(w.active.count(), 0);
    check_invariants(&w).unwrap();
}

#[test]
fn cell_only_lake_stays_cells() {
    let w = lake(Mode::CellOnly);
    assert!(w.bodies.is_empty());
    assert_eq!(w.mass(), 12_800 * F);
    assert_eq!(w.active.count(), 0);
}

#[test]
fn remove_volume_lowers_level_then_drops_a_layer() {
    let mut w = lake(Mode::Bodies);
    let id = *w.bodies.keys().next().unwrap();
    body::remove_volume(&mut w, id, 1600);
    assert_eq!(w.bodies[&id].level(), 16 * F - 1);
    body::remove_volume(&mut w, id, 1600 * F - 1600);
    let b = &w.bodies[&id];
    assert_eq!(b.top, 14);
    assert_eq!(b.level(), 15 * F);
    let vacated = w.dims.idx(20, 15, 20);
    assert_eq!(w.owner[vacated as usize], 0);
    assert!(w.active.get(vacated));
    check_invariants(&w).unwrap();
}

/// Two 6×6 basins joined over a sill (x 10..=13 solid up to y 9), water to y 12.
pub fn two_basins() -> World {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=15);
    carve(&mut w, 4..=9, 4..=15, 4..=9);
    carve(&mut w, 14..=19, 4..=15, 4..=9);
    carve(&mut w, 10..=13, 10..=15, 4..=9);
    fill_water(&mut w, 4..=9, 4..=12, 4..=9);
    fill_water(&mut w, 14..=19, 4..=12, 4..=9);
    fill_water(&mut w, 10..=13, 10..=12, 4..=9);
    finish(&mut w);
    w
}

#[test]
fn split_at_sill_shares_the_top_layer() {
    let mut w = two_basins();
    assert_eq!(w.bodies.len(), 1, "joined over the sill");
    let id = *w.bodies.keys().next().unwrap();
    let total = w.bodies[&id].volume;
    // layers 10..=12 hold 96 cells each (36 + 36 + 24 over the sill)
    body::remove_volume(&mut w, id, 288 * F + 1);
    assert_eq!(w.bodies.len(), 2);
    let v: Vec<u64> = w.bodies.values().map(|b| b.volume).collect();
    assert_eq!(v.iter().sum::<u64>(), total - 288 * F - 1);
    assert!(v[0].abs_diff(v[1]) <= 1);
    let over_sill = w.dims.idx(11, 10, 6);
    assert_eq!(w.owner[over_sill as usize], 0);
    check_invariants(&w).unwrap();
}

#[test]
fn expansion_onto_a_plain_is_refused_and_backs_off() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=3);
    carve(&mut w, 10..=11, 2..=3, 10..=11);
    fill_water(&mut w, 10..=11, 2..=3, 10..=11);
    finish(&mut w);
    let id = *w.bodies.keys().next().unwrap();
    let above = w.dims.idx(10, 4, 10);
    w.water[above as usize] = FULL; // a pour landed on the full pit
    assert!(!body::try_expand(&mut w, id));
    assert_eq!(w.stats.expansion_refusals, 1);
    assert_eq!(w.bodies[&id].expand_retry, w.step + EXPAND_BACKOFF);
    let flooded = w.stats.flood_cells;
    assert!(!body::try_expand(&mut w, id));
    assert_eq!(w.stats.flood_cells, flooded, "backoff: no second flood");
}

#[test]
fn merge_keeps_the_larger_body() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=7);
    carve(&mut w, 2..=5, 4..=7, 2..=5); // 16-cell layers
    carve(&mut w, 10..=11, 4..=7, 2..=3); // 4-cell layers
    fill_water(&mut w, 2..=5, 4..=5, 2..=5);
    fill_water(&mut w, 10..=11, 4..=5, 2..=3);
    finish(&mut w);
    let ids: Vec<u32> = w.bodies.keys().copied().collect();
    assert_eq!(ids.len(), 2);
    let total = w.mass();
    let keep = body::merge(&mut w, ids[1], ids[0]);
    assert_eq!(keep, ids[0], "the 32-cell body survives");
    assert_eq!(w.bodies.len(), 1);
    assert_eq!(w.mass(), total);
    assert_eq!(w.bodies[&keep].cells, 40);
    check_invariants(&w).unwrap();
}
```

- [ ] **Step 2: Run to see them fail**

Run: `just water-proto-test --test t2_bodies`
Expected: FAIL to compile (`fixture` and the `body::` functions don't exist).

- [ ] **Step 3: Implement `fixture.rs`**

```rust
//! Terrain and water builders. They write state directly, without the undo log:
//! a fixture is the initial snapshot, not a step.

use std::ops::RangeInclusive as R;

use crate::state::World;
use crate::{FULL, Mode, body};

pub fn fill_solid(w: &mut World, ys: R<u32>) {
    for y in ys {
        for z in 0..w.dims.z {
            for x in 0..w.dims.x {
                let i = w.dims.idx(x, y, z);
                w.solid.set(i, true);
            }
        }
    }
}

pub fn carve(w: &mut World, xs: R<u32>, ys: R<u32>, zs: R<u32>) {
    for y in ys {
        for z in zs.clone() {
            for x in xs.clone() {
                let i = w.dims.idx(x, y, z);
                w.solid.set(i, false);
            }
        }
    }
}

pub fn fill_water(w: &mut World, xs: R<u32>, ys: R<u32>, zs: R<u32>) {
    for y in ys {
        for z in zs.clone() {
            for x in xs.clone() {
                let i = w.dims.idx(x, y, z);
                assert!(!w.solid.get(i), "fill_water into solid at {x},{y},{z}");
                w.water[i as usize] = FULL;
            }
        }
    }
}

/// Freeze the fixture. In Bodies mode, standing water becomes bodies. Every cell
/// starts asleep and stats start at zero (a settled world costs nothing).
pub fn finish(w: &mut World) {
    if w.mode == Mode::Bodies {
        body::settle_init(w);
    }
    for i in w.active.ones() {
        w.active.set(i, false);
    }
    w.stats = Default::default();
}
```

- [ ] **Step 4: Append the operations to `body.rs`**

Replace the `use` lines at the top of `body.rs` with:
```rust
use crate::grid::{ALL, Dims, Dir, HORIZONTAL};
use crate::state::{BodyId, Fnv, NO_BODY, World};
use crate::{EXPAND_AREA_FACTOR, EXPAND_BACKOFF, EXPAND_BUDGET, FILM, FULL};
```
Then append:
```rust
pub fn on_solid(w: &World, i: u32) -> bool {
    w.dims.step(i, Dir::Down).is_none_or(|b| w.solid.get(b))
}

pub fn on_solid_or_member(w: &World, i: u32) -> bool {
    w.dims
        .step(i, Dir::Down)
        .is_none_or(|b| w.solid.get(b) || w.owner[b as usize] != NO_BODY)
}

fn supported_by(w: &World, i: u32, id: BodyId) -> bool {
    w.dims
        .step(i, Dir::Down)
        .is_none_or(|b| w.solid.get(b) || w.owner[b as usize] == id)
}

/// Members of `id` in layer `y`, ascending (bbox scan).
pub fn layer_members(w: &World, id: BodyId, y: u32) -> Vec<u32> {
    let b = &w.bodies[&id];
    let mut out = Vec::new();
    if b.cells == 0 {
        return out;
    }
    for z in b.lo[2]..=b.hi[2] {
        for x in b.lo[0]..=b.hi[0] {
            let i = w.dims.idx(x, y, z);
            if w.owner[i as usize] == id {
                out.push(i);
            }
        }
    }
    out
}

pub fn all_members(w: &World, id: BodyId) -> Vec<u32> {
    let b = &w.bodies[&id];
    if b.cells == 0 {
        return Vec::new();
    }
    (b.lo[1]..=b.hi[1]).flat_map(|y| layer_members(w, id, y)).collect()
}

/// Move CA cell `c` (and its water) into body `id`. The caller has set `top`
/// so that `c`'s layer is at or below it.
pub fn add_member(w: &mut World, id: BodyId, c: u32) {
    let dims = w.dims;
    let water = w.water[c as usize] as u64;
    w.set_water(c, 0);
    w.set_owner(c, id);
    w.set_active(c, false);
    let y = dims.y_of(c);
    let opens: Vec<u32> = HORIZONTAL
        .iter()
        .filter_map(|&d| dims.step(c, d))
        .filter(|&n| w.is_ca(n))
        .collect();
    let b = w.body_mut(id);
    b.volume += water;
    b.area[y as usize] += 1;
    b.cells += 1;
    if y < b.top {
        b.below += 1;
    }
    b.grow_bbox(dims.xyz(c));
    b.shore.retain(|&s| s != c);
    for n in opens {
        if let Err(p) = b.shore.binary_search(&n) {
            b.shore.insert(p, n);
        }
    }
}

pub fn add_volume(w: &mut World, id: BodyId, q: u64) {
    let b = w.body_mut(id);
    assert!(q <= b.room(), "add_volume beyond room");
    b.volume += q;
}

pub fn remove_volume(w: &mut World, id: BodyId, q: u64) {
    let b = w.body_mut(id);
    assert!(q <= b.volume, "remove_volume beyond volume");
    b.volume -= q;
    drop_empty_layers(w, id);
}

/// While the top layer holds no water its cells leave the body (dry CA cells,
/// awake). A body with no cells is removed. Dropping a layer can disconnect the body.
pub fn drop_empty_layers(w: &mut World, id: BodyId) {
    let mut dropped = false;
    loop {
        let (frac, cells, top) = {
            let b = &w.bodies[&id];
            (b.frac(), b.cells, b.top)
        };
        if cells == 0 || frac > 0 {
            break;
        }
        for c in layer_members(w, id, top) {
            w.set_owner(c, NO_BODY);
            w.set_active(c, true);
            w.touch(c);
        }
        let b = w.body_mut(id);
        b.cells -= b.area[top as usize] as u64;
        b.area[top as usize] = 0;
        match (0..top).rev().find(|&y| b.area[y as usize] > 0) {
            Some(y) => {
                b.top = y;
                b.below -= b.area[y as usize] as u64;
            }
            None => {
                b.top = 0;
                b.below = 0;
            }
        }
        w.stats.layer_drops += 1;
        dropped = true;
    }
    if w.bodies[&id].cells == 0 {
        assert_eq!(w.bodies[&id].volume, 0, "an emptied body kept water");
        w.remove_body(id);
        return;
    }
    if dropped {
        split_if_disconnected(w, id);
    }
}

/// Connected components of `members` (6-connected through members of `id`), each
/// sorted, in order of their lowest cell.
fn components(w: &mut World, members: &[u32], id: BodyId) -> Vec<Vec<u32>> {
    w.mark_gen += 1;
    let generation = w.mark_gen;
    let mut out = Vec::new();
    for &start in members {
        if w.mark[start as usize] == generation {
            continue;
        }
        w.mark[start as usize] = generation;
        let mut comp = vec![start];
        let mut k = 0;
        while k < comp.len() {
            let c = comp[k];
            k += 1;
            for d in ALL {
                if let Some(n) = w.dims.step(c, d) {
                    if w.owner[n as usize] == id && w.mark[n as usize] != generation {
                        w.mark[n as usize] = generation;
                        comp.push(n);
                    }
                }
            }
        }
        w.stats.flood_cells += comp.len() as u64;
        comp.sort_unstable();
        out.push(comp);
    }
    out
}

/// If `id` is no longer connected, split it. Every layer below the old top stays
/// full; the old top layer's water is shared by each part's cell count there
/// (remainder units go to the earliest parts). The first part keeps the id.
pub fn split_if_disconnected(w: &mut World, id: BodyId) {
    let members = all_members(w, id);
    let comps = components(w, &members, id);
    if comps.len() <= 1 {
        return;
    }
    w.stats.splits += 1;
    let old = w.remove_body(id);
    let t = old.top;
    let frac = old.frac();
    let at: Vec<u64> = comps
        .iter()
        .map(|c| c.iter().filter(|&&i| w.dims.y_of(i) == t).count() as u64)
        .collect();
    let total: u64 = at.iter().sum();
    let mut shares: Vec<u64> = at
        .iter()
        .map(|&a| if total == 0 { 0 } else { frac * a / total })
        .collect();
    let mut rem = frac - shares.iter().sum::<u64>();
    for k in 0..shares.len() {
        if rem == 0 {
            break;
        }
        if at[k] > 0 {
            shares[k] += 1;
            rem -= 1;
        }
    }
    assert_eq!(rem, 0, "split lost water");
    let mut ids = Vec::new();
    for (k, cells) in comps.iter().enumerate() {
        let nid = if k == 0 { id } else { w.alloc_id() };
        let mut b = Body::empty(nid, w.dims);
        for &c in cells {
            b.area[w.dims.y_of(c) as usize] += 1;
            b.grow_bbox(w.dims.xyz(c));
        }
        b.cells = cells.len() as u64;
        b.top = (0..w.dims.y)
            .rev()
            .find(|&y| b.area[y as usize] > 0)
            .expect("non-empty component");
        b.below = b.cells - b.top_area();
        b.volume = if b.top == t { F * b.below + shares[k] } else { F * b.cells };
        b.shore = old
            .shore
            .iter()
            .copied()
            .filter(|&s| {
                ALL.iter()
                    .filter_map(|&d| w.dims.step(s, d))
                    .any(|n| cells.binary_search(&n).is_ok())
            })
            .collect();
        if k > 0 {
            for &c in cells {
                w.set_owner(c, nid);
            }
        }
        w.insert_body(b);
        ids.push(nid);
    }
    for nid in ids {
        if w.bodies.contains_key(&nid) {
            drop_empty_layers(w, nid);
        }
    }
}

/// A full, quiet CA cell on solid ground starts a body: the connected patch of
/// full CA cells on solid ground in its layer becomes the body's first layer.
pub fn seed(w: &mut World, start: u32) -> BodyId {
    let dims = w.dims;
    let y = dims.y_of(start);
    w.mark_gen += 1;
    let generation = w.mark_gen;
    w.mark[start as usize] = generation;
    let mut patch = vec![start];
    let mut k = 0;
    while k < patch.len() {
        let c = patch[k];
        k += 1;
        for d in HORIZONTAL {
            if let Some(n) = dims.step(c, d) {
                if w.mark[n as usize] != generation
                    && w.is_ca(n)
                    && w.water[n as usize] == FULL
                    && on_solid(w, n)
                {
                    w.mark[n as usize] = generation;
                    patch.push(n);
                }
            }
        }
    }
    patch.sort_unstable();
    w.stats.flood_cells += patch.len() as u64;
    w.stats.seeds += 1;
    let id = w.alloc_id();
    let mut b = Body::empty(id, dims);
    b.top = y;
    w.insert_body(b);
    for c in patch {
        add_member(w, id, c);
    }
    id
}

/// Grow a full body by one layer (see the model section of the plan).
pub fn try_expand(w: &mut World, id: BodyId) -> bool {
    expand_layer(w, id, false)
}

/// `forced` skips the backoff, budget and plain/film checks (used by `relevel`).
fn expand_layer(w: &mut World, id: BodyId, forced: bool) -> bool {
    let dims = w.dims;
    let s = w.step;
    let (room, retry, top, top_area) = {
        let b = &w.bodies[&id];
        (b.room(), b.expand_retry, b.top, b.top_area())
    };
    if room > 0 || top + 1 >= dims.y || (!forced && s < retry) {
        return false;
    }
    let y = top + 1;
    w.mark_gen += 1;
    let generation = w.mark_gen;
    let mut cand: Vec<u32> = Vec::new();
    for m in layer_members(w, id, top) {
        if let Some(u) = dims.step(m, Dir::Up) {
            if w.is_ca(u) && w.mark[u as usize] != generation {
                w.mark[u as usize] = generation;
                cand.push(u);
            }
        }
    }
    let mut merge_with: Vec<BodyId> = Vec::new();
    let mut over = false;
    let mut k = 0;
    while k < cand.len() {
        if !forced && cand.len() as u32 > EXPAND_BUDGET {
            over = true;
            break;
        }
        let c = cand[k];
        k += 1;
        for d in HORIZONTAL {
            let Some(n) = dims.step(c, d) else { continue };
            if w.mark[n as usize] == generation {
                continue;
            }
            let o = w.owner[n as usize];
            if o != NO_BODY && o != id {
                if !merge_with.contains(&o) {
                    merge_with.push(o);
                }
                continue;
            }
            if w.is_ca(n) && supported_by(w, n, id) {
                w.mark[n as usize] = generation;
                cand.push(n);
            }
        }
    }
    w.stats.flood_cells += cand.len() as u64;
    if cand.is_empty() {
        return false;
    }
    let wet: u64 = cand.iter().map(|&c| w.water[c as usize] as u64).sum();
    let bowl = cand.len() as u64 <= EXPAND_AREA_FACTOR as u64 * top_area;
    let filmy = wet >= FILM as u64 * cand.len() as u64;
    if !forced && (over || !(bowl || filmy)) {
        w.body_mut(id).expand_retry = s + EXPAND_BACKOFF;
        w.stats.expansion_refusals += 1;
        return false;
    }
    cand.sort_unstable();
    {
        let b = w.body_mut(id);
        b.below += b.top_area();
        b.top = y;
    }
    for c in cand {
        add_member(w, id, c);
    }
    w.stats.expansions += 1;
    merge_with.sort_unstable();
    let mut keep = id;
    for o in merge_with {
        if w.bodies.contains_key(&o) && w.bodies.contains_key(&keep) {
            keep = merge(w, keep, o);
        }
    }
    true
}

/// Cut membership back to layer `t0`, then regrow layer by layer while the volume
/// exceeds capacity. This re-derives the container after bodies at different
/// heights join (communicating vessels: one level, instantly).
fn relevel(w: &mut World, id: BodyId, t0: u32) {
    let top = w.bodies[&id].top;
    for y in (t0 + 1..=top).rev() {
        for c in layer_members(w, id, y) {
            w.set_owner(c, NO_BODY);
            w.set_active(c, true);
            w.touch(c);
        }
        let b = w.body_mut(id);
        b.cells -= b.area[y as usize] as u64;
        b.area[y as usize] = 0;
    }
    {
        let b = w.body_mut(id);
        b.top = t0;
        b.below = b.cells - b.top_area();
    }
    while w.bodies.contains_key(&id) && w.bodies[&id].volume > w.bodies[&id].capacity() {
        if !expand_layer(w, id, true) {
            break;
        }
    }
    if let Some(b) = w.bodies.get(&id) {
        assert!(b.volume <= b.capacity(), "relevel: no room to regrow");
    }
}

/// Join two bodies. The one with more cells survives (a tie goes to the lower id).
pub fn merge(w: &mut World, a: BodyId, b: BodyId) -> BodyId {
    let (keep, gone) = {
        let ka = (w.bodies[&a].cells, std::cmp::Reverse(a));
        let kb = (w.bodies[&b].cells, std::cmp::Reverse(b));
        if kb > ka { (b, a) } else { (a, b) }
    };
    let t0 = w.bodies[&a].top.min(w.bodies[&b].top);
    let moved = all_members(w, gone);
    let g = w.remove_body(gone);
    for &c in &moved {
        w.set_owner(c, keep);
    }
    let mut shore: Vec<u32> = w.bodies[&keep]
        .shore
        .iter()
        .chain(&g.shore)
        .copied()
        .filter(|&s| w.is_ca(s))
        .collect();
    shore.sort_unstable();
    shore.dedup();
    let k = w.body_mut(keep);
    k.volume += g.volume;
    for (x, y) in k.area.iter_mut().zip(&g.area) {
        *x += *y;
    }
    k.cells += g.cells;
    for i in 0..3 {
        k.lo[i] = k.lo[i].min(g.lo[i]);
        k.hi[i] = k.hi[i].max(g.hi[i]);
    }
    k.top = k.top.max(g.top);
    k.below = k.cells - k.top_area();
    k.shore = shore;
    w.stats.merges += 1;
    relevel(w, keep, t0);
    if w.bodies.contains_key(&keep) {
        drop_empty_layers(w, keep);
    }
    keep
}

/// Fixture setup: seed every full CA cell on solid ground, then expand until nothing grows.
pub fn settle_init(w: &mut World) {
    for c in 0..w.dims.len() as u32 {
        if w.is_ca(c) && w.water[c as usize] == FULL && on_solid(w, c) {
            seed(w, c);
        }
    }
    loop {
        let ids: Vec<BodyId> = w.bodies.keys().copied().collect();
        let mut grew = false;
        for id in ids {
            if w.bodies.contains_key(&id) && try_expand(w, id) {
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    for b in w.bodies.values_mut() {
        b.expand_retry = 0;
    }
}

/// Structural invariants. O(grid): for tests only.
pub fn check_invariants(w: &World) -> Result<(), String> {
    let mut counted: std::collections::BTreeMap<BodyId, u64> = Default::default();
    for i in 0..w.dims.len() as u32 {
        let (o, v) = (w.owner[i as usize], w.water[i as usize]);
        if w.solid.get(i) && (o != NO_BODY || v != 0) {
            return Err(format!("solid cell {i} holds water/owner"));
        }
        if v > FULL {
            return Err(format!("cell {i} over full: {v}"));
        }
        if o != NO_BODY {
            if v != 0 {
                return Err(format!("member {i} holds CA water"));
            }
            let Some(b) = w.bodies.get(&o) else { return Err(format!("{i} owned by dead body {o}")) };
            if w.dims.y_of(i) > b.top {
                return Err(format!("member {i} above top of {o}"));
            }
            *counted.entry(o).or_default() += 1;
        }
    }
    for (id, b) in &w.bodies {
        let sum: u64 = b.area.iter().map(|&a| a as u64).sum();
        if sum != b.cells || counted.get(id).copied().unwrap_or(0) != b.cells {
            return Err(format!("body {id}: area/cell count mismatch"));
        }
        if b.top_area() == 0 || b.below != b.cells - b.top_area() {
            return Err(format!("body {id}: bad top/below"));
        }
        if b.area[b.top as usize + 1..].iter().any(|&a| a != 0) {
            return Err(format!("body {id}: area above top"));
        }
        if !(F * b.below < b.volume && b.volume <= b.capacity()) {
            return Err(format!("body {id}: volume {} outside ({}, {}]", b.volume, F * b.below, b.capacity()));
        }
    }
    Ok(())
}
```

Add `pub mod fixture;` to `src/lib.rs`, after `pub mod body;`.

- [ ] **Step 5: Run the tests**

Run: `just water-proto-test --test t2_bodies`
Expected: all 6 pass. If `split_at_sill_shares_the_top_layer` sees 1 body, check that
`drop_empty_layers` ran for all three layers: each call drops only while `frac == 0`, and 288
full layers' worth + 1 unit empties layers 12, 11 and 10.

- [ ] **Step 6: Commit**

```bash
git add prototypes/water/src/body.rs prototypes/water/src/fixture.rs prototypes/water/src/lib.rs \
  prototypes/water/tests/t2_bodies.rs
git commit -m "proto(water): bodies seed, expand, drop, split, merge with relevel

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The water step and edits

> **Pre-flight ruling (2026-11-06), overrides the text below:** (1) **Absorb tolerance.** Water fed from a body into a cell stops at FULL−ABSORB_TOL, so in `level_with`, below the top, use `FULL - wc <= ABSORB_TOL`, not `wc == FULL`. Otherwise `hole_fills_pocket_and_joins_the_lake` never absorbs the pocket. (2) **Top-layer drain.** Water leaving a body *from its top layer* (`body_to_ca`, sideways or down) uses the top's head rounded **up**: add `Body::outflow_at(y) -> u16`, which is `FULL` below the top and `frac.div_ceil(top_area)` at the top. Use it wherever an outlet reads the body's head (plan lines near `b.virtual_at(y)` in `horizontal` and the down/up member edges). Without it a draining last layer stalls at 1–15 units and the body never goes away (`floor_hole`). (3) **Shore wake.** Move `w.touched.clear()` from the start of `ca::step` to `World::begin_log`, so a level change from an edit also wakes the shore.

**Files:**
- Create: `prototypes/water/src/ca.rs`, `prototypes/water/src/edit.rs`
- Modify: `prototypes/water/src/lib.rs` (add `pub mod ca;` and `pub mod edit;`)
- Test: `prototypes/water/tests/t3_ca.rs`

**Interfaces:**
- Consumes: Task 2's `body::*` and `fixture::*`.
- Produces:
  - `ca::step(&mut World)`, which advances `w.step` by 1;
  - `edit::{Edit::{Remove(u32), Place(u32), Pour(u32, u16)}, EditError::{NotSolid, Solid,
    NoRoom}, apply(&mut World, Edit) -> Result<(), EditError>}`.

- [ ] **Step 1: Write the failing tests**

`prototypes/water/tests/t3_ca.rs`:
```rust
use water_proto::body::check_invariants;
use water_proto::edit::{Edit, EditError, apply};
use water_proto::fixture::{carve, fill_solid, fill_water, finish};
use water_proto::grid::Dims;
use water_proto::state::World;
use water_proto::{FILM, FULL, Mode, ca};

const F: u64 = FULL as u64;

fn run_until_asleep(w: &mut World, cap: u32) -> u32 {
    for _ in 0..cap {
        ca::step(w);
        if w.active.count() == 0 {
            return w.step;
        }
    }
    panic!("not asleep after {cap} steps");
}

fn lake(mode: Mode) -> World {
    let mut w = World::new(Dims::cube(64), mode);
    fill_solid(&mut w, 0..=19);
    carve(&mut w, 12..=51, 8..=19, 12..=51);
    fill_water(&mut w, 12..=51, 8..=15, 12..=51);
    finish(&mut w);
    w
}

#[test]
fn column_falls_to_the_floor() {
    let mut w = World::new(Dims::cube(16), Mode::CellOnly);
    fill_solid(&mut w, 0..=0);
    apply(&mut w, Edit::Pour(w.dims.idx(5, 10, 5), FULL)).unwrap();
    run_until_asleep(&mut w, 400);
    assert_eq!(w.mass(), F);
    for i in 0..w.dims.len() as u32 {
        if w.water[i as usize] > 0 {
            assert_eq!(w.dims.y_of(i), 1, "all water rests on the floor");
        }
    }
}

#[test]
fn pit_levels_flat() {
    let mut w = World::new(Dims::cube(16), Mode::CellOnly);
    fill_solid(&mut w, 0..=3);
    carve(&mut w, 5..=7, 3..=3, 5..=7);
    let top = w.dims.idx(6, 8, 6);
    for _ in 0..4 {
        apply(&mut w, Edit::Pour(top, FULL)).unwrap();
        for _ in 0..8 {
            ca::step(&mut w);
        }
    }
    run_until_asleep(&mut w, 2000);
    let pit: Vec<u16> = (5..=7)
        .flat_map(|z| (5..=7).map(move |x| (x, z)))
        .map(|(x, z)| w.water[w.dims.idx(x, 3, z) as usize])
        .collect();
    assert_eq!(pit.iter().map(|&v| v as u64).sum::<u64>(), 4 * F);
    assert!(pit.iter().max().unwrap() - pit.iter().min().unwrap() <= 2, "{pit:?}");
}

#[test]
fn film_stops_spreading() {
    let mut w = World::new(Dims::cube(32), Mode::CellOnly);
    fill_solid(&mut w, 0..=3);
    apply(&mut w, Edit::Pour(w.dims.idx(15, 4, 15), FULL)).unwrap();
    apply(&mut w, Edit::Pour(w.dims.idx(16, 4, 15), FULL)).unwrap();
    run_until_asleep(&mut w, 2000);
    let wet = w.water.iter().filter(|&&v| v > 0).count();
    assert_eq!(w.mass(), 2 * F);
    assert!((32..=160).contains(&wet), "wet cells {wet}");
    assert!(w.water.iter().all(|&v| v <= 2 * FILM));
}

#[test]
fn settled_cells_cost_nothing() {
    let mut w = lake(Mode::CellOnly);
    for _ in 0..20 {
        ca::step(&mut w);
    }
    assert_eq!(w.stats.cells_visited, 0);
}

#[test]
fn pour_in_corner_conserves() {
    let mut w = World::new(Dims::cube(16), Mode::Bodies);
    fill_solid(&mut w, 0..=0);
    apply(&mut w, Edit::Pour(w.dims.idx(0, 1, 0), FULL)).unwrap();
    run_until_asleep(&mut w, 2000);
    assert_eq!(w.mass(), F);
    check_invariants(&w).unwrap();
}

#[test]
fn waterfall_into_full_lake_joins_the_body() {
    let mut w = lake(Mode::Bodies);
    let spout = w.dims.idx(30, 18, 30);
    for _ in 0..20 {
        apply(&mut w, Edit::Pour(spout, FULL)).unwrap();
        ca::step(&mut w);
    }
    run_until_asleep(&mut w, 5000);
    assert_eq!(w.bodies.len(), 1);
    assert_eq!(w.water.iter().map(|&v| v as u64).sum::<u64>(), 0, "no stray CA water");
    let b = w.bodies.values().next().unwrap();
    assert_eq!(b.volume, 12_820 * F);
    assert_eq!(b.level(), 16 * F + 20 * F / 1600);
    check_invariants(&w).unwrap();
}

#[test]
fn hole_fills_pocket_and_joins_the_lake() {
    let mut w = World::new(Dims::cube(64), Mode::Bodies);
    fill_solid(&mut w, 0..=19);
    carve(&mut w, 12..=51, 8..=19, 12..=51);
    carve(&mut w, 8..=9, 10..=10, 20..=24); // 10 m³ dry pocket inside the west wall
    fill_water(&mut w, 12..=51, 8..=15, 12..=51);
    finish(&mut w);
    apply(&mut w, Edit::Remove(w.dims.idx(10, 10, 22))).unwrap();
    apply(&mut w, Edit::Remove(w.dims.idx(11, 10, 22))).unwrap();
    run_until_asleep(&mut w, 5000);
    assert_eq!(w.bodies.len(), 1);
    assert_eq!(w.water.iter().map(|&v| v as u64).sum::<u64>(), 0);
    let (id, b) = w.bodies.iter().next().unwrap();
    assert_eq!(w.owner[w.dims.idx(8, 10, 20) as usize], *id, "pocket joined the lake");
    // 12 new cells at y 10: below-top = 7*1600 + 12, frac = 1588 F over 1600 cells
    assert_eq!(b.level(), 15 * F + 1588 * F / 1600);
    check_invariants(&w).unwrap();
}

#[test]
fn place_into_sealed_body_rejected() {
    let mut w = World::new(Dims::cube(16), Mode::Bodies);
    fill_solid(&mut w, 0..=15);
    carve(&mut w, 4..=7, 4..=6, 4..=7);
    fill_water(&mut w, 4..=7, 4..=6, 4..=7);
    finish(&mut w);
    let h = w.hash();
    assert_eq!(apply(&mut w, Edit::Place(w.dims.idx(5, 5, 5))), Err(EditError::NoRoom));
    assert_eq!(w.hash(), h, "a rejected edit changes nothing");
}

#[test]
fn place_into_full_lake_raises_level() {
    let mut w = lake(Mode::Bodies);
    apply(&mut w, Edit::Place(w.dims.idx(20, 10, 20))).unwrap();
    run_until_asleep(&mut w, 2000);
    assert_eq!(w.mass(), 12_800 * F);
    assert_eq!(w.bodies.len(), 1);
    assert_eq!(w.bodies.values().next().unwrap().level(), 16 * F + F / 1600);
    check_invariants(&w).unwrap();
}
```

Expected values:
- **`waterfall_into_full_lake_joins_the_body`:** 20 m³ poured onto the full lake. The first
  pour requests an expansion of layer 16 (1600 cells ≤ 4 × 1600, so it's a "bowl"). Everything
  then pours into that top layer: level = 16 F + ⌊20 F / 1600⌋ = 16 F + 51.
- **`place_into_full_lake_raises_level`:** 1 m³ displaced from y 10 overflows above the full
  top, the next step expands layer 16, and the level = 16 F + ⌊4096 / 1600⌋ = 16 F + 2.

- [ ] **Step 2: Run to see them fail**

Run: `just water-proto-test --test t3_ca`
Expected: FAIL to compile (`ca` and `edit` don't exist).

- [ ] **Step 3: Implement `ca.rs`**

```rust
//! One water step. Every awake CA cell settles its six edges once, in ascending
//! index order. Then (bodies mode) cells are absorbed/seeded, requested
//! expansions run, bodies whose level rose wake their shore, and the step advances.

use crate::body::{self, Body};
use crate::grid::{ALL, Dir, horizontal_order};
use crate::state::{BodyId, NO_BODY, World};
use crate::{ABSORB_TOL, FILM, FULL, MAX_FLOW, Mode, SEED_QUIET, SLEEP_STEPS};

pub fn step(w: &mut World) {
    let s = w.step;
    w.touched.clear();
    w.expand_requests.clear();
    let order = horizontal_order(s);
    for c in w.active.ones() {
        if !w.is_ca(c) {
            w.set_active(c, false);
            continue;
        }
        w.stats.cells_visited += 1;
        vertical(w, c);
        for d in order {
            if w.is_ca(c) {
                horizontal(w, c, d);
            }
        }
        if w.mode == Mode::Bodies && w.is_ca(c) {
            absorb_or_seed(w, c);
        }
        if w.is_ca(c) && s >= w.last_touch[c as usize] + SLEEP_STEPS {
            w.set_active(c, false);
        }
    }
    if w.mode == Mode::Bodies {
        after_step(w);
    }
    w.set_step(s + 1);
}

fn wake(w: &mut World, c: u32) {
    w.set_active(c, true);
    w.touch(c);
}

fn move_ca(w: &mut World, from: u32, to: u32, q: u16) {
    let (a, b) = (w.water[from as usize], w.water[to as usize]);
    w.set_water(from, a - q);
    w.set_water(to, b + q);
    wake(w, from);
    wake(w, to);
    w.stats.transfers += 1;
}

fn ca_to_body(w: &mut World, from: u32, id: BodyId, q: u16) {
    let a = w.water[from as usize];
    w.set_water(from, a - q);
    body::add_volume(w, id, q as u64);
    wake(w, from);
    w.stats.transfers += 1;
}

fn body_to_ca(w: &mut World, id: BodyId, to: u32, q: u16) {
    body::remove_volume(w, id, q as u64);
    let b = w.water[to as usize];
    w.set_water(to, b + q);
    wake(w, to);
    w.stats.transfers += 1;
}

fn vertical(w: &mut World, c: u32) {
    let dims = w.dims;
    // c falls into the cell below
    if let Some(b) = dims.step(c, Dir::Down) {
        let wc = w.water[c as usize];
        if wc > 0 && !w.solid.get(b) {
            match w.owner[b as usize] {
                NO_BODY => {
                    let q = wc.min(FULL - w.water[b as usize]);
                    if q > 0 {
                        move_ca(w, c, b, q);
                    }
                }
                id => {
                    let (top, room) = {
                        let bd = &w.bodies[&id];
                        (bd.top, bd.room())
                    };
                    if dims.y_of(b) == top {
                        if room > 0 {
                            ca_to_body(w, c, id, (wc as u64).min(room) as u16);
                        } else {
                            w.expand_requests.push(id);
                        }
                    }
                }
            }
        }
    }
    // the cell above falls into c
    if let Some(a) = dims.step(c, Dir::Up) {
        if !w.solid.get(a) && w.is_ca(c) {
            let room = FULL - w.water[c as usize];
            match w.owner[a as usize] {
                NO_BODY => {
                    let q = w.water[a as usize].min(room);
                    if q > 0 {
                        move_ca(w, a, c, q);
                    }
                }
                id => {
                    let (v, vol) = {
                        let bd = &w.bodies[&id];
                        (bd.virtual_at(dims.y_of(a)), bd.volume)
                    };
                    let q = (v.min(room).min(MAX_FLOW) as u64).min(vol) as u16;
                    if q > 0 {
                        body_to_ca(w, id, c, q);
                    }
                }
            }
        }
    }
}

fn horizontal(w: &mut World, c: u32, d: Dir) {
    let dims = w.dims;
    let Some(n) = dims.step(c, d) else { return };
    if w.solid.get(n) {
        return;
    }
    let wc = w.water[c as usize];
    match w.owner[n as usize] {
        NO_BODY => {
            let wn = w.water[n as usize];
            let (hi, lo, whi, wlo) = if wc >= wn { (c, n, wc, wn) } else { (n, c, wn, wc) };
            let diff = whi - wlo;
            let q = if !w.supported(lo) {
                diff.div_ceil(2) // over an edge: pour, rounding up so films drain off
            } else if whi <= FILM || diff < 2 {
                0 // film rule
            } else {
                diff / 2
            };
            let q = q.min(MAX_FLOW);
            if q > 0 {
                move_ca(w, hi, lo, q);
            }
        }
        id => {
            let y = dims.y_of(c);
            let (v, top, room, vol) = {
                let b = &w.bodies[&id];
                (b.virtual_at(y), b.top, b.room(), b.volume)
            };
            if v > wc + ABSORB_TOL {
                let q = ((v - wc) / 2).min(MAX_FLOW).min(FULL - wc);
                let q = (q as u64).min(vol) as u16;
                if q > 0 {
                    body_to_ca(w, id, c, q);
                }
            } else if wc > v + ABSORB_TOL && y == top {
                let q = (((wc - v) / 2) as u64).min(MAX_FLOW as u64).min(room) as u16;
                if q > 0 {
                    ca_to_body(w, c, id, q);
                }
            }
        }
    }
}

fn level_with(b: &Body, y: u32, wc: u16) -> bool {
    if y < b.top { wc == FULL } else { wc.abs_diff(b.virtual_at(y)) <= ABSORB_TOL }
}

fn absorb_or_seed(w: &mut World, c: u32) {
    let dims = w.dims;
    let wc = w.water[c as usize];
    if wc == 0 || !body::on_solid_or_member(w, c) {
        return;
    }
    let y = dims.y_of(c);
    let mut touching: Vec<BodyId> = Vec::new();
    for d in ALL {
        if let Some(n) = dims.step(c, d) {
            let o = w.owner[n as usize];
            if o != NO_BODY && !touching.contains(&o) && y <= w.bodies[&o].top {
                touching.push(o);
            }
        }
    }
    if touching.is_empty() {
        if wc == FULL && body::on_solid(w, c) && w.step >= w.last_touch[c as usize] + SEED_QUIET {
            body::seed(w, c);
        }
        return;
    }
    // c joins only if it is level with every body it touches; otherwise it is a pipe
    if !touching.iter().all(|o| level_with(&w.bodies[o], y, wc)) {
        return;
    }
    touching.sort_unstable();
    body::add_member(w, touching[0], c);
    w.stats.absorbs += 1;
    let mut keep = touching[0];
    for &o in &touching[1..] {
        if w.bodies.contains_key(&o) && w.bodies.contains_key(&keep) && o != keep {
            keep = body::merge(w, keep, o);
        }
    }
}

fn after_step(w: &mut World) {
    let mut req = std::mem::take(&mut w.expand_requests);
    req.sort_unstable();
    req.dedup();
    for id in req {
        if w.bodies.contains_key(&id) {
            body::try_expand(w, id);
        }
    }
    // a rising level can start flow at spill edges and openings
    let touched = std::mem::take(&mut w.touched);
    let mut rose: Vec<BodyId> = touched
        .iter()
        .filter(|(id, v0)| w.bodies.get(id).is_some_and(|b| b.volume > *v0))
        .map(|t| t.0)
        .collect();
    rose.sort_unstable();
    for id in rose {
        let shore = w.bodies[&id].shore.clone();
        for s in shore {
            if w.is_ca(s) {
                wake(w, s);
            }
        }
    }
}
```

- [ ] **Step 4: Implement `edit.rs`**

```rust
//! Player edits, applied at the start of a step inside its undo log.

use crate::body::{self, layer_members};
use crate::grid::{ALL, Dir};
use crate::state::{NO_BODY, World};
use crate::FULL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Remove(u32),
    Place(u32),
    Pour(u32, u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    NotSolid,
    Solid,
    NoRoom,
}

pub fn apply(w: &mut World, e: Edit) -> Result<(), EditError> {
    match e {
        Edit::Remove(i) => remove_solid(w, i),
        Edit::Place(i) => place_solid(w, i),
        Edit::Pour(i, q) => pour(w, i, q),
    }
}

fn wake(w: &mut World, i: u32) {
    if w.is_ca(i) {
        w.set_active(i, true);
        w.touch(i);
    }
}

fn remove_solid(w: &mut World, i: u32) -> Result<(), EditError> {
    if !w.solid.get(i) {
        return Err(EditError::NotSolid);
    }
    w.set_solid(i, false);
    wake(w, i);
    for d in ALL {
        if let Some(n) = w.dims.step(i, d) {
            wake(w, n);
            let o = w.owner[n as usize];
            if o != NO_BODY {
                let b = w.body_mut(o);
                if let Err(p) = b.shore.binary_search(&i) {
                    b.shore.insert(p, i);
                }
            }
        }
    }
    Ok(())
}

fn place_solid(w: &mut World, i: u32) -> Result<(), EditError> {
    if w.solid.get(i) {
        return Err(EditError::Solid);
    }
    match w.owner[i as usize] {
        NO_BODY => {
            let wi = w.water[i as usize];
            if wi > 0 {
                let target = [Dir::Up, Dir::PosX, Dir::PosZ, Dir::NegX, Dir::NegZ]
                    .into_iter()
                    .filter_map(|d| w.dims.step(i, d))
                    .find(|&n| w.is_ca(n) && FULL - w.water[n as usize] >= wi);
                let Some(n) = target else { return Err(EditError::NoRoom) };
                let wn = w.water[n as usize];
                w.set_water(n, wn + wi);
                w.set_water(i, 0);
                wake(w, n);
            }
            w.set_active(i, false);
        }
        id => {
            let (cap, vol, top) = {
                let b = &w.bodies[&id];
                (b.capacity(), b.volume, b.top)
            };
            // water that no longer fits once this cell's capacity is gone
            let overflow = (vol + FULL as u64).saturating_sub(cap);
            let dest = if overflow > 0 {
                let d = layer_members(w, id, top)
                    .into_iter()
                    .filter_map(|m| w.dims.step(m, Dir::Up))
                    .find(|&u| w.is_ca(u) && ((FULL - w.water[u as usize]) as u64) >= overflow);
                match d {
                    Some(u) => Some(u),
                    None => return Err(EditError::NoRoom),
                }
            } else {
                None
            };
            let y = w.dims.y_of(i);
            w.set_owner(i, NO_BODY);
            {
                let b = w.body_mut(id);
                b.area[y as usize] -= 1;
                b.cells -= 1;
                if y < b.top {
                    b.below -= 1;
                }
                b.volume -= overflow;
            }
            if let Some(u) = dest {
                let wu = w.water[u as usize];
                w.set_water(u, wu + overflow as u16);
                wake(w, u);
            }
            body::drop_empty_layers(w, id);
            if w.bodies.contains_key(&id) {
                body::split_if_disconnected(w, id);
            }
        }
    }
    w.set_solid(i, true);
    for d in ALL {
        if let Some(n) = w.dims.step(i, d) {
            wake(w, n);
        }
    }
    Ok(())
}

fn pour(w: &mut World, i: u32, q: u16) -> Result<(), EditError> {
    if w.solid.get(i) {
        return Err(EditError::Solid);
    }
    match w.owner[i as usize] {
        NO_BODY => {
            let wi = w.water[i as usize];
            if FULL - wi < q {
                return Err(EditError::NoRoom);
            }
            w.set_water(i, wi + q);
            wake(w, i);
        }
        id => {
            if w.bodies[&id].room() < q as u64 {
                return Err(EditError::NoRoom);
            }
            body::add_volume(w, id, q as u64);
        }
    }
    w.add_poured(q as u64);
    Ok(())
}
```

Add `pub mod ca;` and `pub mod edit;` to `src/lib.rs`.

- [ ] **Step 5: Run the tests**

Run: `just water-proto-test --test t3_ca`
Expected: all 9 pass. If a test fails:
- **Absorb/seed interface.** Check `check_invariants` first; it names the broken body.
- **Oscillation** (a cell alternately absorbed and drained): watch `w.stats.absorbs` grow while
  nothing else changes.
- **A body with volume ≤ `FULL*below`:** a `drop_empty_layers` call is missing after a volume
  decrease.

- [ ] **Step 6: Commit**

```bash
git add prototypes/water/src/ca.rs prototypes/water/src/edit.rs prototypes/water/src/lib.rs \
  prototypes/water/tests/t3_ca.rs
git commit -m "proto(water): water step (CA and body edges, absorb, seed) and edits

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Scenarios, runner, metrics

**Files:**
- Create: `prototypes/water/src/scenario.rs`, `prototypes/water/src/metrics.rs`
- Modify: `prototypes/water/src/lib.rs` (add `pub mod metrics;` and `pub mod scenario;`)
- Test: `prototypes/water/tests/t4_scenarios.rs`

**Interfaces:**
- Consumes: `ca::step`, `edit::{Edit, apply}`, `fixture::*`, `body::check_invariants`.
- Produces:
  - `scenario::{NAMES: [&str; 6], Scenario { name, world, edits, max_steps, stop_when_asleep,
    initial_mass }, Report {..}, build(&str, Mode) -> Scenario,
    step_once(&mut Scenario) -> (Vec<Undo>, u32), run(&mut Scenario, impl FnMut(&World)) ->
    Report, drain_a_spread(&World) -> Option<u64>}`;
  - `metrics::{column_surface, surface_spread, dense_bytes, sparse_bytes}`.

- [ ] **Step 1: Write the failing tests**

`prototypes/water/tests/t4_scenarios.rs`:
```rust
use water_proto::body::check_invariants;
use water_proto::edit::Edit;
use water_proto::fixture::{carve, fill_solid, fill_water, finish};
use water_proto::grid::Dims;
use water_proto::metrics::column_surface;
use water_proto::scenario::{self, Scenario, build, drain_a_spread, run};
use water_proto::state::World;
use water_proto::{EXPAND_BACKOFF, FULL, Mode};

const F: u64 = FULL as u64;

fn ok(r: &scenario::Report) {
    assert!(r.mass_ok, "{} {}: mass drifted", r.name, r.mode.name());
    assert_eq!(r.edit_errors, 0, "{} {}: edit rejected", r.name, r.mode.name());
}

#[test]
fn settled_costs_nothing_in_both_modes() {
    for mode in [Mode::CellOnly, Mode::Bodies] {
        let mut sc = build("settled", mode);
        let r = run(&mut sc, |_| {});
        ok(&r);
        assert_eq!(r.steps, 100);
        assert_eq!(r.stats.cells_visited, 0, "{}", mode.name());
    }
}

#[test]
fn small_spill_settles_in_both_modes() {
    for mode in [Mode::CellOnly, Mode::Bodies] {
        let mut sc = build("small_spill", mode);
        let r = run(&mut sc, |_| {});
        ok(&r);
        assert!(r.asleep_at.is_some(), "{} never slept", mode.name());
    }
}

#[test]
fn bucket_fills_the_pit() {
    let mut sc = build("bucket", Mode::Bodies);
    let r = run(&mut sc, |_| {});
    ok(&r);
    let w = &sc.world;
    assert_eq!(w.bodies.len(), 1);
    // 9-cell pit: y1, y2 full (18 m³), 2 m³ on y3
    assert_eq!(w.bodies.values().next().unwrap().level(), 3 * F + 2 * F / 9);
    check_invariants(w).unwrap();
}

#[test]
fn film_puddle_stays_in_both_modes() {
    for mode in [Mode::CellOnly, Mode::Bodies] {
        let mut sc = build("film", mode);
        let r = run(&mut sc, |_| {});
        ok(&r);
        assert!(r.asleep_at.is_some());
        let wet = sc.world.water.iter().filter(|&&v| v > 0).count();
        assert!((32..=160).contains(&wet), "{} wet {wet}", mode.name());
        assert!(sc.world.bodies.is_empty());
    }
}

#[test]
fn u_tube_communicating_vessels() {
    let mut sc = build("u_tube", Mode::Bodies);
    let r = run(&mut sc, |_| {});
    ok(&r);
    assert_eq!(sc.world.bodies.len(), 1);
    // layers 4,5: 44 cells each; 6: 8 cells; 4 m³ over the 8 cells of layer 7
    assert_eq!(sc.world.bodies.values().next().unwrap().level(), 7 * F + F / 2);

    // the cell-only CA has no pressure: the far shaft stays lower (known limitation)
    let mut sc = build("u_tube", Mode::CellOnly);
    let r = run(&mut sc, |_| {});
    ok(&r);
    let left = column_surface(&sc.world, 20, 30).unwrap();
    let right = column_surface(&sc.world, 40, 30).unwrap();
    assert!(right < left, "cell-only: right {right} left {left}");
}

#[test]
fn big_drain_merges_at_a_common_level() {
    let mut sc = build("big_drain", Mode::Bodies);
    let mut worst = 0;
    let r = run(&mut sc, |w| {
        if w.step % 100 == 0 {
            worst = worst.max(drain_a_spread(w).unwrap_or(0));
        }
    });
    ok(&r);
    assert!(r.asleep_at.is_some(), "drain never settled");
    assert_eq!(worst, 0, "lake A's surface stays flat while draining");
    let w = &sc.world;
    assert_eq!(w.bodies.len(), 1, "A, channel and B are one body");
    let level = w.bodies.values().next().unwrap().level();
    let expect = 23 * F + 1148 * F / 1920;
    assert!(level.abs_diff(expect) <= 8, "level {level} expect {expect}");
    check_invariants(w).unwrap();
}

#[test]
fn floor_hole_drains_lake_into_cavity() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=20);
    carve(&mut w, 4..=7, 2..=5, 4..=7); // 64 m³ cavity
    carve(&mut w, 4..=7, 12..=13, 4..=7); // 32 m³ lake, directly above
    fill_water(&mut w, 4..=7, 12..=13, 4..=7);
    finish(&mut w);
    let d = w.dims;
    let edits = (6..=11).map(|y| (0, Edit::Remove(d.idx(5, y, 5)))).collect();
    let initial_mass = w.mass();
    let mut sc = Scenario {
        name: "floor_hole",
        world: w,
        edits,
        max_steps: 20_000,
        stop_when_asleep: true,
        initial_mass,
    };
    let r = run(&mut sc, |_| {});
    ok(&r);
    let w = &sc.world;
    assert_eq!(w.bodies.len(), 1, "the drained lake body is gone");
    let b = w.bodies.values().next().unwrap();
    assert_eq!(b.level(), 4 * F, "cavity y2, y3 full (32 m³ over 16 cells)");
    assert!(b.hi[1] <= 5);
    check_invariants(w).unwrap();
}

#[test]
fn plain_overflow_backs_off() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=3);
    carve(&mut w, 10..=11, 2..=3, 10..=11); // 8 m³ pit in a plain
    finish(&mut w);
    let spout = w.dims.idx(10, 8, 10);
    let edits = (0..12).map(|k| (k, Edit::Pour(spout, FULL))).collect();
    let mut sc = Scenario {
        name: "plain_overflow",
        world: w,
        edits,
        max_steps: 5_000,
        stop_when_asleep: true,
        initial_mass: 0,
    };
    let r = run(&mut sc, |_| {});
    ok(&r);
    let w = &sc.world;
    assert_eq!(w.bodies.len(), 1, "the pit is a body");
    assert_eq!(w.water.iter().map(|&v| v as u64).sum::<u64>(), 4 * F, "4 m³ film on the plain");
    assert!(r.stats.expansion_refusals >= 1);
    let bound = (r.steps as u64 / EXPAND_BACKOFF as u64 + 2) * 1024 + 64;
    assert!(r.stats.flood_cells <= bound, "flooded {} > {bound}", r.stats.flood_cells);
}
```

Expected values:
- **`big_drain`:** A is 24×56 = 1344 cells per layer, 12 m deep (16128 m³). B is 24×24 = 576
  cells per layer, floor y 4. The channel is 2 cells per layer at y 20, 21.
  - B up to y 19 holds 9216 m³.
  - y 20, 21 hold 1922 cells each.
  - y 22 holds 1920 cells.
  - The remaining 1148 m³ sits on the 1920-cell layer y 23.
  - Level = 23 F + 1148 F / 1920 (±8 units for pipe-cell residue).
- **`floor_hole`:** the six shaft cells end dry. If they hold residue, the level assertion shows
  it.

- [ ] **Step 2: Run to see them fail**

Run: `just water-proto-test --test t4_scenarios`
Expected: FAIL to compile (`scenario` and `metrics` don't exist).

- [ ] **Step 3: Implement `metrics.rs`**

```rust
//! Read-only measurements for reports and tests.

use std::ops::RangeInclusive;

use crate::state::{NO_BODY, World};
use crate::FULL;

const F: u64 = FULL as u64;

/// Height of the highest water in a column, in units (a member reports its body's level).
pub fn column_surface(w: &World, x: u32, z: u32) -> Option<u64> {
    for y in (0..w.dims.y).rev() {
        let i = w.dims.idx(x, y, z) as usize;
        match w.owner[i] {
            NO_BODY => {
                if w.water[i] > 0 {
                    return Some(y as u64 * F + w.water[i] as u64);
                }
            }
            id => return Some(w.bodies[&id].level()),
        }
    }
    None
}

/// Max minus min surface over the wet columns of a footprint.
pub fn surface_spread(w: &World, xs: RangeInclusive<u32>, zs: RangeInclusive<u32>) -> Option<u64> {
    let (mut lo, mut hi, mut any) = (u64::MAX, 0, false);
    for z in zs {
        for x in xs.clone() {
            if let Some(s) = column_surface(w, x, z) {
                lo = lo.min(s);
                hi = hi.max(s);
                any = true;
            }
        }
    }
    any.then(|| hi - lo)
}

/// The cell-only representation: one u16 per cell.
pub fn dense_bytes(w: &World) -> usize {
    w.dims.len() * 2
}

/// The proposed sparse layout: body headers + an 8-byte span per vertical run of
/// member cells + 6 bytes (index + amount) per CA cell holding water.
pub fn sparse_bytes(w: &World) -> usize {
    let d = w.dims;
    let (mut spans, mut ca_wet) = (0usize, 0usize);
    for z in 0..d.z {
        for x in 0..d.x {
            let mut inside = false;
            for y in 0..d.y {
                let i = d.idx(x, y, z) as usize;
                let member = w.owner[i] != NO_BODY;
                if member && !inside {
                    spans += 1;
                }
                inside = member;
                if w.water[i] > 0 {
                    ca_wet += 1;
                }
            }
        }
    }
    spans * 8 + ca_wet * 6 + w.bodies.values().map(|b| b.byte_size()).sum::<usize>()
}
```

- [ ] **Step 4: Implement `scenario.rs`**

```rust
//! The benchmark scenarios. Each is a 64³ world, built identically for both modes.

use crate::edit::{self, Edit};
use crate::fixture::{carve, fill_solid, fill_water, finish};
use crate::grid::Dims;
use crate::metrics::{dense_bytes, sparse_bytes, surface_spread};
use crate::state::{Stats, Undo, World};
use crate::{FULL, Mode, ca};

pub const NAMES: [&str; 6] = ["settled", "small_spill", "big_drain", "bucket", "film", "u_tube"];
/// Full-grid mass checks cost O(grid); run one every this many steps (and at the end).
pub const MASS_CHECK_EVERY: u32 = 64;

pub struct Scenario {
    pub name: &'static str,
    pub world: World,
    /// (step, edit), applied at the start of that step.
    pub edits: Vec<(u32, Edit)>,
    pub max_steps: u32,
    pub stop_when_asleep: bool,
    pub initial_mass: u64,
}

pub struct Report {
    pub name: &'static str,
    pub mode: Mode,
    pub steps: u32,
    pub asleep_at: Option<u32>,
    /// Nanoseconds per `ca::step` (all zero on wasm).
    pub step_ns: Vec<u64>,
    pub undo_bytes: Vec<u32>,
    pub mass_ok: bool,
    pub edit_errors: u32,
    pub stats: Stats,
    pub final_hash: u64,
    pub bodies: usize,
    pub dense_bytes: usize,
    pub sparse_bytes: usize,
}

pub fn build(name: &str, mode: Mode) -> Scenario {
    let d = Dims::cube(64);
    let mut w = World::new(d, mode);
    let (name, edits, max_steps, stop): (&'static str, Vec<(u32, Edit)>, u32, bool) = match name {
        "settled" => {
            lake(&mut w);
            ("settled", vec![], 100, false)
        }
        "small_spill" => {
            lake(&mut w);
            carve(&mut w, 8..=9, 10..=10, 20..=24);
            let e = vec![(0, Edit::Remove(d.idx(10, 10, 22))), (0, Edit::Remove(d.idx(11, 10, 22)))];
            ("small_spill", e, 20_000, true)
        }
        "big_drain" => {
            fill_solid(&mut w, 0..=35);
            carve(&mut w, 4..=27, 20..=35, 4..=59); // lake A, floor y 20
            carve(&mut w, 30..=53, 4..=35, 4..=27); // basin B, floor y 4
            fill_water(&mut w, 4..=27, 20..=31, 4..=59);
            let e = [(28, 20), (29, 20), (28, 21), (29, 21)]
                .map(|(x, y)| (0, Edit::Remove(d.idx(x, y, 16))))
                .to_vec();
            ("big_drain", e, 60_000, true)
        }
        "bucket" => {
            fill_solid(&mut w, 0..=3);
            carve(&mut w, 30..=32, 1..=3, 30..=32);
            let e = (0..20).map(|k| (k, Edit::Pour(d.idx(31, 10, 31), FULL))).collect();
            ("bucket", e, 2_000, true)
        }
        "film" => {
            fill_solid(&mut w, 0..=3);
            let e = vec![(0, Edit::Pour(d.idx(20, 4, 20), FULL)), (0, Edit::Pour(d.idx(21, 4, 20), FULL))];
            ("film", e, 2_000, true)
        }
        "u_tube" => {
            fill_solid(&mut w, 0..=45);
            carve(&mut w, 20..=21, 4..=45, 30..=31);
            carve(&mut w, 40..=41, 4..=45, 30..=31);
            carve(&mut w, 22..=39, 4..=5, 30..=31);
            let e = (0..100u32)
                .map(|k| (k, Edit::Pour(d.idx(20 + k % 2, 44, 30 + (k / 2) % 2), FULL)))
                .collect();
            ("u_tube", e, 20_000, true)
        }
        other => panic!("unknown scenario {other}"),
    };
    finish(&mut w);
    let initial_mass = w.mass();
    Scenario { name, world: w, edits, max_steps, stop_when_asleep: stop, initial_mass }
}

/// 40×40×8 m lake, 4 m freeboard.
fn lake(w: &mut World) {
    fill_solid(w, 0..=19);
    carve(w, 12..=51, 8..=19, 12..=51);
    fill_water(w, 12..=51, 8..=15, 12..=51);
}

/// Lake A's surface spread in `big_drain`.
pub fn drain_a_spread(w: &World) -> Option<u64> {
    surface_spread(w, 4..=27, 4..=59)
}

#[cfg(not(target_arch = "wasm32"))]
fn timed(f: impl FnOnce()) -> u64 {
    let t = std::time::Instant::now();
    f();
    t.elapsed().as_nanos() as u64
}

#[cfg(target_arch = "wasm32")]
fn timed(f: impl FnOnce()) -> u64 {
    f();
    0
}

fn step_timed(sc: &mut Scenario) -> (Vec<Undo>, u32, u64) {
    let Scenario { world: w, edits, .. } = sc;
    w.begin_log();
    let s = w.step;
    let mut errors = 0;
    for &(at, e) in edits.iter() {
        if at == s && edit::apply(w, e).is_err() {
            errors += 1;
        }
    }
    let ns = timed(|| ca::step(w));
    (w.end_log(), errors, ns)
}

/// One logged step (edits due now, then the water step). Returns the undo log and edit errors.
pub fn step_once(sc: &mut Scenario) -> (Vec<Undo>, u32) {
    let (log, errors, _) = step_timed(sc);
    (log, errors)
}

pub fn run(sc: &mut Scenario, mut probe: impl FnMut(&World)) -> Report {
    let last_edit = sc.edits.iter().map(|e| e.0).max();
    let mut r = Report {
        name: sc.name,
        mode: sc.world.mode,
        steps: 0,
        asleep_at: None,
        step_ns: Vec::new(),
        undo_bytes: Vec::new(),
        mass_ok: true,
        edit_errors: 0,
        stats: Stats::default(),
        final_hash: 0,
        bodies: 0,
        dense_bytes: 0,
        sparse_bytes: 0,
    };
    while sc.world.step < sc.max_steps {
        let (log, errors, ns) = step_timed(sc);
        r.edit_errors += errors;
        r.step_ns.push(ns);
        r.undo_bytes.push(log.iter().map(Undo::bytes).sum::<usize>() as u32);
        let s = sc.world.step;
        if s % MASS_CHECK_EVERY == 0 && sc.world.mass() != sc.initial_mass + sc.world.poured {
            r.mass_ok = false;
        }
        probe(&sc.world);
        if sc.stop_when_asleep && last_edit.is_none_or(|e| s > e) && sc.world.active.count() == 0 {
            r.asleep_at = Some(s);
            break;
        }
    }
    let w = &sc.world;
    r.mass_ok &= w.mass() == sc.initial_mass + w.poured;
    r.steps = w.step;
    r.stats = w.stats;
    r.final_hash = w.hash();
    r.bodies = w.bodies.len();
    r.dense_bytes = dense_bytes(w);
    r.sparse_bytes = sparse_bytes(w);
    r
}
```

Add `pub mod metrics;` and `pub mod scenario;` to `src/lib.rs`.

- [ ] **Step 5: Run the tests**

Run: `just water-proto-test --test t4_scenarios`
Expected: all 8 pass.
- `big_drain` runs about 20–25k steps in bodies mode; seconds in release.
- If `u_tube` bodies ends with 2 bodies, the two shaft bottoms seeded separately and never
  merged. The tunnel cells between them must be absorbed when full and level
  (`absorb_or_seed`'s all-touching check). Fix the rule there; do not loosen the test.

- [ ] **Step 6: Commit**

```bash
git add prototypes/water/src/scenario.rs prototypes/water/src/metrics.rs prototypes/water/src/lib.rs \
  prototypes/water/tests/t4_scenarios.rs
git commit -m "proto(water): six scenarios, runner, surface and size metrics

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Rollback and determinism checks, CLI, wasm golden hashes

**Files:**
- Create: `prototypes/water/tests/t5_rollback.rs`, `prototypes/water/src/bin/water_proto.rs`,
  `prototypes/water/src/wasm.rs`, `prototypes/water/wasm_golden.mjs`
- Modify: `prototypes/water/src/lib.rs` (add `#[cfg(target_arch = "wasm32")] mod wasm;`)
- Modify (do not commit): `justfile` (add `water-proto`, `water-proto-wasm`)

**Interfaces:**
- Consumes: `scenario::{NAMES, build, step_once, run}` and `World::{hash, undo, begin_log,
  end_log}`.
- Produces:
  - the binary `water_proto [--scenario NAME] [--mode cell|bodies] [--golden]`;
  - the wasm export `golden(scenario_index: u32, mode: u32) -> u64`, where mode 0 = cell and
    1 = bodies.

- [ ] **Step 1: Write the failing tests**

`prototypes/water/tests/t5_rollback.rs`:
```rust
use water_proto::body;
use water_proto::fixture::{carve, fill_solid, fill_water, finish};
use water_proto::grid::Dims;
use water_proto::scenario::{NAMES, build, run, step_once};
use water_proto::state::World;
use water_proto::{FULL, Mode};

/// Every 5th step: hash, logged step, undo must restore the hash, and replay must
/// reproduce the post-step hash.
fn check_undo(name: &str, mode: Mode, steps: u32) {
    let mut sc = build(name, mode);
    for _ in 0..steps {
        if sc.world.step % 5 == 0 {
            let h0 = sc.world.hash();
            let (log, _) = step_once(&mut sc);
            let h1 = sc.world.hash();
            sc.world.undo(log);
            assert_eq!(sc.world.hash(), h0, "{name} {} undo at step {}", mode.name(), sc.world.step);
            step_once(&mut sc);
            assert_eq!(sc.world.hash(), h1, "{name} {} replay diverged", mode.name());
        } else {
            step_once(&mut sc);
        }
    }
}

#[test]
fn undo_is_exact_all_scenarios() {
    for name in NAMES {
        for mode in [Mode::CellOnly, Mode::Bodies] {
            check_undo(name, mode, 300);
        }
    }
}

#[test]
fn undo_across_split() {
    let mut w = World::new(Dims::cube(32), Mode::Bodies);
    fill_solid(&mut w, 0..=15);
    carve(&mut w, 4..=9, 4..=15, 4..=9);
    carve(&mut w, 14..=19, 4..=15, 4..=9);
    carve(&mut w, 10..=13, 10..=15, 4..=9);
    fill_water(&mut w, 4..=9, 4..=12, 4..=9);
    fill_water(&mut w, 14..=19, 4..=12, 4..=9);
    fill_water(&mut w, 10..=13, 10..=12, 4..=9);
    finish(&mut w);
    let id = *w.bodies.keys().next().unwrap();
    let h0 = w.hash();
    w.begin_log();
    body::remove_volume(&mut w, id, 288 * FULL as u64 + 1);
    let log = w.end_log();
    assert_eq!(w.bodies.len(), 2);
    w.undo(log);
    assert_eq!(w.hash(), h0);
    assert_eq!(w.bodies.len(), 1);
}

#[test]
fn rerun_is_bit_identical() {
    for name in NAMES {
        for mode in [Mode::CellOnly, Mode::Bodies] {
            if name == "big_drain" && mode == Mode::CellOnly {
                continue; // 60k slow steps; the wasm golden check covers it
            }
            let a = run(&mut build(name, mode), |_| {});
            let b = run(&mut build(name, mode), |_| {});
            assert_eq!(a.final_hash, b.final_hash, "{name} {}", mode.name());
            assert!(a.mass_ok && a.edit_errors == 0, "{name} {}", mode.name());
        }
    }
}
```

- [ ] **Step 2: Run to see them fail or pass**

Run: `just water-proto-test --test t5_rollback`
Expected: this compiles with the existing API. It should pass. If an undo assertion fails, the
message names the scenario, mode and step. The usual cause is a mutation that bypasses the
logging setters, e.g. a direct `w.bodies.get_mut` or a `w.water[..] =` write outside
`fixture.rs`. Grep for them:
`grep -n "bodies.get_mut\|water\[.*\] =\|owner\[.*\] =" prototypes/water/src/{ca,body,edit}.rs`.
Note that `settle_init`'s `values_mut` is fixture-time and fine.

- [ ] **Step 3: Implement the CLI** (`src/bin/water_proto.rs`)

```rust
//! Scenario table: cost, undo bytes, state size, structural counters and hashes
//! for both models. `--golden` prints only "<scenario> <mode> <hash>" lines.

use water_proto::Mode;
use water_proto::scenario::{self, NAMES, drain_a_spread};

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let golden = args.iter().any(|a| a == "--golden");
    let only = arg(&args, "--scenario");
    let modes = match arg(&args, "--mode").as_deref() {
        Some("cell") => vec![Mode::CellOnly],
        Some("bodies") => vec![Mode::Bodies],
        _ => vec![Mode::CellOnly, Mode::Bodies],
    };
    if !golden {
        println!(
            "{:<12} {:<6} {:>6} {:>6} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>4} {:>5} {:>5} {:>5} {:>5} {:>5} {:>7}",
            "scenario", "mode", "steps", "asleep", "mean_us", "p99_us", "max_us", "visit/st",
            "undoB/st", "undoBmax", "denseKiB", "sparseKiB", "bods", "seed", "absb", "expn",
            "split", "merge", "A_sprd"
        );
    }
    for name in NAMES {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        for &mode in &modes {
            let mut sc = scenario::build(name, mode);
            let mut spread = 0u64;
            let r = scenario::run(&mut sc, |w| {
                if name == "big_drain" && w.step % 100 == 0 {
                    spread = spread.max(drain_a_spread(w).unwrap_or(0));
                }
            });
            if golden {
                println!("{name} {} {:016x}", mode.name(), r.final_hash);
                continue;
            }
            let n = r.step_ns.len().max(1) as f64;
            let mut ns = r.step_ns.clone();
            ns.sort_unstable();
            let p99 = ns.get(((ns.len() as f64) * 0.99) as usize).copied().unwrap_or(0);
            let undo_mean = r.undo_bytes.iter().map(|&b| b as f64).sum::<f64>() / n;
            println!(
                "{:<12} {:<6} {:>6} {:>6} {:>9.2} {:>9.2} {:>9.2} {:>9.1} {:>9.0} {:>9} {:>9.1} {:>9.1} {:>4} {:>5} {:>5} {:>5} {:>5} {:>5} {:>7}{}",
                name,
                mode.name(),
                r.steps,
                r.asleep_at.map_or("-".to_string(), |s| s.to_string()),
                r.step_ns.iter().sum::<u64>() as f64 / n / 1000.0,
                p99 as f64 / 1000.0,
                ns.last().copied().unwrap_or(0) as f64 / 1000.0,
                r.stats.cells_visited as f64 / n,
                undo_mean,
                r.undo_bytes.iter().max().copied().unwrap_or(0),
                r.dense_bytes as f64 / 1024.0,
                r.sparse_bytes as f64 / 1024.0,
                r.bodies,
                r.stats.seeds,
                r.stats.absorbs,
                r.stats.expansions,
                r.stats.splits,
                r.stats.merges,
                spread,
                if r.mass_ok && r.edit_errors == 0 { "" } else { "  !! MASS/EDIT FAILURE" },
            );
        }
    }
}
```

- [ ] **Step 4: Implement the wasm export and the node runner**

`src/wasm.rs`:
```rust
//! wasm32 entry: run one scenario and return its final state hash, for comparison
//! with the native `water_proto --golden` output.

use crate::{Mode, scenario};

#[unsafe(no_mangle)]
pub extern "C" fn golden(scenario_index: u32, mode: u32) -> u64 {
    let mode = if mode == 0 { Mode::CellOnly } else { Mode::Bodies };
    let mut sc = scenario::build(scenario::NAMES[scenario_index as usize], mode);
    scenario::run(&mut sc, |_| {}).final_hash
}
```
Add to `src/lib.rs`: `#[cfg(target_arch = "wasm32")] mod wasm;`

`prototypes/water/wasm_golden.mjs`:
```js
// Runs every prototype scenario in the wasm32 build and prints
// "<scenario> <mode> <hash>", the same lines `water_proto --golden` prints natively.
import { readFileSync } from "node:fs";

const NAMES = ["settled", "small_spill", "big_drain", "bucket", "film", "u_tube"];
const MODES = ["cell", "bodies"];
const { instance } = await WebAssembly.instantiate(readFileSync(process.argv[2]), {});
for (let s = 0; s < NAMES.length; s++) {
  for (let m = 0; m < MODES.length; m++) {
    const h = BigInt.asUintN(64, instance.exports.golden(s, m));
    console.log(`${NAMES[s]} ${MODES[m]} ${h.toString(16).padStart(16, "0")}`);
  }
}
```
`NAMES` must stay in the same order as `scenario::NAMES`.

- [ ] **Step 5: Add the run and wasm recipes to `justfile`** (do not commit it)

Below `water-proto-test`:
```just
# Scenario table for both water models (a TIMING run: ask before running on the shared box).
# `just water-proto --golden` prints only final state hashes (not a timing run).
water-proto *args:
    cd prototypes/water && {{cap}} cargo run --release --quiet --bin water_proto -- {{args}}

# The same scenarios compiled to wasm32 and run under node; hashes must equal the native ones.
water-proto-wasm:
    cd prototypes/water && {{cap}} cargo build --release --lib --target wasm32-unknown-unknown
    cd prototypes/water && node wasm_golden.mjs target/wasm32-unknown-unknown/release/water_proto.wasm > target/golden-wasm.txt
    cd prototypes/water && {{cap}} cargo run --release --quiet --bin water_proto -- --golden > target/golden-native.txt
    cd prototypes/water && diff target/golden-native.txt target/golden-wasm.txt && echo "golden hashes match (native == wasm32)"
```

- [ ] **Step 6: Verify**

Run: `just water-proto-test --test t5_rollback`
Expected: 3 pass.

Run: `just water-proto-wasm`
Expected: last line `golden hashes match (native == wasm32)`. A mismatch names the scenario
through `diff`. Any divergence is a determinism bug, most likely platform-width `usize`
arithmetic leaking into state. The state uses only `u16`/`u32`/`u64`, so look for `as usize`
used in a value rather than an index.

Run: `just water-proto-test` (all five test files together, as the final gate for the code)
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add prototypes/water/tests/t5_rollback.rs prototypes/water/src/bin prototypes/water/src/wasm.rs \
  prototypes/water/src/lib.rs prototypes/water/wasm_golden.mjs
git commit -m "proto(water): rollback/determinism tests, scenario CLI, wasm golden hashes

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Amendments of 2026-11-06 (Tasks 6–11)

**Spec for these tasks:** `docs/superpowers/specs/2026-11-06-water-revisit/00-synthesis.md`, the
sections "Flow models" and "What players can do with water". Tasks 1–5 stand as written; the
tasks below extend them. Each task starts from the state the previous one committed.

**The order of one step** after these amendments, inside one undo log:
1. edits due this step;
2. evaporation due this step (Task 9);
3. the CA pass over awake cells, in checkerboard order (Task 6);
4. the link phase (Task 7);
5. the after-step: expansions and shore wake-ups (Tasks 2–3).

The expected values below depend on this order.

**New mass ledger.** `World::accounted() = mass() + evaporated + scooped`. Every mass check in
`scenario::run` and the tests becomes `accounted() == initial_mass + poured` (Task 7 makes the
change; Tasks 9 and 11 add the fields it sums).

**New tunables** (in `src/lib.rs`, with the others):

| Constant | Value | Task |
|---|---|---|
| `GATE_CAP` | 1024 (`FULL / 4`) | 7 |
| `PUMP_RATE` | 512 (`FULL / 8`) | 7, 11 |
| `BRICK` | 8 (edge of a journal brick, in cells) | 8 |
| `EVAP_BELOW` | 585 (`FULL / 7`, DF's 1/7: a cell holding less than this is a film; films can rest just above `FILM`) | 9 |
| `EVAP_AFTER_STEPS` | 64 | 9 |
| `SEAM_CAP` | 512 (per face cell, per step) | 10 |
| `SEAM_LAG_STEPS` | 1 | 10 |
| `SEAM_SETTLE_TOL` | 2 (units of level) | 10 |

**Additional review focus** (each pinned by a test in its task):
1. A pump or gate whose source is dry moves nothing and creates nothing:
   `pump_from_dry_source_moves_nothing` (Task 7).
2. Opening and closing a gate in the same step leaves it closed, and the hash equals a
   never-opened run: `gate_open_close_same_step_is_closed` (Task 7).
3. Scooping more than a cell or body holds takes what is there; scooping a dry cell is
   rejected with no state change: `scoop_dry_cell_rejected` (Task 11).
4. A seam face cell that is solid on either side never exchanges:
   `seam_solid_face_is_closed` (Task 10).
5. Evaporation never takes water from a body member or from a cell holding `EVAP_BELOW` or
   more: `evaporation_spares_bodies_and_deep_cells` (Task 9).

---

### Task 6: Checkerboard frontier order

> **Pre-flight ruling (2026-11-06), overrides the text below:** The test asserts `w.mass() == 4 * FULL as u64` (`accounted()` arrives in Task 7). Files also include `src/grid.rs` (`parity`).

**Files:**
- Modify: `prototypes/water/src/ca.rs` (`step`)
- Test: `prototypes/water/tests/t6_order.rs`

**Interfaces:**
- Consumes: `ca::step`, `World::active`, `Dims::xyz`.
- Produces: `grid::parity(dims: Dims, i: u32) -> u32` (`(x + y + z) % 2`). `ca::step` keeps
  its signature.

- [ ] **Step 1: Write the failing test**

`checkerboard_spill_is_unbiased`:
- 64³ world, `fill_solid(0..=3)` (a flat plain), bodies mode;
- four `Edit::Pour(idx(32, 8, 32), FULL)` at steps 0..=3;
- run to sleep (`max_steps` 5000);
- sum the water at x < 32 and at x > 32; assert `|left − right| <= FILM as u64`;
- assert the same for z;
- assert `accounted() == 4 * FULL as u64`.

If the bound fails with the checkerboard in place, report the measured imbalance; don't widen
the bound silently.

- [ ] **Step 2: Run to see it fail.** `just water-proto-test --test t6_order`. Expected: the
  imbalance assertion fails under the ascending sweep. If it passes already, say so in the
  report and keep the test as a regression pin.

- [ ] **Step 3: Change the visit order in `ca::step`.**
  - Take `w.active.ones()` once at the start of the pass.
  - Visit the cells with `parity == step % 2` first, then the rest, each group in ascending
    index.
  - A cell activated during the pass is not visited until the next step (as before).
  - Edge rules and `horizontal_order(step)` stay unchanged.

- [ ] **Step 4: Run Tasks 3–6 tests.** `just water-proto-test`. Expected: all pass.
  - The end-state assertions of Tasks 3–5 (levels, mass, invariants) are order-independent by
    design.
  - If an exact expected value moves, find out why before touching the test, and say so in the
    commit message.

- [ ] **Step 5: Commit** `prototypes/water/src/ca.rs prototypes/water/src/grid.rs
  prototypes/water/tests/t6_order.rs` with message
  `feat(water-proto): checkerboard visit order for the frontier CA`.

---

### Task 7: Links: the shared edge rule (gates and pumps)

> **Pre-flight ruling (2026-11-06), overrides the text below:** (1) **Link ids.** `add_link` returns `links.last_key() + 1`, starting at 1. Links are never removed; `Undo::Link(id, None)` undoes an add. (2) **Zero amounts.** `take` and `give` with `q == 0` change nothing and log nothing. (3) **No expansion through links.** `give` into a member is capped by the body's current `room()`, and the remainder goes back per rule 4. (4) **Stopping.** Add `Stats.link_moved_last_step: u64`, the total moved in this step's link phase. `scenario::run`'s asleep condition becomes `active.count() == 0 && link_moved_last_step == 0`. Task 9 extends it.

**Files:**
- Create: `prototypes/water/src/link.rs`
- Modify:
  - `src/lib.rs`: module and constants;
  - `src/state.rs`: `links`, `scooped`, `Undo::Link`, `Undo::Scooped`, `accounted`, and
    links in `hash`;
  - `src/ca.rs`: call `link::flux_all` between the CA pass and the after-step;
  - `src/edit.rs`: `Edit::SetLink`;
  - `src/scenario.rs`: the mass check uses `accounted`.
- Test: `prototypes/water/tests/t7_links.rs`

**Interfaces:**
- Consumes: `body::{add_volume, remove_volume}`, `Body::{level, room}`, `World` setters.
- Produces:
  - `link::{LinkId = u32, Link { a: u32, b: u32, kind: LinkKind }, LinkKind::{Gate { open:
    bool }, Pump { rate: u16 }}}`;
  - `World.links: BTreeMap<LinkId, Link>`, plus `World::add_link(Link) -> LinkId` (logged);
  - `link::head(&World, i) -> Option<u64>`:
    - `None` for a solid cell;
    - `y * FULL + water` for a CA cell;
    - `body.level()` for a member;
  - `link::avail(&World, i) -> u64`: the cell's water, or its body's `volume`;
  - `link::room(&World, i) -> u64`: `FULL − water` for a CA cell, `Body::room()` for a
    member;
  - `link::take(&mut World, i, q: u64) -> u64` and `link::give(&mut World, i, q: u64) -> u64`:
    each returns the amount actually moved, wakes the cell, and records a touched body;
  - `link::flux_all(&mut World)`;
  - `Edit::SetLink(LinkId, LinkKind)`;
  - `World.scooped: u64`;
  - `World::accounted() -> u64`.

**The rule** (`flux_all`; the algorithm the tests don't determine):
1. For every link in ascending `LinkId`, compute a wanted amount `q` from the state as it was
   *before* this phase. Nothing is applied yet.
   - `Gate { open: true }`: from the higher head `h_hi` to the lower `h_lo`,
     `q = min(GATE_CAP, (h_hi − h_lo) / 2)`. 0 if either head is `None`, or if the gate is
     closed.
   - `Pump { rate }`: from `a` to `b`, `q = rate`, whatever the heads.
2. Cap each `q` by `room(destination)` as it was before the phase.
3. For each source cell, or each body when the source is a member (all members of a body are
   one source), let `out` be the sum of its wanted amounts. If `out > avail`, scale each of
   its amounts to `q * avail / out`, rounded down.
4. Apply in ascending `LinkId`: `take` from the source, then `give` the amount taken to the
   destination. If `give` returns less than was taken, `give` the remainder back to the
   source. Mass then stays exact even if two links fill one destination.

- [ ] **Step 1: Write the failing tests** (all in bodies mode, using the "two basins"
  fixture):
  - **The fixture:**
    - 64³ world, `fill_solid(0..=20)`;
    - basin A: `carve(4..=19, 12..=20, 4..=19)` with `fill_water(4..=19, 12..=17, 4..=19)`,
      i.e. 256 cells per layer at level `18 * F`;
    - basin B: `carve(21..=44, 4..=20, 4..=19)`, dry, 384 cells per layer;
    - the solid wall x = 20 stays;
    - `a = idx(19, 12, 11)` is a member of A's bottom layer;
    - `b = idx(21, 12, 11)` is the air above B.
  - `gate_closed_holds`: a closed gate `a → b`; after 50 steps the hash of all water and bodies
    equals the start (the step counter excluded), and `accounted()` is exact.
  - `gate_open_moves_cap_per_step`:
    - the gate is opened by `SetLink` at step 10 and closed at step 210;
    - run to sleep;
    - B's box holds exactly `200 * GATE_CAP` units, and A lost exactly that.
    - Why it is exact: A's head stays above `b`'s, and `b` empties by falling in each CA pass
      before the link phase.
  - `gate_open_close_same_step_is_closed`: both edits at step 10, open first. Its final hash
    equals the never-opened run's at the same step.
  - `scarce_source_is_shared_proportionally`:
    - 32³, `fill_solid(0..=20)`;
    - three sealed one-cell pockets carved at `(10, 15, 10)`, `(20, 15, 20)` and
      `(28, 15, 28)`;
    - `Edit::Pour` puts 1000 units in the first at step 0;
    - two pumps of `rate = 1000` (links 1 and 2) run from it to the other two;
    - after that step each destination holds 500 and the source holds 0.
    - The pockets are sealed so that the CA pass can't move the water before the link phase.
  - `pump_lifts_water_uphill`:
    - a `Pump { rate: PUMP_RATE }` from `a` (A's bottom, y12) to `idx(50, 21, 50)`, the air on
      the plain's surface outside both basins;
    - after 8 steps, the water outside both basins totals exactly `8 * PUMP_RATE`.
  - `pump_from_dry_source_moves_nothing`: a pump from a dry CA cell. After 10 steps
    `accounted()` and every cell are unchanged.

- [ ] **Step 2: Run to see them fail.** `just water-proto-test --test t7_links`. Expected:
  compile errors (`link` doesn't exist).

- [ ] **Step 3: Implement `link.rs` and the `World` fields.**
  - `Undo::Link(LinkId, Option<Link>)` and `Undo::Scooped(u64)`, each with a `bytes()` entry.
  - `hash` covers `links` (in id order) and `scooped`.
  - `Edit::SetLink` on an unknown id returns `EditError::NoRoom`; don't add a new variant for
    this throwaway.

- [ ] **Step 4: Run all prototype tests.** `just water-proto-test`. Expected: all pass.
  - Scenarios without links are unaffected: `flux_all` over an empty map must do nothing and
    log nothing.

- [ ] **Step 5: Commit** the files above with message
  `feat(water-proto): links with a start-of-step edge rule (gates, pumps)`.

---

### Task 8: Brick-journal undo

> **Pre-flight ruling (2026-11-06), overrides the text below:** Assert the brick-multiple dims in `begin_log` when `journal == Brick`, not in `World::new`, which has no journal argument. Files also include `src/grid.rs` (`brick_of`) and `src/bin/water_proto.rs` (`--undo`).

The engine's per-mutation undo entry clones a value, takes a global mutex, and is as large as
the biggest event variant (`02-engine-today.md` §2). This task measures the alternative: the
first touch of an 8³ brick in a step saves the brick once.

**Files:**
- Modify: `prototypes/water/src/state.rs`
- Test: `prototypes/water/tests/t8_journal.rs`

**Interfaces:**
- Consumes: the `World` cell setters (`set_water`, `set_owner`, `set_active`, `touch`).
- Produces:
  - `state::JournalMode::{PerCell, Brick}` and the field `World.journal` (default `PerCell`;
    config, not hashed);
  - `Undo::Brick(u32, Box<BrickImage>)`, where `BrickImage` holds the brick's `water`,
    `owner`, `active` and `last_touch`;
  - `Dims::brick_of(i) -> u32`;
  - `World::journal_bricks(&self) -> u32` (bricks imaged in the open log, for stats).

**Rules:**
- In `Brick` mode the four cell setters save the cell's brick once per open log (a scratch
  `Bits`, cleared by `begin_log`) and then write without logging.
- Solid, bodies, links and scalars keep their per-entry logging in both modes.
- Dims must be multiples of `BRICK`: `World::new` asserts it in `Brick` mode.
- `bytes()` of a brick image = 4 + 512 × (2 + 4 + 4) + 64.

- [ ] **Step 1: Write the failing tests:**
  - `brick_journal_undo_is_exact_all_scenarios`: for every name in `scenario::NAMES` in
    bodies mode with `journal = Brick`:
    - step 40 times;
    - then take 20 logged steps, recording the hash before each;
    - undo them in reverse;
    - each undo restores the matching hash.

    This mirrors Task 5's per-cell test.
  - `brick_journal_logs_each_brick_once`: one step of `small_spill` in `Brick` mode yields at
    most one `Undo::Brick` per brick index.

- [ ] **Step 2: Run to see them fail.** `just water-proto-test --test t8_journal`. Expected:
  compile errors.

- [ ] **Step 3: Implement the mode in `state.rs`.**

- [ ] **Step 4: Run all prototype tests.** `just water-proto-test`. Expected: all pass. Task
  5's per-cell tests still pass, because the default stays `PerCell`.

- [ ] **Step 5: Add `--undo cell|brick`** to `water_proto` (Task 5's CLI). It sets
  `world.journal` on every built scenario. Run `just water-proto --golden --undo brick`; the
  hashes must equal the `--undo cell` ones, because the journal mode is not state.

- [ ] **Step 6: Commit** `src/state.rs src/grid.rs src/bin/water_proto.rs
  tests/t8_journal.rs` with message `feat(water-proto): brick-journal undo mode`.

---

### Task 9: Thin films evaporate into the ledger (off by default)

> **Pre-flight ruling (2026-11-06), overrides the text below:** (1) Keep a reverse map `evap_of: BTreeMap<u32 cell, u32 due>` next to `evap_due`. Both are state: hashed and undone with `Undo::EvapDue`. A write to the cell's water removes both entries. (2) `scenario.rs` is in Files: `run`'s asleep condition also needs `evap_due.is_empty()`.

This waits on user decision 1 in the 2026-11-06 synthesis, so it is **off** unless a scenario
turns it on. It is DF's 1/7 evaporation done without breaking conservation.

**Files:**
- Modify: `src/state.rs` (`evaporated`, `evap_after`, `evap_due`, their undo entries and the
  hash), `src/ca.rs`
- Test: `prototypes/water/tests/t9_evaporation.rs`

**Interfaces:**
- Consumes: `World::accounted` (Task 7), and `ca::step`'s sleep logic.
- Produces:
  - `World.evaporated: u64` (state, hashed, `Undo::Evaporated`);
  - `World.evap_after: u32` (config, not hashed; 0 = off; `World::new` sets 0);
  - `World.evap_due: BTreeSet<(u32, u32)>` (state, hashed, `Undo::EvapDue(u32, u32, bool)`).
    It is the item-pile expiry-index pattern: `(due_step, cell)`.

**Rules:**
- When a CA cell goes to sleep while every one of these holds:
  - `0 < water < EVAP_BELOW`;
  - it rests on solid or a member;
  - `evap_after > 0`;

  insert `(step + evap_after, cell)`.
- Any write to the cell's water removes its pending entry (look it up by the cell's
  `last_touch` + `evap_after`, or keep a reverse map; choose one and say which in the commit).
- At step start (stage 2 of the step order), pop every due entry. If the cell is still a CA
  cell with `0 < water < EVAP_BELOW`, move its water to `evaporated`.

- [ ] **Step 1: Write the failing tests:**
  - `film_evaporates_into_ledger`: the `film` scenario with `evap_after = EVAP_AFTER_STEPS`,
    run until asleep and the queue is empty (`max_steps` 5000). Then:
    - every `water` entry is 0;
    - `evaporated == initial_mass + poured`;
    - `accounted()` is exact.
  - `film_stays_when_evaporation_off`: the same scenario with `evap_after = 0` ends with
    `evaporated == 0`, and its wet-cell count is inside Task 4's range.
  - `evaporation_spares_bodies_and_deep_cells`:
    - The `bucket` scenario with evaporation on: the body's level still equals Task 4's
      expected `3 * F + 2 * F / 9`, and `accounted()` is exact.
    - Two sealed one-cell pockets (32³, `fill_solid(0..=20)`) poured with 600 and 500 units at
      step 0, run 300 steps.
    - The 600 pocket still holds 600.
    - The 500 pocket is 0, with `evaporated == 500`.
    - Before step `EVAP_AFTER_STEPS` it still held 500.

- [ ] **Step 2: Run to see them fail.** `just water-proto-test --test t9_evaporation`.

- [ ] **Step 3: Implement it.** Evaporation must not keep cells awake: a film costs one queue
  entry while it waits, not a visit per step.

- [ ] **Step 4: Run all prototype tests.** `just water-proto-test`. Expected: all pass.

- [ ] **Step 5: Commit** with message
  `feat(water-proto): film evaporation into the ledger (off by default)`.

---

### Task 10: Two worlds joined at a seam, with lag

> **Pre-flight ruling (2026-11-06), overrides the text below:** (1) The `seam_drain` residue bound is `A's total < 2 * 448` units, not `<= 512`, because the seam's `(own − seen) / 2` floors. Report the measured residue. (2) In bodies mode the full sealed pocket in `seam_bounce` seeds a one-cell body, so read "the pocket ends at exactly FULL" through `link::avail(pocket)`, not `water[]`.

This models two regions on different threads. Each side sends only its own water. It subtracts
the water at once and posts it as an arrival that lands `SEAM_LAG_STEPS` later. Heads seen
across the seam are the other side's heads as reported at the end of the previous step.

**Files:**
- Create: `prototypes/water/src/seam.rs`
- Modify:
  - `src/lib.rs`;
  - `src/wasm.rs`: `golden_seam(index: u32) -> u64`;
  - `wasm_golden.mjs`: print the seam goldens too;
  - `src/bin/water_proto.rs`: `--seam` prints a table per seam scenario.
- Test: `prototypes/water/tests/t10_seam.rs`

**Interfaces:**
- Consumes: `link::{head, avail, room, take, give}` (Task 7), `ca::step`, and
  `World::{begin_log, end_log, undo, hash, accounted}`.
- Produces:
  - `seam::Arrival { to_b: bool, y: u32, z: u32, amount: u64, due: u32 }`;
  - `seam::Pair { a: World, b: World, inflight: Vec<Arrival>, seen_by_a: Vec<Option<(u64,
    u64)>>, seen_by_b: Vec<Option<(u64, u64)>>, bounced: u64, initial: u64 }`:
    - each seen entry is the other side's `(head, room)` for that face cell, or `None` when it
      is solid;
    - `Pair::new` fills both from the initial worlds.
  - `Pair::new(a, b) -> Pair`. The two worlds must have equal `y` and `z`. The seam is between
    A's face `x = dims.x − 1` and B's face `x = 0`, and face cells are indexed by `(y, z)`.
  - `Pair::step(&mut self) -> PairUndo` and `Pair::undo(&mut self, PairUndo)`;
  - `Pair::hash(&self) -> u64` (both worlds, the in-flight arrivals in order, both seen-head
    vectors);
  - `Pair::accounted(&self) -> u64` (both worlds plus in-flight);
  - `Pair::net_flux_last_step(&self) -> i64` (A→B positive);
  - `seam::NAMES: [&str; 3] = ["seam_drain", "seam_lake", "seam_bounce"]` and
    `seam::build(name) -> Pair`.

**One pair step** (the algorithm):
1. **Deliver arrivals.** Deliver every arrival with `due == step` in `inflight` order: `give`
   it to the destination face cell. A remainder that doesn't fit becomes a new arrival back to
   the sender, due next step, and adds to `bounced`. Water is never dropped.
2. **Step each world.** `ca::step` on A, then on B. Order is irrelevant: they share nothing.
3. **Compute outflow.** For each face `(y, z)`, on each side, using its own `head` now and the
   other side's head from `seen_by_*` (stale by one step):
   - `q = min(SEAM_CAP, (own − seen_head) / 2, seen_room)` when positive;
   - 0 when either side is `None`;
   - capped by `avail`.

   Capping by the room the other side last reported is what lets a full receiver settle. Only
   the one-step staleness of that room can cause a bounce.

   Then `take` it and push an arrival due at `step + SEAM_LAG_STEPS`. All faces of a body share
   its volume, so scale them as in Task 7's rule 3.
4. **Report heads.** Refresh both `seen_by_*` vectors from the current heads.

- [ ] **Step 1: Write the failing tests:**
  - **Fixtures** (32³ worlds, `fill_solid(0..=20)` on both sides):
    - `seam_drain`:
      - A has `carve(4..=31, 12..=20, 8..=23)` with water `4..=31, 12..=17, 8..=23`
        (2688 m³);
      - B has the deep basin `carve(0..=27, 4..=20, 8..=23)` (448 cells per layer).
    - `seam_lake`: as `seam_drain`, but B is `carve(0..=27, 12..=20, 8..=23)`, dry and level
      with A's floor.
    - `seam_bounce`:
      - A as in `seam_drain`;
      - B solid except one sealed cell `carve(0..=0, 13..=13, 12..=12)`.
  - `seam_conserves_mass_every_step`: for all three fixtures, for 2000 steps,
    `accounted() == initial` after every step.
  - `seam_drain_moves_lake_into_neighbour`: run until both worlds sleep with `inflight` empty.
    - B is one body with `level.abs_diff(10 * F) <= 512`: 2688 m³ over 448 cells is 6 layers,
      y4..=9.
    - A holds at most 512 units.
  - `seam_lake_levels_across_the_seam`: settled at `max_steps` 20,000.
    - `|level_A − level_B| <= SEAM_SETTLE_TOL`;
    - the two levels sum to `2 * 15 * F` within `SEAM_SETTLE_TOL`: 2688 m³ over 896 cells is
      3 layers each, y12..=14.
  - `seam_flux_is_damped`: in `seam_lake`, the sign of `net_flux_last_step` changes at most
    4 times before settling.
    - If this fails, the lag needs damping. Try `q = diff / 4` for seam faces, record both
      results, and report which held.
    - This decides the seam gain the engine uses.
  - `seam_full_receiver_bounces_back`: run `seam_bounce` until both worlds sleep with
    `inflight` empty.
    - The pocket ends at exactly `FULL`, and mass is exact.
    - `bounced == SEAM_CAP as u64`. Why: sends happen at steps 0–8, because the room seen at
      step 8 is still the one-step-old 512. Deliveries 1–8 fill the pocket, and the 9th
      bounces.
  - `seam_solid_face_is_closed`: replace B in `seam_drain` with a fully solid world. After 200
    steps no arrival was ever created, and A's hash equals A stepped alone.
  - `pair_step_undo_is_exact`: in `seam_lake`, step 30. Then take 10 logged pair steps,
    recording `Pair::hash` before each, and undo them in reverse; every hash is restored.

- [ ] **Step 2: Run to see them fail.** `just water-proto-test --test t10_seam`.

- [ ] **Step 3: Implement `seam.rs`, the wasm export and the CLI table.**

- [ ] **Step 4: Run all prototype tests, then the goldens.**
  - Run `just water-proto-test`, then `just water-proto --golden` and `just water-proto-wasm`.
  - Expected: all tests pass.
  - The seam goldens printed by node equal the native ones.

- [ ] **Step 5: Commit** with message `feat(water-proto): two-world seam with lagged, capped flux`.

---

### Task 11: Player scenarios

> **Pre-flight ruling (2026-11-06), overrides the text below:** (1) `dam_breach`: replace "A's body is gone" with "A's box holds at most 512 units". A residue body may survive at the outlet's tolerance. Report the residue. (2) Name the moved fixture `fixture::gate_basins(mode) -> World`. It returns a finished world, `scenario::build` does not call `finish` on it again, and it avoids a clash with t2's local `two_basins`. (3) Files also include `wasm_golden.mjs` (it must iterate all 10 names) and `tests/t7_links.rs` (switch it to the moved fixture).

These are the synthesis's player actions, run as scenarios. They join the CLI table and the
wasm goldens through `NAMES`.

**Files:**
- Modify:
  - `src/edit.rs`: `Edit::Scoop(u32, u16)` and `EditError::Dry`;
  - `src/scenario.rs`: four new names; `NAMES` becomes `[&str; 10]`;
  - `src/fixture.rs`: `two_basins(mode) -> World`, the Task 7 fixture moved here so tests and
    scenarios share it.
- Test: `prototypes/water/tests/t11_players.rs`

**Interfaces:**
- Consumes: `link::{take, give}`, `Edit::SetLink`, `World::add_link`, and Task 4's
  `Scenario`/`run`.
- Produces:
  - `Edit::Scoop(i, q)`: `take(i, q)`, adding the amount taken to `scooped`. Taking 0 returns
    `Err(EditError::Dry)` with no state change.
  - The scenarios `"dam_breach"`, `"gate_flood"`, `"pumped_cistern"` and `"scoop"`.

- [ ] **Step 1: Write the failing tests** (bodies mode; each runs its scenario to sleep with
  `max_steps` 20,000 and asserts `mass_ok` and `edit_errors == 0` first):
  - `dam_breach_drains_lake_into_valley`:
    - `two_basins`, plus `Edit::Remove` of the wall cells `x = 20`, `z = 11..=12`,
      `y = 12..=20`, all at step 0;
    - A's 1536 m³ fits under its floor in B: 384 cells per layer, 4 layers, y4..=7;
    - so B ends as one body with `level.abs_diff(8 * F) <= 512`;
    - A's box holds at most 512 units, and A's body is gone.
  - `gate_flood_open_then_close`:
    - `two_basins` with a closed gate `a → b`, opened at step 10 and closed at step 210;
    - B's box holds exactly `200 * GATE_CAP`;
    - B's box total is unchanged between step 211 and the end.
  - `pumped_cistern_fills_one_layer`:
    - 64³, `fill_solid(0..=14)`;
    - the pool `carve(4..=11, 4..=14, 4..=11)` with water `4..=11, 4..=8, 4..=11` (level
      `9 * F`);
    - the cistern `carve(30..=32, 12..=14, 30..=32)`;
    - a pump `idx(4, 4, 4) → idx(31, 14, 31)` set to `PUMP_RATE` at step 0 and to 0 at step
      72.
    - The cistern box holds exactly `9 * F` (72 × 512).
    - The pool body's level is `9 * F − 576` (9 m³ over 64 cells).
  - `scoop_lowers_lake_by_one_bucket`: `two_basins`, with `Edit::Scoop(a, FULL)` at step 0.
    A's level is `18 * F − 16` (one bucket over 256 cells), and `scooped == F`.
  - `scoop_dry_cell_rejected`: scooping a dry CA cell returns `Err(Dry)`, and the hash is
    unchanged.

- [ ] **Step 2: Run to see them fail.** `just water-proto-test --test t11_players`.

- [ ] **Step 3: Implement the edit, the fixture move and the four scenarios.**

- [ ] **Step 4: Run all prototype tests and both goldens.** `just water-proto-test`,
  `just water-proto --golden`, `just water-proto-wasm`. Expected: all tests pass, and wasm
  equals native for all 10 scenarios and 3 seam scenarios.

- [ ] **Step 5: Commit** with message
  `feat(water-proto): player scenarios (breach, gate, pump, scoop)`.

---

### Task 12: Measure and record results

**Files:**
- Create: `docs/superpowers/specs/2026-11-06-water-revisit/03-prototype-results.md`
- Modify: add a one-paragraph "Prototype results" pointer to
  `docs/superpowers/specs/2026-11-06-water-revisit/00-synthesis.md` (under "What to build, in
  order") and to `docs/superpowers/specs/2026-09-22-voxel-water-efficiency/00-synthesis.md`
  (under "User decisions").

- [ ] **Step 1: Ask the user before the timing run.** It pins 16 cores for ~2–4 minutes.
  Suggested wording: "Ready to run the water prototype timing tables (`just water-proto`,
  with `--undo brick`, and `--seam`), release, about 2–4 min. OK to run now?"

- [ ] **Step 2: Run it (after the OK) and keep the raw output.**
  Run `just water-proto`, `just water-proto --undo brick` and `just water-proto --seam`, each
  piped through `tee` into `$CLAUDE_JOB_DIR/tmp/`. Also record:
  - the CPU state: `cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq`;
  - the power source: `cat /sys/class/power_supply/*/online`.

  Per CLAUDE.md, the laptop's power clamp can make absolute times several times slower, so
  compare ratios.

- [ ] **Step 3: Write `03-prototype-results.md`** with these sections, filled from the tables:
  1. **Setup:** commit, CPU frequency and power, release profile, world sizes, constants from
     `lib.rs`.
  2. **The tables** (pasted raw). Then, per scenario:
     - the **bodies ÷ cell ratio** of mean µs/step, cells visited/step and steps to sleep;
     - mean undo bytes/step for **per-cell vs brick-journal**.
  3. **Correctness gates**, each pass/fail with the command that proved it:
     - mass exact (`accounted`);
     - undo exact (both journal modes);
     - rerun identical;
     - wasm32 == native (single-world and seam goldens);
     - invariants.
  4. **Behaviour:**
     - lake A's max surface spread during `big_drain`;
     - U-tube levels;
     - film area, with evaporation off and on;
     - the spill imbalance from `checkerboard_spill_is_unbiased`;
     - **seam:** sign reversals of the net flux, the final level difference, the bounced
       volume, and which seam gain held (`diff / 2` or `diff / 4`).
  5. **Interface findings:** `seeds`, `absorbs`, `expansions`, `splits` and `merges` per
     scenario. Flag any sign of member/CA flicker: absorbs far above the number of distinct
     cells that ended as members.
  6. **Decision gate.** The bodies model passes if *all* of these hold:
     - (a) every correctness gate passes;
     - (b) `settled` costs 0 visits in both modes;
     - (c) `big_drain` bodies mean ≤ 20 µs/step native, and lake A's spread is 0;
     - (d) U-tube levels are equal;
     - (e) bodies mean undo bytes/step is below cell-only's on `small_spill` and
       `big_drain`;
     - (f) every player scenario (`dam_breach`, `gate_flood`, `pumped_cistern`, `scoop`)
       meets its exact expected values;
     - (g) the seam conserves mass every step and is damped (`seam_flux_is_damped` passes
       with one of the two gains).

     Also report, without gating on it, whether the brick journal's bytes/step beat per-cell
     on `big_drain` and `dam_breach`. That decides the engine's undo shape for water.

     If (c), (e) or (g) fails, or (5) shows flicker, record which. The synthesis fallback is
     dense bricks plus analytic settle of quiet patches. Say whether the numbers argue for it.
  7. **What the prototype does not model**, carried into any engine plan:
     - chisel/half-block capacity;
     - the finite world sea ledger beyond films;
     - currents for swimming;
     - rendering;
     - a seam with a *parked* neighbour (the pending-volume ledger at the manager);
     - client prediction across a seam.

- [ ] **Step 4: Update both synthesis pointers and commit the docs.**
  - Under "What to build, in order" in the 2026-11-06 synthesis, add
    `**Prototype results:** see [03-prototype-results.md](03-prototype-results.md), gate: <PASS|FAIL + one line>.`
  - Add the same line, linking `../2026-11-06-water-revisit/03-prototype-results.md`, under
    "User decisions" in the 2026-09-22 synthesis.
  - Write the actual verdict in place of the angle-bracket part.

```bash
git add docs/superpowers/specs/2026-11-06-water-revisit/ \
  docs/superpowers/specs/2026-09-22-voxel-water-efficiency/ \
  docs/superpowers/plans/2026-09-22-water-prototype.md
git commit -m "docs(water): prototype results and decision gate

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Check `git status` first. Those docs are all this work's own files. Add nothing else.

- [ ] **Step 5: Final report to the user.** Include:
  - the gate verdict and the two or three headline ratios;
  - the seam gain that held;
  - the brick-journal verdict;
  - the path of the results doc;
  - whether `justfile` was committed or left uncommitted (see Global Constraints).
