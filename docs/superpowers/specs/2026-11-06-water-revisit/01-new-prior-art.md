# Water revisit — 01: new prior art (gaps not in the 2026-09-22 survey)

Date: 2026-11-06. Online research only; no code touched. Scope: the six gaps listed in the brief.
The 2026-09-22 survey (`specs/2026-09-22-voxel-water-efficiency/`) already covers Minecraft/Flowing
Fluids, Luanti, Terraria, ONI, Noita, Timberborn (basic), From Dust, Enshrouded (pre-release),
Hytale, Vintage Story, DF pressure basics, Veloren, Teardown, Valheim, Space Engineers, Cities
Skylines, virtual pipes, Fill-Spill-Merge — not repeated here.

Confidence tags: **[primary]** = developer's own post/docs; **[wiki]** = community wiki (DF wiki
pages are often tagged for older versions); **[secondary]** = press/forum/third party;
**[unverified]** = could not confirm.

Our design for reference: deterministic integer in-sim water on server and every client, rollback
(~10 ticks re-sim), finite volume (sea = finite body + ledger), pooled **bodies** (level + volume)
plus a per-cell CA only at the frontier, cross-region flow as outflow/arrival events.

---

## 1. Factorio — fluid segments (closest analogue to "bodies" under strict determinism)

**Context.** Factorio is fully deterministic lockstep multiplayer (every peer simulates the whole
map). Fluids went through three stages:

