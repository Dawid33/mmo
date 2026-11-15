---
name: headless-harness-world-size
description: "Headless client harnesses must shrink BOTH the LOD ring table AND the near window, or each test builds a full production world (minutes + GB)"
metadata: 
  node_type: memory
  type: project
  originSessionId: befcc3f3-5dd6-43d2-a0d5-a88e2807c9bc
---

Any headless test that stands up a `GameInstanceManager` pays, synchronously, for
whatever world the production client would stream. Two knobs, and you need both:

- `set_lod_table(&[])` — otherwise the client inherits `game::lod::LOD_RING_TABLE`
  (ring bound 32 regions ≈ a 65×65 key footprint) and the inline `LocalServer`
  builds **every key synchronously**. `SimHarness::new` already did this; the
  Bevy harness and two `local_server.rs` tests did not.
- `set_near_radius(0)` — otherwise the client subscribes to the 3×3 window, and
  each region is a full worldgen + voxel-collider genesis on the server *plus* a
  deserialized copy on the client. Added 2026-07-11
  (`crates/client/src/instance.rs`, `RegionCoords::window_radius`).

Both had to be set: the near-window burst is requested at `instance.rs` join time
via `home.window_radius(self.near_radius)` — there were **two** subscription sites
(`desired_window` and the initial join burst), and fixing only one changed nothing.

**Why:** `bevy_input`'s 5 tests each built a 9-region world and peaked ~6 GB;
cargo runs tests at one-thread-per-core, so 20 × that OOM-killed a 32 GB box. The
symptom looked like a hang or a leak and is neither — RSS is flat across ticks.

**How to apply:** when a new headless client test is slow or fat, check these two
knobs before profiling anything else. Related:
[[client-voxeldata-dense-cache]], [[testing-one-client]].
