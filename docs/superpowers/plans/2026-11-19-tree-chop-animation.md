# Tree chopping animation: the whole tree wobbles, then goes top to bottom

Status: BUILT 2026-11-19 (Tasks 2-5), uncommitted; NOT yet looked at in play and the piece count
(Task 1) NOT measured. The two calls marked **(call)** are Claude's; the owner said "implement it"
with them as written.

As built, where it differs from the text below: the shake sets are not a field of `BlockDamageView`
but travel beside it (`HomeView::shake`, `ClientUpdateEvent::TreeShake`, `BlockDamage::shake`), cached
per lead in the runner (`RunnerState::shake_cache`); the wave lives in `block_damage.rs`
(`start_tree_vanish`, `TreeFelledMsg`), not a module of its own; whole-block quads are meshed once
per block id (`PieceMeshes`); a replayed chop is deduplicated on (region, first block) within
`FELL_REPLAY_TICKS`. Tunables: `SHAKE_RIPPLE_SECS_PER_BLOCK`, `FELL_LAYER_SECS`, `FELL_MAX_SECS`,
`FELL_SHRINK_SECS`.

**Direction (owner, 2026-11-19, after seeing both):** top to bottom, as written here; bottom to top
was tried and dropped.

## What is wanted

Dawid, 2026-11-19: "I want the block wobble animation to play on all the blocks and I want the blocks
to disappear from top to bottom, not all at once."

Read as:

1. While a player chops a tree, every block the chop will bring down wobbles with each strike — not
   only the one log under the crosshair.
2. When the chop lands, those blocks go layer by layer from the top down instead of in one frame.

## How it works today

- **Mining** (`crates/game/src/mining.rs`): `Client::mining` holds the one target block; a trunk block
  takes `FELL_TICKS_FACTOR` (2×) as long. The break is `Intent::BreakBlock { survival: true }` to the
  block's owner; a tree block goes to `Rollback::player_chop_safe`.
- **Felling** (`trees.rs`, `orders.rs`): `TREE_FELLING` is a build-time dial, default `Instant`
  (uncommitted work in this checkout). `fell_instant_safe` clears every block `trees::fell` names in
  ONE batch (`clear_blocks_safe`) and returns them; `player_chop_safe` drops their items per column at
  the column's lowest cleared cell. A commander's chop order (`chop_safe`) uses the same call and drops
  nothing. A tree is up to `MAX_TREE_BLOCKS` (16384) blocks; the biggest species box is 11552.
- **Wobble** (`crates/client/src/renderer/block_damage.rs`): render-only. The home region's runner
  projects `Rollback::block_damage_views` each tick (`HomeView::damage`, `regions/runner.rs`), the game
  thread sends it when it changes (`ClientUpdateEvent::BlockDamage`, `instance.rs`), the bridge stores
  it in `BlockDamage`. Each listed block is drawn as a **piece** over the terrain (the terrain keeps the
  block): a root entity at the block's centre with two `ChunkQuads` children (the block, and its crack
  shell). `animate_block_damage` strikes a mined piece every `SWING_SECS` and poses it by `wobble(age,
  strike)`. A piece whose block broke is despawned at once.
- **Render-only sim events**: `GameDataUpdateKind::ItemsLanded` / `ItemsPickedUp` are "Do only" events
  the sim sends (`self.data.send`), stamped with their tick; the bridge forwards them and
  `item_flights.rs` dedupes rollback replays on (region, tick, cells) (`RecentFlights`).

So after a chop the client sees: one log wobbling, then the whole tree gone in one remesh, items
flying to the ground.

## Approaches

**A. Animation only (picked — call).** The sim stays as it is: the blocks are cleared at once. The
client keeps drawing them as pieces and takes them away top to bottom. Two additions: the damage view
names the blocks a chop would fell (so they wobble while chopping), and the sim sends one render-only
event when a tree is felled (so the pieces know to go in a wave).
Trade-off: for about a second the player sees blocks that are no longer solid, and the items already
lie under the tree. No sim state, no hashing, no determinism re-record, works the same for a
neighbour's region and for chop orders.

**B. The sim clears the tree layer by layer.** A pending-felling record per tree in rollback state,
advanced each tick; the blocks stay solid until their layer goes; items drop as layers go.
Trade-off: correct collision during the fall, but new hashed state (a `#[rollback]` field, save
format, `carry_components` / cut classification), a tree that spans a seam needs intents per layer,
and every rule about a half-felled tree must be decided (mining a block of it, a second chop, a
save cut in the middle, leaves no longer supported). Out of proportion for an animation.

