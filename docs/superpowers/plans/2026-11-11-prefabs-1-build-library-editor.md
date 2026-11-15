# Prefabs 1 — Build prefabs, library and editor: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prefabs exist as data with files and export strings; Build prefabs can be stamped at runtime and into a worldgen seed by one shared layout; players register prefabs per player; an in-game editor on an empty world opens, edits and saves them.

**Architecture:** A new engine-agnostic module `game::prefab` holds the file form (names), the resolved form (table ids), codecs, the built-in set and the layout routine. The sim gains a per-`Client` table of registered prefabs and a `GameEventKind::Prefab` command (register / deregister / stamp). The server gains a third keyword world (`prefab-editor`); the client gains a debug preset, a library screen and an editor session that reuses the existing stud build mode.

**Tech Stack:** Rust (nightly pinned), `ron` 0.12, `bincode`, `lz4_flex`, `base64` 0.22, `crc32fast`, `state-hash`, Bevy 0.19.1 (client only), Slint (`crates/slint-ui`), `arboard` (client, native only).

**Spec:** `docs/superpowers/specs/2026-11-11-prefab-system-design.md` (steps 1–4 of its build order). Plan 2 (`2026-11-11-prefabs-2-props.md`) covers steps 5–9.

## Global Constraints

- Work on `develop` in this checkout. **No git worktrees.**
- **Tasks 5–12 edit files that carry someone's uncommitted work** (`crates/game/src/{orders,intents,stud_ops,state,protocol}.rs` and tests). Do not start Task 5 until `git status --short crates/game` is clean. Tasks 1–4 only add files (plus `lib.rs` / `it.rs` / `Cargo.toml` lines) and may start now.
- `game`, `server`, `worldgen` stay engine-agnostic: no Bevy there.
- Sim state: `BTreeMap` only, no `HashMap`; no inherent `f32` transcendentals (use `libm`); hashing via `StateHash`, never std `Hash`; tunables are named `pub const`s.
- Rollback bar: `hash(before) == hash(after undo)`, bit-exact, for every new `_safe` mutation.
- Never write a region edge as a literal — derive from `REGION_BLOCKS` / `REGION_SIZE`.
- Wire data goes through `game::encode_packet` / `decode_packet`.
- UI is Slint only. In widgets never bind `visible` / `opacity` on a component root. Every root component calls `GameTheme.apply()` in `init`.
- Any new per-world client resource, `Local` or entity root is session-registered (`add_session_reset`, `SessionLocal`, `SessionScoped`).
- Tests: only `just test-one <crate> "<args>"` for touched tests, `just smoke` as the sanity check. Game integration tests are modules of the single `it` binary (`crates/game/tests/it.rs`). Tag tests slower than 100 ms `#[ignore = "slow: >100ms — run via just test-full"]`. **Client tests are opt-in: ask the user before running `just test-one client …`.**
- Keep the `justfile` in sync with any new command.
- Caps (spec values): `PREFAB_MAX_PARTS = 4096`, `PREFAB_MAX_BOXES = 4096`, Build cells within x,z `-48..48` and y `-90..180` of the anchor (96 x 270 x 96), `MAX_REGISTERED_PREFABS = 8`, `MAX_REGISTERED_ELEMENTS = 8192`, export prefix `MMOPF1:`.

### Deviations from the spec, decided while planning

1. **No new batched intent.** `Intent` is `Copy` and stored in rollback maps, so it cannot carry a brick list. The part of a stamp that falls in a neighbour region is sent as the existing `Intent::CarveStud` (per box) and `Intent::PlaceStud` (per brick).
2. **Register / deregister / stamp are not `Order` variants** (`Order` is `Copy`). They are a new `GameEventKind::Prefab(ClientId, PrefabCommand)`.
3. **The extent cap is expressed around the anchor** (±48 studs) so a prefab always fits inside the editor region at the smallest region size (64 blocks = 128 studs).
4. **Registered prefabs are saved with the player record** on forged worlds, because `Client` is. Nothing relies on it.

## Review Focus

1. **A stamp whose anchor puts a slope across a chunk edge** — expect the slope skipped and counted, every other brick laid, no panic. (Task 4)
2. **A register command built by hand, bypassing the client's resolver** (out-of-range part index, `data != 0`, 5000 parts) — expect it refused server-side with state unchanged. (Task 6)
3. **An export string that is truncated, not base64, or claims a huge decompressed size** — expect a `PrefabError`, bounded memory, no panic. (Task 2)
4. **Saving in the editor when a chunk under the prefab has not loaded on the client yet** — expect Save refused with a message, not a file missing those bricks. (Task 11)
5. **A library folder holding a file that is not a prefab, or two files resolving to the same name** — expect the list to show the good ones and report the bad file, not fail to open. (Task 8)

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/game/src/prefab/mod.rs` | Module root, re-exports, caps, `PrefabKey`, `prefab_name_key` |
| `crates/game/src/prefab/file.rs` | File-form types (serde), name rules |
| `crates/game/src/prefab/resolve.rs` | Resolved-form types, `resolve`, `unresolve`, `validate`, `Problem` |
| `crates/game/src/prefab/codec.rs` | RON read / write, export strings |
| `crates/game/src/prefab/builtin.rs` | Embedded set, disk override, `install_prefabs` / `prefabs()` |
| `crates/game/src/prefab/layout.rs` | `layout_build`, `ChunkStamp`, turning |
| `crates/game/src/prefab/seed.rs` | `stamp_build_into_seed` |
| `crates/game/src/prefab/capture.rs` | World bricks → `BuildFile` (editor save), `merge_boxes` |
| `crates/game/src/prefab/library.rs` | A prefab folder on disk (native only) |
| `crates/game/src/prefab/command.rs` | `PrefabCommand`, `RegisteredPrefab`, the `_safe` sim mutations |
| `crates/game/build.rs` | Emits the embedded prefab list from `assets/prefabs/` |
| `crates/game/tests/prefab_*.rs` | One test module per task, declared in `tests/it.rs` |
| `assets/prefabs/dev-arch.prefab.ron` | A small sample Build prefab (test fixture and editor smoke) |
| `crates/worldgen/src/prefab_world.rs` | The editor world's genesis |
| `crates/client/src/prefab_editor/{mod,session,save,library_ui}.rs` | Editor session, save, library screen glue |
| `crates/slint-ui/ui/prefab-library.slint`, `prefab-editor.slint` | Library screen, in-editor panel |

---

### Task 1: Prefab types, resolve and validate

**Files:**
- Create: `crates/game/src/prefab/{mod,file,resolve}.rs`
- Modify: `crates/game/src/lib.rs` (add `pub mod prefab; pub use prefab::*;` beside the other glob re-exports)
- Test: `crates/game/tests/prefab_resolve.rs` (+ `mod prefab_resolve;` in `tests/it.rs`)

**Interfaces:**
- Consumes: `STUD_PARTS`, `StudPartDef`, `stud_material_id(name) -> Option<u16>`, `stud_material(id)`, `STUD_COLORS`, `NATURAL_BASE`.
- Produces (file form, all `Debug, Clone, PartialEq, Serialize, Deserialize`):

```rust
pub const PREFAB_FORMAT: u16 = 1;
pub struct PrefabFile { pub format: u16, pub name: String, pub meta: PrefabMeta, pub kind: PrefabKindFile }
pub struct PrefabMeta { pub author: String, pub note: String, pub source: String }   // Default
pub enum PrefabKindFile { Build(BuildFile), Prop(PropFile) }
pub struct BuildFile { pub parts: Vec<BuildPartFile>, pub boxes: Vec<BuildBoxFile>, pub carve: Vec<CellBox>, pub markers: Vec<Marker> }
pub struct BuildPartFile { pub min: [i32; 3], pub part: String, pub rot: u8, pub material: MaterialName, pub data: u16 }
pub struct BuildBoxFile { pub min: [i32; 3], pub size: [u32; 3], pub material: MaterialName, pub data: u16 }
pub struct CellBox { pub min: [i32; 3], pub size: [u32; 3] }
pub enum MaterialName { Plastic(u16), Natural(String) }        // Plastic = LDraw colour code
pub struct Marker { pub at: [i32; 3], pub kind: String, pub turn: u8 }
pub struct PropFile { pub parts: Vec<PropPartFile>, pub slots: Vec<SlotDef>, pub foot: [u8; 2], pub markers: Vec<Marker> }
pub struct PropPartFile { pub part: String, pub pos: [i32; 3], pub rot: [f32; 4], pub colour: PropColour, pub solid: bool }
pub enum PropColour { Fixed(u16), Slot(u8) }
pub struct SlotDef { pub name: String, pub default: u16 }
pub fn prefab_name_ok(name: &str) -> bool      // ^[a-z0-9][a-z0-9-]{0,47}$
```

- Produces (resolved form, `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, StateHash`):

```rust
pub struct ResolvedPrefab { pub name: String, pub kind: ResolvedKind }
pub enum ResolvedKind { Build(ResolvedBuild) }                  // Plan 2 adds Prop
pub struct ResolvedBuild { pub parts: Vec<BuildPart>, pub boxes: Vec<BuildBox>, pub carve: Vec<CellBox>, pub markers: Vec<Marker> }
pub struct BuildPart { pub min: [i32; 3], pub part: u16, pub rot: u8, pub material: u16, pub data: u16 }
pub struct BuildBox { pub min: [i32; 3], pub size: [u32; 3], pub material: u16, pub data: u16 }
pub struct PrefabKey(pub u32);                                  // Copy, Ord
pub enum Problem { BadName, BadFormat(u16), UnknownPart(String), UnknownMaterial(String), BadRotation(usize),
                   OutOfBounds(usize), TooManyParts(usize), TooManyBoxes(usize), EmptyBox(usize), PropsNotSupported }
