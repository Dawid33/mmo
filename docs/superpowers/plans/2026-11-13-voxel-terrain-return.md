# Voxel Terrain Return Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Voxel chunk terrain as it was at `0104a406` is the only terrain again, and everything that
landed after the stud range (role split, many hosts, world cuts, `SlotInbox`) runs on it.

**Architecture:** Rewind and replay. One `git revert --no-commit` of the 21-commit stud range
(`0104a406..cc728a58`) on `develop`, committed as a revert that does not build; then the later
commits' code is ported onto voxel types crate by crate, in dependency order, until the project's
verification bar passes. No dual-terrain switch, no world conversion.

**Tech Stack:** Rust (dated nightly), the `just` recipes, SQLite/Postgres via `crates/world-store`,
Bevy client, git.

**Spec:** `docs/superpowers/specs/2026-11-13-voxel-terrain-return-design.md` — read it first. It holds
the facts (dry-run conflict list, commit ranges) this plan argues from.

**Status:** approved by the user 2026-11-13 ("go subagent-driven"): R4 waits for this port, the
known-red list is accepted, the R3 go / no-go is **not** re-measured. Task 0 Steps 1–2 are still theirs.

## Global Constraints

- Work on `develop` in this checkout. **No git worktrees, no checkout of other commits.** Read old code
  with `git show <commit>:<path>`.
- Reference commits: `V` = `0104a406` (last voxel-only), `S` = `cc728a58` (last of the stud range),
  `P` = the `develop` tip the revert starts from (record its hash in Task 1).
- Conflict rule: where both sides changed a line, the post-range behaviour (what `S..P` added) wins and is
  rewritten against voxel types; stud-only content goes.
- A file that `S..P` did not touch must end **byte-equal to its content at `V`** (or absent, if absent
  at `V`). No clean-up, no improvement of voxel code on the way.
- Never run a whole game/server/client suite as a routine check. Use
  `just test-one <crate> "<cargo test args>"`; several filters need `--` first. `roles`,
  `coordinator_rt`, `hosts` need `--features test-hooks` (`just test-r2`, `just test-r3`,
  `just test-server` pass it). Client tests are a slow opt-in: **ask the user first**.
- Every store write stays fenced on the owner epoch; a cut part writes only rows of its own regions; no
  table two hosts' parts can both write.
- `BTreeMap` for ordered sim state, `libm` for transcendentals, hashing through `StateHash`.
- Never write a region edge as a literal; derive from `REGION_BLOCKS` / `REGION_SIZE`.
- Do not delete or `#[ignore]` a post-range test to get a crate green. A test that cannot be ported is
  a finding for the user.
