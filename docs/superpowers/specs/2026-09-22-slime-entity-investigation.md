# Slime entity — investigation (2026-09-22)

Status: **S1 implemented 2026-09-22 (uncommitted)** — see §6. The user's answers to §5: ambient wildlife, the Minecraft cube look, spawning anywhere on the surface, and configurable spawn rates with a made-up default. This covers what a Minecraft / Slime Rancher–style slime would take in this codebase, which design choices are open, and a recommended first slice.

## TL;DR

- **The engine is closer to ready than it looks.** These are already entity-generic:
  - ECS components
  - `Motion` / `plan` / `settle`
  - region handoff (`EntityBundle`, ghosts)
  - parking and snapshots
- **The slime is the first non-player entity.** `EntityKind` has only `Player`, and there are no NPCs, mobs or items anywhere.
- **Four player-only seams must open up:**
  1. Movement is driven only from `CameraController` (camera.rs:154-285).
  2. Per-tick pose events reach only entities that have a sim `Camera` (physics.rs:60-69). A camera-less slime would freeze on screen.
  3. The avatar mesh has one hard-coded kind (renderer/avatar.rs:8-44).
  4. `ColliderSpec` only knows `CapsuleY` (protocol.rs:192-195, state.rs:940-944, state.rs:1119-1137).
- **Minecraft's hop AI is close to ideal for rollback:**
  - `TICK_RATE` = 50 ms, which *is* a Minecraft tick, so its tick constants port 1:1.
  - The hop is `MoveKeys { jump_held, forward }` through the existing `plan`, since `plan` re-jumps on every grounded tick while jump is held (movement.rs:152-156).
  - The slime sits still between hops.
  - Its state is a few bytes.
- **All squash and stretch is render-only**, using Minecraft's `targetSquish` spring and volume-preserving scale. None of it enters the sim or the state hash.
- **The real cost is physics.** The client simulates every loaded region (3×3), and physics is ~99.8% of sim time, so every slime's KCC step is paid in 9 regions and again on each replay.
  - Mitigation: a slime sitting on the ground between hops skips the KCC entirely. That is most ticks.
  - Also cap slimes per region.

## 1. Reference designs (what we're borrowing)

Full notes with sources are in the research hand-back. These are the parts that matter.

### Minecraft slime

Source: 1.9 MCP `EntitySlime`, `RenderSlime`. All figures are in ticks, which equal ours.

- **Sizes and health:** sizes are 1, 2 and 4, with a hitbox of 0.52·size blocks (a cube). Health is size².
- **Split rule:** a slime above size 1 splits into 2–4 slimes of size/2 when it "dies".
- **Locomotion (`SlimeMoveHelper`):**
  - On the ground it counts down `jump_delay`. At 0 it jumps and redraws `jump_delay = 10 + rand(20)` (0.5–1.5 s), divided by 3 while it has a target.
  - Between hops on the ground, horizontal input is **zero**, so it sits still. In the air it holds full speed on its heading.
  - Yaw turns toward the desired yaw at most 90° per tick. The whole cube turns, with no separate head.
  - With no target it picks a random yaw every 40 + rand(60) ticks.
  - It hops constantly in water (80% of ticks).
  - The jump impulse is the player's, so a hop is about 1.25 blocks.
- **Spawning:** "slime chunks" come from a pure hash of (seed, chunk x, z), about 10% of chunks, deep underground. Swamps add a moon-phase and light gate. This model is deterministic and has no RNG state.
- **Rendering:**
  - A 6³ opaque core with eyes and a mouth, inside an 8³ translucent gel cube.
  - **Squish spring:**
    - On landing: `target = -0.5`.
    - On take-off: `target = 1.0`.
    - Every tick: `target *= 0.6` and `squish += (target - squish) * 0.5`.
    - Render scale: `f = squish / (size*0.5 + 1)`, `s = 1/(f+1)`, `scale(s, 1/s, s)`, which preserves volume.
  - The magma-cube variant "accordions" 8 stacked slabs instead.