**C. Wobble in the terrain shader.** No pieces; the vertex shader displaces the tree's quads.
Trade-off: the terrain mesh has no per-block identity (greedy quads span blocks), so "per block"
cannot be done without a remesh or new record bits (`voxel_pull.wgsl` layout is locked).

B is the fallback if the non-solid second in A turns out to matter in play.

## Design (approach A)

### Calls

- **(call) When the wobble plays:** on every strike while chopping (all blocks the chop would fell,
  struck together with the target), and once more per layer as the wave reaches it. If only the
  second is wanted, drop Task 2 and Task 3's shake half — the rest stands.
- **(call) Items:** unchanged — they drop when the chop lands, under a tree that is still dissolving.
  Holding their flights until the wave reaches the column's lowest block is a possible follow-up.
- `TreeFelling::Physics` keeps toppling a body; it only gains the shake while chopping.

### Sim side (`crates/game`) — render-only, nothing hashed

1. `BlockDamageView` gains `shake: Vec<(BlockPos, Block)>` — the other blocks that go with the
   target: for a target that is tree wood, `trees::fell(self, target)`'s `cut` ∪ `falling` minus the
   target, sorted; empty otherwise. `block_damage_views` is called every tick by the runner and the
   flood is up to 16384 blocks, so it is NOT computed there per tick: the runner keeps a one-entry
   cache per mined target (below).
2. `GameDataUpdateKind::TreeFelled(TreeFelledView { blocks: Vec<(BlockPos, Block)>, tick })`,
   appended LAST in the enum (bincode, web workers), "Render-only, Do only … dedupe replays on
   `tick`" like `ItemsLanded`. Sent from `fell_instant_safe` with the `gone` list (what was really
   cleared), so player chops and chop orders both animate. Region-local positions, as `PileFlight`.
   `fell_instant_safe` has no tick argument: read `*self.data.tick`, as `fell_safe` does.

### Client side