- Do not re-record a physics golden.
- Commit only what the task names; never commit `deploy/**/*.tfvars` or `.env`. End commit messages
  with `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.
- Keep the justfile in sync with every changed build/run/test command, in the same commit.

## Review Focus

1. **A stud world store opened by the new build** — expected: refused at open with a message naming
   re-forge, before any migration runs, the file left as it was. Pinned in Task 2.
2. **A voxel world saved before the stud switch** (schema ≤ 0003, has regions) — expected: refused too,
   because migration `0005` would drop its chunks. Pinned in Task 2.
3. **A block broken across a host seam** (the breaker on host A, the block in host B's region) —
   expected: the intent is granted by B, B's chunk changes, A's halo shows it, no desync. Pinned in
   Task 6 (`server/tests/hosts.rs`, the port of the stud dig test).
4. **A neighbour's `BreakBlock` reply arriving in the same slot as an arrival** — expected: one
   canonical order on server and client. Pinned in Task 4 (`slot_order` tests).
5. **A world cut taken while a chunk is edited on two hosts** — expected: each part writes only its
   own regions' `chunks` rows; the fence refuses a stale epoch. Pinned in Task 3.

---

### Task 0: Clear the way (user)

Nothing in this plan starts before these are true.

- [ ] **Step 1:** The user commits or discards the uncommitted edits to
  `assets/studs/parts.bin`, `crates/client/src/renderer/stud/mesh.rs`, `crates/game/src/stud_assets.rs`,
  `tools/ldraw-bake/src/classify.rs` (the revert deletes these files).
- [ ] **Step 2:** The user commits the uncommitted `CLAUDE.md` and `justfile` edits (both conflict).
- [x] **Step 3:** Done 2026-11-13: the spec is approved and R4 waits for this port.
- [ ] **Step 4:** Check.

Run: `git status --porcelain -- CLAUDE.md justfile assets/studs crates/client/src/renderer/stud crates/game/src/stud_assets.rs tools/ldraw-bake`
Expected: no output.

### Task 1: The revert commit

**Files:** every file in `git diff --name-only 0104a406 cc728a58` (472 files).

**Interfaces:**
- Produces: a tree where every file untouched by `S..P` equals `V`; the store migrations `0004` and
  `0005` still present; no conflict markers. It does **not** build.

- [ ] **Step 1: Mark the starting tip** with a local tag (never pushed; deleted in Task 9).

Run: `git tag pre-voxel-return HEAD`
`P` below is this tag.

- [ ] **Step 2: Undo the range as ONE three-way merge**, not 21 chained reverts (a chained
  `git revert 0104a406..cc728a58` stops at every conflicting commit and resolves the same lines
  repeatedly). Base = `S`, ours = `P`, theirs = `V` — the merge the spec's dry run measured.

Run: `git read-tree -m -u cc728a58 HEAD 0104a406 && git merge-index -o git-merge-one-file -a`
Expected: `git merge-index` exits non-zero and reports the 26 conflicted paths of the spec; the working
tree holds conflict markers in 24 files. If `read-tree` refuses because an untracked file is in the
way, stop and tell the user which file — do not move or delete it.

Run: `git diff --name-only --diff-filter=U | wc -l`
Expected: `26`. A different number means `develop` moved since the dry run: re-run
`git merge-tree --write-tree --name-only --merge-base=cc728a58 HEAD 0104a406`, update the list in
Step 5, and go on.

- [ ] **Step 3: Resolve the two modify/delete conflicts as deletions.**

Run: `git rm crates/client/src/renderer/stud/mod.rs crates/world-store/tests/voxel_refusal.rs`

- [ ] **Step 4: Keep the two stud-era store migrations.**

Run: `git checkout cc728a58 -- crates/world-store/migrations/pg/0004_stud_terrain.sql crates/world-store/migrations/pg/0005_drop_voxel_terrain.sql crates/world-store/migrations/sqlite/0004_stud_terrain.sql crates/world-store/migrations/sqlite/0005_drop_voxel_terrain.sql`

- [ ] **Step 4b: Keep the parry determinism fix** (spec, "Fixes that rode inside the range"): the
  7-line hunk that builds the `Voxels` chunk BVH in chunk-id order.

Run: `git checkout pre-voxel-return -- crates/parry/src/shape/voxels/voxels.rs`
Then find the physics-next test that pins that hunk (`git log -p 0104a406..cc728a58 -- crates/physics-next/tests`,
the test naming BVH or chunk order) and keep that one test, moved into a file that exists at `V` if its
own file is stud-only (`stud_voxels.rs` goes otherwise). These paths join the Step 6 exception list.

- [ ] **Step 5: Resolve the 24 content conflicts by the conflict rule.** Text only — make each file
  parse as Rust / TOML / just; do not chase compile errors here. Files and hunk counts:
  `world-store/src/rows.rs` 5, `game/src/world_manager/mod.rs` 5, `CLAUDE.md` 4, `justfile` 3,
  `world-store/src/thread.rs` 3, `game/tests/common/rig.rs` 3, `world-store/tests/schema.rs` 2,
  `game/tests/halo.rs` 2, `client/src/instance.rs` 2, and one each in `TODO.md`,
  `world-store/tests/it.rs`, `world-store/src/{open,handle,codec}.rs`, `server/tests/it.rs`,
  `server/src/lib.rs`, `game/tests/{state_hash_check,packet_codec,handoff_schedule}.rs`,
  `game/tests/common/mod.rs`, `game/src/{protocol,lib}.rs`, `client/src/{local_server,harness}.rs`.
  For `CLAUDE.md` and `TODO.md` take the `P` side whole; Task 9 rewrites them.

- [ ] **Step 6: Verify the invariants.**

Run: `git diff --cached --check && ! git grep -n -E '^(<<<<<<<|>>>>>>>) ' -- . ':!docs'`
Expected: no output, exit 0.

Run: `git add -A -- crates assets tools justfile CLAUDE.md TODO.md Cargo.toml Cargo.lock && comm -23 <(git diff --cached --name-only 0104a406 | sort) <(git diff --name-only cc728a58 pre-voxel-return | sort)`
Expected: only the four migration files of Step 4. Any other path is a file that should equal `V` and
does not — fix it with `git checkout 0104a406 -- <path>` (or `git rm` if absent at `V`).

- [ ] **Step 7: Commit.**

```bash
git commit -m "revert: the stud terrain range 4a81dfd9..cc728a58 — voxel terrain returns (does not build)