- **Uses:** slimeballs → slime block (bounce, cancels fall damage).

### Slime Rancher

- **Core loop:** capture (vacpack) → corral → feed → **plorts** → market with saturating prices.
- **Traits:** each type has a diet and a favourite food, plus one special trait (rock rolls, phosphor glows and dies in daylight, tabby steals…).
- **Emotion model:** 3 hidden scalars — **hunger** (rises over time), **agitation** (rises once starving) and **fear** (from Tarr). They drive the face and behaviour rates (a lerp between the calm and agitated cadence).
- **Largo:** eating another type's plort fuses two slimes into one big slime. **Tarr** is a largo that ate a third type: a contagious failure state, cleansed with water rather than killed. It is systemic risk without violence, and it comes from the player's own logistics.
- **Jiggle:** CPU-animated vertices (238 per slime), SDF face textures, no skeleton, no GPU soft-body. SR2 uses PhysX rigid bodies.

**Fit with our direction (non-violent, cozy):** Slime Rancher is the proof that a creature-care loop carries a game without combat. Minecraft supplies the rollback-friendly locomotion and the look. Drop contact damage entirely.

## 2. What exists in the engine

| Piece | Where | Generic? |
|---|---|---|
| Entity slotmap + components | state.rs:183-193 (`Ecs`), `Component<T>` 246-252 | yes |
| Entity kind | state.rs:132-138 `EntityKind { Player }` | enum to extend |
| Body builder | state.rs:912-945 `build_kinematic_body_safe` (private) | yes, but capsule only |
| Player spawn | state.rs:745-815 `create_player_safe` | template for `spawn_slime_safe` |
| Removal | state.rs:824-890 `remove_entity_safe` | yes |
| Movement math | movement.rs `Motion` 83-94, `plan` 114-169, `settle` 172-185, `depenetration_lift` | yes ("NPCs can fill MoveKeys from anything") |
| Movement driver | camera.rs:188-254 inside `CameraController::on_tick` | **player-only**; iterates `player_entites` |
| Controllers | region.rs:166 `[CameraController, PhysicsController]` | add a mob controller before physics |
| Handoff | state.rs `scan_boundaries` 1202-1247, `extract_entity_safe`, `apply_arrival`, `apply_ghost` | yes (any kind + body, non-ghost) |
| Parking / snapshot | whole `Rollback` bincode | automatic |
| Render: create / move | bridge.rs:533-711 `CreateEntity`/`SetEntityPosition`/`SetEntityKind` | yes |
| Render: per-tick pose | physics.rs:60-69 emits `UpdateCameraViewMatrix` only for camera entities | **gap** |
| Avatar | avatar.rs:13-44, one `Capsule3d` for `Player` | **gap** |
| RNG in sim | none (`oorandom` declared, unused in game/src) | **gap** |

## 3. Design decisions

### D1. Where does slime AI run? → **in the region sim, as a controller** (recommended)

- **(a) In-sim `MobController`, deterministic on both sides.**
  - Zero new packets. The client predicts slimes for free, and they never mispredict unless a player touches them.
  - Parking, snapshots and handoff carry them automatically.
  - Cost: every client sims every slime in 9 regions.
