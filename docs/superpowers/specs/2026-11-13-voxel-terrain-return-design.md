# Voxel terrain returns — design

Date: 2026-11-13. Status: **approved by the user 2026-11-13** (R4 waits; known-red accepted; no R3
re-measure).

## Intent

Bring back the voxel chunk terrain that was deleted on 2026-11-10/11 and remove the stud (toy-brick)
terrain that replaced it. The user's reason (2026-11-13): "the brick system experiment is not as good."

**Who decided what.** The user asked for the return and picked the method ("rewind and replay",
2026-11-13). Everything else in this document is **Claude's call** unless marked *user*; each call is
listed under "Calls to check" at the end.

## Facts this rests on

All checked in git on 2026-11-13; nothing was built or run.

- `0104a406` (2026-11-09) is the last commit with voxels as the only terrain and no stud code.
- The **stud range** is the 21 commits `4a81dfd9` … `cc728a58` (`git log 0104a406..cc728a58`): the stud
  world (P1–P9), the cutover, the two voxel deletions, brick selection and orders, re-bricking, brick
  cliffs, and 14 prefab commits. Its net diff is 472 files, +46,082 / −63,018.
- After the range come 107 commits (at the time of the dry run) (`cc728a58..develop`): R2b cleanups, R3 multi-host, `SlotInbox`, the
  website and login work. Net 202 files, +39,938 / −2,807. None of them is stud work, but several were
  written against stud types.
- **Dry run** — `git merge-tree --write-tree --merge-base=cc728a58 develop 0104a406`, which is what
  reverting the whole range amounts to:
  - 26 conflicted files, about 45 hunks. Heaviest: `world-store/src/rows.rs` (5),
    `game/src/world_manager/mod.rs` (5), `CLAUDE.md` (4), `justfile` (3), `world-store/src/thread.rs` (3),
    `game/tests/common/rig.rs` (3). Two are modify/delete (`client/src/renderer/stud/mod.rs`,
    `world-store/tests/voxel_refusal.rs`) and resolve to "delete".
  - 11 files newer than the range merge cleanly but still use stud types, so they will not compile:
    `game/src/slot_order.rs`, `game/tests/{peer_schedule,coordinator_requests,coordinator_lifecycle}.rs`,
    `game/tests/common/multi_rig.rs`, `server/tests/{hosts,observer,roles}.rs`,
    `world-store/tests/{entity_key,notify,two_phase}.rs`.
  - 66 files were changed on both sides; the ones that merged cleanly may still be wrong in meaning.
    Only a build and the tests will show that.
  - Store migrations after the revert: `0001`–`0003`, then `0006`–`0010`. `0004_stud_terrain` and
    `0005_drop_voxel_terrain` vanish, leaving a gap.
  - `game/src/region_wire.rs` (slim join snapshots, `VOXEL_SNAPSHOT_COLLIDERS`) was added inside the
    range and goes away with it.
- The stud commit message lists tests that failed at the time and calls them "not from this work": 8
  `lod_rings` tests, `block_target::stages_grow_the_same_cracks`,
  `selection::click_anywhere_on_a_tall_body_hits_it`, 3 game-lib LOD tests when run together. Whether
  they failed at `0104a406` is unknown.
- The working tree has uncommitted edits to four files the revert deletes
  (`assets/studs/parts.bin`, `client/src/renderer/stud/mesh.rs`, `game/src/stud_assets.rs`,
  `tools/ldraw-bake/src/classify.rs`) and to two conflicted files (`CLAUDE.md`, `justfile`).

## Goals

1. Voxel terrain as it was at `0104a406` is the only terrain: chunk storage, chisels, meshing and the
   pull renderer, LOD rings, voxel bodies, colliders, worldgen columns, the store's `chunks` table.
2. Everything that landed after the stud range keeps working on voxel terrain: role split, many hosts,
   peer links, world cuts, observer sessions, `SlotInbox`, per-region entity rows, terrain notify.