Undoes 21 commits as one three-way merge onto P=<hash of pre-voxel-return>. The tree does not build until the port
commits that follow; git bisect must skip from here to the commit that
says 'tree builds again'. Store migrations 0004/0005 are kept on purpose.
Plan: docs/superpowers/plans/2026-11-13-voxel-terrain-return.md"
```

### Task 2: Store schema — migration 0011 and the flipped refusal

**Files:**
- Create: `crates/world-store/migrations/sqlite/0011_voxel_terrain_returns.sql`,
  `crates/world-store/migrations/pg/0011_voxel_terrain_returns.sql`
- Modify: `crates/world-store/src/open.rs` (`refuse_voxel_world` at `P` :268, `check_schema`,
  `newest_migration`), `crates/world-store/src/lib.rs` (`TerrainKind`)
- Create: `crates/world-store/tests/stud_refusal.rs` (a module of the `it` binary; add `mod` in
  `tests/it.rs`)
- Modify: `crates/world-store/tests/schema.rs`

**Interfaces:**
- Produces: `async fn refuse_unreturned_world(pool: &Pool, applied: Option<i64>) -> Result<(), StoreError>`
  in `open.rs`, called where `refuse_voxel_world` was, **before** migrations run.
  `const VOXEL_RETURN_MIGRATION: i64 = 11`. `TerrainKind::{Voxels = 0, Studs = 1}` unchanged.
  New worlds are written with `terrain = 0`.

- [ ] **Step 1: Write the failing tests** in `stud_refusal.rs`:
  - `a_stud_world_is_refused_before_any_migration` — build a store at migration 10 with
    `world.terrain = 1` and one `regions` row; open it writable; assert `Err`, the message contains
    `"re-forge"`, and `applied_migration` is still 10 and `stud_chunks` still exists.
  - `a_voxel_world_from_before_the_stud_switch_is_refused` — a store at migration 3 with one `regions`
    row and one `chunks` row; open; assert `Err` and the `chunks` row is still there.
  - `an_empty_store_migrates_to_11_and_holds_voxel_tables` — fresh store; open; assert applied = 11,
    `chunks` exists, `regions.ground` exists, `stud_chunks` does not.
  - `a_read_only_handle_needs_migration_11` — `OpenMode::ReadOnly` on a store at 10 is refused.
- [ ] **Step 2: Run** `just test-one world-store "--test it stud_refusal::"` — expected: FAIL (does
  not compile: `stud_refusal` tests reference the new behaviour; the crate itself may not build yet —
  then do Task 3 Step 1 first and return).
- [ ] **Step 3: Write `0011`** for both backends: `CREATE TABLE chunks` exactly as in
  `0001_schema_v1.sql` (`rx, rz, cx, cy, cz, data`, primary key all five coordinates);
  `ALTER TABLE regions ADD COLUMN ground` with the `0001` type and an empty-list default so existing
  rows satisfy `NOT NULL`; `DROP TABLE stud_chunks`; `ALTER TABLE regions DROP COLUMN stud_ground`.
  `world.terrain` stays. Head each file with the "keep in step with the other backend" comment the
  other migrations carry.
- [ ] **Step 4: Implement `refuse_unreturned_world`**: refuse when the store has at least one `regions`
  row and `applied < Some(11)` (covers both a stud world and an old voxel world), or when
  `applied >= 11` and `world.terrain == Studs`. Message names the world's kind and says to re-forge.
- [ ] **Step 5: Run** `just test-one world-store "--test it stud_refusal::"` and
  `just test-one world-store "--test it schema::"` — expected: PASS.
- [ ] **Step 6: Commit** `feat(world-store): migration 0011 brings the voxel tables back; unreturned worlds are refused`.

### Task 3: world-store port — voxel rows under today's rules

**Files:**
- Modify: `crates/world-store/src/{rows,codec,thread,handle,terrain,listen,two_phase,lib}.rs`
- Modify: `crates/world-store/tests/{it,schema,entity_key,notify,two_phase}.rs`

**Interfaces:**
- Consumes: Task 2's schema.
- Produces: the store API `game` and `server` call for terrain, with the **names and signatures it had
  at `V`** for chunk and ground rows (read them with `git show 0104a406:crates/world-store/src/rows.rs`
  and `…/handle.rs`), each write taking the owner-epoch fence argument the stud versions take at `P`.
  `TerrainFromStore` (`terrain.rs`) yields voxel `Chunk`s. Codec constants: `FORMAT_CHUNK = 1`,
  `FORMAT_COLUMN_GROUND = 1`, `FORMAT_REGION_GROUND = 1`; `FORMAT_REGION_META` and
  `FORMAT_ENTITY_BUNDLE` keep their `P` values unless their payload carried stud data — then bump by one.

- [ ] **Step 1:** Make the crate build: `cargo`-check through `just test-one world-store "--no-run"`.
  Port each stud row function at `P` that has a later (R3) change — fencing, `prepare_cut` parts,
  per-region entity keys, the cut `NOTIFY` — onto the voxel row functions of `V`.
- [ ] **Step 2: Port the three post-range test files** (`entity_key.rs`, `notify.rs`, `two_phase.rs`):
  replace each stud chunk fixture by a voxel `Chunk` fixture of the same region and chunk coordinates;
  assertions unchanged.
- [ ] **Step 3: Add the Review Focus 5 test** to `two_phase.rs`:
  `a_cut_part_writes_only_its_own_regions_chunks` — two hosts stage parts for one cut, each with a
  changed chunk in its own region; after commit assert each `chunks` row carries its owner's data, and
  that a part staged with a stale epoch returns `StoreError::Fenced` and writes no `chunks` row.
- [ ] **Step 4: Restore the fixtures** `chunk.bin`, `column_ground.bin`, `region_ground.bin` came back
  with the revert — run the codec fixture tests and expect PASS **without regenerating them**.
- [ ] **Step 5: Run the crate's whole suite once** (wide change): `just pg-up` then
  `just test-one world-store ""` — expected: PASS, including the Postgres-backed tests
  (`MMO_TEST_DATABASE_URL` from `crates/pg-test-support`).
- [ ] **Step 6: Commit** `feat(world-store): voxel chunk rows under fencing, two-phase cuts and terrain notify`.

### Task 4: game port — sim core and slot order

**Files:**
- Modify: `crates/game/src/{lib,protocol,state,region,region_runner,player,slot_order,intents}.rs`
- Modify: `crates/game/tests/{packet_codec,state_hash_check}.rs`, `crates/game/tests/common/{mod,rig}.rs`

**Interfaces:**
- Consumes: Task 3's store API.
- Produces: `Intent` with exactly the `V` variants (`BreakBlock`, `PlaceBlock`, `DepositItems`,
  `TakeFromPile`, `PutInContainer`, `TakeFromContainer`); `Target` without `Stud`;
  `GameEventKind` without `Prefab`; `slot_class` with no catch-all arm; the voxel test rig in
  `tests/common` used by Tasks 5 and 6: `common::install_real_block_props()` and the flat-ground region
  builders as named at `V`.

- [ ] **Step 1: Write the failing tests** in `slot_order.rs` (its `#[cfg(test)]` module), replacing
  the stud-intent helpers at `P` :375–386 and :614:
  - `a_break_block_reply_sorts_by_sender_and_content` — two `IntentReply::Granted(Intent::BreakBlock{..})`
    replies of one slot from different senders come out in sender order whatever order they went in.
  - `a_block_reply_and_an_arrival_of_one_slot_keep_class_order` — arrival first, then the reply
    (`SlotClass::Arrival` before `SlotClass::Intent`).