* **0.17 (FFF #271, 2018) [primary]:** fluid logic moved into its own system; connected fluid boxes
  form a *fluid system*; "these systems don't affect one another, [so] their updates can run in
  parallel". Straight pipe runs were to be merged into segments; the new flow algorithm was shelved
  (forum note on FFF #283). → Independent connected networks are both the determinism-safe
  parallelism unit and the performance unit.
* **2.0 / Space Age (FFF #416 "Fluids 2.0", Jun 2024) [primary]:** pipes, undergrounds and tanks
  are merged into **segments**; a segment's volume is the sum of its fluid boxes; **one fluid per
  segment**. Fluid no longer propagates along the pipe: fluid pushed in is "immediately available
  at any point along a segment". Machines push into a segment at an unlimited rate; pulls are
  scaled by the segment's fill ratio (half full → half of max pull). Junction order stops mattering
  ("a segment only cares about the machines connected to it"). Motivation: Space Age's production
  scale "brought the flow algorithm to its knees"; the earlier hack (bigger pipe volume) made
  buffers absurd (flamethrower walls buffering tens of thousands of units). What they gave up, in
  their words: "a fairly large step back in terms of the 'realism' of the fluid simulation", and
  pumps no longer add throughput. "Longer pipelines have higher throughput, but take longer to
  fully empty."
* **FFF #430 "Drowning in fluids" (Sep 2024) [primary]:** because a segment is instant, they added
  a hard **extent limit** — announced as 250×250 tiles; exceeding it stops *all* flow in the
  segment and the game shows where a pump must split it. Shipped value per the official wiki is
  **320×320 tiles / 10×10 chunks [wiki]** (the FFF figure was pre-release; treat 320 as current).
  Per-connection cap: **100 units per flow operation (6000/s at 60 UPS)**, now multiplied by
  *source fullness × (inverse) sink fullness*, so pipelines settle at an equilibrium fill and
  oscillate around it. Pumps cut to 1200/s. A pump is the segment boundary.
* **Split/merge [wiki, partly unverified]:** placing adjacent pipes merges segments; removing a
  fluid entity pushes its fluid into the connected entities and **excess is destroyed**; mixed
  fluids on contact → all but one fluid deleted. No published detail on how volume is apportioned
  when a segment splits (presumably pro-rata by capacity — unverified). Temperature: a segment holds
  one temperature (averaging rule not documented).
* Determinism, threading, rollback of segments: **not discussed** in either FFF.

**Steal.**
* A pooled body that is *one number* (volume) with level derived from capacity is exactly what a
  lockstep game shipped at mega-factory scale; bodies as the parallel/determinism unit is proven.
* **Bound the body's extent** (they had to add it): an instantly-equalising body that spans a huge
  area is unphysical and lets players exploit it. For us the natural bound is the region (a body
  never spans a seam; a seam is a "pump" — an explicit, rate-limited link).
* Rate limits as **products of fill ratios** at body↔consumer links give deterministic, order-light
  sharing; they explicitly fixed the "whoever updates first wins" bias of 1.1 — we must make any
  per-tick link evaluation order-independent (compute all link flows from the start-of-tick state,
  then apply).
* Make the boundary *visible and interactive* (their pump-needed markers) rather than silently
  physical.

**Avoid.** Destroying fluid on split/removal (we require conservation — the ledger must catch
every split remainder: integer volume split with an explicit remainder rule). "Instant anywhere in
the body" is fine for a lake surface but wrong for long rivers; Factorio accepts it, a DF-style
game probably should only pool *still* water and keep rivers as frontier/flow.

---

## 2. Dwarf Fortress water beyond pressure

All [wiki] unless noted; many DF wiki pages are for 0.34/40d — v50 differences flagged where known.

* **Sourced water / map edge.** Water from aquifers and from any source that "extends from the edge
  of the map (rivers, brooks, oceans, some lakes)" is *sourced* (infinite). River tiles at the map
  edge regenerate water at intervals; a river behaves as an unlimited supply in at the upstream edge
  and out at the downstream edge. A dammed river refills only up to its source level. **The map
  edge is a sink**: water reaching an open edge (or a chasm/bottomless pit) leaves the simulation;
  the standard flood-proofing is an underground drain carved through to the map edge. "The final
  z-level never fills" when the river can drain off-map. Edge-spill is only simulated while the
  site is loaded [secondary, reddit].
* **Aquifers.** Light vs heavy: light aquifer tiles leak water "very, very slowly" and do *not*
  absorb water; heavy aquifers produce near-full (7/7) water almost immediately and their walls act
  as an **infinite sink like an open map edge**. Horizontal/downward digging gives a "damp stone"
  warning before breach; digging *up* into one gives none. No official production rates found
  [rates unverified].
* **Evaporation.** At normal temperature, water (and magma) at depth **1/7** evaporates; keeping a
  pool at ≥2/7 stops it except in dry seasons; scorching heat dries even full murky pools. Rain on a
  murky pool adds 1/7. Murky-pool water is **stagnant**; *pumping* cleans it, gravity flow does not.
* **Freezing.** Outdoor water freezes/thaws effectively instantly with the season; underground
  water usually stays liquid. Shallow water → ice floor, deeper → ice wall (threshold sources
  disagree: 1/7 vs ≤3/7). **Creatures in a tile that freezes are encased and die**; items/corpses
  are recovered only by mining/melting. Wells/grates over freezing water are deconstructed.
  **Thawed ice walls always become 7/7 water** — i.e. DF creates volume on thaw (non-conservative).
* **Flow moves things.** Flowing liquid pushes items (by weight and flow speed) and creatures
  (including through grates). Water *created on a tile* (pumped, or river source that is not
  draining) is not "flowing" and pushes nothing; neither does steady river flow without movement.
* **Performance.** "Flowing water slows the game down" (framerate page); map-edge river tiles keep
  generating water, so badly drained designs keep the CA permanently active. The only liquid-perf
  note in recent release notes: **53.01 "major optimization for pond filling"** [wiki release
  info]. No published v50 change to liquid rules found [unverified].
* Pumps (create water on output, clean it, can lift one z per pump), cisterns, wells, obsidian
  casting (water + magma), pressure-teleport — covered previously.

**Steal.** The embark edge as an explicit *boundary condition* (source tiles at the upstream edge,
sink at the downstream edge) — for us the world's outer seam and unloaded neighbours can play this
role with a ledger instead of infinity. Evaporation of a 1-unit film is the classic way to stop a
CA from leaving a permanent 1/7 smear that never sleeps — we need *some* film rule (absorb into a
body's ledger / "ground moisture" rather than delete, to stay conservative). Encased-in-ice deaths,
flow-pushed items and "stagnant vs clean" are cheap emergent hooks players love. Damp-stone warning
is a must-have UX for underground water.

**Avoid.** Infinite sources/sinks (we are finite by decision), thaw-creates-water, and the
permanent-activity river source: DF's FPS complaints come exactly from water that never settles.

---

## 3. Colony sims with dig-able terrain and water

* **Timberborn [primary/secondary].** Original sim: 2D heightfield, each tile links to 4 neighbours
  by **virtual pipes** (from Mei/Decaudin/Hu GPU erosion), stores depth, momentum = flux between
  ticks, sim tick **"every couple hundred ms"**, water rendered as one sheet. Water *sources* have a
  strength that drops before a drought starts so flow ramps down smoothly. Evaporation exists; they
  considered faster evaporation for small bodies (the "water dump exploit") but shipped without.
  Moisture (irrigation) is a **separate derived field** spilling from water tiles, decaying per step
  and sharply uphill. Dams regulate flow "without hard values, since those would cause
  oscillations"; levees block; floodgates switch. A water-wheel resistance was added to kill a
  perpetual-motion pump loop. Path recomputes when water cut paths caused lag spikes → beavers keep
  their path and swim. **Update 6 "Wonders of Water" (Oct 2024)** replaced the 2.5D sim with a 3D
  one (water/badwater over and under buildings: aqueducts, tunnels, underwater cities); badwater
  (Update 5) is a contamination share carried with water. No published algorithm for the 3D sim,
  threads or determinism [unverified]. Single-player.
* **Going Medieval [primary dev post].** Water voxel = terrain voxel (1×1×3 m); **3 fill levels +
  empty**; deliberately *not* realistic: no pressure ("would require constant layer calculations"),
  no fast voxel-to-voxel distribution (CPU); fills step by step like Minecraft; waterfalls at
  ledges. **Water is not infinite**: map-generated *inflow voxels* (count tuned for CPU) keep
  emitting until covered. Later patch 0.16.2: a full voxel spreads over more voxels as its level
  drops; rain does not fill an empty pit. Player complaints: river diversion → floods, uneven flow,
  random waterfalls [forum].
* **DwarfCorp [primary, 2013].** Per-voxel cell: byte level 0–255, flow vector, fluid type;
  only cells with water are processed; per cell: evaporate below threshold → fall → spread to 4
  neighbours ordered by flow vector, **random amounts** → reset flow. No pressure. Random transfer
  amounts = nondeterministic as written.
* **RimWorld (incl. Odyssey), Songs of Syx, Stonehearth, Gnomoria, Kenshi:** no evidence found of
  a volume-conserving flow sim relevant to us (RimWorld/Songs of Syx water is terrain type; Kenshi
  none) [unverified, not searched in depth].

**What the genre's players expect water to do** (from DF wiki/forum and the above): flood
control (drains, floodgates, levers), cisterns and wells (winter supply), moats and drowning
invaders (flood traps), irrigating underground farms (muddying), obsidian casting, damp-stone
warnings, freezing hazards, drought pressure (Timberborn), dams/reservoirs as base-building.
Common complaint across games: water that won't settle (perf) and diversions that cause chaotic
floods. All of these work with settled bodies + short frontier events; none needs high-fidelity
flow.

**Steal.** Few discrete fill levels are acceptable to players (Going Medieval 3, DF 7). A derived,
non-conserved *moisture* field next to water (Timberborn) is the cheap irrigation answer and keeps
the conserved sim small. Rate-limited, non-binary dam/gate links (no hard thresholds → no
oscillation). Pathing must not replan on every water change (Timberborn spike lesson).

**Avoid.** Infinite inflow voxels; random per-cell amounts (DwarfCorp) — need hash RNG or none.

---

## 4. Deterministic / lockstep / rollback fluids

* **Shipped lockstep + fluids:** Factorio (above) is the only well-documented shipped example.
  Its recipe: integer-ish volumes per independent network, fixed per-tick caps, deterministic
  update order, networks updated in parallel because they don't interact. No rollback.
* **Rollback + fluid/CA:** **nothing found.** No GDC talk, paper or shipped game describing a
  fluid/falling-sand CA under rollback netcode. RTS terrain water (Supreme Commander, Planetary
  Annihilation) is static sea level, not simulated [unverified but consistent with their docs].
  A Rust crate `alice-physics` advertises a `fluid_netcode` module (Fix128 state, delta
  compression, snapshot/restore, checksums) — hobby-scale, no write-up [secondary].
* General lockstep advice that applies [secondary blogs]: fixed-point/integer state, no float
  transcendental, **no hash-map iteration order in sim effects**, periodic checksums.
* Rollback cost: the Delta Rollback post (Dehaene) and Jewel Run notes stress that per-frame full
  saves dominate; delta/undo logs win when touched state is small per frame. For water this argues
  for **logging per-tick deltas of active frontier cells + body scalars**, never snapshotting grids
  — which matches our existing undo-log model.

**Conclusion.** We are on our own for rollback; the defensible line is: bodies are O(1) scalars
(cheap to undo), the frontier CA touches few cells per tick (cheap to log), and settled water costs
zero per tick. Frontier size is the rollback-cost risk and needs a hard budget per region tick.

---

## 5. Partitioned / distributed fluid across boundaries

* **Folia (Paper MC region threading) [primary docs].** Avoids the seam problem instead of solving
  it: regions are groups of N×N-chunk *sections*; every section with chunks keeps a **buffer of
  empty sections** around it; any chunk within the merge radius of another must be in the same
  region, so **independent regions merge when they come close** and split again later. Invariant:
  "a ticking region cannot expand the chunk positions it owns as it ticks", and code in one region
  "under no circumstance" touches another's data. Fluid (scheduled block ticks) therefore never
  crosses a live region boundary — water at the edge of loaded land just stops (vanilla behaviour
  at unloaded chunks) [the fluid consequence is inferred, not documented]. Not applicable as-is
  (our regions are fixed tiles, not dynamic), but it shows the two clean options: never let a
  sim interaction span owners, or make it an explicit message.
* **SpatialOS / Dual Universe / server-meshing:** no fluid simulation found; their cross-boundary
  mechanisms are entity authority handoff, which is what we already have.
* **CFD domain decomposition [papers].** Mass is conserved across subdomain interfaces when the
  interface quantity exchanged is the **flux** (amount crossing the face), applied as −F on one side
  and +F on the other, not the neighbour's state; face-based asynchronous schemes (Omelchenko &
  Karimabadi-style "asynchronous discrete event" FV, arXiv 1610.05051) treat each face transfer as
  an event and allow different update times per side; mortar/space-time methods enforce
  normal-flux continuity for different time steps. No source analyses a fixed 1-tick lag as in
  our case [gap].

**Derived rule for our seam (not from a single source):** make each seam face **owned by exactly
one side** (e.g. the region that holds the water above it, or a fixed side), have only the owner
compute the flux from *its own* state plus the neighbour's last *reported* level, subtract it
immediately and send it as an arrival event (conserved by construction, like entity handoff); add
**damping/rate-cap** proportional to the lag (Timberborn's "no hard values" and Factorio's
fill-ratio product) so two sides reacting to each other's 1-tick-old level cannot ping-pong. When
both sides are bodies, prefer a single seam *link* with a capped flux that is only re-evaluated
from both last-reported levels — oscillation at worst = cap × lag, and it is bounded.

---

## 6. 2025–2026 notable

* **Enshrouded "Wake of the Water" (Nov 2025) [secondary press].** Fully simulated voxel water
  with pressure, flow into bases, pipes/canals/irrigation/water-powered structures; Keen said voxel
  water was very hard and delayed it; hosting providers note server knobs for **simulation distance
  and tick rate** for water-heavy builds. No algorithm published. Server-authoritative co-op, not
  lockstep.
* **Engineer game devlog (Dec 2025) [primary].** 1 m liquid voxels in chunks; per *horizontal
  slice* of a water body a small virtual-pipes solve produces a flow (velocity) map **only when the
  slice's shape changes** — the shader fakes motion in between; liquid meshing on its own thread;
  only changed chunks remesh. → "Visual flow is derived from body shape, not simulated per tick" is
  a cheap trick we can use for settled bodies (rendering only, outside the sim).
* **DF 53.01** pond-filling optimisation (above).
* **van der Scheer, "Cellular Automata Fluid Physics for Voxel Engines" (Utrecht MSc, 2022)** — CA
  with momentum advection + compression constraints on GPU; background only (GPU, float).
* No GDC 2026 talk on voxel water found [searched lightly].

---

## Decision-relevant summary

1. Factorio proves pooled bodies under lockstep at scale; they needed a **hard extent bound** and
   **fill-ratio-product link rates**; they *destroy* fluid on splits — we must not.
2. Nobody has published fluids under rollback; our cost argument (O(1) bodies + logged frontier
   deltas + zero-cost settled water) is original and must carry a per-tick frontier budget.
3. DF's edge = infinite source/sink and its 1/7 evaporation are how DF keeps water settling; we
   need a conservative equivalent (film → body ledger / moisture) to let the frontier sleep.
4. Seams: transfer **flux, not state**, owned by one side, applied −F/+F via events, rate-capped
   to bound 1-tick-lag oscillation. Folia's alternative (merge owners) is unavailable to us.
5. Genre expectation is gameplay (drains, gates, cisterns, moats, drowning, irrigation, freezing),
   not fidelity; 3–7 discrete levels are accepted; irrigation can be a derived moisture field.

## Sources

* FFF #271 Fluid optimisations — https://www.factorio.com/blog/post/fff-271 (mirror https://cf-www.factorio.com/blog/post/fff-271)
* FFF #283 forum note on what shipped in 0.17 — https://forums.factorio.com/viewtopic.php?p=399626
* FFF #416 Fluids 2.0 — https://www.factorio.com/blog/post/fff-416
* FFF #430 Drowning in fluids — https://www.factorio.com/blog/post/fff-430
* Factorio wiki, Fluid system — https://wiki.factorio.com/Fluid_system
* DF wiki: Water (v0.34) — https://dwarffortresswiki.org/index.php/v0.34:Water ; DF2014:Water — https://dwarffortresswiki.org/index.php/DF2014:Water
* DF wiki: Aquifer — https://dwarffortresswiki.org/index.php/Aquifer ; Damp stone — https://www.dwarffortresswiki.org/index.php/DF2014:Damp_stone
* DF wiki: River — https://dwarffortresswiki.org/index.php/DF2014:River ; Waterfall — https://www.dwarffortresswiki.org/index.php/DF2014:Waterfall ; 40d:Pressure — https://dwarffortresswiki.org/index.php/40d:Pressure
* DF wiki: Murky pool — https://dwarffortresswiki.org/Murky_pool ; 40d:Evaporate — https://dwarffortresswiki.org/index.php/40d:Evaporate ; Depth — https://dwarffortresswiki.org/index.php/DF2014:Depth
* DF wiki: Freezing — https://dwarffortresswiki.org/index.php/DF2014:Freezing ; Ice — https://dwarffortresswiki.org/index.php/Ice
* DF wiki: Flow — https://dwarffortresswiki.org/Flow ; Maximizing framerate — https://dwarffortresswiki.org/index.php/DF2014:Maximizing_framerate
* DF wiki: Release information 53.01 — https://dwarffortresswiki.org/index.php/Release_information/53.01
* Steam DF discussion (edge river tiles generate water) — https://steamcommunity.com/app/975370/discussions/0/3716063566027745889
* Deep dive: Timberborn's water mechanics — https://www.gamedeveloper.com/design/deep-dive-timberborn-s-water-mechanics
* Timberborn Update 6 press release — https://mechanistry.com/press/wonders-of-water-timberborn-update-6-adds-3d-water-physics-new-skyward-engineering-and-official-mod-support ; wiki https://timberborn.wiki.gg/wiki/Update_6
* Going Medieval water dev post — https://foxyvoxel.io/?p=576 ; Update #10 — https://foxyvoxel.io/2023/11/21/update-10/ ; 0.16.2 notes — https://www.eprison.de/spiele/going-medieval/steam-news/5302486388001865628/6460/19787.html ; player reports — https://steamcommunity.com/app/1029780/discussions/0/3959288052800898049/?ctp=2
* How water works in DwarfCorp — https://www.gamedeveloper.com/programming/how-water-works-in-dwarfcorp
* alice-physics fluid_netcode — https://docs.rs/alice-physics/latest/alice_physics/fluid_netcode/index.html
* Determinism in a lockstep simulation — https://warman.pages.dev/blog/determinism-in-a-lockstep-simulation
* Delta rollback — https://medium.com/@david.dehaene/delta-rollback-new-optimizations-for-rollback-netcode-7d283d56e54b
* Folia README — https://github.com/PaperMC/Folia (branch ver/1.21.4) ; Region logic — https://docs.papermc.io/folia/reference/region-logic
* Asynchronous discrete event schemes for PDEs — https://arxiv.org/pdf/1610.05051
* Enshrouded Wake of the Water — https://www.pcgamesn.com/enshrouded/wake-of-the-water-interview ; https://hosthavoc.com/blog/enshrouded-wake-of-water-update ; https://enshrouded.wiki.gg/wiki/Water_has_come_to_Enshrouded!_%F0%9F%92%A6
* Engineer game water devlog (Dec 2025) — https://engineer-game.org/devlog/2025/12/22/water-rendering.html
* van der Scheer, Cellular Automata Fluid Physics for Voxel Engines — https://studenttheses.uu.nl/handle/20.500.12932/42762
* Noita GDC 2019 talk — https://gdcvault.com/play/1025695/Exploring-the-Tech-and-Design