3. The verification bar of the project holds at the end: `just smoke`, each touched crate's non-client
   suite, `just test-threads`, `just test-r2`, `just test-r3`, `just determinism-wasm`, and a human look
   at the world with `just run`.

## Non-goals

- No conversion of stud worlds. Worlds are re-forged (*as the stud switch did in the other direction*).
- No dual-terrain switch. Stud code is not kept alive beside voxels.
- No new voxel features and no cleanup of the old system. In particular the `far_voxel` brick raymarch
  renderer comes back as it was; deleting it is a separate question for the user
  (`docs/memories/brick-far-renderer-unwanted.md`: "ask before removing it").
- Prefabs are not ported. They leave the build with the range and stay in git at `cc728a58`.
- The D4 rename (`BLOCK_*` names) is moot: the names mean blocks again.

## Anti-goals

- Losing or weakening any R3 behaviour to make the port easier (a seam test deleted instead of ported).
- A store write that skips the fence, or a table two hosts' cut parts can both write.
- A stud world opened as a voxel world and silently regenerated.

## Approaches

1. **Reverse strangler** — bring voxel code back beside studs behind `VOXEL_TERRAIN`, port layer by
   layer, flip, delete studs. Always buildable; costs a throwaway switch and keeps studs alive only to
   delete them. Claude's first recommendation.
2. **Rewind and replay** — undo the stud range in one step and port what came after onto voxels. Less
   total work, an honest history, no throwaway code; costs a stretch where the tree does not build and
   all multi-host conflicts at once. ***User's pick.***
3. **Keep the stud sim, draw it as cubes** — dropped: the user asked for the voxel system back, not a
   different look.

## Design

### The rewind

On `develop`, in this checkout (the project forbids worktrees): the range is undone as one three-way
merge (base `cc728a58`, ours `develop`, theirs `0104a406` — the same merge the dry run measured, so
each line conflicts once instead of once per reverted commit), conflicts resolved, committed as **one
revert commit that does not build**, said so in its message. The port follows as ordinary commits until the tree builds;
`git bisect` skips the stretch. Reverts, not a reset or a rebuilt branch: `develop` stays linear and
nothing published is rewritten.

Conflict rule: where both sides changed a line, the **post-range behaviour** (R3) wins and is rewritten
against voxel types; where the range's own stud code is the only content, it goes.

### The port (what the later commits need)

In dependency order, each crate building before the next starts:

1. **world-store** — the voxel rows (`chunks`, `regions.ground`) under today's rules: owner-epoch
   fencing on every write, two-phase cut parts, per-region entity keys (0009/0010), terrain notify
   (`listen.rs`, `terrain.rs`) carrying voxel chunks instead of stud chunks.
2. **game** — `world_manager` (peers, placement, world cut), `slot_order` (classify `BreakBlock` /
   `PlaceBlock` / `SetBlocks` where it now classifies the stud intents), halo across host seams
   (`5d6d1eca`, `8ca48709`, `f3177da6`) on `HaloChunk` / `HaloDelta`, `protocol.rs`.
3. **worldgen**, **server** — generator selection in `server/src/lib.rs` and `world_file.rs`; the
   prefab-editor world goes.
4. **client** — `instance.rs`, `local_server.rs`, `harness.rs`, the connection set against the voxel
   bridge. The prefab editor and its Slint screens go.
5. **tests** — the 11 clean-but-stud files and the conflicted test files get voxel ground fixtures.

### Store schema

Migrations are append-only: a database that ran `0004`/`0005` cannot un-run them, and Postgres on orion
never ran `0005`. So `0004` and `0005` **stay in the tree** and a new **`0011_voxel_terrain_returns`**
recreates `chunks` and `regions.ground`, drops `stud_chunks` and `regions.stud_ground`, and keeps
`world.terrain`. The refusal flips and, like today's, runs **before** any migration: a store that has
saved regions and has not yet applied `0011` is refused and left as it was — that covers a stud world
and also a voxel world saved before the stud switch, whose chunks `0005` would otherwise drop. From
`0011` on, a world whose `terrain` is `Studs` is refused. `TerrainKind` keeps both values for that
reason.