- [ ] **Step 2:** In `reply_owner` drop the two stud arms (`P` :178–183); `BreakBlock` / `PlaceBlock`
  stay `None`. In `slot_class` drop `GameEventKind::Prefab(..)` (`P` :92). Keep the match exhaustive.
- [ ] **Step 3:** Port `state.rs`, `region.rs`, `region_runner.rs`, `player.rs`, `protocol.rs`,
  `lib.rs`: every post-range hunk that names a stud type gets its voxel counterpart
  (`StudRegionSeed` → the `V` region seed of `Column`s, stud chunk maps → `Chunk` maps,
  `carry_components` / `Client::arrive` destructured with no `..` and without `prefabs`).
  `game/src/lib.rs` keeps its glob re-exports.
- [ ] **Step 4: Run** `just test-one game "--lib slot_order::"`,
  `just test-one game "--test it packet_codec::"`, `just test-one game "--test it state_hash_check::"`
  — expected: PASS. The rollback bar holds: `hash(before) == hash(after undo)`.
- [ ] **Step 5: Commit** `feat(game): sim core and slot order on voxel terrain`.

### Task 5: game port — world manager, halo and hand-off across host seams

**Files:**
- Modify: `crates/game/src/world_manager/{mod,peers,placement,world_link}.rs`, `world_cut.rs`,
  `halo.rs`, `peer.rs`