pub const PREFAB_MAX_PARTS: usize = 4096;
pub const PREFAB_MAX_BOXES: usize = 4096;
pub const PREFAB_HALF_STUDS: i32 = 48;
pub const PREFAB_MIN_PLATE: i32 = -90;
pub const PREFAB_MAX_PLATE: i32 = 180;
pub enum Trust { Builtin, Player }
pub fn resolve(file: &PrefabFile, trust: Trust) -> Result<ResolvedPrefab, Vec<Problem>>
pub fn unresolve(p: &ResolvedPrefab, meta: PrefabMeta) -> PrefabFile
impl ResolvedPrefab { pub fn validate(&self, trust: Trust) -> Result<(), Vec<Problem>>; pub fn key(&self) -> PrefabKey; pub fn element_count(&self) -> usize }
pub fn prefab_name_key(name: &str) -> u32                       // crc32fast of the name bytes
```

`validate` is what the server runs on a hand-built resolved prefab: every check `resolve` makes, on ids instead of names. `Trust::Player` requires every `data == 0` (and `resolve` with `Trust::Player` forces it to 0 rather than failing). Bounds: every cell of every part (rotated footprint), box and carve box lies in `-48..48` x,z and `-90..180` y. `key()` is `state_hash(self)`. `element_count` = parts + boxes.

- [ ] **Step 1: Write the failing tests** in `prefab_resolve.rs`:

```rust
fn arch() -> PrefabFile { /* 3001 at [0,0,0], 3001 at [0,3,0] rot 1, box stone 4x3x2 at [-4,0,0], carve [0,0,0] 1x1x1 */ }
#[test] fn resolve_maps_names_to_ids() { let r = resolve(&arch(), Trust::Builtin).unwrap();
    let ResolvedKind::Build(b) = &r.kind; assert_eq!(STUD_PARTS[b.parts[0].part as usize].number, "3001");
    assert_eq!(b.boxes[0].material, stud_material_id("stone").unwrap()); }
#[test] fn unresolve_round_trips() { let f = arch(); let r = resolve(&f, Trust::Builtin).unwrap();
    assert_eq!(unresolve(&r, f.meta.clone()), f); }
#[test] fn problems_are_listed_not_panicked() { /* unknown part "99999", material Natural("nope"), rot 7, part at x=48,
    name "Bad Name" */ let e = resolve(&bad, Trust::Builtin).unwrap_err();
    assert!(e.contains(&Problem::UnknownPart("99999".into()))); assert!(e.contains(&Problem::UnknownMaterial("nope".into())));
    assert!(e.contains(&Problem::BadRotation(1))); assert!(e.contains(&Problem::OutOfBounds(2))); assert!(e.contains(&Problem::BadName)); }
#[test] fn caps() { /* 4097 parts */ assert!(matches!(err[..], [Problem::TooManyParts(4097)])); }
#[test] fn player_trust_zeroes_data() { /* part with data 7 */ assert_eq!(b.parts[0].data, 0); }
#[test] fn validate_refuses_hand_built() { /* ResolvedBuild with part: u16::MAX; data: 3 under Trust::Player; */ assert!(p.validate(Trust::Player).is_err()); }
#[test] fn key_is_content_addressed() { assert_eq!(a.key(), a.clone().key()); assert_ne!(a.key(), moved_one_cell.key()); }
#[test] fn prop_files_are_refused_for_now() { assert_eq!(resolve(&prop_file, Trust::Builtin), Err(vec![Problem::PropsNotSupported])); }
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_resolve::"` — expected: fails to compile (`prefab` not found).
- [ ] **Step 3:** Implement the three files and the `lib.rs` lines. Part lookup is `STUD_PARTS.iter().position(|p| p.number == n)`; `MaterialName::Plastic(c)` is valid only if `STUD_COLORS` contains `c`.
- [ ] **Step 4:** Run the same command — expected: 8 passed.
- [ ] **Step 5:** Commit: `feat(game): prefab file and resolved forms with validation`.

---

### Task 2: RON files and export strings

**Files:**
- Create: `crates/game/src/prefab/codec.rs`
- Modify: `crates/game/Cargo.toml` (add `base64 = { workspace = true }`)
- Create: `assets/prefabs/dev-arch.prefab.ron`
- Test: `crates/game/tests/prefab_codec.rs`

**Interfaces:**
- Consumes: Task 1 file form.
- Produces:

```rust
pub const EXPORT_PREFIX: &str = "MMOPF1:";
pub const EXPORT_MAX_CHARS: usize = 1 << 20;        // refuse longer strings before decoding
pub const EXPORT_MAX_BYTES: usize = 4 << 20;        // refuse a larger declared decompressed size
pub enum PrefabError { Syntax(String), NotAPrefabString, TooLarge, Corrupt, Problems(Vec<Problem>), Io(String) }
pub fn prefab_to_ron(file: &PrefabFile) -> String                       // pretty, stable field order, trailing newline
pub fn prefab_from_ron(src: &str) -> Result<PrefabFile, PrefabError>
pub fn prefab_to_export(file: &PrefabFile) -> String                    // PREFIX + base64(lz4 size-prepended(bincode))
pub fn prefab_from_export(s: &str) -> Result<PrefabFile, PrefabError>   // trims whitespace; never panics
```

`prefab_from_export` checks, in order: prefix, char length, base64, the lz4 size header against `EXPORT_MAX_BYTES` **before** decompressing, bincode with a size limit.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn ron_round_trip_is_byte_stable() { let s = prefab_to_ron(&arch()); assert_eq!(prefab_to_ron(&prefab_from_ron(&s).unwrap()), s); }
#[test] fn sample_asset_parses_and_resolves() { let f = prefab_from_ron(include_str!("../../../assets/prefabs/dev-arch.prefab.ron")).unwrap();
    assert_eq!(f.name, "dev-arch"); resolve(&f, Trust::Builtin).unwrap(); }
#[test] fn export_round_trip() { let s = prefab_to_export(&arch()); assert!(s.starts_with("MMOPF1:")); assert_eq!(prefab_from_export(&format!("  {s}\n")).unwrap(), arch()); }
#[test] fn hostile_strings_are_refused() {
    assert!(matches!(prefab_from_export("hello"), Err(PrefabError::NotAPrefabString)));
    assert!(matches!(prefab_from_export("MMOPF1:!!!"), Err(PrefabError::Corrupt)));
    let s = prefab_to_export(&arch()); assert!(matches!(prefab_from_export(&s[..s.len() / 2]), Err(PrefabError::Corrupt)));
    /* base64 of an lz4 header declaring u32::MAX bytes */ assert!(matches!(r, Err(PrefabError::TooLarge)));
    assert!(matches!(prefab_from_export(&format!("MMOPF1:{}", "A".repeat(EXPORT_MAX_CHARS + 1))), Err(PrefabError::TooLarge))); }
#[test] fn ron_syntax_error_is_reported() { assert!(matches!(prefab_from_ron("(format: 1, nam"), Err(PrefabError::Syntax(_)))); }
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_codec::"` — expected: compile failure.
- [ ] **Step 3:** Implement `codec.rs`; write `dev-arch.prefab.ron` with `prefab_to_ron` output for the `arch()` fixture (two `3001` bricks as posts at x=0 and x=6, a `3008`-style lintel across, in plastic colour 4).
- [ ] **Step 4:** Run — expected: 5 passed.
- [ ] **Step 5:** Commit: `feat(game): prefab RON files and export strings`.

---

### Task 3: Built-in set

**Files:**
- Create: `crates/game/build.rs` (or extend it if one exists), `crates/game/src/prefab/builtin.rs`
- Modify: `crates/server/src/lib.rs` (next to `game::install_block_props`), `crates/client/src/blocks.rs` (`load_registry`)
- Test: `crates/game/tests/prefab_builtin.rs`

**Interfaces:**
- Produces:

```rust
pub struct PrefabSet { /* BTreeMap<u32 name key, Arc<ResolvedPrefab>> + names */ }
impl PrefabSet { pub fn from_sources<'a>(sources: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self, String>;
                 pub fn get(&self, name_key: u32) -> Option<&Arc<ResolvedPrefab>>; pub fn named(&self, name: &str) -> Option<&Arc<ResolvedPrefab>>;
                 pub fn names(&self) -> impl Iterator<Item = &str> }
pub fn embedded_prefab_sources() -> &'static [(&'static str, &'static str)]     // (file stem, RON), from build.rs
pub fn load_builtin_prefabs(disk_dir: Option<&std::path::Path>) -> Result<PrefabSet, String>
pub fn install_prefabs(set: PrefabSet)             // OnceLock; idempotent for an equal set, panics on a different one
pub fn prefabs() -> &'static PrefabSet             // empty set if nothing installed
pub const BUILTIN_PREFAB_DIR: &str = "assets/prefabs";
```

`build.rs` lists `../../assets/prefabs/*.prefab.ron` (sorted), emits `include_str!` pairs into `OUT_DIR/builtin_prefabs.rs`, and prints `cargo:rerun-if-changed` for the directory; a missing directory yields an empty list. `from_sources` fails on: a parse error, any `Problem` (resolved with `Trust::Builtin`), a file stem that differs from the prefab's `name`, or two names with the same `prefab_name_key`. `load_builtin_prefabs(Some(dir))` reads the directory when it exists (native), else falls back to the embedded sources — the `blocks.ron` pattern. Server and client call `install_prefabs(load_builtin_prefabs(...).expect("invalid built-in prefab"))` with the repo-root-relative directory on native and `None` on wasm.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn embedded_set_contains_the_sample() { let s = PrefabSet::from_sources(embedded_prefab_sources().iter().copied()).unwrap();
    assert!(s.named("dev-arch").is_some()); assert_eq!(s.get(prefab_name_key("dev-arch")).unwrap().name, "dev-arch"); }
#[test] fn stem_must_match_name() { assert!(PrefabSet::from_sources([("other", ARCH_RON)]).unwrap_err().contains("other")); }
#[test] fn bad_prefab_fails_the_set_with_its_name() { assert!(PrefabSet::from_sources([("x", "(nope")]).unwrap_err().contains("x")); }
#[test] fn disk_overrides_embedded() { /* tempdir with dev-arch.prefab.ron moved one cell */ assert_ne!(disk.named("dev-arch").unwrap().key(), embedded.named("dev-arch").unwrap().key()); }
#[test] fn no_name_key_collisions_in_assets() { /* from_sources on the embedded set succeeds */ }
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_builtin::"` — expected: compile failure.
- [ ] **Step 3:** Implement; wire the two install call sites.
- [ ] **Step 4:** Run — expected: 5 passed. Run `just smoke` — expected: passes.
- [ ] **Step 5:** Commit: `feat(game): built-in prefab set embedded and installed`.

---

### Task 4: Build layout

**Files:**
- Create: `crates/game/src/prefab/layout.rs`
- Modify: `crates/game/src/stud_carve.rs` only if no chunk-level carve exists (see Step 3)
- Test: `crates/game/tests/prefab_layout.rs`

**Interfaces:**
- Consumes: `ResolvedBuild`, `StudBrick::part_at`, `StudBrick::{size, cells, stud_cells, is_box}`, `fill_box(min, size, material, data, out)`, `StudChunk::place`, `WorldChunkPos { x: i64, y: i32, z: i64 }`, `STUD_CHUNK_STUDS` (32), `STUD_CHUNK_PLATES` (90).
- Produces:

```rust
pub struct ChunkStamp { pub carve: Vec<([u8; 3], [u8; 3])>, pub bricks: Vec<StudBrick> }   // carve = (min, size) in chunk cells
pub struct BuildLayout { pub chunks: BTreeMap<WorldChunkPos, ChunkStamp>, pub skipped_split: u32 }
pub fn turn_cell(c: [i32; 3], turn: u8) -> [i32; 3]                 // quarter turns about the anchor column, y unchanged
pub fn layout_build(p: &ResolvedBuild, anchor: [i64; 3], turn: u8) -> BuildLayout
impl ChunkStamp { pub fn apply(&self, chunk: &mut StudChunk) -> u32 }   // carve, then place; returns bricks skipped for overlap
```