3. **Runner** (`regions/runner.rs`): `home_view` fills `shake` through a cache held beside the
   region — key `(target, block)`, value `Arc<Vec<(BlockPos, Block)>>`; recomputed when a mined
   tree target is not in the cache, dropped when no client mines a tree. A tree that changes while
   it is being chopped (someone cuts a branch) shows a stale shake set until the target changes:
   accepted, the fell event is what decides what goes. `HomeView` derives `PartialEq` and
   `instance.rs` compares and clones `damage` every tick: keep `shake` an `Arc` so both are cheap
   (`Arc` compares by pointer first only if written so — compare with `Arc::ptr_eq ||` equality in a
   hand `PartialEq`, or the per-tick compare walks thousands of entries).
   A target in the halo (a neighbour's tree) gets no shake: `fell` over this region's `TileSource`
   does not see it. Accepted for now.
4. **Pieces** (`renderer/block_damage.rs`): a shake block is a piece like any other, with no crack
   shell and no state of its own: `DamagedBlock` gains `lead: Option<BlockPos>` (the target it
   shakes with). `sync_block_damage` spawns / keeps one per `shake` entry of every listed view
   (a block that is itself a listed target is only a target). `animate_block_damage` strikes a
   shake piece when its lead is struck (same frame, same `strikes` step, so the whole tree tilts
   together; a per-block phase offset `SHAKE_PHASE_STEP` by height makes it ripple up the trunk —
   a named const, 0 to turn off).
   When a view leaves the list, its shake pieces and — new — a tree target's own piece take the
   existing `leaving` path (one last wobble, then despawn) instead of the instant despawn: the
   `BlockDamage` list and the `TreeFelled` event travel different channels and may land in either
   order within a few frames.
5. **The wave** (new `renderer/tree_felling.rs`, registered beside `block_damage` in
   `renderer/mod.rs`; any resource through `add_session_reset`, entities `SessionScoped`): the bridge
   forwards `TreeFelled` as a message (as `ItemPileMsg::Landed`). On a first-seen event (dedupe on
   (region, tick, first block), a bounded `Recent…` set like `RecentFlights`):
   - for every block, adopt the existing piece at (region, block) or spawn one (a chop order, or
     another player's chop outside the damage list) — spawning goes through the same
     `piece_records` + `quads_child` helpers, made `pub(super)`;
   - mark it `Vanishing { delay, age }` with `delay = (y_top − y) · layer_secs`, where
     `layer_secs = min(FELL_LAYER_SECS, FELL_MAX_SECS / layers)` — proposed `FELL_LAYER_SECS` 0.06,
     `FELL_MAX_SECS` 1.5 (a 20-layer tree takes 1.2 s). A `Vanishing` piece is no longer touched by
     `sync_block_damage`.
   - animate: at `delay` the piece is struck once (`wobble`), and over the last `FELL_SHRINK_SECS`
     (0.12) of that wobble it scales to 0 about its centre, then despawns. Pure function
     `vanish_pose(age_since_delay, strike) -> Option<(scale, Quat)>` (`None` = gone).
   The terrain remesh that removes the real blocks is triggered by the same tick's updates, so the
   pieces are already standing when it lands; a piece at rest is `PIECE_SCALE` over the block and
   hides it until then.

### Risks to check before keeping it

- **Piece count.** A big crown is thousands of blocks; each piece is an entity with a `ChunkQuads`
  child = one arena allocation and one chunk slot in `voxel_pull`, and a wobbling piece changes its
  transform every frame. First thing to measure (Task 1). If it is too slow: (a) skip blocks none
  of whose six faces is exposed; (b) share one `Arc` of records per (block id) — already cheap on
  the CPU side; (c) last resort, draw each LAYER as one body mesh (`mesh_body` over the layer's
  blocks) that wobbles as a whole — per-block wobble only near the cut.
- **Leaves are cutout.** A tilted leaf piece shows the terrain's own leaf block through its holes
  while the tree still stands (shake phase). Look at it; if ugly, shake leaves with pop only (no
  tilt).
- **A refused prediction.** A client that predicted the chop and is rolled back plays the wave over
  blocks that come back. Rare, harmless (the terrain redraws them).

## Tasks

Verification is by `just smoke`, the touched crates' checks and looking at it (`just run`); under the
repository's rule no test is committed for this (temporary ones while working are deleted first).
Nothing here is hashed or serialized into saves, so no determinism re-record — confirm
`GameDataUpdateKind` is not part of any golden before relying on that.

1. **Measure the piece count (throwaway).** In a scratch change, spawn pieces for every block of
   `trees::fell` of the biggest tree near the debug spawn and wobble them all; release build,
   `VOXEL_VSYNC=off`, confirm the adapter. Decide per-block pieces vs the fallbacks above. Ask the
   owner before running (benchmarks on this box are theirs to schedule). Discard the change.
2. **Sim: `shake` on `BlockDamageView`.** `mining.rs`: the field and a `pub fn shake_blocks(&self,
   target) -> Vec<(BlockPos, Block)>` on `Rollback` (`trees::fell` → cut ∪ falling − target, with
   `block_at`); `block_damage_views` takes the shake sets from its caller (a lookup closure) so it
   stays cheap. Update the one other constructor of the view, if any.
3. **Client: the runner's cache and shake pieces.** `regions/runner.rs` (cache, `home_view`),
   `block_damage.rs` (`lead`, spawn / keep / strike together, the `leaving` path for tree pieces).
   Look: chopping an oak shakes the whole tree per swing; letting go leaves it standing with the
   cracks on the one log; another player's chop shakes too (`just run-two`).
4. **Sim: `TreeFelled`.** `state.rs` (variant, last), a view struct beside `PileFlight`,
   `orders.rs` `fell_instant_safe` sends it. Every `match` on `GameDataUpdateKind` must compile
   (the bridge, any recorder).
5. **Client: the wave.** `bridge.rs` forwards; `renderer/tree_felling.rs` (dedupe, adopt / spawn,
   `Vanishing`, `vanish_pose`); register in `renderer/mod.rs` after `animate_block_damage`. Look:
   a player chop and a commander chop order both go top to bottom; a branch cut takes only its
   limb; a rejoin / second world leaves no piece behind.
6. **Tune and close.** The consts above by eye; update the module docs of `block_damage.rs` and
   `trees.rs` (`Instant`: "vanishes at once" is now the sim's view only); `just smoke`; `just
   web` type-check if the compute / worker side was touched (it should not be).

## Left to the builder

Names, the exact shape of the cache, whether the wave lives in `block_damage.rs` or its own module,
and whether a chiselled tree block (if one can exist) carries its chisel in the two new lists.