- Modify: `crates/game/tests/{halo,handoff_schedule,peer_schedule,coordinator_requests,coordinator_lifecycle}.rs`,
  `crates/game/tests/common/multi_rig.rs`

**Interfaces:**
- Consumes: Task 4's rig and types.
- Produces: `WorldManager` spawner hook
  `pub fn with_genesis(mut self, genesis: InlineGenesis) -> Self` replacing `with_stud_genesis`
  (`P` `world_manager/mod.rs:347`), where `InlineGenesis = Arc<dyn Fn(RegionCoords) -> RegionSeed + Send + Sync>`
  and `RegionSeed` is the `V` region seed type. Halo across hosts carries `HaloChunk` / `HaloDelta`.

- [ ] **Step 1:** Port the halo-across-host-seams behaviour of `5d6d1eca`, `8ca48709` and `f3177da6`
  onto `HaloChunk` / `HaloDelta`: a dropped peer link is `HaloSourceGone`; a parked region across a
  host seam keeps its mirrors; a halo seed sorts after its source's `HaloSourceGone`.
- [ ] **Step 2:** Port the test files: stud ground fixtures become the voxel flat-ground fixtures of
  Task 4; every assertion on a stud cell becomes the same assertion on the block at that tile.
- [ ] **Step 3: Run** `just test-one game "--test it halo::"`,
  `just test-one game "--test it peer_schedule::"`, `just test-one game "--test it coordinator_requests::"`,
  `just test-one game "--test it coordinator_lifecycle::"` — expected: PASS.