`anchor` is a world cell (studs x, plates y, studs z). A world cell maps to `WorldChunkPos` by Euclidean division by 32 / 90 / 32. Algorithm for `layout_build`:

1. Each part: turn its footprint box (`min .. min + unrotated size`) with `turn_cell` on both corners, take the new min, brick rot = `(rot + turn) % 4`.
2. If the turned brick lies in one chunk, emit it. If it straddles and `is_box()`, emit `fill_box` over each chunk's slice of its cells (same material and data). If it straddles and is not a box, `skipped_split += 1`.
3. Each box: turn, split per chunk, `fill_box` each slice.
4. Each carve box: turn, split per chunk, push to `carve`.
5. Bricks within a chunk are sorted by `min` (`[u8;3]` order) so output is canonical.

**The turn direction must agree with `rotate_cell`** — Step 1's third test pins it; if it fails, flip `turn_cell`'s direction, not the brick rot.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn turn_cell_four_times_is_identity() { for t in 0..4 { /* compose */ } assert_eq!(c4, c); }
#[test] fn one_brick_lands_on_the_anchor() { let l = layout_build(&one_3001_at_origin(), [40, 95, 70], 0);
    let s = &l.chunks[&WorldChunkPos::new(1, 1, 2)]; assert_eq!(s.bricks[0].min, [8, 5, 6]); assert_eq!(l.chunks.len(), 1); }
#[test] fn turning_agrees_with_brick_rotation() { // slope 3040: studs on one side only
    let base = layout_build(&one_slope(), [16, 0, 16], 0); for t in 1..4 { let l = layout_build(&one_slope(), [16, 0, 16], t);
        assert_eq!(world_cells(&l), turned(world_cells(&base), [16, 0, 16], t)); assert_eq!(world_stud_cells(&l), turned(world_stud_cells(&base), [16, 0, 16], t)); } }
#[test] fn box_brick_across_a_chunk_edge_is_retiled() { let l = layout_build(&one_2x8_at_origin(), [28, 0, 0], 0);   // 3007 spans x 28..36
    assert_eq!(l.chunks.len(), 2); assert_eq!(l.skipped_split, 0); assert_eq!(world_cells(&l).len(), 2 * 8 * 3); }
#[test] fn slope_across_a_chunk_edge_is_skipped_and_counted() { let l = layout_build(&slope_plus_brick(), [31, 0, 0], 0);
    assert_eq!(l.skipped_split, 1); assert_eq!(total_bricks(&l), 1); }
#[test] fn negative_cells_go_to_lower_chunks() { let l = layout_build(&part_at([-1, -1, -1]), [0, 0, 0], 0);
    assert!(l.chunks.contains_key(&WorldChunkPos::new(-1, -1, -1))); }
#[test] fn boxes_are_tiled_per_chunk() { /* 40x3x2 stone box at x=-4 from anchor [30,0,0] */ assert_eq!(world_cells(&l).len(), 40 * 3 * 2); }
#[test] fn apply_carves_then_places_and_counts_overlaps() { let mut c = StudChunk::new([brick("3001", [0, 0, 0])]).unwrap();
    let s = ChunkStamp { carve: vec![([0, 0, 0], [2, 3, 4])], bricks: vec![brick("3001", [0, 0, 0]), brick("3001", [10, 0, 0]), brick("3001", [10, 0, 0])] };
    assert_eq!(s.apply(&mut c), 1); assert_eq!(c.len(), 2); }
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_layout::"` — expected: compile failure.
- [ ] **Step 3:** Implement. For `apply`'s carve, reuse the chunk-level routine `carve_stud_boxes_safe` uses for one chunk (it splits bricks that the box cuts, via `box_minus`); if that logic is private inside `stud_ops.rs`, lift it to `pub(crate) fn StudChunk::carve_box(&mut self, min: [u8;3], size: [u8;3])` in `stud_carve.rs` and call it from both places without changing behaviour.
- [ ] **Step 4:** Run — expected: 8 passed. If `carve_box` was lifted, also run `just test-one game "--test it stud_carve:: stud_ops::"` — expected: unchanged pass count.
- [ ] **Step 5:** Commit: `feat(game): build prefab layout shared by runtime and worldgen`.

---

> **Gate:** stop here until `git status --short crates/game` shows no uncommitted changes from the survival-placement work.

### Task 5: Stamping into a worldgen seed

**Files:**
- Create: `crates/game/src/prefab/seed.rs`
- Modify: `crates/game/src/stud_ground.rs` (`StudRegionSeed`, `feature_chunks`), every `StudRegionSeed { .. }` literal (find with `rg "StudRegionSeed \{" crates`; add `..Default::default()`), `crates/worldgen/src/lib.rs` (re-export)
- Test: `crates/game/tests/prefab_seed.rs`

**Interfaces:**
- Consumes: `layout_build`, `ChunkStamp::apply`, `WorldChunkPos::to_local(self) -> (RegionCoords, ChunkCoords)`.
- Produces:

```rust
// stud_ground.rs
pub struct StudRegionSeed { pub ground: …, pub features: …, pub stamps: Vec<(StudChunkCoords, ChunkStamp)> }
// feature_chunks: per chunk, start from pristine, apply each stamp for that chunk in order (ChunkStamp::apply), then the features.
// seed.rs
pub struct SeedStampReport { pub skipped_split: u32, pub chunks_in_region: u32 }
pub fn stamp_build_into_seed(seed: &mut StudRegionSeed, region: RegionCoords, prefab: &ResolvedBuild, anchor: [i64; 3], turn: u8) -> SeedStampReport
```

`stamp_build_into_seed` keeps only the layout chunks whose `to_local().0 == region`. `worldgen` re-exports it as `worldgen::stamp_build_into_seed`. `WORLD_MAP_VERSION` is not bumped (nothing calls this yet).

- [ ] **Step 1: Write the failing tests** (flat ground via `common::floor_seed` / `StudColumnGround::flat`):

```rust
#[test] fn stamp_appears_in_feature_chunks() { let mut seed = flat_seed(); stamp_build_into_seed(&mut seed, RC, &arch(), anchor_on_surface(), 0);
    let chunks = seed.feature_chunks(); assert!(has_brick_at_world(&chunks, anchor_on_surface())); }
#[test] fn carve_removes_ground_before_laying() { /* prefab with a carve box below the surface and a brick inside it */ assert!(brick_is_the_prefabs(...)); }
#[test] fn only_this_regions_chunks_are_kept() { /* anchor one stud inside the region's east edge, 2x8 brick pointing east */
    let a = stamp(RC); let b = stamp(RC.east()); assert!(a.chunks_in_region >= 1 && b.chunks_in_region >= 1);
    assert_eq!(cells(a_seed).len() + cells(b_seed).len(), 2 * 8 * 3); }
#[test] fn stamps_apply_before_features() { /* a feature brick overlapping a stamped brick is the one dropped */ }
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_seed::"` — expected: compile failure.
- [ ] **Step 3:** Implement; fix every seed literal.
- [ ] **Step 4:** Run — expected: 4 passed. Run `just test-one game "--test it stud_ground::"` and `just smoke` — expected: pass.
- [ ] **Step 5:** Commit: `feat(game,worldgen): stamp build prefabs into a region seed`.

