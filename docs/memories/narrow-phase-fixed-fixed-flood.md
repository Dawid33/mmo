---
name: narrow-phase-fixed-fixed-flood
description: "SUPERSEDED 2026-09-27: the fork's fixed-fixed pair filter + set_body_type guards are gone; upstream rapier 0.36 filters pairs at creation and re-pairs on type change (physics-next pair_filter.rs); keep for the 2026-07 measurements only"
metadata: 
  node_type: memory
  type: project
  originSessionId: 66e418bc-43d1-4cc7-bdf8-762a64181424
---

**SUPERSEDED 2026-09-27 by the rapier 0.36 switch** ([[vendored-deps-update-investigation]]): upstream 0.36 filters pairs at creation (same parent, `ActiveCollisionTypes` — which also drops kinematic↔fixed — and collision groups) and re-inserts a collider's broad-phase leaf when a filter input changes, so the fork's `pair_permanently_dead` and the `set_body_type`/`set_parent` panic guards are gone (no fork patch). Pinned as parity tests in `crates/physics-next/tests/pair_filter.rs` (the panic tests became `*_re_pairs`). The cached `pairs_hash` is gone too (sorted StateHash walk instead). What follows is the 2026-07 history and measurements.

Probe: `crates/worldgen/examples/narrow_phase_probe.rs` (release, Delta mode,
region (0,0), seeded worldgen). One region = 169 chunk bodies / 197 colliders →
**4,884 contact-graph edges, 4,855 (99.4%) fixed-fixed chunk-chunk** pairs that
can never contact (default `ActiveCollisionTypes` excludes FIXED_FIXED — but
that filter runs only AFTER the edge exists). 28 more are same-parent
(block-scale vs chisel-scale collider of one chunk). Only 29 involve the
player. Mean node degree 49, max 139. Cause: chunk AABBs tile space exactly, so
each touches up to 26 neighbors inclusively + broad-phase change-detection skin;
the BVH `pairs_collector` (broad_phase_bvh.rs) has NO body-type filter.

Measured costs the dead edges drive (per region):
- Pair-creation storm on genesis/first tick: broad 1.1ms + narrow 2.6ms
  (4,884 journaled add_pair graph ops). Re-paid on any rollback crossing it.
- Collider-removal tick: `narrow_wholesale = Box::new(self.clone())`
  (narrow_phase.rs:335) clones the whole graph; `pre_digest` hashes it all —
  1.4ms digest vs 0.57ms of actual step work; every rollback re-pays
  restore + revert_digest. `remove_entity_safe` whole-PhysicsState
  snapshot_raw = 2.9ms, also graph-dominated.
- Quiescent step: narrow phase iterates every edge (fork TODO "don't iterate
  on all the edges") = 19µs of a 25µs step — 76% relative, small absolute.
- First-tick 72.9ms total is NOT the graph: 69.2ms is mass-props
  ([[first-tick-massprops-spike]], still unlanded).

Highest-leverage fix: filter fixed-fixed (and same-parent) pairs before edge
creation (broad-phase collector or add_pair), shrinking the graph ~170x; must
handle body TYPE changes re-emitting pairs. Was scope-excluded by user decision
from the 2026-07-10-surgical-collider-attach plan; user later approved after
the research below — plan: docs/superpowers/plans/2026-07-10-fixed-fixed-pair-filter.md.
Amplifiers: [[client-prediction-runaway]] re-sim multiplies all of the above.

Online research verdict (2026-07-10, 3-agent survey): filtering is SAFE and
industry-default. Box2D (b2Body::ShouldCollide), Bullet (default masks),
PhysX (filter shader always drops static-static), Jolt (NON_MOVING layer
filter) all kill static-static pairs BEFORE pair creation. Upstream rapier is
the outlier: both SAP and BVH broad phases are body-type-agnostic; the
ActiveCollisionTypes FIXED_FIXED gate (default off since v0.9, issue #15/PR
#44) only pair.clear()s the already-created edge. No upstream objection found;
upstream's static-world answers are the persistent BVH (issue #558 was BVH
rebuild cost, fixed in 0.27+) and the parry Voxels shape (PR #336).
Correctness obligations for our filter: (1) both bodies strictly Fixed, not
merely non-dynamic; (2) don't filter if either collider opts into
ActiveCollisionTypes::FIXED_FIXED or is a sensor (fork runs contacts-only
today); (3) fixed->dynamic type change must force broad-phase re-pairing
(upstream relies on edges being present-but-cleared; with the filter they're
absent) — every engine has this escape hatch (Box2D TouchProxy, PhysX
resetFiltering, Jolt SetObjectLayer) and documents it as expensive; chunks
never type-flip today; (4) raycasts/queries unaffected (never used contact
pairs); (5) filter must live in the shared sim layer so client+server agree
(pure function of body type -> deterministic; hashes shift, same-build only).

IMPLEMENTED 2026-07-10 (develop fa63f5e..f7ccb66, subagent-driven):
`BroadPhaseBvh::pair_permanently_dead` suppresses pairs at BVTT collection when
both effective body types are Fixed and neither collider opts into
ActiveCollisionTypes::FIXED_FIXED (mirrors the narrow-phase gate exactly;
sensors covered by the same gate). Guards: `set_body_type` and `set_parent`
panic on fixed->non-fixed transitions with attached colliders ("fixed-fixed
pairs are suppressed" message) — the suppressed pairs have no re-pairing path.
Measured after: 29 edges/region (0 ff), quiescent step 25us->2us, removal-tick
pre_digest 1.4ms->44us, remove_entity_safe 2.9ms->852us. Tests:
crates/game/tests/pair_filter.rs (10). Runtime FIXED_FIXED/sensor flag toggles
on already-overlapping fixed colliders remain a DOCUMENTED unguarded blind spot.
First-tick 71ms mass-props spike remains ([[first-tick-massprops-spike]]).
Live GUI smoke pending.

FOLLOW-UP FIXED (885bf6c): pre-existing unpark landmine — the broad-phase
`pairs` map serde round-trips FULL content while serde-skipped `pairs_hash`
deserialized to 0 (doc falsely claimed capacity-only). Deserialize now routes
through a serde(from) shadow struct (`BroadPhaseBvhDe`) whose From impl
recomputes the cache; serialized byte format unchanged; doc corrected;
regression test broad_phase_pairs_hash_survives_serde_round_trip.