- [ ] **Step 4: Run** `just test-threads` — expected: PASS (seeded jitter in `handoff_schedule`,
  `peer_schedule`).
- [ ] **Step 5: Run the game crate's whole suite once** (wide change — say so before starting):
  `just test-one game ""`. Record the state of the tests the stud commit listed as failing (spec,
  "Facts"): the 3 game-lib LOD tests when run together. Report them; do not fix them here.
- [ ] **Step 6: Commit** `feat(game): halo, hand-off and world cuts across hosts on voxel terrain`.

### Task 6: worldgen and server port

**Files:**
- Modify: `crates/worldgen/src/lib.rs` (only if a post-range commit touched it; otherwise it equals `V`)
- Modify: `crates/server/src/{lib,world_file,perf}.rs`
- Modify: `crates/server/tests/{it,hosts,observer,roles}.rs`

**Interfaces:**
- Consumes: `with_genesis` (Task 5), the store API (Task 3).
- Produces: `server::world_file::bind_world_terrain(&store)` returns `Ok` for a voxel world and the
  Task 2 refusal for a stud one; no `prefab-editor` keyword world.

- [ ] **Step 1:** In `hosts.rs` replace `stud_flat_seed` (`P` :73) by
  `fn flat_seed(_rc: RegionCoords) -> game::RegionSeed` — flat ground, grass over dirt over stone,
  surface at the same height in metres the stud fixture had.
- [ ] **Step 2: Port the dig test** (`P` `hosts.rs:2491–2568`) as Review Focus 3:
  `a_block_broken_across_a_host_seam_changes_the_owner_and_the_halo` — the breaker stands in host A's
  last block column, breaks the block one column over in host B's region; assert the intent is
  `Granted`, B's chunk has air there, A's halo reads air there, and both hosts' state hashes match
  their own replays.
- [ ] **Step 3:** Port `server/src/lib.rs` and `world_file.rs`: generator selection is the `V` voxel
  one under the `P` role split; remove the `prefab-editor` world.
- [ ] **Step 4: Run** `just test-server`, then `just test-r2`, then `just test-r3` (serial; Postgres
  up) — expected: PASS.
- [ ] **Step 5: Run** `just smoke` — expected: PASS inside 60 s. This is the first point the whole
  workspace type-checks; if the client does not, do Task 7 and return.
- [ ] **Step 6: Commit** `feat(server): voxel worlds under the role split and many hosts`.

### Task 7: client port

**Files:**
- Modify: `crates/client/src/{instance,local_server,harness,bevy_harness,lib,connections}.rs`,
  `crates/client/src/renderer/{mod,join_timeline,slint_overlay}.rs`
- Modify: `crates/slint-ui/ui/` only where a post-range commit references a prefab or stud screen

**Interfaces:**
- Consumes: `ServerPacket` and `TerrainSnapshot` of Task 4.
- Produces: the voxel bridge (`renderer/bridge.rs`, `bricks.rs`, `meshing/`, `voxel_pull/`,
  `lod_rings.rs` as at `V`) fed by the `P` connection set: region traffic counts only from the
  connection its region maps to; LOD requests go to the home only.

- [ ] **Step 1:** Port the listed files. A new per-world resource, `Local` or entity root is registered
  (`add_session_reset`, `SessionLocal`) or tagged `SessionScoped`.
- [ ] **Step 2: Run** `just smoke` — expected: PASS. Commit message of this task's commit carries the
  line `tree builds again` (the bisect marker Task 1 promised).
- [ ] **Step 3: Run** `just client-wasm` and `just slint-wasm` — expected: both build.
- [ ] **Step 4: Ask the user** before running client tests. With a yes:
  `just test-one client "--lib lod_rings::"`, `just test-one client "--lib block_target::"`,
  `just test-one client "--lib selection::"`, and the restored `mesh_parity` and `pull_smoke` tests
  under the target names their `[[test]]` entries have at `V`. Record the
  state of the 8 `lod_rings` tests, `block_target::stages_grow_the_same_cracks` and
  `selection::click_anywhere_on_a_tall_body_hits_it` (listed as failing before the stud work).