---

### Task 6: Per-player registration

**Files:**
- Create: `crates/game/src/prefab/command.rs`
- Modify: `crates/game/src/state.rs` (`Client`), `crates/game/src/migrate.rs` (`Client::arrive`), `crates/game/src/protocol.rs` (`GameEventKind`), `crates/game/src/region.rs` (`Region::handle_event`), the player-record codec (see Step 3)
- Test: `crates/game/tests/prefab_register.rs`

**Interfaces:**
- Produces:

```rust
pub const MAX_REGISTERED_PREFABS: usize = 8;
pub const MAX_REGISTERED_ELEMENTS: usize = 8192;
pub struct RegisteredPrefab { pub key: PrefabKey, pub def: Arc<ResolvedPrefab> }   // StateHash hashes `key` only; serde writes the def inline
pub enum PrefabRef { Registered(PrefabKey), Builtin(u32) }                          // Copy
pub enum PrefabCommand { Register(Box<ResolvedPrefab>), Deregister(PrefabKey), Stamp { prefab: PrefabRef, anchor: [i64; 3], turn: u8 } }
// state.rs
pub struct Client { …, pub prefabs: BTreeMap<PrefabKey, RegisteredPrefab> }
// protocol.rs
GameEventKind::Prefab(ClientId, crate::PrefabCommand)
// impl Rollback
pub fn register_prefab_safe(&mut self, client: ClientId, prefab: ResolvedPrefab) -> bool
pub fn deregister_prefab_safe(&mut self, client: ClientId, key: PrefabKey) -> bool
```

`register_prefab_safe` returns `false` with state untouched when: the client is unknown, `prefab.validate(Trust::Player)` fails, the count would exceed 8, or the client's total `element_count` would exceed 8192. Registering a key already present returns `true` and changes nothing. Mutate `Client` the way existing `_safe` client mutations do (follow how `inventory` / `selected` are changed under rollback). `Client::arrive` lists `prefabs` as **travelling**.

`GameEventKind::Prefab` must be handled at **every** site that handles `GameEventKind::Order` (find with `rg "GameEventKind::Order" crates`): region dispatch, the client→server send path and its server-side accept list, client prediction. `Region::handle_event` routes `Register` / `Deregister` here and `Stamp` to Task 7 (until then: returns `false`).

- [ ] **Step 1: Write the failing tests** (region built as in `tests/stud_orders.rs`; `r.handle_event(GameEventKind::CreateClient(0))` first):

```rust
#[test] fn register_then_undo_restores_the_hash() { let before = hash(&r); prefab(&mut r, PrefabCommand::Register(Box::new(arch())));
    assert!(r.data().clients[&0].prefabs.contains_key(&arch().key())); assert_ne!(before, hash(&r)); r.rollback_last_event(); assert_eq!(before, hash(&r)); }
#[test] fn deregister_then_undo_restores_the_hash() { /* same shape */ }
#[test] fn ninth_prefab_is_refused() { /* 8 distinct, then a 9th */ assert_eq!(r.data().clients[&0].prefabs.len(), 8); assert_eq!(h8, hash(&r)); }
#[test] fn element_budget_is_enforced() { /* 4096 + 4096 parts ok, +1 more refused */ }
#[test] fn hand_built_invalid_prefab_is_refused() { for bad in [part_index_out_of_range(), data_nonzero(), parts_5000(), cell_at_x_48()] {
    let h = hash(&r); prefab(&mut r, PrefabCommand::Register(Box::new(bad))); assert_eq!(h, hash(&r)); } }
#[test] fn prefabs_travel_on_arrive() { let c = r.data().clients[&0].clone().arrive(); assert_eq!(c.prefabs.len(), 1); }
#[test] fn registered_prefabs_survive_a_snapshot_round_trip() { /* encode_packet(r.data()) → decode → state_hash equal */ }
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_register::"` — expected: compile failure.
- [ ] **Step 3:** Implement. `Client` is stored in `PlayerRecord` in the world store: find how the last `Client` field addition kept old records readable (`git log -S open_container -- crates/game/src/state.rs crates/world-store crates/server/src/player_store.rs`) and do the same for `prefabs` (old records load with an empty map). Add a test beside the existing player-record codec tests that an old-format record still decodes.
- [ ] **Step 4:** Run — expected: 7 passed, plus the codec test. Run `just test-one game "--test it migrate::"` and `just smoke`.
- [ ] **Step 5:** Run `just determinism-wasm` (a hashed, serialized type changed) — expected: pass.
- [ ] **Step 6:** Commit: `feat(game): per-player prefab registration`.

---

### Task 7: Runtime stamp

**Files:**
- Modify: `crates/game/src/prefab/command.rs`, `crates/game/src/stud_ops.rs`, `crates/game/src/region.rs`
- Test: `crates/game/tests/prefab_stamp.rs`; seam cases in `crates/game/tests/handoff_schedule.rs`

**Interfaces:**
- Consumes: `layout_build`, `ChunkStamp`, `Rollback::stud_chunk_or_pristine(c) -> Option<Cow<StudChunk>>`, `stud_key_at`, `set_stud_chunk_safe(key, chunk)`, `create_stud_chunk_safe(c, chunk)`, `stud_brick_overlaps_body(chunk: [i32;3], brick) -> bool`, `send_intent_safe(target, intent, out)`, `Target::Stud(WorldChunkPos)`, `Intent::CarveStud { lo: [i64;3], hi: [i64;3], by }`, `Intent::PlaceStud { chunk, brick, by }`, `stud_edit_source(client) -> EntityId`, `prefabs()`.
- Produces:

```rust
pub struct StampOutcome { pub placed: u32, pub skipped_overlap: u32, pub skipped_split: u32, pub sent_to_neighbours: u32 }
// impl Rollback
pub fn apply_chunk_stamp_safe(&mut self, c: StudChunkCoords, stamp: &ChunkStamp) -> (u32, u32)     // (placed, skipped); ONE chunk write
pub(crate) fn stamp_prefab_order_safe(&mut self, client: ClientId, prefab: PrefabRef, anchor: [i64; 3], turn: u8, out: &mut Vec<IntentMsg>) -> Option<StampOutcome>
```

`stamp_prefab_order_safe` returns `None` (nothing changed) when the reference does not resolve (`Registered` key not in this client's table; `Builtin` key not in `prefabs()`) or `turn > 3`. Otherwise it runs `layout_build` and, per layout chunk:

- **owned by this region:** drop bricks for which `stud_brick_overlaps_body` is true (count as `skipped_overlap`), then `apply_chunk_stamp_safe`: start from `stud_chunk_or_pristine` (or an empty chunk), `ChunkStamp::apply`, then `set_stud_chunk_safe` / `create_stud_chunk_safe` — exactly one `SetStudChunk` / `AddStudChunk` update per chunk. Illegal layers (`!stud_layer_ok`) skip the whole chunk.
- **owned by a neighbour:** one `Intent::CarveStud` per carve box (world cells) then one `Intent::PlaceStud` per brick, via `send_intent_safe` with `Target::Stud(chunk)`; count into `sent_to_neighbours`. No reply is awaited.

The stamp is developer-grade (spec): no cost, no gate beyond `CarveStud` / `FillStud`'s.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test] fn stamp_lays_the_prefab_and_undo_restores_the_hash() { /* register arch, stamp at a surface cell */ assert!(brick_at_world(&r, anchor).is_some());
    r.rollback_last_event(); assert_eq!(before, hash(&r)); }