### Fixes that rode inside the range

The range is not pure stud work. One rider **stays**: the `mmo-fork` hunk in
`crates/parry/src/shape/voxels/voxels.rs` that builds the `Voxels` chunk BVH in chunk-id order
(hash-map order differs between native and wasm32), with the physics-next test that pins it. Voxel
terrain uses the same `Voxels` shape, so reverting it would bring back a cross-target determinism
hazard. The other riders go with the range: `set_raw` in `crates/macros`, `swap_shape_raw` in
`crates/rapier` (it only serves `region_wire`), and the physics-next test changes made for stud ground.

If the kept parry hunk makes an old voxel golden fail, the determinism fix wins and the golden question
goes to the user — it is the one case where "no re-record" may give way, and never silently.

### Tests that were already red

No baseline run at `0104a406` is possible (no worktrees, no checkout of old commits). So the tests the
stud commits name as failing before their work are **known-red**: 8 client `lod_rings` tests,
`block_target::stages_grow_the_same_cracks`, `selection::click_anywhere_on_a_tall_body_hits_it`, 3
game-lib LOD tests when run together, `atmosphere shadow reach`, `instance desired_window`,
`just pull-smoke`, and `just clippy` on parry3d, slotmapd, macros and bbmodel. Their state is recorded
and reported; a red test outside this list is a port mistake. Making the list green is not part of this
work.

### Join snapshots

`region_wire.rs` goes with the range, so voxel joins send whole chunks and colliders again, as at
`0104a406`. Slim snapshots for voxels are a follow-up, not part of this work The R3 go / no-go was
accepted on stud terrain with slim snapshots (0.03–0.04 MiB per 64 m region); the user decided
(2026-11-13) not to re-measure it on voxel terrain.

### Goldens and versions

- Physics goldens: the 3 deleted voxel goldens return with the revert; the 5 re-recorded on stud ground
  return to their voxel recordings. They must pass **without re-recording**; a failure is a finding.
- `WORLD_MAP_VERSION` stays 1 (the forge did not change in either direction).
- Codec formats return to the voxel ones; `FORMAT_REGION_META` and `FORMAT_ENTITY_BUNDLE` keep whatever
  later commits need — decided file by file during the store port.

### Deploy

The public web world and any orion world are re-forged after the switch. Not done as part of the code
work: it is an outward-facing step the user starts.

## Before any of it starts (user)

1. Commit or discard the uncommitted edits to the four stud files; the revert deletes those files.
2. Commit the uncommitted `CLAUDE.md` and `justfile` edits; both are in the conflict set.
3. Hold work on `develop` in `game`, `server`, `world-store` and `client` while the tree does not
   build (`crates/login` and the website are not affected). *User, 2026-11-13: R4 waits.* The R4 takeover draft
   (`docs/superpowers/plans/2026-11-13-r4-takeover.md`) targets `world_manager/{mod,peers,placement,
   world_cut}.rs`, the heaviest conflict area.

## Left to the builder

- The exact resolution of each conflict hunk.
- Whether the port lands as one commit per crate or finer.
- Fixture shape for voxel ground in the multi-host test rigs.

## Calls to check

| Call | Why |
|---|---|
| Revert on `develop`, one non-building revert commit | No worktrees allowed; linear history; nothing rewritten |
| Prefabs leave the build, kept in git | They are bricks by LDraw number; a block prefab is a new design |
| `far_voxel` returns untouched | Pure restore; its removal needs the user's yes |
| `0004`/`0005` stay, new `0011` | Migrations cannot be un-run |
| Slim join snapshots dropped for now | Written for stud chunks only |
| Stud worlds refused, not converted | Mirrors the stud switch's own rule |
| Goldens must pass without re-recording | They are the proof the old physics is back |
| The parry BVH-order fix stays | A determinism fix for the `Voxels` shape voxel terrain also uses |
| The named pre-existing failures are known-red | No baseline run is possible under the no-worktree rule |
| Worlds saved before `0011` are refused, voxel ones too | `0005` would drop their chunks; it cannot be edited (checksummed) |
