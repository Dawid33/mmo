# Follow-up: seamless local-player crossing under latency

**Date:** 2026-07-06
**Status:** Deferred — needs its own brainstorm/spec cycle
**Context:** landed with the entity/region handoff feature (spec `specs/2026-07-05-entity-region-handoff-design.md`)

## What shipped

The handoff feature (Tasks 1–9) is complete and reviewed: boundary scan + in-tick
extraction, ghost mirroring with colliders, manager relay (rebase, wake-cold-on-arrival,
home flip, `PlayerRegion` push), server-authoritative input routing by `homes`, client
predicted synthesis for non-local entities, and bridge render-dedupe. The real-threads
crossing test (`crates/server/tests/threaded_world.rs`) exercises an end-to-end crossing.

## The deferred problem

Predicting the **local client's own** crossing under latency is a genuine
distributed-systems problem, not a bug: it requires predicting a transfer of
*server-authoritative* ownership + input-routing across the RTT window in which client
and server disagree about the player's home region. The handoff spec listed this
"flip-tick divergence" as an **accepted risk**.

### Current behavior (as merged — the eviction basis)

The client predicts its crossing (extract from A, arrive in B, flip `home_region`) and
routes input to B; the server, still routing by `homes = A` for k ≈ RTT ticks, sends
those inputs to A, so B's authoritative stream lacks them. `Region::reconcile` calls
`evict_orphan_local_inputs` on the authoritative local `EntityArrived` into B to drop the
un-confirmable predictions. Net result, verified by
`crates/client/src/main.rs` tests `predicted_crossing_target_region_converges_after_authoritative_catchup`
and the coexistence test:

- **Final state converges bit-exact** to authoritative on every crossing; self-heals per
  crossing (no accumulation), no unbounded growth, no panic.
- **Known limitation:** a transient *rubber-band* — if you are actively inputting while
  crossing a boundary under real latency, the post-flip inputs are briefly evicted and
  re-applied ~RTT later as their echoes arrive. Cosmetic; converges.

### Approaches investigated (and why they were not adopted)

1. **Predict flip + input into B, no eviction** — orphan inputs accumulate *unboundedly*
   in the target per crossing. Rejected.
2. **Eviction on authoritative arrival** (merged) — converges bit-exact; transient
   rubber-band. *Proven* that no region-local eviction predicate can do better: at the
   arrival-reconcile instant the client's state is byte-identical whether the server
   routed an input to A (orphan) or B (legit); that routing is the server's decision and
   its echo is future information.
3. **Server-authoritative local crossing** (`is_home`-gated extraction exemption; home
   flips only on authoritative `PlayerRegion`; self-heal re-extracts from A) — eliminates
   the *target-input* orphan class by construction, BUT a streamed-delivery test
   (`local_crossing_streamed_arrival_leaves_stuck_self_heal_orphan`, in the abandoned
   branch) *confirmed* it relocates the orphan: on localhost same-pump delivery the
   authoritative `EntityArrived` is consumed before the self-heal tick, so the self-heal's
   own `EntityArrived` prediction sticks permanently → a **persistent +32-unit pose
   desync** of the player in its home region. Strictly worse than the eviction's transient
   rubber-band. Rejected.

Each approach trades one artifact for another — the signature of a problem that needs a
protocol-level design, not an incremental client-side patch.

## Directions for the future spec

- **Server-side input stamp-routing / cross-region input correlation:** let the server
  disambiguate which region an input belongs to (or forward late A-routed inputs to B on
  flip), so the client's prediction and the authoritative stream agree on where each input
  lands. Removes the orphan class at the source.
- **Explicit predicted-authority-transfer protocol:** a handshake so the client knows the
  exact tick the server flips, letting it predict input into the correct region without
  guessing.
- **Bound-and-accept:** keep the eviction, accept the rubber-band, and only invest in the
  above if playtesting shows it matters.

The full investigation (event-id traces, the streamed-delivery repro, the impossibility
argument for region-local eviction) was captured in per-run reports under the working
tree's scratch area during development; re-derive from the tests named above if needed.
