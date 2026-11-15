---
name: df-worldgen-research
description: RESEARCH DONE 2026-09-20 — Dwarf Fortress worldgen (pipeline, rivers/erosion, geology); recommendation REVISED same day after user's super-region direction — offline whole-map erosion/hydrology forge producing a stored world artifact (arch B graph machinery obsolete); nothing implemented; don't re-research
metadata:
  type: project
---

Four subagent reports + `00-synthesis.md` in `docs/superpowers/specs/2026-09-20-df-worldgen-research/`.
User's stated interest (2026-09-20): rivers, lakes, valleys, erosion, geological structure — "DF worldgen
is very good".

**Recommendation (not started):** hydrology = architecture B — jittered ~512 m lattice drainage graph per
~16 km cache tile (window padded by the dependency radius ⇒ exact border agreement), capped integer
Dijkstra from ocean nodes, capped flow accumulation, elevation assigned FROM the graph by Flint's law
(`z += k/√A·len`, only sqrt), terrain = lower envelope + `mountains *= valley_mask`, lakes at spill
level, channel carve per 1 m column after ProfileGrid, LOD rule "water rounds down". Then a per-point
erosion filter (runevision Phacelle) as detail; defer Veloren-style per-continent stream-power sim.
Geology minimal cut = stratigraphic coordinate `s = y − B(x,z)` (tilt/folds gated by mountain field) into
per-province rock columns + 2–6 m bed banding + slope/altitude-aware regolith; DF's surface-draped
strata are what NOT to copy. Proposed order: B0 graph overlay prototype in world-map → B1 terrain →
geology cut (parallel) → B2 water/LOD → erosion detail → climate. Builds on [[worldgen-ocean-world-finding]].

**Process lesson:** research briefs to subagents must say what to do when a source refuses access — one
agent read Bay 12 forum threads via the `wap2` view with curl after an anti-bot page (noted in report 02's
Sources). Tell agents up front: record blocked sources as gaps, never route around a refusal.

**DIRECTION CHANGE (user, 2026-09-20, later) — supersedes the arch-B recommendation above:** the world
is divided into super regions, one k8s pod each, LOADING SCREEN between them (this promotes the fallback
of [[multi-instance-k8s-postgres-research]] §1a decision 2 to the chosen direction); each super region is
generated as ITS OWN FINITE WORLD with a seam at the edge, UNLIMITED offline CPU to pre-generate; gameplay
fits around it. Terrain is therefore no longer a pure function of position: it becomes a stored, frozen
world artifact; generator cross-machine bit-exactness, border agreement, per-point cost and cold stalls
stop being constraints (rollback/sim determinism unaffected). Revised plan (addendum in `00-synthesis.md`):
offline "forge" = today's pipeline as land/uplift initial condition → geology BEFORE erosion (erodibility)
→ whole-map stream-power + diffusion + deposition, multi-res → priority-flood lakes + exact flow
accumulation, rainfall-weighted → climate/rain shadows, iterate → DF-style generate-and-test → artifact
(height ~1–2 m, water surfaces + river graph, provinces, climate). `generate_region`/`coarse_tile` sample
the artifact + local refinement. Still needed from the old plan: rivers as objects w/ monotone water
profile, 1 m channel carve, elevated voxel water + LOD "water rounds down" rule, strat-coordinate banding,
regolith. OPEN USER DECISIONS: super-region size (k8s doc's S=8 regions is only 512 m — its "≈8 km" is a
slip; rivers mountain→sea want ≥8–16 km), edge type (ocean-bounded recommended vs mountain/portal vs
edge-matched strip), what wasm `?offline` mode uses (small artifact vs keep procedural generator).