- **(b) Server decides and streams intents** (the modding research's "input-like stream for NPCs").
  - Lets AI be arbitrarily expensive or hidden.
  - But it needs a packet per decision, and the client predicts slimes wrong until the intent arrives. That means rubber-banding on the most visible thing in a cozy game.
  - Keep it for mod-driven or complex NPCs later.

(a) wins for slimes: their AI is a handful of integer comparisons.

### D2. Randomness → **stateless hash `h(tick, EntityKey, salt)`**

- A splitmix64 of the tuple needs no state, so there is nothing to undo-log and nothing that can drift under rollback.
- Keys agree between client and server because the client region is a clone of the server snapshot, and in-sim spawns allocate in the same order on both sides.
- Alternative: an `#[undo(cell)] rng: u64` — more state and more hashing for no gain.
- Keep transcendental math on libm (`yaw_quat`), as movement already does.
- **Caveat:** the durable-persistence spec re-keys entities to `EntityId(u64)`. When that lands, hash that id instead of the slotmap key.

### D3. Collider → **v1: a short capsule (ball-ish), no protocol change; later `ColliderSpec::Cuboid`**

- The Minecraft slime is a cube, so a cuboid collider would be faithful.
- But the KCC-on-voxels jitter fixes (the three parry fork shape-cast fixes) were proven on capsules only. A cuboid sliding on flush voxel faces is exactly the internal-edge case that jittered.
- A capsule with a tiny `half_height` rides the existing proven path, and `ColliderSpec::CapsuleY` already crosses the seam.
- The visual can still be a cube; nobody sees a 0.5-block collider's corners.
- Add `Cuboid`/`Ball` to `ColliderSpec` + `build_kinematic_body_safe` + `collider_spec_of` only if a cube-shaped interaction needs it (e.g. slimes stacking flush).

### D4. Movement → **reuse `Motion` + `plan` + `settle`; extract the body-stepping half of CameraController**

- Refactor camera.rs:188-254 into `step_body(data, e, &MoveKeys, yaw)`, covering:
  - the query excluding self
  - `depenetration_lift`
  - `move_shape`
  - `settle`
  - `motion.set_safe` on change
  - `set_next_kinematic_translation`

  Both controllers call it. This is also the "entity-generic mechanisms" direction.
- **Hop mapping per tick** (after Minecraft):
  - grounded and `jump_delay > 0`: decrement, keys = none, **skip `step_body` entirely** unless displaced (see D7).
  - grounded and `jump_delay == 0`: keys = `{jump_held, forward}`, redraw `jump_delay = 10 + h % 20`.
  - airborne: keys = `{forward}` along the heading.
  - Yaw eases toward the target yaw by ≤ 90°/tick.
- **Never set `jump_pressed`.** It drives the double-tap flight toggle (movement.rs:115-125), so a slime could start flying.
- Hop height and speed differ from a player's.
  - Pass a `MoveParams` into `plan` instead of the `WALK_SPEED`/`JUMP_SPEED` consts. Keep the player's values as `MoveParams::PLAYER`, which matches the named-tunables preference.
  - Magma-style variants are then just another params row.

### D5. Slime state → **one component, `Slime`**

- **Fields:**
  - `size: u8` (1/2/4)
  - `jump_delay: u8`
  - `yaw: OrderedFloat<f32>`
  - `wander_until: u32`
  - later `hunger/agitation: u16` fixed-point
- **Adding the component:**
  - Add an `Ecs` field, with lines in `create_entity_safe`, `create_entity_raw` and `remove_entity_safe`. Removal must stay unconditional (state.rs:871-880).
  - Add it to `EntityBundle`, so a slime crossing a seam keeps its size and timers.
  - Optionally add `size` to `GhostData`, so a ghost slime renders the right size.
- **Tiny, integer-heavy state keeps the hash and snapshot cost negligible.**

### D6. Spawning → **v1: deterministic per-region spawner inside the Tick; debug `/summon` later**

- **Genesis spawn** (in `seed_terrain_genesis`) is server-only and rides the snapshot.
  - But it never re-runs on a parked restore, and it needs a raw unlogged body insert.
  - Fine for "slimes exist at world start", poor for a living world.
- **Tick-driven spawner** (recommended):
  - Every N ticks, for a region under its slime cap, pick a candidate column from `h(tick, region, salt)`.
  - Gate it on a slime-chunk-style `h(seed, chunk) % 10 == 0` and a surface check (`surface_voxel_y` exists).
  - Spawn through the logged, undoable path.
  - It runs identically on client and server. Minecraft's slime-chunk rule gives explorable "slime places" for free, which fits the exploratory direction.
- **Despawn:** remove slimes far from any player once the region has been cold for a while, or simply let parking freeze them. Parked regions don't tick, so frozen slimes cost nothing.
- **Chat commands:** the chat/commands investigation's predicted `GameEventKind::Command` would give `/summon slime` for testing when that lands.

### D7. Performance guardrails

- **Skip the KCC while sitting.** A grounded slime waiting on `jump_delay` issues no `move_shape`, leaving only the (already-sleeping-capable) kinematic body. With a 10–30 tick delay and ~12 airborne ticks per hop, this cuts KCC work roughly in half.
  - It must still be woken if something moves it: a player bump, a block edit under it. The cheap trigger is "the block under the feet changed, or a body overlapped last tick".
- **Per-region cap** (named const, e.g. `SLIME_CAP_PER_REGION = 8`) until measured.
- **Let slimes sleep.** Don't copy the player's `can_sleep(false)`.
- **Slime–slime contact:** kinematic bodies don't push each other, so slimes will overlap.
  - Fine for v1.
  - Later, a cheap circle-separation nudge in the controller is enough (no contact solver).
- **Stacking (SR towers):** a scripted "climb onto" state, never emergent contact stacking.
- **Measure** with `HashMode::Off` (per sim-cost note) before and after, in a region with the cap filled.

### D8. Rendering → **per-kind avatar + render-only squish**

1. **Motion events:** emit `SetEntityPosition` per tick for kind-bearing, camera-less active bodies (physics.rs:60-69), and once at spawn (as edit.rs:216-223 does). Otherwise a slime sits at the origin or freezes.
2. **Avatar:** make `AvatarAssets` a per-kind map. `EntityKind::Slime` makes the avatar match non-exhaustive, so the compiler finds the spot.
3. **Mesh:** a child entity with the pivot at the feet, holding:
   - an inner core cube with eyes and mouth (texture or tiny cubes),
   - an outer translucent gel cube (alpha-blended material).
4. **Squish:** a Bevy system per slime child.
   - It detects take-off and landing from the interpolated vertical velocity of `SimTarget` (the sign flip and the stop), rather than shipping `grounded` over the bridge.
   - It runs Minecraft's spring at frame rate, with constants rescaled from per-tick to per-second.
   - It sets `scale(s, 1/s, s)` on the child.
   - The sim never sees it, so there is no rollback double-fire.
5. **Landing particles and sound:** later. Trigger them from the same client-side detection. Because it's render-derived they won't double-fire on replay, which is the point of keeping them out of the sim.

**Size:** scale the whole child by `size`. The squish divisor `size*0.5+1` makes big slimes wobble less, as in Minecraft.

### D9. Non-violent interactions (open, not needed for v1)

These are candidates, in rough order of cost:

- **Curious:** when a player is within ~6 blocks, the slime targets them, hopping faster (delay/3) and stopping at ~2 blocks to face them. This is Minecraft's Attack goal minus the damage. The "look at me" moment sells the creature.
- **Feed:** use-item on a slime lowers hunger and drops a plort-like item. Needs the item and inventory path; SR's core loop.
- **Grow and split:** feeding grows size 1→2→4, and a size-4 slime splits into small ones after a while. This is Minecraft's split rule without death, and it exercises the create/destroy churn under rollback.
- **Mood face:** hunger and agitation select an expression texture (SR's readable needs).
- **Fusion/Tarr-like systemic risk:** only if the game direction wants a "mess to clean up" loop.

## 4. Recommended first slice (S1 "a slime hops around")

1. `EntityKind::Slime`; a `Slime` component (size, jump_delay, yaw, wander timer) in `Ecs` + `EntityBundle`.
2. `MoveParams` threaded through `plan` (the player's behaviour stays bit-identical; covered by `tests/jump_arc.rs`).
3. Extract `step_body` from `CameraController`, then add a `MobController` in region.rs:166 before `PhysicsController`.
   - It skips ghosts.
   - It uses the hash RNG, the Minecraft hop and wander logic, and skips the KCC while sitting.
4. `spawn_slime_safe` on `build_kinematic_body_safe` with a small capsule, plus the per-region deterministic spawner with a cap.
5. `SetEntityPosition` emit for camera-less movers.
6. Client: slime avatar (core + gel cube) and the squish system.
7. **Tests**, each named with `just test-one`:
   - A slime's hop cadence and heading from a fixed seed.
   - `hash(before) == hash(after undo)` across a tick with a spawn and a hop.
   - Rollback replay reproduces the same slime path.
   - A slime crosses a region seam and keeps its `Slime` state, run in the seeded `handoff_schedule` layer (so `just test-threads`).

Later slices: S2 curious targeting + sizes/split; S3 hunger/feeding/plort; S4 art pass (particles, sound, faces).

## 5. Open questions for the user

1. **Scope of v1:** ambient wildlife only (hop, wander, curious), or straight into the feed → produce loop?
2. **Look:** Minecraft cube (core + gel, cheap, on-style with voxels), or SR round blob (needs a sphere mesh + vertex wobble shader)? The cube is recommended for v1.
3. **Where do slimes live:** slime-chunk-style hotspots everywhere, or tied to a biome/wetness once the forged world has rivers and lakes (a "slimes near water" rule is a natural fit)?
4. **Density:** what count per region feels alive? This sets the perf budget (D7).

## 6. What was built (S1, 2026-09-22)

- **Sim (`crates/game/src/slime.rs`):**
  - `EntityKind::Slime { size }`. The size lives in the kind, so ghosts and the renderer get it for free.
  - A `Slime` component in `Ecs`, carried in `EntityBundle.slime`.
  - `Rollback::tick_slimes_safe(region)` runs at the start of each region tick, before the controllers.
    - It isn't a `Controller`, because the body builder and surface query are `Rollback` methods.
  - A stateless `roll(tick, subject, salt)` splitmix RNG.
  - The Minecraft hop/rest cadence: a sitting slime skips the KCC entirely.
  - Heading re-rolls every 40–100 ticks, turning at most 45° a tick.
  - An **edge leash**: within `SLIME_EDGE_MARGIN` (4 blocks) of a region edge, a slime heads inwards. It never enters the ghost margin, so it never departs or wakes a cold region.
  - Spawner: `slime_spawn_plan` (pure) plus a surface check.
    - It rejects water (block id 5) and needs terrain under the column.
    - It never spawns on tick 0.
- **Spawn tunables** (named constants, because the client sims too, so they must match on both sides):
  - `SLIME_SPAWN_INTERVAL_TICKS` = 100
  - `SLIME_SPAWN_CHANCE_PERCENT` = 50
  - `SLIME_CAP_PER_REGION` = 8
  - `SLIME_SIZE_WEIGHTS` = 1:60, 2:30, 4:10

  An empty region fills in about 80 s.
- **Movement:**
  - `movement::MoveParams` + `plan_with`: the player path goes through `MoveParams::PLAYER`, which is bit-identical.
  - `movement::sweep_character` was extracted from `CameraController`, and players and slimes share it.
- **Render events:** `PhysicsController` emits `SetEntityPosition` every tick for camera-less bodies with `Motion`.
- **Client (`renderer/avatar.rs`):**
  - A slime body of 5 scaled unit cubes under a feet pivot: translucent gel, core, 2 eyes, a mouth.
  - The `SlimeAvatar` marker guards against a rebuild when a rollback re-sends the kind.
  - `squish_slimes` runs Minecraft's squish spring. Take-off and landing are read from the per-tick `SimTarget` updates, so it is render-only.
- **Tests:** `crates/game/tests/slime.rs` covers:
  - the rest/hop cadence, including no sliding while sitting and a pose update each tick,
  - the edge turn-back with no departures,
  - spawn plus 60 ticks undoing bit-exact and replaying identically.

  There are also lib unit tests in `slime.rs`, and avatar tests in `avatar.rs`: slime built once, squish shape. The avatar tests were **not run**, because client tests are opt-in.
- **Not done:** a live look in the client, and a perf measurement with full regions.