#[test] fn one_update_per_chunk() { /* arch across two chunks */ assert_eq!(drain(&rx).len(), 2); }
#[test] fn overlapping_bricks_are_skipped_not_fatal() { /* stamp twice at the same anchor */ assert_eq!(second.placed, 0); assert_eq!(second.skipped_overlap, first.placed); }
#[test] fn unregistered_key_changes_nothing() { let h = hash(&r); prefab(&mut r, PrefabCommand::Stamp { prefab: PrefabRef::Registered(PrefabKey(1)), anchor, turn: 0 }); assert_eq!(h, hash(&r)); }
#[test] fn another_players_registration_is_not_usable() { /* client 1 stamps client 0's key */ assert_eq!(h, hash(&r)); }
#[test] fn builtin_ref_stamps_dev_arch() { common::install_test_prefabs(); /* PrefabRef::Builtin(prefab_name_key("dev-arch")) */ }
#[test] fn brick_under_a_body_is_skipped() { /* Order::DevDropBody at the anchor tile first */ assert!(o.skipped_overlap >= 1); }
#[test] fn runtime_and_seed_paths_lay_identical_bricks() { for (anchor, turn) in [(mid_chunk, 0), (on_chunk_edge, 1), (below_surface_with_carve, 3)] {
    let a = chunks_after_runtime_stamp(anchor, turn); let b = { let mut s = flat_seed(); stamp_build_into_seed(&mut s, RC, &p, anchor, turn); s.feature_chunks() };
    assert_eq!(a, b); } }
```

Seam cases (seeded layer, `Rig`): `prefab_stamp_across_a_seam_lays_both_sides` — register, stamp with the anchor two studs inside the seam so the lintel crosses, `rig.settle()`, assert the union of both regions' bricks equals the single-region result and `rig.fingerprint()` is stable across schedules; `prefab_stamp_into_a_sleeping_neighbour_is_partial_and_does_not_wake_it` — assert the neighbour is still not running and this side's bricks are laid.

- [ ] **Step 2:** Run `just test-one game "--test it prefab_stamp::"` — expected: compile failure.
- [ ] **Step 3:** Implement; add `common::install_test_prefabs()` (installs the embedded set) beside `install_real_block_props`.
- [ ] **Step 4:** Run — expected: 8 passed. Run `just test-one game "--test it handoff_schedule::prefab_"` — expected: 2 passed.
- [ ] **Step 5:** Run `just test-threads` — expected: pass.
- [ ] **Step 6:** Commit: `feat(game): stamp build prefabs at runtime`.

---

### Task 8: Prefab folders on disk

**Files:**
- Create: `crates/game/src/prefab/library.rs` (`#[cfg(not(target_arch = "wasm32"))]`)
- Test: `crates/game/tests/prefab_library.rs`

**Interfaces:**
- Produces:

```rust
pub struct PrefabDir { /* root: PathBuf */ }
pub struct PrefabEntry { pub name: String, pub kind: PrefabKindTag, pub elements: usize, pub size: [u32; 3] }
pub enum PrefabKindTag { Build, Prop }
pub struct PrefabListing { pub entries: Vec<PrefabEntry>, pub bad: Vec<(String /*file name*/, String /*why*/)> }
impl PrefabDir {
    pub fn new(root: impl Into<PathBuf>) -> Self;
    pub fn list(&self) -> PrefabListing;                               // missing folder = empty listing
    pub fn load(&self, name: &str) -> Result<PrefabFile, PrefabError>;
    pub fn save(&self, file: &PrefabFile) -> Result<(), PrefabError>;  // creates the folder; writes a temp file then renames
    pub fn delete(&self, name: &str) -> Result<(), PrefabError>;
    pub fn duplicate(&self, name: &str) -> Result<String, PrefabError>;// "<name>-copy", "<name>-copy-2", …
    pub fn import(&self, export: &str) -> Result<String, PrefabError>; // decode, resolve(Trust::Player) to check, unique name, save
    pub fn export(&self, name: &str) -> Result<String, PrefabError>;
}
pub const PREFAB_FILE_SUFFIX: &str = ".prefab.ron";
```

File name is `<name>.prefab.ron`; `save` refuses a name failing `prefab_name_ok`. `list` puts in `bad` any `*.prefab.ron` that fails to parse or whose stem differs from its `name`; entries are sorted by name. Prop files are listed (kind `Prop`) without resolving.

- [ ] **Step 1: Write the failing tests** (std temp dir under `std::env::temp_dir()` with a unique suffix; remove at the end):

```rust
#[test] fn save_list_load_round_trip() { d.save(&arch()).unwrap(); let l = d.list(); assert_eq!(l.entries[0].name, "dev-arch"); assert_eq!(d.load("dev-arch").unwrap(), arch()); }
#[test] fn missing_folder_lists_empty() { assert!(PrefabDir::new(tmp.join("nope")).list().entries.is_empty()); }
#[test] fn junk_and_mismatched_files_are_reported_not_fatal() { /* write "x.prefab.ron" = "junk"; copy arch as "other.prefab.ron"; save arch */
    let l = d.list(); assert_eq!(l.entries.len(), 1); assert_eq!(l.bad.len(), 2); }
#[test] fn duplicate_picks_free_names() { assert_eq!(d.duplicate("dev-arch").unwrap(), "dev-arch-copy"); assert_eq!(d.duplicate("dev-arch").unwrap(), "dev-arch-copy-2"); }
#[test] fn import_export_round_trip_and_name_clash() { let s = d.export("dev-arch").unwrap(); assert_eq!(d.import(&s).unwrap(), "dev-arch-2"); }
#[test] fn import_refuses_an_invalid_prefab() { /* export of a file with an unknown part */ assert!(matches!(d.import(&s), Err(PrefabError::Problems(_)))); assert_eq!(d.list().entries.len(), before); }
#[test] fn delete_removes_the_file() { d.delete("dev-arch").unwrap(); assert!(d.list().entries.is_empty()); }
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_library::"` — expected: compile failure.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run — expected: 7 passed.
- [ ] **Step 5:** Commit: `feat(game): prefab folders on disk with import and export`.

---

### Task 9: The editor world