- [ ] **Step 5: Commit** `feat(client): the voxel renderer on one connection per host — tree builds again`.

### Task 8: determinism, physics goldens, lint

**Files:** none planned; fixes land where a check fails.

- [ ] **Step 1: Run** `just determinism-wasm` — expected: PASS.
- [ ] **Step 2: Run** `just test-one game "--test it physics_goldens::"` — expected: all 8 goldens PASS
  as recorded at `V` (`walk_scale_seam`, `terrace_uphill_1_voxel`, `step_up_one_block` and the 5 that
  had been re-recorded on stud ground). A failure is a finding: stop and report, do not re-record.
  If the failure traces to the kept parry BVH-order hunk (Task 1 Step 4b), the fix stays and the
  golden question goes to the user.
- [ ] **Step 3: Run** the physics-next suite: `just test-one physics-next ""` — expected: PASS.
- [ ] **Step 4: Run** `just clippy` — expected: no warnings outside the known-red crates (parry3d,
  slotmapd, macros, bbmodel).
- [ ] **Step 4b: Write the known-red report** into `TODO.md` (Task 9 Step 3 picks it up): for each
  test on the spec's known-red list, its state now. Any red test **not** on that list is a port
  mistake and is fixed before this task closes.
- [ ] **Step 5: Commit** any fix as `fix(<crate>): …`, one per finding.

### Task 9: justfile, docs, memory

**Files:**
- Modify: `justfile`, `CLAUDE.md`, `TODO.md`,
  `docs/superpowers/plans/2026-11-09-stud-world-replacement.md` (one line at the top: superseded, with
  this plan's path)

- [ ] **Step 1: justfile** — the `V` voxel recipes and dials return (`chunk_blocks=` /
  `VOXEL_CHUNK_BLOCKS`, the far/near render dials) beside every recipe added in `S..P`
  (`dev-r2`, `dev-r3`, `host`, `go-nogo-r3`, `test-r2`, `test-r3`, `site-*`, `orion-*`); `run-studs`,
  `prefab-editor`, `forge-ui` stud arguments go. Check: `just --list` runs and names no stud recipe.
- [ ] **Step 2: CLAUDE.md** — Project line (voxel terrain, not toy-brick); the chunk-size bullet back
  to the build-time dial; the Worldgen, Client and Prefabs sections rewritten from
  `git show 0104a406:CLAUDE.md` for terrain and from `P` for everything else; the join-snapshot bullet
  removed (slim snapshots are gone); add the Task 2 refusal rule under Server / store.
- [ ] **Step 3: TODO.md** — mark the stud world entry reverted with this plan's path; add follow-ups:
  slim join snapshots for voxel chunks; prefabs as block groups; the `far_voxel` question for the user;
  the pre-existing failing tests recorded in Tasks 5 and 7.
- [ ] **Step 4: Commit** `docs: voxel terrain is back — CLAUDE.md, TODO.md, justfile`, then
  `git tag -d pre-voxel-return`.

### Task 10: a human look, then the worlds (user)

- [ ] **Step 1:** The user runs `just run -- --reset` (the debug world is a stud store and is refused
  otherwise) and checks: terrain meshes near and far, break and place a block, dig underground, walk
  a slope, cross a region seam, fell a tree, water shows. Check the `AdapterInfo` log line first.
- [ ] **Step 2:** The user runs `just run-two` and `just dev-r3 2`, crosses a host seam, breaks a block
  across it.
- [ ] **Step 3:** With the user's go: re-forge local worlds under `worlds/`; plan and apply the public
  web world and any orion world re-forge (`just deploy`, `just orion-plan …` first). Outward-facing —
  never started without the user.