**Files:**
- Create: `crates/worldgen/src/prefab_world.rs` (+ `pub mod prefab_world;` in `worldgen/src/lib.rs`)
- Modify: `crates/server/src/world_file.rs` (`WorldSource`, `resolve`, `open_world`, tests), `crates/server/src/lib.rs` (the `toy_studs` block, the log line), `crates/client/src/local_host.rs` (`server_log_path` keyword case)
- Test: `crates/worldgen/tests` (follow the crate's existing `stud_world` test location), `world_file.rs` unit tests

**Interfaces:**
- Produces:

```rust
// worldgen::prefab_world
pub const EDITOR_WORLD_RADIUS: i32 = 1;
pub const EDITOR_SURFACE_PLATE: i32 = 90;                 // top of chunk layer 0; 90 plates of ground below, 180 of air in reach above
pub fn region_seed(rc: RegionCoords) -> StudRegionSeed   // flat inside the radius, empty outside; no features
pub fn editor_anchor() -> [i64; 3]                        // centre of region (0,0) in studs (from REGION_BLOCKS), y = EDITOR_SURFACE_PLATE
// server::world_file
pub const PREFAB_EDITOR_KEYWORD: &str = "prefab-editor";
WorldSource::PrefabEditor
```

Ground: `StudColumnGround::flat(EDITOR_SURFACE_PLATE, PROFILE-like single material, rock, bedrock)` using one plain natural material so the baseplate reads as a baseplate. `WorldSource::PrefabEditor` behaves like `Studs` at every match site (no store, no map, `NullLodBuilder`, `with_genesis(Arc::new(worldgen::prefab_world::region_seed))`).

- [ ] **Step 1: Write the failing tests:**

```rust
// worldgen
#[test] fn editor_ground_is_flat_everywhere_inside() { let s = region_seed(RegionCoords::new(0, 0)); assert!(s.features.is_empty());
    for (_, g) in &s.ground { for (x, z) in [(0, 0), (31, 31)] { assert_eq!(g.surface(x, z), EDITOR_SURFACE_PLATE); } } }
#[test] fn outside_the_radius_is_empty() { assert_eq!(region_seed(RegionCoords::new(2, 0)), StudRegionSeed::default()); }
#[test] fn anchor_leaves_the_extent_cap_inside_region_zero() { let a = editor_anchor(); let studs = (game::REGION_BLOCKS * 2) as i64;
    assert!(a[0] - 48 >= 0 && a[0] + 48 <= studs && a[2] - 48 >= 0 && a[2] + 48 <= studs); }
// server world_file tests
#[test] fn prefab_editor_keyword_resolves() { assert_eq!(resolve(Some("prefab-editor"), None), Ok(WorldSource::PrefabEditor)); }
```

- [ ] **Step 2:** Run `just test-one worldgen "prefab_world"` and `just test-one server "--lib world_file"` — expected: compile failures.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run both — expected: pass. `just smoke` — expected: pass.
- [ ] **Step 5:** Commit: `feat(worldgen,server): the empty prefab-editor world`.

---

### Task 10: Launching the editor (debug preset, session, opening a prefab)

**Files:**
- Create: `crates/client/src/prefab_editor/{mod,session}.rs`
- Modify: `crates/client/src/debug_launch.rs` (`DebugScenario`, `parse_args`, `boot`, `USAGE`, the "treated as embarked" site), `crates/client/src/lib.rs` or `main.rs` (plugin registration), `justfile`
- Test: `debug_launch.rs` unit tests (opt-in client tests — ask before running)

**Interfaces:**
- Consumes: `PendingWorld { entry_dir, terrain, name }`, `AppState`, `StudBuild`, `GameEventKind::Prefab`, `PrefabDir`, `worldgen::prefab_world::editor_anchor`, `player_name::data_dir`.
- Produces:

```rust
DebugScenario::Prefab { game: bool, name: Option<String> }
// prefab_editor::session
pub enum PrefabSource { Library, Game }                      // Game = BUILTIN_PREFAB_DIR on disk; only when cfg!(feature = "dev_ui")
#[derive(Resource)] pub struct EditorRequest { pub source: PrefabSource, pub name: String, pub new_kind: Option<PrefabKindTag> }
#[derive(Resource)] pub struct EditorSession { pub source: PrefabSource, pub name: String, pub kind: PrefabKindTag,
    pub kept_carve: Vec<CellBox>, pub kept_markers: Vec<Marker>, pub meta: PrefabMeta, pub loaded: bool, pub dirty: bool, pub message: Option<String> }
pub fn prefab_dir(source: PrefabSource) -> Option<PrefabDir>     // Library: data_dir()/prefabs; Game: repo-root/assets/prefabs
pub fn start_editor(commands: &mut Commands, next: &mut NextState<AppState>, req: EditorRequest)   // inserts PendingWorld{terrain:"prefab-editor"} + the request
pub struct PrefabEditorPlugin;
```

CLI: `client debug prefab [--game] [<name>]` — `--game` is an error without `dev_ui`; no name means a new Build prefab named `untitled`. Systems (all gated on `EditorSession` existing, all session-reset):

- `enter_editor` (on entering `InWorld` with an `EditorRequest`): build `EditorSession`; force `StudBuild.on = true`; place the commander camera over `editor_anchor()`.
- `load_into_world`: once the chunks covering the prefab extent around the anchor are present in `StudWorld`, and if the file exists: resolve with `Trust::Builtin` for `Game` / `Trust::Player` for `Library`, send `PrefabCommand::Register` then `PrefabCommand::Stamp { prefab: Registered(key), anchor: editor_anchor(), turn: 0 }`, then `Deregister`; set `loaded = true`. A resolve failure sets `message` and leaves the world empty.
- `mark_dirty`: any place / remove order sent while in the editor sets `dirty`.
- `draw_editor_guides`: gizmo outlines (no entities to leak) for the anchor cell on the baseplate, the ±48-stud extent square, and every `kept_carve` box and `kept_markers` entry (marker = a small cross with its `kind` in the panel list), so the spec's "anchor is marked" and "carve boxes and markers are kept and shown" hold.

`just prefab-editor *args`: modelled on `run-studs` (`VOXEL_REGION_BLOCKS={{play_blocks}}`, `VOXEL_LOD_RINGS=off`, `VOXEL_NEAR_RADIUS=1`), runs `client debug prefab {{args}}`.

- [ ] **Step 1: Write the failing parse tests** beside the existing `parse_args` tests:

```rust
#[test] fn debug_prefab_parses() {
    assert_eq!(parse_args(&["debug", "prefab"]), Ok(Some(DebugScenario::Prefab { game: false, name: None })));
    assert_eq!(parse_args(&["debug", "prefab", "--game", "dev-arch"]), Ok(Some(DebugScenario::Prefab { game: true, name: Some("dev-arch".into()) })));
    assert!(parse_args(&["debug", "prefab", "Bad Name"]).is_err());
    assert!(parse_args(&["debug", "prefab", "--reset"]).is_err()); }
```

- [ ] **Step 2:** Implement the scenario, boot arm, session module, plugin and recipe.
- [ ] **Step 3:** `just smoke` — expected: pass. Ask the user, then `just test-one client "--lib debug_launch"` — expected: pass.
- [ ] **Step 4: Verify by running:** `just prefab-editor --game dev-arch` — expected: a flat baseplate with the arch standing at the centre, build mode on, bricks placeable and removable. Check the `AdapterInfo` log line names a real adapter.
- [ ] **Step 5:** Commit: `feat(client): prefab editor session and debug preset`.

---

### Task 11: Saving from the editor

**Files:**
- Create: `crates/game/src/prefab/capture.rs`, `crates/client/src/prefab_editor/save.rs`
- Modify: `crates/worldgen/src/stud_trees.rs` (use the moved `merge_boxes`)
- Test: `crates/game/tests/prefab_capture.rs`

**Interfaces:**
- Produces:

```rust
// game::prefab::capture
pub fn merge_boxes(cells: BTreeSet<[i32; 3]>) -> Vec<([i32; 3], [i32; 3])>     // moved verbatim from worldgen::stud_trees (now pub)
pub enum CaptureError { ChunkNotLoaded(WorldChunkPos), TooManyParts(usize), TooManyBoxes(usize), OutOfBounds }
pub fn capture_build(chunk: impl Fn(WorldChunkPos) -> Option<StudChunk>, pristine: impl Fn(WorldChunkPos) -> Option<StudChunk>,
                     anchor: [i64; 3], kept_carve: Vec<CellBox>, kept_markers: Vec<Marker>) -> Result<BuildFile, CaptureError>
// client
pub fn save_editor(...)   // system: on SaveRequested, capture → PrefabFile → PrefabDir::save; sets EditorSession.message / dirty
```

`capture_build` visits every chunk the extent cap can touch around the anchor. `chunk` returning `None` for a chunk whose `pristine` is `Some` is `ChunkNotLoaded` (Review Focus 4); both `None` means air. A brick is **the prefab's** when `pristine` has no identical brick at the same `min`. Plastic materials (`< NATURAL_BASE`) become `BuildPartFile`s exactly as placed (cells relative to the anchor, part by LDraw number, `data` kept). Natural-material bricks contribute their cells to a per-`(material, data)` cell set, merged with `merge_boxes` into `BuildBoxFile`s. Output order is canonical: parts by `(min.y, min.z, min.x)`, boxes by `(material, min)`. Removed baseplate bricks are ignored. The client passes `chunk` from `StudWorld::entity_at` + `SimStudChunk`, and `pristine` from `StudWorld::recipe(x, z)` + `pristine_chunk(y)`.

Save is triggered by **Ctrl+S** and by the panel's Save button (Task 12); it writes through `prefab_dir(session.source)`.

- [ ] **Step 1: Write the failing tests** (region on the editor ground, stamp through Task 7's order):

```rust
#[test] fn stamp_then_capture_gives_the_same_file() { let f = canonical(arch()); stamp(&mut r, &f, editor_anchor());
    let got = capture_build(chunk_of(&r), pristine_of(&r), editor_anchor(), f_carve, f_markers).unwrap();
    assert_eq!(prefab_to_ron(&with_build(&f, got)), prefab_to_ron(&f)); }                       // byte-identical
#[test] fn natural_bricks_come_back_as_boxes() { /* prefab with one 8x3x4 stone box */ assert_eq!(got.boxes.len(), 1); assert!(got.parts.is_empty()); }
#[test] fn baseplate_is_not_captured() { let got = capture_build(.. untouched world ..).unwrap(); assert!(got.parts.is_empty() && got.boxes.is_empty()); }
#[test] fn an_unloaded_chunk_refuses_the_save() { assert!(matches!(capture_build(|_| None, pristine_of(&r), ..), Err(CaptureError::ChunkNotLoaded(_)))); }
#[test] fn over_the_cap_is_refused_with_the_count() { /* 4097 1x1 plates */ assert!(matches!(e, CaptureError::TooManyParts(4097))); }
#[test] fn merge_boxes_is_a_fixed_point() { let b = merge_boxes(cells.clone()); assert_eq!(merge_boxes(cells_of(&b)), b); }
```

- [ ] **Step 2:** Run `just test-one game "--test it prefab_capture::"` — expected: compile failure.
- [ ] **Step 3:** Implement; move `merge_boxes` and point `stud_trees.rs` at it.
- [ ] **Step 4:** Run — expected: 6 passed. Run `just test-one worldgen "stud_trees"` — expected: unchanged.
- [ ] **Step 5: Verify by running:** `just prefab-editor --game dev-arch`, add a brick, Ctrl+S, `git diff assets/prefabs/dev-arch.prefab.ron` shows exactly one added part; restore the file. Open and save without edits: no diff.
- [ ] **Step 6:** Commit: `feat: save build prefabs from the editor`.

---

### Task 12: Library screen, editor panel, main-menu entry

**Files:**
- Create: `crates/slint-ui/ui/prefab-library.slint`, `crates/slint-ui/ui/prefab-editor.slint`, `crates/client/src/prefab_editor/library_ui.rs`
- Modify: `crates/slint-ui/ui/main-menu.slint`, `frontend.slint` (callback forwarding, new screen), `game.slint` (panel), `crates/client/src/frontend/mod.rs` (`FrontendAction`, `screen_index`, `connect_frontend`), `crates/client/src/session.rs` (`AppState`), `crates/client/Cargo.toml` (`arboard`, native target only)

**Interfaces:**
- Consumes: `PrefabDir`, `PrefabListing`, `start_editor`, `EditorSession`, `prefab_dir`.
- Produces:

```rust
AppState::PrefabLibrary                                   // screen_index: next free index after WORLDS_SCREEN
FrontendAction::{ PrefabLibrary, PrefabNew(PrefabKindTag), PrefabOpen { game: bool, name: String }, PrefabDuplicate(String),
                  PrefabDelete(String), PrefabImport, PrefabExport(String) }
pub enum EditorUiAction { Save, Exit, ConfirmDiscard, CancelDiscard }      // queue type as palette_ui::StudUiActions
```

Screens (Slint only, `fortress` theme, `GameTheme.apply()` in `init`, no `visible` on roots):

- **Main menu:** a **Prefab editor** button, native only (hidden on wasm as the world buttons are).
- **Library screen:** two lists — *My prefabs* and, when `cfg!(feature = "dev_ui")`, *Game prefabs*. Each row: name, kind, element count, size. Buttons: New build, New prop (disabled, tooltip "comes with prop prefabs"), Open, Duplicate, Delete (confirm), Import from clipboard, Export to clipboard, Back. A status line shows the last result and one line per `PrefabListing::bad` entry.
- **Editor panel** (in-world, shown only with an `EditorSession`): prefab name, kind, part and box counts, a dirty marker, `message`, **Save**, **Exit**. Exit with `dirty` shows a discard / cancel prompt, then leaves the session the way the existing return-to-menu path does.

Import reads the clipboard through `arboard` and calls `PrefabDir::import`; Export calls `PrefabDir::export` and writes the clipboard. A clipboard failure is shown in the status line.

- [ ] **Step 1:** Add the Slint files, callbacks and the Rust glue.
- [ ] **Step 2:** `just smoke` — expected: pass. `just slint-wasm` — expected: pass (the web build still compiles the generated UI).
- [ ] **Step 3: Verify by running** `just play`: Prefab editor → New build → place bricks → Save → Exit → the prefab is listed with its counts; Export, Delete, Import brings it back; Duplicate adds `-copy`; a junk file dropped into `~/.local/share/mmo/prefabs/` shows in the status line while the list still works; Exit with unsaved edits prompts.
- [ ] **Step 4: Session-leak check:** open a prefab, exit, start a normal world with `just play`: build mode is off, no editor panel, no leftover ghost.
- [ ] **Step 5:** Commit: `feat(client,ui): prefab library screen and editor panel`.

---

## Done when

- `just test-one game "--test it prefab_"` passes; `just test-threads`, `just determinism-wasm` and `just smoke` pass.
- `just prefab-editor --game dev-arch` opens, edits and saves the sample; open-then-save unchanged leaves the file byte-identical.
- From `just play`, a player can create, save, export, delete and re-import a Build prefab.
- Update `CLAUDE.md` (a short "Prefabs" entry under architecture: file locations, `Trust`, the stamp being developer-grade, `just prefab-editor`) and `TODO.md`.
